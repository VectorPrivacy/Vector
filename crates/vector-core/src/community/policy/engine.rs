//! `evaluate` — the pure conviction engine (§1, §3.2, §5).
//!
//! Signals in, report out. No I/O, no keys, no capability checks, no clock: the
//! caller passes `now`. Two clients holding the same inputs reach byte-identical
//! conclusions, which is the whole reason the engine convicts and never
//! sentences.

use super::combine::run_pipeline;
use super::document::{ArmScope, Enforcement, Exempt, ExemptPatterns, Match, Policy, Ratio, Rule, Rung};
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
    /// Everything they have ever said here, not just inside the window.
    /// Standing is historical by nature: a regular of two years who had a quiet
    /// week is still a regular, and a window-only count would make them re-earn
    /// their standing every week. A declared input, so the engine stays pure.
    pub lifetime_messages: u64,
    /// Their FIRST post ever, ms. Tenure is the oldest trace of someone, and
    /// the window can only see a few days of it — a member whose Join was lost
    /// (or who joined long after they started posting) would otherwise have
    /// their tenure capped at the window's width and read as a newcomer.
    pub first_post_ms: Option<u64>,
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

/// Who is evaluating. A member's client runs only stateless rules — word and
/// link filters over the message in front of it — because everything else needs
/// history it may not hold and authority it does not have. Admins and
/// moderation bots run everything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvalMode {
    Member,
    Admin,
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
    evaluate_as(signals, policies, overrides, now_ms, EvalMode::Admin)
}

/// [`evaluate`] with an explicit evaluator role.
pub fn evaluate_as(
    signals: &Signals,
    policies: &[LoadedPolicy],
    overrides: &[Override],
    now_ms: u64,
    mode: EvalMode,
) -> ModerationReport {
    let mut reports: Vec<PolicyReport> =
        policies.iter().map(|p| evaluate_one(signals, p, overrides, now_ms, mode)).collect();
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

fn evaluate_one(
    signals: &Signals,
    lp: &LoadedPolicy,
    overrides: &[Override],
    now_ms: u64,
    mode: EvalMode,
) -> PolicyReport {
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

    // Only the people who are HERE can be convicted, or arm anything. The
    // corpus outlives membership — a banned raider's messages stay in local
    // storage long after the rotation removed them — so without this the engine
    // re-litigates people a moderator cannot act on, and last night's attack
    // keeps arming rules against whoever is still here today. Scores are
    // present tense; so is the evidence behind them.
    let member_set: BTreeSet<[u8; 32]> = signals.members.iter().map(|m| m.subject.0).collect();

    for rule in &policy.rules {
        let mut out = evaluate_rule(rule, policy, signals, &corpus, &codes, &exempt_channels, &shields, lp, now_ms, mode);
        out.convictions.retain(|c| member_set.contains(&c.subject.0));
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
        // Any one of the named rules arms it: an aggravator that hangs off a
        // single detector goes silent for every raid that detector cannot see.
        std::iter::once(&arm.rule).chain(arm.also.iter()).any(|id| {
            match convicted_by_rule.get(id.as_str()) {
                Some(s) => match arm.scope {
                    ArmScope::Subject => s.contains(&c.subject.0),
                    ArmScope::Community => s.len() as u32 >= arm.min_subjects.unwrap_or(1),
                },
                None => false,
            }
        })
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
        // Tenure = now minus the OLDEST trace of them: their Join, their first
        // post ever, or the earliest thing they said inside the window —
        // whichever reaches furthest back. Unknown only when nothing does.
        let window_first = first_post.get(&b).copied().filter(|v| *v != u64::MAX);
        let candidates = [member.joined_at_ms, member.first_post_ms, window_first];
        let oldest = candidates.into_iter().flatten().min();
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
        // Volume is LIFETIME (standing is historical); variety is the window's
        // (how someone speaks now is what tells a person from a script).
        let lifetime = member.lifetime_messages.max(vol);
        let by_role = bar.roles_trust && !member.roles.is_empty();
        let by_veteran = tenure >= bar.veteran_secs && lifetime >= 1;
        let by_active = tenure >= bar.tenure_secs && lifetime >= bar.messages && var >= bar.distinct;
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
    mode: EvalMode,
) -> RuleOutcome {
    let mut out =
        RuleOutcome { convictions: vec![], citations: vec![], unknown: vec![], state: RuleState::Evaluated };

    // A member's client judges only what it can judge from one message. Saying
    // so is the point: a silent skip would read as "clean".
    if mode == EvalMode::Member && !rule.matcher.is_stateless() {
        out.state = RuleState::RequiresHistory;
        return out;
    }

    // A declared-but-unimplemented normalizer is unevaluated, never approximated.
    if let Match::Keyword { normalize: n, .. } | Match::Regex { normalize: n, .. } | Match::Repeat { normalize: n, .. } =
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
        Match::Repeat { .. } | Match::Rate { .. } => {
            window_rule(rule, corpus, codes, &rule_exempt_channels, shields, lp, &subject_exempt, &mut out);
        }
        Match::Cohort { min, quiet_max, short_factor, thin_ratio } => {
            cohort_rule(
                rule, corpus, codes, &rule_exempt_channels, shields, lp, &subject_exempt, signals,
                *min, *quiet_max, *short_factor, thin_ratio.unwrap_or(Ratio { num: 1, denom: 2 }), &mut out,
            );
        }
        Match::JoinBurst { gap_secs, min } => {
            join_burst_rule(rule, signals, shields, lp, &subject_exempt, *gap_secs, *min, &mut out);
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

fn push_direct_with(
    rule: &Rule,
    lp: &LoadedPolicy,
    subject: SubjectId,
    mut citations: Vec<Citation>,
    out: &mut RuleOutcome,
) {
    citations.sort_by(|a, b| a.id.0.cmp(&b.id.0));
    citations.dedup_by(|a, b| a.id == b.id);
    let citation_count = citations.len() as u32;
    let earliest = citations.iter().map(|c| c.at).min().unwrap_or(0);
    let latest = citations.iter().map(|c| c.at).max().unwrap_or(0);
    citations.truncate(caps::MAX_CITATIONS_PER_CONVICTION);
    let ids: Vec<CitationId> = citations.iter().map(|c| c.id).collect();
    out.citations.extend(citations);
    push_direct(rule, lp, subject, out);
    if let Some(c) = out.convictions.last_mut() {
        c.citations = ids;
        c.citation_count = citation_count;
        c.earliest_citation_at = earliest;
        c.latest_citation_at = latest;
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
    // The needle has to live in the same space as the haystack. Message text is
    // normalized below; a pattern that is not gets compared against text it can
    // never equal, so "Vector" silently matched nothing while "vector" matched.
    // Hoisted out of the corpus loop: this does not vary per message.
    let folded_patterns: Vec<String> = match &rule.matcher {
        Match::Keyword { patterns, normalize: n } => {
            patterns.iter().map(|p| normalize::apply(p, *n, codes)).collect()
        }
        _ => Vec::new(),
    };
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
            Match::Keyword { normalize: n, .. } => {
                let text = normalize::apply(&m.text, *n, codes);
                let hits = cancel_exempt_hits(&text, keyword_hits(&text, &folded_patterns), exempts);
                let cits = hits
                    .iter()
                    .map(|h| {
                        let target = CitationTarget::Message { message_id: m.id };
                        // Carry WHAT matched. A moderator reading "matched rule
                        // words" learns nothing; reading the word itself can
                        // judge in a second whether the rule is right.
                        let matched: String = text
                            .get(h.start..h.end)
                            .unwrap_or_default()
                            .chars()
                            .take(caps::MAX_DETAIL_LEN)
                            .collect();
                        Citation {
                            id: citation_id(&lp.hash, &rule.id, Scope::PerMessage, &m.author, &target, Some(h.span())),
                            target,
                            at: m.at_ms,
                            span: Some(h.span()),
                            detail: (!matched.is_empty()).then_some(matched),
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

/// Cross-account clustering: many identities posting one shape.
///
/// A cluster convicts on TWO counts, never one. Size alone would hang a
/// community catchphrase on whoever repeats it, so the cluster must also be
/// THIN — a script gives each identity a line or two, while regulars who share
/// a greeting have hundreds of other messages between them. A short shape is
/// cheap coincidence on top of that, so it clears a higher size bar.
///
/// Statistics read the FULL corpus: exempt members and channels still count
/// toward cluster size and thinness (exemptions change who can be accused,
/// never what the community looks like), they simply cannot be convicted.
#[allow(clippy::too_many_arguments)]
fn cohort_rule(
    rule: &Rule,
    corpus: &[&MessageSignal],
    codes: &EmojiCodes,
    exempt_channels: &BTreeSet<[u8; 32]>,
    shields: &BTreeMap<[u8; 32], Shield>,
    lp: &LoadedPolicy,
    subject_exempt: &dyn Fn(&SubjectId) -> bool,
    signals: &Signals,
    min: u32,
    quiet_max: u32,
    short_factor: u32,
    thin_ratio: Ratio,
    out: &mut RuleOutcome,
) {
    // skeleton -> the distinct authors who posted it, plus their messages.
    let mut clusters: BTreeMap<String, BTreeMap<[u8; 32], Vec<&MessageSignal>>> = BTreeMap::new();
    for m in corpus {
        let key = normalize::skeleton(&m.text, codes);
        if key.is_empty() {
            continue;
        }
        clusters.entry(key).or_default().entry(m.author.0).or_default().push(m);
    }
    // Window volume per author — the thinness input.
    let mut volume: BTreeMap<[u8; 32], u32> = BTreeMap::new();
    for m in corpus {
        *volume.entry(m.author.0).or_insert(0) += 1;
    }

    // Each subject is convicted once (scope Whole) on the LARGEST cluster they
    // belong to; ties break by clustering key ascending.
    let mut best: BTreeMap<[u8; 32], (usize, String)> = BTreeMap::new();
    for (key, authors) in &clusters {
        let need = if key.chars().count() < caps::MIN_SKELETON_LEN {
            min.saturating_mul(short_factor)
        } else {
            min
        } as usize;
        if authors.len() < need {
            continue;
        }
        let thin = authors.keys().filter(|a| volume.get(*a).copied().unwrap_or(0) <= quiet_max).count();
        if (thin as u64) * (thin_ratio.denom as u64) < (authors.len() as u64) * (thin_ratio.num as u64) {
            continue;
        }
        for author in authors.keys() {
            let slot = best.entry(*author).or_insert((0, String::new()));
            if authors.len() > slot.0 {
                *slot = (authors.len(), key.clone());
            }
        }
    }

    for (author_bytes, (size, key)) in best {
        let subject = SubjectId(author_bytes);
        if subject_exempt(&subject) || gated(shields, &subject, rule, usize::from(rule.pierces_trusted)) {
            continue;
        }
        let messages = clusters.get(&key).and_then(|a| a.get(&author_bytes)).cloned().unwrap_or_default();
        // Exempt content is barred from being cited even though it counted
        // toward the cluster's shape.
        let citable: Vec<&MessageSignal> =
            messages.iter().copied().filter(|m| !exempt_channels.contains(&m.channel.0)).collect();
        if citable.is_empty() {
            continue;
        }
        let citations: Vec<Citation> = citable
            .iter()
            .map(|m| {
                let target = CitationTarget::Message { message_id: m.id };
                Citation {
                    id: citation_id(&lp.hash, &rule.id, Scope::Whole, &subject, &target, None),
                    target,
                    at: m.at_ms,
                    span: None,
                    detail: None,
                    suppressed: false,
                }
            })
            .collect();
        // The sample is the lowest inner message id in the cluster, excluding
        // exempt members and channels: `peers` already omits them, and a sample
        // would leak the same accusation the report may not make.
        let sample = clusters
            .get(&key)
            .map(|authors| {
                let mut candidates: Vec<&MessageSignal> = authors
                    .iter()
                    .filter(|(a, _)| !subject_exempt(&SubjectId(**a)))
                    .flat_map(|(_, ms)| ms.iter().copied())
                    .filter(|m| !exempt_channels.contains(&m.channel.0))
                    .collect();
                candidates.sort_by(|a, b| a.id.0.cmp(&b.id.0));
                candidates.first().map(|m| m.text.clone()).unwrap_or_default()
            })
            .unwrap_or_default();
        let mut peers: Vec<SubjectId> = clusters
            .get(&key)
            .map(|a| a.keys().filter(|k| **k != author_bytes).map(|k| SubjectId(*k)).collect())
            .unwrap_or_default();
        peers.sort_by(|a, b| a.0.cmp(&b.0));
        peers.truncate(caps::COHORT_SAMPLE_CAP);
        use sha2::Digest;
        let mut hasher = sha2::Sha256::default();
        hasher.update(key.as_bytes());
        let evidence = vec![Evidence::Cohort {
            skeleton_hash: Hash32(hasher.finalize().into()),
            sample: sample.chars().take(caps::MAX_SAMPLE_LEN).collect(),
            size: size as u32,
            peers,
        }];
        let before = out.convictions.len();
        push_direct_with(rule, lp, subject, citations, out);
        if let Some(c) = out.convictions.get_mut(before) {
            c.evidence = evidence;
        }
    }
    let _ = signals;
}

/// The densest run of joins inside one window. Deliberately weak alone: a
/// freshly-posted invite link is SUPPOSED to bring a burst of quiet newcomers,
/// so this is normally `armed_by` a cohort rule and convicts nobody until
/// something else already has.
#[allow(clippy::too_many_arguments)]
fn join_burst_rule(
    rule: &Rule,
    signals: &Signals,
    shields: &BTreeMap<[u8; 32], Shield>,
    lp: &LoadedPolicy,
    subject_exempt: &dyn Fn(&SubjectId) -> bool,
    gap_secs: u64,
    min: u32,
    out: &mut RuleOutcome,
) {
    // The owner is never part of a burst, and an unknown join is not evidence
    // of one.
    let mut joins: Vec<(u64, SubjectId)> = signals
        .members
        .iter()
        .filter(|m| m.subject != signals.owner)
        .filter_map(|m| m.joined_at_ms.map(|at| (at, m.subject)))
        .collect();
    joins.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1 .0.cmp(&b.1 .0)));
    let gap_ms = gap_secs.saturating_mul(1000);
    let (mut best_start, mut best_len) = (0usize, 0usize);
    let mut start = 0usize;
    for end in 0..joins.len() {
        while joins[end].0.saturating_sub(joins[start].0) > gap_ms {
            start += 1;
        }
        // Strictly greater: on a tie the EARLIEST window wins, which is where a
        // raid starts.
        if end + 1 - start > best_len {
            best_len = end + 1 - start;
            best_start = start;
        }
    }
    if best_len < min as usize {
        return;
    }
    let window = &joins[best_start..best_start + best_len];
    let evidence = vec![Evidence::Burst {
        from: window.first().map(|(at, _)| *at).unwrap_or(0),
        to: window.last().map(|(at, _)| *at).unwrap_or(0),
        size: best_len as u32,
    }];
    for (_, subject) in window {
        if subject_exempt(subject) || gated(shields, subject, rule, usize::from(rule.pierces_trusted)) {
            continue;
        }
        let before = out.convictions.len();
        push_direct(rule, lp, *subject, out);
        if let Some(c) = out.convictions.get_mut(before) {
            c.evidence = evidence.clone();
        }
    }
}

/// Window-level rules: one conviction per subject, scope PerWindow.
///
///  * `repeat` — the most-repeated normalized text by that author; ties break by
///    the normalized key ascending, so the citation set is reproducible. This is
///    the copy-paste counter that catches one account spamming, where `cohort`
///    catches many accounts sharing one line.
///  * `rate` — the densest half-open `[t, t + per_secs)` span over the author's
///    own message timestamps; ties take the EARLIEST span.
#[allow(clippy::too_many_arguments)]
fn window_rule(
    rule: &Rule,
    corpus: &[&MessageSignal],
    codes: &EmojiCodes,
    exempt_channels: &BTreeSet<[u8; 32]>,
    shields: &BTreeMap<[u8; 32], Shield>,
    lp: &LoadedPolicy,
    subject_exempt: &dyn Fn(&SubjectId) -> bool,
    out: &mut RuleOutcome,
) {
    let tiers = rule.tiers.as_ref().expect("validated: window rules carry tiers");
    let rungs = &tiers.per_window;
    if rungs.is_empty() {
        return;
    }
    // Citable content only: exempt content still counts toward the corpus
    // statistics every other rule reads, but it can never be cited here.
    let mut by_author: BTreeMap<[u8; 32], Vec<&MessageSignal>> = BTreeMap::new();
    for m in corpus {
        if exempt_channels.contains(&m.channel.0) || subject_exempt(&m.author) {
            continue;
        }
        by_author.entry(m.author.0).or_default().push(m);
    }

    for (subject_bytes, messages) in by_author {
        let subject = SubjectId(subject_bytes);
        let (hits, cited, evidence) = match &rule.matcher {
            Match::Repeat { normalize: n, within_secs } => {
                let mut groups: BTreeMap<String, Vec<&MessageSignal>> = BTreeMap::new();
                for m in &messages {
                    let key = normalize::apply(&m.text, *n, codes);
                    if key.is_empty() {
                        continue;
                    }
                    groups.entry(key).or_default().push(m);
                }
                // With a span, each group counts only its densest run inside it,
                // and the winner is the tightest burst rather than the biggest
                // pile. "gm" once a morning is seven hits across a week and one
                // hit in any half hour, which is the difference between a
                // regular and a spammer.
                let scored: Vec<(String, u32, Vec<&MessageSignal>)> = groups
                    .into_iter()
                    .map(|(key, mut ms)| {
                        let Some(secs) = within_secs else {
                            let n = ms.len() as u32;
                            return (key, n, ms);
                        };
                        ms.sort_by_key(|m| m.at_ms);
                        let span = secs.saturating_mul(1000);
                        let (mut best, mut from) = (0usize, 0u64);
                        for (i, m) in ms.iter().enumerate() {
                            // Half-open [start, start + span), starts taken from
                            // the author's own timestamps — same walk as `Rate`.
                            let start = m.at_ms;
                            let count =
                                ms[i..].iter().take_while(|x| x.at_ms < start.saturating_add(span)).count();
                            if count > best {
                                best = count;
                                from = start;
                            }
                        }
                        let cited: Vec<&MessageSignal> = ms
                            .into_iter()
                            .filter(|m| m.at_ms >= from && m.at_ms < from.saturating_add(span))
                            .collect();
                        (key, best as u32, cited)
                    })
                    .collect();
                // Most repeated wins; ties by the normalized key ascending.
                let Some((_key, n_hits, winners)) =
                    scored.into_iter().max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.cmp(&a.0)))
                else {
                    continue;
                };
                (n_hits, winners, vec![])
            }
            Match::Rate { per_secs } => {
                let mut times: Vec<u64> = messages.iter().map(|m| m.at_ms).collect();
                times.sort_unstable();
                let span = per_secs.saturating_mul(1000);
                let (mut best, mut best_from) = (0usize, 0u64);
                for (i, &start) in times.iter().enumerate() {
                    // Half-open [start, start + span); candidate starts are the
                    // author's own timestamps, never every millisecond.
                    let count = times[i..].iter().take_while(|&&t| t < start.saturating_add(span)).count();
                    if count > best {
                        best = count;
                        best_from = start;
                    }
                }
                let cited: Vec<&MessageSignal> = messages
                    .iter()
                    .copied()
                    .filter(|m| m.at_ms >= best_from && m.at_ms < best_from.saturating_add(span))
                    .collect();
                (
                    best as u32,
                    cited,
                    vec![Evidence::Rate { window_secs: *per_secs, count: best as u32, from: best_from }],
                )
            }
            _ => continue,
        };

        let Some((idx, rung)) = rungs.iter().enumerate().filter(|(_, g)| hits >= g.hits).next_back() else {
            continue;
        };
        if gated(shields, &subject, rule, usize::from(rung.pierces_trusted)) {
            continue;
        }
        let citations: Vec<Citation> = cited
            .iter()
            .map(|m| {
                let target = CitationTarget::Message { message_id: m.id };
                Citation {
                    id: citation_id(&lp.hash, &rule.id, Scope::PerWindow, &subject, &target, None),
                    target,
                    at: m.at_ms,
                    span: None,
                    detail: None,
                    suppressed: false,
                }
            })
            .collect();
        let before = out.convictions.len();
        push_tiered(rule, lp, subject, Scope::PerWindow, idx as u8, hits, rung, citations, out);
        if let Some(c) = out.convictions.get_mut(before) {
            c.evidence = evidence;
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
            lifetime_messages: 0,
            first_post_ms: None,
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
                MemberSignal { subject: sid(3), joined_at_ms: Some(NOW - HOUR), roles: vec![], is_staff: true, lifetime_messages: 0, first_post_ms: None },
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
            MemberSignal { subject: sid(1), joined_at_ms: Some(NOW - day), roles: vec![ch(0xaa)], is_staff: false, lifetime_messages: 0, first_post_ms: None };
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

    /// A regular of months who had a quiet week keeps their standing: volume is
    /// lifetime, not "what have you said for me lately".
    #[test]
    fn a_quiet_week_does_not_cost_a_regular_their_standing() {
        let mut veteran = member(6, Some(24 * 10));
        veteran.lifetime_messages = 900;
        let mut newcomer = member(7, Some(2));
        newcomer.lifetime_messages = 900; // history it could not possibly have
        let mut s = signals(vec![veteran, newcomer], vec![]);
        // Three distinct shapes this week is all the variety bar asks for.
        for (i, w) in ["morning all", "shipping today", "nice one"].iter().enumerate() {
            s.messages.push(msg(0x80 + i as u8, 6, NOW - (i as u64 + 1) * HOUR, w));
            s.messages.push(msg(0x90 + i as u8, 7, NOW - (i as u64 + 1) * HOUR, w));
        }
        let r = evaluate(&s, &[loaded(policy_with(vec![link_rule()]))], &[], NOW);
        let pr = only(&r);
        assert_eq!(subject(pr, 6).map(|x| x.shield), Some(Shield::Trusted), "ten days and a long history");
        assert_eq!(subject(pr, 7).map(|x| x.shield), None, "tenure still gates: two hours buys nothing");
    }

    /// The fifth member to join, very active early, then quiet for months.
    /// Still trusted: standing is what you built, not what you said this week.
    #[test]
    fn an_early_member_who_lurks_for_months_keeps_their_standing() {
        let mut lurker = member(8, Some(24 * 200));
        lurker.lifetime_messages = 400; // all of it long ago
        let s = signals(vec![lurker], vec![]); // nothing at all in the window
        let r = evaluate(&s, &[loaded(policy_with(vec![link_rule()]))], &[], NOW);
        assert_eq!(subject(only(&r), 8).map(|x| x.shield), Some(Shield::Trusted));
    }

    /// A member's client judges word and link filters on the message in front
    /// of it, and nothing that needs history. It says which is which, because a
    /// silent skip would read as "clean".
    fn cohort_rule_doc() -> Rule {
        Rule {
            id: "cohort".into(),
            matcher: Match::Cohort { min: 3, quiet_max: 2, short_factor: 3, thin_ratio: None },
            tiers: None,
            severity: Some(Severity::Severe),
            weight: Some(85),
            pierces_trusted: false,
            family: None,
            armed_by: None,
            exempt: Exempt::default(),
            enforcement: Enforcement::Advisory,
        }
    }

    /// The shape of the 2026-08-19 raid: a hundred fresh identities, one line
    /// each. This is the test the whole engine exists to pass.
    #[test]
    fn a_hundred_fresh_accounts_saying_one_thing_is_a_raid() {
        let mut members: Vec<MemberSignal> = (0..100).map(|i| member(i as u8 + 10, Some(1))).collect();
        let mut messages: Vec<MessageSignal> =
            (0..100).map(|i| msg(0x10 + i as u8, i as u8 + 10, NOW - HOUR, "hello world")).collect();
        // A regular with real history, saying the same words in passing.
        members.push(member(2, Some(24 * 60)));
        members.last_mut().unwrap().lifetime_messages = 800;
        messages.push(msg(0xf1, 2, NOW - 2 * HOUR, "hello world"));
        for (i, w) in ["morning all", "shipping today", "nice one"].iter().enumerate() {
            messages.push(msg(0xf2 + i as u8, 2, NOW - (i as u64 + 3) * HOUR, w));
        }

        let s = signals(members, messages);
        let r = evaluate(&s, &[loaded(policy_with(vec![cohort_rule_doc()]))], &[], NOW);
        let pr = only(&r);
        let convicted: Vec<&SubjectReport> =
            pr.subjects.iter().filter(|x| x.convictions.iter().any(|c| !c.suppressed)).collect();
        assert_eq!(convicted.len(), 100, "every raider, and only the raiders");
        assert!(convicted.iter().all(|x| x.confidence == 85 && x.band == Band::Alert));
        // Heuristic evidence never reaches `proven`: a human still decides.
        assert!(convicted.iter().all(|x| x.proven == 0), "a cohort is inference, not proof");
        assert_eq!(subject(pr, 2).map(|x| x.shield), Some(Shield::Trusted), "the regular is shielded, not convicted");
        assert!(subject(pr, 2).unwrap().convictions.is_empty());

        let ev = &convicted[0].convictions[0].evidence;
        assert!(
            matches!(ev.first(), Some(Evidence::Cohort { size: 101, .. })),
            "the exhibit names the true cluster size: {ev:?}"
        );
    }

    /// The corpus outlives membership: a banned raider's messages sit in local
    /// storage long after the rotation removed them.
    #[test]
    fn someone_already_removed_is_never_convicted_again() {
        // Twenty identities posted one line each; only three are still members.
        let members: Vec<MemberSignal> = (0..3).map(|i| member(i as u8 + 10, Some(1))).collect();
        let messages: Vec<MessageSignal> =
            (0..20).map(|i| msg(0x10 + i as u8, i as u8 + 10, NOW - HOUR, "free airdrop claim now")).collect();
        let s = signals(members, messages);
        let r = evaluate(&s, &[loaded(policy_with(vec![cohort_rule_doc()]))], &[], NOW);
        let pr = only(&r);
        assert_eq!(pr.subjects.len(), 3, "the seventeen who are gone are not re-litigated");
        assert!(pr.subjects.iter().all(|x| !x.convictions.is_empty()), "the three still here are convicted");
        // The cluster's true size still counts all twenty: exemption and
        // absence change who can be accused, never what the community saw.
        let ev = &pr.subjects[0].convictions[0].evidence;
        assert!(matches!(ev.first(), Some(Evidence::Cohort { size: 20, .. })), "{ev:?}");
    }

    /// Arming asks whether a raid is happening HERE and NOW, not whether one
    /// ever happened: a cohort of accounts that are all gone must not arm a
    /// burst rule against whoever is still present.
    #[test]
    fn a_past_raid_does_not_arm_a_rule_against_the_living() {
        let mut burst = Rule {
            id: "burst".into(),
            matcher: Match::JoinBurst { gap_secs: 600, min: 5 },
            tiers: None,
            severity: Some(Severity::Major),
            weight: Some(40),
            pierces_trusted: false,
            family: None,
            armed_by: Some(ArmedBy { rule: "cohort".into(), scope: ArmScope::Community, min_subjects: Some(3), also: vec![] }),
            exempt: Exempt::default(),
            enforcement: Enforcement::Advisory,
        };
        // Twenty raiders posted one line each and were removed; six ordinary
        // members remain, all of whom joined in one burst.
        let members: Vec<MemberSignal> = (0..6)
            .map(|i| MemberSignal {
                subject: sid(i as u8 + 40),
                joined_at_ms: Some(NOW - HOUR + i * 30_000),
                roles: vec![],
                is_staff: false,
                lifetime_messages: 0,
                first_post_ms: None,
            })
            .collect();
        let messages: Vec<MessageSignal> =
            (0..20).map(|i| msg(0x10 + i as u8, i as u8 + 10, NOW - HOUR, "free airdrop claim now")).collect();
        let s = signals(members, messages);
        let r = evaluate(&s, &[loaded(policy_with(vec![cohort_rule_doc(), burst.clone()]))], &[], NOW);
        assert_eq!(
            only(&r).subjects.iter().filter(|x| !x.convictions.is_empty()).count(),
            0,
            "the cohort is entirely gone, so it arms nothing against the living"
        );

        // The same burst DOES fire when the cohort is among the current members.
        burst.armed_by = Some(ArmedBy { rule: "cohort".into(), scope: ArmScope::Community, min_subjects: Some(3), also: vec![] });
        let mut members: Vec<MemberSignal> = (0..6)
            .map(|i| MemberSignal {
                subject: sid(i as u8 + 40),
                joined_at_ms: Some(NOW - HOUR + i * 30_000),
                roles: vec![],
                is_staff: false,
                lifetime_messages: 0,
                first_post_ms: None,
            })
            .collect();
        members.extend((0..20).map(|i| member(i as u8 + 10, Some(1))));
        let messages: Vec<MessageSignal> =
            (0..20).map(|i| msg(0x10 + i as u8, i as u8 + 10, NOW - HOUR, "free airdrop claim now")).collect();
        let s = signals(members, messages);
        let r = evaluate(&s, &[loaded(policy_with(vec![cohort_rule_doc(), burst]))], &[], NOW);
        let convicted = only(&r).subjects.iter().filter(|x| !x.convictions.is_empty()).count();
        assert!(convicted >= 20, "a live cohort arms it: {convicted}");
    }

    /// Size alone must never convict: a catchphrase everyone repeats is a
    /// community, and the thinness bar is what tells it from a script.
    #[test]
    fn a_community_catchphrase_is_not_a_cohort() {
        let members: Vec<MemberSignal> = (0..20)
            .map(|i| {
                let mut m = member(i as u8 + 10, Some(24 * 60));
                m.lifetime_messages = 500;
                m
            })
            .collect();
        let mut messages: Vec<MessageSignal> = Vec::new();
        for i in 0..20u8 {
            // Everyone says it — and everyone also says plenty else.
            messages.push(msg(0x20 + i, i + 10, NOW - HOUR, "good morning everyone"));
            for (j, w) in ["shipping today", "nice one", "agreed", "on it"].iter().enumerate() {
                messages.push(msg(0x60 + i * 4 + j as u8, i + 10, NOW - (j as u64 + 2) * HOUR, w));
            }
        }
        let s = signals(members, messages);
        let r = evaluate(&s, &[loaded(policy_with(vec![cohort_rule_doc()]))], &[], NOW);
        let convicted = only(&r).subjects.iter().filter(|x| !x.convictions.is_empty()).count();
        assert_eq!(convicted, 0, "twenty chatty regulars sharing a greeting are not a raid");
    }

    /// A burst of quiet newcomers is what a freshly-posted invite link LOOKS
    /// like, so it convicts nobody until a cohort already has.
    #[test]
    fn an_invite_link_burst_convicts_nobody_on_its_own() {
        let mut burst = Rule {
            id: "burst".into(),
            matcher: Match::JoinBurst { gap_secs: 600, min: 5 },
            tiers: None,
            severity: Some(Severity::Major),
            weight: Some(40),
            pierces_trusted: false,
            family: None,
            armed_by: None,
            exempt: Exempt::default(),
            enforcement: Enforcement::Advisory,
        };
        // Ten people joined within a few minutes and said nothing alike.
        let members: Vec<MemberSignal> = (0..10)
            .map(|i| MemberSignal {
                subject: sid(i as u8 + 10),
                joined_at_ms: Some(NOW - HOUR + i * 30_000),
                roles: vec![],
                is_staff: false,
                lifetime_messages: 0,
                first_post_ms: None,
            })
            .collect();
        let s = signals(members, vec![]);

        // Unarmed, the burst convicts them all — which is why the built-in
        // policy never ships it that way.
        let r = evaluate(&s, &[loaded(policy_with(vec![burst.clone()]))], &[], NOW);
        assert_eq!(only(&r).subjects.iter().filter(|x| !x.convictions.is_empty()).count(), 10);

        // Armed by a cohort that never fired, it convicts nobody.
        burst.armed_by = Some(ArmedBy { rule: "cohort".into(), scope: ArmScope::Community, min_subjects: Some(3), also: vec![] });
        let r = evaluate(&s, &[loaded(policy_with(vec![cohort_rule_doc(), burst]))], &[], NOW);
        assert_eq!(
            only(&r).subjects.iter().filter(|x| !x.convictions.is_empty()).count(),
            0,
            "no cohort, no conviction: an invite link is not an attack"
        );
    }

    #[test]
    fn a_member_evaluates_only_stateless_rules() {
        let stateful = Rule {
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
        let s = signals(vec![member(1, Some(2))], vec![msg(0xb1, 1, NOW - HOUR, "bit.ly/a")]);
        let policies = [loaded(policy_with(vec![link_rule(), stateful]))];

        let as_member = evaluate_as(&s, &policies, &[], NOW, EvalMode::Member);
        let pr = &as_member.policies[0];
        let state = |id: &str| pr.rule_status.iter().find(|r| r.rule_id == id).map(|r| r.state);
        assert_eq!(state("links"), Some(RuleState::Evaluated), "a link filter needs no history");
        assert_eq!(state("quiet"), Some(RuleState::RequiresHistory), "counting a member's past does");
        let sub = subject(pr, 1).unwrap();
        assert_eq!(sub.convictions.len(), 1, "the link conviction stands on its own");

        // The same evidence, evaluated by an admin, runs everything.
        let as_admin = evaluate_as(&s, &policies, &[], NOW, EvalMode::Admin);
        let pr = &as_admin.policies[0];
        assert!(pr.rule_status.iter().all(|r| r.state == RuleState::Evaluated));
        assert_eq!(subject(pr, 1).unwrap().convictions.len(), 2);
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

    fn window_rule_policy(m: Match, rungs: Vec<Rung>) -> Policy {
        policy_with(vec![Rule {
            id: "w".into(),
            matcher: m,
            tiers: Some(Tiers { per_message: vec![], per_window: rungs }),
            severity: None,
            weight: None,
            pierces_trusted: false,
            family: None,
            armed_by: None,
            exempt: Exempt::default(),
            enforcement: Enforcement::Advisory,
        }])
    }

    /// One account pasting the same line over and over — the counterpart to
    /// `cohort`, which catches many accounts sharing one line.
    #[test]
    fn repeat_catches_one_account_pasting_the_same_thing() {
        let mut messages: Vec<MessageSignal> = (0..5)
            .map(|i| msg(0xa0 + i as u8, 1, NOW - (i + 1) * HOUR, "CLAIM YOUR FREE AIRDROP NOW"))
            .collect();
        // Variations a spammer reaches for first: case, punctuation, digits.
        messages.push(msg(0xb0, 1, NOW - 6 * HOUR, "claim your free airdrop now!!!"));
        messages.push(msg(0xb1, 1, NOW - 7 * HOUR, "hello everyone"));
        let s = signals(vec![member(1, Some(2))], messages);
        let p = window_rule_policy(
            Match::Repeat { normalize: Normalize::Skeleton, within_secs: None },
            vec![Rung { hits: 3, severity: Severity::Major, weight: 50, pierces_trusted: false }],
        );
        let r = evaluate(&s, &[loaded(p)], &[], NOW);
        let sub = subject(only(&r), 1).unwrap();
        let c = &sub.convictions[0];
        assert_eq!(c.scope, Scope::PerWindow);
        assert_eq!(c.hits, 6, "case and punctuation are one line of attacker code, so they do not vary the shape");
        assert_eq!(c.citation_count, 6, "every repetition is cited, not just the ones past the rung");
        assert_eq!(sub.confidence, 50);
    }

    #[test]
    fn rate_finds_the_densest_span_and_cites_only_it() {
        // Five in a burst, then one much later: the burst is the evidence.
        let mut messages: Vec<MessageSignal> =
            (0..5).map(|i| msg(0xc0 + i as u8, 1, NOW - 60 * 60 * 1000 + i * 10_000, "spam")).collect();
        messages.push(msg(0xcf, 1, NOW - 10 * 60 * 1000, "unrelated"));
        let s = signals(vec![member(1, Some(2))], messages);
        let p = window_rule_policy(
            Match::Rate { per_secs: 60 },
            vec![Rung { hits: 5, severity: Severity::Major, weight: 55, pierces_trusted: false }],
        );
        let r = evaluate(&s, &[loaded(p)], &[], NOW);
        let sub = subject(only(&r), 1).unwrap();
        let c = &sub.convictions[0];
        assert_eq!(c.hits, 5, "five inside one minute");
        assert_eq!(c.citation_count, 5, "the straggler is not part of the burst");
        assert!(
            matches!(c.evidence.first(), Some(Evidence::Rate { count: 5, window_secs: 60, .. })),
            "the span is reported as evidence: {:?}",
            c.evidence
        );
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

    fn word_rule(pattern: &str) -> Rule {
        Rule {
            id: "words".into(),
            matcher: Match::Keyword { patterns: vec![pattern.into()], normalize: Normalize::Fold },
            tiers: Some(Tiers {
                per_message: vec![Rung { hits: 1, severity: Severity::Minor, weight: 10, pierces_trusted: false }],
                per_window: vec![],
            }),
            severity: None,
            weight: None,
            pierces_trusted: false,
            family: None,
            armed_by: None,
            exempt: Exempt::default(),
            enforcement: Enforcement::Advisory,
        }
    }

    fn hits_for(pattern: &str, text: &str) -> usize {
        let s = signals(vec![member(1, Some(2))], vec![msg(0xd1, 1, NOW - HOUR, text)]);
        let r = evaluate(&s, &[loaded(policy_with(vec![word_rule(pattern)]))], &[], NOW);
        only(&r).subjects.len()
    }

    /// A capitalised word matched nothing at all: the corpus was casefolded and
    /// the pattern was not, so the two could never meet. Silent, and the filter
    /// looked like it was simply finding nothing in a clean community.
    #[test]
    fn a_pattern_matches_in_the_same_case_space_as_the_text() {
        for pattern in ["Vector", "VECTOR", "vector", "VeCtOr"] {
            assert_eq!(hits_for(pattern, "we should ship vector today"), 1, "pattern {pattern:?} caught nothing");
        }
    }

    /// Casefolding the pattern must not quietly turn a whole-word rule into a
    /// substring one: token anchoring is the whole reason folding is safe.
    #[test]
    fn folding_a_pattern_keeps_its_word_anchoring() {
        assert_eq!(hits_for("Art", "we should start today"), 0, "\"Art\" must not match inside \"start\"");
        assert_eq!(hits_for("*Art*", "we should start today"), 1, "an explicit wildcard still matches inside");
    }

    fn repeat_policy(within_secs: Option<u64>) -> Policy {
        let mut p = window_rule_policy(
            Match::Repeat { normalize: Normalize::Skeleton, within_secs },
            vec![
                Rung { hits: 4, severity: Severity::Major, weight: 50, pierces_trusted: false },
                Rung { hits: 8, severity: Severity::Severe, weight: 85, pierces_trusted: false },
            ],
        );
        // The shipped window: the point is that a WEEK of history is in scope
        // and the burst span is what narrows it.
        p.window = Window { hours: 168, max_messages: 4000 };
        p
    }

    fn repeat_hits(within_secs: Option<u64>, at_hours: &[u64]) -> usize {
        let msgs: Vec<MessageSignal> = at_hours
            .iter()
            .enumerate()
            .map(|(i, h)| msg(0xe0 + i as u8, 1, NOW - h * HOUR, "gm"))
            .collect();
        // No shield: a brand-new member is exactly who this must not misjudge.
        let s = signals(vec![member(1, Some(1))], msgs);
        let r = evaluate(&s, &[loaded(repeat_policy(within_secs))], &[], NOW);
        only(&r).subjects.len()
    }

    /// A regular saying "gm" once a morning is not a spammer, and counting
    /// across the whole seven-day window could not tell the two apart: eight
    /// mornings read exactly like eight messages in a minute.
    #[test]
    fn a_daily_greeting_is_not_a_repeat_burst() {
        let mornings: Vec<u64> = (0..8).map(|d| d * 24 + 1).collect();
        assert_eq!(repeat_hits(None, &mornings), 1, "the unbounded rule convicts a regular");
        assert_eq!(
            repeat_hits(Some(super::super::harness::REPEAT_BURST_SECS), &mornings),
            0,
            "a burst-bounded rule must leave a daily greeting alone"
        );
    }

    /// The thing it is actually for: the same line over and over inside minutes.
    #[test]
    fn a_burst_of_the_same_line_still_convicts() {
        let burst: Vec<u64> = vec![1, 1, 1, 1, 1, 1, 1, 1];
        assert_eq!(
            repeat_hits(Some(super::super::harness::REPEAT_BURST_SECS), &burst),
            1,
            "eight identical messages in one span must still be caught"
        );
    }

    /// The engine convicts on EVIDENCE, and one anomaly is not evidence.
    ///
    /// A join flood alone asks for a look; a join flood that is also one script
    /// across many accounts is two independent sources and convicts. The gap
    /// between them is what keeps a popular invite link from being staged for
    /// removal, so it is pinned here rather than left to the weights.
    #[test]
    fn one_anomaly_asks_for_a_look_and_two_convict() {
        const MIN: u64 = 60 * 1000;
        const VARIED: [&str; 12] = [
            "hey everyone", "glad to be here", "what is this place",
            "found you through a podcast", "nice community", "hello all",
            "just looking around", "someone linked me here", "good morning",
            "how does this work", "reading the pins now", "thanks for the invite",
        ];
        // `n` strangers arriving across `span` minutes, brand new and silent but
        // for one message each.
        let wave = |n: u64, span: u64, same_text: bool| -> (usize, u32) {
            let members: Vec<MemberSignal> = (1..=n)
                .map(|i| {
                    let mut m = member(i as u8, Some(1));
                    m.joined_at_ms = Some(NOW - (span * MIN) + (i * span * MIN / n));
                    m.first_post_ms = None;
                    m.lifetime_messages = 0;
                    m
                })
                .collect();
            let msgs: Vec<MessageSignal> = (1..=n)
                .map(|i| {
                    let text = if same_text {
                        "join our airdrop".to_string()
                    } else {
                        VARIED[(i as usize - 1) % VARIED.len()].to_string()
                    };
                    msg(0xa0 + i as u8, i as u8, NOW - 2 * MIN, &text)
                })
                .collect();
            let s = signals(members, msgs);
            let r = evaluate(&s, &[loaded(crate::community::policy::harness::default_policy())], &[], NOW);
            let p = only(&r);
            (p.subjects.len(), p.subjects.first().map(|x| x.confidence).unwrap_or(0))
        };

        // A good afternoon on an invite link: no anomaly at all.
        assert_eq!(wave(5, 10, false), (0, 0), "an ordinary invite wave must convict nobody");

        // One anomaly — they arrived fast, but they are saying different things.
        let (caught, conf) = wave(12, 5, false);
        assert_eq!(caught, 12);
        assert!((25..50).contains(&conf), "a lone join flood reached {conf}; it must stay a look, not a verdict");

        // Two anomalies — arrived fast AND reading from one script.
        let (caught, conf) = wave(12, 5, true);
        assert_eq!(caught, 12);
        assert!(conf >= 75, "a scripted flood only reached {conf}; two sources must convict");

        // One anomaly again, the other way round: one script, arriving slowly.
        let (caught, conf) = wave(20, 60, true);
        assert_eq!(caught, 20);
        assert!(conf >= 75, "a scripted wave only reached {conf}");
    }
}
