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
    pub kind: &'static str, // "strictness" | "wordlist" | "domainlist" | "channels" | "summary" | "text" | "rules" | "seconds"
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

/// One rule, in the words an admin would use.
///
/// The engine's own vocabulary — matcher variants, rungs, weights — is exactly
/// what makes a shipped policy read as a black box. This is the only place
/// that translation lives, so a console, a bot and a CLI all describe the same
/// rule the same way.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RuleSummary {
    pub label: String,
    pub detail: String,
    /// True when the rule stays silent until another rule has already
    /// convicted. Unmarked, an aggravator reads as cover it does not provide.
    pub armed: bool,
}

fn plural(n: u32, one: &str, many: &str) -> String {
    if n == 1 { format!("{n} {one}") } else { format!("{n} {many}") }
}

fn describe_thresholds(r: &Rule) -> String {
    // A burst-bounded repeat already named its span, so "in the window" would
    // point at a different stretch of time than the sentence before it.
    let window_scoped = !matches!(r.matcher, Match::Repeat { within_secs: Some(_), .. });
    if let Some(t) = &r.tiers {
        let mut parts: Vec<String> = Vec::new();
        for rung in &t.per_message {
            parts.push(format!("{} in one message", plural(rung.hits, "hit", "hits")));
        }
        for rung in &t.per_window {
            parts.push(if window_scoped {
                format!("{} in the window", plural(rung.hits, "hit", "hits"))
            } else {
                plural(rung.hits, "hit", "hits")
            });
        }
        if !parts.is_empty() {
            return format!("Trips at {}.", parts.join(", then "));
        }
    }
    String::new()
}

/// Describe every rule in a policy, in order.
pub fn describe(policy: &Policy) -> Vec<RuleSummary> {
    policy.rules.iter().map(describe_rule).collect()
}

pub fn describe_rule(r: &Rule) -> RuleSummary {
    let (label, mut detail) = match &r.matcher {
        Match::Keyword { patterns, .. } => (
            "Words".to_string(),
            format!(
                "{} on the list, matched as whole words unless you wrap them in *.",
                plural(patterns.len() as u32, "word", "words")
            ),
        ),
        Match::Regex { patterns, .. } => (
            "Patterns".to_string(),
            plural(patterns.len() as u32, "expression", "expressions") + " matched against each message.",
        ),
        Match::Link { patterns } => (
            "Link domains".to_string(),
            format!(
                "{} blocked, subdomains included.",
                plural(patterns.len() as u32, "domain", "domains")
            ),
        ),
        Match::Repeat { within_secs, .. } => (
            "Same message repeated".to_string(),
            match within_secs {
                Some(secs) => format!(
                    "One account posting the same thing over and over inside {} minutes. Case, punctuation and digits are ignored, so changing them does not help.",
                    secs / 60
                ),
                None => "One account posting the same thing over and over. Case, punctuation and digits are ignored, so changing them does not help.".to_string(),
            },
        ),
        Match::Rate { per_secs } => (
            "Posting too fast".to_string(),
            format!(
                "One account's messages counted over any {} seconds, whatever they say.",
                per_secs
            ),
        ),
        Match::Mentions {} => (
            "Mass tagging".to_string(),
            "How many DISTINCT people one message names. Tagging the same person repeatedly counts once.".to_string(),
        ),
        Match::Cohort { min, .. } => (
            "Many accounts, one line".to_string(),
            format!(
                "{} or more separate accounts posting the same line at the same time. The shape of a raid.",
                min
            ),
        ),
        Match::JoinBurst { gap_secs, min } => (
            "Join flood".to_string(),
            format!("{} or more joins within {} minutes.", min, gap_secs / 60),
        ),
        Match::TenureLt { secs } => (
            "New account".to_string(),
            format!("Joined less than {} hours ago.", secs / 3600),
        ),
        Match::MessagesLte { n } => (
            "Barely posted".to_string(),
            format!("Has posted at most {}.", plural(*n, "message", "messages")),
        ),
    };
    let t = describe_thresholds(r);
    if !t.is_empty() {
        detail.push(' ');
        detail.push_str(&t);
    }
    RuleSummary { label, detail, armed: r.armed_by.is_some() }
}

/// One rule a from-scratch policy may add: what it catches, what the author has
/// to supply, and the starting numbers.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RuleKind {
    pub id: &'static str,
    pub label: &'static str,
    /// Plain-language: what trips it.
    pub description: &'static str,
    /// What the author supplies: "wordlist" | "domainlist" | "none".
    pub input: &'static str,
    pub input_label: &'static str,
    pub input_hint: &'static str,
    /// The starting rule, weights and rungs included, so a builder never has to
    /// invent numbers the engine would then have to honour.
    pub rule: Rule,
}

/// The rule kinds a from-scratch policy may add.
///
/// Deliberately NOT the whole `Match` set. `TenureLt` and `MessagesLte`
/// describe most of a healthy community on their own, so they exist only as
/// aggravators armed behind a real conviction and the builder never offers
/// them loose. Every kind here can stand as the only rule in a policy and
/// still convict the right person.
pub fn rule_kinds() -> Vec<RuleKind> {
    vec![
        RuleKind {
            id: "keyword",
            label: "Words",
            description: "Someone posts a word from your list.",
            input: "wordlist",
            input_label: "Words to catch",
            input_hint: "One per line. Use *word* to match inside longer words.",
            rule: rule(
                "words",
                Match::Keyword { patterns: vec![], normalize: Normalize::Fold },
                tiers(vec![rung(1, Severity::Minor, 10)], vec![rung(10, Severity::Major, 45)]),
            ),
        },
        RuleKind {
            id: "link",
            label: "Link domains",
            description: "Someone posts a link to a domain on your list.",
            input: "domainlist",
            input_label: "Domains to block",
            input_hint: "One per line. Matches the domain and its subdomains.",
            rule: rule(
                "links",
                Match::Link { patterns: vec![] },
                tiers(vec![rung(1, Severity::Severe, 70)], vec![rung(3, Severity::Severe, 90)]),
            ),
        },
        RuleKind {
            id: "repeat",
            label: "Same message repeated",
            description: "One account posting the same thing over and over inside half an hour. Case, punctuation and digits are ignored, so changing them does not help.",
            input: "none",
            input_label: "",
            input_hint: "",
            rule: rule(
                "repeat",
                Match::Repeat { normalize: Normalize::Skeleton, within_secs: Some(super::harness::REPEAT_BURST_SECS) },
                tiers(vec![], vec![rung(4, Severity::Major, 50), rung(8, Severity::Severe, 85)]),
            ),
        },
        RuleKind {
            id: "mentions",
            label: "Mass tagging",
            description: "One message naming a crowd. Counts distinct people, so twenty pings at one person is one person — annoying, but not a raid.",
            input: "none",
            input_label: "",
            input_hint: "",
            rule: rule(
                "mentions",
                Match::Mentions {},
                tiers(vec![rung(6, Severity::Major, 50), rung(12, Severity::Severe, 85)], vec![]),
            ),
        },
        RuleKind {
            id: "rate",
            label: "Posting too fast",
            description: "One account posting faster than a person reasonably types, whatever they are saying. Catches a spammer who varies their text just enough to look different every time.",
            input: "seconds",
            input_label: "Over how many seconds",
            input_hint: "The span it counts within. 10 catches a burst; 60 catches a sustained flood.",
            rule: rule(
                "rate",
                Match::Rate { per_secs: 10 },
                tiers(vec![], vec![rung(6, Severity::Major, 50), rung(12, Severity::Severe, 85)]),
            ),
        },
        RuleKind {
            id: "cohort",
            label: "Many accounts, one line",
            description: "Separate accounts posting the same line at the same time. The shape of a raid.",
            input: "none",
            input_label: "",
            input_hint: "",
            rule: single(
                "cohort",
                Match::Cohort { min: 3, quiet_max: 2, short_factor: 3, thin_ratio: None },
                Severity::Severe,
                85,
                None,
            ),
        },
    ]
}

pub fn all() -> Vec<Preset> {
    vec![
        Preset {
            id: super::harness::DEFAULTS_POLICY_ID,
            name: "Vector's Defaults",
            description: "Raid detection, running here already. Open it to read every rule, or save your own version.",
            example: "a swarm of fresh accounts posting one line",
            caveat: "A Built-In Anti-Raid Policy: detects raids proactively and alerts you with a list of suspects for quick handling.",
            dials: vec![
                Dial { key: "summary", label: "What these rules do", kind: "summary",
                       hint: "Every rule that runs here, in order." },
                Dial { key: "strictness", label: "Sensitivity", kind: "strictness",
                       hint: "How much it takes to trip a rule, and how confident the result is." },
            ],
            policy: super::harness::default_policy(),
        },
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
            id: "no_spam",
            name: "No Spam",
            description: "The same message over and over, from one account or many.",
            example: "the same pitch, twelve times",
            caveat: "Counts shapes, not words: changing case, punctuation or digits does not help a spammer.",
            dials: vec![Dial {
                key: "strictness",
                label: "Sensitivity",
                kind: "strictness",
                hint: "How much it takes to trip a rule, and how confident the result is.",
            }],
            policy: base(
                "No Spam",
                vec![
                    rule(
                        "repeat",
                        Match::Repeat { normalize: Normalize::Skeleton, within_secs: Some(super::harness::REPEAT_BURST_SECS) },
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
            id: "word_filter",
            name: "Word Filter",
            description: "Any list of words you choose, with per-channel exceptions.",
            example: "spoilers, slurs, or a scam phrase",
            caveat: "Whole words by default, so filtering \"art\" leaves \"start\" alone.",
            dials: vec![
                Dial { key: "words", label: "Words to catch", kind: "wordlist",
                       hint: "One per line. Use *word* to match inside longer words." },
                Dial { key: "exempt_channels", label: "Allowed in these channels", kind: "channels",
                       hint: "The filter ignores these channels entirely." },
            ],
            policy: base(
                "Word Filter",
                vec![rule(
                    "words",
                    Match::Keyword { patterns: vec![], normalize: Normalize::Fold },
                    tiers(vec![rung(1, Severity::Minor, 10)], vec![rung(10, Severity::Major, 45)]),
                )],
                168,
            ),
        },
        Preset {
            id: "mass_tagging",
            name: "Mass Tagging",
            description: "One message that names a crowd, rather than a person.",
            example: "a post tagging twelve people at once",
            caveat: "Counts distinct people per message: pinging one person twenty times reads as one, because it is.",
            dials: vec![
                Dial { key: "strictness", label: "Sensitivity", kind: "strictness",
                       hint: "How much it takes to trip a rule, and how confident the result is." },
                Dial { key: "exempt_channels", label: "Allowed in these channels", kind: "channels",
                       hint: "Announcement channels where tagging everyone is the point." },
            ],
            policy: base(
                "Mass Tagging",
                vec![rule(
                    "mentions",
                    Match::Mentions {},
                    tiers(vec![rung(6, Severity::Major, 50), rung(12, Severity::Severe, 85)], vec![]),
                )],
                168,
            ),
        },
        Preset {
            id: "rate_limit",
            name: "Rate Limit",
            description: "Too many messages too quickly from one account, whatever they say.",
            example: "twelve messages in ten seconds",
            caveat: "Counts messages, not content: a fast typer in a lively channel can trip it, so preview before enabling.",
            dials: vec![
                Dial { key: "per_secs", label: "Over how many seconds", kind: "seconds",
                       hint: "The span it counts within. 10 catches a burst; 60 catches a sustained flood." },
                Dial { key: "strictness", label: "Sensitivity", kind: "strictness",
                       hint: "How much it takes to trip a rule, and how confident the result is." },
            ],
            policy: base(
                "Rate Limit",
                vec![rule(
                    "rate",
                    Match::Rate { per_secs: 10 },
                    tiers(vec![], vec![rung(6, Severity::Major, 50), rung(12, Severity::Severe, 85)]),
                )],
                168,
            ),
        },
        Preset {
            id: "blank",
            name: "Start from scratch",
            description: "An empty policy. Name it, add the rules you want, preview before anything runs.",
            example: "whatever this community actually needs",
            caveat: "A policy with no rules catches nothing, and the preview will tell you so.",
            dials: vec![
                Dial { key: "name", label: "Policy name", kind: "text",
                       hint: "What you will see in the list." },
                Dial { key: "rules", label: "Rules", kind: "rules",
                       hint: "Add as many as you like. Any one of them can convict on its own." },
                Dial { key: "exempt_channels", label: "Allowed in these channels", kind: "channels",
                       hint: "These channels are ignored entirely." },
            ],
            policy: base("New policy", vec![], 168),
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
                // A join flood may stand alone (it is a raid path in its own
                // right, kept below the acting bands); "joined recently" and
                // "barely posted" describe most of a healthy community and never
                // may.
                let describes_the_innocent =
                    matches!(r.matcher, Match::TenureLt { .. } | Match::MessagesLte { .. });
                if describes_the_innocent {
                    assert!(r.armed_by.is_some(), "preset {} rule {} must never speak alone", p.id, r.id);
                }
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

    /// A description that skips a rule is worse than none: it reads as a
    /// complete account of what runs.
    #[test]
    fn every_rule_of_every_preset_is_described() {
        for p in all() {
            let described = describe(&p.policy);
            assert_eq!(described.len(), p.policy.rules.len(), "{} left rules undescribed", p.id);
            for (d, r) in described.iter().zip(p.policy.rules.iter()) {
                assert!(!d.label.is_empty() && !d.detail.is_empty(), "{} rule {} has no words", p.id, r.id);
                assert_eq!(d.armed, r.armed_by.is_some(), "{} rule {} misreports arming", p.id, r.id);
            }
        }
    }

    /// Aggravators only mean anything behind a conviction, so a summary that
    /// does not mark them promises cover the engine will not give.
    #[test]
    fn the_shipped_defaults_mark_their_aggravators() {
        let d = describe(&super::super::harness::default_policy());
        assert!(d.iter().any(|x| x.armed), "the defaults carry aggravators and none was marked");
        assert!(d.iter().any(|x| !x.armed), "every rule marked armed would leave nothing that can convict");
    }
}
