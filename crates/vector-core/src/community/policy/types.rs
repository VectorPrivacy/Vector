//! Wire-frozen report and policy types (design doc §4, §12, §13).
//!
//! Everything here is byte-compared across clients once distribution ships, so
//! shapes follow the doc exactly: snake_case fields, enums as snake_case
//! strings, payload enums internally tagged with `type`, every `[u8; 32]` as
//! lowercase hex, absent options omitted (never `null`).

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

// ── Constants (§12, draft until the Phase-1 freeze ratifies them) ────────────

/// Wire-frozen: these change convictions on other people's clients.
pub mod caps {
    pub const MAX_POLICIES_PER_COMMUNITY: usize = 16;
    pub const MAX_RULES_PER_POLICY: usize = 32;
    pub const MAX_PATTERNS_PER_RULE: usize = 256;
    pub const MAX_PATTERN_LEN: usize = 260;
    pub const MAX_INLINE_HASHES_PER_LIST: usize = 512;
    pub const WINDOW_MAX_HOURS: u64 = 720;
    pub const WINDOW_MAX_MESSAGES: usize = 4000;
    pub const COMBINATOR_MAX_CONVICTIONS: usize = 12;
    pub const MAX_DETAIL_LEN: usize = 256;
    pub const MAX_SAMPLE_LEN: usize = 256;
    pub const MAX_CITATIONS_PER_CONVICTION: usize = 32;
    pub const MAX_CONVICTIONS_STORED_PER_SUBJECT: usize = 16;
    pub const MAX_SUBJECTS_PER_REPORT: usize = 512;
    pub const MAX_EVIDENCE_PER_CONVICTION: usize = 8;
    pub const MAX_EMOJI_CODES: usize = 256;
    pub const MAX_RULE_ID_LEN: usize = 64;
    pub const MAX_POLICY_BYTES: usize = 64 * 1024;
    pub const MIN_SKELETON_LEN: usize = 8; // Unicode scalar values, never bytes
    /// Peers named per cohort exhibit. A 500-strong cluster would otherwise put
    /// tens of KB of ids through the IPC boundary for nothing.
    pub const COHORT_SAMPLE_CAP: usize = 24;
    pub const WEIGHT_MIN: u32 = 1;
    pub const WEIGHT_MAX: u32 = 99; // 100 reserved: no unconditional-sentencing number exists
    pub const REFUSE_MIN_LITERAL_CHARS: usize = 4; // per alternation branch, minimum
    pub const ENGINE_VERSION: u32 = 1;
    pub const BUNDLE_VERSION: u32 = 1;
}

// ── Fieldless enums (§13.1: snake_case strings on the wire) ─────────────────

/// The author's opinion of the crime — gravity, never proof.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Notice,
    Minor,
    Major,
    Severe,
}

/// The provenance of a detection: byte-provable and replayable, or inference.
/// Only Deterministic weight ever reaches `proven`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Basis {
    Deterministic,
    Heuristic,
}

/// Fixed anchor ranges over confidence — strictly DERIVED from `conf_pm`,
/// never independently computed (§3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Band {
    Clear,
    Noted,
    Watch,
    Flagged,
    Alert,
}

/// Which ladder a conviction fired on. Total order (preimage tags 0/1/2): a
/// rule may convict on both content scopes, so `(rule_id, scope)` is the
/// tie-break identity everywhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    PerMessage,
    PerWindow,
    Whole,
}

impl Scope {
    pub fn tag(self) -> u8 {
        match self {
            Scope::PerMessage => 0,
            Scope::PerWindow => 1,
            Scope::Whole => 2,
        }
    }
}

/// Whether a conviction's evidence predates the policy. Only `Yes` gates
/// irreversible action; `Unknown` is the honest default until signed
/// activation exists (Phase 4) and is NON-gating (§3.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Retroactive {
    No,
    Yes,
    Unknown,
}

/// Which shield applied. ONLY `Protected` and `Trusted` gate:
///  * `Protected` — no convictions, no citations, confidence 0, nothing pierces.
///  * `Trusted` — gated EXCEPT against rungs declaring `pierces_trusted`, so a
///    Trusted subject CAN carry convictions and a non-zero confidence.
///  * `Indeterminate` — informational ONLY (tenure unknowable): the subject is
///    judged exactly as if unshielded; a consumer can tell "not trusted" from
///    "we could not tell".
/// Precedence when several apply: Protected > Trusted > Indeterminate > None.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Shield {
    None,
    Trusted,
    Protected,
    Indeterminate,
}

/// Rule-level evaluation states — for conditions that affect EVERY subject.
/// Per-item failures land in `RuleStatus::unknown_subjects` instead, or one
/// busy evaluation would block exoneration community-wide.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleState {
    Evaluated,
    UnknownType,
    NoClassifier,
    Errored,
    /// This evaluator does not run rules that need historical state. A member's
    /// client says so rather than silently skipping, so the report can never be
    /// read as "nothing was found" when the truth is "nobody looked".
    RequiresHistory,
}

/// One profile field (preimage: one byte, `name=0 about=1 avatar=2 banner=3`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileField {
    Name,
    About,
    Avatar,
    Banner,
}

impl ProfileField {
    pub fn tag(self) -> u8 {
        match self {
            ProfileField::Name => 0,
            ProfileField::About => 1,
            ProfileField::Avatar => 2,
            ProfileField::Banner => 3,
        }
    }
}

// ── Identifiers (lowercase hex on the wire, raw 32 bytes in preimages) ──────

macro_rules! hex32_newtype {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(pub [u8; 32]);

        impl Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str(&crate::simd::hex::bytes_to_hex_32(&self.0))
            }
        }
        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let s = String::deserialize(d)?;
                let arr = crate::simd::hex::hex_to_bytes_32_checked(&s)
                    .ok_or_else(|| serde::de::Error::custom("expected 64 hex chars"))?;
                Ok($name(arr))
            }
        }
    };
}

hex32_newtype!(
    /// A member — the x-only pubkey. Members are the only subjects; content is
    /// cited, never scored.
    SubjectId
);
hex32_newtype!(
    /// The INNER message id (a chunk-set wire event holds many messages, so a
    /// wire id does not name one).
    MessageId
);
hex32_newtype!(
    /// sha256 of a content blob (attachment, avatar).
    ContentHash
);
hex32_newtype!(
    /// Deterministic conviction identity: H(policy_hash ‖ rule_id ‖ scope-tag ‖
    /// rung ‖ subject), every field length-prefixed. `hits` is deliberately
    /// absent (an id that changed per hit would re-sentence streaming bots);
    /// `rung` is present, so escalation mints a new id — a pardon forgives what
    /// was done, not what comes next.
    ConvictionId
);
hex32_newtype!(
    /// Deterministic citation identity: H(policy_hash ‖ rule_id ‖ scope-tag ‖
    /// subject ‖ target-tag ‖ target-parts ‖ span). The SUBJECT is in the
    /// preimage unconditionally — without it two members matching one rule at
    /// the same offsets mint the same id, and one member's pardon suppresses
    /// another's citation.
    CitationId
);
hex32_newtype!(
    /// SHA-256 of exact received bytes (policies) or a length-prefixed
    /// canonical serialization (roster, overrides).
    Hash32
);

/// One length-prefixed preimage field: `u32-BE length ‖ bytes`. Single-byte
/// tags are `length 1`; integer bodies are `u32-BE` (`length 4`); an absent
/// span contributes `length 0` twice. No exceptions — two readings hash apart.
fn frame(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    out.extend_from_slice(bytes);
}

fn sha256(preimage: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(preimage);
    h.finalize().into()
}

pub fn conviction_id(policy_hash: &Hash32, rule_id: &str, scope: Scope, rung: u8, subject: &SubjectId) -> ConvictionId {
    let mut p = Vec::with_capacity(96);
    frame(&mut p, &policy_hash.0);
    frame(&mut p, rule_id.as_bytes());
    frame(&mut p, &[scope.tag()]);
    frame(&mut p, &[rung]);
    frame(&mut p, &subject.0);
    ConvictionId(sha256(&p))
}

pub fn citation_id(
    policy_hash: &Hash32,
    rule_id: &str,
    scope: Scope,
    subject: &SubjectId,
    target: &CitationTarget,
    span: Option<Span>,
) -> CitationId {
    let mut p = Vec::with_capacity(160);
    frame(&mut p, &policy_hash.0);
    frame(&mut p, rule_id.as_bytes());
    frame(&mut p, &[scope.tag()]);
    frame(&mut p, &subject.0);
    match target {
        CitationTarget::Message { message_id } => {
            frame(&mut p, &[0u8]);
            frame(&mut p, &message_id.0);
        }
        // Composite targets contribute one field PER PART, and never repeat the
        // subject (it is already its own field above).
        CitationTarget::ProfileField { field, .. } => {
            frame(&mut p, &[1u8]);
            frame(&mut p, &[field.tag()]);
        }
        CitationTarget::Attachment { message_id, content_hash, .. } => {
            frame(&mut p, &[2u8]);
            frame(&mut p, &message_id.0);
            frame(&mut p, &content_hash.0);
        }
    }
    match span {
        Some(sp) => {
            frame(&mut p, &sp.start.to_be_bytes());
            frame(&mut p, &sp.end.to_be_bytes());
        }
        None => {
            frame(&mut p, &[]);
            frame(&mut p, &[]);
        }
    }
    CitationId(sha256(&p))
}

// ── Citations and convictions ────────────────────────────────────────────────

/// HALF-OPEN `[start, end)`, byte offsets into the text produced by the citing
/// rule's normalizer. `end` is exclusive: it is a preimage field, so the other
/// reading shifts every text CitationId by one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

/// What a citation points at. EVERY variant resolves to its subject; wire key
/// names are enumerated here (tuple positions don't survive serde).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CitationTarget {
    Message { message_id: MessageId },
    ProfileField { subject: SubjectId, field: ProfileField },
    Attachment { subject: SubjectId, message_id: MessageId, content_hash: ContentHash },
}

/// "This content matched this rule." Citations drive content-level sentences
/// (hide, delete), carry stable ids, and NEVER enter a combinator — content has
/// no confidence, it is cited or not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Citation {
    pub id: CitationId,
    pub target: CitationTarget,
    /// Content timestamp (ms); drives retroactivity. For a ProfileField target
    /// this is the snapshot's fetched_at; for an Attachment, the containing
    /// message's inner timestamp.
    pub at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub span: Option<Span>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// An override pardoned the conviction that cited this; reported, never
    /// acted on.
    pub suppressed: bool,
}

/// Rule-shaped exhibits, all length-capped. Media evidence is hash + label,
/// NEVER pixel bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Evidence {
    Cohort { skeleton_hash: Hash32, sample: String, size: u32, peers: Vec<SubjectId> },
    Burst { from: u64, to: u64, size: u32 },
    Rate { window_secs: u64, count: u32, from: u64 },
    Snapshot { field: ProfileField, value: String, fetched_at: u64 },
    Label { classifier: String, label: String, score: u8, content: ContentHash },
    Adoption { hash: ContentHash, first_seen: u64, witnessed_from: u64 },
}

impl Evidence {
    /// Preimage/order tag (§13.2).
    pub fn tag(&self) -> u8 {
        match self {
            Evidence::Cohort { .. } => 0,
            Evidence::Burst { .. } => 1,
            Evidence::Rate { .. } => 2,
            Evidence::Snapshot { .. } => 3,
            Evidence::Label { .. } => 4,
            Evidence::Adoption { .. } => 5,
        }
    }
}

/// One member conviction: enters the combinator, owns citations as exhibits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Conviction {
    pub id: ConvictionId,
    pub subject: SubjectId,
    pub rule_id: String,
    pub scope: Scope,
    /// Index of the rung that fired; 0 for `Whole`.
    pub rung: u8,
    /// Observed count; ALWAYS 1 for Whole scope. NOT part of any id or sort key.
    pub hits: u32,
    pub severity: Severity,
    pub basis: Basis,
    /// The DECLARED rung weight, never a marginal effect.
    pub tier_weight: u32,
    pub retroactive: Retroactive,
    /// An override matched: reported, excluded from combination.
    pub suppressed: bool,
    /// Lost its family fold IN THE CONFIDENCE PIPELINE (the proven pipeline's
    /// folds are recomputable from basis + family + tier_weight).
    pub folded: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folded_into: Option<ConvictionId>,
    /// Entered the CONFIDENCE combinator.
    pub combined: bool,
    /// Entered the PROVEN combinator — a separate pipeline with its own folds
    /// and top-N, so one flag cannot answer for both.
    pub proven_combined: bool,
    pub citations: Vec<CitationId>,
    /// True count; `citations` may be truncated. Computed over the FULL
    /// pre-truncation set, like the two timestamps below, so a truncated
    /// exhibit list never reads as fewer offenses nor flips `retroactive`.
    pub citation_count: u32,
    /// Both 0 for a citation-less conviction (tenure_lt, messages_lte,
    /// join_burst) — which is also why those always report `Unknown`.
    pub earliest_citation_at: u64,
    pub latest_citation_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family: Option<String>,
    pub evidence: Vec<Evidence>,
}

// ── Reports ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubjectReport {
    pub subject: SubjectId,
    pub shield: Shield,
    /// 0-99. Weights validate to 1..=99, so 100 is unreachable by construction.
    pub confidence: u32,
    /// The same pipeline run independently over the Deterministic-only subset.
    pub proven: u32,
    pub band: Band,
    pub convictions: Vec<Conviction>,
}

/// Why a policy evaluated NOTHING. Carries no free text — a closed code list —
/// or every invalid policy would produce different bytes per implementation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InertReason {
    UnknownFormat { found: u32 },
    UnknownRequiredKey { key: String },
    ValidationFailed { code: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleStatus {
    pub rule_id: String,
    pub state: RuleState,
    /// Per-subject InputUnknown exceptions (lost Join, undownloaded attachment,
    /// exhausted classify budget). Consumers must not exonerate these subjects
    /// for this rule; everyone else's evaluation stands.
    pub unknown_subjects: Vec<SubjectId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyReport {
    /// SHA-256 over the exact policy bytes as received — never a
    /// re-serialization.
    pub policy_hash: Hash32,
    /// An INERT policy evaluated NOTHING: rule_status, subjects and citations
    /// all empty, coverage_complete false. Consumers must never read its empty
    /// subject list as "everyone is clean".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inert: Option<InertReason>,
    /// Signed activation only (Phase 4); None until then, which is why
    /// `Retroactive::Unknown` is the pre-distribution constant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activated_at: Option<u64>,
    /// THIS policy's declared window vs the confirmed range (the report-level
    /// coverage is community-wide; 16 policies may declare 16 windows).
    pub coverage_complete: bool,
    /// Exactly ONE entry per rule in the policy, always — including
    /// `Evaluated`. "Emit only the interesting ones" is a divergence.
    pub rule_status: Vec<RuleStatus>,
    pub subjects: Vec<SubjectReport>,
    pub subjects_truncated: u32,
    /// A SET: one entry per distinct CitationId, exactly those referenced by
    /// retained convictions.
    pub citations: Vec<Citation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelCoverage {
    /// One entry per channel the client can decrypt, whether or not it
    /// contributed — a channel that went silent is exactly what a mod needs to
    /// see. Counts come from the CLAMPED corpus, never raw event counts.
    pub channel: Hash32,
    pub messages: u32,
    pub from: u64,
    pub to: u64,
}

/// An INPUT to `evaluate`, supplied by the caller — the engine has no network.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelayCoverage {
    pub url: String,
    pub eose: bool,
    pub events: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct WindowCoverage {
    /// The SUBSCRIBED range.
    pub requested_from: u64,
    pub requested_to: u64,
    /// EOSE-confirmed range: coverage gating reads THIS, not observed data — a
    /// quiet community must not be un-moderatable.
    pub confirmed_from: u64,
    pub confirmed_to: u64,
    /// Min/max inner timestamp over the UNION of every evaluated policy's
    /// clamped corpus (policies declare different windows).
    pub observed_from: u64,
    pub observed_to: u64,
    pub channels: Vec<ChannelCoverage>,
    pub relays: Vec<RelayCoverage>,
    pub complete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModerationReport {
    pub engine_version: u32,
    /// The frozen normalizer bundle evaluated under.
    pub bundle_version: u32,
    /// The shield roster evaluated against (owner + sorted role grants).
    pub roster_version: Hash32,
    /// The override set applied — two mods detect a pardon-list difference
    /// instead of arguing about it.
    pub override_hash: Hash32,
    pub evaluated_at: u64,
    pub window: WindowCoverage,
    /// Every law scored independently; consumers may merge, the engine never
    /// does.
    pub policies: Vec<PolicyReport>,
}

// ── Overrides (pardons): consumer state, fed IN ─────────────────────────────

/// `{ target, issuer, issued_at, expires_at }`. Suppression removes the
/// conviction (and its citations) from combination and action — never from the
/// report, and never from any corpus statistic, so one mod's pardon list can
/// never change a third party's verdict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Override {
    pub target: OverrideTarget,
    pub issuer: SubjectId,
    pub issued_at: u64,
    /// Mandatory and finite.
    pub expires_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OverrideTarget {
    /// Pardons one conviction id (lapses on rung escalation — deliberate).
    Conviction { id: ConvictionId },
    /// Coarse form; carries `scope` so a pardon does not silently forgive both
    /// the density and the persistence conviction when only one was reviewed.
    Rule { policy_hash: Hash32, rule_id: String, scope: Scope, subject: SubjectId },
}

impl Override {
    pub fn matches(&self, c: &Conviction, policy_hash: &Hash32, now_ms: u64) -> bool {
        if now_ms > self.expires_at {
            return false;
        }
        match &self.target {
            OverrideTarget::Conviction { id } => *id == c.id,
            OverrideTarget::Rule { policy_hash: ph, rule_id, scope, subject } => {
                ph == policy_hash && *rule_id == c.rule_id && *scope == c.scope && *subject == c.subject
            }
        }
    }
}

/// H over the framed, sorted override set (§13.2): framed count, then each
/// override sorted by its FRAMED serialized target bytes (form-tag included).
pub fn override_hash(overrides: &[Override]) -> Hash32 {
    let mut items: Vec<(Vec<u8>, &Override)> = overrides
        .iter()
        .map(|o| {
            let mut t = Vec::new();
            match &o.target {
                OverrideTarget::Conviction { id } => {
                    frame(&mut t, &[0u8]);
                    frame(&mut t, &id.0);
                }
                OverrideTarget::Rule { policy_hash, rule_id, scope, subject } => {
                    frame(&mut t, &[1u8]);
                    frame(&mut t, &policy_hash.0);
                    frame(&mut t, rule_id.as_bytes());
                    frame(&mut t, &[scope.tag()]);
                    frame(&mut t, &subject.0);
                }
            }
            (t, o)
        })
        .collect();
    items.sort_by(|a, b| a.0.cmp(&b.0));
    let mut p = Vec::new();
    frame(&mut p, &(items.len() as u32).to_be_bytes());
    for (t, o) in items {
        p.extend_from_slice(&t);
        frame(&mut p, &o.issuer.0);
        frame(&mut p, &(o.issued_at).to_be_bytes());
        frame(&mut p, &(o.expires_at).to_be_bytes());
    }
    Hash32(sha256(&p))
}

/// H over the shield roster: the OWNER (a shield input no other stamp covers),
/// then framed count, then per role sorted by eid — eid, permissions (u64-BE),
/// framed member count, members sorted ascending.
pub fn roster_version(owner: &SubjectId, roles: &[(Hash32, u64, BTreeSet<SubjectId>)]) -> Hash32 {
    let mut sorted: Vec<_> = roles.iter().collect();
    sorted.sort_by(|a, b| a.0 .0.cmp(&b.0 .0));
    let mut p = Vec::new();
    frame(&mut p, &owner.0);
    frame(&mut p, &(sorted.len() as u32).to_be_bytes());
    for (eid, perms, members) in sorted {
        frame(&mut p, &eid.0);
        frame(&mut p, &perms.to_be_bytes());
        frame(&mut p, &(members.len() as u32).to_be_bytes());
        for m in members {
            frame(&mut p, &m.0);
        }
    }
    Hash32(sha256(&p))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_stable_and_subject_scoped() {
        let ph = Hash32([0x11; 32]);
        let a = SubjectId([0x22; 32]);
        let b = SubjectId([0x23; 32]);
        let cid = conviction_id(&ph, "swears", Scope::PerMessage, 1, &a);
        assert_eq!(cid, conviction_id(&ph, "swears", Scope::PerMessage, 1, &a), "deterministic");
        assert_ne!(cid, conviction_id(&ph, "swears", Scope::PerMessage, 1, &b), "subject in preimage");
        assert_ne!(cid, conviction_id(&ph, "swears", Scope::PerWindow, 1, &a), "scope in preimage");
        assert_ne!(cid, conviction_id(&ph, "swears", Scope::PerMessage, 2, &a), "rung in preimage");

        // Two members matching one rule at the same offsets mint DIFFERENT
        // citation ids — the collision that once let one pardon suppress
        // another member's citation.
        let m = MessageId([0xb1; 32]);
        let t = CitationTarget::Message { message_id: m };
        let sp = Some(Span { start: 5, end: 11 });
        let ca = citation_id(&ph, "swears", Scope::PerMessage, &a, &t, sp);
        let cb = citation_id(&ph, "swears", Scope::PerMessage, &b, &t, sp);
        assert_ne!(ca, cb);
        // Span half-openness is a preimage fact: shifting end by one moves it.
        assert_ne!(ca, citation_id(&ph, "swears", Scope::PerMessage, &a, &t, Some(Span { start: 5, end: 12 })));
        // Absent span (whole-content rules) is its own identity.
        assert_ne!(ca, citation_id(&ph, "swears", Scope::PerMessage, &a, &t, None));
    }

    #[test]
    fn enums_serialize_snake_case_and_targets_internally_tagged() {
        assert_eq!(serde_json::to_string(&Severity::Severe).unwrap(), "\"severe\"");
        assert_eq!(serde_json::to_string(&Scope::PerWindow).unwrap(), "\"per_window\"");
        assert_eq!(serde_json::to_string(&RuleState::NoClassifier).unwrap(), "\"no_classifier\"");
        let t = CitationTarget::Message { message_id: MessageId([0xab; 32]) };
        let j = serde_json::to_string(&t).unwrap();
        assert!(j.contains("\"type\":\"message\"") && j.contains("\"message_id\":\"abab"), "{j}");
    }

    #[test]
    fn overrides_hash_ignores_order_and_expiry_is_enforced() {
        let a = Override {
            target: OverrideTarget::Conviction { id: ConvictionId([1; 32]) },
            issuer: SubjectId([9; 32]),
            issued_at: 1,
            expires_at: 10,
        };
        let b = Override {
            target: OverrideTarget::Rule {
                policy_hash: Hash32([2; 32]),
                rule_id: "x".into(),
                scope: Scope::Whole,
                subject: SubjectId([3; 32]),
            },
            issuer: SubjectId([9; 32]),
            issued_at: 1,
            expires_at: 10,
        };
        assert_eq!(override_hash(&[a.clone(), b.clone()]), override_hash(&[b, a.clone()]));

        let c = Conviction {
            id: ConvictionId([1; 32]),
            subject: SubjectId([3; 32]),
            rule_id: "x".into(),
            scope: Scope::Whole,
            rung: 0,
            hits: 1,
            severity: Severity::Minor,
            basis: Basis::Deterministic,
            tier_weight: 10,
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
            family: None,
            evidence: vec![],
        };
        assert!(a.matches(&c, &Hash32([0; 32]), 5), "within expiry");
        assert!(!a.matches(&c, &Hash32([0; 32]), 11), "expired pardons stop pardoning");
    }
}
