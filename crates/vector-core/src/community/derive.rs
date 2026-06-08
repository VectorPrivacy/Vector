//! Key-derivation convention (GROUP_PROTOCOL.md) — FROZEN.
//!
//! Every HKDF use in the Community protocol funnels through here. Changing any
//! byte of the construction shifts every pseudonym and sub-key, orphaning all
//! prior events — a forced migration to avoid. The layout is
//! locked by the golden vectors in the test module; treat those as the spec.
//!
//! Construction: `HKDF-SHA256(IKM, salt=∅, info, L=32)`, where
//! `info = utf8(label) || 0x00 || id32 || epoch_be` —
//!   - `label`    : ASCII purpose string, no terminator
//!   - `0x00`     : single separator byte
//!   - `id32`     : raw 32-byte id (channel id, or scope id), never hex
//!   - `epoch_be` : the epoch as u64 big-endian (8 bytes); omitted where noted

use hkdf::Hkdf;
use sha2::Sha256;

use super::{ChannelId, ChannelKey, CommunityId, Epoch, Pseudonym, ServerRootKey};
use nostr_sdk::prelude::SecretKey;

/// Purpose labels. These strings are part of the wire format — append new
/// ones, never edit or reuse an existing one.
const LABEL_CHANNEL_PSEUDONYM: &str = "vector-community/v1/channel-pseudonym";
const LABEL_RECIPIENT_PSEUDONYM: &str = "vector-community/v1/recipient-pseudonym";
const LABEL_REKEY_PSEUDONYM: &str = "vector-community/v1/rekey-pseudonym";
const LABEL_BASE_REKEY_PSEUDONYM: &str = "vector-community/v1/base-rekey-pseudonym";
const LABEL_PUBLIC_INVITE_KEY: &str = "vector-community/v1/public-invite-key";
const LABEL_PUBLIC_INVITE_LOCATOR: &str = "vector-community/v1/public-invite-locator";
const LABEL_PUBLIC_INVITE_SIGNER: &str = "vector-community/v1/public-invite-signer";
const LABEL_BANLIST_LOCATOR: &str = "vector-community/v1/banlist-locator";
const LABEL_GRANT_LOCATOR: &str = "vector-community/v1/grant-locator";
const LABEL_INVITE_LINKS_LOCATOR: &str = "vector-community/v1/invite-links-locator";
const LABEL_DISSOLVED_LOCATOR: &str = "vector-community/v1/dissolved-locator";
const LABEL_DISSOLVED_PSEUDONYM: &str = "vector-community/v1/dissolved-pseudonym";
const LABEL_DISSOLVED_ENVELOPE: &str = "vector-community/v1/dissolved-envelope-key";

/// Opaque coordinate for the banlist entity, HKDF-derived from the **community id** — a STABLE
/// logical id that survives a server-root rotation, so a re-anchored banlist binds the same coordinate
/// at every epoch (re-anchoring). Member-computable (members hold the community id from their
/// invite), outsider-opaque (the id is never on the wire — the relay sees only the rotating
/// `control_pseudonym`), and the content stays server-root-encrypted, so privacy is unchanged.
pub fn banlist_locator(community_id: &CommunityId) -> [u8; 32] {
    hkdf_sha256_32(&community_id.0, LABEL_BANLIST_LOCATOR.as_bytes())
}

/// Opaque coordinate for the owner-dissolution tombstone (vsk=10), HKDF-derived from the **community
/// id** — STABLE across a server-root rotation, exactly like `banlist_locator`. This rotation-stability is
/// load-bearing for dissolution: a fresh joiner after a re-founding derives only the NEW epoch root, but
/// can still compute this community-scoped coordinate and discover the tombstone, so a dissolved community
/// can never look "alive" to anyone who can derive the community id. Member-computable, outsider-opaque.
pub fn dissolved_locator(community_id: &CommunityId) -> [u8; 32] {
    hkdf_sha256_32(&community_id.0, LABEL_DISSOLVED_LOCATOR.as_bytes())
}

/// Rotation-stable relay `#z` for the dissolution tombstone — community-id-derived (NOT the per-epoch
/// `control_pseudonym`), so ANY client that can derive the community id finds the tombstone at the SAME
/// coordinate regardless of which epoch root it holds. This is what closes the post-rotation
/// discoverability split: a fresh joiner who only ever derives a later epoch's root still probes this
/// fixed coordinate and learns the community is dead. Outsider-opaque (community id is never on the wire).
pub fn dissolved_pseudonym(community_id: &CommunityId) -> String {
    crate::simd::hex::bytes_to_hex_32(&hkdf_sha256_32(&community_id.0, LABEL_DISSOLVED_PSEUDONYM.as_bytes()))
}

/// Rotation-stable envelope key for the dissolution tombstone — community-id-derived so the tombstone is
/// openable by any member or joiner at ANY epoch. The control plane is server-root-encrypted (per-epoch),
/// which a post-rotation joiner can't open for the publish-epoch; the tombstone carries no secret (content
/// is `{}`), so a community-id key is the right scope — member-computable, outsider-opaque, epoch-free.
pub fn dissolved_envelope_key(community_id: &CommunityId) -> [u8; 32] {
    hkdf_sha256_32(&community_id.0, LABEL_DISSOLVED_ENVELOPE.as_bytes())
}

/// Opaque coordinate for a CREATOR's own invite-links entity (vsk=8) — the per-creator list of
/// active public-invite-link locators THEY published. Bound to the creator's x-only pubkey exactly like a
/// per-member grant (`grant_locator`), so a creator can only publish links at their own coordinate, and
/// members fold every creator's list into the aggregate active-set (`is_public` = aggregate non-empty).
/// Community-id-derived (stable across rotation, member-computable, outsider-opaque). There is no shared
/// registry — each creator owns only their own list (per-creator ownership).
pub fn invite_links_locator(community_id: &CommunityId, creator_xonly: &[u8; 32]) -> [u8; 32] {
    let info = build_info(LABEL_INVITE_LINKS_LOCATOR, creator_xonly, None);
    hkdf_sha256_32(&community_id.0, &info)
}

/// Opaque coordinate for a member's Grant entity (vsk=3), HKDF-derived from the **community id**
/// bound to the member's x-only pubkey. Community-scoped (not server-root-scoped) so the coordinate is
/// STABLE across a base rotation — the keystone that lets a re-anchored grant fold under the new root
/// (re-anchoring): a new joiner holding only the new root still derives the same `entity_id`.
/// Member-computable, outsider-opaque (community id never on the wire), content still server-root-
/// encrypted — privacy unchanged. Roles need no locator (their `d`-tag is the role's random id).
pub fn grant_locator(community_id: &CommunityId, member_xonly: &[u8; 32]) -> [u8; 32] {
    let info = build_info(LABEL_GRANT_LOCATOR, member_xonly, None);
    hkdf_sha256_32(&community_id.0, &info)
}

/// Scope of a per-recipient rekey blob. Disambiguates two blobs a single
/// sender delivers to the same recipient in one epoch (a server-root rotation and
/// a channel rekey), which would otherwise collide on the same tag.
#[derive(Debug, Clone, Copy)]
pub enum RekeyScope {
    /// A specific channel being rekeyed.
    Channel(ChannelId),
    /// A server-wide root rotation — not channel-scoped, uses the all-zero sentinel.
    ServerRoot,
}

impl RekeyScope {
    /// The 32-byte scope id this rekey binds: the channel id, or the all-zero server-root
    /// sentinel. Used by `recipient_pseudonym`, the epoch-keys archive scope, and the blob binding.
    pub fn id32(&self) -> [u8; 32] {
        match self {
            RekeyScope::Channel(c) => c.0,
            RekeyScope::ServerRoot => [0u8; 32],
        }
    }
}

/// Build the frozen `info` byte string. `epoch` is `None` for the no-epoch derivations
/// (the grant + invite-links locators and the public-invite sub-keys).
fn build_info(label: &str, id32: &[u8; 32], epoch: Option<Epoch>) -> Vec<u8> {
    let mut info = Vec::with_capacity(label.len() + 1 + 32 + 8);
    info.extend_from_slice(label.as_bytes());
    info.push(0x00);
    info.extend_from_slice(id32);
    if let Some(e) = epoch {
        info.extend_from_slice(&e.0.to_be_bytes());
    }
    info
}

/// HKDF-SHA256 expand to 32 bytes with an empty salt.
///
/// RFC 5869 with no salt uses HashLen zero bytes; the `hkdf` crate's `new(None, ..)`
/// does exactly that, and for HMAC-SHA256 a zero-length salt and a 32-zero-byte salt
/// produce an identical PRK (both pad to the 64-byte block), so this matches the
/// spec's "salt=∅". The expand never fails for L=32 (≤ 255·HashLen).
fn hkdf_sha256_32(ikm: &[u8; 32], info: &[u8]) -> [u8; 32] {
    let hk = Hkdf::<Sha256>::new(None, ikm);
    let mut okm = [0u8; 32];
    hk.expand(info, &mut okm)
        .expect("HKDF expand of 32 bytes is infallible");
    okm
}

/// Channel pseudonym: the value carried in the relay-filterable `z` tag.
/// Every member derives the same one from the shared channel secret, so it both
/// addresses and (by rotation) unlinks the channel's traffic.
pub fn channel_pseudonym(channel_key: &ChannelKey, channel_id: &ChannelId, epoch: Epoch) -> Pseudonym {
    let info = build_info(LABEL_CHANNEL_PSEUDONYM, &channel_id.0, Some(epoch));
    Pseudonym(hkdf_sha256_32(channel_key.as_bytes(), &info))
}

/// The relay-filterable address of a channel REKEY event for `(channel, epoch)`. Derived from
/// the **server-root key** (NOT the channel key) + the channel id + the epoch the rekey introduces.
/// Because the IKM is the server root — which every member always holds and which is stable across a
/// channel rotation — any member can compute this for ANY epoch directly, WITHOUT holding that epoch's
/// (or the prior epoch's) channel key. That is what makes epochs **independently recoverable**: a
/// member fetches the rekey for whichever epoch(s) they choose (latest only, or all, in parallel),
/// rather than chaining forward one key at a time. Distinct from the channel message pseudonym (channel
/// key IKM) and the control pseudonym (community-id binding) by IKM/id, and domain-separated by label.
pub fn rekey_pseudonym(server_root: &ServerRootKey, channel_id: &ChannelId, epoch: Epoch) -> Pseudonym {
    let info = build_info(LABEL_REKEY_PSEUDONYM, &channel_id.0, Some(epoch));
    Pseudonym(hkdf_sha256_32(server_root.as_bytes(), &info))
}

/// The relay-filterable address of a SERVER-ROOT (base) rekey for `(community, new_epoch)`.
/// Keyed by the **PRIOR** server-root key — the base layer has no stable key above it, so the prior
/// root is the handle every current member holds: a returning member derives this from the root they
/// currently hold, finds the base rekey, learns the rotator from its inner sig, and recovers the next
/// root (a short forward-walk; base rotations are rare). Binds the community id + epoch, and is
/// label-separated from the channel-rekey / channel-message / control pseudonyms.
pub fn base_rekey_pseudonym(prior_root: &ServerRootKey, community_id: &CommunityId, new_epoch: Epoch) -> Pseudonym {
    let info = build_info(LABEL_BASE_REKEY_PSEUDONYM, &community_id.0, Some(new_epoch));
    Pseudonym(hkdf_sha256_32(prior_root.as_bytes(), &info))
}

/// Per-recipient rekey-blob tag. `IKM` is the pairwise sender↔recipient
/// ECDH secret (not the channel key), so only that pair can locate the blob and a
/// removed member cannot derive tags for pairs they are not in.
pub fn recipient_pseudonym(per_recipient_secret: &[u8; 32], scope: RekeyScope, epoch: Epoch) -> Pseudonym {
    let info = build_info(LABEL_RECIPIENT_PSEUDONYM, &scope.id32(), Some(epoch));
    Pseudonym(hkdf_sha256_32(per_recipient_secret, &info))
}

/// Reduce HKDF output to a valid secp256k1 scalar with reject-and-retry (the reject
/// branch is ~2^-128 rare but kept deterministic via a counter byte appended to
/// `info`, so derivation stays reproducible cross-implementation).
fn hkdf_to_secret_key(ikm: &[u8; 32], base_info: Vec<u8>) -> SecretKey {
    let mut counter: u8 = 0;
    loop {
        let info = if counter == 0 {
            base_info.clone()
        } else {
            let mut extended = base_info.clone();
            extended.push(counter);
            extended
        };
        let okm = hkdf_sha256_32(ikm, &info);
        if let Ok(sk) = SecretKey::from_slice(&okm) {
            return sk;
        }
        counter = counter
            .checked_add(1)
            .expect("secp256k1 scalar rejection 256 times running is impossible");
    }
}

/// Public-invite sub-keys, all derived from the URL fetch-token. The token
/// is the IKM and there is no channel/epoch context, so the frozen `info` uses the
/// all-zero id and no epoch — the token alone provides uniqueness. The three labels
/// domain-separate the decryption key, the relay locator (addressable `d`-tag), and the
/// bundle's signing key (so the owner can re-post under one coordinate to rotate, and
/// joiners reject an impostor squatting the locator).
pub fn public_invite_key(token: &[u8; 32]) -> [u8; 32] {
    hkdf_sha256_32(token, &build_info(LABEL_PUBLIC_INVITE_KEY, &[0u8; 32], None))
}

pub fn public_invite_locator(token: &[u8; 32]) -> [u8; 32] {
    hkdf_sha256_32(token, &build_info(LABEL_PUBLIC_INVITE_LOCATOR, &[0u8; 32], None))
}

pub fn public_invite_signer(token: &[u8; 32]) -> SecretKey {
    hkdf_to_secret_key(token, build_info(LABEL_PUBLIC_INVITE_SIGNER, &[0u8; 32], None))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Fixed test inputs. The golden hex below was produced by an INDEPENDENT
    // HKDF-SHA256 implementation (Python hmac+hashlib, RFC 5869) over these exact
    // bytes, so a match proves the construction is correct cross-implementation,
    // not merely self-consistent. If any of these assertions ever change, the wire
    // format changed — that must be a conscious, versioned decision.
    fn test_channel_key() -> ChannelKey {
        // 0x00,0x01,..,0x1f
        let mut k = [0u8; 32];
        for (i, b) in k.iter_mut().enumerate() {
            *b = i as u8;
        }
        ChannelKey(k)
    }

    fn test_channel_id() -> ChannelId {
        // 0xff,0xfe,..
        let mut id = [0u8; 32];
        for (i, b) in id.iter_mut().enumerate() {
            *b = (255 - i) as u8;
        }
        ChannelId(id)
    }

    #[test]
    fn channel_pseudonym_is_deterministic() {
        let key = test_channel_key();
        let id = test_channel_id();
        let a = channel_pseudonym(&key, &id, Epoch(0));
        let b = channel_pseudonym(&key, &id, Epoch(0));
        assert_eq!(a, b, "same inputs must yield the same pseudonym");
    }

    #[test]
    fn channel_pseudonym_golden_epoch0() {
        let p = channel_pseudonym(&test_channel_key(), &test_channel_id(), Epoch(0));
        assert_eq!(p.to_hex(), GOLDEN_CHANNEL_PSEUDONYM_EPOCH0);
    }

    #[test]
    fn channel_pseudonym_golden_epoch1() {
        let p = channel_pseudonym(&test_channel_key(), &test_channel_id(), Epoch(1));
        assert_eq!(p.to_hex(), GOLDEN_CHANNEL_PSEUDONYM_EPOCH1);
    }

    // Independent (Python hmac+hashlib, RFC 5869) over IKM=0x11*32 (the COMMUNITY id), member=0x22*32,
    // info = "vector-community/v1/grant-locator" ‖ 0x00 ‖ member.
    const GOLDEN_GRANT_LOCATOR: &str =
        "c18d4d5955ecdd258f44240019a493a01fc01d51b5f0b8f7679ae424f8d5bfcc";

    #[test]
    fn grant_locator_golden() {
        let loc = grant_locator(&crate::community::CommunityId([0x11u8; 32]), &[0x22u8; 32]);
        assert_eq!(crate::simd::hex::bytes_to_hex_32(&loc), GOLDEN_GRANT_LOCATOR);
    }

    #[test]
    fn invite_links_locator_golden_and_domain_separated() {
        let cid = crate::community::CommunityId([0x11u8; 32]);
        let alice = [0x22u8; 32];
        let bob = [0x33u8; 32];
        // Frozen output (drift = a silent coordinate change → members lose a creator's links).
        assert_eq!(
            crate::simd::hex::bytes_to_hex_32(&invite_links_locator(&cid, &alice)),
            "cf42937a815ec561da6b4ca5ddd0c361634b0d9744693b744d4f5b34ec209ec2"
        );
        // Per-creator: each creator's list lives at a DISTINCT coordinate (no shared registry).
        assert_ne!(invite_links_locator(&cid, &alice), invite_links_locator(&cid, &bob));
        // Domain-separated from grant + banlist despite sharing the community-id IKM (distinct label).
        assert_ne!(invite_links_locator(&cid, &alice), grant_locator(&cid, &alice));
        assert_ne!(invite_links_locator(&cid, &alice), banlist_locator(&cid));
        // Community-bound (a different community → a different coordinate).
        assert_ne!(invite_links_locator(&cid, &alice), invite_links_locator(&crate::community::CommunityId([0x99u8; 32]), &alice));
    }

    #[test]
    fn grant_locator_binds_member_and_community() {
        let cid = crate::community::CommunityId([0x11u8; 32]);
        // Deterministic for the same inputs.
        assert_eq!(grant_locator(&cid, &[0x22u8; 32]), grant_locator(&cid, &[0x22u8; 32]));
        // The member pubkey is bound in: a different member → a different locator.
        assert_ne!(grant_locator(&cid, &[0x22u8; 32]), grant_locator(&cid, &[0x23u8; 32]));
        // A different COMMUNITY → a different locator (so coordinates don't collide across communities,
        // and an outsider without the community id can't compute any).
        assert_ne!(
            grant_locator(&cid, &[0x22u8; 32]),
            grant_locator(&crate::community::CommunityId([0x99u8; 32]), &[0x22u8; 32])
        );
    }

    #[test]
    fn epoch_changes_the_pseudonym() {
        let key = test_channel_key();
        let id = test_channel_id();
        assert_ne!(
            channel_pseudonym(&key, &id, Epoch(0)),
            channel_pseudonym(&key, &id, Epoch(1)),
            "rotating the epoch must rotate the pseudonym (unlinkability)"
        );
    }

    #[test]
    fn different_channel_id_changes_the_pseudonym() {
        let key = test_channel_key();
        let other = ChannelId([0x42u8; 32]);
        assert_ne!(
            channel_pseudonym(&key, &test_channel_id(), Epoch(0)),
            channel_pseudonym(&key, &other, Epoch(0)),
        );
    }

    #[test]
    fn different_label_does_not_collide() {
        // Channel pseudonym and recipient pseudonym share IKM-shape + id + epoch but
        // differ only by label — domain separation must keep them distinct.
        let secret = test_channel_key();
        let id = test_channel_id();
        let chan = channel_pseudonym(&secret, &id, Epoch(0));
        let recip = recipient_pseudonym(secret.as_bytes(), RekeyScope::Channel(id), Epoch(0));
        assert_ne!(chan.0, recip.0, "labels must domain-separate");
    }

    #[test]
    fn recipient_pseudonym_golden() {
        let secret = [7u8; 32];
        let chan = recipient_pseudonym(&secret, RekeyScope::Channel(test_channel_id()), Epoch(3));
        let root = recipient_pseudonym(&secret, RekeyScope::ServerRoot, Epoch(3));
        assert_eq!(chan.to_hex(), GOLDEN_RECIPIENT_CHANNEL_EPOCH3);
        assert_eq!(root.to_hex(), GOLDEN_RECIPIENT_SERVERROOT_EPOCH3);
    }

    #[test]
    fn rekey_pseudonym_is_server_root_derived_and_distinct() {
        let sr = ServerRootKey([0x07u8; 32]);
        let chan = test_channel_id();
        // Deterministic + golden (regression pin for the channel-rekey address derivation).
        let p = rekey_pseudonym(&sr, &chan, Epoch(1));
        assert_eq!(p, rekey_pseudonym(&sr, &chan, Epoch(1)));
        assert_eq!(p.to_hex(), GOLDEN_REKEY_PSEUDONYM);
        // Server-root-derived: a different root → different address (so a non-member can't compute it,
        // and crucially a member needs ONLY the server root — not the channel key — to find it).
        assert_ne!(p, rekey_pseudonym(&ServerRootKey([0x08u8; 32]), &chan, Epoch(1)));
        // Per-epoch + per-channel binding.
        assert_ne!(p, rekey_pseudonym(&sr, &chan, Epoch(2)));
        assert_ne!(p, rekey_pseudonym(&sr, &ChannelId([0x42u8; 32]), Epoch(1)));
        // Domain-separated from the channel message pseudonym even with the same (id, epoch): the
        // message pseudonym keys off the CHANNEL key, this off the SERVER ROOT + a different label.
        let as_chan_key = channel_pseudonym(&ChannelKey(*sr.as_bytes()), &chan, Epoch(1));
        assert_ne!(p.0, as_chan_key.0, "label must domain-separate rekey-address from channel-message");
        // The subtle pairing: rekey vs control plane share IKM=server_root AND epoch — separation rests
        // ENTIRELY on the label (and id namespace). Pin it so a future label edit can't collapse them.
        let as_control =
            crate::community::roster::control_pseudonym(&sr, &crate::community::CommunityId(chan.0), Epoch(1));
        assert_ne!(p.to_hex(), as_control, "label must domain-separate rekey-address from control-plane");
    }

    #[test]
    fn base_rekey_pseudonym_is_prior_root_derived_and_distinct() {
        let root = ServerRootKey([0x07u8; 32]);
        let community = crate::community::CommunityId([0x09u8; 32]);
        let p = base_rekey_pseudonym(&root, &community, Epoch(1));
        assert_eq!(p, base_rekey_pseudonym(&root, &community, Epoch(1)));
        assert_eq!(p.to_hex(), GOLDEN_BASE_REKEY_PSEUDONYM);
        // Keyed by the PRIOR root: a different root → different address (so a member needs the root they
        // hold to find the next base rekey — the forward-walk handle).
        assert_ne!(p, base_rekey_pseudonym(&ServerRootKey([0x08u8; 32]), &community, Epoch(1)));
        // Per-epoch + per-community binding.
        assert_ne!(p, base_rekey_pseudonym(&root, &community, Epoch(2)));
        assert_ne!(p, base_rekey_pseudonym(&root, &crate::community::CommunityId([0x42u8; 32]), Epoch(1)));
        // Distinct from the control pseudonym (same IKM=root + community id + epoch) by label.
        let control = super::super::roster::control_pseudonym(&root, &community, Epoch(1));
        assert_ne!(p.to_hex(), control, "label must domain-separate base-rekey from control-plane");
    }

    #[test]
    fn server_root_scope_sentinel_matches_rekey_scope() {
        // The epoch-keys archive scopes the base key under `SERVER_ROOT_SCOPE_HEX`; it must equal the
        // hex of `RekeyScope::ServerRoot`'s all-zero `id32`, so the storage layer and the recipient
        // pseudonym name the same server-root scope. Pinning this stops the two from drifting apart.
        assert_eq!(
            crate::simd::hex::bytes_to_hex_32(&RekeyScope::ServerRoot.id32()),
            crate::community::SERVER_ROOT_SCOPE_HEX
        );
    }

    #[test]
    fn recipient_scope_disambiguates() {
        // Same sender, same recipient, same epoch, but a channel rekey vs a
        // server-root rotation must land on different tags (no blob collision).
        let secret = [7u8; 32];
        let chan = recipient_pseudonym(&secret, RekeyScope::Channel(test_channel_id()), Epoch(3));
        let root = recipient_pseudonym(&secret, RekeyScope::ServerRoot, Epoch(3));
        assert_ne!(chan.0, root.0);
    }

    #[test]
    fn channel_pseudonym_golden_multibyte_epoch_is_big_endian() {
        // A multi-byte epoch pins big-endian serialization explicitly (epoch 0/1
        // alone could be satisfied by either order beyond the low byte).
        let p = channel_pseudonym(&test_channel_key(), &test_channel_id(), Epoch(0x0102030405060708));
        assert_eq!(p.to_hex(), GOLDEN_CHANNEL_PSEUDONYM_EPOCH_BE);
    }

    #[test]
    fn public_invite_subkeys_golden() {
        // Independent RFC-5869 HKDF over token=[5;32], each label, all-zero id, no epoch.
        let token = [5u8; 32];
        assert_eq!(crate::simd::hex::bytes_to_hex_32(&public_invite_key(&token)), GOLDEN_PUBLIC_INVITE_KEY);
        assert_eq!(crate::simd::hex::bytes_to_hex_32(&public_invite_locator(&token)), GOLDEN_PUBLIC_INVITE_LOCATOR);
        assert_eq!(public_invite_signer(&token).to_secret_hex(), GOLDEN_PUBLIC_INVITE_SIGNER);
    }

    #[test]
    fn public_invite_subkeys_domain_separated_and_token_bound() {
        let token = [5u8; 32];
        let other = [6u8; 32];
        // Three sub-keys from one token must all differ (domain separation).
        assert_ne!(public_invite_key(&token), public_invite_locator(&token));
        assert_ne!(
            public_invite_key(&token).to_vec(),
            public_invite_signer(&token).as_secret_bytes().to_vec()
        );
        // A different token yields different sub-keys (token-bound).
        assert_ne!(public_invite_key(&token), public_invite_key(&other));
        assert_ne!(public_invite_locator(&token), public_invite_locator(&other));
    }

    // --- Golden vectors (independent Python HKDF-SHA256, RFC 5869) ---
    const GOLDEN_PUBLIC_INVITE_KEY: &str =
        "7f02a8a832a1744adf286676038446dc94762c2c8332650c9ad62a0c870e0751";
    const GOLDEN_PUBLIC_INVITE_LOCATOR: &str =
        "33c098d6e4cddc2b8ee98ab6b5182186794c35f5b71391130a49ae3d88588c2c";
    const GOLDEN_PUBLIC_INVITE_SIGNER: &str =
        "9154a3a7e4a03e94eaad2f76efeebd43e25ee9df4fbca12454edcee0ef666e8d";

    // server_root = [7;32], channel id = test_channel_id (0xff,0xfe,..), epoch 1.
    const GOLDEN_REKEY_PSEUDONYM: &str =
        "3a848655f79a586510e1113131f078aa1ce0ff8dcb74374507e6af07ff49fd24";
    // prior_root = [7;32], community id = [9;32], epoch 1.
    const GOLDEN_BASE_REKEY_PSEUDONYM: &str =
        "23ced8fd6cad30a21ded43c96bd040311cf20bcfff935453dc0985b41ff660be";

    const GOLDEN_CHANNEL_PSEUDONYM_EPOCH0: &str =
        "d55b9f5fad668887d41d46b7c08ba63725a39d7c86b602c7c36e2f2e0eff8c40";
    const GOLDEN_CHANNEL_PSEUDONYM_EPOCH1: &str =
        "050079d9899c85bebf5c73fd777cdd812132d262e3ceec83c847a056dea41293";
    // secret = [7;32], epoch 3; channel scope = test_channel_id, root scope = all-zero.
    const GOLDEN_RECIPIENT_CHANNEL_EPOCH3: &str =
        "971f69d6a948c79704f8077188cded86bd35c82960e88043ebb2c2c3d60a3b71";
    const GOLDEN_RECIPIENT_SERVERROOT_EPOCH3: &str =
        "e50e5d803fd2edc310be8cd7354586d12fcb8e3f30162553be53da1a34a17c46";
    // channel key [0..31], epoch 0x0102030405060708 (proves u64 big-endian).
    const GOLDEN_CHANNEL_PSEUDONYM_EPOCH_BE: &str =
        "cec398094d17688cd127bc609d34fa067331427400b023d0c70ff77fafe17e0b";
}
