//! The preset library: what an admin picks instead of writing a document.
//!
//! Every preset is a real policy — the same bytes the engine evaluates and the
//! validator polices — so "pick a template" and "write JSON" produce the same
//! artifact. The dials a designer shows are the only things a preset varies,
//! which keeps the numbers in ONE place (here) rather than scattered across a
//! UI that could drift from the engine.

use super::document::*;
use super::types::Severity;

/// A dial the designer renders, and what it edits inside the policy.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Dial {
    pub key: &'static str,
    pub label: &'static str,
    pub kind: &'static str, // "strictness" | "wordlist" | "domainlist" | "channels" | "toggle"
    pub hint: &'static str,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Preset {
    pub id: &'static str,
    pub name: &'static str,
    /// Plain-language: what it catches, in the words an admin would use.
    pub description: &'static str,
    /// A concrete example, so nobody has to imagine what it means.
    pub example: &'static str,
    /// What it CANNOT do on its own — stated up front, because a preset that
    /// only advertises its powers reads as a promise it may not keep.
    pub caveat: &'static str,
    pub dials: Vec<Dial>,
    /// The starting document, as bytes the engine would evaluate.
    pub policy: Policy,
}

fn tiers(per_message: Vec<Rung>, per_window: Vec<Rung>) -> Option<Tiers> {
    Some(Tiers { per_message, per_window })
}

fn rung(hits: u32, severity: Severity, weight: u32) -> Rung {
    Rung { hits, severity, weight, pierces_trusted: false }
}

fn rule(id: &str, matcher: Match, t: Option<Tiers>) -> Rule {
    Rule {
        id: id.into(),
        matcher,
        tiers: t,
        severity: None,
        weight: None,
        pierces_trusted: false,
        family: None,
        armed_by: None,
        exempt: Exempt::default(),
        enforcement: Enforcement::Advisory,
    }
}

fn single(id: &str, matcher: Match, severity: Severity, weight: u32, armed_by: Option<ArmedBy>) -> Rule {
    Rule {
        id: id.into(),
        matcher,
        tiers: None,
        severity: Some(severity),
        weight: Some(weight),
        pierces_trusted: false,
        family: None,
        armed_by,
        exempt: Exempt::default(),
        enforcement: Enforcement::Advisory,
    }
}

fn base(name: &str, rules: Vec<Rule>, hours: u64) -> Policy {
    Policy {
        format: FORMAT,
        requires: vec![],
        name: name.into(),
        emoji_codes: vec![],
        shields: Shields::default(),
        window: Window { hours, max_messages: 4000 },
        exempt: Exempt::default(),
        rules,
    }
}

/// Link shorteners and redirectors a scam campaign hides behind. Bundled with
/// the build — a moderation feature must not phone home — so it is only as
/// fresh as the release.
pub const SHORTENERS: &[&str] = &[
    "bit.ly", "tinyurl.com", "t.co", "goo.gl", "ow.ly", "is.gd", "buff.ly", "adf.ly", "bit.do", "cutt.ly",
    "rebrand.ly", "shorturl.at", "rb.gy", "tiny.cc", "shorte.st", "bc.vc", "clck.ru", "soo.gd", "s2r.co",
    "tr.ee", "dub.sh", "e.vg", "paw.wf", "shm.to", "snl.ink", "surl.li", "url9.de", "waffl.link",
];

pub fn all() -> Vec<Preset> {
    vec![
        Preset {
            id: "scam_links",
            name: "Scam Links",
            description: "Blocks link shorteners and redirectors that scams hide behind.",
            example: "claim your airdrop at bit.ly/…",
            caveat: "Only catches the shorteners on the bundled list, plus any domain you add.",
            dials: vec![
                Dial { key: "domains", label: "Also block these domains", kind: "domainlist",
                       hint: "One per line. The bundled shortener list is always included." },
                Dial { key: "allow", label: "Never flag these domains", kind: "domainlist",
                       hint: "Your own links, docs, anything you trust." },
            ],
            policy: base(
                "Scam Links",
                vec![rule(
                    "shorteners",
                    Match::Link { patterns: SHORTENERS.iter().map(|s| s.to_string()).collect() },
                    tiers(vec![rung(1, Severity::Severe, 70)], vec![rung(3, Severity::Severe, 90)]),
                )],
                168,
            ),
        },
        Preset {
            id: "raid_shield",
            name: "Raid Shield",
            description: "Spots waves of fresh accounts posting the same thing.",
            example: "400 new members all saying \"hello world\"",
            caveat: "Raid Shield finds patterns, not proof — it flags for you and never removes anyone on its own.",
            dials: vec![Dial {
                key: "strictness",
                label: "How eager should it be?",
                kind: "strictness",
                hint: "Relaxed catches less, not softer.",
            }],
            policy: base(
                "Raid Shield",
                vec![
                    single(
                        "cohort",
                        Match::Cohort { min: 3, quiet_max: 2, short_factor: 3, thin_ratio: None },
                        Severity::Severe,
                        85,
                        None,
                    ),
                    single(
                        "burst",
                        Match::JoinBurst { gap_secs: 600, min: 5 },
                        Severity::Major,
                        40,
                        Some(ArmedBy { rule: "cohort".into(), scope: ArmScope::Community, min_subjects: Some(3) }),
                    ),
                    single(
                        "fresh",
                        Match::TenureLt { secs: 24 * 3600 },
                        Severity::Notice,
                        20,
                        Some(ArmedBy { rule: "cohort".into(), scope: ArmScope::Subject, min_subjects: None }),
                    ),
                ],
                168,
            ),
        },
        Preset {
            id: "no_spam",
            name: "No Spam",
            description: "The same message over and over, from one account or many.",
            example: "someone pasting the same pitch twelve times",
            caveat: "Counts shapes, not words: changing case, punctuation or digits does not help a spammer.",
            dials: vec![Dial {
                key: "strictness",
                label: "How eager should it be?",
                kind: "strictness",
                hint: "Relaxed catches less, not softer.",
            }],
            policy: base(
                "No Spam",
                vec![
                    rule(
                        "repeat",
                        Match::Repeat { normalize: Normalize::Skeleton },
                        tiers(vec![], vec![rung(4, Severity::Major, 50), rung(8, Severity::Severe, 85)]),
                    ),
                    single(
                        "cohort",
                        Match::Cohort { min: 3, quiet_max: 2, short_factor: 3, thin_ratio: None },
                        Severity::Severe,
                        85,
                        None,
                    ),
                ],
                168,
            ),
        },
        Preset {
            id: "language_filter",
            name: "Language Filter",
            description: "Your word list, with per-channel exceptions.",
            example: "words you would rather nobody used here",
            caveat: "Whole words by default, so \"class\" never trips on \"ass\".",
            dials: vec![
                Dial { key: "words", label: "Words to catch", kind: "wordlist",
                       hint: "One per line. Use *word* to match inside longer words." },
                Dial { key: "exempt_channels", label: "Allowed in these channels", kind: "channels",
                       hint: "The filter ignores these channels entirely." },
            ],
            policy: base(
                "Language Filter",
                vec![rule(
                    "words",
                    Match::Keyword { patterns: vec![], normalize: Normalize::Fold },
                    tiers(vec![rung(1, Severity::Minor, 10)], vec![rung(10, Severity::Major, 45)]),
                )],
                168,
            ),
        },
    ]
}

pub fn by_id(id: &str) -> Option<Preset> {
    all().into_iter().find(|p| p.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A preset that cannot validate is a shipped outage: the designer would
    /// offer it, the admin would pick it, and the save would fail.
    #[test]
    fn every_preset_validates() {
        for p in all() {
            assert!(p.policy.validate().is_ok(), "preset {} must validate: {:?}", p.id, p.policy.validate());
        }
    }

    /// Weak signals must never speak alone — the lesson from convicting 147 of
    /// 155 members on "has barely posted".
    #[test]
    fn no_preset_lets_a_weak_signal_convict_alone() {
        for p in all() {
            for r in &p.policy.rules {
                let weak = matches!(
                    r.matcher,
                    Match::TenureLt { .. } | Match::MessagesLte { .. } | Match::JoinBurst { .. }
                );
                assert_eq!(weak, r.armed_by.is_some(), "preset {} rule {} armed iff weak", p.id, r.id);
            }
        }
    }

    /// Every preset says what it cannot do. A template that only advertises its
    /// powers reads as a promise it may not keep.
    #[test]
    fn every_preset_states_its_limits() {
        for p in all() {
            assert!(!p.caveat.is_empty(), "preset {} needs a caveat", p.id);
            assert!(!p.example.is_empty(), "preset {} needs a concrete example", p.id);
            assert!(!p.dials.is_empty(), "preset {} needs at least one dial", p.id);
        }
    }
}
