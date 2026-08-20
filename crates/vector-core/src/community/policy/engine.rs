//! `evaluate` — the pure conviction engine (§1, §3.2, §5).
//!
//! Signals in, report out. No I/O, no keys, no capability checks, no clock: the
//! caller passes `now`. Two clients holding the same inputs reach byte-identical
//! conclusions, which is the whole reason the engine convicts and never
//! sentences.

use super::combine::run_pipeline;
use super::document::{ArmScope, Enforcement, Exempt, ExemptPatterns, Match, Policy, Rule, Rung};
use super::matchers::{cancel_exempt_hits, keyword_hits, link_hits};
use super::normalize::{self, EmojiCodes};
use super::types::*;
use std::collections::{BTreeMap, BTreeSet};

// ── Inputs ───────────────────────────────────────────────────────────────────

/// One message in the evaluation corpus.
#[derive(Debug, Clone)]
pub struct MessageSignal {
    pub id: MessageId,
    pub author: SubjectId,
    pub channel: Hash32,
    /// Inner ms timestamp — the corpus orders and clamps on this.
    pub at_ms: u64,
    pub text: String,
    /// p-tags: the only thing that counts as a mention (inline `@name` is
    /// renderer-dependent).
    pub mentions: u32,
}

/// What the caller knows about one member before any judgement.
#[derive(Debug, Clone)]
pub struct MemberSignal {
    pub subject: SubjectId,
    /// Guestbook join, ms. `None` = unknown; with no first post either, tenure
    /// is Indeterminate and the member is never convicted on tenure.
    pub joined_at_ms: Option<u64>,
    /// Roles held, by eid — shields key on permissions, never on names.
    pub roles: Vec<Hash32>,
    /// Holds a role carrying any moderation permission bit.
    pub is_staff: bool,
}

/// Everything `evaluate` reads. The caller owns fetching, decryption and the
/// clock; the engine owns only arithmetic.
#[derive(Debug, Clone)]
pub struct Signals {
    pub owner: SubjectId,
    pub members: Vec<MemberSignal>,
    /// The corpus, newest-first clamping applied per policy window inside.
    pub messages: Vec<MessageSignal>,
    pub channels: Vec<Hash32>,
    pub relays: Vec<RelayCoverage>,
    pub requested_from: u64,
    pub requested_to: u64,
    pub confirmed_from: u64,
    pub confirmed_to: u64,
    pub roster_version: Hash32,
}

/// A policy plus the hash of the exact bytes it arrived as. The hash is never
/// recomputed from a re-serialization — a client that re-encodes JSON has
/// changed the policy.
#[derive(Debug, Clone)]
pub struct LoadedPolicy {
    pub hash: Hash32,
    pub policy: Policy,
    pub activated_at: Option<u64>,
}

// ── Evaluation ───────────────────────────────────────────────────────────────

/// Evaluate every policy against the signals. Pure: same inputs, same bytes.
pub fn evaluate(signals: &Signals, policies: &[LoadedPolicy], overrides: &[Override], now_ms: u64) -> ModerationReport {
    let mut reports: Vec<PolicyReport> = policies.iter().map(|p| evaluate_one(signals, p, overrides, now_ms)).collect();
    reports.sort_by(|a, b| a.policy_hash.0.cmp(&b.policy_hash.0));

    // Report-level coverage spans the UNION of every evaluated policy's clamped
    // corpus: policies declare different windows, so one field cannot describe
    // just one of them.
    let mut observed_from = u64::MAX;
    let mut observed_to = 0u64;
    let mut per_channel: BTreeMap<[u8; 32], (u32, u64, u64)> = BTreeMap::new();
    for lp in policies {
        for m in clamp_corpus(&signals.messages, &lp.policy, now_ms) {
            observed_from = observed_from.min(m.at_ms);
            observed_to = observed_to.max(m.at_ms);
            let e = per_channel.entry(m.channel.0).or_insert((0, u64::MAX, 0));
            e.0 += 1;
            e.1 = e.1.min(m.at_ms);
            e.2 = e.2.max(m.at_ms);
        }
    }
    let mut channels: Vec<ChannelCoverage> = signals
        .channels
        .iter()
        .map(|c| {
            let (messages, from, to) = per_channel.get(&c.0).copied().unwrap_or((0, 0, 0));
            ChannelCoverage { channel: *c, messages, from: if from == u64::MAX { 0 } else { from }, to }
        })
        .collect();
    channels.sort_by(|a, b| a.channel.0.cmp(&b.channel.0));
    let mut relays = signals.relays.clone();
    relays.sort_by(|a, b| a.url.as_bytes().cmp(b.url.as_bytes()));

    let widest_hours = policies.iter().map(|p| p.policy.window.hours).max().unwrap_or(0);
    let complete = !relays.is_empty()
        && relays.iter().all(|r| r.eose)
        && signals.confirmed_from <= now_ms.saturating_sub(widest_hours.saturating_mul(3_600_000));

    ModerationReport {
        engine_version: caps::ENGINE_VERSION,
        bundle_version: caps::BUNDLE_VERSION,
        roster_version: signals.roster_version,
        override_hash: override_hash(overrides),
        evaluated_at: now_ms,
        window: WindowCoverage {
            requested_from: signals.requested_from,
            requested_to: signals.requested_to,
            confirmed_from: signals.confirmed_from,
            confirmed_to: signals.confirmed_to,
            observed_from: if observed_from == u64::MAX { 0 } else { observed_from },
            observed_to,
            channels,
            relays,
            complete,
        },
        policies: reports,
    }
}

/// The corpus a policy reads: newest `max_messages` within `[now - hours, now)`,
/// ordered by inner timestamp, ties by message id ascending.
fn clamp_corpus<'a>(messages: &'a [MessageSignal], policy: &Policy, now_ms: u64) -> Vec<&'a MessageSignal> {
    let floor = now_ms.saturating_sub(policy.window.hours.saturating_mul(3_600_000));
    let mut in_window: Vec<&MessageSignal> = messages.iter().filter(|m| m.at_ms >= floor && m.at_ms < now_ms).collect();
    in_window.sort_by(|a, b| a.at_ms.cmp(&b.at_ms).then_with(|| a.id.0.cmp(&b.id.0)));
    if in_window.len() > policy.window.max_messages {
        in_window.drain(..in_window.len() - policy.window.max_messages);
    }
    in_window
}

struct RuleOutcome {
    convictions: Vec<Conviction>,
    citations: Vec<Citation>,
    unknown: Vec<SubjectId>,
    state: RuleState,
}

fn evaluate_one(signals: &Signals, lp: &LoadedPolicy, overrides: &[Override], now_ms: u64) -> PolicyReport {
    // An INERT policy evaluated NOTHING, and says so: an empty subject list must
    // never read as "everyone is clean".
    if let Err(reason) = lp.policy.validate() {
        return PolicyReport {
            policy_hash: lp.hash,
            inert: Some(reason),
            activated_at: lp.activated_at,
            coverage_complete: false,
            rule_status: vec![],
            subjects: vec![],
            subjects_truncated: 0,
            citations: vec![],
        };
    }

    let policy = &lp.policy;
    let corpus = clamp_corpus(&signals.messages, policy, now_ms);
    let codes = EmojiCodes::from_policy(policy.emoji_codes.iter());
    let exempt_channels: BTreeSet<[u8; 32]> = policy
        .exempt
        .channels
        .iter()
        .filter_map(|c| crate::simd::hex::hex_to_bytes_32_checked(c))
        .collect();

    // Shields gate BEFORE conviction, and only Protected/Trusted gate.
    let shields = compute_shields(signals, policy, &corpus, &codes, now_ms);

    let mut all_convictions: Vec<Conviction> = Vec::new();
    let mut all_citations: Vec<Citation> = Vec::new();
    let mut rule_status: Vec<RuleStatus> = Vec::new();
    // armed_by counts convictions BEFORE suppression, folding and top-N.
    let mut convicted_by_rule: BTreeMap<&str, BTreeSet<[u8; 32]>> = BTreeMap::new();

    for rule in &policy.rules {
        let out = evaluate_rule(rule, policy, signals, &corpus, &codes, &exempt_channels, &shields, lp, now_ms);
        let mut subjects: BTreeSet<[u8; 32]> = BTreeSet::new();
        for c in &out.convictions {
            subjects.insert(c.subject.0);
        }
        convicted_by_rule.insert(rule.id.as_str(), subjects);
        rule_status.push(RuleStatus { rule_id: rule.id.clone(), state: out.state, unknown_subjects: out.unknown });
        all_convictions.extend(out.convictions);
        all_citations.extend(out.citations);
    }

    // Drop convictions whose rule was armed by another that did not fire.
    all_convictions.retain(|c| {
        let Some(rule) = policy.rules.iter().find(|r| r.id == c.rule_id) else { return true };
        let Some(arm) = &rule.armed_by else { return true };
        match convicted_by_rule.get(arm.rule.as_str()) {
            Some(s) => match arm.scope {
                ArmScope::Subject => s.contains(&c.subject.0),
                ArmScope::Community => s.len() as u32 >= arm.min_subjects.unwrap_or(1),
            },
            None => false,
        }
    });

    // Pardons: reported, never combined, and their citations are suppressed too
    // (otherwise a pardon leaves the content consumer still hiding forgiven
    // messages).
    let mut pardoned: BTreeSet<[u8; 32]> = BTreeSet::new();
    for c in &mut all_convictions {
        if overrides.iter().any(|o| o.matches(c, &lp.hash, now_ms)) {
            c.suppressed = true;
            for cid in &c.citations {
                pardoned.insert(cid.0);
            }
        }
    }
    for cit in &mut all_citations {
        if pardoned.contains(&cit.id.0) {
            cit.suppressed = true;
        }
    }

    // Group by subject, run the frozen pipeline, keep what the report must be
    // able to justify.
    let mut by_subject: BTreeMap<[u8; 32], Vec<Conviction>> = BTreeMap::new();
    for c in all_convictions {
        by_subject.entry(c.subject.0).or_default().push(c);
    }
    let mut subjects: Vec<SubjectReport> = Vec::new();
    for (subject_bytes, mut convictions) in by_subject {
        let subject = SubjectId(subject_bytes);
        let score = run_pipeline(&mut convictions);
        convictions.sort_by(|a, b| a.rule_id.cmp(&b.rule_id).then_with(|| a.scope.cmp(&b.scope)));
        truncate_convictions(&mut convictions);
        subjects.push(SubjectReport {
            subject,
            shield: shields.get(&subject_bytes).copied().unwrap_or(Shield::None),
            confidence: score.confidence,
            proven: score.proven,
            band: score.band,
            convictions,
        });
    }
    // A subject is emitted with any conviction OR a non-None shield: a shielded
    // subject is confidence-0 by construction, so omitting them would delete the
    // field that explains their immunity.
    for (bytes, shield) in &shields {
        if *shield != Shield::None && !subjects.iter().any(|s| s.subject.0 == *bytes) {
            subjects.push(SubjectReport {
                subject: SubjectId(*bytes),
                shield: *shield,
                confidence: 0,
                proven: 0,
                band: super::combine::band(0),
                convictions: vec![],
            });
        }
    }
    subjects.sort_by(|a, b| a.subject.0.cmp(&b.subject.0));

    // Citations are a SET holding exactly what retained convictions reference.
    let retained: BTreeSet<[u8; 32]> =
        subjects.iter().flat_map(|s| s.convictions.iter()).flat_map(|c| c.citations.iter()).map(|c| c.0).collect();
    all_citations.retain(|c| retained.contains(&c.id.0));
    all_citations.sort_by(|a, b| a.id.0.cmp(&b.id.0));
    all_citations.dedup_by(|a, b| a.id == b.id);

    let floor = now_ms.saturating_sub(policy.window.hours.saturating_mul(3_600_000));
    PolicyReport {
        policy_hash: lp.hash,
        inert: None,
        activated_at: lp.activated_at,
        coverage_complete: signals.confirmed_from <= floor && signals.relays.iter().all(|r| r.eose),
        rule_status,
        subjects,
        subjects_truncated: 0,
        citations: all_citations,
    }
}

/// Retain every conviction that entered EITHER pipeline, then fill to the cap
/// from the rest in retain order — a report must always justify both scores.
fn truncate_convictions(convictions: &mut Vec<Conviction>) {
    if convictions.len() <= caps::MAX_CONVICTIONS_STORED_PER_SUBJECT {
        return;
    }
    let mut order: Vec<usize> = (0..convictions.len()).collect();
    order.sort_by(|&a, &b| {
        let (ca, cb) = (&convictions[a], &convictions[b]);
        let ea = ca.combined || ca.proven_combined;
        let eb = cb.combined || cb.proven_combined;
        eb.cmp(&ea)
            .then_with(|| cb.tier_weight.cmp(&ca.tier_weight))
            .then_with(|| ca.rule_id.cmp(&cb.rule_id))
            .then_with(|| ca.scope.cmp(&cb.scope))
    });
    let exempt = order.iter().filter(|&&i| convictions[i].combined || convictions[i].proven_combined).count();
    let keep = exempt.max(caps::MAX_CONVICTIONS_STORED_PER_SUBJECT);
    let keep_set: BTreeSet<usize> = order.into_iter().take(keep).collect();
    let mut i = 0usize;
    convictions.retain(|_| {
        let k = keep_set.contains(&i);
        i += 1;
        k
    });
}

fn compute_shields(
    signals: &Signals,
    policy: &Policy,
    corpus: &[&MessageSignal],
    codes: &EmojiCodes,
    now_ms: u64,
) -> BTreeMap<[u8; 32], Shield> {
    let bar = policy.shields.trusted;
    let mut volume: BTreeMap<[u8; 32], u64> = BTreeMap::new();
    let mut variety: BTreeMap<[u8; 32], BTreeSet<String>> = BTreeMap::new();
    let mut first_post: BTreeMap<[u8; 32], u64> = BTreeMap::new();
    for m in corpus {
        *volume.entry(m.author.0).or_insert(0) += 1;
        let sk = normalize::skeleton(&m.text, codes);
        if !sk.is_empty() {
            variety.entry(m.author.0).or_default().insert(sk);
        }
        let e = first_post.entry(m.author.0).or_insert(u64::MAX);
        *e = (*e).min(m.at_ms);
    }
    let mut out = BTreeMap::new();
    for member in &signals.members {
        let b = member.subject.0;
        // Protected: the owner, plus holders of a moderation permission bit —
        // never mere role membership.
        if member.subject == signals.owner || member.is_staff {
            out.insert(b, Shield::Protected);
            continue;
        }
        // Tenure = now minus the oldest trace (join, or first post if the Join
        // was lost). Unknown only when BOTH are missing.
        let oldest = match (member.joined_at_ms, first_post.get(&b).copied().filter(|v| *v != u64::MAX)) {
            (None, None) => None,
            (a, bb) => Some(a.unwrap_or(u64::MAX).min(bb.unwrap_or(u64::MAX))),
        };
        let Some(oldest) = oldest else {
            out.insert(b, Shield::Indeterminate);
            continue;
        };
        let tenure = now_ms.saturating_sub(oldest) / 1000;
        let vol = volume.get(&b).copied().unwrap_or(0);
        let var = variety.get(&b).map(|s| s.len() as u64).unwrap_or(0);
        // Three paths to standing (§5.3): a role the community granted, long
        // tenure with any activity, or tenure with volume and variety. Every
        // path carries a tenure floor, so none of them is farmable in a day.
        let by_role = bar.roles_trust && !member.roles.is_empty();
        let by_veteran = tenure >= bar.veteran_secs && vol >= 1;
        let by_active = tenure >= bar.tenure_secs && vol >= bar.messages && var >= bar.distinct;
        out.insert(b, if by_role || by_veteran || by_active { Shield::Trusted } else { Shield::None });
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn evaluate_rule(
    rule: &Rule,
    policy: &Policy,
    signals: &Signals,
    corpus: &[&MessageSignal],
    codes: &EmojiCodes,
    exempt_channels: &BTreeSet<[u8; 32]>,
    shields: &BTreeMap<[u8; 32], Shield>,
    lp: &LoadedPolicy,
    _now_ms: u64,
) -> RuleOutcome {
    let mut out =
        RuleOutcome { convictions: vec![], citations: vec![], unknown: vec![], state: RuleState::Evaluated };

    // A declared-but-unimplemented normalizer is unevaluated, never approximated.
    if let Match::Keyword { normalize: n, .. } | Match::Regex { normalize: n, .. } | Match::Repeat { normalize: n } =
        &rule.matcher
    {
        if !normalize::is_available(*n) {
            out.state = RuleState::UnknownType;
            return out;
        }
    }

    let exempt_roles: BTreeSet<[u8; 32]> = policy
        .exempt
        .roles
        .iter()
        .chain(rule.exempt.roles.iter())
        .filter_map(|r| crate::simd::hex::hex_to_bytes_32_checked(r))
        .collect();
    let rule_exempt_channels: BTreeSet<[u8; 32]> = rule
        .exempt
        .channels
        .iter()
        .filter_map(|c| crate::simd::hex::hex_to_bytes_32_checked(c))
        .chain(exempt_channels.iter().copied())
        .collect();
    let exempts: Vec<&ExemptPatterns> = policy.exempt.patterns.iter().chain(rule.exempt.patterns.iter()).collect();

    let subject_exempt = |s: &SubjectId| -> bool {
        signals
            .members
            .iter()
            .find(|m| m.subject == *s)
            .is_some_and(|m| m.roles.iter().any(|r| exempt_roles.contains(&r.0)))
    };

    match &rule.matcher {
        Match::Keyword { .. } | Match::Regex { .. } | Match::Link { .. } | Match::Mentions {} => {
            content_rule(rule, policy, corpus, codes, &rule_exempt_channels, &exempts, shields, lp, &subject_exempt, &mut out);
        }
        Match::TenureLt { secs } => {
            for member in &signals.members {
                if subject_exempt(&member.subject) || gated(shields, &member.subject, rule, 0) {
                    continue;
                }
                match member.joined_at_ms {
                    // Unknown tenure is per-subject unknown — never a conviction.
                    None => out.unknown.push(member.subject),
                    Some(joined) => {
                        let age = _now_ms.saturating_sub(joined) / 1000;
                        if age < *secs {
                            push_direct(rule, lp, member.subject, &mut out);
                        }
                    }
                }
            }
        }
        Match::MessagesLte { n } => {
            for member in &signals.members {
                if subject_exempt(&member.subject) || gated(shields, &member.subject, rule, 0) {
                    continue;
                }
                let count = corpus.iter().filter(|m| m.author == member.subject).count() as u32;
                if count <= *n {
                    push_direct(rule, lp, member.subject, &mut out);
                }
            }
        }
        // Phase 2: the heuristic planes keep running through `raid.rs` until the
        // console swaps over, so they report unevaluated rather than pretending.
        Match::Cohort { .. } | Match::JoinBurst { .. } | Match::Repeat { .. } | Match::Rate { .. } => {
            out.state = RuleState::UnknownType;
        }
    }
    out
}

/// Does a shield stop this rule convicting this subject?
fn gated(shields: &BTreeMap<[u8; 32], Shield>, subject: &SubjectId, rule: &Rule, rung_pierces: usize) -> bool {
    match shields.get(&subject.0).copied().unwrap_or(Shield::None) {
        Shield::Protected => true,
        // Trusted gates EXCEPT against a rung declaring pierces_trusted.
        Shield::Trusted => rung_pierces == 0 && !rule.pierces_trusted,
        Shield::None | Shield::Indeterminate => false,
    }
}

fn push_direct(rule: &Rule, lp: &LoadedPolicy, subject: SubjectId, out: &mut RuleOutcome) {
    let severity = rule.severity.expect("validated: direct form carries a severity");
    let weight = rule.weight.expect("validated: direct form carries a weight");
    out.convictions.push(Conviction {
        id: conviction_id(&lp.hash, &rule.id, Scope::Whole, 0, &subject),
        subject,
        rule_id: rule.id.clone(),
        scope: Scope::Whole,
        rung: 0,
        hits: 1,
        severity,
        basis: rule.matcher.basis(),
        tier_weight: weight,
        // Citation-less by nature: no content timestamps to compare against a
        // policy activation, so the honest answer is Unknown.
        retroactive: Retroactive::Unknown,
        suppressed: false,
        folded: false,
        folded_into: None,
        combined: false,
        proven_combined: false,
        citations: vec![],
        citation_count: 0,
        earliest_citation_at: 0,
        latest_citation_at: 0,
        family: rule.matcher.builtin_family().map(|s| s.to_string()).or_else(|| rule.family.clone()),
        evidence: vec![],
    });
}

#[allow(clippy::too_many_arguments)]
fn content_rule(
    rule: &Rule,
    _policy: &Policy,
    corpus: &[&MessageSignal],
    codes: &EmojiCodes,
    exempt_channels: &BTreeSet<[u8; 32]>,
    exempts: &[&ExemptPatterns],
    shields: &BTreeMap<[u8; 32], Shield>,
    lp: &LoadedPolicy,
    subject_exempt: &dyn Fn(&SubjectId) -> bool,
    out: &mut RuleOutcome,
) {
    let tiers = rule.tiers.as_ref().expect("validated: content rules carry tiers");
    // (subject, message) -> hits, plus the citations each produced.
    let mut per_message: BTreeMap<([u8; 32], [u8; 32]), (u32, u64, Vec<Citation>)> = BTreeMap::new();

    for m in corpus {
        // Exempt content is barred from being CITED, but stays in every corpus
        // statistic — exemptions change who can be accused, never what the
        // community looks like.
        if exempt_channels.contains(&m.channel.0) || subject_exempt(&m.author) {
            continue;
        }
        let (hits, citations) = match &rule.matcher {
            Match::Keyword { patterns, normalize: n } => {
                let text = normalize::apply(&m.text, *n, codes);
                let hits = cancel_exempt_hits(&text, keyword_hits(&text, patterns), exempts);
                let cits = hits
                    .iter()
                    .map(|h| {
                        let target = CitationTarget::Message { message_id: m.id };
                        Citation {
                            id: citation_id(&lp.hash, &rule.id, Scope::PerMessage, &m.author, &target, Some(h.span())),
                            target,
                            at: m.at_ms,
                            span: Some(h.span()),
                            detail: None,
                            suppressed: false,
                        }
                    })
                    .collect::<Vec<_>>();
                (hits.len() as u32, cits)
            }
            Match::Link { patterns } => {
                let domains = link_hits(&m.text, patterns, exempts);
                let target = CitationTarget::Message { message_id: m.id };
                // A link hit is a domain, not a text range: no span.
                let cits = if domains.is_empty() {
                    vec![]
                } else {
                    vec![Citation {
                        id: citation_id(&lp.hash, &rule.id, Scope::PerMessage, &m.author, &target, None),
                        target,
                        at: m.at_ms,
                        span: None,
                        detail: Some(domains.join(", ")),
                        suppressed: false,
                    }]
                };
                (domains.len() as u32, cits)
            }
            Match::Mentions {} => {
                let target = CitationTarget::Message { message_id: m.id };
                let cits = if m.mentions == 0 {
                    vec![]
                } else {
                    vec![Citation {
                        id: citation_id(&lp.hash, &rule.id, Scope::PerMessage, &m.author, &target, None),
                        target,
                        at: m.at_ms,
                        span: None,
                        detail: None,
                        suppressed: false,
                    }]
                };
                (m.mentions, cits)
            }
            // Regex needs the pinned crate build; declared, not yet wired.
            _ => (0, vec![]),
        };
        if hits > 0 {
            per_message.insert((m.author.0, m.id.0), (hits, m.at_ms, citations));
        }
    }

    // Per-subject: every message's own hit count, kept individually so a
    // density rung can cite EVERY message that reached it.
    let mut by_subject: BTreeMap<[u8; 32], Vec<(u32, Vec<Citation>)>> = BTreeMap::new();
    for ((subject, _), (hits, _at, cits)) in per_message {
        by_subject.entry(subject).or_default().push((hits, cits));
    }

    for (subject_bytes, messages) in by_subject {
        let subject = SubjectId(subject_bytes);
        // Density = the densest single message; persistence = the window sum.
        let density: u32 = messages.iter().map(|(h, _)| *h).max().unwrap_or(0);
        let persistence: u32 = messages.iter().map(|(h, _)| *h).sum();

        for (scope, rungs, hits) in [
            (Scope::PerMessage, &tiers.per_message, density),
            (Scope::PerWindow, &tiers.per_window, persistence),
        ] {
            if rungs.is_empty() {
                continue;
            }
            // The highest rung reached: rungs validate strictly ascending.
            let Some((idx, rung)) = rungs.iter().enumerate().filter(|(_, g)| hits >= g.hits).next_back() else {
                continue;
            };
            if gated(shields, &subject, rule, usize::from(rung.pierces_trusted)) {
                continue;
            }
            // A density conviction cites every message whose OWN hit count meets
            // the rung that fired; a persistence conviction cites the whole
            // contributing window.
            let citations: Vec<Citation> = match scope {
                Scope::PerMessage => {
                    messages.iter().filter(|(h, _)| *h >= rung.hits).flat_map(|(_, c)| c.iter().cloned()).collect()
                }
                _ => messages.iter().flat_map(|(_, c)| c.iter().cloned()).collect(),
            };
            push_tiered(rule, lp, subject, scope, idx as u8, hits, rung, citations, out);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn push_tiered(
    rule: &Rule,
    lp: &LoadedPolicy,
    subject: SubjectId,
    scope: Scope,
    rung_idx: u8,
    hits: u32,
    rung: &Rung,
    mut citations: Vec<Citation>,
    out: &mut RuleOutcome,
) {
    citations.sort_by(|a, b| a.id.0.cmp(&b.id.0));
    citations.dedup_by(|a, b| a.id == b.id);
    // Counts and timestamps come from the FULL set, so a truncated exhibit list
    // never reads as fewer offenses nor flips `retroactive`.
    let citation_count = citations.len() as u32;
    let earliest = citations.iter().map(|c| c.at).min().unwrap_or(0);
    let latest = citations.iter().map(|c| c.at).max().unwrap_or(0);
    // Pre-Phase-4 there is no signed activation, so retroactivity is Unknown —
    // never `No`, which a locally-set activation time would fake.
    let retroactive = match lp.activated_at {
        None => Retroactive::Unknown,
        Some(activated) => {
            if citations.is_empty() {
                Retroactive::Unknown
            } else if latest >= activated {
                Retroactive::No
            } else {
                Retroactive::Yes
            }
        }
    };
    citations.truncate(caps::MAX_CITATIONS_PER_CONVICTION);
    let ids: Vec<CitationId> = citations.iter().map(|c| c.id).collect();
    out.citations.extend(citations);
    out.convictions.push(Conviction {
        id: conviction_id(&lp.hash, &rule.id, scope, rung_idx, &subject),
        subject,
        rule_id: rule.id.clone(),
        scope,
        rung: rung_idx,
        hits,
        severity: rung.severity,
        basis: rule.matcher.basis(),
        tier_weight: rung.weight,
        retroactive,
        suppressed: false,
        folded: false,
        folded_into: None,
        combined: false,
        proven_combined: false,
        citations: ids,
        citation_count,
        earliest_citation_at: earliest,
        latest_citation_at: latest,
        family: rule.family.clone(),
        evidence: vec![],
    });
}

/// Which rules a policy would refuse a send under (cooperative prevention).
/// Only Deterministic rules reach here — the validator already refused the rest.
pub fn refusing_rules(policy: &Policy) -> Vec<&Rule> {
    policy.rules.iter().filter(|r| r.enforcement == Enforcement::Refuse).collect()
}

/// Union of a policy-level and rule-level exemption block.
pub fn merged_exempt<'a>(policy: &'a Exempt, rule: &'a Exempt) -> Vec<&'a ExemptPatterns> {
    policy.patterns.iter().chain(rule.patterns.iter()).collect()
}

#[cfg(test)]
mod tests {
    use super::super::document::*;
    use super::*;

    const HOUR: u64 = 3_600_000;
    const NOW: u64 = 1_800_000_000_000;

    fn sid(b: u8) -> SubjectId {
        SubjectId([b; 32])
    }
    fn mid(b: u8) -> MessageId {
        MessageId([b; 32])
    }
    fn ch(b: u8) -> Hash32 {
        Hash32([b; 32])
    }

    fn msg(id: u8, author: u8, at: u64, text: &str) -> MessageSignal {
        MessageSignal { id: mid(id), author: sid(author), channel: ch(0x0a), at_ms: at, text: text.into(), mentions: 0 }
    }

    fn member(b: u8, joined_ago_h: Option<u64>) -> MemberSignal {
        MemberSignal {
            subject: sid(b),
            joined_at_ms: joined_ago_h.map(|h| NOW - h * HOUR),
            roles: vec![],
            is_staff: false,
        }
    }

    fn signals(members: Vec<MemberSignal>, messages: Vec<MessageSignal>) -> Signals {
        Signals {
            owner: sid(0xf0),
            members,
            messages,
            channels: vec![ch(0x0a)],
            relays: vec![RelayCoverage { url: "wss://r".into(), eose: true, events: 10 }],
            requested_from: NOW - 24 * HOUR,
            requested_to: NOW,
            confirmed_from: 0,
            confirmed_to: NOW,
            roster_version: Hash32([0x77; 32]),
        }
    }

    fn loaded(policy: Policy) -> LoadedPolicy {
        LoadedPolicy { hash: Hash32([0x11; 32]), policy, activated_at: None }
    }

    fn policy_with(rules: Vec<Rule>) -> Policy {
        Policy {
            format: FORMAT,
            requires: vec![],
            name: "p".into(),
            emoji_codes: vec![],
            shields: Shields::default(),
            window: Window::default(),
            exempt: Exempt::default(),
            rules,
        }
    }

    fn link_rule() -> Rule {
        Rule {
            id: "links".into(),
            matcher: Match::Link { patterns: vec![] },
            tiers: Some(Tiers {
                per_message: vec![Rung { hits: 1, severity: Severity::Severe, weight: 70, pierces_trusted: false }],
                per_window: vec![Rung { hits: 3, severity: Severity::Severe, weight: 90, pierces_trusted: false }],
            }),
            severity: None,
            weight: None,
            pierces_trusted: false,
            family: None,
            armed_by: None,
            exempt: Exempt {
                patterns: vec![ExemptPatterns {
                    kind: Some(ExemptKind::Domain),
                    values: vec!["vectorapp.io".into()],
                }],
                ..Default::default()
            },
            enforcement: Enforcement::Advisory,
        }
    }

    fn only(report: &ModerationReport) -> &PolicyReport {
        &report.policies[0]
    }

    fn subject<'a>(pr: &'a PolicyReport, b: u8) -> Option<&'a SubjectReport> {
        pr.subjects.iter().find(|s| s.subject == sid(b))
    }

    /// The design's Strict Link Blocker: three links fire BOTH scopes, and the
    /// numbers must match the frozen reference (70 + 90 -> 970pm -> 97).
    #[test]
    fn strict_link_blocker_reproduces_the_reference_vector() {
        let s = signals(
            vec![member(1, Some(2))],
            vec![
                msg(0xb1, 1, NOW - 3 * HOUR, "grab it at bit.ly/a"),
                msg(0xb2, 1, NOW - 2 * HOUR, "also tr.ee/b"),
                msg(0xb3, 1, NOW - HOUR, "and shm.to/c"),
            ],
        );
        let r = evaluate(&s, &[loaded(policy_with(vec![link_rule()]))], &[], NOW);
        let pr = only(&r);
        let sub = subject(pr, 1).expect("the linker is reported");
        assert_eq!((sub.confidence, sub.proven), (97, 97), "both scopes, both Deterministic");
        assert_eq!(sub.band, Band::Alert);
        let scopes: Vec<Scope> = sub.convictions.iter().map(|c| c.scope).collect();
        assert_eq!(scopes, vec![Scope::PerMessage, Scope::PerWindow], "density and persistence both convict");
        assert_eq!(pr.citations.len(), 3, "one citation per offending message");
        assert!(pr.citations.iter().all(|c| c.span.is_none()), "a link hit is a domain, not a text span");
    }

    #[test]
    fn an_allowlisted_domain_never_produces_a_hit() {
        let s = signals(
            vec![member(1, Some(2))],
            vec![msg(0xb1, 1, NOW - HOUR, "read https://vectorapp.io/blog and https://vectorapp.io/x")],
        );
        let r = evaluate(&s, &[loaded(policy_with(vec![link_rule()]))], &[], NOW);
        assert!(subject(only(&r), 1).is_none(), "no conviction, and no subject to report");
        assert!(only(&r).citations.is_empty());
    }

    /// Exempt content is barred from being CITED but stays in every corpus
    /// statistic — the rung arithmetic counts citable hits only.
    #[test]
    fn an_exempt_channel_contributes_no_hits() {
        let mut s = signals(
            vec![member(1, Some(2))],
            vec![msg(0xb1, 1, NOW - HOUR, "bit.ly/a"), msg(0xb2, 1, NOW - HOUR, "tr.ee/b")],
        );
        s.messages[1].channel = ch(0x0b);
        let mut p = policy_with(vec![link_rule()]);
        p.exempt.channels = vec![crate::simd::hex::bytes_to_hex_32(&[0x0b; 32])];
        let r = evaluate(&s, &[loaded(p)], &[], NOW);
        let sub = subject(only(&r), 1).unwrap();
        assert_eq!(sub.convictions.len(), 1, "only the density rung is reached; two links would have been needed");
        assert_eq!(sub.convictions[0].scope, Scope::PerMessage);
        assert_eq!(only(&r).citations.len(), 1, "the exempt channel's message is never cited");
    }

    #[test]
    fn the_soft_swear_ladder_lands_in_review_not_alert() {
        let keyword = Rule {
            id: "swears".into(),
            matcher: Match::Keyword { patterns: vec!["darn".into()], normalize: Normalize::Fold },
            tiers: Some(Tiers {
                per_message: vec![Rung { hits: 1, severity: Severity::Minor, weight: 10, pierces_trusted: false }],
                per_window: vec![Rung { hits: 10, severity: Severity::Severe, weight: 70, pierces_trusted: false }],
            }),
            severity: None,
            weight: None,
            pierces_trusted: false,
            family: None,
            armed_by: None,
            exempt: Exempt::default(),
            enforcement: Enforcement::Advisory,
        };
        let messages: Vec<MessageSignal> =
            (0..10).map(|i| msg(0xc0 + i as u8, 1, NOW - (i + 1) * HOUR, "darn it")).collect();
        let s = signals(vec![member(1, Some(2))], messages);
        let r = evaluate(&s, &[loaded(policy_with(vec![keyword]))], &[], NOW);
        let sub = subject(only(&r), 1).unwrap();
        // 10 + 70 -> 730pm -> 73: the soft policy peaks at human review.
        assert_eq!(sub.confidence, 73);
        assert_eq!(sub.band, Band::Flagged);
    }

    /// Shields gate before conviction: Protected is absolute, Trusted yields
    /// only to a piercing rung, Indeterminate never gates.
    #[test]
    fn shields_gate_exactly_as_declared() {
        let mut s = signals(
            vec![
                member(1, Some(24 * 30)), // long tenure, will be Trusted
                member(2, None),          // no join, no posts elsewhere: Indeterminate
                MemberSignal { subject: sid(3), joined_at_ms: Some(NOW - HOUR), roles: vec![], is_staff: true },
            ],
            vec![],
        );
        // Give the veteran enough varied history to clear the trust bar. The
        // text must be genuinely varied: `skeleton` strips digits, so numbered
        // fixtures collapse to ONE distinct shape and clear nothing.
        const WORDS: [&str; 20] = [
            "morning all", "shipping today", "nice one", "agreed", "on it", "good catch", "thanks",
            "will review", "merged", "looks fine", "same here", "no idea", "try again", "fixed now",
            "welcome aboard", "see you", "sounds good", "my turn", "any thoughts", "done",
        ];
        for (i, w) in WORDS.iter().enumerate() {
            s.messages.push(msg(0x40 + i as u8, 1, NOW - (i as u64 + 1) * HOUR, w));
        }
        s.messages.push(msg(0xd1, 1, NOW - HOUR, "bit.ly/a"));
        s.messages.push(msg(0xd2, 2, NOW - HOUR, "bit.ly/b"));
        s.messages.push(msg(0xd3, 3, NOW - HOUR, "bit.ly/c"));

        let r = evaluate(&s, &[loaded(policy_with(vec![link_rule()]))], &[], NOW);
        let pr = only(&r);
        assert_eq!(subject(pr, 1).unwrap().shield, Shield::Trusted);
        assert!(subject(pr, 1).unwrap().convictions.is_empty(), "a non-piercing rule cannot touch Trusted");
        // A member with no Join but who HAS posted has a known tenure (the
        // oldest trace is their first post), so they are simply un-Trusted.
        assert_eq!(subject(pr, 2).unwrap().shield, Shield::None);
        assert!(!subject(pr, 2).unwrap().convictions.is_empty(), "no shield, ordinary conviction");
        assert_eq!(subject(pr, 3).unwrap().shield, Shield::Protected);
        assert!(subject(pr, 3).unwrap().convictions.is_empty(), "staff are never convicted");

        // The same evidence WITH a piercing rung reaches the Trusted member.
        let mut piercing = link_rule();
        piercing.tiers.as_mut().unwrap().per_message[0].pierces_trusted = true;
        piercing.pierces_trusted = true;
        let r = evaluate(&s, &[loaded(policy_with(vec![piercing]))], &[], NOW);
        assert!(!subject(only(&r), 1).unwrap().convictions.is_empty(), "a piercing rung reaches Trusted");
        assert!(subject(only(&r), 3).unwrap().convictions.is_empty(), "but nothing pierces Protected");
    }

    /// Standing has three doors, and none of them opens in a day.
    #[test]
    fn trust_comes_from_roles_tenure_or_sustained_activity() {
        let day = 24 * HOUR;
        // A role the community granted is a vouch: trusted, though NOT immune
        // (that is Protected, and it keys on moderation permissions).
        let with_role =
            MemberSignal { subject: sid(1), joined_at_ms: Some(NOW - day), roles: vec![ch(0xaa)], is_staff: false };
        let veteran = member(2, Some(24 * 40));
        let active = member(3, Some(24 * 8));
        let lurker = member(4, Some(24 * 8));
        let loud_newcomer = member(5, Some(1));

        let mut s = signals(vec![with_role, veteran, active, lurker, loud_newcomer], vec![]);
        s.messages.push(msg(0x50, 2, NOW - HOUR, "still here"));
        const VARIED: [&str; 5] = ["morning all", "shipping today", "nice one", "agreed", "on it"];
        for (i, w) in VARIED.iter().enumerate() {
            s.messages.push(msg(0x60 + i as u8, 3, NOW - (i as u64 + 1) * HOUR, w));
            s.messages.push(msg(0x70 + i as u8, 5, NOW - (i as u64 + 1) * HOUR, w));
        }

        let r = evaluate(&s, &[loaded(policy_with(vec![link_rule()]))], &[], NOW);
        let pr = only(&r);
        let shield = |b: u8| subject(pr, b).map(|x| x.shield);
        assert_eq!(shield(1), Some(Shield::Trusted), "a granted role vouches");
        assert_eq!(shield(2), Some(Shield::Trusted), "long tenure plus any activity");
        assert_eq!(shield(3), Some(Shield::Trusted), "tenure with volume and variety");
        assert_eq!(shield(4), None, "a silent week earns nothing");
        assert_eq!(shield(5), None, "and chatter cannot buy standing in a day");
    }

    #[test]
    fn fresh_account_aggravators_fold_to_one() {
        let tenure = Rule {
            id: "tenure".into(),
            matcher: Match::TenureLt { secs: 24 * 3600 },
            tiers: None,
            severity: Some(Severity::Notice),
            weight: Some(20),
            pierces_trusted: false,
            family: None,
            armed_by: None,
            exempt: Exempt::default(),
            enforcement: Enforcement::Advisory,
        };
        let quiet = Rule {
            id: "quiet".into(),
            matcher: Match::MessagesLte { n: 2 },
            tiers: None,
            severity: Some(Severity::Notice),
            weight: Some(10),
            pierces_trusted: false,
            family: None,
            armed_by: None,
            exempt: Exempt::default(),
            enforcement: Enforcement::Advisory,
        };
        let s = signals(vec![member(1, Some(2))], vec![msg(0xb1, 1, NOW - HOUR, "hello world")]);
        let r = evaluate(&s, &[loaded(policy_with(vec![tenure, quiet]))], &[], NOW);
        let sub = subject(only(&r), 1).unwrap();
        // Two correlated proxies for "new account" fold to the strongest: 20,
        // not 20-OR-10 = 28.
        assert_eq!(sub.confidence, 20);
        let folded = sub.convictions.iter().find(|c| c.rule_id == "quiet").unwrap();
        assert!(folded.folded && !folded.combined);
        assert_eq!(sub.convictions.iter().filter(|c| c.combined).count(), 1);
    }

    #[test]
    fn unknown_tenure_is_never_a_conviction() {
        let tenure = Rule {
            id: "tenure".into(),
            matcher: Match::TenureLt { secs: 24 * 3600 },
            tiers: None,
            severity: Some(Severity::Notice),
            weight: Some(20),
            pierces_trusted: false,
            family: None,
            armed_by: None,
            exempt: Exempt::default(),
            enforcement: Enforcement::Advisory,
        };
        let s = signals(vec![member(9, None)], vec![]);
        let r = evaluate(&s, &[loaded(policy_with(vec![tenure]))], &[], NOW);
        let pr = only(&r);
        let status = pr.rule_status.iter().find(|s| s.rule_id == "tenure").unwrap();
        assert_eq!(status.unknown_subjects, vec![sid(9)], "per-subject unknown, not a community-wide block");
        assert!(subject(pr, 9).unwrap().convictions.is_empty());
    }

    /// Indeterminate means "we could not establish tenure" — no join AND no
    /// posts — and it is informational: the subject is judged as if unshielded.
    #[test]
    fn indeterminate_tenure_never_gates_a_conviction() {
        let quiet = Rule {
            id: "quiet".into(),
            matcher: Match::MessagesLte { n: 2 },
            tiers: None,
            severity: Some(Severity::Notice),
            weight: Some(10),
            pierces_trusted: false,
            family: None,
            armed_by: None,
            exempt: Exempt::default(),
            enforcement: Enforcement::Advisory,
        };
        let s = signals(vec![member(7, None)], vec![]);
        let r = evaluate(&s, &[loaded(policy_with(vec![quiet]))], &[], NOW);
        let sub = subject(only(&r), 7).unwrap();
        assert_eq!(sub.shield, Shield::Indeterminate, "no join, no posts: unknowable");
        assert_eq!(sub.confidence, 10, "and judged exactly as if unshielded");
    }

    #[test]
    fn a_pardon_suppresses_the_conviction_and_its_citations() {
        let s = signals(
            vec![member(1, Some(2))],
            vec![
                msg(0xb1, 1, NOW - 3 * HOUR, "bit.ly/a"),
                msg(0xb2, 1, NOW - 2 * HOUR, "tr.ee/b"),
                msg(0xb3, 1, NOW - HOUR, "shm.to/c"),
            ],
        );
        let policy = policy_with(vec![link_rule()]);
        let lp = loaded(policy);
        let pardon = Override {
            target: OverrideTarget::Rule {
                policy_hash: lp.hash,
                rule_id: "links".into(),
                scope: Scope::PerMessage,
                subject: sid(1),
            },
            issuer: sid(0xf0),
            issued_at: NOW - HOUR,
            expires_at: NOW + HOUR,
        };
        let r = evaluate(&s, &[lp], &[pardon], NOW);
        let pr = only(&r);
        let sub = subject(pr, 1).unwrap();
        let pardoned = sub.convictions.iter().find(|c| c.scope == Scope::PerMessage).unwrap();
        assert!(pardoned.suppressed && !pardoned.combined, "reported, never combined");
        // The pardoned conviction's citations are suppressed for the content
        // consumer; the surviving per-window conviction keeps citing them.
        assert!(pr.citations.iter().any(|c| c.suppressed), "a pardon reaches the content consumer too");
        // The per-window conviction was NOT pardoned (scope is part of the key).
        assert!(!sub.convictions.iter().find(|c| c.scope == Scope::PerWindow).unwrap().suppressed);
    }

    #[test]
    fn an_inert_policy_reports_why_and_convicts_nothing() {
        let mut p = policy_with(vec![link_rule()]);
        p.requires = vec!["quarantine".into()];
        let r = evaluate(&signals(vec![member(1, Some(2))], vec![]), &[loaded(p)], &[], NOW);
        let pr = only(&r);
        assert!(matches!(&pr.inert, Some(InertReason::UnknownRequiredKey { key }) if key == "quarantine"));
        assert!(pr.subjects.is_empty() && pr.rule_status.is_empty());
    }

    #[test]
    fn evaluation_is_pure_and_ordering_is_canonical() {
        let s = signals(
            vec![member(3, Some(2)), member(1, Some(2)), member(2, Some(2))],
            vec![
                msg(0xb3, 3, NOW - HOUR, "bit.ly/c"),
                msg(0xb1, 1, NOW - 2 * HOUR, "bit.ly/a"),
                msg(0xb2, 2, NOW - 3 * HOUR, "bit.ly/b"),
            ],
        );
        let a = evaluate(&s, &[loaded(policy_with(vec![link_rule()]))], &[], NOW);
        let b = evaluate(&s, &[loaded(policy_with(vec![link_rule()]))], &[], NOW);
        assert_eq!(a, b, "same inputs, same report");
        let ids: Vec<[u8; 32]> = only(&a).subjects.iter().map(|s| s.subject.0).collect();
        let mut sorted = ids.clone();
        sorted.sort();
        assert_eq!(ids, sorted, "subjects sort by canonical id");
        let cits: Vec<[u8; 32]> = only(&a).citations.iter().map(|c| c.id.0).collect();
        let mut cs = cits.clone();
        cs.sort();
        assert_eq!(cits, cs, "citations sort by canonical id");
    }

    #[test]
    fn the_window_clamps_by_recency_and_count() {
        let mut p = policy_with(vec![link_rule()]);
        p.window = Window { hours: 2, max_messages: 2 };
        let s = signals(
            vec![member(1, Some(48))],
            vec![
                msg(0xb0, 1, NOW - 10 * HOUR, "bit.ly/old"), // outside the window
                msg(0xb1, 1, NOW - 90 * 60 * 1000, "bit.ly/a"),
                msg(0xb2, 1, NOW - 60 * 60 * 1000, "bit.ly/b"),
                msg(0xb3, 1, NOW - 30 * 60 * 1000, "bit.ly/c"),
            ],
        );
        let r = evaluate(&s, &[loaded(p)], &[], NOW);
        assert_eq!(only(&r).citations.len(), 2, "newest two inside the window; the older two never enter the corpus");
    }
}
