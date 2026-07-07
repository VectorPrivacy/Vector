//! Concord v2 key derivations — CORD-02 Appendix A. **FROZEN.**
//!
//! Everything v2 addresses on the wire derives from a Community secret through
//! one of the shapes below; changing any labeled byte re-addresses every prior
//! event ("a breaking change re-labels and becomes a different universe").
//! The layout is locked by the golden vectors in the test module — minted by an
//! independent implementation — treat those as the spec.
//!
//! Construction (A.1): `HKDF-SHA256(ikm=secret, salt=∅, info, L=32)` where
//! `info = utf8(label) || 0x00 || id[32] || epoch_be[8]?`
//!   - `id` is ALWAYS present: 32 raw bytes, all-zeroes where a label has no
//!     meaningful id.
//!   - the epoch (u64 big-endian) is the ONLY omittable field: labels marked
//!     no-epoch omit the 8 bytes entirely.
//!   - the `scalar_normalize` retry counter (A.3) appends AFTER whatever fields
//!     are present, starting at byte value 0. (v1's equivalent starts its retry
//!     byte at 1 — the two conventions differ only in a ~2⁻¹²⁸ branch, but v2
//!     follows the spec exactly.)
//!
//! These are DISTINCT from v1's `vector-community/v1/*` labels — the two
//! protocols are different address universes by construction. The one label the
//! specs share is the edition hash (`vector-community/v1/edition`,
//! `community::version::EDITION_LABEL`), which upstream froze verbatim.

use hkdf::Hkdf;
use nostr_sdk::nips::nip44::v2::ConversationKey;
use nostr_sdk::prelude::{Keys, PublicKey, SecretKey};
use sha2::{Digest, Sha256};

use super::super::{ChannelId, CommunityId, Epoch};

/// A.6 purpose labels. Part of the wire format — append, never edit or reuse.
const LABEL_CHANNEL: &str = "concord/channel";
const LABEL_CONTROL: &str = "concord/control";
const LABEL_REKEY_PSEUDONYM: &str = "concord/rekey-pseudonym";
const LABEL_BASE_REKEY_PSEUDONYM: &str = "concord/base-rekey-pseudonym";
const LABEL_RECIPIENT_PSEUDONYM: &str = "concord/recipient-pseudonym";
const LABEL_GUESTBOOK: &str = "concord/guestbook";
const LABEL_VOICE_SIGNER: &str = "concord/voice-signer";
const LABEL_VOICE_MEDIA: &str = "concord/voice-media";
const LABEL_VOICE_SENDER: &str = "concord/voice-sender";
const LABEL_DISSOLVED: &str = "concord/dissolved";
const LABEL_GRANT: &str = "concord/grant";
const LABEL_BANLIST: &str = "concord/banlist";
const LABEL_INVITE_LINKS: &str = "concord/invite-links";
const LABEL_INVITE_KEY: &str = "concord/invite-key";
/// A.4 community_id commitment prefix — plain SHA-256, NOT the hkdf shape.
const LABEL_COMMUNITY: &str = "concord/community";
/// A.5 epoch-key commitment prefix — plain SHA-256.
const LABEL_EPOCH_COMMITMENT: &str = "concord/epoch-key-commitment";

const ZERO32: [u8; 32] = [0u8; 32];

/// The size of a public-invite unlock token (CORD-05 §2) — 16 bytes in v2
/// (v1 tokens were 32).
pub const TOKEN_LEN: usize = 16;

/// Build the frozen A.1 `info` byte string. `epoch` is `None` for the no-epoch
/// labels (grant/banlist/invite-links/invite-key/dissolved/voice-sender).
fn build_info(label: &str, id32: &[u8; 32], epoch: Option<u64>) -> Vec<u8> {
    let mut info = Vec::with_capacity(label.len() + 1 + 32 + 8);
    info.extend_from_slice(label.as_bytes());
    info.push(0x00);
    info.extend_from_slice(id32);
    if let Some(e) = epoch {
        info.extend_from_slice(&e.to_be_bytes());
    }
    info
}

/// HKDF-SHA256 to 32 bytes with a zero-length salt (RFC 5869: identical PRK to
/// a 32-zero-byte salt under HMAC-SHA256). `ikm` length varies by caller: 32
/// for keys/ids, 64 for the recipient-locator pair, 16 for an invite token.
fn hkdf32(ikm: &[u8], info: &[u8]) -> [u8; 32] {
    let hk = Hkdf::<Sha256>::new(None, ikm);
    let mut okm = [0u8; 32];
    hk.expand(info, &mut okm)
        .expect("HKDF expand of 32 bytes is infallible");
    okm
}

/// A.3 `scalar_normalize`: reduce an hkdf seed to a valid secp256k1 secret key.
/// First attempt carries NO counter byte; on rejection append one incrementing
/// counter byte to the info and retry, the counter starting at 0. The reject
/// branch is ~2⁻¹²⁸ rare; the counter keeps it deterministic cross-impl.
fn hkdf_to_secret_key(ikm: &[u8], base_info: &[u8]) -> SecretKey {
    if let Ok(sk) = SecretKey::from_slice(&hkdf32(ikm, base_info)) {
        return sk;
    }
    for counter in 0u8..=255 {
        let mut info = base_info.to_vec();
        info.push(counter);
        if let Ok(sk) = SecretKey::from_slice(&hkdf32(ikm, &info)) {
            return sk;
        }
    }
    unreachable!("secp256k1 scalar rejection 257 times running is impossible")
}

/// A.2 `group_key` — a plane's stream keypair. The x-only pubkey is the on-wire
/// Stream address (the `authors` filter), the secret key signs the plane's
/// wraps, and the NIP-44 self-ECDH conversation key encrypts them. Only a
/// holder of the deriving secret can produce any of the three, so only members
/// can even *identify* a plane's traffic.
#[derive(Clone)]
pub struct GroupKey {
    keys: Keys,
    conv_key: ConversationKey,
}

impl GroupKey {
    fn derive(label: &str, secret: &[u8], id32: &[u8; 32], epoch: Option<u64>) -> Self {
        let info = build_info(label, id32, epoch);
        let sk = hkdf_to_secret_key(secret, &info);
        let keys = Keys::new(sk);
        let conv_key = ConversationKey::derive(keys.secret_key(), &keys.public_key())
            .expect("self-ECDH of a valid keypair cannot fail");
        GroupKey { keys, conv_key }
    }

    /// The Stream address (x-only pubkey) — what `authors` filters match.
    pub fn pk(&self) -> PublicKey {
        self.keys.public_key()
    }

    /// The Stream address as lowercase hex.
    pub fn pk_hex(&self) -> String {
        self.keys.public_key().to_hex()
    }

    /// The keypair that signs this plane's wraps.
    pub fn keys(&self) -> &Keys {
        &self.keys
    }

    /// The NIP-44 conversation key (self-ECDH) that encrypts this plane's wraps.
    pub fn conv_key(&self) -> &ConversationKey {
        &self.conv_key
    }
}

impl std::fmt::Debug for GroupKey {
    // No key material in logs — address only.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GroupKey").field("pk", &self.pk_hex()).finish()
    }
}

// ── Plane keys (CORD-02 §5, CORD-03 §1, CORD-06 §2) ─────────────────────────

/// A Channel's Chat Plane group key. `secret` is the `community_root` for a
/// Public Channel (at the root epoch) or the Channel's independent key for a
/// Private one (at its own channel epoch) — CORD-03 §1. The channel id in the
/// derivation gives every Channel a distinct address regardless of which secret
/// feeds it.
pub fn channel_group_key(secret: &[u8; 32], channel_id: &ChannelId, epoch: Epoch) -> GroupKey {
    GroupKey::derive(LABEL_CHANNEL, secret, &channel_id.0, Some(epoch.0))
}

/// The Control Plane's group key (community_root-keyed, community-id-bound).
pub fn control_group_key(community_root: &[u8; 32], community_id: &CommunityId, epoch: Epoch) -> GroupKey {
    GroupKey::derive(LABEL_CONTROL, community_root, &community_id.0, Some(epoch.0))
}

/// The Guestbook Plane's group key (community_root-keyed, community-id-bound).
pub fn guestbook_group_key(community_root: &[u8; 32], community_id: &CommunityId, epoch: Epoch) -> GroupKey {
    GroupKey::derive(LABEL_GUESTBOOK, community_root, &community_id.0, Some(epoch.0))
}

/// A private Channel's rekey address for `new_epoch`, keyed by the
/// community_root the receiver already holds (CORD-06 §2) — root-keyed, not
/// channel-keyed, so any member recovers any epoch's rekey directly (no
/// ratchet; epochs stay independently recoverable).
pub fn channel_rekey_group_key(root: &[u8; 32], channel_id: &ChannelId, new_epoch: Epoch) -> GroupKey {
    GroupKey::derive(LABEL_REKEY_PSEUDONYM, root, &channel_id.0, Some(new_epoch.0))
}

/// The base-rotation rekey address for `new_epoch`, keyed by the PRIOR
/// community_root — the base has no stable key above it, so the prior root is
/// the one handle every retained member holds through the rotation (CORD-06 §2/§3).
pub fn base_rekey_group_key(prior_root: &[u8; 32], community_id: &CommunityId, new_epoch: Epoch) -> GroupKey {
    GroupKey::derive(LABEL_BASE_REKEY_PSEUDONYM, prior_root, &community_id.0, Some(new_epoch.0))
}

/// The dissolution tombstone's group key — derived from the community_id ALONE
/// (no key, no epoch), so every member past or present resolves the same
/// address and a Refounding can never strand the grave (CORD-02 §9).
pub fn dissolved_group_key(community_id: &CommunityId) -> GroupKey {
    GroupKey::derive(LABEL_DISSOLVED, &community_id.0, &ZERO32, None)
}

// ── Voice sub-keys (CORD-07 §1/§3 — Vector defers voice; derivations frozen
//    now so the registry can't drift) ─────────────────────────────────────────

/// A voice Channel's SFU room keypair: `pk` IS the room name, `sk` signs token
/// grants. Same (secret, epoch) pair that addresses the Channel's Chat Plane,
/// so the room rolls exactly when the Channel's key does.
pub fn voice_group_key(secret: &[u8; 32], channel_id: &ChannelId, epoch: Epoch) -> GroupKey {
    GroupKey::derive(LABEL_VOICE_SIGNER, secret, &channel_id.0, Some(epoch.0))
}

/// A voice Channel's raw 32-byte media-encryption root — never feeds a cipher
/// directly, every publisher's per-sender frame key derives from it.
pub fn voice_media_key(secret: &[u8; 32], channel_id: &ChannelId, epoch: Epoch) -> [u8; 32] {
    hkdf32(secret, &build_info(LABEL_VOICE_MEDIA, &channel_id.0, Some(epoch.0)))
}

/// A publisher's per-sender frame key material:
/// `hkdf(voice_media_key, "concord/voice-sender", sha256(utf8(identity)))` —
/// epoch omitted, the media key already carries it. Distinct per-sender keys
/// partition the AEAD nonce domains.
pub fn voice_sender_key(media_key: &[u8; 32], identity: &str) -> [u8; 32] {
    let id: [u8; 32] = Sha256::digest(identity.as_bytes()).into();
    hkdf32(media_key, &build_info(LABEL_VOICE_SENDER, &id, None))
}

// ── Keyless coordinates (32-byte edition locators; community-id-bound so they
//    survive every Refounding — CORD-04 §1) ──────────────────────────────────

/// A member's Grant entity coordinate (the edition `eid`).
pub fn grant_locator(community_id: &CommunityId, member_xonly: &[u8; 32]) -> [u8; 32] {
    hkdf32(&community_id.0, &build_info(LABEL_GRANT, member_xonly, None))
}

/// The community-wide Banlist coordinate.
pub fn banlist_locator(community_id: &CommunityId) -> [u8; 32] {
    hkdf32(&community_id.0, &build_info(LABEL_BANLIST, &ZERO32, None))
}

/// A creator's invite-link Registry coordinate (CORD-05 §5) — bound to the
/// creator so each creator owns exactly their own list.
pub fn invite_links_locator(community_id: &CommunityId, creator_xonly: &[u8; 32]) -> [u8; 32] {
    hkdf32(&community_id.0, &build_info(LABEL_INVITE_LINKS, creator_xonly, None))
}

/// A rekey blob's per-recipient locator (CORD-06 §2):
/// `hkdf(rotator_xonly || recipient_xonly, "concord/recipient-pseudonym", scope_id, new_epoch)`.
///
/// Derived from PUBLIC inputs on purpose (full NIP-46 bunker parity, no raw-key
/// access) — which means a locator match proves NOTHING. It is a lookup index
/// only; authenticity rests on the rotator's seal + authority check and the
/// blob's bound plaintext (D1 security relocation — never port v1's
/// locator-match-⇒-authentic assumption).
pub fn recipient_locator(
    rotator_xonly: &[u8; 32],
    recipient_xonly: &[u8; 32],
    scope_id: &[u8; 32],
    new_epoch: Epoch,
) -> [u8; 32] {
    let mut ikm = [0u8; 64];
    ikm[..32].copy_from_slice(rotator_xonly);
    ikm[32..].copy_from_slice(recipient_xonly);
    hkdf32(&ikm, &build_info(LABEL_RECIPIENT_PSEUDONYM, scope_id, Some(new_epoch.0)))
}

/// The public-invite bundle decrypt key, derived from the link's 16-byte
/// unlock token alone (CORD-05 §2).
pub fn invite_bundle_key(token: &[u8; TOKEN_LEN]) -> [u8; 32] {
    hkdf32(token, &build_info(LABEL_INVITE_KEY, &ZERO32, None))
}

// ── A.4: the self-certifying community_id ────────────────────────────────────

/// `community_id = sha256("concord/community" || owner_xonly || owner_salt)` —
/// a plain SHA-256 commitment, NOT the hkdf shape. Ownership is a property of
/// the id itself: forging a different owner onto an existing id is a
/// second-preimage on SHA-256. (This is the root fix for the v1 forgeable
/// owner-attestation anchor.)
pub fn community_id_of(owner_xonly: &[u8; 32], owner_salt: &[u8; 32]) -> CommunityId {
    let mut h = Sha256::new();
    h.update(LABEL_COMMUNITY.as_bytes());
    h.update(owner_xonly);
    h.update(owner_salt);
    CommunityId(h.finalize().into())
}

/// Verify a claimed `(owner, salt)` pair reproduces `community_id`. Every
/// bundle, pointer, and rehydrate path MUST pass this before trusting a claimed
/// owner.
pub fn verify_community_id(community_id: &CommunityId, owner_xonly: &[u8; 32], owner_salt: &[u8; 32]) -> bool {
    community_id_of(owner_xonly, owner_salt) == *community_id
}

// ── A.5: the epoch-key commitment ────────────────────────────────────────────

/// `sha256("concord/epoch-key-commitment" || prev_epoch_be[8] || prev_key[32])`
/// — the `prevcommit` continuity check on every rekey (CORD-06 §2). A
/// convergence mechanism, never a secrecy one.
pub fn epoch_key_commitment(prev_epoch: Epoch, prev_key: &[u8; 32]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(LABEL_EPOCH_COMMITMENT.as_bytes());
    h.update(prev_epoch.0.to_be_bytes());
    h.update(prev_key);
    h.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Fixed test inputs. The golden hex below was produced by an INDEPENDENT
    // implementation (Python: hmac+hashlib RFC 5869 HKDF, and pure-integer
    // secp256k1 point math for the x-only pubkeys), so a match proves the
    // construction — including the secp keypair step — is correct
    // cross-implementation, not merely self-consistent. If any of these
    // assertions ever change, the wire format changed — that must be a
    // conscious, versioned decision.
    fn secret() -> [u8; 32] {
        // 0x00,0x01,..,0x1f
        let mut k = [0u8; 32];
        for (i, b) in k.iter_mut().enumerate() {
            *b = i as u8;
        }
        k
    }

    fn id32() -> [u8; 32] {
        // 0xff,0xfe,..,0xe0
        let mut id = [0u8; 32];
        for (i, b) in id.iter_mut().enumerate() {
            *b = (255 - i) as u8;
        }
        id
    }

    fn alt() -> [u8; 32] {
        [0x11u8; 32]
    }

    fn cid() -> CommunityId {
        CommunityId(id32())
    }

    fn chan() -> ChannelId {
        ChannelId(id32())
    }

    /// A multibyte epoch whose big-endian bytes are order-revealing.
    const EPOCH_MULTI: u64 = 0x0102030405060708;

    const GOLDEN_CHANNEL_E0_SEED: &str = "1a99a5958bf9fcc5336e6e19db42aabf36ffbfa12f38a1d5fbde2ae383ed751b";
    const GOLDEN_CHANNEL_E0_PK: &str = "7a5c5dff759a63f1fc2779864487432bae3d1ea72c4ffabd39f4c1fdaf62097a";
    const GOLDEN_CHANNEL_EMULTI_PK: &str = "f20c7d192cc87615d7341e86f38f85303f4708b40232d4fea521ab8217767391";
    const GOLDEN_CONTROL_E0_PK: &str = "c43df20bf4d6eeaea5149619662ffe9b211f31e11bb4a59f56b6e906f702d46f";
    const GOLDEN_GUESTBOOK_E0_PK: &str = "ad09de582026fa7a052db18bb5827fa24c15e929d59aadcc91efb8508f5368ad";
    const GOLDEN_CHANNEL_REKEY_E1_PK: &str = "7c55cdb957e9db2b4800d687b2a07d3f7066b1a35824a1e86ba871f55e87e8b5";
    const GOLDEN_BASE_REKEY_E1_PK: &str = "fb2fa44fba66ba15595f784255a1cb569531db8784432ac0e4fe838498dd9dea";
    const GOLDEN_DISSOLVED_PK: &str = "4d3d55d88fdf9d9c2089651e5cbb0dfa93b6b9b10cdcb2319b0dce1a1398096a";
    const GOLDEN_GRANT_LOCATOR: &str = "fd2f88cc7f1eb8d7d862c91dc22afe700c358d1845158b3f353b769ce4898e35";
    const GOLDEN_BANLIST_LOCATOR: &str = "88089214afae6d3c412fd817ada44d6df4d485a53565646471e74476397693c9";
    const GOLDEN_INVITE_LINKS_LOCATOR: &str = "f4ae29994165767bac23e8dce630f81b926d2c8aa150e5cbf0bdf75865e8379a";
    const GOLDEN_RECIPIENT_LOCATOR: &str = "342deb400e191f0f52c81f27600934552550beb85aa9bf169f02d0e7f826cf74";
    const GOLDEN_INVITE_KEY: &str = "94bf8b0d89e579ddaeccf8d9db3f5de5c86a1259c597f2560ff0120173bc5e1f";
    const GOLDEN_VOICE_MEDIA_E0: &str = "8ab5b935c5e17f156563860ae6263f3700bfd836c326f8b3d7082be2fbaef6a0";
    const GOLDEN_VOICE_SIGNER_E0_PK: &str = "7591f1306c265ee1dce6a07b72c76fadb3af13bf9ccce0d284cb3af6134211a1";
    const GOLDEN_VOICE_SENDER: &str = "9ce1c11a39ce16a84b72c2697724a39e5c41ec07f4d703dcb01241271837599e";
    const GOLDEN_COMMUNITY_ID: &str = "2b790bd59df98bdc52092b74ebd6933a89ef8eaeecc9030861cbdeae7c814c46";
    const GOLDEN_EPOCH_COMMITMENT: &str = "3e6d6a3c9973c16d1ca7c5602d36979927c55c21a7e2c840f883af3f047e80a4";

    fn hex(bytes: &[u8]) -> String {
        crate::simd::hex::bytes_to_hex_32(bytes.try_into().expect("32 bytes"))
    }

    #[test]
    fn channel_group_key_golden_vector() {
        let gk = channel_group_key(&secret(), &chan(), Epoch(0));
        // The hkdf seed is a valid scalar (overwhelming case), so sk == seed —
        // pinning both proves hkdf AND the secp keypair step.
        assert_eq!(hex(gk.keys().secret_key().as_secret_bytes()), GOLDEN_CHANNEL_E0_SEED);
        assert_eq!(gk.pk_hex(), GOLDEN_CHANNEL_E0_PK);
    }

    #[test]
    fn channel_group_key_golden_multibyte_epoch_is_big_endian() {
        let gk = channel_group_key(&secret(), &chan(), Epoch(EPOCH_MULTI));
        assert_eq!(gk.pk_hex(), GOLDEN_CHANNEL_EMULTI_PK);
    }

    #[test]
    fn control_group_key_golden_vector() {
        assert_eq!(control_group_key(&secret(), &cid(), Epoch(0)).pk_hex(), GOLDEN_CONTROL_E0_PK);
    }

    #[test]
    fn guestbook_group_key_golden_vector() {
        assert_eq!(guestbook_group_key(&secret(), &cid(), Epoch(0)).pk_hex(), GOLDEN_GUESTBOOK_E0_PK);
    }

    #[test]
    fn rekey_group_keys_golden_vectors() {
        assert_eq!(
            channel_rekey_group_key(&secret(), &chan(), Epoch(1)).pk_hex(),
            GOLDEN_CHANNEL_REKEY_E1_PK
        );
        assert_eq!(
            base_rekey_group_key(&secret(), &cid(), Epoch(1)).pk_hex(),
            GOLDEN_BASE_REKEY_E1_PK
        );
    }

    #[test]
    fn dissolved_group_key_golden_and_is_epoch_free() {
        assert_eq!(dissolved_group_key(&cid()).pk_hex(), GOLDEN_DISSOLVED_PK);
        // Epoch omission is real omission, not epoch=0: a manual derivation WITH
        // an epoch field of 0 must land elsewhere.
        let with_epoch = GroupKey::derive(LABEL_DISSOLVED, &cid().0, &ZERO32, Some(0));
        assert_ne!(with_epoch.pk_hex(), GOLDEN_DISSOLVED_PK);
    }

    #[test]
    fn locator_golden_vectors() {
        assert_eq!(hex(&grant_locator(&cid(), &alt())), GOLDEN_GRANT_LOCATOR);
        assert_eq!(hex(&banlist_locator(&cid())), GOLDEN_BANLIST_LOCATOR);
        assert_eq!(hex(&invite_links_locator(&cid(), &alt())), GOLDEN_INVITE_LINKS_LOCATOR);
        assert_eq!(
            hex(&recipient_locator(&secret(), &alt(), &id32(), Epoch(3))),
            GOLDEN_RECIPIENT_LOCATOR
        );
        assert_eq!(hex(&invite_bundle_key(&[0x07u8; TOKEN_LEN])), GOLDEN_INVITE_KEY);
    }

    #[test]
    fn voice_golden_vectors() {
        let media = voice_media_key(&secret(), &chan(), Epoch(0));
        assert_eq!(hex(&media), GOLDEN_VOICE_MEDIA_E0);
        assert_eq!(voice_group_key(&secret(), &chan(), Epoch(0)).pk_hex(), GOLDEN_VOICE_SIGNER_E0_PK);
        assert_eq!(
            hex(&voice_sender_key(&media, "00112233445566778899aabbccddeeff")),
            GOLDEN_VOICE_SENDER
        );
    }

    #[test]
    fn community_id_golden_and_verifies() {
        let id = community_id_of(&secret(), &alt());
        assert_eq!(hex(&id.0), GOLDEN_COMMUNITY_ID);
        assert!(verify_community_id(&id, &secret(), &alt()));
        // Wrong owner or wrong salt must fail the commitment.
        assert!(!verify_community_id(&id, &alt(), &alt()));
        assert!(!verify_community_id(&id, &secret(), &id32()));
    }

    #[test]
    fn epoch_key_commitment_golden_and_binds_both_inputs() {
        assert_eq!(hex(&epoch_key_commitment(Epoch(2), &secret())), GOLDEN_EPOCH_COMMITMENT);
        assert_ne!(hex(&epoch_key_commitment(Epoch(3), &secret())), GOLDEN_EPOCH_COMMITMENT);
        assert_ne!(hex(&epoch_key_commitment(Epoch(2), &alt())), GOLDEN_EPOCH_COMMITMENT);
    }

    #[test]
    fn labels_domain_separate_every_plane() {
        // One (secret, id, epoch) triple across every keyed label — all
        // addresses must be pairwise distinct.
        let pks = [
            channel_group_key(&secret(), &chan(), Epoch(0)).pk_hex(),
            control_group_key(&secret(), &cid(), Epoch(0)).pk_hex(),
            guestbook_group_key(&secret(), &cid(), Epoch(0)).pk_hex(),
            channel_rekey_group_key(&secret(), &chan(), Epoch(0)).pk_hex(),
            base_rekey_group_key(&secret(), &cid(), Epoch(0)).pk_hex(),
            voice_group_key(&secret(), &chan(), Epoch(0)).pk_hex(),
        ];
        let unique: std::collections::HashSet<_> = pks.iter().collect();
        assert_eq!(unique.len(), pks.len(), "two labels collided on one address");
    }

    #[test]
    fn epoch_rotates_every_keyed_address() {
        assert_ne!(
            channel_group_key(&secret(), &chan(), Epoch(0)).pk_hex(),
            channel_group_key(&secret(), &chan(), Epoch(1)).pk_hex()
        );
        assert_ne!(
            control_group_key(&secret(), &cid(), Epoch(0)).pk_hex(),
            control_group_key(&secret(), &cid(), Epoch(1)).pk_hex()
        );
        assert_ne!(
            guestbook_group_key(&secret(), &cid(), Epoch(0)).pk_hex(),
            guestbook_group_key(&secret(), &cid(), Epoch(1)).pk_hex()
        );
    }

    #[test]
    fn recipient_locator_binds_direction_scope_and_epoch() {
        let base = recipient_locator(&secret(), &alt(), &id32(), Epoch(1));
        // Rotator↔recipient direction matters (concatenation order).
        assert_ne!(recipient_locator(&alt(), &secret(), &id32(), Epoch(1)), base);
        assert_ne!(recipient_locator(&secret(), &alt(), &id32(), Epoch(2)), base);
        assert_ne!(recipient_locator(&secret(), &alt(), &ZERO32, Epoch(1)), base);
    }

    #[test]
    fn conv_key_is_deterministic_self_ecdh() {
        let a = channel_group_key(&secret(), &chan(), Epoch(0));
        let b = channel_group_key(&secret(), &chan(), Epoch(0));
        assert_eq!(a.conv_key().as_bytes(), b.conv_key().as_bytes());
    }
}
