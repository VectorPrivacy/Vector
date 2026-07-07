//! Dissolution — CORD-02 §9.
//!
//! A Community ends by one owner-signed **tombstone** at a coordinate derived
//! from the `community_id` ALONE ([`super::derive::dissolved_group_key`]) — no
//! key, no epoch — so every member past or present resolves the same address and
//! a Refounding can never strand the grave. The tombstone is terminal and
//! CHAINLESS: no version to race, nothing to edit; the presence of one valid
//! owner-signed edition at the coordinate IS the state.
//!
//! The tombstone is authority-bearing state anyone holding only the
//! `community_id` must be able to verify, and its content is empty, so Vector
//! seals it with the **plaintext** seal form (kind 20014, like the Control
//! Plane) rather than the double-encrypted form — nothing here is secret. On
//! read the check is lenient about the seal form but strict about who signed it:
//! only the owner the `community_id` commits to (§1) counts. An impostor's event
//! at the (findable-by-anyone) address is noise — fail-closed, an
//! unverifiable-or-foreign tombstone is NOT death.
//!
//! Two caller responsibilities this module deliberately does NOT implement:
//!   - On a verified tombstone the client seals the Community read-only:
//!     subscriptions halted, held keys still open history but nothing new is
//!     honored. Death wins every race — once a valid tombstone exists no epoch
//!     advance past it is honored, so a Refounding racing a dissolution loses
//!     (the caller enforces this at epoch-advance, not here).
//!   - The one carve-out is a member's own kind-5 self-delete of their own past
//!     message, honored even post-seal (a self-scrub injects no content, so
//!     read-only isn't violated). That lives on the message plane, not here.

use nostr_sdk::prelude::{Event, Keys, PublicKey, Tag, TagKind, Timestamp, UnsignedEvent};

use super::super::CommunityId;
use super::control::CommunityIdentity;
use super::derive::dissolved_group_key;
use super::stream::{self, SealForm, StreamError};
use super::{kind, vsk};

const TAG_VSK: &str = "vsk";
const TAG_EID: &str = "eid";

/// A verified dissolution tombstone: it carries only its proven owner (the seal
/// signer). Everything else is fixed by the protocol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DissolvedTombstone {
    pub owner: PublicKey,
}

/// Errors from opening a tombstone wrap.
#[derive(Debug)]
pub enum DissolveError {
    Stream(StreamError),
    /// The opened rumor isn't a chainless `vsk 10` / all-zero-`eid` tombstone.
    NotATombstone,
}

impl std::fmt::Display for DissolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DissolveError::Stream(e) => write!(f, "stream: {e}"),
            DissolveError::NotATombstone => write!(f, "not a dissolution tombstone"),
        }
    }
}

impl std::error::Error for DissolveError {}

impl From<StreamError> for DissolveError {
    fn from(e: StreamError) -> Self {
        DissolveError::Stream(e)
    }
}

/// The all-zero `eid` hex, 64 chars — the tombstone's chainless coordinate.
fn zero_eid_hex() -> String {
    crate::simd::hex::bytes_to_hex_32(&[0u8; 32])
}

/// Build the owner-dissolution tombstone rumor: kind 3308, empty content,
/// `["vsk","10"]` + `["eid", 0…0]`, and CHAINLESS — no `ev`, `ep`, or `vac`.
///
/// The `community_id` binds the tombstone through its ADDRESS (see
/// [`seal_dissolved`]), not through the rumor, so it takes no id here — only the
/// owner and a send time.
pub fn dissolved_tombstone_rumor(owner: PublicKey, created_at_secs: u64) -> UnsignedEvent {
    let tags = vec![
        Tag::custom(TagKind::Custom(TAG_VSK.into()), [vsk::DISSOLVED.to_string()]),
        Tag::custom(TagKind::Custom(TAG_EID.into()), [zero_eid_hex()]),
    ];
    stream::build_rumor_secs(kind::CONTROL, owner, "", tags, created_at_secs)
}

/// Sign (plaintext seal) + wrap the tombstone rumor at the community's dissolved
/// address. Local-keys convenience; a bunker signs the seal itself via
/// [`stream::seal_content`] + [`stream::wrap_seal`] for identical wire output.
pub fn seal_dissolved(
    rumor: &UnsignedEvent,
    community_id: &CommunityId,
    owner_keys: &Keys,
    wrap_at: Timestamp,
) -> Result<Event, DissolveError> {
    let group = dissolved_group_key(community_id);
    let seal = stream::build_seal(rumor, SealForm::Plaintext, &group, owner_keys)?;
    let (wrap, _ephemeral) = stream::wrap_seal(&seal, &group, stream::KIND_WRAP, wrap_at)?;
    Ok(wrap)
}

/// Open + structurally verify a wrap at the dissolved address into its signer.
/// This proves the seal signature and the tombstone shape but NOT the owner —
/// [`verify_dissolved`] is the fail-closed authority gate.
pub fn open_dissolved(wrap: &Event, community_id: &CommunityId) -> Result<DissolvedTombstone, DissolveError> {
    let group = dissolved_group_key(community_id);
    let opened = stream::open_wrap(wrap, &group)?;
    if !is_tombstone_rumor(&opened.rumor) {
        return Err(DissolveError::NotATombstone);
    }
    Ok(DissolvedTombstone { owner: opened.author })
}

/// Whether `wrap` is a valid owner-signed dissolution for `identity`. Valid ONLY
/// if the identity self-certifies AND the tombstone's seal signer is the exact
/// owner the `community_id` commits to. Fail-closed: a foreign-signed or
/// unverifiable tombstone is NOT death.
pub fn verify_dissolved(wrap: &Event, identity: &CommunityIdentity) -> bool {
    if !identity.verify() {
        return false;
    }
    let Ok(owner) = identity.owner() else {
        return false;
    };
    match open_dissolved(wrap, &identity.community_id) {
        Ok(tombstone) => tombstone.owner == owner,
        Err(_) => false,
    }
}

/// A chainless `vsk 10` tombstone: kind 3308, `vsk == "10"`, `eid == 0…0`. The
/// seal form and any extra tags are irrelevant — the marker plus the (already
/// seal-verified) owner signature is the whole state.
fn is_tombstone_rumor(rumor: &UnsignedEvent) -> bool {
    rumor.kind.as_u16() == kind::CONTROL
        && first_tag(rumor, TAG_VSK).as_deref() == Some(vsk::DISSOLVED)
        && first_tag(rumor, TAG_EID).as_deref() == Some(zero_eid_hex().as_str())
}

fn first_tag(rumor: &UnsignedEvent, name: &str) -> Option<String> {
    rumor.tags.iter().find_map(|t| {
        let s = t.as_slice();
        (s.len() >= 2 && s[0] == name).then(|| s[1].clone())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity_and_owner() -> (CommunityIdentity, Keys) {
        let owner = Keys::generate();
        let identity = CommunityIdentity::mint(&owner.public_key());
        assert!(identity.verify());
        (identity, owner)
    }

    #[test]
    fn owner_tombstone_round_trips_and_verifies() {
        let (identity, owner) = identity_and_owner();
        let rumor = dissolved_tombstone_rumor(owner.public_key(), 1_725_000_000);
        let wrap = seal_dissolved(&rumor, &identity.community_id, &owner, Timestamp::from_secs(1_725_000_000)).unwrap();

        let opened = open_dissolved(&wrap, &identity.community_id).unwrap();
        assert_eq!(opened.owner, owner.public_key());
        assert!(verify_dissolved(&wrap, &identity));
    }

    #[test]
    fn a_non_owner_tombstone_is_not_death() {
        let (identity, _owner) = identity_and_owner();
        // Anyone holding the community_id can FIND the address and post there,
        // but only the committed owner's signature counts.
        let impostor = Keys::generate();
        let rumor = dissolved_tombstone_rumor(impostor.public_key(), 1_725_000_000);
        let wrap = seal_dissolved(&rumor, &identity.community_id, &impostor, Timestamp::from_secs(1_725_000_000)).unwrap();

        // It opens (structurally valid, impostor-signed) but fails the authority gate.
        assert_eq!(open_dissolved(&wrap, &identity.community_id).unwrap().owner, impostor.public_key());
        assert!(!verify_dissolved(&wrap, &identity), "a foreign-signed tombstone is not death");
    }

    #[test]
    fn a_non_self_certifying_identity_fails_closed() {
        let (identity, owner) = identity_and_owner();
        let rumor = dissolved_tombstone_rumor(owner.public_key(), 1_725_000_000);
        let wrap = seal_dissolved(&rumor, &identity.community_id, &owner, Timestamp::from_secs(1_725_000_000)).unwrap();

        // Same id, a claimed owner that doesn't reproduce the commitment.
        let attacker = Keys::generate();
        let forged = CommunityIdentity {
            community_id: identity.community_id,
            owner_xonly: attacker.public_key().to_bytes(),
            owner_salt: identity.owner_salt,
        };
        assert!(!forged.verify());
        assert!(!verify_dissolved(&wrap, &forged), "an identity that doesn't self-certify can't accept a tombstone");
    }

    #[test]
    fn the_address_is_community_id_derived_and_epoch_free() {
        let (identity, owner) = identity_and_owner();
        let rumor = dissolved_tombstone_rumor(owner.public_key(), 1_725_000_000);
        let wrap = seal_dissolved(&rumor, &identity.community_id, &owner, Timestamp::from_secs(1_725_000_000)).unwrap();

        // A fresh joiner holding ONLY the community_id resolves the same grave at
        // "any epoch" — the derivation takes none — and opens the tombstone.
        let a = dissolved_group_key(&identity.community_id);
        let b = dissolved_group_key(&identity.community_id);
        assert_eq!(a.pk_hex(), b.pk_hex());
        assert_eq!(open_dissolved(&wrap, &identity.community_id).unwrap().owner, owner.public_key());

        // A different community's address can't open it (WrongStream, not death).
        let other = CommunityIdentity::mint(&owner.public_key());
        assert!(open_dissolved(&wrap, &other.community_id).is_err());
        assert!(!verify_dissolved(&wrap, &other));
    }

    #[test]
    fn the_tombstone_rumor_is_chainless() {
        let owner = Keys::generate();
        let rumor = dissolved_tombstone_rumor(owner.public_key(), 1_725_000_000);
        assert_eq!(rumor.kind.as_u16(), kind::CONTROL);
        assert!(rumor.content.is_empty());
        assert_eq!(first_tag(&rumor, TAG_VSK).as_deref(), Some(vsk::DISSOLVED));
        assert_eq!(first_tag(&rumor, TAG_EID).as_deref(), Some(zero_eid_hex().as_str()));
        // No chain machinery: ev / ep / vac are all absent.
        for machinery in ["ev", "ep", "vac"] {
            assert!(first_tag(&rumor, machinery).is_none(), "chainless: {machinery} must be absent");
        }
    }
}
