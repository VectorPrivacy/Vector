//! Real-npub authority editions — the keyless model's authorship + version carrier.
//!
//! An authority change (a Grant, RoleMetadata, RoleOrder, Banlist, ...) is an **inner event signed
//! by the ACTOR's own npub**, carrying the entity id, a per-entity
//! `version`, and the previous edition's hash (`prev_hash`, see [`super::version`]). That inner event
//! lives inside the channel/server-root encryption (the outer wrapper is the usual ephemeral signer,
//! [`super::envelope`]), so authorship is **member-verifiable**: the inner Schnorr signature *is* the
//! proof of who acted, and members check that npub against the roster.
//!
//! This module is the wire encoding of one edition (build + verify + parse). It does NOT decide
//! authorization — the signature proves WHO acted; the roster (§roles) decides WHETHER they were
//! allowed, and [`super::version::fold`] decides which edition is current.

use crate::event_ext::FinalizeUnsignedWithId;
use super::version;
use crate::stored_event::event_kind;
use nostr_sdk::prelude::*;

const TAG_SUBKIND: &str = "vsk";
const TAG_ENTITY: &str = "eid";
const TAG_EVERSION: &str = "ev";
const TAG_EPREV: &str = "ep";
const TAG_VERSION: &str = "v";
const PROTOCOL_VERSION: &str = "1";
/// Authority citation tag: `["vac", <authorizing-entity hex>, <version>, <edition-hash hex>]`.
/// The "pinned proof" — the grant edition the actor claims their authority under. In the MVP it is a
/// COMPLETENESS floor: a verifier confirms it has synced that exact grant to ≥ the cited version (an
/// un-forked, complete view) before acting, and resolves the actor's actual rank against its current
/// (refuse-downgrade-protected) roster — so a since-demoted actor is dropped there. The full 
/// "resolve rank AT the cited version" (block-until-synced re-fetch + a roster-wide snapshot version) is
/// the deferred refinement; today it pins the actor's own grant, not a whole-roster moment. Absent when
/// the OWNER acts (supreme, no grant to cite).
pub const TAG_AUTHORITY_CITATION: &str = "vac";

/// CORD-01 §5: a tag number rides as "its decimal form with no leading zeros".
/// So `"4"` and `"0"` are the shape; `"04"`, `"+4"` and `""` are not.
/// `u64::from_str` accepts a leading `+` and any number of leading zeros, and a
/// peer that doesn't would drop an event we honored — a divergence neither side
/// can see, because a declined parse is never logged.
pub fn is_tag_decimal(raw: &str) -> bool {
    !raw.is_empty()
        && raw.bytes().all(|b| b.is_ascii_digit())
        && !(raw.len() > 1 && raw.starts_with('0'))
}

/// The pinned authority an actor claims for an action (mechanism a). Points at the actor's own
/// authorizing edition (their Grant — or a RoleMetadata for a role-position claim) by stable
/// coordinate + the exact version/hash, so the verifier resolves authority against that frozen point,
/// not its own possibly-lagging-or-ahead live roster.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AuthorityCitation {
    /// The authorizing edition's entity id (e.g. `grant_locator(community_id, actor)`).
    pub entity_id: [u8; 32],
    /// The version of that edition the actor claims authority under.
    pub version: u64,
    /// That edition's [`version::edition_hash`] — pins the exact content, not just the version number.
    pub edition_hash: [u8; 32],
}

impl AuthorityCitation {
    /// The signed `vac` tag carrying this citation.
    pub fn to_tag(&self) -> Tag {
        Tag::custom(
            TAG_AUTHORITY_CITATION,
            [
                crate::simd::hex::bytes_to_hex_32(&self.entity_id),
                self.version.to_string(),
                crate::simd::hex::bytes_to_hex_32(&self.edition_hash),
            ],
        )
    }

    /// Extract the citation from an event's tags, or `None` if absent. A malformed `vac` (bad hex /
    /// unparseable version) returns `None` — the verifier then treats the action as uncited (owner-only
    /// or rejected), never trusting a corrupt citation.
    pub fn from_tags(tags: &Tags) -> Option<AuthorityCitation> {
        let s = tags.iter().find_map(|t| {
            let s = t.as_slice();
            (s.len() >= 4 && s[0] == TAG_AUTHORITY_CITATION).then(|| (s[1].clone(), s[2].clone(), s[3].clone()))
        })?;
        let valid_hex = |h: &str| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit());
        if !valid_hex(&s.0) || !valid_hex(&s.2) {
            return None;
        }
        // Digit-only BEFORE the parse: CORD-01 §5 says a tag number rides as its
        // decimal form, and `u64::from_str` accepts a leading `+` ("+5") that a
        // digit-checking peer rejects — so the two would disagree on whether a
        // citation exists at all, one honoring the action and the other parking
        // it. Same guard `resolve_ms_strict` already applies to `ms`.
        if !is_tag_decimal(&s.1) {
            return None;
        }
        Some(AuthorityCitation {
            entity_id: crate::simd::hex::hex_to_bytes_32(&s.0),
            version: s.1.parse().ok()?,
            edition_hash: crate::simd::hex::hex_to_bytes_32(&s.2),
        })
    }
}

/// Build the unsigned inner edition event. Sign it with the ACTOR's real identity keys — that
/// signature is the authorship proof. `entity_id` is the entity's 32-byte id, `prev_hash` is the
/// previous edition's [`version::edition_hash`] (`None` for the first edition), `content` is the
/// entity payload JSON, and `created_at_secs` is the authored time (the version-fold tiebreak).
#[allow(clippy::too_many_arguments)]
pub fn build_edition_inner(
    author: PublicKey,
    vsk: &str,
    entity_id: &[u8; 32],
    version: u64,
    prev_hash: Option<&[u8; 32]>,
    content: &str,
    created_at_secs: u64,
    authority: Option<&AuthorityCitation>,
) -> UnsignedEvent {
    let mut tags = vec![
        Tag::custom(TAG_SUBKIND, [vsk.to_string()]),
        Tag::custom(TAG_ENTITY, [crate::simd::hex::bytes_to_hex_32(entity_id)]),
        Tag::custom(TAG_EVERSION, [version.to_string()]),
        Tag::custom(TAG_VERSION, [PROTOCOL_VERSION.to_string()]),
    ];
    if let Some(p) = prev_hash {
        tags.push(Tag::custom(TAG_EPREV, [crate::simd::hex::bytes_to_hex_32(p)]));
    }
    // The pinned authority proof: absent when the OWNER signs (supreme), present for a delegated
    // admin so verifiers resolve their rank at the cited grant version. Outside the version-chain
    // self_hash (it's per-action metadata, not chain identity), but covered by the inner signature.
    if let Some(a) = authority {
        tags.push(a.to_tag());
    }
    EventBuilder::new(Kind::Custom(event_kind::COMMUNITY_CONTROL), content)
        .tags(tags)
        .custom_created_at(Timestamp::from_secs(created_at_secs))
        .finalize_unsigned_with_id(author)
}

/// A signature-verified, parsed edition.
#[derive(Clone, Debug)]
pub struct ParsedEdition {
    /// The real npub that signed (and is thus accountable for) this edition.
    pub author: PublicKey,
    pub vsk: String,
    pub entity_id: [u8; 32],
    pub version: u64,
    pub prev_hash: Option<[u8; 32]>,
    pub content: String,
    /// [`version::edition_hash`] of this edition — what the next edition's `prev_hash` must cite.
    pub self_hash: [u8; 32],
    pub created_at: u64,
    pub inner_id: [u8; 32],
    /// The pinned authority proof, if the actor cited one. `None` when the OWNER signs (supreme)
    /// or a non-authority edition carries no citation. Verified separately against the roster (#3c).
    pub authority: Option<AuthorityCitation>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum EditionError {
    BadSignature,
    MissingField(&'static str),
    BadField(&'static str),
}

fn decode_hash(hex: &str, field: &'static str) -> Result<[u8; 32], EditionError> {
    if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(EditionError::BadField(field));
    }
    Ok(crate::simd::hex::hex_to_bytes_32(hex))
}

/// Verify + parse an inner edition event. Checks the inner Schnorr signature (the real-npub
/// authorship proof) and extracts the edition fields, computing `self_hash` over the canonical
/// edition bytes. Does NOT check roster authorization — that is the caller's separate step.
pub fn parse_edition_inner(inner: &Event) -> Result<ParsedEdition, EditionError> {
    inner.verify().map_err(|_| EditionError::BadSignature)?;
    // Reject duplicate authority tags: the signature covers all of them, but if two clients picked a
    // different duplicate they would compute a different `self_hash` for the same signed event and
    // diverge on the chain. The map signed-event → canonical bytes must be total and unambiguous.
    for name in [TAG_SUBKIND, TAG_ENTITY, TAG_EVERSION, TAG_EPREV, TAG_AUTHORITY_CITATION] {
        let count = inner
            .tags
            .iter()
            .filter(|t| t.as_slice().first().map(|s| s.as_str() == name).unwrap_or(false))
            .count();
        if count > 1 {
            return Err(EditionError::BadField("duplicate authority tag"));
        }
    }
    let get = |name: &str| -> Option<String> {
        inner.tags.iter().find_map(|t| {
            let s = t.as_slice();
            (s.len() >= 2 && s[0] == name).then(|| s[1].clone())
        })
    };
    let vsk = get(TAG_SUBKIND).ok_or(EditionError::MissingField("vsk"))?;
    let entity_id = decode_hash(&get(TAG_ENTITY).ok_or(EditionError::MissingField("eid"))?, "eid")?;
    // Digit-only: `u64::from_str` accepts a leading `+`, so "+5"/"5" would fold to
    // one version as distinct signed inners (a convergence fork). Shared v1/v2 grammar.
    let ev_raw = get(TAG_EVERSION).ok_or(EditionError::MissingField("ev"))?;
    if !is_tag_decimal(&ev_raw) {
        return Err(EditionError::BadField("ev"));
    }
    let version: u64 = ev_raw.parse().map_err(|_| EditionError::BadField("ev"))?;
    let prev_hash = match get(TAG_EPREV) {
        Some(h) => Some(decode_hash(&h, "ep")?),
        None => None,
    };
    let content = inner.content.clone();
    let self_hash = version::edition_hash(&entity_id, version, prev_hash.as_ref(), content.as_bytes());
    Ok(ParsedEdition {
        author: inner.pubkey,
        vsk,
        entity_id,
        version,
        prev_hash,
        content,
        self_hash,
        created_at: inner.created_at.as_secs(),
        inner_id: inner.id.to_bytes(),
        authority: AuthorityCitation::from_tags(&inner.tags),
    })
}

impl ParsedEdition {
    /// The [`version::Edition`] view used by [`version::fold`].
    pub fn to_fold_edition(&self) -> version::Edition {
        version::Edition {
            version: self.version,
            prev_hash: self.prev_hash,
            self_hash: self.self_hash,
            created_at: self.created_at,
            tiebreak_id: self.inner_id,
        }
    }
}

/// The folded current head of one control entity — what the caller persists via `set_edition_head`
/// (the monotonic refuse-downgrade floor) and the send side reads to emit the next version.
#[derive(Clone)]
pub struct EntityHead {
    /// The entity coordinate (role_id for roles, grant_locator for grants), lowercase hex.
    pub entity_hex: String,
    pub version: u64,
    pub self_hash: [u8; 32],
    /// The head edition's deterministic tiebreak key (the inner edition id). Used to resolve a
    /// same-version concurrent fork: every client converges on the lower `inner_id` among authorized
    /// editions, and the persisted head ranks by it so a same-version adopt only moves toward the min.
    pub inner_id: [u8; 32],
    /// The authority citation the head edition carried, if any — the actor's pinned proof of
    /// the authority they claimed for this edit. `None` for an owner-signed edition (supreme, cites
    /// nothing) or an uncited one. Verifiers resolve the actor's standing at the cited grant version
    /// via [`authority_citation_satisfied`].
    pub citation: Option<AuthorityCitation>,
}

/// version-pinned authority **completeness** check for an action that carries an
/// [`AuthorityCitation`] (a ban, a hide, a delegated grant). The action names the authorizing
/// grant edition it claims authority under; this confirms we have folded a COMPLETE, un-forked view of
/// that grant — synced to AT LEAST the cited version, with the cited hash matching ours at equality.
///
/// It does NOT decide the permission/outrank — that stays with the caller's `can_act_on_member` against
/// the CURRENT authorized roster, so a since-demoted actor is refused there (refuse-superseded falls out:
/// we hold a later head of their grant that dropped the role). This check is the orthogonal half: it
/// guarantees the verdict is computed over the same authority view the actor cited, not a stale or
/// partial one.
///
/// `heads` is the fold's per-entity head set — the haystack the cited grant
/// must appear in. `actor_grant_hex` is the entity coordinate of the ACTOR's OWN authorizing grant
/// (`grant_locator(community_id, actor)`, lowercase hex): the citation MUST name it, so an actor can't
/// borrow completeness by citing some other synced edition. Returns true iff:
///   - the actor is the proven owner (supreme — cites nothing), OR
///   - the citation names the actor's own grant AND we surfaced it at version ≥ the cited version, and —
///     when our head is EXACTLY the cited version — the cited hash equals ours (a cited fork is rejected).
///
/// A non-owner who cited nothing, cited a foreign entity, or whose cited grant we have NOT folded up to
/// the cited version (a withholding relay, or we are simply behind), returns false: FAIL CLOSED — never
/// act on an incomplete authority view (the §"never act on a partial view" tenet). The block-until-synced
/// re-fetch escalation across a relay quorum is deferred (one signature suffices, per the MVP directive);
/// MVP fails closed here and self-heals on the next sync once the cited grant arrives.
pub fn authority_citation_satisfied(
    heads: &[EntityHead],
    owner_hex: Option<&str>,
    actor_hex: &str,
    actor_grant_hex: &str,
    citation: Option<&AuthorityCitation>,
) -> bool {
    if owner_hex == Some(actor_hex) {
        return true;
    }
    let Some(c) = citation else { return false };
    let entity_hex = crate::simd::hex::bytes_to_hex_32(&c.entity_id);
    // The citation must name the ACTOR's OWN authorizing grant — citing a foreign synced edition can't
    // borrow completeness (the permission check keys on the actor, but pinning to their grant keeps the
    // sync-floor honest and is the coordinate the delegation verifier will resolve rank at).
    if entity_hex != actor_grant_hex {
        return false;
    }
    match heads.iter().find(|h| h.entity_hex == entity_hex) {
        // We hold a LATER edition of the actor's grant than they cited — synced past it. Whether the
        // actor is STILL authorized is the caller's roster check (which reflects this later head).
        Some(h) if h.version > c.version => true,
        // Synced to exactly the cited version: the cited hash must be the one that won our fold (else
        // the actor cited a non-canonical fork of their own grant).
        Some(h) if h.version == c.version => h.self_hash == c.edition_hash,
        // Not surfaced, or our head is BEHIND the cited version → we cannot confirm the authority.
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authority_citation_satisfied_pins_to_a_synced_complete_grant() {
        let owner = "ow".repeat(32);
        let actor = "ac".repeat(32);
        let entity = [0x55u8; 32];
        let hash = [0x66u8; 32];
        let eh = crate::simd::hex::bytes_to_hex_32(&entity); // the actor's own grant locator
        let head = |v: u64, h: [u8; 32]| {
            vec![EntityHead { entity_hex: eh.clone(), version: v, self_hash: h, inner_id: [0u8; 32], citation: None }]
        };
        let cite = AuthorityCitation { entity_id: entity, version: 3, edition_hash: hash };

        // Owner is supreme and cites nothing.
        assert!(authority_citation_satisfied(&[], Some(&owner), &owner, &eh, None));
        // A non-owner with no citation fails closed.
        assert!(!authority_citation_satisfied(&head(3, hash), Some(&owner), &actor, &eh, None));
        // Synced to exactly the cited version with the matching hash → satisfied.
        assert!(authority_citation_satisfied(&head(3, hash), Some(&owner), &actor, &eh, Some(&cite)));
        // Same version, wrong hash (a cited fork at the tip) → rejected.
        assert!(!authority_citation_satisfied(&head(3, [0xEE; 32]), Some(&owner), &actor, &eh, Some(&cite)));
        // We hold a LATER head of the actor's grant → synced past it (the roster check handles supersession).
        assert!(authority_citation_satisfied(&head(4, [0x77; 32]), Some(&owner), &actor, &eh, Some(&cite)));
        // We are BEHIND the cited version → fail closed.
        assert!(!authority_citation_satisfied(&head(2, hash), Some(&owner), &actor, &eh, Some(&cite)));
        // The cited grant isn't surfaced at all → fail closed.
        assert!(!authority_citation_satisfied(&[], Some(&owner), &actor, &eh, Some(&cite)));
        // The citation names a FOREIGN entity (not the actor's grant) → rejected even if surfaced.
        let foreign = "ff".repeat(32);
        assert!(!authority_citation_satisfied(&head(3, hash), Some(&owner), &actor, &foreign, Some(&cite)));
    }

    const VSK_GRANT: &str = "3";

    fn eid() -> [u8; 32] {
        [0x42; 32]
    }

    #[test]
    fn round_trips_authorship_version_and_chain_hash() {
        let actor = Keys::generate();
        let prev = version::edition_hash(&eid(), 1, None, b"{}");
        let inner = build_edition_inner(actor.public_key(), VSK_GRANT, &eid(), 2, Some(&prev), "{\"role_ids\":[]}", 1_700_000_000, None)
            .finalize(&actor)
            .unwrap();

        let parsed = parse_edition_inner(&inner).expect("valid edition parses");
        assert_eq!(parsed.author, actor.public_key(), "authorship = the real signer");
        assert_eq!(parsed.vsk, VSK_GRANT);
        assert_eq!(parsed.entity_id, eid());
        assert_eq!(parsed.version, 2);
        assert_eq!(parsed.prev_hash, Some(prev));
        assert_eq!(parsed.created_at, 1_700_000_000);
        // self_hash matches the canonical recomputation (what the next edition will cite).
        assert_eq!(
            parsed.self_hash,
            version::edition_hash(&eid(), 2, Some(&prev), b"{\"role_ids\":[]}")
        );
        // Folds into a version::Edition cleanly.
        let fe = parsed.to_fold_edition();
        assert_eq!(fe.version, 2);
        assert_eq!(fe.prev_hash, Some(prev));
    }

    #[test]
    fn authority_citation_round_trips_on_an_edition() {
        // A delegated admin's edition carries the pinned authority citation; it survives sign→parse,
        // covered by the inner signature, and does NOT alter the chain self_hash (per-action metadata).
        let actor = Keys::generate();
        let cite = AuthorityCitation { entity_id: [0xab; 32], version: 7, edition_hash: [0xcd; 32] };
        let inner = build_edition_inner(actor.public_key(), VSK_GRANT, &eid(), 1, None, "{}", 100, Some(&cite))
            .finalize(&actor)
            .unwrap();
        let parsed = parse_edition_inner(&inner).unwrap();
        assert_eq!(parsed.authority.as_ref(), Some(&cite), "citation round-trips");
        // self_hash is over (entity, version, prev, content) only — the citation doesn't perturb it.
        assert_eq!(parsed.self_hash, version::edition_hash(&eid(), 1, None, b"{}"));

        // An uncited edition (owner-signed) parses with authority == None.
        let owner = build_edition_inner(actor.public_key(), VSK_GRANT, &eid(), 1, None, "{}", 100, None)
            .finalize(&actor)
            .unwrap();
        assert_eq!(parse_edition_inner(&owner).unwrap().authority, None);
    }

    #[test]
    fn authority_citation_tag_layout_is_frozen() {
        // FROZEN wire layout: the citation rides as a 4-element `vac` tag
        // `["vac", <entity hex>, <version>, <edition-hash hex>]`. A change here reshuffles how every
        // verifier reads pinned authority, so pin the exact shape (not just a round-trip).
        let cite = AuthorityCitation { entity_id: [0x11; 32], version: 9, edition_hash: [0x22; 32] };
        let tag = cite.to_tag();
        let s = tag.as_slice();
        assert_eq!(s.len(), 4, "vac is a 4-element tag");
        assert_eq!(s[0], TAG_AUTHORITY_CITATION);
        assert_eq!(s[1], "11".repeat(32), "entity id is lowercase hex");
        assert_eq!(s[2], "9", "version is the decimal string");
        assert_eq!(s[3], "22".repeat(32), "edition hash is lowercase hex");
    }

    #[test]
    fn genesis_edition_has_no_prev() {
        let actor = Keys::generate();
        let inner = build_edition_inner(actor.public_key(), "1", &eid(), 1, None, "{}", 100, None)
            .finalize(&actor)
            .unwrap();
        let parsed = parse_edition_inner(&inner).unwrap();
        assert_eq!(parsed.prev_hash, None, "first edition cites no predecessor");
        assert_eq!(parsed.version, 1);
    }

    #[test]
    fn tampered_content_fails_verification() {
        // Re-sign integrity: flipping the content after signing breaks the inner Schnorr sig.
        let actor = Keys::generate();
        let inner = build_edition_inner(actor.public_key(), "3", &eid(), 1, None, "{\"a\":1}", 100, None)
            .finalize(&actor)
            .unwrap();
        let mut json: serde_json::Value = serde_json::from_str(&inner.as_json()).unwrap();
        json["content"] = serde_json::Value::String("{\"a\":2}".into()); // tamper
        let tampered: Event = serde_json::from_value(json).unwrap();
        assert!(matches!(parse_edition_inner(&tampered), Err(EditionError::BadSignature)));
    }

    #[test]
    fn missing_required_field_is_rejected_not_panicked() {
        // An inner event lacking the entity-id tag is a parse error, never a panic.
        let actor = Keys::generate();
        let inner = EventBuilder::new(Kind::Custom(event_kind::COMMUNITY_CONTROL), "{}")
            .tags([Tag::custom("vsk", ["3".to_string()])])
            .finalize(&actor)
            .unwrap();
        assert!(matches!(parse_edition_inner(&inner), Err(EditionError::MissingField("eid"))));
    }

    #[test]
    fn duplicate_authority_tag_is_rejected() {
        // A duplicate of ANY of the 5 authority tags (vsk/eid/ev/ep/vac) makes signed-event → canonical
        // bytes ambiguous (clients could pick different ones) → chain divergence, so it must be rejected.
        // Parameterized across all 5 — a regression dropping any one from the dedup loop is caught.
        let actor = Keys::generate();
        let hash = crate::simd::hex::bytes_to_hex_32(&[0xAB; 32]);
        let base = || -> Vec<Tag> {
            vec![
                Tag::custom("vsk", ["1".to_string()]),
                Tag::custom("eid", [crate::simd::hex::bytes_to_hex_32(&eid())]),
                Tag::custom("ev", ["1".to_string()]),
                Tag::custom("ep", [hash.clone()]),
                Tag::custom("vac", [crate::simd::hex::bytes_to_hex_32(&eid()), "1".to_string(), hash.clone()]),
            ]
        };
        let build = |tags: Vec<Tag>| EventBuilder::new(Kind::Custom(event_kind::COMMUNITY_CONTROL), "{}")
            .tags(tags).finalize(&actor).unwrap();
        assert!(parse_edition_inner(&build(base())).is_ok(), "a clean 5-tag base edition parses");
        for name in ["vsk", "eid", "ev", "ep", "vac"] {
            let mut tags = base();
            let dup = tags.iter().find(|t| t.as_slice().first().map(|s| s == name).unwrap_or(false)).cloned().unwrap();
            tags.push(dup);
            assert!(
                matches!(parse_edition_inner(&build(tags)), Err(EditionError::BadField("duplicate authority tag"))),
                "a duplicate `{name}` tag must be rejected"
            );
        }
    }
}
