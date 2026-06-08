//! Rekey blob primitive (GROUP_PROTOCOL.md).
//!
//! A rotation (a Private removal, a re-founding, or a scheduled rekey) mints a fresh-random key for
//! the next epoch and delivers it to every member who STAYS, one **per-recipient blob** each.
//! This module is that atom — the located, wrapped single blob — built and opened in isolation. The
//! 3303 event that carries a *set* of these blobs, and the apply/recipient-set logic, sit on top
//! (later sub-pieces).
//!
//! Each blob is:
//! - **located** by `recipient_pseudonym(pairwise_secret, scope, epoch)`: an opaque tag only the
//!   sender↔recipient pair can compute, so a recipient jumps straight to their own blob (no
//! trial-decryption) and a removed member can't even find a slot for a pair they're not in; and
//! - **wrapped** under the same pairwise secret via NIP-44 v2 (`cipher`), so only that recipient
//!   decrypts it.
//!
//! The `pairwise_secret` is the NIP-44 v2 ConversationKey between the two identities — the ECDH-derived
//! secret names. It is **symmetric**: the sender derives it from `(their sk, recipient pk)`, the
//! recipient recomputes the identical secret from `(their sk, sender pk)`. (How the recipient learns
//! the sender's pubkey is the 3303-envelope layer's job, not this atom's — `open_rekey_blob` takes it
//! as a parameter.)
//!
//! The wrapped plaintext **binds `(scope, epoch)`** so a blob can't be spliced into a different
//! coordinate: even though only the authorized rotator can mint blobs, the binding makes a
//! cross-scope/epoch reuse fail closed on open, the same discipline as the message envelope.

use nostr_sdk::nips::nip44::v2::ConversationKey;
use nostr_sdk::prelude::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use super::cipher;
use super::derive::{base_rekey_pseudonym, recipient_pseudonym, rekey_pseudonym, RekeyScope};
use super::{ChannelId, CommunityId, Epoch, Pseudonym, ServerRootKey};
use crate::stored_event::event_kind;

/// One located, wrapped rekey blob — the unit a 3303 Rekey event carries N of. `locator` is the
/// recipient-pseudonym hex (where the recipient finds it); `wrapped` is the base64 NIP-44 ciphertext
/// of `scope_id ‖ epoch ‖ new_key`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RekeyBlob {
    pub locator: String,
    pub wrapped: String,
}

/// The pairwise sender↔recipient secret: the NIP-44 v2 ConversationKey, HKDF-extracted from
/// the ECDH shared point. Symmetric — `pairwise_secret(a_sk, b_pk) == pairwise_secret(b_sk, a_pk)` —
/// so the recipient recomputes exactly what the sender used, for BOTH the locator and the wrap key.
pub fn rekey_pairwise_secret(sk: &SecretKey, pk: &PublicKey) -> Result<[u8; 32], String> {
    let ck = ConversationKey::derive(sk, pk).map_err(|e| format!("pairwise ECDH: {e}"))?;
    let bytes = ck.as_bytes();
    bytes
        .try_into()
        .map_err(|_| format!("conversation key is {} bytes, expected 32", bytes.len()))
}

/// The 72-byte bound plaintext a blob wraps: `scope_id[32] ‖ epoch_be[8] ‖ new_key[32]`. Fixed-width
/// fields, so no separators/length prefixes are needed to be unambiguous.
fn bound_plaintext(scope: RekeyScope, epoch: Epoch, new_key: &[u8; 32]) -> Vec<u8> {
    let mut pt = Vec::with_capacity(72);
    pt.extend_from_slice(&scope.id32());
    pt.extend_from_slice(&epoch.0.to_be_bytes());
    pt.extend_from_slice(new_key);
    pt
}

/// Build one rekey blob: the fresh `new_key` for `(scope, epoch)`, located + wrapped to `recipient_pk`.
/// `sender_sk` is the rotator's identity secret (the ECDH half the recipient pairs against).
pub fn build_rekey_blob(
    sender_sk: &SecretKey,
    recipient_pk: &PublicKey,
    scope: RekeyScope,
    epoch: Epoch,
    new_key: &[u8; 32],
) -> Result<RekeyBlob, String> {
    // Zeroized intermediates: both the pairwise secret and the plaintext (which holds the fresh epoch
    // key in bytes 40..72) are wiped on drop, so this atom leaks no key material into freed memory.
    let secret = Zeroizing::new(rekey_pairwise_secret(sender_sk, recipient_pk)?);
    let locator = recipient_pseudonym(&secret, scope, epoch).to_hex();
    let pt = Zeroizing::new(bound_plaintext(scope, epoch, new_key));
    let wrapped = cipher::seal(&secret, &pt).map_err(|e| format!("wrap rekey blob: {e}"))?;
    Ok(RekeyBlob { locator, wrapped })
}

/// Open a blob addressed to me: recompute the pairwise secret from `(my_sk, sender_pk)`, confirm the
/// blob's `locator` is the one THIS pair+scope+epoch produces (rejects a blob handed to us under the
/// wrong coordinate), decrypt, and verify the wrapped plaintext binds the SAME `(scope, epoch)` before
/// returning the new key. Any mismatch (wrong sender, wrong coordinate, tamper, splice) is `Err`.
pub fn open_rekey_blob(
    my_sk: &SecretKey,
    sender_pk: &PublicKey,
    scope: RekeyScope,
    epoch: Epoch,
    blob: &RekeyBlob,
) -> Result<[u8; 32], String> {
    let secret = Zeroizing::new(rekey_pairwise_secret(my_sk, sender_pk)?);
    // The locator is unforgeable without the pairwise secret, so a matching one proves the blob was
    // minted for this exact (pair, scope, epoch) — reject anything else rather than trust placement.
    let expected = recipient_pseudonym(&secret, scope, epoch).to_hex();
    if blob.locator != expected {
        return Err("rekey blob locator does not match this recipient/scope/epoch".to_string());
    }
    let pt = Zeroizing::new(cipher::open(&secret, &blob.wrapped).map_err(|e| format!("open rekey blob: {e}"))?);
    if pt.len() != 72 {
        return Err(format!("rekey blob plaintext is {} bytes, expected 72", pt.len()));
    }
    // Strict-equality binding (discipline): the wrapped scope+epoch must equal what we opened
    // under, so a blob can't be lifted from one coordinate into another.
    if pt[..32] != scope.id32() {
        return Err("rekey blob scope binding mismatch (splice)".to_string());
    }
    let mut epoch_be = [0u8; 8];
    epoch_be.copy_from_slice(&pt[32..40]);
    if u64::from_be_bytes(epoch_be) != epoch.0 {
        return Err("rekey blob epoch binding mismatch (splice)".to_string());
    }
    let mut new_key = [0u8; 32];
    new_key.copy_from_slice(&pt[40..72]);
    Ok(new_key)
}

// --- The 3303 Rekey event (carries N blobs) -----------------------------------------------------

/// Outer protocol-version tag (same discipline as the message envelope) — checked before decrypt.
const PROTOCOL_VERSION: &str = "1";
const TAG_VERSION: &str = "v";
const TAG_SCOPE: &str = "scope";
const TAG_NEW_EPOCH: &str = "newepoch";
const TAG_PREV_EPOCH: &str = "prevepoch";
const TAG_PREV_COMMIT: &str = "prevcommit";

/// strfry's default `maxEventSize` (the NIP-11 `max_event_size` the common relay enforces) — the basis
/// for [`MAX_REKEY_BLOBS`], asserted by the size-guard test. Test-only (the cap encodes the conclusion).
#[cfg(test)]
const STRFRY_MAX_EVENT_SIZE: usize = 65536;

/// Max recipients (blobs) in ONE Rekey event. MEASURED against [`STRFRY_MAX_EVENT_SIZE`]: 126 blobs
/// serialize to 55,131 bytes (the NIP-44 padding bucket), 127 jumps to 66,055 (over 64KB). So 126 is the
/// hard ceiling; we cap at **120** — slightly under, in the same 55KB bucket (~10KB margin under 64KB)
/// plus count-headroom so a future blob-format tweak can't silently tip a full event over the limit
/// (the `max_rekey_blobs_event_stays_under_relay_size_limit` test guards this). A rotation whose recipient
/// set exceeds this must SPLIT across events (— deferred); the send side fails closed at this cap, and
/// the receiver rejects a larger array (checked AFTER `cipher::open`, so a hostile array is also
/// relay-size-bounded). `pub(crate)` so the send side fails closed BEFORE publishing an unacceptable event.
pub(crate) const MAX_REKEY_BLOBS: usize = 120;

/// A commitment to the prior epoch's key (fork detection): a Rekey references it so two managers
/// who both rotate epoch N→N+1 produce a *detectable* fork (resolved by authority-first→time→id),
/// and so a recipient who holds the prior key can confirm the rotator did too (a legitimacy check).
/// Domain-separated SHA-256 over `prev_epoch_be ‖ prev_key`; the epoch binds the commitment to the
/// specific link in the chain.
pub fn epoch_key_commitment(prev_epoch: Epoch, prev_key: &[u8; 32]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(b"vector-community/v1/epoch-key-commitment");
    h.update(prev_epoch.0.to_be_bytes());
    h.update(prev_key);
    h.finalize().into()
}

/// A parsed, inner-signature-verified Rekey — what `open_rekey_event` yields. Authority (does the
/// rotator's roster rank permit this rotation?) and blob-opening are SEPARATE later steps: the
/// `rotator` pubkey here is what the recipient pairs against in [`open_rekey_blob`], and what the
/// roster check is run against.
#[derive(Debug, Clone)]
pub struct ParsedRekey {
    /// The verified rotator (the inner event's real author) — the ECDH identity + the authority actor.
    pub rotator: PublicKey,
    /// The scope this rekey rotates (a channel, or the server root).
    pub scope: RekeyScope,
    /// The epoch this rekey introduces.
    pub new_epoch: Epoch,
    /// The epoch being rotated FROM (the chain link this extends).
    pub prev_epoch: Epoch,
    /// Commitment to the prior epoch's key (fork detection); verified against the held prior key
    /// by the apply path, not here.
    pub prev_key_commitment: [u8; 32],
    /// The per-recipient blobs; the recipient finds theirs by locator and opens it (`open_rekey_blob`).
    pub blobs: Vec<RekeyBlob>,
}

/// Encode a [`RekeyScope`] for the signed `scope` tag: the 32-byte scope id as hex (channel id, or the
/// all-zero server-root sentinel). A channel id is random-32, so all-zero unambiguously means server
/// root (same non-collision argument as [`crate::community::SERVER_ROOT_SCOPE_HEX`]).
fn scope_to_hex(scope: RekeyScope) -> String {
    crate::simd::hex::bytes_to_hex_32(&scope.id32())
}

fn scope_from_hex(hex: &str) -> Option<RekeyScope> {
    if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let bytes = crate::simd::hex::hex_to_bytes_32(hex);
    Some(if bytes == [0u8; 32] {
        RekeyScope::ServerRoot
    } else {
        RekeyScope::Channel(ChannelId(bytes))
    })
}

/// Build the rotator-signed INNER rekey event (shared by channel + server-root rekeys): kind 3303,
/// real-npub sig, carrying the scope / epochs / prior-key commitment / blobs. Enforces the 
/// monotonic-epoch invariant at mint (fail closed — a non-advancing epoch would otherwise surface only
/// downstream as a spurious fork). The caller seals it under the appropriate envelope key + address.
fn build_rekey_inner(
    rotator: &Keys,
    scope: RekeyScope,
    new_epoch: Epoch,
    prev_epoch: Epoch,
    prev_key_commitment: &[u8; 32],
    blobs: &[RekeyBlob],
) -> Result<Event, String> {
    if new_epoch.0 <= prev_epoch.0 {
        return Err(format!("rekey new_epoch {} must exceed prev_epoch {}", new_epoch.0, prev_epoch.0));
    }
    let blobs_json = serde_json::to_string(blobs).map_err(|e| format!("serialize blobs: {e}"))?;
    EventBuilder::new(Kind::Custom(event_kind::COMMUNITY_REKEY), blobs_json)
        .tags([
            Tag::custom(TagKind::Custom(TAG_SCOPE.into()), [scope_to_hex(scope)]),
            Tag::custom(TagKind::Custom(TAG_NEW_EPOCH.into()), [new_epoch.0.to_string()]),
            Tag::custom(TagKind::Custom(TAG_PREV_EPOCH.into()), [prev_epoch.0.to_string()]),
            Tag::custom(TagKind::Custom(TAG_PREV_COMMIT.into()), [crate::simd::hex::bytes_to_hex_32(prev_key_commitment)]),
        ])
        .sign_with_keys(rotator)
        .map_err(|e| format!("sign rekey inner: {e}"))
}

/// Seal a signed inner rekey into the ephemeral-signed outer: encrypt under `envelope_key`, address by
/// `address` (the `z` pseudonym a member fetches it by). `open_rekey_event(outer, envelope_key)` is the
/// inverse for both rekey kinds.
fn seal_rekey_outer(ephemeral: &Keys, inner: &Event, envelope_key: &[u8; 32], address: &Pseudonym) -> Result<Event, String> {
    let content = cipher::seal(envelope_key, inner.as_json().as_bytes()).map_err(|e| format!("seal rekey: {e}"))?;
    EventBuilder::new(Kind::Custom(event_kind::COMMUNITY_REKEY), content)
        .tags([
            Tag::custom(TagKind::SingleLetter(SingleLetterTag::lowercase(Alphabet::Z)), [address.to_hex()]),
            Tag::custom(TagKind::Custom(TAG_VERSION.into()), [PROTOCOL_VERSION.to_string()]),
        ])
        .sign_with_keys(ephemeral)
        .map_err(|e| format!("sign rekey outer: {e}"))
}

/// Build a signed 3303 channel-Rekey distributing `blobs` introducing `new_epoch` for `channel_id`.
///
/// Per the rekey is **enveloped under the (stable) server-root key** — NOT the prior channel key
/// — and addressed by the **server-root-derived** `rekey_pseudonym(server_root, channel_id, new_epoch)`.
/// Because both the envelope key and the address come from the server root (which every member always
/// holds, unchanged by a channel rotation), a member recovers ANY epoch's key independently — derive
/// that epoch's rekey address, fetch it — with no dependency on the prior epoch's channel key. No
/// ratchet: epochs are recoverable by choice (latest only, or all in parallel). The new channel key
/// lives ONLY in the per-recipient ECDH `blobs`, never under the server-root envelope, so a member
/// removed in this rotation reads the header but recovers no new key.
#[allow(clippy::too_many_arguments)]
pub fn build_channel_rekey_event(
    ephemeral: &Keys,
    rotator: &Keys,
    server_root: &[u8; 32],
    channel_id: &ChannelId,
    new_epoch: Epoch,
    prev_epoch: Epoch,
    prev_key_commitment: &[u8; 32],
    blobs: &[RekeyBlob],
) -> Result<Event, String> {
    let inner = build_rekey_inner(rotator, RekeyScope::Channel(*channel_id), new_epoch, prev_epoch, prev_key_commitment, blobs)?;
    // Envelope under the SERVER ROOT (stable across channel rotations), addressed by the server-root-
    // derived per-epoch pseudonym — so any member finds + decrypts it without the channel's prior key.
    let address = rekey_pseudonym(&ServerRootKey(*server_root), channel_id, new_epoch);
    seal_rekey_outer(ephemeral, &inner, server_root, &address)
}

/// Build a 3303 SERVER-ROOT rekey (a base rotation): the new server root reaches recipients ONLY
/// via per-recipient ECDH blobs (`RekeyScope::ServerRoot`), while the event itself is enveloped under
/// the **PRIOR** root and addressed by `base_rekey_pseudonym(prior_root, community_id, new_epoch)`.
///
/// The base layer has no stable key above it (unlike a channel, which rides the server root), so the
/// prior root is the best handle every current member holds: a returning member derives the address
/// from the root they have, finds the event, learns the ROTATOR from its inner sig, and recovers the
/// new root from their blob — a short forward-walk (base rotations are rare). The prior root only
/// *addresses + hides* the event; the new root is NOT under it (it lives in the ECDH blobs), so a
/// member removed in this rotation can find the event but recovers no new root. Re-anchoring the
/// control plane under the new epoch is a SEPARATE step the rotation orchestration performs.
#[allow(clippy::too_many_arguments)]
pub fn build_server_root_rekey_event(
    ephemeral: &Keys,
    rotator: &Keys,
    prior_root: &[u8; 32],
    community_id: &CommunityId,
    new_epoch: Epoch,
    prev_epoch: Epoch,
    prev_key_commitment: &[u8; 32],
    blobs: &[RekeyBlob],
) -> Result<Event, String> {
    let inner = build_rekey_inner(rotator, RekeyScope::ServerRoot, new_epoch, prev_epoch, prev_key_commitment, blobs)?;
    let address = base_rekey_pseudonym(&ServerRootKey(*prior_root), community_id, new_epoch);
    seal_rekey_outer(ephemeral, &inner, prior_root, &address)
}

/// Open + verify a 3303 Rekey outer with the **server-root key** (which every member always holds):
/// version-check, decrypt, parse the inner, verify the rotator's inner signature, and read the rekey
/// fields. Does NOT check authority (the rotator's roster rank) or open any blob; the caller does
/// both, pairing against the returned `rotator`. A wrong key (or non-member) fails the MAC → `Err`.
pub fn open_rekey_event(outer: &Event, server_root: &[u8; 32]) -> Result<ParsedRekey, String> {
    if outer.kind.as_u16() != event_kind::COMMUNITY_REKEY {
        return Err("not a rekey outer (kind != 3303)".to_string());
    }
    match find_unique_tag(outer, TAG_VERSION)?.as_deref() {
        Some(PROTOCOL_VERSION) => {}
        other => return Err(format!("unsupported rekey version: {other:?}")),
    }
    let plaintext = cipher::open(server_root, &outer.content).map_err(|e| format!("open rekey: {e}"))?;
    let json = String::from_utf8(plaintext).map_err(|e| format!("rekey inner utf8: {e}"))?;
    let inner = Event::from_json(&json).map_err(|e| format!("rekey inner parse: {e}"))?;

    // Inner authorship signature — proves the rotator authored this (and yields their pubkey).
    inner.verify().map_err(|_| "rekey inner signature invalid".to_string())?;
    if inner.kind.as_u16() != event_kind::COMMUNITY_REKEY {
        return Err("rekey inner is not kind 3303".to_string());
    }

    let scope = find_unique_tag(&inner, TAG_SCOPE)?
        .and_then(|h| scope_from_hex(&h))
        .ok_or("rekey missing/invalid scope")?;
    let new_epoch = Epoch(parse_u64_tag(&inner, TAG_NEW_EPOCH)?);
    let prev_epoch = Epoch(parse_u64_tag(&inner, TAG_PREV_EPOCH)?);
    let prev_hex = find_unique_tag(&inner, TAG_PREV_COMMIT)?.ok_or("rekey missing prev-commit")?;
    if prev_hex.len() != 64 || !prev_hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("rekey prev-commit is not 32-byte hex".to_string());
    }
    let prev_key_commitment = crate::simd::hex::hex_to_bytes_32(&prev_hex);

    let blobs: Vec<RekeyBlob> =
        serde_json::from_str(&inner.content).map_err(|e| format!("parse rekey blobs: {e}"))?;
    if blobs.len() > MAX_REKEY_BLOBS {
        return Err(format!("rekey carries {} blobs, over the cap", blobs.len()));
    }

    Ok(ParsedRekey {
        rotator: inner.pubkey,
        scope,
        new_epoch,
        prev_epoch,
        prev_key_commitment,
        blobs,
    })
}

/// Value of a tag required to appear at most once (a duplicate makes the signed inner ambiguous —
/// reject rather than trust first-match, the envelope discipline).
fn find_unique_tag(event: &Event, name: &str) -> Result<Option<String>, String> {
    let mut found = None;
    for t in event.tags.iter() {
        let s = t.as_slice();
        if s.len() >= 2 && s[0] == name {
            if found.is_some() {
                return Err(format!("duplicate rekey tag: {name}"));
            }
            found = Some(s[1].clone());
        }
    }
    Ok(found)
}

fn parse_u64_tag(event: &Event, name: &str) -> Result<u64, String> {
    find_unique_tag(event, name)?
        .ok_or_else(|| format!("rekey missing tag: {name}"))?
        .parse::<u64>()
        .map_err(|_| format!("rekey tag {name} is not a u64"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// FROZEN size guard: a full `MAX_REKEY_BLOBS`-recipient rekey event must serialize under strfry's
    /// default `maxEventSize` (65536). Measured boundary: 126 blobs = 55,131 bytes (fits), 127 = 66,055
    /// (over). The cap (120) sits in the 55KB bucket; if a blob-format change pushes a full event over the
    /// limit this test goes red BEFORE shipping an event relays would reject. Both rekey scopes use the
    /// same wire builder (`build_rekey_inner`/`seal_rekey_outer`), so the server-root case bounds both.
    #[test]
    fn max_rekey_blobs_event_stays_under_relay_size_limit() {
        use nostr_sdk::JsonUtil;
        let rotator = Keys::generate();
        let blobs: Vec<RekeyBlob> = (0..MAX_REKEY_BLOBS)
            .map(|_| {
                let r = Keys::generate();
                build_rekey_blob(rotator.secret_key(), &r.public_key(), RekeyScope::ServerRoot, Epoch(1), &[0xCDu8; 32]).unwrap()
            })
            .collect();
        let outer = build_server_root_rekey_event(
            &Keys::generate(), &rotator, &[0x07u8; 32], &CommunityId([0x09u8; 32]), Epoch(1), Epoch(0), &[0u8; 32], &blobs,
        )
        .unwrap();
        let size = outer.as_json().len();
        assert!(
            size <= STRFRY_MAX_EVENT_SIZE,
            "a full {MAX_REKEY_BLOBS}-blob rekey event is {size} bytes; must stay <= {STRFRY_MAX_EVENT_SIZE} (strfry maxEventSize)"
        );
    }

    fn sk(byte: u8) -> SecretKey {
        SecretKey::from_slice(&[byte; 32]).unwrap()
    }

    #[test]
    fn bound_plaintext_layout_is_frozen() {
        // The 72-byte wrapped layout is wire format (frozen-layout discipline): scope_id[32] ‖
        // epoch_be[8] ‖ new_key[32]. Pin it byte-exact so a field-order/width change can't slip in.
        let pt = bound_plaintext(RekeyScope::ServerRoot, Epoch(1), &[0xABu8; 32]);
        let expected = format!("{}{}{}", "00".repeat(32), "0000000000000001", "ab".repeat(32));
        assert_eq!(crate::simd::hex::bytes_to_hex_string(&pt), expected);
        // A channel scope puts the channel id in the first 32 bytes; a multi-byte epoch is big-endian.
        let pt2 = bound_plaintext(RekeyScope::Channel(ChannelId([0x11u8; 32])), Epoch(0x0102), &[0xCDu8; 32]);
        let expected2 = format!("{}{}{}", "11".repeat(32), "0000000000000102", "cd".repeat(32));
        assert_eq!(crate::simd::hex::bytes_to_hex_string(&pt2), expected2);
    }

    #[test]
    fn pairwise_secret_regression_pin() {
        // Regression pin for the pairwise-secret derivation = NIP-44 v2 ConversationKey (HKDF-extract
        // of the ECDH shared point) between sk=[1;32] and pk(of sk=[2;32]). Pins the ECDH→IKM step the
        // recipient-pseudonym golden vector (which starts from a literal secret) does not cover, so a
        // change to which secret feeds the locator can't slip through silently.
        let a = Keys::new(sk(1));
        let b = Keys::new(sk(2));
        let secret = rekey_pairwise_secret(a.secret_key(), &b.public_key()).unwrap();
        assert_eq!(crate::simd::hex::bytes_to_hex_string(&secret), GOLDEN_PAIRWISE_SECRET);
    }

    // Captured from the NIP-44 v2 ConversationKey::derive(sk=[1;32], pk=secp(sk=[2;32])). Pins the
    // exact pairwise secret that locates + wraps a rekey blob.
    const GOLDEN_PAIRWISE_SECRET: &str =
        "59c6d24d9c3a7bf8ca4cec54031a3e2ecfaa553452a2b2fa3147e31ee55f33d5";

    #[test]
    fn pairwise_secret_is_symmetric_and_deterministic() {
        let a = Keys::new(sk(1));
        let b = Keys::new(sk(2));
        let from_a = rekey_pairwise_secret(a.secret_key(), &b.public_key()).unwrap();
        let from_b = rekey_pairwise_secret(b.secret_key(), &a.public_key()).unwrap();
        assert_eq!(from_a, from_b, "ECDH is symmetric: both sides derive the same secret");
        // Deterministic across calls.
        assert_eq!(from_a, rekey_pairwise_secret(a.secret_key(), &b.public_key()).unwrap());
        // A third party derives something different.
        let c = Keys::new(sk(3));
        assert_ne!(from_a, rekey_pairwise_secret(a.secret_key(), &c.public_key()).unwrap());
    }

    #[test]
    fn blob_round_trips_server_root_scope() {
        let sender = Keys::new(sk(7));
        let recipient = Keys::new(sk(8));
        let new_key = [0xABu8; 32];
        let blob = build_rekey_blob(
            sender.secret_key(), &recipient.public_key(), RekeyScope::ServerRoot, Epoch(1), &new_key,
        )
        .unwrap();
        let got = open_rekey_blob(
            recipient.secret_key(), &sender.public_key(), RekeyScope::ServerRoot, Epoch(1), &blob,
        )
        .unwrap();
        assert_eq!(got, new_key, "the recipient recovers the fresh key");
    }

    #[test]
    fn blob_round_trips_channel_scope() {
        let sender = Keys::new(sk(7));
        let recipient = Keys::new(sk(8));
        let chan = RekeyScope::Channel(ChannelId([0x42u8; 32]));
        let new_key = [0xCDu8; 32];
        let blob = build_rekey_blob(sender.secret_key(), &recipient.public_key(), chan, Epoch(5), &new_key).unwrap();
        let got = open_rekey_blob(recipient.secret_key(), &sender.public_key(), chan, Epoch(5), &blob).unwrap();
        assert_eq!(got, new_key);
    }

    #[test]
    fn locator_is_the_recipient_pseudonym() {
        // The blob's locator must equal recipient_pseudonym(pairwise_secret, scope, epoch) — that is how
        // the recipient finds it (compute their own, look it up) with no trial-decryption.
        let sender = Keys::new(sk(7));
        let recipient = Keys::new(sk(8));
        let secret = rekey_pairwise_secret(sender.secret_key(), &recipient.public_key()).unwrap();
        let blob = build_rekey_blob(
            sender.secret_key(), &recipient.public_key(), RekeyScope::ServerRoot, Epoch(2), &[1u8; 32],
        )
        .unwrap();
        assert_eq!(blob.locator, recipient_pseudonym(&secret, RekeyScope::ServerRoot, Epoch(2)).to_hex());
    }

    #[test]
    fn wrong_sender_cannot_open() {
        // A removed member who guesses the slot but pairs against the wrong sender derives a different
        // secret → wrong locator (rejected before even decrypting).
        let sender = Keys::new(sk(7));
        let recipient = Keys::new(sk(8));
        let impostor = Keys::new(sk(9));
        let blob = build_rekey_blob(
            sender.secret_key(), &recipient.public_key(), RekeyScope::ServerRoot, Epoch(1), &[2u8; 32],
        )
        .unwrap();
        let err = open_rekey_blob(
            recipient.secret_key(), &impostor.public_key(), RekeyScope::ServerRoot, Epoch(1), &blob,
        );
        assert!(err.is_err(), "pairing against the wrong sender must fail");
    }

    #[test]
    fn non_recipient_cannot_open() {
        // A different recipient (not the one the blob was wrapped to) derives a different pairwise
        // secret with the sender, so the locator won't match.
        let sender = Keys::new(sk(7));
        let recipient = Keys::new(sk(8));
        let other = Keys::new(sk(10));
        let blob = build_rekey_blob(
            sender.secret_key(), &recipient.public_key(), RekeyScope::ServerRoot, Epoch(1), &[3u8; 32],
        )
        .unwrap();
        assert!(open_rekey_blob(
            other.secret_key(), &sender.public_key(), RekeyScope::ServerRoot, Epoch(1), &blob,
        )
        .is_err());
    }

    #[test]
    fn scope_splice_is_rejected() {
        // A blob minted for the server root, presented at a channel coordinate: the locator differs, so
        // it's rejected. (And even past the locator, the plaintext scope binding would fire.)
        let sender = Keys::new(sk(7));
        let recipient = Keys::new(sk(8));
        let blob = build_rekey_blob(
            sender.secret_key(), &recipient.public_key(), RekeyScope::ServerRoot, Epoch(1), &[4u8; 32],
        )
        .unwrap();
        let chan = RekeyScope::Channel(ChannelId([0x42u8; 32]));
        assert!(open_rekey_blob(recipient.secret_key(), &sender.public_key(), chan, Epoch(1), &blob).is_err());
    }

    #[test]
    fn epoch_splice_is_rejected() {
        let sender = Keys::new(sk(7));
        let recipient = Keys::new(sk(8));
        let blob = build_rekey_blob(
            sender.secret_key(), &recipient.public_key(), RekeyScope::ServerRoot, Epoch(1), &[5u8; 32],
        )
        .unwrap();
        assert!(open_rekey_blob(
            recipient.secret_key(), &sender.public_key(), RekeyScope::ServerRoot, Epoch(2), &blob,
        )
        .is_err());
    }

    #[test]
    fn relocated_blob_is_rejected() {
        // An attacker who moves a valid blob to a DIFFERENT recipient's locator string can't make it
        // open there: open recomputes the expected locator and refuses a mismatch.
        let sender = Keys::new(sk(7));
        let recipient = Keys::new(sk(8));
        let mut blob = build_rekey_blob(
            sender.secret_key(), &recipient.public_key(), RekeyScope::ServerRoot, Epoch(1), &[6u8; 32],
        )
        .unwrap();
        blob.locator = "ff".repeat(32);
        assert!(open_rekey_blob(
            recipient.secret_key(), &sender.public_key(), RekeyScope::ServerRoot, Epoch(1), &blob,
        )
        .is_err());
    }

    #[test]
    fn tampered_ciphertext_is_rejected() {
        let sender = Keys::new(sk(7));
        let recipient = Keys::new(sk(8));
        let mut blob = build_rekey_blob(
            sender.secret_key(), &recipient.public_key(), RekeyScope::ServerRoot, Epoch(1), &[7u8; 32],
        )
        .unwrap();
        // Flip a byte mid-ciphertext → NIP-44 MAC fails.
        let mut bytes = base64_simd::STANDARD.decode_to_vec(blob.wrapped.as_bytes()).unwrap();
        let mid = bytes.len() / 2;
        bytes[mid] ^= 0xff;
        blob.wrapped = base64_simd::STANDARD.encode_to_string(&bytes);
        assert!(open_rekey_blob(
            recipient.secret_key(), &sender.public_key(), RekeyScope::ServerRoot, Epoch(1), &blob,
        )
        .is_err());
    }

    // --- 3303 Rekey event tests ---

    // A fixed server-root key for the event tests — the rekey envelope key (NOT a channel key).
    const SR: [u8; 32] = [0x55u8; 32];
    const CHAN: [u8; 32] = [0x42u8; 32];

    #[test]
    fn epoch_key_commitment_golden_and_binds_epoch() {
        // Pin the fork-detection commitment: domain ‖ prev_epoch_be ‖ prev_key.
        let c = epoch_key_commitment(Epoch(1), &[0x33u8; 32]);
        assert_eq!(crate::simd::hex::bytes_to_hex_32(&c), GOLDEN_EPOCH_COMMITMENT);
        // The epoch binds: same key, different epoch → different commitment.
        assert_ne!(c, epoch_key_commitment(Epoch(2), &[0x33u8; 32]));
        // The key binds: same epoch, different key → different commitment.
        assert_ne!(c, epoch_key_commitment(Epoch(1), &[0x34u8; 32]));
    }

    #[test]
    fn channel_rekey_round_trips() {
        let rotator = Keys::new(sk(1));
        let scope = RekeyScope::Channel(ChannelId(CHAN));
        let commit = epoch_key_commitment(Epoch(0), &[0xEEu8; 32]);
        let blob = build_rekey_blob(rotator.secret_key(), &Keys::new(sk(8)).public_key(), scope, Epoch(1), &[0xABu8; 32]).unwrap();

        let outer = build_channel_rekey_event(
            &Keys::generate(), &rotator, &SR, &ChannelId(CHAN), Epoch(1), Epoch(0), &commit, &[blob.clone()],
        )
        .unwrap();
        // The outer must NOT be signed by the rotator (no identity on the wire).
        assert_ne!(outer.pubkey, rotator.public_key());

        // Opened with the SERVER ROOT (not a channel key) — every member always holds it.
        let parsed = open_rekey_event(&outer, &SR).unwrap();
        assert_eq!(parsed.rotator, rotator.public_key(), "rotator recovered from the inner sig");
        assert!(matches!(parsed.scope, RekeyScope::Channel(c) if c.0 == CHAN));
        assert_eq!(parsed.new_epoch, Epoch(1));
        assert_eq!(parsed.prev_epoch, Epoch(0));
        assert_eq!(parsed.prev_key_commitment, commit);
        assert_eq!(parsed.blobs, vec![blob]);
    }

    #[test]
    fn epochs_are_independently_recoverable_with_only_the_server_root() {
        // THE no-ratchet property: a member holding ONLY the server root (no channel key at ANY epoch)
        // recovers the LATEST epoch's key directly, skipping every intermediate rekey. This is what
        // makes catch-up a parallel choose-what-you-want fetch, not a forward chain walk.
        let rotator = Keys::new(sk(1));
        let recipient = Keys::new(sk(8));
        let scope = RekeyScope::Channel(ChannelId(CHAN));

        // Rotations introduced epochs 1..=5. The member was away for all of them and holds no channel key.
        let mut events = Vec::new();
        let mut keys = std::collections::HashMap::new();
        for e in 1..=5u64 {
            let key = [e as u8; 32];
            keys.insert(e, key);
            let blob = build_rekey_blob(rotator.secret_key(), &recipient.public_key(), scope, Epoch(e), &key).unwrap();
            events.push(build_channel_rekey_event(
                &Keys::generate(), &rotator, &SR, &ChannelId(CHAN), Epoch(e), Epoch(e - 1),
                &epoch_key_commitment(Epoch(e - 1), &[0u8; 32]), &[blob],
            ).unwrap());
        }

        // Jump straight to epoch 5: address it from the server root alone, open it, recover key_5 —
        // NEVER touching epochs 1..4 or any prior channel key.
        let want = rekey_pseudonym(&ServerRootKey(SR), &ChannelId(CHAN), Epoch(5)).to_hex();
        let latest = events.iter().find(|ev| ev.tags.iter().any(|t| {
            let s = t.as_slice();
            s.len() >= 2 && s[0] == "z" && s[1] == want
        })).expect("epoch-5 rekey is addressable from the server root");
        let parsed = open_rekey_event(latest, &SR).unwrap();
        let secret = rekey_pairwise_secret(recipient.secret_key(), &parsed.rotator).unwrap();
        let loc = recipient_pseudonym(&secret, parsed.scope, parsed.new_epoch).to_hex();
        let mine = parsed.blobs.iter().find(|b| b.locator == loc).unwrap();
        let got = open_rekey_blob(recipient.secret_key(), &parsed.rotator, parsed.scope, parsed.new_epoch, mine).unwrap();
        assert_eq!(got, keys[&5], "recovered the latest key with only the server root, skipping all earlier epochs");
    }

    #[test]
    fn end_to_end_recipient_opens_their_new_key() {
        // The full #3a+#3b path: a rotator builds a Rekey carrying a blob for a recipient; the recipient
        // opens the event (learns the rotator pubkey from the inner sig), then opens THEIR blob.
        let rotator = Keys::new(sk(1));
        let recipient = Keys::new(sk(8));
        let scope = RekeyScope::Channel(ChannelId(CHAN));
        let new_key = [0xCDu8; 32];
        let blob = build_rekey_blob(rotator.secret_key(), &recipient.public_key(), scope, Epoch(1), &new_key).unwrap();
        let outer = build_channel_rekey_event(
            &Keys::generate(), &rotator, &SR, &ChannelId(CHAN), Epoch(1), Epoch(0),
            &epoch_key_commitment(Epoch(0), &[0u8; 32]), &[blob],
        )
        .unwrap();

        let parsed = open_rekey_event(&outer, &SR).unwrap();
        // The recipient finds their blob by computing their own locator, then opens it.
        let secret = rekey_pairwise_secret(recipient.secret_key(), &parsed.rotator).unwrap();
        let my_locator = recipient_pseudonym(&secret, parsed.scope, parsed.new_epoch).to_hex();
        let mine = parsed.blobs.iter().find(|b| b.locator == my_locator).expect("my blob is present");
        let got = open_rekey_blob(recipient.secret_key(), &parsed.rotator, parsed.scope, parsed.new_epoch, mine).unwrap();
        assert_eq!(got, new_key, "recipient recovers the fresh epoch key end to end");
    }

    #[test]
    fn server_root_rekey_round_trips_under_the_prior_root() {
        // A base rotation: enveloped under the PRIOR root, addressed by base_rekey_pseudonym(prior_root,
        // community, new_epoch), ServerRoot-scope blobs. A recipient opens with the PRIOR root (the one
        // they hold), learns the rotator, recovers the NEW root from their blob.
        let rotator = Keys::new(sk(1));
        let recipient = Keys::new(sk(8));
        let prior_root = [0x66u8; 32];
        let community_id = CommunityId([0x77u8; 32]);
        let new_root = [0x99u8; 32];
        let blob = build_rekey_blob(rotator.secret_key(), &recipient.public_key(), RekeyScope::ServerRoot, Epoch(1), &new_root).unwrap();
        let commit = epoch_key_commitment(Epoch(0), &prior_root);
        let outer = build_server_root_rekey_event(
            &Keys::generate(), &rotator, &prior_root, &community_id, Epoch(1), Epoch(0), &commit, &[blob],
        )
        .unwrap();
        assert_ne!(outer.pubkey, rotator.public_key(), "outer is ephemeral, not the rotator");

        // Addressed by the PRIOR-root-derived base pseudonym (so a member finds it with the root they hold).
        let expected = base_rekey_pseudonym(&ServerRootKey(prior_root), &community_id, Epoch(1)).to_hex();
        let z = outer.tags.iter().find_map(|t| {
            let s = t.as_slice();
            (s.len() >= 2 && s[0] == "z").then(|| s[1].clone())
        });
        assert_eq!(z.as_deref(), Some(expected.as_str()));

        // Opened under the PRIOR root; the recipient recovers the NEW root from their ServerRoot blob.
        let parsed = open_rekey_event(&outer, &prior_root).unwrap();
        assert!(matches!(parsed.scope, RekeyScope::ServerRoot));
        assert_eq!(parsed.rotator, rotator.public_key());
        let secret = rekey_pairwise_secret(recipient.secret_key(), &parsed.rotator).unwrap();
        let loc = recipient_pseudonym(&secret, parsed.scope, parsed.new_epoch).to_hex();
        let mine = parsed.blobs.iter().find(|b| b.locator == loc).unwrap();
        let got = open_rekey_blob(recipient.secret_key(), &parsed.rotator, parsed.scope, parsed.new_epoch, mine).unwrap();
        assert_eq!(got, new_root, "recipient recovers the new server root");
    }

    #[test]
    fn base_and_channel_blobs_to_same_recipient_same_epoch_do_not_collide() {
        // A base rotation and a channel rekey at the same epoch to the same member land their
        // per-recipient blobs at DIFFERENT locators (the recipient_pseudonym scope disambiguates), so
        // neither overwrites the other inside the recipient's view.
        let sender = Keys::new(sk(1));
        let recipient = Keys::new(sk(8));
        let chan = RekeyScope::Channel(ChannelId([0x42u8; 32]));
        let base = RekeyScope::ServerRoot;
        let cb = build_rekey_blob(sender.secret_key(), &recipient.public_key(), chan, Epoch(1), &[0xAAu8; 32]).unwrap();
        let bb = build_rekey_blob(sender.secret_key(), &recipient.public_key(), base, Epoch(1), &[0xBBu8; 32]).unwrap();
        assert_ne!(cb.locator, bb.locator, "channel-scope and base-scope blobs must not collide");
    }

    #[test]
    fn server_root_rekey_not_openable_without_the_prior_root() {
        let rotator = Keys::new(sk(1));
        let recipient = Keys::new(sk(8));
        let prior_root = [0x66u8; 32];
        let community_id = CommunityId([0x77u8; 32]);
        let blob = build_rekey_blob(rotator.secret_key(), &recipient.public_key(), RekeyScope::ServerRoot, Epoch(1), &[0x99u8; 32]).unwrap();
        let outer = build_server_root_rekey_event(
            &Keys::generate(), &rotator, &prior_root, &community_id, Epoch(1), Epoch(0),
            &epoch_key_commitment(Epoch(0), &prior_root), &[blob],
        )
        .unwrap();
        // A member who doesn't hold the prior root (e.g. a never-member, or removed before this epoch)
        // can't even read the envelope.
        assert!(open_rekey_event(&outer, &[0x00u8; 32]).is_err());
    }

    #[test]
    fn outer_address_is_server_root_derived_not_channel_key() {
        // The crux of the no-ratchet fix: the `z` address is rekey_pseudonym(server_root, channel, NEW
        // epoch) — derivable by any member from the server root alone, independent of any channel key.
        let rotator = Keys::new(sk(1));
        let outer = build_channel_rekey_event(
            &Keys::generate(), &rotator, &SR, &ChannelId(CHAN), Epoch(1), Epoch(0), &[0u8; 32], &[],
        )
        .unwrap();
        let expected = rekey_pseudonym(&ServerRootKey(SR), &ChannelId(CHAN), Epoch(1)).to_hex();
        let z = outer.tags.iter().find_map(|t| {
            let s = t.as_slice();
            (s.len() >= 2 && s[0] == "z").then(|| s[1].clone())
        });
        assert_eq!(z.as_deref(), Some(expected.as_str()));
    }

    #[test]
    fn wrong_server_root_cannot_open() {
        let rotator = Keys::new(sk(1));
        let outer = build_channel_rekey_event(
            &Keys::generate(), &rotator, &SR, &ChannelId(CHAN), Epoch(1), Epoch(0), &[0u8; 32], &[],
        )
        .unwrap();
        assert!(open_rekey_event(&outer, &[0x00u8; 32]).is_err(), "a non-member (wrong server root) can't read it");
    }

    #[test]
    fn forged_inner_signature_is_rejected() {
        // Tamper the inner after signing, re-seal, re-wrap → inner sig must fail on open.
        let rotator = Keys::new(sk(1));
        let old_key = SR;
        let inner = EventBuilder::new(Kind::Custom(event_kind::COMMUNITY_REKEY), "[]")
            .tags([
                Tag::custom(TagKind::Custom(TAG_SCOPE.into()), [scope_to_hex(RekeyScope::ServerRoot)]),
                Tag::custom(TagKind::Custom(TAG_NEW_EPOCH.into()), ["1".to_string()]),
                Tag::custom(TagKind::Custom(TAG_PREV_EPOCH.into()), ["0".to_string()]),
                Tag::custom(TagKind::Custom(TAG_PREV_COMMIT.into()), ["00".repeat(32)]),
            ])
            .sign_with_keys(&rotator)
            .unwrap();
        let mut v: serde_json::Value = serde_json::from_str(&inner.as_json()).unwrap();
        v["content"] = serde_json::Value::String("[{\"locator\":\"x\",\"wrapped\":\"y\"}]".into());
        let tampered = serde_json::to_string(&v).unwrap();
        let content = cipher::seal(&old_key, tampered.as_bytes()).unwrap();
        let outer = EventBuilder::new(Kind::Custom(event_kind::COMMUNITY_REKEY), content)
            .tags([Tag::custom(TagKind::Custom(TAG_VERSION.into()), [PROTOCOL_VERSION.to_string()])])
            .sign_with_keys(&Keys::generate())
            .unwrap();
        assert!(open_rekey_event(&outer, &old_key).is_err());
    }

    #[test]
    fn wrong_outer_kind_and_bad_version_rejected() {
        let old_key = [0x55u8; 32];
        // Wrong outer kind.
        let not_rekey = EventBuilder::new(Kind::Custom(event_kind::COMMUNITY_MESSAGE), "x")
            .sign_with_keys(&Keys::generate())
            .unwrap();
        assert!(open_rekey_event(&not_rekey, &old_key).is_err());
        // Right kind, missing/garbage version (checked before decrypt).
        let bad_ver = EventBuilder::new(Kind::Custom(event_kind::COMMUNITY_REKEY), "x")
            .tags([Tag::custom(TagKind::Custom(TAG_VERSION.into()), ["999".to_string()])])
            .sign_with_keys(&Keys::generate())
            .unwrap();
        assert!(open_rekey_event(&bad_ver, &old_key).is_err());
    }

    #[test]
    fn version_is_checked_before_decrypt() {
        // Prove ordering: a bogus version tag AND a key we can't decrypt under must fail on VERSION,
        // never reaching cipher::open. (`wrong_outer_kind_and_bad_version_rejected` can't prove this —
        // its content would fail decrypt anyway, so a regression moving the check after decrypt would
        // still error there. Here the error must specifically be the version rejection.)
        let real_key = [0x55u8; 32];
        let inner = EventBuilder::new(Kind::Custom(event_kind::COMMUNITY_REKEY), "[]")
            .sign_with_keys(&Keys::generate())
            .unwrap();
        let content = cipher::seal(&real_key, inner.as_json().as_bytes()).unwrap();
        let outer = EventBuilder::new(Kind::Custom(event_kind::COMMUNITY_REKEY), content)
            .tags([Tag::custom(TagKind::Custom(TAG_VERSION.into()), ["999".to_string()])])
            .sign_with_keys(&Keys::generate())
            .unwrap();
        // Open with a DIFFERENT key: if the version check were after decrypt, this would surface a
        // decrypt error instead of the version error.
        let err = open_rekey_event(&outer, &[0x00u8; 32]).unwrap_err();
        assert!(err.contains("version"), "must reject on version before decrypt, got: {err}");
    }

    const GOLDEN_EPOCH_COMMITMENT: &str =
        "5e706f60f1c6f39208071e914d3284dab5f93a1c8d178260e7daf5d23e26a81f";

    #[test]
    fn distinct_recipients_get_distinct_locators() {
        // Two recipients of the same rotation land at different slots (no collision, O(1) lookup each).
        let sender = Keys::new(sk(7));
        let r1 = Keys::new(sk(8));
        let r2 = Keys::new(sk(9));
        let b1 = build_rekey_blob(sender.secret_key(), &r1.public_key(), RekeyScope::ServerRoot, Epoch(1), &[1u8; 32]).unwrap();
        let b2 = build_rekey_blob(sender.secret_key(), &r2.public_key(), RekeyScope::ServerRoot, Epoch(1), &[1u8; 32]).unwrap();
        assert_ne!(b1.locator, b2.locator);
    }
}
