//! Policies, for bots.
//!
//! Three things a moderation bot needs and one thing it must not do by
//! accident. It can WRITE policies, WATCH what they convict, and ACT on the
//! result — and it cannot act destructively without saying so out loud, because
//! the difference between a bot that flags and a bot that removes people is one
//! forgotten default.
//!
//! ```no_run
//! # use vector_sdk::{VectorBot, policy::*};
//! # async fn demo(bot: &VectorBot) -> vector_sdk::Result<()> {
//! let community = bot.community("fe4a…");
//!
//! // Write a rulebook.
//! community.policies().set("scam-links", Policy::preset(Preset::ScamLinks)?).await?;
//!
//! // Watch what it convicts. Dry-run: reports only, touches nobody.
//! let mut watch = community.watch_policies().await?;
//! while let Some(verdicts) = watch.next().await {
//!     for m in verdicts.actionable() {
//!         println!("{} — {} ({})", m.name(), m.why(), m.confidence());
//!     }
//! }
//! # Ok(()) }
//! ```

use crate::{Community, Result};
use vector_core::community::policy::document::{
    ArmScope, ArmedBy, Enforcement, Exempt, ExemptKind, ExemptPatterns, Match, Normalize, Policy as PolicyDoc,
    Rule, Rung, Tiers, FORMAT,
};
use vector_core::community::policy::harness::REPEAT_BURST_SECS;
use vector_core::community::policy::presets as core_presets;
use vector_core::community::policy::types::Severity;

/// The built-in templates, by name. Each is a real policy the engine
/// evaluates — picking one and hand-writing JSON produce the same artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preset {
    /// Everything already running in a community that has not written its own.
    VectorDefaults,
    ScamLinks,
    NoSpam,
    WordFilter,
}

impl Preset {
    fn id(self) -> &'static str {
        match self {
            Preset::VectorDefaults => "vector_defaults",
            Preset::ScamLinks => "scam_links",
            Preset::NoSpam => "no_spam",
            Preset::WordFilter => "word_filter",
        }
    }
}

/// How gravely a community treats a rule. The author's judgement, never a
/// score: severity decides what KIND of response fits, the engine decides how
/// convinced to be.
pub use vector_core::community::policy::types::Severity as Seriousness;

/// A policy under construction.
///
/// The numbers live here, chosen once, so nobody writing a bot has to pick a
/// weight. What a rule is worth is a property of the DETECTION — how convincing
/// "matched a keyword" is does not depend on which word it is — and gravity is
/// the only judgement the author actually makes.
#[derive(Debug, Clone)]
pub struct Policy {
    doc: PolicyDoc,
}

impl Policy {
    /// Start from a built-in template.
    pub fn preset(preset: Preset) -> Result<Self> {
        let p = core_presets::by_id(preset.id())
            .ok_or_else(|| crate::Error::Other(format!("unknown preset {}", preset.id())))?;
        Ok(Policy { doc: p.policy })
    }

    /// Start from an empty rulebook.
    pub fn named(name: impl Into<String>) -> Self {
        Policy {
            doc: PolicyDoc {
                format: FORMAT,
                requires: vec![],
                name: name.into(),
                emoji_codes: vec![],
                shields: Default::default(),
                window: Default::default(),
                exempt: Exempt::default(),
                rules: vec![],
            },
        }
    }

    /// Parse an existing document — what `policies().list()` returns.
    pub fn from_json(bytes: &str) -> Result<Self> {
        serde_json::from_str(bytes)
            .map(|doc| Policy { doc })
            .map_err(|e| crate::Error::Other(format!("policy is not valid JSON: {e}")))
    }

    pub fn rule(mut self, rule: PolicyRule) -> Self {
        self.doc.rules.push(rule.0);
        self
    }

    /// How far back the rules look. Longer windows make standing easier to
    /// earn, since a member's volume and variety are measured over it.
    pub fn window(mut self, hours: u64, max_messages: usize) -> Self {
        self.doc.window.hours = hours;
        self.doc.window.max_messages = max_messages;
        self
    }

    /// Never apply any of this in these channels.
    pub fn except_channels(mut self, channels: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.doc.exempt.channels = channels.into_iter().map(Into::into).collect();
        self
    }

    /// Validate and render. Errors name the rule and the reason, so a bot
    /// author learns what is wrong rather than that something is.
    pub fn build(mut self) -> Result<String> {
        // Declared here rather than asked of the caller: forgetting it means an
        // older engine silently ignores the alternatives under an identical hash.
        if self.doc.rules.iter().any(|r| r.armed_by.as_ref().is_some_and(|a| !a.also.is_empty()))
            && !self.doc.requires.iter().any(|k| k == "armed_by_any")
        {
            self.doc.requires.push("armed_by_any".into());
        }
        self.doc
            .validate()
            .map_err(|r| crate::Error::Other(format!("policy rejected: {r:?}")))?;
        serde_json::to_string(&self.doc).map_err(|e| crate::Error::Other(e.to_string()))
    }

    /// The document, for inspection.
    pub fn document(&self) -> &PolicyDoc {
        &self.doc
    }
}

/// One rule, in the terms an author thinks in: what to look for, and how
/// gravely to treat it.
#[derive(Debug, Clone)]
pub struct PolicyRule(Rule);

/// Weights by detection shape, chosen once. An escalating rule starts softer
/// because repetition raises it; a one-shot rule carries its full weight
/// immediately, since it either fires or does not.
fn ladder(seriousness: Severity) -> Vec<Rung> {
    let (a, b, c) = match seriousness {
        Severity::Notice => (10, 15, 20),
        Severity::Minor => (25, 35, 40),
        Severity::Major => (45, 55, 60),
        Severity::Severe => (70, 85, 90),
    };
    vec![
        Rung { hits: 1, severity: seriousness, weight: a, pierces_trusted: false },
        Rung { hits: 3, severity: seriousness, weight: b, pierces_trusted: false },
        Rung { hits: 10, severity: seriousness, weight: c, pierces_trusted: false },
    ]
}

fn one_shot(seriousness: Severity) -> u32 {
    match seriousness {
        Severity::Notice => 20,
        Severity::Minor => 30,
        Severity::Major => 40,
        Severity::Severe => 85,
    }
}

fn tiered(id: &str, matcher: Match, seriousness: Severity, per_message: bool) -> PolicyRule {
    let rungs = ladder(seriousness);
    let tiers = if per_message {
        Tiers { per_message: vec![rungs[0].clone()], per_window: vec![rungs[1].clone(), rungs[2].clone()] }
    } else {
        Tiers { per_message: vec![], per_window: rungs }
    };
    PolicyRule(Rule {
        id: id.into(),
        matcher,
        tiers: Some(tiers),
        severity: None,
        weight: None,
        pierces_trusted: false,
        family: None,
        armed_by: None,
        exempt: Exempt::default(),
        enforcement: Enforcement::Advisory,
    })
}

fn single(id: &str, matcher: Match, seriousness: Severity) -> PolicyRule {
    PolicyRule(Rule {
        id: id.into(),
        matcher,
        tiers: None,
        severity: Some(seriousness),
        weight: Some(one_shot(seriousness)),
        pierces_trusted: false,
        family: None,
        armed_by: None,
        exempt: Exempt::default(),
        enforcement: Enforcement::Advisory,
    })
}

/// Rule constructors. Every one names what it looks for; none asks for a
/// number.
impl PolicyRule {
    /// Words. Bare patterns match whole words — "class" never trips on "ass" —
    /// and `*word*` matches inside longer ones.
    pub fn words(id: &str, patterns: impl IntoIterator<Item = impl Into<String>>) -> Self {
        tiered(
            id,
            Match::Keyword {
                patterns: patterns.into_iter().map(Into::into).collect(),
                normalize: Normalize::Fold,
            },
            Severity::Minor,
            true,
        )
    }

    /// Links to these domains. Anchored on the registrable domain, so
    /// `evil.com/yoursite.com` never passes as yours.
    pub fn links(id: &str, domains: impl IntoIterator<Item = impl Into<String>>) -> Self {
        tiered(id, Match::Link { patterns: domains.into_iter().map(Into::into).collect() }, Severity::Severe, true)
    }

    /// The same message over and over from one account.
    /// One message naming a crowd. Counts DISTINCT people, so twenty pings at
    /// one person is one person.
    pub fn mass_tagging(id: &str) -> Self {
        tiered(id, Match::Mentions {}, Severity::Major, true)
    }

    /// One account posting faster than a person reasonably types, whatever they
    /// are saying.
    pub fn rate_limit(id: &str, per_secs: u64) -> Self {
        tiered(id, Match::Rate { per_secs }, Severity::Major, false)
    }

    pub fn repetition(id: &str) -> Self {
        tiered(id, Match::Repeat { normalize: Normalize::Skeleton, within_secs: Some(REPEAT_BURST_SECS) }, Severity::Major, false)
    }

    /// Too many messages too fast.
    pub fn flooding(id: &str, per_secs: u64) -> Self {
        tiered(id, Match::Rate { per_secs }, Severity::Major, false)
    }

    /// Many accounts posting the same thing — the raid shape. Heuristic: it
    /// flags, and never feeds the number a bot may act on unattended.
    pub fn cohort(id: &str) -> Self {
        single(id, Match::Cohort { min: 3, quiet_max: 2, short_factor: 3, thin_ratio: None }, Severity::Severe)
    }

    /// A burst of joins. Weak alone — a freshly-posted invite link looks
    /// exactly like this — so arm it with [`Self::only_after`].
    pub fn join_burst(id: &str, within_secs: u64, at_least: u32) -> Self {
        single(id, Match::JoinBurst { gap_secs: within_secs, min: at_least }, Severity::Major)
    }

    /// Fires only once another rule has convicted. The guard that keeps weak
    /// signals from convicting anyone by themselves.
    ///
    /// Call it more than once to accept ALTERNATIVES: the rule then fires if any
    /// named detector convicted. Hanging an aggravator off a single rule means a
    /// raid that rule cannot see silences the aggravator too, so a policy with
    /// two detectors should arm on both.
    pub fn only_after(mut self, rule_id: &str, min_subjects: Option<u32>) -> Self {
        match self.0.armed_by.as_mut() {
            Some(arm) => arm.also.push(rule_id.into()),
            None => {
                self.0.armed_by = Some(ArmedBy {
                    rule: rule_id.into(),
                    scope: if min_subjects.is_some() { ArmScope::Community } else { ArmScope::Subject },
                    min_subjects,
                    also: Vec::new(),
                });
            }
        }
        self
    }

    /// How gravely this community treats it.
    pub fn seriousness(mut self, s: Severity) -> Self {
        if let Some(t) = self.0.tiers.as_mut() {
            for r in t.per_message.iter_mut().chain(t.per_window.iter_mut()) {
                r.severity = s;
            }
            let l = ladder(s);
            for (i, r) in t.per_message.iter_mut().enumerate() {
                r.weight = l[i.min(l.len() - 1)].weight;
            }
            for (i, r) in t.per_window.iter_mut().enumerate() {
                r.weight = l[(i + 1).min(l.len() - 1)].weight;
            }
        } else {
            self.0.severity = Some(s);
            self.0.weight = Some(one_shot(s));
        }
        self
    }

    /// Reach members who have earned standing. Only ever appropriate for the
    /// gravest rules, and the validator enforces that.
    pub fn even_for_trusted(mut self) -> Self {
        self.0.pierces_trusted = true;
        if let Some(t) = self.0.tiers.as_mut() {
            for r in t.per_message.iter_mut().chain(t.per_window.iter_mut()) {
                r.pierces_trusted = r.severity == Severity::Severe;
            }
        }
        self
    }

    /// Never flag these — domains, words, or whatever this rule matches.
    pub fn allowing(mut self, values: impl IntoIterator<Item = impl Into<String>>) -> Self {
        let kind = match self.0.matcher {
            Match::Link { .. } => ExemptKind::Domain,
            _ => ExemptKind::Literal,
        };
        self.0.exempt.patterns.push(ExemptPatterns {
            kind: Some(kind),
            values: values.into_iter().map(Into::into).collect(),
        });
        self
    }

    /// Not in these channels.
    pub fn except_channels(mut self, channels: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.0.exempt.channels = channels.into_iter().map(Into::into).collect();
        self
    }

    /// Ask compliant clients not to SEND this at all. Cooperative, never
    /// security: an adversary ships a client that ignores it and is caught at
    /// receive. Only provable rules may do this, and the validator refuses
    /// patterns broad enough to mute a community.
    pub fn refuse_to_send(mut self) -> Self {
        self.0.enforcement = Enforcement::Refuse;
        self
    }
}

/// Policy management for one community.
pub struct Policies<'a> {
    pub(crate) community: &'a Community,
}

impl Policies<'_> {
    /// Every stored policy, with the validator's current verdict.
    pub async fn list(&self) -> Result<Vec<StoredPolicy>> {
        let v = self.community.core.list_community_policies(self.community.id())?;
        let rows = v["policies"].as_array().cloned().unwrap_or_default();
        Ok(rows
            .into_iter()
            .map(|r| StoredPolicy {
                id: r["policy_id"].as_str().unwrap_or_default().to_string(),
                bytes: r["bytes"].as_str().unwrap_or_default().to_string(),
                hash: r["hash"].as_str().unwrap_or_default().to_string(),
                enabled: r["enabled"].as_bool().unwrap_or(false),
                valid: r["valid"].as_bool().unwrap_or(false),
                error: r["error"].as_str().map(String::from),
            })
            .collect())
    }

    /// Store a policy, enabled. Validated first: an invalid policy is a
    /// rejected write, never a stored one that evaluates to nothing.
    pub async fn set(&self, id: &str, policy: Policy) -> Result<()> {
        let bytes = policy.build()?;
        self.community.core.set_community_policy(self.community.id(), id, &bytes, true)?;
        Ok(())
    }

    /// Store without enabling — useful for staging a rule you want to preview
    /// before it runs.
    pub async fn stage(&self, id: &str, policy: Policy) -> Result<()> {
        let bytes = policy.build()?;
        self.community.core.set_community_policy(self.community.id(), id, &bytes, false)?;
        Ok(())
    }

    pub async fn enable(&self, id: &str, on: bool) -> Result<()> {
        let stored = self.list().await?;
        let p = stored
            .iter()
            .find(|p| p.id == id)
            .ok_or_else(|| crate::Error::Other(format!("no policy {id}")))?;
        self.community.core.set_community_policy(self.community.id(), id, &p.bytes, on)?;
        Ok(())
    }

    pub async fn delete(&self, id: &str) -> Result<()> {
        self.community.core.delete_community_policy(self.community.id(), id)?;
        Ok(())
    }

    /// What a policy WOULD do against real history, without storing it or
    /// touching anyone. Always available, and the honest way to develop a rule.
    pub async fn preview(&self, policy: &Policy) -> Result<Preview> {
        let bytes = serde_json::to_string(policy.document()).map_err(|e| crate::Error::Other(e.to_string()))?;
        let v = self.community.core.preview_community_policy(self.community.id(), &bytes)?;
        serde_json::from_value(v).map_err(|e| crate::Error::Other(e.to_string()))
    }
}

#[derive(Debug, Clone)]
pub struct StoredPolicy {
    pub id: String,
    pub bytes: String,
    pub hash: String,
    pub enabled: bool,
    /// False when a policy stored under an older engine no longer validates —
    /// it is loaded and reported INERT rather than silently dropped.
    pub valid: bool,
    pub error: Option<String>,
}

/// What a policy would do, as the designer's preview reports it.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct Preview {
    pub valid: bool,
    pub error: Option<String>,
    pub flagged: Vec<PreviewRow>,
    /// Members who ALSO matched and were spared only by their standing. A short
    /// flagged list can hide a rule that catches ordinary conversation; this is
    /// the number that says so.
    pub shielded_matches: Vec<PreviewRow>,
    pub messages_cited: usize,
    pub corpus: usize,
    pub unevaluated: Vec<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct PreviewRow {
    pub npub: String,
    pub score: u32,
    pub proven: u32,
    pub band: String,
    pub shield: String,
    pub reasons: Vec<String>,
    pub tenure_days: u64,
    pub messages: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_preset_builds_and_validates() {
        for p in [Preset::VectorDefaults, Preset::ScamLinks, Preset::NoSpam, Preset::WordFilter] {
            assert!(Policy::preset(p).unwrap().build().is_ok(), "{p:?} must build");
        }
    }

    /// The builder must not let an author write a policy the validator would
    /// reject — errors belong at build time, not at save time.
    #[test]
    fn the_builder_refuses_what_the_engine_refuses() {
        // A weak signal alone: the mistake that convicted 147 of 155 members.
        let unarmed = Policy::named("x").rule(PolicyRule::join_burst("burst", 600, 5)).build();
        assert!(unarmed.is_ok(), "the engine allows it; the preview is what teaches otherwise");

        // Refusing to send needs a provable rule and a narrow pattern.
        let broad = Policy::named("x")
            .rule(PolicyRule::cohort("c").refuse_to_send())
            .build();
        assert!(broad.is_err(), "a hunch may never refuse a send");
    }

    #[test]
    fn seriousness_moves_weights_without_the_author_naming_one() {
        let mild = Policy::named("x").rule(PolicyRule::words("w", ["darn"]).seriousness(Severity::Notice)).build().unwrap();
        let grave = Policy::named("x").rule(PolicyRule::words("w", ["darn"]).seriousness(Severity::Severe)).build().unwrap();
        assert!(mild.contains("\"weight\":10"), "{mild}");
        assert!(grave.contains("\"weight\":70"), "{grave}");
    }

    fn verdict(proven: u32, band: &str, shield: &str) -> Verdict {
        Verdict {
            npub: "npub1test".into(),
            name: "test".into(),
            confidence: 90,
            proven,
            band: band.into(),
            shield: shield.into(),
            reasons: vec![],
            messages: 0,
            tenure_secs: 0,
        }
    }

    /// The distinction the whole engine rests on, at the API surface: a cohort
    /// reads 90 confidence and 0 proven — loud enough to show a human, never
    /// enough to remove someone unattended.
    #[test]
    fn inference_is_never_actionable_alone() {
        let vs = Verdicts {
            members: vec![
                verdict(0, "alert", "none"),   // a raid cohort: suspected
                verdict(85, "alert", "none"),  // a hash match: provable
                verdict(85, "alert", "trusted"), // provable, but has standing
            ],
            raid_detected: true,
        };
        assert_eq!(vs.actionable().count(), 1, "only the provable, unshielded one");
        assert_eq!(vs.needs_human().count(), 1, "the cohort goes to a person");
    }

    /// A bot that removes people must say so. The default cannot be the
    /// dangerous one.
    #[test]
    fn autopilot_floors_cannot_be_lowered_into_guesswork() {
        let core = vector_core::VectorCore;
        let community = Community { core, id: "aa".repeat(32) };
        let pilot = community.autopilot(Action::Ban);
        assert!(pilot.dry_run, "a fresh autopilot rehearses; arming is explicit");
        assert_eq!(pilot.min_proven, 70);
        // Trying to act on inference is clamped, not honoured.
        let reckless = community.autopilot(Action::Ban).min_proven(0);
        assert_eq!(reckless.min_proven, 50, "inference may never remove anyone unattended");
        assert!(community.autopilot(Action::Kick).arm().dry_run == false);
    }

    #[test]
    fn arming_reads_the_way_an_author_says_it() {
        let p = Policy::named("raid")
            .rule(PolicyRule::cohort("cohort"))
            .rule(PolicyRule::join_burst("burst", 600, 5).only_after("cohort", Some(3)))
            .build();
        assert!(p.is_ok(), "{p:?}");
    }
}

// ── Watching and acting ─────────────────────────────────────────────────────

/// What the engine convicted, in the terms a bot acts on.
#[derive(Debug, Clone)]
pub struct Verdicts {
    pub(crate) members: Vec<Verdict>,
    pub(crate) raid_detected: bool,
}

/// One member's standing right now.
#[derive(Debug, Clone)]
pub struct Verdict {
    pub npub: String,
    pub name: String,
    /// 0-99. How convinced the engine is, all evidence considered.
    pub confidence: u32,
    /// The part that survives replay — a hash match, a counted rate. Inference
    /// never reaches this number, which is why unattended action keys on it.
    pub proven: u32,
    /// clear · noted · watch · flagged · alert
    pub band: String,
    /// none · trusted · protected · indeterminate
    pub shield: String,
    /// Evidence in words: "Posted the same message as 87 other members".
    pub reasons: Vec<String>,
    pub messages: u64,
    pub tenure_secs: u64,
}

impl Verdict {
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Everything the engine could say about them, in one line.
    pub fn why(&self) -> String {
        if self.reasons.is_empty() {
            "no findings".into()
        } else {
            self.reasons.join("; ")
        }
    }

    pub fn confidence(&self) -> u32 {
        self.confidence
    }

    /// Safe to act on unattended: something REPLAYABLE convicted them, not an
    /// inference. A cohort reads 85 confidence and 0 proven — strong enough to
    /// show a human, never strong enough to remove someone on its own.
    pub fn is_provable(&self) -> bool {
        self.proven >= 50
    }

    /// The community granted them standing; leave them to a human.
    pub fn is_shielded(&self) -> bool {
        self.shield != "none"
    }
}

impl Verdicts {
    /// Members with provable convictions in the acting band — what a bot may
    /// handle by itself.
    pub fn actionable(&self) -> impl Iterator<Item = &Verdict> {
        self.members.iter().filter(|m| m.is_provable() && m.band == "alert" && !m.is_shielded())
    }

    /// Strong patterns without proof: the raid case. Show a human.
    pub fn needs_human(&self) -> impl Iterator<Item = &Verdict> {
        self.members.iter().filter(|m| !m.is_provable() && (m.band == "alert" || m.band == "flagged"))
    }

    pub fn all(&self) -> impl Iterator<Item = &Verdict> {
        self.members.iter()
    }

    pub fn raid_detected(&self) -> bool {
        self.raid_detected
    }
}

/// A polling watch over a community's verdicts.
///
/// Deliberately a poll rather than a push: evaluation reads a window of history
/// and clusters every author, so running it per message would be wasteful and
/// running it on a timer is what the design asks for anyway.
pub struct PolicyWatch {
    community: Community,
    interval: std::time::Duration,
}

impl PolicyWatch {
    /// Wait for the next evaluation and return it. `None` never happens in
    /// normal operation; it means the community went away.
    pub async fn next(&mut self) -> Option<Verdicts> {
        tokio::time::sleep(self.interval).await;
        self.community.verdicts().await.ok()
    }
}

impl Community {
    /// This community's rulebook (see [`Policies`]).
    pub fn watch_policies_every(&self, interval: std::time::Duration) -> PolicyWatch {
        PolicyWatch { community: self.clone(), interval }
    }

    /// Watch on the default cadence — often enough to catch a wave, rarely
    /// enough that a full evaluation is cheap.
    pub async fn watch_policies(&self) -> Result<PolicyWatch> {
        Ok(self.watch_policies_every(std::time::Duration::from_secs(30)))
    }

    /// Evaluate now and return every member's standing.
    pub async fn verdicts(&self) -> Result<Verdicts> {
        let v = self.core.community_moderation_intel(self.id())?;
        let report = &v["report"];
        let members = report["members"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|m| Verdict {
                npub: m["npub"].as_str().unwrap_or_default().to_string(),
                name: m["npub"].as_str().unwrap_or_default().chars().take(12).collect(),
                confidence: m["score"].as_u64().unwrap_or(0) as u32,
                proven: m["proven"].as_u64().unwrap_or(0) as u32,
                band: m["band"].as_str().unwrap_or("clear").to_string(),
                shield: if m["is_owner"].as_bool().unwrap_or(false) || m["is_admin"].as_bool().unwrap_or(false) {
                    "protected".into()
                } else {
                    match m["verdict"].as_str() {
                        Some("trusted") => "trusted".to_string(),
                        _ => "none".to_string(),
                    }
                },
                reasons: m["reasons"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|r| r.as_str().map(String::from)).collect())
                    .unwrap_or_default(),
                messages: m["messages"].as_u64().unwrap_or(0),
                tenure_secs: m["tenure_secs"].as_u64().unwrap_or(0),
            })
            .collect();
        Ok(Verdicts { members, raid_detected: report["raid_detected"].as_bool().unwrap_or(false) })
    }
}

/// What a bot does about a verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Report it and do nothing else.
    Report,
    /// Kick. They may rejoin.
    Kick,
    /// Ban. Terminal, and in a private community it rekeys around them.
    Ban,
}

/// An autopilot that acts on verdicts.
///
/// **Dry-run by default, and deliberately so.** The difference between a bot
/// that flags and a bot that removes people is one forgotten default, so the
/// default is the harmless one: it reports what it WOULD do until someone
/// writes `.arm()` and means it.
pub struct Autopilot {
    community: Community,
    action: Action,
    dry_run: bool,
    min_proven: u32,
    max_per_run: usize,
}

impl Community {
    /// Act on verdicts. Starts in dry-run: see [`Autopilot::arm`].
    pub fn autopilot(&self, action: Action) -> Autopilot {
        Autopilot {
            community: self.clone(),
            action,
            dry_run: true,
            // Only replayable evidence may remove anyone unattended.
            min_proven: 70,
            // A bug must not be able to empty a community.
            max_per_run: 25,
        }
    }
}

/// What one pass would do, or did.
#[derive(Debug, Clone)]
pub struct AutopilotRun {
    pub dry_run: bool,
    pub acted: Vec<String>,
    pub skipped: Vec<(String, String)>,
}

impl Autopilot {
    /// Take real action. Until this is called, every pass is a rehearsal.
    pub fn arm(mut self) -> Self {
        self.dry_run = false;
        self
    }

    /// How provable a conviction must be before this bot acts on it. Cannot go
    /// below 50: inference must never remove anyone unattended.
    pub fn min_proven(mut self, floor: u32) -> Self {
        self.min_proven = floor.max(50);
        self
    }

    /// Ceiling per pass, so a bug cannot empty a community.
    pub fn max_per_run(mut self, n: usize) -> Self {
        self.max_per_run = n;
        self
    }

    /// Evaluate once and act (or rehearse).
    pub async fn run_once(&self) -> Result<AutopilotRun> {
        let verdicts = self.community.verdicts().await?;
        let mut acted = Vec::new();
        let mut skipped = Vec::new();
        for v in verdicts.all() {
            if v.is_shielded() {
                continue;
            }
            if v.band != "alert" {
                continue;
            }
            if v.proven < self.min_proven {
                skipped.push((v.npub.clone(), format!("suspected, not proven ({} < {})", v.proven, self.min_proven)));
                continue;
            }
            if acted.len() >= self.max_per_run {
                skipped.push((v.npub.clone(), "run limit reached".into()));
                continue;
            }
            if self.dry_run {
                acted.push(v.npub.clone());
                continue;
            }
            let member = self.community.member(v.npub.clone());
            let outcome = match self.action {
                Action::Report => Ok(()),
                Action::Kick => member.kick().await,
                Action::Ban => member.ban().await,
            };
            match outcome {
                Ok(()) => acted.push(v.npub.clone()),
                Err(e) => skipped.push((v.npub.clone(), e.to_string())),
            }
        }
        Ok(AutopilotRun { dry_run: self.dry_run, acted, skipped })
    }
}
