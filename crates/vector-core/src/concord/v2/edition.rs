//! CORD-04 §1: Control Plane editions.
//!
//! Every authority action is a per-entity **edition**: a kind 3308 rumor whose
//! tags carry the machinery (`vsk` entity type, `eid` coordinate, `ev`
//! version, `ep` prev-hash chain link, `vac` authority citation) and whose
//! `content` is the entity's new state. Editions ride plaintext seals so a
//! compaction re-wraps them signature-intact (CORD-06 §3); the fold rules
//! live in `control.rs`.

use nostr_sdk::prelude::*;
use sha2::{Digest, Sha256};

use super::{kind, vsk, ChannelId, CommunityId, RoleId};

/// The frozen edition-hash domain label. The name is historical (the hash
/// construction predates the CORD numbering) and is pinned by the spec —
/// changing a byte forks every chain.
const EDITION_HASH_LABEL: &[u8] = b"vector-community/v1/edition";

/// Edition tag names.
pub const TAG_VSK: &str = "vsk";
pub const TAG_EID: &str = "eid";
pub const TAG_EV: &str = "ev";
pub const TAG_EP: &str = "ep";
pub const TAG_VAC: &str = "vac";

/// The exact Grant edition an actor claims their rank under (CORD-04 §5) —
/// pinned by coordinate, version, AND content hash. A sync floor, never the
/// verdict: the verifier blocks until it holds at least this Grant, then
/// judges against its *current* refuse-downgrade roster.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Citation {
    pub grant_eid: [u8; 32],
    pub grant_version: u64,
    pub grant_hash: [u8; 32],
}

#[derive(Debug)]
pub enum EditionError {
    NotAnEdition(u16),
    MissingTag(&'static str),
    Malformed(String),
}

impl std::fmt::Display for EditionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EditionError::NotAnEdition(k) => write!(f, "kind {k} is not a control edition"),
            EditionError::MissingTag(t) => write!(f, "edition missing tag {t}"),
            EditionError::Malformed(e) => write!(f, "malformed edition: {e}"),
        }
    }
}

impl std::error::Error for EditionError {}

/// One parsed, seal-verified edition. `content` is the exact wire bytes — the
/// hash preimage — never a re-serialization.
#[derive(Debug, Clone)]
pub struct Edition {
    pub vsk: u8,
    pub eid: [u8; 32],
    pub version: u64,
    pub prev: Option<[u8; 32]>,
    pub content: String,
    /// The seal-verified actor.
    pub author: PublicKey,
    /// The rumor id — the deterministic same-version tiebreak (lower wins).
    pub rumor_id: EventId,
    pub created_at: Timestamp,
    pub citation: Option<Citation>,
}

impl Edition {
    /// The edition's identity: what the next edition's `ep` cites (CORD-04 §1).
    /// Length-prefixed, domain-separated, fixed-width — distinct inputs can
    /// never collide, and `content` hashes as its exact wire bytes so a
    /// compaction re-wrap preserves the value.
    pub fn hash(&self) -> [u8; 32] {
        edition_hash(&self.eid, self.version, self.prev.as_ref(), self.content.as_bytes())
    }
}

/// The frozen preimage (CORD-04 §1).
pub fn edition_hash(eid: &[u8; 32], version: u64, prev: Option<&[u8; 32]>, content: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update((EDITION_HASH_LABEL.len() as u64).to_be_bytes());
    h.update(EDITION_HASH_LABEL);
    h.update(eid);
    h.update(version.to_be_bytes());
    match prev {
        Some(p) => {
            h.update([0x01]);
            h.update(p);
        }
        None => {
            h.update([0x00]);
            h.update([0u8; 32]);
        }
    }
    h.update((content.len() as u64).to_be_bytes());
    h.update(content);
    h.finalize().into()
}

/// Deterministic entity coordinates (CORD-04 §1). All bind to the
/// `community_id`, never a key or epoch, so they survive every Refounding.
pub fn metadata_eid(id: &CommunityId) -> [u8; 32] {
    id.0
}

pub fn role_eid(role: &RoleId) -> [u8; 32] {
    role.0
}

pub fn channel_eid(channel: &ChannelId) -> [u8; 32] {
    channel.0
}

/// Build the kind 3308 edition rumor. `citation` is absent when the owner
/// acts — supreme needs no citation.
pub fn build_edition_rumor(
    author: PublicKey,
    entity_vsk: u8,
    eid: &[u8; 32],
    version: u64,
    prev: Option<&[u8; 32]>,
    content: &str,
    created_at_secs: u64,
    citation: Option<&Citation>,
) -> UnsignedEvent {
    let mut tags = vec![
        Tag::custom(TagKind::Custom(TAG_VSK.into()), [entity_vsk.to_string()]),
        Tag::custom(TagKind::Custom(TAG_EID.into()), [crate::simd::hex::bytes_to_hex_32(eid)]),
        Tag::custom(TagKind::Custom(TAG_EV.into()), [version.to_string()]),
    ];
    if let Some(p) = prev {
        tags.push(Tag::custom(TagKind::Custom(TAG_EP.into()), [crate::simd::hex::bytes_to_hex_32(p)]));
    }
    if let Some(c) = citation {
        tags.push(Tag::custom(
            TagKind::Custom(TAG_VAC.into()),
            [
                crate::simd::hex::bytes_to_hex_32(&c.grant_eid),
                c.grant_version.to_string(),
                crate::simd::hex::bytes_to_hex_32(&c.grant_hash),
            ],
        ));
    }
    let mut rumor = EventBuilder::new(Kind::Custom(kind::CONTROL_EDITION), content)
        .tags(tags)
        .custom_created_at(Timestamp::from_secs(created_at_secs))
        .build(author);
    rumor.ensure_id();
    rumor
}

/// Build the chainless Dissolution tombstone (CORD-02 §9): no `ev`, no `ep`,
/// no `vac` — presence of one valid owner-signed edition *is* the state.
pub fn build_dissolved_rumor(owner: PublicKey, created_at_secs: u64) -> UnsignedEvent {
    let mut rumor = EventBuilder::new(Kind::Custom(kind::CONTROL_EDITION), "")
        .tags([
            Tag::custom(TagKind::Custom(TAG_VSK.into()), [vsk::DISSOLVED.to_string()]),
            Tag::custom(TagKind::Custom(TAG_EID.into()), [crate::simd::hex::bytes_to_hex_32(&super::ZERO_ID)]),
        ])
        .custom_created_at(Timestamp::from_secs(created_at_secs))
        .build(owner);
    rumor.ensure_id();
    rumor
}

fn tag_values<'a>(tags: &'a Tags, name: &str) -> Option<Vec<&'a str>> {
    tags.iter()
        .find(|t| t.kind() == TagKind::Custom(name.into()))
        .map(|t| t.as_slice().iter().skip(1).map(|s| s.as_str()).collect())
}

fn tag_value<'a>(tags: &'a Tags, name: &str) -> Option<&'a str> {
    tag_values(tags, name).and_then(|v| v.first().copied())
}

fn hex32(s: &str) -> Result<[u8; 32], EditionError> {
    crate::simd::hex::hex_to_bytes_32_checked(s)
        .ok_or_else(|| EditionError::Malformed(format!("bad 32-byte hex ({} chars)", s.len())))
}

/// Parse a seal-verified kind 3308 rumor into an [`Edition`]. The rumor MUST
/// come out of [`super::stream::open`] (authorship already verified).
pub fn parse_edition(rumor: &UnsignedEvent) -> Result<Edition, EditionError> {
    if rumor.kind.as_u16() != kind::CONTROL_EDITION {
        return Err(EditionError::NotAnEdition(rumor.kind.as_u16()));
    }
    let mut rumor = rumor.clone();
    rumor.ensure_id();

    let vsk_val: u8 = tag_value(&rumor.tags, TAG_VSK)
        .ok_or(EditionError::MissingTag(TAG_VSK))?
        .parse()
        .map_err(|_| EditionError::Malformed("vsk not a u8".into()))?;
    let eid = hex32(tag_value(&rumor.tags, TAG_EID).ok_or(EditionError::MissingTag(TAG_EID))?)?;

    // The dissolved tombstone is chainless and exempt from version discipline.
    let version: u64 = if vsk_val == vsk::DISSOLVED {
        0
    } else {
        tag_value(&rumor.tags, TAG_EV)
            .ok_or(EditionError::MissingTag(TAG_EV))?
            .parse()
            .map_err(|_| EditionError::Malformed("ev not a u64".into()))?
    };
    if vsk_val != vsk::DISSOLVED && version == 0 {
        return Err(EditionError::Malformed("version starts at 1".into()));
    }

    let prev = match tag_value(&rumor.tags, TAG_EP) {
        Some(p) => Some(hex32(p)?),
        None => None,
    };

    let citation = match tag_values(&rumor.tags, TAG_VAC) {
        Some(parts) if parts.len() >= 3 => Some(Citation {
            grant_eid: hex32(parts[0])?,
            grant_version: parts[1]
                .parse()
                .map_err(|_| EditionError::Malformed("vac version not a u64".into()))?,
            grant_hash: hex32(parts[2])?,
        }),
        Some(_) => return Err(EditionError::Malformed("vac tag needs 3 values".into())),
        None => None,
    };

    Ok(Edition {
        vsk: vsk_val,
        eid,
        version,
        prev,
        content: rumor.content.clone(),
        author: rumor.pubkey,
        rumor_id: rumor.id.expect("ensured"),
        created_at: rumor.created_at,
        citation,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Independent Python (hashlib) over the frozen preimage:
    // len64(label)||label || eid=[0x11;32] || be64(4) || 0x01||prev=[0x22;32] || len64(content)||content=b"{\"name\":\"x\"}"
    const GOLDEN_EDITION_HASH_CHAINED: &str =
        "8c038656615a33561ad71efbec67849436c0d01e45d3bbffd6a9e1b696864934";
    // Same but version 1, no prev (0x00 || zeros).
    const GOLDEN_EDITION_HASH_FIRST: &str =
        "2d0f62c1f046b38e61c5c2ff3e67aadcb1bbd429eda822b916c09b1a7f8ac87c";

    #[test]
    fn edition_hash_goldens() {
        let h = edition_hash(&[0x11; 32], 4, Some(&[0x22; 32]), b"{\"name\":\"x\"}");
        assert_eq!(crate::simd::hex::bytes_to_hex_32(&h), GOLDEN_EDITION_HASH_CHAINED);
        let h1 = edition_hash(&[0x11; 32], 1, None, b"{\"name\":\"x\"}");
        assert_eq!(crate::simd::hex::bytes_to_hex_32(&h1), GOLDEN_EDITION_HASH_FIRST);
    }

    #[test]
    fn edition_hash_binds_every_field() {
        let base = edition_hash(&[0x11; 32], 4, Some(&[0x22; 32]), b"c");
        assert_ne!(base, edition_hash(&[0x12; 32], 4, Some(&[0x22; 32]), b"c"));
        assert_ne!(base, edition_hash(&[0x11; 32], 5, Some(&[0x22; 32]), b"c"));
        assert_ne!(base, edition_hash(&[0x11; 32], 4, Some(&[0x23; 32]), b"c"));
        assert_ne!(base, edition_hash(&[0x11; 32], 4, None, b"c"));
        assert_ne!(base, edition_hash(&[0x11; 32], 4, Some(&[0x22; 32]), b"d"));
    }

    #[test]
    fn build_parse_roundtrip() {
        let keys = Keys::generate();
        let citation = Citation { grant_eid: [0xAB; 32], grant_version: 2, grant_hash: [0xCD; 32] };
        let rumor = build_edition_rumor(
            keys.public_key(),
            super::super::vsk::ROLE,
            &[0x11; 32],
            4,
            Some(&[0x22; 32]),
            "{\"name\":\"x\"}",
            1_686_840_217,
            Some(&citation),
        );
        let ed = parse_edition(&rumor).unwrap();
        assert_eq!(ed.vsk, super::super::vsk::ROLE);
        assert_eq!(ed.eid, [0x11; 32]);
        assert_eq!(ed.version, 4);
        assert_eq!(ed.prev, Some([0x22; 32]));
        assert_eq!(ed.content, "{\"name\":\"x\"}");
        assert_eq!(ed.author, keys.public_key());
        assert_eq!(ed.citation, Some(citation));
        // The parsed edition's hash matches the direct construction.
        assert_eq!(ed.hash(), edition_hash(&[0x11; 32], 4, Some(&[0x22; 32]), b"{\"name\":\"x\"}"));
    }

    #[test]
    fn first_edition_has_no_prev_and_version_zero_is_rejected() {
        let keys = Keys::generate();
        let rumor = build_edition_rumor(keys.public_key(), 0, &[0x11; 32], 1, None, "{}", 1, None);
        let ed = parse_edition(&rumor).unwrap();
        assert_eq!(ed.prev, None);

        let bad = build_edition_rumor(keys.public_key(), 0, &[0x11; 32], 0, None, "{}", 1, None);
        assert!(parse_edition(&bad).is_err(), "versions climb from 1");
    }

    #[test]
    fn dissolved_tombstone_is_chainless() {
        let keys = Keys::generate();
        let rumor = build_dissolved_rumor(keys.public_key(), 1_725_000_000);
        let ed = parse_edition(&rumor).unwrap();
        assert_eq!(ed.vsk, super::super::vsk::DISSOLVED);
        assert_eq!(ed.version, 0);
        assert_eq!(ed.prev, None);
        assert_eq!(ed.citation, None);
        assert_eq!(ed.content, "");
    }

    #[test]
    fn non_edition_kind_is_rejected() {
        let keys = Keys::generate();
        let rumor = EventBuilder::new(Kind::Custom(9), "hi").build(keys.public_key());
        assert!(matches!(parse_edition(&rumor), Err(EditionError::NotAnEdition(9))));
    }
}
