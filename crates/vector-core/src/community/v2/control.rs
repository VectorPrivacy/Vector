//! The v2 Control Plane (CORD-04 over CORD-01/02).
//!
//! Editions keep v1's exact grammar — the `vsk/eid/ev/ep/vac` tags, the frozen
//! `edition_hash` (`community::version`, label `vector-community/v1/edition`,
//! which upstream froze verbatim), and the fold rules — but ride the v2 stream
//! envelope: a kind-3308 UNSIGNED rumor inside a **plaintext seal** (20014) at
//! `control_pk`. Authorship moves from the rumor's own signature (v1) to the
//! seal's (v2): [`super::stream::open_wrap`] Schnorr-verifies the seal and pins
//! `rumor.pubkey == seal.pubkey`, so by the time an edition is parsed here its
//! author is already proven. The plaintext seal is load-bearing: a compaction
//! re-wraps the signed seal into a new epoch byte-verbatim, and the signature
//! (over the rumor string) survives — an encrypted seal could not.
//!
//! Two v1↔v2 wire deltas, both deliberate:
//!   - v2 editions carry NO `["v","1"]` protocol tag (frozen derivations
//!     partition protocol revisions by address; version tags are the rejected
//!     mechanism).
//!   - the owner is proven by the self-certifying `community_id` commitment
//!     ([`super::derive::verify_community_id`]), not an attestation event —
//!     vsk 7 is retired.

use nostr_sdk::prelude::{Event, Keys, PublicKey, Tag, TagKind, Timestamp, UnsignedEvent};
use serde::{Deserialize, Serialize};

use super::super::edition::{AuthorityCitation, EditionError, ParsedEdition, TAG_AUTHORITY_CITATION};
use super::super::{version, ChannelId, CommunityId, Epoch};
use super::derive::{control_group_key, verify_community_id, GroupKey};
use super::stream::{self, OpenedStream, SealForm, StreamError};
use super::{kind, vsk};

const TAG_SUBKIND: &str = "vsk";
const TAG_ENTITY: &str = "eid";
const TAG_EVERSION: &str = "ev";
const TAG_EPREV: &str = "ep";

/// Protocol-wide UTF-8 byte cap on names (community, channel, role).
pub const MAX_NAME_BYTES: usize = 64;
/// UTF-8 byte cap on a community description.
pub const MAX_DESCRIPTION_BYTES: usize = 10_000;

/// Errors from the control plane layer (envelope errors ride inside).
#[derive(Debug)]
pub enum ControlError {
    Stream(StreamError),
    Edition(EditionError),
    /// The rumor isn't a kind-3308 edition.
    NotAnEdition(u16),
    /// A control edition arrived in an encrypted seal — CORD-02 §5 requires the
    /// plaintext form (compaction must preserve signatures), so a strict reader
    /// drops it rather than folding a chain a re-wrap would later fork.
    NotPlaintextSealed,
    /// A name/description exceeds its protocol byte cap.
    OverCap(&'static str),
}

impl std::fmt::Display for ControlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ControlError::Stream(e) => write!(f, "stream: {e}"),
            ControlError::Edition(e) => write!(f, "edition: {e:?}"),
            ControlError::NotAnEdition(k) => write!(f, "rumor kind {k} is not a control edition"),
            ControlError::NotPlaintextSealed => write!(f, "control edition must ride a plaintext seal"),
            ControlError::OverCap(what) => write!(f, "{what} exceeds its byte cap"),
        }
    }
}

impl std::error::Error for ControlError {}

impl From<StreamError> for ControlError {
    fn from(e: StreamError) -> Self {
        ControlError::Stream(e)
    }
}

// ── Identity ─────────────────────────────────────────────────────────────────

/// A v2 community's self-certifying identity triple. The id IS the owner
/// commitment — carry all three together and verify before trusting any claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommunityIdentity {
    pub community_id: CommunityId,
    pub owner_xonly: [u8; 32],
    pub owner_salt: [u8; 32],
}

impl CommunityIdentity {
    /// Mint a fresh identity for `owner` (a new random salt ⇒ a new community).
    pub fn mint(owner: &PublicKey) -> CommunityIdentity {
        let owner_xonly = owner.to_bytes();
        let owner_salt = super::super::random_32();
        CommunityIdentity {
            community_id: super::derive::community_id_of(&owner_xonly, &owner_salt),
            owner_xonly,
            owner_salt,
        }
    }

    /// True iff the commitment reproduces the id — the ONLY valid owner proof.
    pub fn verify(&self) -> bool {
        verify_community_id(&self.community_id, &self.owner_xonly, &self.owner_salt)
    }

    /// The proven owner as a `PublicKey` (call only after [`Self::verify`]).
    pub fn owner(&self) -> Result<PublicKey, String> {
        PublicKey::from_slice(&self.owner_xonly).map_err(|e| e.to_string())
    }
}

// ── Edition rumors (build + parse) ───────────────────────────────────────────

/// Build the unsigned kind-3308 edition rumor — v1's grammar minus the protocol
/// `v` tag. Control editions carry no `ms` tag: they fold by version, not time.
#[allow(clippy::too_many_arguments)]
pub fn build_edition_rumor(
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
        Tag::custom(TagKind::Custom(TAG_SUBKIND.into()), [vsk.to_string()]),
        Tag::custom(TagKind::Custom(TAG_ENTITY.into()), [crate::simd::hex::bytes_to_hex_32(entity_id)]),
        Tag::custom(TagKind::Custom(TAG_EVERSION.into()), [version.to_string()]),
    ];
    if let Some(p) = prev_hash {
        tags.push(Tag::custom(TagKind::Custom(TAG_EPREV.into()), [crate::simd::hex::bytes_to_hex_32(p)]));
    }
    if let Some(a) = authority {
        tags.push(a.to_tag());
    }
    stream::build_rumor_secs(kind::CONTROL, author, content, tags, created_at_secs)
}

/// Parse an edition from an ALREADY-VERIFIED rumor (one produced by
/// [`stream::open_wrap`], which proved the seal signature, the author binding,
/// and the rumor id). No signature lives on the rumor itself — never feed this
/// a rumor that didn't come through the stream verifier.
pub fn parse_edition_rumor(rumor: &UnsignedEvent) -> Result<ParsedEdition, ControlError> {
    if rumor.kind.as_u16() != kind::CONTROL {
        return Err(ControlError::NotAnEdition(rumor.kind.as_u16()));
    }
    // Duplicate machinery tags make signed-bytes → canonical-fields ambiguous
    // (two clients could pick different duplicates and fork on self_hash).
    for name in [TAG_SUBKIND, TAG_ENTITY, TAG_EVERSION, TAG_EPREV, TAG_AUTHORITY_CITATION] {
        let count = rumor
            .tags
            .iter()
            .filter(|t| t.as_slice().first().map(|s| s.as_str() == name).unwrap_or(false))
            .count();
        if count > 1 {
            return Err(ControlError::Edition(EditionError::BadField("duplicate authority tag")));
        }
    }
    let get = |name: &str| -> Option<String> {
        rumor.tags.iter().find_map(|t| {
            let s = t.as_slice();
            (s.len() >= 2 && s[0] == name).then(|| s[1].clone())
        })
    };
    let decode_hash = |hex: &str, field: &'static str| -> Result<[u8; 32], ControlError> {
        if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(ControlError::Edition(EditionError::BadField(field)));
        }
        Ok(crate::simd::hex::hex_to_bytes_32(hex))
    };
    let vsk = get(TAG_SUBKIND).ok_or(ControlError::Edition(EditionError::MissingField("vsk")))?;
    let entity_id = decode_hash(&get(TAG_ENTITY).ok_or(ControlError::Edition(EditionError::MissingField("eid")))?, "eid")?;
    let version: u64 = get(TAG_EVERSION)
        .ok_or(ControlError::Edition(EditionError::MissingField("ev")))?
        .parse()
        .map_err(|_| ControlError::Edition(EditionError::BadField("ev")))?;
    let prev_hash = match get(TAG_EPREV) {
        Some(h) => Some(decode_hash(&h, "ep")?),
        None => None,
    };
    let self_hash = version::edition_hash(&entity_id, version, prev_hash.as_ref(), rumor.content.as_bytes());
    Ok(ParsedEdition {
        author: rumor.pubkey,
        vsk,
        entity_id,
        version,
        prev_hash,
        content: rumor.content.clone(),
        self_hash,
        created_at: rumor.created_at.as_secs(),
        inner_id: rumor.id.expect("verified rumors carry their id").to_bytes(),
        authority: AuthorityCitation::from_tags(&rumor.tags),
    })
}

// ── Seal / open over the stream ──────────────────────────────────────────────

/// Seal a signed-by-`author_keys` edition rumor into a control-plane wrap.
/// Local-keys convenience; bunker accounts use [`stream::seal_content`] +
/// their remote signer + [`stream::wrap_seal`] for identical wire output.
pub fn seal_control_edition(
    rumor: &UnsignedEvent,
    group: &GroupKey,
    author_keys: &Keys,
    wrap_at: Timestamp,
) -> Result<(Event, Keys), ControlError> {
    let seal = stream::build_seal(rumor, SealForm::Plaintext, group, author_keys)?;
    Ok(stream::wrap_seal(&seal, group, stream::KIND_WRAP, wrap_at)?)
}

/// Open a control-plane wrap into a verified, parsed edition. Strict on both
/// gates: the rumor must be kind 3308, and the seal must be the plaintext form.
pub fn open_control_edition(wrap: &Event, group: &GroupKey) -> Result<(ParsedEdition, OpenedStream), ControlError> {
    let opened = stream::open_wrap(wrap, group)?;
    if opened.seal_form != SealForm::Plaintext {
        return Err(ControlError::NotPlaintextSealed);
    }
    let edition = parse_edition_rumor(&opened.rumor)?;
    Ok((edition, opened))
}

// ── Entity payloads (vsk 0 / vsk 2) ──────────────────────────────────────────

/// An encrypted-blob image pointer (icon/banner): the media server sees only an
/// opaque blob; members fetch, decrypt with `key`/`nonce`, and verify `hash`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImageRef {
    pub url: String,
    pub key: String,
    pub nonce: String,
    pub hash: String,
    /// Unknown fields round-trip (e.g. Vector's `ext`) — editors MUST preserve
    /// what they don't understand (CORD-02 §6).
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// Community metadata — the vsk-0 entity content (CORD-02 §6). `eid` = the
/// community_id itself; gated by `MANAGE_METADATA`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct CommunityMetadata {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relays: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<ImageRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub banner: Option<ImageRef>,
    /// Client-extensible opaque object; folds atomically with the entity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom: Option<serde_json::Map<String, serde_json::Value>>,
    /// Reserved-for-protocol unknown top-level fields, round-tripped verbatim.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// Channel metadata — the vsk-2 entity content (CORD-03 §2). `eid` = the
/// channel_id; gated by `MANAGE_CHANNELS`. Absent flags mean false; deletion is
/// terminal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ChannelMetadata {
    pub name: String,
    pub private: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub voice: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deleted: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom: Option<serde_json::Map<String, serde_json::Value>>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// Enforce the protocol byte caps before an edition is built (a strict reader
/// may drop over-cap state; never publish what peers would refuse).
pub fn validate_community_metadata(meta: &CommunityMetadata) -> Result<(), ControlError> {
    if meta.name.len() > MAX_NAME_BYTES {
        return Err(ControlError::OverCap("name"));
    }
    if meta.description.as_ref().is_some_and(|d| d.len() > MAX_DESCRIPTION_BYTES) {
        return Err(ControlError::OverCap("description"));
    }
    Ok(())
}

pub fn validate_channel_metadata(meta: &ChannelMetadata) -> Result<(), ControlError> {
    if meta.name.len() > MAX_NAME_BYTES {
        return Err(ControlError::OverCap("name"));
    }
    Ok(())
}

// ── Genesis (CORD-02 §1) ─────────────────────────────────────────────────────

/// The two wraps of a community genesis — exactly two owner-signed editions:
/// the community metadata (vsk 0) and one public `#general` channel (vsk 2).
/// Nothing more — no default roles, no scaffolding.
pub struct Genesis {
    pub identity: CommunityIdentity,
    /// The community_root minted for epoch 0.
    pub community_root: [u8; 32],
    pub general_channel_id: ChannelId,
    /// `[metadata wrap, #general wrap]`, both sealed at the epoch-0 control_pk.
    pub wraps: [Event; 2],
}

/// Mint a v2 community: fresh identity (salt-committed to the owner), fresh
/// community_root, and the two genesis editions sealed at the epoch-0 control
/// plane. The caller persists the secrets and publishes the wraps.
pub fn genesis(owner_keys: &Keys, mut metadata: CommunityMetadata, at_secs: u64) -> Result<Genesis, ControlError> {
    validate_community_metadata(&metadata)?;
    // Relays ride the metadata entity so they can evolve by edit; cap on write.
    metadata.relays.truncate(super::super::MAX_COMMUNITY_RELAYS);

    let identity = CommunityIdentity::mint(&owner_keys.public_key());
    let community_root = super::super::random_32();
    let general_channel_id = ChannelId(super::super::random_32());
    let group = control_group_key(&community_root, &identity.community_id, Epoch(0));

    let meta_json = serde_json::to_string(&metadata).map_err(|e| ControlError::Stream(StreamError::Parse(e.to_string())))?;
    let meta_rumor = build_edition_rumor(
        owner_keys.public_key(),
        vsk::COMMUNITY_METADATA,
        &identity.community_id.0,
        1,
        None,
        &meta_json,
        at_secs,
        None,
    );

    let general = ChannelMetadata { name: "general".into(), private: false, ..Default::default() };
    let general_json = serde_json::to_string(&general).map_err(|e| ControlError::Stream(StreamError::Parse(e.to_string())))?;
    let general_rumor = build_edition_rumor(
        owner_keys.public_key(),
        vsk::CHANNEL_METADATA,
        &general_channel_id.0,
        1,
        None,
        &general_json,
        at_secs,
        None,
    );

    let (meta_wrap, _) = seal_control_edition(&meta_rumor, &group, owner_keys, Timestamp::from_secs(at_secs))?;
    let (general_wrap, _) = seal_control_edition(&general_rumor, &group, owner_keys, Timestamp::from_secs(at_secs))?;

    Ok(Genesis {
        identity,
        community_root,
        general_channel_id,
        wraps: [meta_wrap, general_wrap],
    })
}

#[cfg(test)]
mod tests {
    use super::super::super::edition::build_edition_inner;
    use super::*;

    fn cid() -> CommunityId {
        CommunityId([0x33; 32])
    }

    fn group_at(epoch: u64) -> GroupKey {
        control_group_key(&[0x44; 32], &cid(), Epoch(epoch))
    }

    fn simple_edition(author: &Keys, version: u64, prev: Option<&[u8; 32]>) -> UnsignedEvent {
        build_edition_rumor(
            author.public_key(),
            vsk::GRANT,
            &[0x55; 32],
            version,
            prev,
            "{\"member\":\"aa\",\"role_ids\":[]}",
            1_700_000_000,
            None,
        )
    }

    #[test]
    fn edition_round_trips_through_the_control_plane() {
        let owner = Keys::generate();
        let group = group_at(0);
        let cite = AuthorityCitation { entity_id: [0xab; 32], version: 7, edition_hash: [0xcd; 32] };
        let rumor = build_edition_rumor(
            owner.public_key(),
            vsk::GRANT,
            &[0x55; 32],
            2,
            Some(&[0x66; 32]),
            "{\"member\":\"aa\",\"role_ids\":[]}",
            1_700_000_000,
            Some(&cite),
        );
        let (wrap, _) = seal_control_edition(&rumor, &group, &owner, Timestamp::from_secs(1_700_000_001)).unwrap();

        let (edition, opened) = open_control_edition(&wrap, &group).unwrap();
        assert_eq!(edition.author, owner.public_key());
        assert_eq!(edition.vsk, vsk::GRANT);
        assert_eq!(edition.entity_id, [0x55; 32]);
        assert_eq!(edition.version, 2);
        assert_eq!(edition.prev_hash, Some([0x66; 32]));
        assert_eq!(edition.authority.as_ref(), Some(&cite));
        assert_eq!(opened.seal_form, SealForm::Plaintext);
        // self_hash matches the canonical recomputation.
        assert_eq!(
            edition.self_hash,
            version::edition_hash(&[0x55; 32], 2, Some(&[0x66; 32]), rumor.content.as_bytes())
        );
    }

    #[test]
    fn v2_edition_tags_carry_no_protocol_version_tag() {
        // FROZEN: the v2 tag set is exactly vsk/eid/ev(+ep/vac) — a `v` tag is
        // the rejected versioning mechanism (address partitioning does the job).
        let owner = Keys::generate();
        let rumor = simple_edition(&owner, 1, None);
        assert!(
            !rumor.tags.iter().any(|t| t.as_slice().first().map(|s| s == "v").unwrap_or(false)),
            "v2 editions must not carry a protocol version tag"
        );
        // And no ms tag — editions fold by version, not time.
        assert!(!rumor.tags.iter().any(|t| t.as_slice().first().map(|s| s == "ms").unwrap_or(false)));
    }

    #[test]
    fn edition_hash_is_identical_across_protocols() {
        // The edition hash is the ONE construction both protocols share (upstream
        // froze v1's byte layout, label included). The same logical edition must
        // hash identically whether built as a v1 signed inner or a v2 rumor —
        // this is what makes the fold engine shareable.
        let author = Keys::generate();
        let entity = [0x55; 32];
        let content = "{\"member\":\"aa\",\"role_ids\":[]}";
        let v1_inner = build_edition_inner(author.public_key(), "3", &entity, 2, Some(&[0x66; 32]), content, 100, None)
            .sign_with_keys(&author)
            .unwrap();
        let v1_parsed = super::super::super::edition::parse_edition_inner(&v1_inner).unwrap();

        let v2_rumor = build_edition_rumor(author.public_key(), "3", &entity, 2, Some(&[0x66; 32]), content, 100, None);
        let v2_parsed = parse_edition_rumor(&v2_rumor).unwrap();

        assert_eq!(v1_parsed.self_hash, v2_parsed.self_hash);
    }

    #[test]
    fn encrypted_seal_control_edition_is_rejected() {
        let owner = Keys::generate();
        let group = group_at(0);
        let rumor = simple_edition(&owner, 1, None);
        let seal = stream::build_seal(&rumor, SealForm::Encrypted, &group, &owner).unwrap();
        let (wrap, _) = stream::wrap_seal(&seal, &group, stream::KIND_WRAP, Timestamp::from_secs(1)).unwrap();
        assert!(matches!(open_control_edition(&wrap, &group), Err(ControlError::NotPlaintextSealed)));
    }

    #[test]
    fn non_edition_rumor_is_rejected() {
        let owner = Keys::generate();
        let group = group_at(0);
        let rumor = stream::build_rumor_secs(super::kind::MESSAGE, owner.public_key(), "hi", vec![], 100);
        let seal = stream::build_seal(&rumor, SealForm::Plaintext, &group, &owner).unwrap();
        let (wrap, _) = stream::wrap_seal(&seal, &group, stream::KIND_WRAP, Timestamp::from_secs(1)).unwrap();
        assert!(matches!(open_control_edition(&wrap, &group), Err(ControlError::NotAnEdition(k)) if k == super::kind::MESSAGE));
    }

    #[test]
    fn duplicate_machinery_tags_are_rejected() {
        let owner = Keys::generate();
        let mut rumor = simple_edition(&owner, 1, None);
        let dup = Tag::custom(TagKind::Custom("eid".into()), [crate::simd::hex::bytes_to_hex_32(&[0x55; 32])]);
        let mut tags: Vec<Tag> = rumor.tags.iter().cloned().collect();
        tags.push(dup);
        rumor = stream::build_rumor_secs(kind::CONTROL, owner.public_key(), &rumor.content, tags, 100);
        assert!(matches!(
            parse_edition_rumor(&rumor),
            Err(ControlError::Edition(EditionError::BadField("duplicate authority tag")))
        ));
    }

    #[test]
    fn compaction_rewrap_preserves_the_edition_chain_identity() {
        // The whole reason control seals are plaintext: carry a signed head into
        // a new epoch and its self_hash + authorship must be untouched, so a
        // fresh joiner folds the same chain the old epoch held.
        let owner = Keys::generate();
        let e0 = group_at(0);
        let rumor = simple_edition(&owner, 3, Some(&[0x77; 32]));
        let (wrap, _) = seal_control_edition(&rumor, &e0, &owner, Timestamp::from_secs(10)).unwrap();
        let (edition, opened) = open_control_edition(&wrap, &e0).unwrap();

        let e1 = group_at(1);
        let (rewrapped, _) = stream::rewrap_seal(&opened.seal, &e1, Timestamp::from_secs(20)).unwrap();
        let (re_edition, _) = open_control_edition(&rewrapped, &e1).unwrap();

        assert_eq!(re_edition.self_hash, edition.self_hash);
        assert_eq!(re_edition.author, edition.author);
        assert_eq!(re_edition.inner_id, edition.inner_id, "rumor id survives compaction");
    }

    #[test]
    fn editions_opened_from_wraps_fold_to_the_head() {
        let owner = Keys::generate();
        let group = group_at(0);
        let entity = [0x55; 32];
        let content = |v: u64| format!("{{\"v\":{v}}}");

        // Build a 3-link chain v1 → v2 → v3.
        let mut prev: Option<[u8; 32]> = None;
        let mut parsed = Vec::new();
        for v in 1..=3u64 {
            let rumor = build_edition_rumor(owner.public_key(), vsk::GRANT, &entity, v, prev.as_ref(), &content(v), 100 + v, None);
            let (wrap, _) = seal_control_edition(&rumor, &group, &owner, Timestamp::from_secs(100 + v)).unwrap();
            let (edition, _) = open_control_edition(&wrap, &group).unwrap();
            prev = Some(edition.self_hash);
            parsed.push(edition);
        }

        let fold_editions: Vec<version::Edition> = parsed.iter().map(|p| p.to_fold_edition()).collect();
        let folded = version::fold(&fold_editions, 0, None);
        assert_eq!(fold_editions[folded.head.expect("chain folds")].version, 3);
        assert!(!folded.gap);

        // Withhold the middle link: the chain gaps at v1 (fail-closed signal).
        let partial = [fold_editions[0].clone(), fold_editions[2].clone()];
        let gapped = version::fold(&partial, 0, None);
        assert_eq!(partial[gapped.head.expect("genesis edition anchors")].version, 1);
        assert!(gapped.gap, "a missing middle version is a gap, not a silent skip");
    }

    #[test]
    fn genesis_mints_a_verifiable_two_edition_community() {
        let owner = Keys::generate();
        let meta = CommunityMetadata {
            name: "Vector".into(),
            description: Some("Private messaging, no compromises.".into()),
            relays: vec!["wss://jskitty.com/nostr".into()],
            ..Default::default()
        };
        let g = genesis(&owner, meta, 1_700_000_000).unwrap();

        // The identity self-certifies and names the owner.
        assert!(g.identity.verify());
        assert_eq!(g.identity.owner().unwrap(), owner.public_key());

        // Exactly two editions, both owner-signed, both openable at epoch 0.
        let group = control_group_key(&g.community_root, &g.identity.community_id, Epoch(0));
        let (meta_ed, _) = open_control_edition(&g.wraps[0], &group).unwrap();
        let (chan_ed, _) = open_control_edition(&g.wraps[1], &group).unwrap();
        assert_eq!(meta_ed.author, owner.public_key());
        assert_eq!(chan_ed.author, owner.public_key());
        assert_eq!(meta_ed.vsk, vsk::COMMUNITY_METADATA);
        assert_eq!(chan_ed.vsk, vsk::CHANNEL_METADATA);
        // Metadata's coordinate IS the community id; the channel's its channel id.
        assert_eq!(meta_ed.entity_id, g.identity.community_id.0);
        assert_eq!(chan_ed.entity_id, g.general_channel_id.0);
        // Both are genesis editions: version 1, no prev, no citation (owner is supreme).
        for e in [&meta_ed, &chan_ed] {
            assert_eq!(e.version, 1);
            assert_eq!(e.prev_hash, None);
            assert_eq!(e.authority, None);
        }
        let general: ChannelMetadata = serde_json::from_str(&chan_ed.content).unwrap();
        assert_eq!(general.name, "general");
        assert!(!general.private);
    }

    #[test]
    fn a_forged_identity_fails_the_commitment() {
        let owner = Keys::generate();
        let attacker = Keys::generate();
        let real = CommunityIdentity::mint(&owner.public_key());
        // An attacker claiming the real community id with their own key + any salt
        // needs a second preimage — verify() must fail.
        let forged = CommunityIdentity {
            community_id: real.community_id,
            owner_xonly: attacker.public_key().to_bytes(),
            owner_salt: real.owner_salt,
        };
        assert!(!forged.verify());
    }

    #[test]
    fn metadata_caps_and_unknown_field_round_trip() {
        let over_name = CommunityMetadata { name: "x".repeat(MAX_NAME_BYTES + 1), ..Default::default() };
        assert!(matches!(validate_community_metadata(&over_name), Err(ControlError::OverCap("name"))));
        let over_desc = CommunityMetadata {
            name: "ok".into(),
            description: Some("d".repeat(MAX_DESCRIPTION_BYTES + 1)),
            ..Default::default()
        };
        assert!(matches!(validate_community_metadata(&over_desc), Err(ControlError::OverCap("description"))));
        // The cap is BYTES, not chars: 22 three-byte chars = 66 bytes > 64.
        let multibyte = ChannelMetadata { name: "€".repeat(22), private: false, ..Default::default() };
        assert!(matches!(validate_channel_metadata(&multibyte), Err(ControlError::OverCap("name"))));

        // Round-trip discipline: unknown top-level fields, unknown icon fields,
        // and the custom object all survive a parse → serialize cycle.
        let wire = r#"{"name":"Vector","relays":["wss://a"],"icon":{"url":"u","key":"k","nonce":"n","hash":"h","ext":"png"},"custom":{"rules":"Be excellent."},"future_field":{"deep":[1,2]}}"#;
        let parsed: CommunityMetadata = serde_json::from_str(wire).unwrap();
        let out = serde_json::to_string(&parsed).unwrap();
        let reparsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(reparsed["future_field"]["deep"][1], 2);
        assert_eq!(reparsed["icon"]["ext"], "png");
        assert_eq!(reparsed["custom"]["rules"], "Be excellent.");
    }
}
