//! Side-by-side harness: run the engine against a live community and diff its
//! verdicts against the shipped assessor.
//!
//! The engine convicts nothing in production yet. `raid.rs` keeps driving the
//! moderation console while this runs alongside, so a disagreement is a finding
//! to read rather than a member wrongly removed. Everything here is diagnostic:
//! it reads local state, evaluates, and reports — it publishes nothing and
//! changes no membership.

use super::document::*;
use super::engine::{evaluate, evaluate_as, LoadedPolicy, MemberSignal, MessageSignal, Signals};
use super::types::*;
use super::engine::EvalMode;
use super::normalize::EmojiCodes;
use nostr_sdk::prelude::{PublicKey, ToBech32};
use std::collections::BTreeSet;

/// Hash a policy the way the wire will: over the exact bytes it arrived as,
/// never a re-serialization.
pub fn hash_policy_bytes(bytes: &[u8]) -> Hash32 {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    Hash32(h.finalize().into())
}

fn subject_of(npub_or_hex: &str) -> Option<SubjectId> {
    PublicKey::parse(npub_or_hex).ok().map(|p| SubjectId(p.to_bytes()))
}

/// The Phase-1 built-in policy: the deterministic rules that counter the live
/// attack shapes (scam shortlinks and copy-paste spam), plus the fresh-account
/// aggravators. The heuristic planes stay with `raid.rs` until they are wired.
pub fn scam_links_policy() -> Policy {
    Policy {
        format: FORMAT,
        requires: vec![],
        name: "scam-links".into(),
        emoji_codes: vec![],
        // A WEEK, not a day. Shield inputs are measured over the declared
        // window (that is what keeps them identical across clients), so a
        // 24-hour window asks members to earn trust daily and almost nobody
        // does: the first live run trusted 1 of 155.
        shields: Shields::default(),
        window: Window { hours: 168, max_messages: 4000 },
        exempt: Exempt::default(),
        rules: vec![
            Rule {
                id: "shorteners".into(),
                matcher: Match::Link {
                    // The shipped shortener/scam list, as a denylist so ordinary
                    // links stay ordinary.
                    patterns: SHORTENERS.iter().map(|s| s.to_string()).collect(),
                },
                tiers: Some(Tiers {
                    per_message: vec![Rung { hits: 1, severity: Severity::Severe, weight: 70, pierces_trusted: false }],
                    per_window: vec![Rung { hits: 3, severity: Severity::Severe, weight: 90, pierces_trusted: false }],
                }),
                severity: None,
                weight: None,
                pierces_trusted: false,
                family: None,
                armed_by: None,
                exempt: Exempt::default(),
                enforcement: Enforcement::Advisory,
            },
            // The raid shape: many identities, one line each. Heuristic, so it
            // flags for a human and never feeds `proven`.
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
            },
            // A burst of joins is what a freshly-posted invite link looks like,
            // so it stays silent until the cohort rule has already convicted.
            Rule {
                id: "burst".into(),
                matcher: Match::JoinBurst { gap_secs: 600, min: 5 },
                tiers: None,
                severity: Some(Severity::Major),
                weight: Some(40),
                pierces_trusted: false,
                family: None,
                armed_by: Some(ArmedBy { rule: "cohort".into(), scope: ArmScope::Community, min_subjects: Some(3) }),
                exempt: Exempt::default(),
                enforcement: Enforcement::Advisory,
            },
            // Copy-paste spam from ONE account. `cohort` (still with raid.rs)
            // catches the other shape: many accounts sharing one line.
            Rule {
                id: "repeat".into(),
                matcher: Match::Repeat { normalize: Normalize::Skeleton },
                tiers: Some(Tiers {
                    per_message: vec![],
                    per_window: vec![
                        Rung { hits: 4, severity: Severity::Major, weight: 50, pierces_trusted: false },
                        Rung { hits: 8, severity: Severity::Severe, weight: 85, pierces_trusted: false },
                    ],
                }),
                severity: None,
                weight: None,
                pierces_trusted: false,
                family: None,
                armed_by: None,
                exempt: Exempt::default(),
                enforcement: Enforcement::Advisory,
            },
            // Aggravators, and ONLY aggravators: each fires only for a subject
            // the link rule already convicted. Unarmed, "has posted at most
            // twice" describes most of a healthy community — the first live run
            // convicted 147 of 155 on it alone, which is exactly the
            // weak-signal-convicts-nobody rule being violated by a policy the
            // engine faithfully executed.
            Rule {
                id: "fresh".into(),
                matcher: Match::TenureLt { secs: 24 * 3600 },
                tiers: None,
                severity: Some(Severity::Notice),
                weight: Some(20),
                pierces_trusted: false,
                family: None,
                armed_by: Some(ArmedBy { rule: "cohort".into(), scope: ArmScope::Subject, min_subjects: None }),
                exempt: Exempt::default(),
                enforcement: Enforcement::Advisory,
            },
            Rule {
                id: "quiet".into(),
                matcher: Match::MessagesLte { n: 2 },
                tiers: None,
                severity: Some(Severity::Notice),
                weight: Some(10),
                pierces_trusted: false,
                family: None,
                armed_by: Some(ArmedBy { rule: "cohort".into(), scope: ArmScope::Subject, min_subjects: None }),
                exempt: Exempt::default(),
                enforcement: Enforcement::Advisory,
            },
        ],
    }
}

/// Link shorteners and redirectors a scam campaign hides behind. Bundled with
/// the build (a moderation feature must not phone home), so it is only as fresh
/// as the release.
const SHORTENERS: &[&str] = &[
    "bit.ly", "tinyurl.com", "t.co", "goo.gl", "ow.ly", "is.gd", "buff.ly", "adf.ly", "bit.do", "cutt.ly",
    "rebrand.ly", "shorturl.at", "rb.gy", "tiny.cc", "shorte.st", "bc.vc", "clck.ru", "soo.gd", "s2r.co",
    "tr.ee", "dub.sh", "e.vg", "paw.wf", "shm.to", "snl.ink", "surl.li", "url9.de", "waffl.link",
];

/// Everything an evaluation needs, assembled from local state once: the engine
/// inputs and the display facts the console shows beside each verdict. Both the
/// console and the side-by-side diff read this, so they can never disagree
/// about what they were looking at.
pub struct Assembled {
    pub signals: Signals,
    pub facts: std::collections::BTreeMap<String, MemberFacts>,
    pub corpus: usize,
}

#[allow(clippy::type_complexity)]
pub fn assemble(
    community_id_hex: &str,
    owner: &PublicKey,
    me: Option<&PublicKey>,
    members: &[(PublicKey, Option<u64>, bool, Vec<String>, Option<String>)],
    now_ms: u64,
) -> Result<Assembled, String> {
    let rows = crate::db::community::community_policy_messages(community_id_hex, caps::WINDOW_MAX_MESSAGES)?;
    let corpus = rows.len();
    let mut messages: Vec<MessageSignal> = Vec::with_capacity(rows.len());
    for m in rows {
        let (Some(id), Some(author), Some(channel)) = (
            crate::simd::hex::hex_to_bytes_32_checked(&m.id),
            subject_of(&m.npub),
            crate::simd::hex::hex_to_bytes_32_checked(&m.channel_id),
        ) else {
            continue;
        };
        messages.push(MessageSignal {
            id: MessageId(id),
            author,
            channel: Hash32(channel),
            at_ms: m.at_ms,
            text: m.text,
            mentions: m.mentions,
        });
    }

    let footprints: std::collections::HashMap<String, crate::db::community::AuthorFootprint> =
        crate::db::community::community_author_footprints(community_id_hex)
            .unwrap_or_default()
            .into_iter()
            .map(|f| (f.npub.clone(), f))
            .collect();
    // Distinct shapes per author, over the window — how someone speaks now is
    // what tells a person from a script.
    let codes = EmojiCodes::from_policy(scam_links_policy().emoji_codes.iter());
    let mut distinct: std::collections::HashMap<[u8; 32], BTreeSet<String>> = std::collections::HashMap::new();
    for m in &messages {
        let sk = super::normalize::skeleton(&m.text, &codes);
        if !sk.is_empty() {
            distinct.entry(m.author.0).or_default().insert(sk);
        }
    }

    let channels: BTreeSet<[u8; 32]> = messages.iter().map(|m| m.channel.0).collect();
    let mut member_signals = Vec::with_capacity(members.len());
    let mut facts = std::collections::BTreeMap::new();
    for (pk, joined, is_staff, roles, invite_label) in members {
        let b32 = pk.to_bech32().unwrap_or_default();
        let fp = footprints.get(&b32);
        let first_post_ms = fp.map(|f| f.first_secs).filter(|s| *s > 0).map(|s| s.saturating_mul(1000));
        member_signals.push(MemberSignal {
            subject: SubjectId(pk.to_bytes()),
            joined_at_ms: *joined,
            roles: roles.iter().filter_map(|r| crate::simd::hex::hex_to_bytes_32_checked(r)).map(Hash32).collect(),
            is_staff: *is_staff,
            lifetime_messages: fp.map(|f| f.messages).unwrap_or(0),
            first_post_ms,
        });
        // Tenure for DISPLAY uses the same oldest-trace rule the shield does.
        let oldest = [*joined, first_post_ms].into_iter().flatten().min();
        facts.insert(
            b32,
            MemberFacts {
                joined_at_ms: joined.unwrap_or(0),
                invite_label: invite_label.clone(),
                messages: fp.map(|f| f.messages).unwrap_or(0),
                distinct: distinct.get(&pk.to_bytes()).map(|d| d.len() as u64).unwrap_or(0),
                tenure_secs: oldest.map(|o| now_ms.saturating_sub(o) / 1000).unwrap_or(0),
                last_secs: fp.map(|f| f.last_secs).unwrap_or(0),
                is_owner: pk == owner,
                is_admin: *is_staff,
                is_me: me == Some(pk),
            },
        );
    }

    Ok(Assembled {
        signals: Signals {
            owner: SubjectId(owner.to_bytes()),
            members: member_signals,
            messages,
            channels: channels.into_iter().map(Hash32).collect(),
            relays: vec![],
            requested_from: 0,
            requested_to: now_ms,
            confirmed_from: u64::MAX,
            confirmed_to: now_ms,
            roster_version: Hash32([0; 32]),
        },
        facts,
        corpus,
    })
}

/// Evaluate the built-in policy and render the console's report.
pub fn evaluate_for_console(assembled: &Assembled, now_ms: u64) -> Result<serde_json::Value, String> {
    let policy = scam_links_policy();
    let bytes = serde_json::to_vec(&policy).map_err(|e| e.to_string())?;
    let lp = LoadedPolicy { hash: hash_policy_bytes(&bytes), policy, activated_at: None };
    let report = evaluate_as(&assembled.signals, &[lp], &[], now_ms, EvalMode::Admin);
    Ok(console_report(&report, &assembled.facts, now_ms / 1000))
}

/// What the moderation console needs about one member beyond the verdict:
/// the display facts a moderator reads before acting.
#[derive(Debug, Clone, Default)]
pub struct MemberFacts {
    pub joined_at_ms: u64,
    pub invite_label: Option<String>,
    pub messages: u64,
    pub distinct: u64,
    pub tenure_secs: u64,
    pub last_secs: u64,
    pub is_owner: bool,
    pub is_admin: bool,
    pub is_me: bool,
}

/// Turn an engine report into the shape the moderation console already speaks,
/// so the panel changes its SOURCE without changing its contract. `raid.rs`
/// keeps running beside it (see [`run_side_by_side`]) until the two have agreed
/// on live data for long enough to retire one.
pub fn console_report(
    report: &ModerationReport,
    facts: &std::collections::BTreeMap<String, MemberFacts>,
    now_secs: u64,
) -> serde_json::Value {
    let Some(pr) = report.policies.first() else {
        return serde_json::json!({ "members": [], "cohorts": [], "suspects": 0, "trusted": 0, "protected": 0,
                                   "raid_detected": false, "burst_size": 0, "burst_from_ms": 0, "burst_to_ms": 0 });
    };
    let by_subject: std::collections::BTreeMap<String, &SubjectReport> = pr
        .subjects
        .iter()
        .filter_map(|s| PublicKey::from_slice(&s.subject.0).ok()?.to_bech32().ok().map(|b| (b, s)))
        .collect();

    let (mut suspects, mut trusted, mut protected) = (0usize, 0usize, 0usize);
    let mut members: Vec<serde_json::Value> = Vec::with_capacity(facts.len());
    for (npub, f) in facts {
        let s = by_subject.get(npub);
        let convictions: Vec<&Conviction> =
            s.map(|s| s.convictions.iter().filter(|c| !c.suppressed).collect()).unwrap_or_default();
        let shield = s.map(|s| s.shield).unwrap_or(Shield::None);
        // The console's four verdicts, from the engine's own vocabulary.
        let verdict = if shield == Shield::Protected {
            protected += 1;
            "protected"
        } else if !convictions.is_empty() {
            suspects += 1;
            "suspect"
        } else if shield == Shield::Trusted {
            trusted += 1;
            "trusted"
        } else {
            "neutral"
        };
        // Reasons in evidence, never in score — the panel shows WHY, and a
        // moderator who disagrees can see exactly what they are disagreeing with.
        let mut reasons: Vec<String> = Vec::new();
        let mut cohort_peers = 0u32;
        for c in &convictions {
            for e in &c.evidence {
                match e {
                    Evidence::Cohort { size, sample, .. } => {
                        cohort_peers = cohort_peers.max(size.saturating_sub(1));
                        let quoted: String = sample.chars().take(48).collect();
                        reasons.push(format!(
                            "Posted the same message as {} other member{} — \"{}\"",
                            size.saturating_sub(1),
                            if *size == 2 { "" } else { "s" },
                            quoted
                        ));
                    }
                    Evidence::Burst { size, .. } => reasons.push(format!("Joined in a burst of {size}")),
                    Evidence::Rate { count, window_secs, .. } => {
                        reasons.push(format!("Sent {count} messages in {window_secs}s"))
                    }
                    _ => {}
                }
            }
            if c.evidence.is_empty() {
                reasons.push(match c.rule_id.as_str() {
                    "fresh" => "Joined in the last 24h".to_string(),
                    "quiet" => "Has barely posted".to_string(),
                    "repeat" => format!("Repeated one message {} times", c.hits),
                    "shorteners" => format!("Posted {} link(s) to a known shortener", c.hits),
                    other => format!("Matched rule {other}"),
                });
            }
        }
        if verdict == "protected" {
            reasons.push(if f.is_owner { "Community owner".into() } else { "Holds a role".into() });
        } else if verdict == "trusted" && reasons.is_empty() {
            reasons.push("Long-standing member".into());
        }

        members.push(serde_json::json!({
            "npub": npub,
            "verdict": verdict,
            "score": s.map(|s| s.confidence).unwrap_or(0),
            "proven": s.map(|s| s.proven).unwrap_or(0),
            "reasons": reasons,
            "joined_at_ms": f.joined_at_ms,
            "invite_label": f.invite_label,
            "messages": f.messages,
            "distinct": f.distinct,
            "cohort": cohort_peers,
            "tenure_secs": f.tenure_secs,
            "last_secs": f.last_secs,
            "is_owner": f.is_owner,
            "is_admin": f.is_admin,
            "is_me": f.is_me,
        }));
    }
    // Suspects first, then by score: the panel opens on what needs deciding.
    members.sort_by(|a, b| {
        let rank = |v: &serde_json::Value| match v["verdict"].as_str() {
            Some("suspect") => 0,
            Some("neutral") => 1,
            Some("trusted") => 2,
            _ => 3,
        };
        rank(a).cmp(&rank(b)).then_with(|| b["score"].as_u64().cmp(&a["score"].as_u64()))
    });

    // Cohort exhibits, largest first.
    let mut cohorts: std::collections::BTreeMap<[u8; 32], (usize, String)> = std::collections::BTreeMap::new();
    let mut burst = (0u32, 0u64, 0u64);
    for s in &pr.subjects {
        for c in s.convictions.iter().filter(|c| !c.suppressed) {
            for e in &c.evidence {
                match e {
                    Evidence::Cohort { skeleton_hash, size, sample, .. } => {
                        let slot = cohorts.entry(skeleton_hash.0).or_insert((0, sample.clone()));
                        slot.0 = slot.0.max(*size as usize);
                    }
                    Evidence::Burst { size, from, to } => {
                        if *size > burst.0 {
                            burst = (*size, *from, *to);
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    let mut cohort_list: Vec<serde_json::Value> = cohorts
        .into_values()
        .map(|(size, sample)| serde_json::json!({ "size": size, "sample": sample, "members": [] }))
        .collect();
    cohort_list.sort_by(|a, b| b["size"].as_u64().cmp(&a["size"].as_u64()));

    let _ = now_secs;
    serde_json::json!({
        "members": members,
        "cohorts": cohort_list,
        "suspects": suspects,
        "trusted": trusted,
        "protected": protected,
        "raid_detected": !cohort_list_is_empty(&cohort_list) && suspects > 0,
        "burst_size": burst.0,
        "burst_from_ms": burst.1,
        "burst_to_ms": burst.2,
    })
}

fn cohort_list_is_empty(v: &[serde_json::Value]) -> bool {
    v.is_empty()
}

/// One member's verdict from each side, for eyeballing where they differ.
#[derive(Debug, Clone, serde::Serialize)]
pub struct VerdictRow {
    pub npub: String,
    /// What `raid.rs` said.
    pub assessor: String,
    /// What the engine scored.
    pub confidence: u32,
    pub proven: u32,
    pub band: String,
    pub shield: String,
    pub rules: Vec<String>,
}

/// What the side-by-side run found.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DiffReport {
    pub members: usize,
    pub corpus: usize,
    /// Members the engine convicted (any conviction).
    pub engine_convicted: usize,
    /// Members `raid.rs` calls Suspect.
    pub assessor_suspects: usize,
    /// Convicted by the engine, cleared by the assessor — and the reverse.
    pub engine_only: Vec<VerdictRow>,
    pub assessor_only: Vec<VerdictRow>,
    /// Both agree something is wrong.
    pub agreed: Vec<VerdictRow>,
    /// Shield distribution, so a mis-shielded roster is visible immediately.
    pub protected: usize,
    pub trusted: usize,
    pub indeterminate: usize,
    /// Rule-level states — a rule that could not evaluate says so.
    pub rule_states: Vec<(String, String)>,
    pub coverage_complete: bool,
    /// Wall-clock cost, split so a slow run says WHICH half is slow: reading
    /// and decrypting local state, versus the pure evaluation.
    pub signals_ms: u64,
    pub evaluate_ms: u64,
}

/// Assemble signals from local state and evaluate. Reads only; the engine is
/// pure, so this is safe to run against a live community at any time.
pub fn run_side_by_side(
    community_id_hex: &str,
    owner: &PublicKey,
    members: &[(PublicKey, Option<u64>, bool, Vec<String>)],
    assessments: &[(String, String)],
    now_ms: u64,
) -> Result<DiffReport, String> {
    let policy = scam_links_policy();
    let bytes = serde_json::to_vec(&policy).map_err(|e| e.to_string())?;
    let lp = LoadedPolicy { hash: hash_policy_bytes(&bytes), policy, activated_at: None };

    let t0 = std::time::Instant::now();
    let rows = crate::db::community::community_policy_messages(community_id_hex, caps::WINDOW_MAX_MESSAGES)?;
    let corpus = rows.len();
    let messages: Vec<MessageSignal> = rows
        .into_iter()
        .filter_map(|m| {
            Some(MessageSignal {
                id: MessageId(crate::simd::hex::hex_to_bytes_32_checked(&m.id)?),
                author: subject_of(&m.npub)?,
                channel: Hash32(crate::simd::hex::hex_to_bytes_32_checked(&m.channel_id)?),
                at_ms: m.at_ms,
                text: m.text,
                mentions: m.mentions,
            })
        })
        .collect();

    let channels: BTreeSet<[u8; 32]> = messages.iter().map(|m| m.channel.0).collect();
    // Lifetime footprints: standing is historical, so a quiet week must not
    // cost a regular the standing they built over months.
    let footprints: std::collections::HashMap<String, (u64, u64)> =
        crate::db::community::community_author_footprints(community_id_hex)
            .unwrap_or_default()
            .into_iter()
            .map(|f| (f.npub, (f.messages, f.first_secs)))
            .collect();
    let member_signals: Vec<MemberSignal> = members
        .iter()
        .map(|(pk, joined, is_staff, roles)| MemberSignal {
            subject: SubjectId(pk.to_bytes()),
            joined_at_ms: *joined,
            roles: roles.iter().filter_map(|r| crate::simd::hex::hex_to_bytes_32_checked(r)).map(Hash32).collect(),
            is_staff: *is_staff,
            lifetime_messages: pk.to_bech32().ok().and_then(|b| footprints.get(&b).map(|f| f.0)).unwrap_or(0),
            first_post_ms: pk
                .to_bech32()
                .ok()
                .and_then(|b| footprints.get(&b).map(|f| f.1))
                .filter(|s| *s > 0)
                .map(|s| s.saturating_mul(1000)),
        })
        .collect();

    let signals = Signals {
        owner: SubjectId(owner.to_bytes()),
        members: member_signals,
        messages,
        channels: channels.into_iter().map(Hash32).collect(),
        // The harness reads local state, so it makes no coverage claim: the
        // caller has not proven an EOSE-confirmed range here.
        relays: vec![],
        requested_from: 0,
        requested_to: now_ms,
        confirmed_from: u64::MAX,
        confirmed_to: now_ms,
        roster_version: Hash32([0; 32]),
    };

    let signals_ms = t0.elapsed().as_millis() as u64;
    let t1 = std::time::Instant::now();
    let report = evaluate(&signals, &[lp], &[], now_ms);
    let evaluate_ms = t1.elapsed().as_millis() as u64;
    let pr = report.policies.first().ok_or("no policy report")?;

    let verdict_of = |npub: &str| -> String {
        assessments.iter().find(|(n, _)| n == npub).map(|(_, v)| v.clone()).unwrap_or_else(|| "unknown".into())
    };

    let mut engine_only = Vec::new();
    let mut assessor_only = Vec::new();
    let mut agreed = Vec::new();
    let (mut protected, mut trusted, mut indeterminate) = (0, 0, 0);
    let mut engine_convicted = 0usize;

    for s in &pr.subjects {
        match s.shield {
            Shield::Protected => protected += 1,
            Shield::Trusted => trusted += 1,
            Shield::Indeterminate => indeterminate += 1,
            Shield::None => {}
        }
        let npub = PublicKey::from_slice(&s.subject.0).ok().and_then(|p| p.to_bech32().ok()).unwrap_or_default();
        let assessor = verdict_of(&npub);
        let convicted = s.convictions.iter().any(|c| !c.suppressed);
        if convicted {
            engine_convicted += 1;
        }
        let row = VerdictRow {
            npub: npub.clone(),
            assessor: assessor.clone(),
            confidence: s.confidence,
            proven: s.proven,
            band: format!("{:?}", s.band).to_lowercase(),
            shield: format!("{:?}", s.shield).to_lowercase(),
            rules: s.convictions.iter().map(|c| format!("{}:{:?}", c.rule_id, c.scope)).collect(),
        };
        match (convicted, assessor == "suspect") {
            (true, true) => agreed.push(row),
            (true, false) => engine_only.push(row),
            (false, true) => assessor_only.push(row),
            (false, false) => {}
        }
    }
    // A member the assessor suspects but the engine never scored at all is
    // still a disagreement worth seeing.
    for (npub, verdict) in assessments.iter().filter(|(_, v)| v == "suspect") {
        if !pr.subjects.iter().any(|s| {
            PublicKey::from_slice(&s.subject.0).ok().and_then(|p| p.to_bech32().ok()).as_deref() == Some(npub.as_str())
        }) {
            assessor_only.push(VerdictRow {
                npub: npub.clone(),
                assessor: verdict.clone(),
                confidence: 0,
                proven: 0,
                band: "clear".into(),
                shield: "none".into(),
                rules: vec![],
            });
        }
    }

    Ok(DiffReport {
        members: members.len(),
        corpus,
        engine_convicted,
        assessor_suspects: assessments.iter().filter(|(_, v)| v == "suspect").count(),
        engine_only,
        assessor_only,
        agreed,
        protected,
        trusted,
        indeterminate,
        rule_states: pr
            .rule_status
            .iter()
            .map(|r| (r.rule_id.clone(), format!("{:?}", r.state).to_lowercase()))
            .collect(),
        coverage_complete: pr.coverage_complete,
        signals_ms,
        evaluate_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_builtin_policy_validates() {
        assert!(scam_links_policy().validate().is_ok(), "a shipped policy that cannot validate is a shipped outage");
    }

    /// The first live run convicted 147 of 155 members on "has posted at most
    /// twice" alone. A weak signal must never speak by itself — that includes
    /// a join burst, which is exactly what a freshly-posted invite link looks
    /// like.
    #[test]
    fn every_weak_signal_is_armed_by_a_real_conviction() {
        let p = scam_links_policy();
        for rule in &p.rules {
            let is_aggravator = matches!(
                rule.matcher,
                Match::TenureLt { .. } | Match::MessagesLte { .. } | Match::JoinBurst { .. }
            );
            assert_eq!(
                is_aggravator,
                rule.armed_by.is_some(),
                "rule {} must be armed if and only if it is a weak signal",
                rule.id
            );
        }
    }

    #[test]
    fn the_policy_hash_is_over_the_bytes_it_arrived_as() {
        let p = scam_links_policy();
        let a = serde_json::to_vec(&p).unwrap();
        // Same bytes, same hash; different bytes (even semantically equal
        // JSON), different hash — which is exactly why the wire hashes bytes
        // rather than a re-serialization.
        assert_eq!(hash_policy_bytes(&a), hash_policy_bytes(&a));
        let mut spaced = String::from_utf8(a.clone()).unwrap();
        spaced.push(' ');
        assert_ne!(hash_policy_bytes(&a), hash_policy_bytes(spaced.as_bytes()));
    }
}
