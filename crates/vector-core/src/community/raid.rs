//! Raid assessment: which members read as people, and which read as spawned identities.
//!
//! A sybil raid never looks like spam from one account. It looks like one message each
//! from a hundred fresh accounts, so per-sender rate limits and per-account reputation
//! both miss it entirely. What survives is the COHORT: many distinct senders posting the
//! same shape inside one window, from identities that were minted minutes ago. This module
//! folds those signals into a per-member verdict plus the reasons behind it, so a moderator
//! sees why before acting — the output drives a rotation's retain set, and a wrong call
//! there evicts a real member.
//!
//! Conviction rests on cohort evidence, never on freshness alone: a community that just
//! posted an invite link is *supposed* to see a burst of quiet new accounts.

use std::collections::{HashMap, HashSet};

/// Below this length a shared message is too generic to convict on ("gm", "lol", "+1"),
/// so a cohort of that shape needs [`RaidParams::short_text_factor`] times the members.
const MIN_SKELETON_LEN: usize = 8;

/// Collapse a message to the shape a raid script repeats. Case, punctuation, spacing and
/// digit variation are all one line of attacker code to randomise, so none of them may
/// carry weight.
///
/// A `:shortcode:` that renders as an image is dropped rather than flattened to its name:
/// three people answering with the same reaction is a community, not a cohort, and leaving
/// the name in convicts them on a skeleton nobody typed. See [`skeleton_with`] for the
/// resolution that decides which codes are images.
pub fn skeleton(text: &str) -> String {
    skeleton_with(text, &HashSet::new())
}

/// [`skeleton`] against a set of shortcodes that actually render as images. A `:code:`
/// outside the set reaches the reader as literal text, so it stays in the skeleton —
/// otherwise wrapping the payload in colons hides a cohort in plain sight.
pub fn skeleton_with(text: &str, known: &HashSet<String>) -> String {
    strip_shortcodes(text, known)
        .chars()
        .filter(|c| c.is_alphanumeric() && !c.is_numeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

/// Remove `:name:` runs that stand in for an image: one this account can render, or a
/// short name a unicode set plausibly carries. A long unresolved run is prose in a
/// costume — `:buycheapcoinsnow:` renders as itself — so its text is kept. A lone `:`
/// or an unclosed code is left alone.
fn strip_shortcodes(text: &str, known: &HashSet<String>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find(':') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let close = after.find(':').filter(|end| {
            *end > 0 && after[..*end].chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '+')
        });
        match close {
            Some(end) => {
                let name = &after[..end];
                if !known.contains(name) && name.chars().count() >= MIN_SKELETON_LEN {
                    out.push_str(name);
                }
                rest = &after[end + 1..];
            }
            None => {
                out.push(':');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Tunables for [`assess`]. The defaults are the shape of a real raid; a moderator can
/// widen them from the panel when a wave is slower or quieter than the norm.
#[derive(Debug, Clone)]
pub struct RaidParams {
    /// A join newer than this reads as "just now".
    pub fresh_join_secs: u64,
    /// Tenure at which a member is trusted on age alone (with some volume).
    pub trusted_tenure_secs: u64,
    /// Messages that, with tenure or variety, make a member trusted.
    pub trusted_messages: u64,
    /// Distinct message shapes that read as real conversation.
    pub trusted_distinct: u64,
    /// At or below this, a member has effectively not spoken.
    pub quiet_messages: u64,
    /// Distinct senders sharing one skeleton before it counts as a cohort.
    pub min_cohort: usize,
    /// Multiplier on `min_cohort` for skeletons under [`MIN_SKELETON_LEN`].
    pub short_text_factor: usize,
    /// How far apart two joins may be and still belong to the same burst.
    pub burst_gap_secs: u64,
    /// Joins in one window before it reads as a burst rather than a busy afternoon.
    pub min_burst: usize,
    /// Shortcodes that render as images here; anything else in colons is read as text.
    pub known_shortcodes: HashSet<String>,
}

impl Default for RaidParams {
    fn default() -> Self {
        Self {
            fresh_join_secs: 24 * 60 * 60,
            trusted_tenure_secs: 7 * 24 * 60 * 60,
            trusted_messages: 5,
            trusted_distinct: 3,
            quiet_messages: 2,
            min_cohort: 3,
            short_text_factor: 3,
            burst_gap_secs: 10 * 60,
            min_burst: 5,
            known_shortcodes: HashSet::new(),
        }
    }
}

/// What the caller knows about one member before any judgement is applied.
#[derive(Debug, Clone, Default)]
pub struct MemberSignals {
    pub npub: String,
    /// Guestbook join, ms. `0` = unknown (predates the store, or the Join was lost).
    pub joined_at_ms: u64,
    /// Invite label they joined through, when the Guestbook carried the attribution.
    pub invite_label: Option<String>,
    pub messages: u64,
    /// First and last posting times, seconds. `0` when they have never posted.
    pub first_secs: u64,
    pub last_secs: u64,
    /// Their texts from the recent window only — the cohort evidence.
    pub texts: Vec<String>,
    pub is_owner: bool,
    pub is_admin: bool,
    pub is_me: bool,
}

/// Where a member lands. Only [`Verdict::Suspect`] is pre-selected for removal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    /// The owner, yourself, or a role-holder: never removable from this panel.
    Protected,
    /// Real history behind them. Kept by default, and never auto-selected.
    Trusted,
    /// Not enough either way — the moderator decides.
    Neutral,
    /// Cohort evidence convicts them.
    Suspect,
}

/// One member's verdict with the evidence that produced it.
#[derive(Debug, Clone, serde::Serialize)]
pub struct MemberAssessment {
    pub npub: String,
    pub verdict: Verdict,
    /// Ordering weight only — the verdict is decided by rules, not by a threshold.
    pub score: u32,
    pub reasons: Vec<String>,
    pub joined_at_ms: u64,
    pub invite_label: Option<String>,
    pub messages: u64,
    /// Distinct message shapes seen in the recent window.
    pub distinct: u64,
    /// Other members who posted a shape this member also posted.
    pub cohort: u64,
    /// Seconds between their first trace (join or first post) and `now`.
    pub tenure_secs: u64,
    pub last_secs: u64,
    pub is_owner: bool,
    pub is_admin: bool,
    pub is_me: bool,
}

/// Cap on the npubs carried per cohort. The panel shows the count and the sample; a
/// 500-strong cluster would otherwise put 30KB of bech32 through the IPC boundary for
/// nothing.
const COHORT_SAMPLE_CAP: usize = 24;

/// A duplicate-text cluster: the evidence a raid actually leaves behind.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Cohort {
    /// A representative verbatim message, for display.
    pub sample: String,
    /// The true size, which `members` may be a truncated sample of.
    pub size: usize,
    pub members: Vec<String>,
}

/// The whole picture for one community.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RaidReport {
    pub members: Vec<MemberAssessment>,
    pub suspects: usize,
    pub trusted: usize,
    pub protected: usize,
    pub cohorts: Vec<Cohort>,
    /// Members whose joins landed inside the convicted burst window.
    pub burst_size: usize,
    /// Start/end of that window, ms. Absent when no burst was found.
    pub burst_from_ms: u64,
    pub burst_to_ms: u64,
    /// True when the cohort evidence is strong enough to call this a raid rather
    /// than a busy day.
    pub raid_detected: bool,
}

/// Judge every member against the cohort evidence. `now_secs` is the clock the
/// caller reads once, so a long assessment can't drift mid-pass.
pub fn assess(signals: &[MemberSignals], now_secs: u64, p: &RaidParams) -> RaidReport {
    // ── Cohorts: skeleton → the distinct members who posted it ───────────────
    let mut by_skeleton: HashMap<String, HashSet<String>> = HashMap::new();
    let mut sample_of: HashMap<String, String> = HashMap::new();
    for s in signals {
        for t in &s.texts {
            let sk = skeleton_with(t, &p.known_shortcodes);
            if sk.is_empty() {
                continue;
            }
            by_skeleton.entry(sk.clone()).or_default().insert(s.npub.clone());
            sample_of.entry(sk).or_insert_with(|| t.clone());
        }
    }
    let volume: HashMap<&str, u64> = signals.iter().map(|s| (s.npub.as_str(), s.messages)).collect();
    // A cohort convicts on two counts, not one. Size alone would hang a community
    // catchphrase on whoever repeats it, so the cluster must ALSO be thin: a script
    // gives each identity one line, while regulars who happen to share a greeting have
    // hundreds of other messages between them. A short skeleton is cheap coincidence on
    // top of that, so it clears a higher size bar.
    let convicting = |sk: &str, members: &HashSet<String>| -> bool {
        let need = if sk.chars().count() < MIN_SKELETON_LEN {
            p.min_cohort.saturating_mul(p.short_text_factor)
        } else {
            p.min_cohort
        };
        if members.len() < need {
            return false;
        }
        let thin = members
            .iter()
            .filter(|n| volume.get(n.as_str()).copied().unwrap_or(0) <= p.quiet_messages)
            .count();
        thin * 2 >= members.len()
    };

    let mut cohorts: Vec<Cohort> = Vec::new();
    // npub → the largest convicting cohort they belong to (excluding themselves).
    let mut cohort_of: HashMap<String, u64> = HashMap::new();
    for (sk, members) in &by_skeleton {
        if !convicting(sk, members) {
            continue;
        }
        let peers = members.len().saturating_sub(1) as u64;
        for m in members {
            let e = cohort_of.entry(m.clone()).or_insert(0);
            *e = (*e).max(peers);
        }
        let mut list: Vec<String> = members.iter().cloned().collect();
        list.sort();
        let size = list.len();
        list.truncate(COHORT_SAMPLE_CAP);
        cohorts.push(Cohort { sample: sample_of.get(sk).cloned().unwrap_or_default(), size, members: list });
    }
    cohorts.sort_by(|a, b| b.size.cmp(&a.size));

    // ── Join burst: the densest run of joins within `burst_gap_secs` ─────────
    let mut joins: Vec<(u64, &str)> = signals
        .iter()
        .filter(|s| s.joined_at_ms > 0 && !s.is_owner)
        .map(|s| (s.joined_at_ms, s.npub.as_str()))
        .collect();
    joins.sort_by_key(|(at, _)| *at);
    let gap_ms = p.burst_gap_secs.saturating_mul(1000);
    let mut best: (usize, usize) = (0, 0); // [start, end) into `joins`
    let mut start = 0usize;
    for end in 0..joins.len() {
        while joins[end].0.saturating_sub(joins[start].0) > gap_ms {
            start += 1;
        }
        if end + 1 - start > best.1 - best.0 {
            best = (start, end + 1);
        }
    }
    let burst: HashSet<&str> = if best.1 - best.0 >= p.min_burst {
        joins[best.0..best.1].iter().map(|(_, n)| *n).collect()
    } else {
        HashSet::new()
    };
    let (burst_from_ms, burst_to_ms) = if burst.is_empty() {
        (0, 0)
    } else {
        (joins[best.0].0, joins[best.1 - 1].0)
    };
    // A burst only convicts its silent members once the noisy ones are already
    // convicted — otherwise a freshly-posted invite link reads as an attack.
    let burst_convicted = burst.iter().filter(|n| cohort_of.contains_key(**n)).count() >= p.min_cohort;

    // ── Per-member verdicts ──────────────────────────────────────────────────
    let now_ms = now_secs.saturating_mul(1000);
    let mut members: Vec<MemberAssessment> = Vec::with_capacity(signals.len());
    for s in signals {
        let distinct: u64 = s.texts.iter().map(|t| skeleton_with(t, &p.known_shortcodes)).filter(|sk| !sk.is_empty()).collect::<HashSet<_>>().len() as u64;
        let cohort = cohort_of.get(&s.npub).copied().unwrap_or(0);
        let in_burst = burst.contains(s.npub.as_str());
        // Oldest trace wins: a member whose Join was lost still has their first post.
        let first_seen_secs = match (s.joined_at_ms / 1000, s.first_secs) {
            (0, f) => f,
            (j, 0) => j,
            (j, f) => j.min(f),
        };
        let tenure_secs = if first_seen_secs == 0 { 0 } else { now_secs.saturating_sub(first_seen_secs) };
        let fresh = s.joined_at_ms > 0 && now_ms.saturating_sub(s.joined_at_ms) <= p.fresh_join_secs.saturating_mul(1000);

        let mut reasons: Vec<String> = Vec::new();
        let mut score: u32 = 0;

        let verdict = if s.is_owner || s.is_me || s.is_admin {
            if s.is_owner {
                reasons.push("Community owner".into());
            } else if s.is_me {
                reasons.push("You".into());
            } else {
                reasons.push("Holds a role".into());
            }
            Verdict::Protected
        } else {
            if cohort > 0 {
                score += 40 + (cohort.min(50) as u32);
                reasons.push(format!("Posted the same message as {cohort} other member{}", if cohort == 1 { "" } else { "s" }));
            }
            if in_burst && burst_convicted {
                score += 15;
                reasons.push(format!("Joined in a burst of {}", burst.len()));
            }
            if fresh {
                score += 10;
                reasons.push("Joined in the last 24h".into());
            }
            if s.messages == 0 {
                score += 5;
                reasons.push("Has never posted".into());
            } else if s.messages <= p.quiet_messages {
                score += 5;
                reasons.push(format!("Only {} message{}", s.messages, if s.messages == 1 { "" } else { "s" }));
            }
            if s.messages >= 2 && distinct == 1 {
                score += 10;
                reasons.push("Repeats one message".into());
            }

            let long_tenure = tenure_secs >= p.trusted_tenure_secs;
            let talks = s.messages >= p.trusted_messages;
            let varied = distinct >= p.trusted_distinct;

            // Trust is checked BEFORE the cohort, so a member with real history is
            // never unticked by default. The asymmetry is deliberate: a missed raider
            // is one click to add, an evicted two-year member is not recoverable.
            if (long_tenure && talks) || (talks && varied) {
                // The row already prints the numbers; this says what they earned.
                reasons.clear();
                reasons.push(if long_tenure && talks {
                    "Long-standing, active member".to_string()
                } else {
                    "Varied conversation history".to_string()
                });
                Verdict::Trusted
            } else if cohort > 0 || (in_burst && burst_convicted && s.messages == 0) {
                Verdict::Suspect
            } else {
                Verdict::Neutral
            }
        };

        members.push(MemberAssessment {
            npub: s.npub.clone(),
            verdict,
            score,
            reasons,
            joined_at_ms: s.joined_at_ms,
            invite_label: s.invite_label.clone(),
            messages: s.messages,
            distinct,
            cohort,
            tenure_secs,
            last_secs: s.last_secs,
            is_owner: s.is_owner,
            is_admin: s.is_admin,
            is_me: s.is_me,
        });
    }

    // Suspects first, then by evidence weight — the panel's default reading order
    // is "what do I need to look at".
    members.sort_by(|a, b| {
        let rank = |v: Verdict| match v {
            Verdict::Suspect => 0,
            Verdict::Neutral => 1,
            Verdict::Trusted => 2,
            Verdict::Protected => 3,
        };
        rank(a.verdict).cmp(&rank(b.verdict)).then(b.score.cmp(&a.score)).then(a.npub.cmp(&b.npub))
    });

    let suspects = members.iter().filter(|m| m.verdict == Verdict::Suspect).count();
    let trusted = members.iter().filter(|m| m.verdict == Verdict::Trusted).count();
    let protected = members.iter().filter(|m| m.verdict == Verdict::Protected).count();
    RaidReport {
        raid_detected: suspects >= p.min_cohort && !cohorts.is_empty(),
        members,
        suspects,
        trusted,
        protected,
        cohorts,
        burst_size: burst.len(),
        burst_from_ms,
        burst_to_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR: u64 = 3_600;
    const DAY: u64 = 86_400;

    fn member(npub: &str) -> MemberSignals {
        MemberSignals { npub: npub.to_string(), ..Default::default() }
    }

    /// now = a fixed clock so every case reads the same age.
    const NOW: u64 = 1_700_000_000;

    fn raider(npub: &str, at_secs: u64, text: &str) -> MemberSignals {
        MemberSignals {
            npub: npub.to_string(),
            joined_at_ms: at_secs * 1000,
            messages: 1,
            first_secs: at_secs,
            last_secs: at_secs,
            texts: vec![text.to_string()],
            ..Default::default()
        }
    }

    fn regular(npub: &str, joined_days_ago: u64, messages: u64, texts: &[&str]) -> MemberSignals {
        let joined = NOW - joined_days_ago * DAY;
        MemberSignals {
            npub: npub.to_string(),
            joined_at_ms: joined * 1000,
            messages,
            first_secs: joined,
            last_secs: NOW - HOUR,
            texts: texts.iter().map(|t| t.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn skeleton_ignores_the_cheap_variations() {
        assert_eq!(skeleton("Hello World!"), skeleton("hello,   world"));
        assert_eq!(skeleton("hello world 123"), skeleton("HELLO WORLD 4567"));
        assert_ne!(skeleton("hello world"), skeleton("goodbye world"));
    }

    #[test]
    fn a_payload_wrapped_in_colons_still_has_a_skeleton() {
        assert_eq!(skeleton(":buycheapcoinsnow:"), "buycheapcoinsnow");
        // Short codes read as emoji even unresolved; a rendering pack silences the rest.
        assert!(skeleton(":fire:").is_empty());
        assert!(skeleton(":+1:").is_empty());
        assert_eq!(skeleton("nice one :fire:"), skeleton("nice one"));
        let known = HashSet::from(["buycheapcoinsnow".to_string()]);
        assert!(skeleton_with(":buycheapcoinsnow:", &known).is_empty());
    }

    #[test]
    fn a_cohort_hiding_behind_shortcode_colons_still_convicts() {
        let mut signals: Vec<MemberSignals> = (0..40)
            .map(|i| raider(&format!("npub_raider_{i}"), NOW - 60, ":freeairdropclaimnow:"))
            .collect();
        signals.push(regular("npub_owner", 400, 900, &["morning all", "shipping today", "nice one"]));
        signals[40].is_owner = true;

        let r = assess(&signals, NOW, &RaidParams::default());
        assert!(r.raid_detected, "colons must not blind the cohort");
        assert_eq!(r.suspects, 40);
    }

    #[test]
    fn a_hundred_fresh_npubs_saying_one_thing_is_a_raid() {
        let mut signals: Vec<MemberSignals> = (0..100)
            .map(|i| raider(&format!("npub_raider_{i}"), NOW - 60, "hello world"))
            .collect();
        signals.push(regular("npub_owner", 400, 900, &["morning all", "shipping today", "nice one"]));
        signals[100].is_owner = true;

        let r = assess(&signals, NOW, &RaidParams::default());
        assert!(r.raid_detected, "cohort of 100 identical posts must convict");
        assert_eq!(r.suspects, 100, "every raider, and only the raiders");
        assert_eq!(r.members.iter().filter(|m| m.verdict == Verdict::Protected).count(), 1);
        assert_eq!(r.cohorts.len(), 1);
        assert_eq!(r.cohorts[0].size, 100, "the true size survives the display cap");
    }

    #[test]
    fn a_quiet_newcomer_alone_is_not_convicted() {
        let signals = vec![
            regular("npub_old", 400, 900, &["morning all", "shipping today", "nice one"]),
            MemberSignals {
                npub: "npub_new".into(),
                joined_at_ms: (NOW - 600) * 1000,
                messages: 1,
                first_secs: NOW - 500,
                last_secs: NOW - 500,
                texts: vec!["hey everyone, glad to be here".into()],
                ..Default::default()
            },
        ];
        let r = assess(&signals, NOW, &RaidParams::default());
        assert_eq!(r.suspects, 0, "freshness alone must never convict");
        assert_eq!(r.members.iter().find(|m| m.npub == "npub_new").unwrap().verdict, Verdict::Neutral);
    }

    #[test]
    fn an_invite_link_burst_of_real_people_is_not_a_raid() {
        // Twenty newcomers inside ten minutes, each saying something of their own.
        let greetings = [
            "hi there", "glad to join", "hello from berlin", "whats up", "found this via nostr",
            "hey all", "good to be here", "long time lurker", "anyone here into rust", "morning",
            "just joined, what is this about", "greetings", "hello hello", "howdy folks",
            "nice community", "reading the pins now", "is there a channel for dev talk",
            "coming over from the other group", "hola", "yo",
        ];
        let signals: Vec<MemberSignals> = (0..20)
            .map(|i| {
                let at = NOW - 300 + i * 10;
                MemberSignals {
                    npub: format!("npub_person_{i}"),
                    joined_at_ms: at * 1000,
                    messages: 1,
                    first_secs: at,
                    last_secs: at,
                    texts: vec![greetings[i as usize].to_string()],
                    ..Default::default()
                }
            })
            .collect();
        let r = assess(&signals, NOW, &RaidParams::default());
        assert_eq!(r.suspects, 0, "a burst with no duplicate text is just a busy afternoon");
        assert!(!r.raid_detected);
    }

    #[test]
    fn silent_members_of_a_convicted_burst_are_caught_too() {
        let mut signals: Vec<MemberSignals> = (0..10)
            .map(|i| raider(&format!("npub_loud_{i}"), NOW - 120 + i, "buy cheap coins now"))
            .collect();
        // Same wave, never spoke — the sleeper accounts a text-only rule misses.
        for i in 0..5 {
            let at = NOW - 100 + i;
            signals.push(MemberSignals {
                npub: format!("npub_silent_{i}"),
                joined_at_ms: at * 1000,
                ..Default::default()
            });
        }
        let r = assess(&signals, NOW, &RaidParams::default());
        assert_eq!(r.suspects, 15, "the whole wave, loud and silent");
    }

    #[test]
    fn short_shared_text_needs_a_bigger_cohort() {
        // "gm" from four people is a morning, not an attack.
        let signals: Vec<MemberSignals> =
            (0..4).map(|i| raider(&format!("npub_{i}"), NOW - 60 + i, "gm")).collect();
        let r = assess(&signals, NOW, &RaidParams::default());
        assert_eq!(r.suspects, 0, "min_cohort * short_text_factor = 9 needed for a 2-char skeleton");

        let many: Vec<MemberSignals> =
            (0..12).map(|i| raider(&format!("npub_{i}"), NOW - 60 + i, "gm")).collect();
        assert_eq!(assess(&many, NOW, &RaidParams::default()).suspects, 12);
    }

    #[test]
    fn tenure_and_variety_earn_trust_that_survives_a_raid() {
        let mut signals: Vec<MemberSignals> = (0..20)
            .map(|i| raider(&format!("npub_raider_{i}"), NOW - 60, "hello world"))
            .collect();
        // This veteran ALSO said "hello world" once, so they sit inside the convicted
        // cohort. Tenure still wins: the default selection must not evict them.
        signals.push(regular("npub_veteran", 90, 400, &["morning", "on it", "shipped", "hello world"]));
        let r = assess(&signals, NOW, &RaidParams::default());
        let vet = r.members.iter().find(|m| m.npub == "npub_veteran").unwrap();
        assert_eq!(vet.verdict, Verdict::Trusted, "cohort membership must not outrank real history");
        assert!(vet.cohort > 0, "they are genuinely in the cohort — the panel just keeps them ticked");
        assert!(vet.reasons.iter().any(|s| s.contains("Long-standing")));
        assert_eq!(r.suspects, 20, "the raiders, and only the raiders");
    }

    #[test]
    fn role_holders_and_self_are_never_selectable() {
        let mut admin = member("npub_admin");
        admin.is_admin = true;
        admin.joined_at_ms = (NOW - 60) * 1000;
        admin.texts = vec!["hello world".into()];
        admin.messages = 1;
        let mut me = member("npub_me");
        me.is_me = true;
        let mut signals = vec![admin, me];
        signals.extend((0..10).map(|i| raider(&format!("npub_r{i}"), NOW - 60, "hello world")));

        let r = assess(&signals, NOW, &RaidParams::default());
        assert_eq!(r.members.iter().find(|m| m.npub == "npub_admin").unwrap().verdict, Verdict::Protected);
        assert_eq!(r.members.iter().find(|m| m.npub == "npub_me").unwrap().verdict, Verdict::Protected);
        assert_eq!(r.suspects, 10, "protection wins over cohort evidence");
    }

    #[test]
    fn suspects_sort_ahead_of_everyone_else() {
        let mut signals = vec![regular("npub_old", 400, 900, &["a", "b", "c", "d", "e"])];
        signals.extend((0..5).map(|i| raider(&format!("npub_r{i}"), NOW - 60, "free airdrop here")));
        let r = assess(&signals, NOW, &RaidParams::default());
        assert!(r.members[0].verdict == Verdict::Suspect);
        assert_eq!(r.members.last().unwrap().npub, "npub_old");
    }

    #[test]
    fn a_community_catchphrase_shared_by_regulars_is_not_a_cohort() {
        // Five long-standing members who all greet with the same line. Same skeleton,
        // same count as a small raid — but every one of them has real volume behind it.
        let signals: Vec<MemberSignals> = (0..5)
            .map(|i| regular(&format!("npub_regular_{i}"), 200, 400, &["gm vectorians", "on it", "shipped it", "nice"]))
            .collect();
        let r = assess(&signals, NOW, &RaidParams::default());
        assert!(r.cohorts.is_empty(), "a shared phrase between chatty members is not evidence");
        assert_eq!(r.suspects, 0);
    }

    #[test]
    fn one_line_identities_still_convict_beside_a_chatty_member() {
        // The raid wave, plus one regular who happens to use the same words. The cohort
        // is still overwhelmingly thin, so it convicts — and the regular stays trusted.
        let mut signals: Vec<MemberSignals> = (0..30)
            .map(|i| raider(&format!("npub_r{i}"), NOW - 90 + i, "claim your free airdrop"))
            .collect();
        signals.push(regular("npub_regular", 200, 400, &["claim your free airdrop", "lol", "sure", "ok"]));
        let r = assess(&signals, NOW, &RaidParams::default());
        assert_eq!(r.suspects, 30);
        assert_eq!(r.members.iter().find(|m| m.npub == "npub_regular").unwrap().verdict, Verdict::Trusted);
    }

    #[test]
    fn shared_emoji_reactions_never_form_a_cohort() {
        // Three regulars answering with the same custom emoji is a community, not a raid.
        let signals: Vec<MemberSignals> = (0..6)
            .map(|i| raider(&format!("npub_{i}"), NOW - 300 + i, ":vector_logo:"))
            .collect();
        let p = RaidParams { known_shortcodes: HashSet::from(["vector_logo".to_string()]), ..Default::default() };
        let r = assess(&signals, NOW, &p);
        assert_eq!(r.suspects, 0);
        assert!(r.cohorts.is_empty(), "an emoji-only message carries no convicting content");
    }

    #[test]
    fn a_shortcode_does_not_hide_the_text_around_it() {
        assert_eq!(skeleton("free airdrop :fire: click here"), skeleton("free airdrop click here"));
        assert_eq!(skeleton(":smile:"), "");
        // A bare colon is punctuation, not an unterminated shortcode.
        assert_eq!(skeleton("note: buy now"), skeleton("note buy now"));
        assert!(!skeleton("ratio 3:1 wins").is_empty());
    }

    #[test]
    fn an_empty_community_assesses_cleanly() {
        let r = assess(&[], NOW, &RaidParams::default());
        assert_eq!(r.suspects, 0);
        assert!(!r.raid_detected);
        assert_eq!(r.burst_size, 0);
    }
}
