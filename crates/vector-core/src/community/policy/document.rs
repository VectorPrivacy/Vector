//! The policy document: what a community declares, and what the validator
//! refuses (§5, §6, §12).
//!
//! Policies are UNTRUSTED input — a community you join hands your CPU a
//! document to evaluate — so every cap is enforced here at validation, never at
//! evaluation. A rejected policy is INERT (it evaluated nothing), which is a
//! reported state rather than a silent skip: an empty subject list must never
//! read as "everyone is clean".

use super::types::{caps, Basis, InertReason, Scope, Severity};
use serde::{Deserialize, Serialize};

/// The validator's closed code list. A policy failing several checks reports
/// the FIRST code in this order, so a multiply-invalid policy still produces
/// one deterministic set of bytes.
pub mod code {
    pub const UNKNOWN_RULE_TYPE: &str = "unknown_rule_type";
    pub const UNKNOWN_NORMALIZE: &str = "unknown_normalize";
    pub const RULE_ID_DUPLICATE: &str = "rule_id_duplicate";
    pub const RULE_ID_NOT_ASCII: &str = "rule_id_not_ascii";
    pub const CAP_EXCEEDED: &str = "cap_exceeded";
    pub const WINDOW_OUT_OF_RANGE: &str = "window_out_of_range";
    pub const WEIGHT_OUT_OF_RANGE: &str = "weight_out_of_range";
    pub const TIERS_AND_DIRECT_FORM: &str = "tiers_and_direct_form";
    pub const MISSING_REQUIRED_PARAMETER: &str = "missing_required_parameter";
    pub const RUNG_ORDER_NOT_ASCENDING: &str = "rung_order_not_ascending";
    pub const PIERCES_BELOW_SEVERE: &str = "pierces_below_severe";
    pub const SCOPE_NOT_ADMISSIBLE: &str = "scope_not_admissible";
    pub const DIRECT_FORM_NOT_ADMISSIBLE: &str = "direct_form_not_admissible";
    pub const FAMILY_ON_TIERED_RULE: &str = "family_on_tiered_rule";
    pub const FAMILY_REASSIGNED_BUILTIN: &str = "family_reassigned_builtin";
    pub const BOUNDARY_ON_STRIPPING_NORMALIZER: &str = "boundary_on_stripping_normalizer";
    pub const DOMAIN_EXEMPT_ON_STRIPPING_NORMALIZER: &str = "domain_exempt_on_stripping_normalizer";
    pub const REFUSE_ON_HEURISTIC: &str = "refuse_on_heuristic";
    pub const REFUSE_TOO_BROAD: &str = "refuse_too_broad";
    pub const REFUSE_ON_UNNARROWED_RULE: &str = "refuse_on_unnarrowed_rule";
    pub const ARMED_BY_UNKNOWN_RULE: &str = "armed_by_unknown_rule";
    pub const ARMED_BY_NESTED: &str = "armed_by_nested";
    pub const ARMED_BY_MIN_SUBJECTS_WITH_SUBJECT_SCOPE: &str = "armed_by_min_subjects_with_subject_scope";
    pub const CORROBORATOR_NOT_CONTENT_DERIVED: &str = "corroborator_not_content_derived";
    pub const EID_EXPECTED: &str = "eid_expected";
}

/// Normalizer names (the frozen bundle, §7.4). `skeleton` deletes separators,
/// which is why it refuses word boundaries and domain exemptions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Normalize {
    None,
    Fold,
    Skeleton,
    Confusables,
}

impl Normalize {
    /// Does this normalizer delete the separators a token boundary needs?
    pub fn strips_separators(self) -> bool {
        matches!(self, Normalize::Skeleton)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExemptKind {
    Literal,
    Wildcard,
    Domain,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ExemptPatterns {
    pub kind: Option<ExemptKind>,
    #[serde(default)]
    pub values: Vec<String>,
}

/// Exemptions remove TARGETS, never corpus statistics: exempt content and
/// members stay in every aggregate input (cohort sizes, thinness denominators,
/// tenure, volume) and are barred only from being cited or convicted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Exempt {
    /// A LIST: policy-level and rule-level lists concatenate, each entry
    /// matching under its own kind, so a mixed-kind union is well-defined.
    #[serde(default)]
    pub patterns: Vec<ExemptPatterns>,
    #[serde(default)]
    pub roles: Vec<String>,
    #[serde(default)]
    pub channels: Vec<String>,
}

/// One rung of an escalation ladder. Severity, weight and piercing are declared
/// PER RUNG — the rule is a ladder and the engine reports which rung fired.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rung {
    pub hits: u32,
    pub severity: Severity,
    pub weight: u32,
    #[serde(default)]
    pub pierces_trusted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Tiers {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub per_message: Vec<Rung>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub per_window: Vec<Rung>,
}

/// A rule fires only after the named rule (same policy) convicted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArmedBy {
    pub rule: String,
    pub scope: ArmScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_subjects: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArmScope {
    Subject,
    Community,
}

/// Phase-1 rule types. Each entry states its admissible scopes and basis; the
/// validator rejects anything the spec does not define, so an undefined rule
/// type can never evaluate as a silent no-op.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Match {
    /// Bare pattern = token-anchored (the Scunthorpe guard); `*word`, `word*`,
    /// `*word*` relax an edge; `\*` is a literal asterisk. No `boundary` field:
    /// the grammar makes it redundant.
    Keyword { patterns: Vec<String>, normalize: Normalize },
    Regex {
        patterns: Vec<String>,
        normalize: Normalize,
        #[serde(default)]
        boundary_word: bool,
    },
    /// Hits = distinct non-exempt registrable domains per message. Carries no
    /// span (its hit is a domain, not a text range).
    Link {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        patterns: Vec<String>,
    },
    /// Window-only. Hits = occurrences of the most-repeated normalized text by
    /// that author; ties by the normalized key ascending.
    Repeat { normalize: Normalize },
    /// Window-only. Half-open sliding window; candidate starts are the author's
    /// own message timestamps.
    Rate { per_secs: u64 },
    /// Hits = p-tags on the message. Inline `@name` is NOT a mention.
    Mentions {},
    /// Cross-account same-skeleton clustering. Whole-only, Heuristic.
    Cohort { min: u32, quiet_max: u32, short_factor: u32, thin_ratio: Option<Ratio> },
    /// Densest run of joins within `gap_secs`; earliest wins on ties.
    JoinBurst { gap_secs: u64, min: u32 },
    /// Member-plane aggravators: citation-less, immutably family-tagged.
    TenureLt { secs: u64 },
    MessagesLte { n: u32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ratio {
    pub num: u32,
    pub denom: u32,
}

impl Match {
    pub fn type_name(&self) -> &'static str {
        match self {
            Match::Keyword { .. } => "keyword",
            Match::Regex { .. } => "regex",
            Match::Link { .. } => "link",
            Match::Repeat { .. } => "repeat",
            Match::Rate { .. } => "rate",
            Match::Mentions {} => "mentions",
            Match::Cohort { .. } => "cohort",
            Match::JoinBurst { .. } => "join_burst",
            Match::TenureLt { .. } => "tenure_lt",
            Match::MessagesLte { .. } => "messages_lte",
        }
    }

    /// Deterministic = byte-provable and replayable by anyone. Only these feed
    /// `proven`, and only these may `refuse`.
    pub fn basis(&self) -> Basis {
        match self {
            Match::Cohort { .. } | Match::JoinBurst { .. } => Basis::Heuristic,
            _ => Basis::Deterministic,
        }
    }

    /// Admissible scopes. A window-level quantity must not also declare
    /// per_message tiers, or both convictions enter the combinator and
    /// double-count one fact.
    pub fn admits(&self, scope: Scope) -> bool {
        match self {
            Match::Keyword { .. } | Match::Regex { .. } | Match::Link { .. } | Match::Mentions {} => {
                matches!(scope, Scope::PerMessage | Scope::PerWindow)
            }
            Match::Repeat { .. } | Match::Rate { .. } => scope == Scope::PerWindow,
            Match::Cohort { .. } | Match::JoinBurst { .. } | Match::TenureLt { .. } | Match::MessagesLte { .. } => {
                scope == Scope::Whole
            }
        }
    }

    /// Built-in immutable family tag — the fresh-account proxies fold together
    /// so correlated signals never OR into an inflated score.
    pub fn builtin_family(&self) -> Option<&'static str> {
        match self {
            Match::TenureLt { .. } | Match::MessagesLte { .. } => Some("fresh-account"),
            _ => None,
        }
    }

    /// Cites content (so a conviction needs at least one citable citation), or
    /// is member-state (citation-less by nature).
    pub fn is_content_derived(&self) -> bool {
        !matches!(self, Match::TenureLt { .. } | Match::MessagesLte { .. })
    }

    fn patterns(&self) -> &[String] {
        match self {
            Match::Keyword { patterns, .. } | Match::Regex { patterns, .. } | Match::Link { patterns } => patterns,
            _ => &[],
        }
    }

    fn normalize(&self) -> Option<Normalize> {
        match self {
            Match::Keyword { normalize, .. } | Match::Regex { normalize, .. } | Match::Repeat { normalize } => {
                Some(*normalize)
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Enforcement {
    #[default]
    Advisory,
    Warn,
    /// Only Deterministic rules may refuse — a heuristic maybe-conviction
    /// refusing speech is indefensible. Cooperative, never security: an
    /// adversary ships a non-enforcing client and is caught at receive.
    Refuse,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    pub id: String,
    #[serde(rename = "match")]
    pub matcher: Match,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tiers: Option<Tiers>,
    /// Direct single-rung form (scope `Whole`), for rules that admit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub severity: Option<Severity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weight: Option<u32>,
    #[serde(default)]
    pub pierces_trusted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub armed_by: Option<ArmedBy>,
    #[serde(default)]
    pub exempt: Exempt,
    #[serde(default)]
    pub enforcement: Enforcement,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Window {
    pub hours: u64,
    pub max_messages: usize,
}

impl Default for Window {
    fn default() -> Self {
        Window { hours: 24, max_messages: 500 }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Shields {
    pub trusted: TrustedBar,
}

/// Standing, earned three ways. Thresholds live in the POLICY (a locally
/// tunable shield would make two mods convict differently under identical
/// hashes), and every input is a shared fact — tenure from the guestbook, roles
/// from the stamped roster, activity over the DECLARED window — so two clients
/// compute the same standing.
///
/// A member is Trusted if ANY path clears:
///  * **role** — the community granted them one. A cosmetic role is not
///    immunity (that is `Protected`, and it keys on moderation permissions),
///    but it is a vouch, so it earns the gate a regular gets.
///  * **veteran** — long tenure plus any activity at all. Someone who has been
///    here for months and still talks is not a raid.
///  * **active** — tenure AND volume AND variety together, all `>=`. Never
///    volume alone: five varied lines from a script must not buy immunity,
///    which is why every path carries a tenure floor a fresh account cannot
///    clear.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustedBar {
    /// Tenure floor for the active path.
    pub tenure_secs: u64,
    pub messages: u64,
    pub distinct: u64,
    /// Tenure that earns trust on its own, given any activity.
    pub veteran_secs: u64,
    /// Holding any role earns trust.
    pub roles_trust: bool,
}

impl Default for TrustedBar {
    fn default() -> Self {
        TrustedBar {
            tenure_secs: 7 * 24 * 3600,
            messages: 5,
            distinct: 3,
            veteran_secs: 30 * 24 * 3600,
            roles_trust: true,
        }
    }
}

impl Default for Shields {
    fn default() -> Self {
        Shields { trusted: TrustedBar::default() }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Policy {
    pub format: u32,
    /// Must-understand keys: an engine that does not know one marks the policy
    /// INERT rather than silently convicting differently under an identical
    /// policy_hash.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requires: Vec<String>,
    pub name: String,
    /// Community-declared shortcodes: resolution is community-scoped, never
    /// viewer-scoped, or two mods compute different skeletons.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub emoji_codes: Vec<String>,
    #[serde(default)]
    pub shields: Shields,
    #[serde(default)]
    pub window: Window,
    #[serde(default)]
    pub exempt: Exempt,
    pub rules: Vec<Rule>,
}

pub const FORMAT: u32 = 1;

/// Keys this engine understands in `requires`.
const KNOWN_REQUIRED: &[&str] = &["emoji_codes", "shields", "window", "exempt"];

impl Policy {
    /// Validate against the frozen caps and semantics. Returns the FIRST
    /// failure in the §13.3 code order, as an `InertReason` — the same shape the
    /// report carries, since an invalid policy evaluates nothing.
    pub fn validate(&self) -> Result<(), InertReason> {
        if self.format != FORMAT {
            return Err(InertReason::UnknownFormat { found: self.format });
        }
        for key in &self.requires {
            if !KNOWN_REQUIRED.contains(&key.as_str()) {
                return Err(InertReason::UnknownRequiredKey { key: key.clone() });
            }
        }
        self.check().map_err(|c| InertReason::ValidationFailed { code: c.to_string() })
    }

    fn check(&self) -> Result<(), &'static str> {
        if self.rules.len() > caps::MAX_RULES_PER_POLICY || self.emoji_codes.len() > caps::MAX_EMOJI_CODES {
            return Err(code::CAP_EXCEEDED);
        }
        if self.window.hours == 0
            || self.window.hours > caps::WINDOW_MAX_HOURS
            || self.window.max_messages == 0
            || self.window.max_messages > caps::WINDOW_MAX_MESSAGES
        {
            return Err(code::WINDOW_OUT_OF_RANGE);
        }
        let mut seen: Vec<&str> = Vec::with_capacity(self.rules.len());
        for r in &self.rules {
            if !r.id.is_ascii() {
                return Err(code::RULE_ID_NOT_ASCII);
            }
            if r.id.is_empty() || r.id.len() > caps::MAX_RULE_ID_LEN {
                return Err(code::CAP_EXCEEDED);
            }
            if seen.contains(&r.id.as_str()) {
                return Err(code::RULE_ID_DUPLICATE);
            }
            seen.push(&r.id);
            self.check_rule(r)?;
        }
        // armed_by resolves within this policy, one level only.
        for r in &self.rules {
            let Some(a) = &r.armed_by else { continue };
            let Some(target) = self.rules.iter().find(|t| t.id == a.rule) else {
                return Err(code::ARMED_BY_UNKNOWN_RULE);
            };
            if target.armed_by.is_some() {
                return Err(code::ARMED_BY_NESTED);
            }
            if a.scope == ArmScope::Subject && a.min_subjects.is_some() {
                return Err(code::ARMED_BY_MIN_SUBJECTS_WITH_SUBJECT_SCOPE);
            }
        }
        Ok(())
    }

    fn check_rule(&self, r: &Rule) -> Result<(), &'static str> {
        let m = &r.matcher;
        if m.patterns().len() > caps::MAX_PATTERNS_PER_RULE
            || m.patterns().iter().any(|p| p.chars().count() > caps::MAX_PATTERN_LEN)
        {
            return Err(code::CAP_EXCEEDED);
        }
        if let Match::Cohort { min, short_factor, thin_ratio, .. } = m {
            if *min == 0 || *short_factor == 0 {
                return Err(code::MISSING_REQUIRED_PARAMETER);
            }
            if thin_ratio.is_some_and(|t| t.denom == 0) {
                return Err(code::MISSING_REQUIRED_PARAMETER);
            }
        }
        if let Match::JoinBurst { gap_secs, min } = m {
            if *gap_secs == 0 || *min == 0 {
                return Err(code::MISSING_REQUIRED_PARAMETER);
            }
        }
        if let Match::Rate { per_secs } = m {
            if *per_secs == 0 {
                return Err(code::MISSING_REQUIRED_PARAMETER);
            }
        }

        // Tiers or direct form, never both; and each must be admissible.
        let has_direct = r.severity.is_some() || r.weight.is_some();
        let has_tiers = r.tiers.as_ref().is_some_and(|t| !t.per_message.is_empty() || !t.per_window.is_empty());
        match (has_tiers, has_direct) {
            (true, true) => return Err(code::TIERS_AND_DIRECT_FORM),
            (false, false) => return Err(code::MISSING_REQUIRED_PARAMETER),
            (false, true) => {
                if !m.admits(Scope::Whole) {
                    return Err(code::DIRECT_FORM_NOT_ADMISSIBLE);
                }
                let w = r.weight.ok_or(code::MISSING_REQUIRED_PARAMETER)?;
                if !(caps::WEIGHT_MIN..=caps::WEIGHT_MAX).contains(&w) {
                    return Err(code::WEIGHT_OUT_OF_RANGE);
                }
                if r.severity.is_none() {
                    return Err(code::MISSING_REQUIRED_PARAMETER);
                }
                if r.pierces_trusted && r.severity != Some(Severity::Severe) {
                    return Err(code::PIERCES_BELOW_SEVERE);
                }
            }
            (true, false) => {
                let t = r.tiers.as_ref().expect("has_tiers");
                for (scope, rungs) in [(Scope::PerMessage, &t.per_message), (Scope::PerWindow, &t.per_window)] {
                    if rungs.is_empty() {
                        continue;
                    }
                    if !m.admits(scope) {
                        return Err(code::SCOPE_NOT_ADMISSIBLE);
                    }
                    let mut prev = 0u32;
                    for rung in rungs {
                        if rung.hits <= prev {
                            return Err(code::RUNG_ORDER_NOT_ASCENDING);
                        }
                        prev = rung.hits;
                        if !(caps::WEIGHT_MIN..=caps::WEIGHT_MAX).contains(&rung.weight) {
                            return Err(code::WEIGHT_OUT_OF_RANGE);
                        }
                        if rung.pierces_trusted && rung.severity != Severity::Severe {
                            return Err(code::PIERCES_BELOW_SEVERE);
                        }
                    }
                }
                // Families exist for one-shot aggravators; allowing a tiered
                // rule into a fold would contradict "both scope convictions
                // enter the combinator".
                if r.family.is_some() {
                    return Err(code::FAMILY_ON_TIERED_RULE);
                }
            }
        }

        // Built-in family tags are immutable: the fold is what stops correlated
        // proxies over-convicting newcomers.
        if let Some(builtin) = m.builtin_family() {
            if r.family.as_deref().is_some_and(|f| f != builtin) {
                return Err(code::FAMILY_REASSIGNED_BUILTIN);
            }
        }

        // `skeleton` deletes separators, so word boundaries and domain
        // exemptions have nothing to anchor on.
        if let Some(n) = m.normalize() {
            if n.strips_separators() {
                if matches!(m, Match::Regex { boundary_word: true, .. }) {
                    return Err(code::BOUNDARY_ON_STRIPPING_NORMALIZER);
                }
                if matches!(m, Match::Keyword { patterns, .. } if patterns.iter().any(|p| !is_double_wildcard(p))) {
                    return Err(code::BOUNDARY_ON_STRIPPING_NORMALIZER);
                }
                let domain_exempt = r
                    .exempt
                    .patterns
                    .iter()
                    .chain(self.exempt.patterns.iter())
                    .any(|p| p.kind == Some(ExemptKind::Domain));
                if domain_exempt {
                    return Err(code::DOMAIN_EXEMPT_ON_STRIPPING_NORMALIZER);
                }
            }
        }

        if r.enforcement == Enforcement::Refuse {
            if m.basis() != Basis::Deterministic {
                return Err(code::REFUSE_ON_HEURISTIC);
            }
            // Breadth is capped STATICALLY: a corpus dry-run would be
            // viewer-local and time-varying, so the same bytes would validate
            // for the author and fail for a mod who joined yesterday.
            match m {
                Match::Keyword { patterns, .. } | Match::Regex { patterns, .. } => {
                    if patterns.iter().any(|p| min_branch_literals(p) < caps::REFUSE_MIN_LITERAL_CHARS) {
                        return Err(code::REFUSE_TOO_BROAD);
                    }
                }
                // Rule types whose rung 1 is satisfiable by an arbitrary message
                // may not refuse unless they narrow it. An allowlist-shaped
                // exemption IS a filter (the Strict Link Blocker's shape).
                Match::Link { patterns } => {
                    let narrowed = !patterns.is_empty()
                        || r.exempt.patterns.iter().chain(self.exempt.patterns.iter()).any(|p| !p.values.is_empty());
                    if !narrowed {
                        return Err(code::REFUSE_ON_UNNARROWED_RULE);
                    }
                }
                Match::Rate { .. } | Match::Repeat { .. } | Match::Mentions {} => {
                    let rung1_needs_more_than_one = r
                        .tiers
                        .as_ref()
                        .and_then(|t| t.per_window.first().or(t.per_message.first()))
                        .is_some_and(|g| g.hits > 1);
                    if !rung1_needs_more_than_one {
                        return Err(code::REFUSE_ON_UNNARROWED_RULE);
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }
}

/// `*word*` — the only keyword form `skeleton` can match, since it deletes the
/// separators a token anchor needs.
fn is_double_wildcard(p: &str) -> bool {
    let unescaped_star = |s: &str, at_start: bool| {
        if at_start {
            s.starts_with('*')
        } else {
            s.ends_with('*') && !s.ends_with("\\*")
        }
    };
    unescaped_star(p, true) && unescaped_star(p, false) && p.chars().count() > 2
}

/// Literal characters required by the THINNEST alternation branch. A
/// whole-pattern count passes `abcd|.*`, which mutes a community.
fn min_branch_literals(pattern: &str) -> usize {
    pattern
        .split('|')
        .map(|branch| {
            let mut n = 0usize;
            let mut chars = branch.chars().peekable();
            while let Some(c) = chars.next() {
                match c {
                    '\\' => {
                        // An escaped literal counts once; a shorthand class does not.
                        if let Some(next) = chars.next() {
                            if !next.is_ascii_alphanumeric() {
                                n += 1;
                            }
                        }
                    }
                    '[' => {
                        for c2 in chars.by_ref() {
                            if c2 == ']' {
                                break;
                            }
                        }
                    }
                    '.' | '*' | '+' | '?' | '(' | ')' | '{' | '}' | '^' | '$' => {}
                    _ => n += 1,
                }
            }
            n
        })
        .min()
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base(rules: Vec<Rule>) -> Policy {
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

    fn keyword_rule() -> Rule {
        Rule {
            id: "swears".into(),
            matcher: Match::Keyword { patterns: vec!["darn".into()], normalize: Normalize::Fold },
            tiers: Some(Tiers {
                per_message: vec![Rung { hits: 1, severity: Severity::Minor, weight: 10, pierces_trusted: false }],
                per_window: vec![Rung { hits: 10, severity: Severity::Severe, weight: 70, pierces_trusted: true }],
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

    fn cohort_rule() -> Rule {
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

    fn err_code(p: &Policy) -> String {
        match p.validate() {
            Err(InertReason::ValidationFailed { code }) => code,
            other => panic!("expected a validation failure, got {other:?}"),
        }
    }

    #[test]
    fn a_well_formed_policy_validates() {
        assert!(base(vec![keyword_rule(), cohort_rule()]).validate().is_ok());
    }

    #[test]
    fn format_and_required_keys_make_a_policy_inert_not_merely_invalid() {
        let mut p = base(vec![keyword_rule()]);
        p.format = 2;
        assert!(matches!(p.validate(), Err(InertReason::UnknownFormat { found: 2 })));

        let mut p = base(vec![keyword_rule()]);
        p.requires = vec!["quarantine".into()];
        assert!(matches!(p.validate(), Err(InertReason::UnknownRequiredKey { key }) if key == "quarantine"));
    }

    #[test]
    fn weights_and_rungs_are_policed() {
        let mut p = base(vec![keyword_rule()]);
        p.rules[0].tiers.as_mut().unwrap().per_message[0].weight = 100;
        assert_eq!(err_code(&p), code::WEIGHT_OUT_OF_RANGE, "100 is reserved");

        let mut p = base(vec![keyword_rule()]);
        p.rules[0].tiers.as_mut().unwrap().per_window =
            vec![Rung { hits: 10, severity: Severity::Severe, weight: 70, pierces_trusted: false },
                 Rung { hits: 3, severity: Severity::Severe, weight: 80, pierces_trusted: false }];
        assert_eq!(err_code(&p), code::RUNG_ORDER_NOT_ASCENDING, "'highest rung reached' needs an order");

        let mut p = base(vec![keyword_rule()]);
        p.rules[0].tiers.as_mut().unwrap().per_message[0].pierces_trusted = true;
        assert_eq!(err_code(&p), code::PIERCES_BELOW_SEVERE, "only Severe may pierce");
    }

    #[test]
    fn scope_and_form_admissibility() {
        // repeat is window-level: per_message would double-count one fact.
        let mut r = keyword_rule();
        r.matcher = Match::Repeat { normalize: Normalize::Fold };
        let p = base(vec![r]);
        assert_eq!(err_code(&p), code::SCOPE_NOT_ADMISSIBLE);

        // A keyword rule cannot become single-rung by declaring weights directly.
        let mut r = keyword_rule();
        r.tiers = None;
        r.severity = Some(Severity::Major);
        r.weight = Some(40);
        let p = base(vec![r]);
        assert_eq!(err_code(&p), code::DIRECT_FORM_NOT_ADMISSIBLE);

        // Both forms at once is ambiguous.
        let mut r = keyword_rule();
        r.severity = Some(Severity::Major);
        let p = base(vec![r]);
        assert_eq!(err_code(&p), code::TIERS_AND_DIRECT_FORM);
    }

    #[test]
    fn families_are_immutable_on_builtins_and_banned_on_ladders() {
        let mut r = keyword_rule();
        r.family = Some("mine".into());
        assert_eq!(err_code(&base(vec![r])), code::FAMILY_ON_TIERED_RULE);

        let tenure = Rule {
            id: "tenure".into(),
            matcher: Match::TenureLt { secs: 86_400 },
            tiers: None,
            severity: Some(Severity::Notice),
            weight: Some(20),
            pierces_trusted: false,
            family: Some("not-fresh-account".into()),
            armed_by: None,
            exempt: Exempt::default(),
            enforcement: Enforcement::Advisory,
        };
        assert_eq!(err_code(&base(vec![tenure])), code::FAMILY_REASSIGNED_BUILTIN);
    }

    #[test]
    fn refuse_is_deterministic_only_and_narrow() {
        // A heuristic rule may never refuse a send.
        let mut r = cohort_rule();
        r.enforcement = Enforcement::Refuse;
        assert_eq!(err_code(&base(vec![r])), code::REFUSE_ON_HEURISTIC);

        // Per-branch literal counting: the whole-pattern count passes `abcd|.*`,
        // which mutes a community.
        let mut r = keyword_rule();
        r.matcher = Match::Regex { patterns: vec!["abcd|.*".into()], normalize: Normalize::Fold, boundary_word: false };
        r.enforcement = Enforcement::Refuse;
        assert_eq!(err_code(&base(vec![r])), code::REFUSE_TOO_BROAD);

        // A bare link rule bans every link; an allowlist narrows it and passes.
        let mut r = keyword_rule();
        r.matcher = Match::Link { patterns: vec![] };
        r.enforcement = Enforcement::Refuse;
        assert_eq!(err_code(&base(vec![r.clone()])), code::REFUSE_ON_UNNARROWED_RULE);
        r.exempt = Exempt {
            patterns: vec![ExemptPatterns { kind: Some(ExemptKind::Domain), values: vec!["vectorapp.io".into()] }],
            ..Default::default()
        };
        assert!(base(vec![r]).validate().is_ok(), "an allowlist IS a filter");
    }

    #[test]
    fn skeleton_refuses_anchors_it_cannot_honour() {
        let mut r = keyword_rule();
        r.matcher = Match::Keyword { patterns: vec!["darn".into()], normalize: Normalize::Skeleton };
        assert_eq!(err_code(&base(vec![r.clone()])), code::BOUNDARY_ON_STRIPPING_NORMALIZER, "bare pattern needs separators");

        r.matcher = Match::Keyword { patterns: vec!["*darn*".into()], normalize: Normalize::Skeleton };
        assert!(base(vec![r.clone()]).validate().is_ok(), "*word* is the only skeleton-safe form");

        r.exempt = Exempt {
            patterns: vec![ExemptPatterns { kind: Some(ExemptKind::Domain), values: vec!["a.io".into()] }],
            ..Default::default()
        };
        assert_eq!(err_code(&base(vec![r])), code::DOMAIN_EXEMPT_ON_STRIPPING_NORMALIZER);
    }

    #[test]
    fn armed_by_resolves_one_level_within_the_policy() {
        let mut burst = Rule {
            id: "burst".into(),
            matcher: Match::JoinBurst { gap_secs: 600, min: 5 },
            tiers: None,
            severity: Some(Severity::Major),
            weight: Some(40),
            pierces_trusted: false,
            family: None,
            armed_by: Some(ArmedBy { rule: "nope".into(), scope: ArmScope::Community, min_subjects: Some(3) }),
            exempt: Exempt::default(),
            enforcement: Enforcement::Advisory,
        };
        assert_eq!(err_code(&base(vec![cohort_rule(), burst.clone()])), code::ARMED_BY_UNKNOWN_RULE);

        burst.armed_by = Some(ArmedBy { rule: "cohort".into(), scope: ArmScope::Subject, min_subjects: Some(3) });
        assert_eq!(
            err_code(&base(vec![cohort_rule(), burst.clone()])),
            code::ARMED_BY_MIN_SUBJECTS_WITH_SUBJECT_SCOPE,
            "a subject-scoped arm has no count to threshold"
        );

        burst.armed_by = Some(ArmedBy { rule: "cohort".into(), scope: ArmScope::Community, min_subjects: Some(3) });
        assert!(base(vec![cohort_rule(), burst]).validate().is_ok());
    }

    #[test]
    fn caps_and_ids_are_enforced() {
        let mut p = base(vec![keyword_rule(), keyword_rule()]);
        assert_eq!(err_code(&p), code::RULE_ID_DUPLICATE);

        p = base(vec![keyword_rule()]);
        p.window.hours = caps::WINDOW_MAX_HOURS + 1;
        assert_eq!(err_code(&p), code::WINDOW_OUT_OF_RANGE);

        p = base(vec![keyword_rule()]);
        p.rules[0].id = "sw€ars".into();
        assert_eq!(err_code(&p), code::RULE_ID_NOT_ASCII, "rule_id enters every id preimage");
    }

    #[test]
    fn min_branch_literals_counts_the_thinnest_branch() {
        assert_eq!(min_branch_literals("scam"), 4);
        assert_eq!(min_branch_literals("abcd|.*"), 0, "the escape hatch branch decides");
        assert_eq!(min_branch_literals("[a-z]+word"), 4, "classes contribute nothing");
        assert_eq!(min_branch_literals(r"\*sale"), 5, "an escaped literal counts");
    }
}
