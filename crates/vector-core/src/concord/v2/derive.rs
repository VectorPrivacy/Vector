//! Appendix A derivations (CORD-02) — FROZEN.
//!
//! Everything Concord v2 addresses on the wire derives from a Community
//! secret through the shapes below. Changing any labeled byte re-addresses
//! every prior event and forces a migration; the golden vectors in the test
//! module are the spec.
//!
//! Construction (A.1): `HKDF-SHA256(ikm=secret, salt=∅, info, L=32)` where
//! `info = utf8(label) || 0x00 || id[32] || epoch_be[8]` — the `id` always
//! present (all-zeroes where a label has no meaningful id), the epoch the only
//! omittable field, and the `scalar_normalize` retry counter (A.3) appended
//! after whatever fields are present.

use hkdf::Hkdf;
use nostr_sdk::nips::nip44::v2::ConversationKey;
use nostr_sdk::prelude::{Keys, PublicKey, SecretKey};
use sha2::{Digest, Sha256};

use super::{ChannelId, ChannelKey, CommunityId, CommunityRoot, Epoch, OwnerSalt, ZERO_ID};

// Labels (A.6). Part of the wire format — append new ones, never edit or
// reuse an existing one.
const LABEL_CHANNEL: &str = "concord/channel";
const LABEL_CONTROL: &str = "concord/control";
const LABEL_REKEY_PSEUDONYM: &str = "concord/rekey-pseudonym";
const LABEL_BASE_REKEY_PSEUDONYM: &str = "concord/base-rekey-pseudonym";
const LABEL_RECIPIENT_PSEUDONYM: &str = "concord/recipient-pseudonym";
const LABEL_GUESTBOOK: &str = "concord/guestbook";
const LABEL_DISSOLVED: &str = "concord/dissolved";
const LABEL_GRANT: &str = "concord/grant";
const LABEL_BANLIST: &str = "concord/banlist";
const LABEL_INVITE_LINKS: &str = "concord/invite-links";
const LABEL_INVITE_KEY: &str = "concord/invite-key";

// Domain prefixes for the two plain-SHA-256 commitments (A.4, A.5).
const DOMAIN_COMMUNITY_ID: &[u8] = b"concord/community";
const DOMAIN_EPOCH_COMMITMENT: &[u8] = b"concord/epoch-key-commitment";

/// A plane's derived keypair (A.2): the x-only pubkey is the on-wire Stream
/// address (`authors` filter), the secret signs its wraps, and the NIP-44
/// self-ECDH conversation key encrypts them.
#[derive(Clone)]
pub struct GroupKey {
    keys: Keys,
    conv: ConversationKey,
}

impl GroupKey {
    fn from_secret(sk: SecretKey) -> Self {
        let keys = Keys::new(sk);
        let conv = ConversationKey::derive(keys.secret_key(), &keys.public_key())
            .expect("self-ECDH of a valid keypair is infallible");
        GroupKey { keys, conv }
    }

    /// The Stream address: where this plane's events live.
    pub fn public_key(&self) -> PublicKey {
        self.keys.public_key()
    }

    /// The signing keys for this plane's wraps.
    pub fn keys(&self) -> &Keys {
        &self.keys
    }

    /// The NIP-44 self-ECDH conversation key (encrypts the wrap and the
    /// encrypted seal's rumor layer).
    pub fn conversation_key(&self) -> &ConversationKey {
        &self.conv
    }
}

impl core::fmt::Debug for GroupKey {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "GroupKey({})", self.keys.public_key())
    }
}

/// Scope of a rekey (CORD-06 §1): a specific Private Channel, or the base
/// `community_root` (the all-zero sentinel — never collides with a Channel
/// id, which is random).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RekeyScope {
    Channel(ChannelId),
    CommunityRoot,
}

impl RekeyScope {
    pub fn id32(&self) -> [u8; 32] {
        match self {
            RekeyScope::Channel(c) => c.0,
            RekeyScope::CommunityRoot => ZERO_ID,
        }
    }

    pub fn from_id32(id: [u8; 32]) -> Self {
        if id == ZERO_ID {
            RekeyScope::CommunityRoot
        } else {
            RekeyScope::Channel(ChannelId(id))
        }
    }
}

/// Build the frozen `info` bytes (A.1). `counter` is the scalar-normalize
/// retry byte (A.3), appended only when non-zero retries occur.
fn build_info(label: &str, id32: &[u8; 32], epoch: Option<Epoch>, counter: Option<u8>) -> Vec<u8> {
    let mut info = Vec::with_capacity(label.len() + 1 + 32 + 8 + 1);
    info.extend_from_slice(label.as_bytes());
    info.push(0x00);
    info.extend_from_slice(id32);
    if let Some(e) = epoch {
        info.extend_from_slice(&e.0.to_be_bytes());
    }
    if let Some(c) = counter {
        info.push(c);
    }
    info
}

/// HKDF-SHA256, empty salt, 32-byte output. RFC 5869 with no salt pads to the
/// block, identical to a zero-length salt; expand of 32 bytes never fails.
fn hkdf32(ikm: &[u8], info: &[u8]) -> [u8; 32] {
    let hk = Hkdf::<Sha256>::new(None, ikm);
    let mut okm = [0u8; 32];
    hk.expand(info, &mut okm)
        .expect("HKDF expand of 32 bytes is infallible");
    okm
}

/// A.2 + A.3: derive a plane's `GroupKey`, normalizing the seed into a valid
/// secp256k1 scalar by appending an incrementing counter byte to `info` on the
/// ~2⁻¹²⁸-rare reject, deterministic across implementations.
fn group_key(label: &str, ikm: &[u8], id32: &[u8; 32], epoch: Option<Epoch>) -> GroupKey {
    let mut counter: u8 = 0;
    loop {
        let info = build_info(label, id32, epoch, (counter > 0).then_some(counter));
        let seed = hkdf32(ikm, &info);
        if let Ok(sk) = SecretKey::from_slice(&seed) {
            return GroupKey::from_secret(sk);
        }
        counter = counter
            .checked_add(1)
            .expect("secp256k1 scalar rejection 256 times running is impossible");
    }
}

// ============================================================================
// Identity commitments (A.4, A.5)
// ============================================================================

/// A.4: the self-certifying Community identity —
/// `sha256("concord/community" || owner_xonly || owner_salt)`. Forging a
/// different owner onto an existing id is a second-preimage on SHA-256.
pub fn community_id(owner_xonly: &[u8; 32], salt: &OwnerSalt) -> CommunityId {
    let mut h = Sha256::new();
    h.update(DOMAIN_COMMUNITY_ID);
    h.update(owner_xonly);
    h.update(salt.0);
    CommunityId(h.finalize().into())
}

/// Verify a claimed `(owner, salt)` pair against a `community_id`.
pub fn verify_owner(id: &CommunityId, owner_xonly: &[u8; 32], salt: &OwnerSalt) -> bool {
    community_id(owner_xonly, salt) == *id
}

/// A.5: the epoch-key continuity commitment a rekey's `prevcommit` tag carries
/// (CORD-06 §2) — proves a rotation extends the very key the receiver holds.
pub fn epoch_key_commitment(prev_epoch: Epoch, prev_key: &[u8; 32]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(DOMAIN_EPOCH_COMMITMENT);
    h.update(prev_epoch.0.to_be_bytes());
    h.update(prev_key);
    h.finalize().into()
}

// ============================================================================
// Plane addresses (group keys)
// ============================================================================

/// The Control Plane's group key (CORD-02 §5).
pub fn control_key(root: &CommunityRoot, id: &CommunityId, epoch: Epoch) -> GroupKey {
    group_key(LABEL_CONTROL, root.as_bytes(), &id.0, Some(epoch))
}

/// The Guestbook Plane's group key (CORD-02 §5).
pub fn guestbook_key(root: &CommunityRoot, id: &CommunityId, epoch: Epoch) -> GroupKey {
    group_key(LABEL_GUESTBOOK, root.as_bytes(), &id.0, Some(epoch))
}

/// A Public Channel's group key: derived from the `community_root` at the
/// root's epoch, so it needs no delivery and rotates with the base (CORD-03).
pub fn public_channel_key(root: &CommunityRoot, channel: &ChannelId, root_epoch: Epoch) -> GroupKey {
    group_key(LABEL_CHANNEL, root.as_bytes(), &channel.0, Some(root_epoch))
}

/// A Private Channel's group key: its own independent secret and epoch.
pub fn private_channel_key(key: &ChannelKey, channel: &ChannelId, epoch: Epoch) -> GroupKey {
    group_key(LABEL_CHANNEL, key.as_bytes(), &channel.0, Some(epoch))
}

/// A Private Channel rekey address for the epoch it *introduces*, keyed by the
/// prior `community_root` every member holds (CORD-06 §2).
pub fn rekey_address(prior_root: &CommunityRoot, channel: &ChannelId, new_epoch: Epoch) -> GroupKey {
    group_key(LABEL_REKEY_PSEUDONYM, prior_root.as_bytes(), &channel.0, Some(new_epoch))
}

/// A base-rotation rekey address, keyed by the prior root (CORD-06 §2).
pub fn base_rekey_address(prior_root: &CommunityRoot, id: &CommunityId, new_epoch: Epoch) -> GroupKey {
    group_key(LABEL_BASE_REKEY_PSEUDONYM, prior_root.as_bytes(), &id.0, Some(new_epoch))
}

/// The dissolution tombstone address (CORD-02 §9): derived from the
/// `community_id` alone — no key, no epoch — so every member past or present
/// resolves the same grave and a Refounding can never strand it.
pub fn dissolved_address(id: &CommunityId) -> GroupKey {
    group_key(LABEL_DISSOLVED, &id.0, &ZERO_ID, None)
}

// ============================================================================
// Coordinates (plain 32-byte hkdf outputs — edition `eid`s and locators)
// ============================================================================

/// A member's Grant coordinate (CORD-04): bound to the `community_id`, never a
/// key or epoch, so it survives every Refounding.
pub fn grant_locator(id: &CommunityId, member_xonly: &[u8; 32]) -> [u8; 32] {
    hkdf32(&id.0, &build_info(LABEL_GRANT, member_xonly, None, None))
}

/// The Banlist coordinate (CORD-04 §4).
pub fn banlist_locator(id: &CommunityId) -> [u8; 32] {
    hkdf32(&id.0, &build_info(LABEL_BANLIST, &ZERO_ID, None, None))
}

/// A creator's invite Registry coordinate (CORD-05 §5): bound to the creator,
/// so each creator owns exactly their own list.
pub fn invite_links_locator(id: &CommunityId, creator_xonly: &[u8; 32]) -> [u8; 32] {
    hkdf32(&id.0, &build_info(LABEL_INVITE_LINKS, creator_xonly, None, None))
}

/// The public-invite bundle decrypt key, derived from the link's 16-byte
/// off-network unlock token (CORD-05 §2) — the only thing the token derives.
pub fn invite_bundle_key(token: &[u8; 16]) -> [u8; 32] {
    hkdf32(token, &build_info(LABEL_INVITE_KEY, &ZERO_ID, None, None))
}

/// A rekey blob's per-recipient locator (CORD-06 §2):
/// `hkdf(rotator_xonly || recipient_xonly, "concord/recipient-pseudonym", scope_id, new_epoch)`.
/// Public inputs by design — a NIP-46 bunker finds its blob with no raw-key
/// access, and only key-holding members ever see the list to search it.
pub fn recipient_locator(
    rotator_xonly: &[u8; 32],
    recipient_xonly: &[u8; 32],
    scope: RekeyScope,
    new_epoch: Epoch,
) -> [u8; 32] {
    let mut ikm = [0u8; 64];
    ikm[..32].copy_from_slice(rotator_xonly);
    ikm[32..].copy_from_slice(recipient_xonly);
    hkdf32(&ikm, &build_info(LABEL_RECIPIENT_PSEUDONYM, &scope.id32(), Some(new_epoch), None))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex32(b: &[u8; 32]) -> String {
        crate::simd::hex::bytes_to_hex_32(b)
    }

    fn test_id() -> [u8; 32] {
        let mut id = [0u8; 32];
        for (i, b) in id.iter_mut().enumerate() {
            *b = (255 - i) as u8;
        }
        id
    }

    fn test_key() -> [u8; 32] {
        let mut k = [0u8; 32];
        for (i, b) in k.iter_mut().enumerate() {
            *b = i as u8;
        }
        k
    }

    // --- Golden vectors ---
    // Produced by an INDEPENDENT implementation (Python hashlib/hmac, RFC 5869)
    // over these exact bytes, so a match proves the construction cross-
    // implementation, not merely self-consistent. If any assertion here ever
    // changes, the wire format changed — a conscious, re-labeled decision.

    // ikm = 0x00..0x1f, label "concord/channel", id = 0xff,0xfe,.., epoch 0 / 1 / big-endian probe.
    const GOLDEN_CHANNEL_SEED_EPOCH0: &str =
        "1a99a5958bf9fcc5336e6e19db42aabf36ffbfa12f38a1d5fbde2ae383ed751b";
    const GOLDEN_CHANNEL_SEED_EPOCH1: &str =
        "4019ac9c2e15aba177749da7a0cfa59bacbac064bb76658628d9e23683717cad";
    const GOLDEN_CHANNEL_SEED_EPOCH_BE: &str =
        "016fc386d9d4137c99908870a269ee51e52306a2562b749f5eddcce5e657f7ee";
    // ikm = [7;32], label "concord/control", id = [0x11;32], epoch 1.
    const GOLDEN_CONTROL_SEED: &str =
        "e64e120fa7d2103c036fb21530d4dac3c5ee172d5656fc10781ab6fb43b8a49f";
    // ikm = [0x11;32] (community id), member = [0x22;32], no epoch.
    const GOLDEN_GRANT_LOCATOR: &str =
        "f25f8fabe256fc32c922a83df441b277ca88498c9169746ce3133eb1fb166a79";
    // ikm = [0x11;32], id = zeros, no epoch.
    const GOLDEN_BANLIST_LOCATOR: &str =
        "ace9cb9651e5d98827fa13783b150fb7c07ac9598c5632353cd5fe6ed2c0b4e2";
    // ikm = [0x11;32], creator = [0x22;32], no epoch.
    const GOLDEN_INVITE_LINKS_LOCATOR: &str =
        "9e9af9144a43ab8da9eb6abfd8b5d4d7e9d31a30024db4ac6c7488030f2eba67";
    // ikm = token [5;16], id = zeros, no epoch.
    const GOLDEN_INVITE_BUNDLE_KEY: &str =
        "d2b67ec14b1bcdbceab051b4cf4165f33f8e305b9fa10741dd8065b06c4f5988";
    // ikm = [0xAA;32] || [0xBB;32], scope = test_id channel, epoch 3.
    const GOLDEN_RECIPIENT_LOCATOR: &str =
        "e717fe03605f26174ef5df87cb39ef31bd4fefad1d713e41adbfbf3fb8c7458c";
    // sha256("concord/community" || [0x22;32] || [0x33;32]).
    const GOLDEN_COMMUNITY_ID: &str =
        "0f244078c710a20430f7cce317cc3a2b0a99614348ac5a3df2dea67741abe378";
    // sha256("concord/epoch-key-commitment" || be64(2) || [7;32]).
    const GOLDEN_EPOCH_COMMITMENT: &str =
        "550c4dfe037bc9d45b768ce5e0a4a0aae740f508bfe80d455a16d8cc19597876";
    // ikm = [0x11;32] (community id), label "concord/dissolved", id zeros, no epoch.
    const GOLDEN_DISSOLVED_SEED: &str =
        "37d70cd43a168e1bea76f38368f705123e83ee7f50560eb3ab2044b51902c977";

    #[test]
    fn channel_seed_goldens_pin_layout_and_endianness() {
        let seed0 = hkdf32(&test_key(), &build_info(LABEL_CHANNEL, &test_id(), Some(Epoch(0)), None));
        let seed1 = hkdf32(&test_key(), &build_info(LABEL_CHANNEL, &test_id(), Some(Epoch(1)), None));
        let seed_be = hkdf32(
            &test_key(),
            &build_info(LABEL_CHANNEL, &test_id(), Some(Epoch(0x0102030405060708)), None),
        );
        assert_eq!(hex32(&seed0), GOLDEN_CHANNEL_SEED_EPOCH0);
        assert_eq!(hex32(&seed1), GOLDEN_CHANNEL_SEED_EPOCH1);
        // A multi-byte epoch pins big-endian serialization explicitly.
        assert_eq!(hex32(&seed_be), GOLDEN_CHANNEL_SEED_EPOCH_BE);
    }

    #[test]
    fn control_seed_golden() {
        let seed = hkdf32(&[7u8; 32], &build_info(LABEL_CONTROL, &[0x11u8; 32], Some(Epoch(1)), None));
        assert_eq!(hex32(&seed), GOLDEN_CONTROL_SEED);
        // The seed is the group key's secret when it's a valid scalar (the
        // overwhelmingly common case) — pin that the public API agrees.
        let gk = control_key(&CommunityRoot([7u8; 32]), &CommunityId([0x11u8; 32]), Epoch(1));
        assert_eq!(gk.keys().secret_key().to_secret_hex(), GOLDEN_CONTROL_SEED);
    }

    #[test]
    fn coordinate_goldens() {
        let cid = CommunityId([0x11u8; 32]);
        assert_eq!(hex32(&grant_locator(&cid, &[0x22u8; 32])), GOLDEN_GRANT_LOCATOR);
        assert_eq!(hex32(&banlist_locator(&cid)), GOLDEN_BANLIST_LOCATOR);
        assert_eq!(hex32(&invite_links_locator(&cid, &[0x22u8; 32])), GOLDEN_INVITE_LINKS_LOCATOR);
        assert_eq!(hex32(&invite_bundle_key(&[5u8; 16])), GOLDEN_INVITE_BUNDLE_KEY);
    }

    #[test]
    fn recipient_locator_golden_and_scope_bound() {
        let loc = recipient_locator(&[0xAA; 32], &[0xBB; 32], RekeyScope::Channel(ChannelId(test_id())), Epoch(3));
        assert_eq!(hex32(&loc), GOLDEN_RECIPIENT_LOCATOR);
        // Base-rotation scope must not collide with a channel scope.
        let base = recipient_locator(&[0xAA; 32], &[0xBB; 32], RekeyScope::CommunityRoot, Epoch(3));
        assert_ne!(loc, base);
        // Direction matters: rotator||recipient is not recipient||rotator.
        let swapped = recipient_locator(&[0xBB; 32], &[0xAA; 32], RekeyScope::Channel(ChannelId(test_id())), Epoch(3));
        assert_ne!(loc, swapped);
    }

    #[test]
    fn community_id_golden_and_self_certifies() {
        let owner = [0x22u8; 32];
        let salt = OwnerSalt([0x33u8; 32]);
        let id = community_id(&owner, &salt);
        assert_eq!(id.to_hex(), GOLDEN_COMMUNITY_ID);
        assert!(verify_owner(&id, &owner, &salt));
        // A forged owner or salt fails the commitment.
        assert!(!verify_owner(&id, &[0x23u8; 32], &salt));
        assert!(!verify_owner(&id, &owner, &OwnerSalt([0x34u8; 32])));
    }

    #[test]
    fn epoch_commitment_golden() {
        assert_eq!(hex32(&epoch_key_commitment(Epoch(2), &[7u8; 32])), GOLDEN_EPOCH_COMMITMENT);
        // Epoch and key both bind.
        assert_ne!(epoch_key_commitment(Epoch(3), &[7u8; 32]), epoch_key_commitment(Epoch(2), &[7u8; 32]));
        assert_ne!(epoch_key_commitment(Epoch(2), &[8u8; 32]), epoch_key_commitment(Epoch(2), &[7u8; 32]));
    }

    #[test]
    fn dissolved_seed_golden_and_keyless() {
        let seed = hkdf32(&[0x11u8; 32], &build_info(LABEL_DISSOLVED, &ZERO_ID, None, None));
        assert_eq!(hex32(&seed), GOLDEN_DISSOLVED_SEED);
        let gk = dissolved_address(&CommunityId([0x11u8; 32]));
        assert_eq!(gk.keys().secret_key().to_secret_hex(), GOLDEN_DISSOLVED_SEED);
    }

    #[test]
    fn labels_domain_separate_shared_ikm() {
        // Control, guestbook, and base-rekey all key off the root with the
        // community id — separation rests entirely on the label.
        let root = CommunityRoot([7u8; 32]);
        let cid = CommunityId([0x11u8; 32]);
        let control = control_key(&root, &cid, Epoch(1)).public_key();
        let guestbook = guestbook_key(&root, &cid, Epoch(1)).public_key();
        let base_rekey = base_rekey_address(&root, &cid, Epoch(1)).public_key();
        assert_ne!(control, guestbook);
        assert_ne!(control, base_rekey);
        assert_ne!(guestbook, base_rekey);
        // Grant / banlist / invite-links share the community-id IKM.
        assert_ne!(grant_locator(&cid, &[0x22; 32]), invite_links_locator(&cid, &[0x22; 32]));
        assert_ne!(grant_locator(&cid, &ZERO_ID), banlist_locator(&cid));
    }

    #[test]
    fn epoch_rotates_every_address() {
        let root = CommunityRoot([7u8; 32]);
        let cid = CommunityId([0x11u8; 32]);
        let chan = ChannelId(test_id());
        assert_ne!(
            control_key(&root, &cid, Epoch(0)).public_key(),
            control_key(&root, &cid, Epoch(1)).public_key()
        );
        assert_ne!(
            public_channel_key(&root, &chan, Epoch(0)).public_key(),
            public_channel_key(&root, &chan, Epoch(1)).public_key()
        );
    }

    #[test]
    fn public_and_private_channel_keying_share_one_derivation() {
        // A Public Channel is "a Channel whose key derives from the root" —
        // same label, same layout, only the secret differs (CORD-03 §1).
        let secret = test_key();
        let chan = ChannelId(test_id());
        let as_root = public_channel_key(&CommunityRoot(secret), &chan, Epoch(0));
        let as_key = private_channel_key(&ChannelKey(secret), &chan, Epoch(0));
        assert_eq!(as_root.public_key(), as_key.public_key());
    }

    #[test]
    fn group_key_signs_and_self_decrypts() {
        // The conv key is the self-ECDH of the group secret — encrypt/decrypt
        // roundtrip through the nip44 string API used by the stream layer.
        let gk = control_key(&CommunityRoot([7u8; 32]), &CommunityId([0x11u8; 32]), Epoch(0));
        let ct = nostr_sdk::nips::nip44::encrypt(
            gk.keys().secret_key(),
            &gk.public_key(),
            "concord",
            nostr_sdk::nips::nip44::Version::V2,
        )
        .unwrap();
        let pt = nostr_sdk::nips::nip44::decrypt(gk.keys().secret_key(), &gk.public_key(), &ct).unwrap();
        assert_eq!(pt, "concord");
    }
}
