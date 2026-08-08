//! Concord v2 rekeys (CORD-06) — the per-recipient key-delivery atom + the 3303
//! event that carries a rotation.
//!
//! A rotation mints a fresh-random key for the next epoch and delivers it only
//! to the members who STAY, one **per-recipient blob** each. Two structural
//! shifts from v1, both security-load-bearing:
//!
//! **D1 — the locator is PUBLIC and authenticates nothing.** v1 located a blob
//! by `HKDF(pairwise_ECDH_secret, …)` — a value only the sender↔recipient pair
//! could compute, so in v1 a matching locator *proved* the blob was minted for
//! that pair. v2 locates by `HKDF(rotator_xonly || recipient_xonly, …)` — public
//! inputs (full NIP-46 bunker parity: a bunker computes it without a raw key).
//! A public locator proves NOTHING; it is a lookup index only. Authenticity now
//! rests entirely on (a) the crate's **rotator seal + roster authority** —
//! verified by the caller before any blob is trusted — and (b) the blob's
//! **bound plaintext** (scope+epoch checked after decrypt). So [`open_blob`]
//! does NOT gate on the locator (that was v1's `open_rekey_blob` assumption —
//! do not port it): the decrypt itself (only the addressed recipient's key
//! opens a blob wrapped to them) plus the bound-plaintext check are the gate.
//!
//! **D5 — the blob wrap carries base64.** NIP-44/NIP-46 encrypt surfaces are
//! string-typed, and the 72 raw bytes aren't valid UTF-8, so the wrapped
//! plaintext is `base64(scope_id ‖ epoch_be ‖ new_key)` — a string a bunker can
//! `nip44_encrypt`/`nip44_decrypt` to the recipient's identity key with no raw
//! secret. The `wrapped` field is then the standard NIP-44 payload string, so a
//! local-keys wrap and a bunker wrap produce identical wire output.
//!
//! The 3303 event itself is a v2 stream event (kind-1059 wrap, ENCRYPTED seal
//! signed by the rotator's real identity) at the rekey address — reusing
//! [`super::stream`]. Its seal is what tells the recipient WHO rotated, which is
//! both the ECDH counterparty and the authority actor.
//!
//! What lives here: the blob atom, the 3303 build/parse, chunk-set assembly, and
//! the continuity/fork comparators — all PURE. The stateful orchestration
//! (recipient-set computation, the base+channels lockstep read-cut, DB epoch
//! archival, and the D2 BAN-vs-MANAGE_CHANNELS authority gate, which is an
//! apply-path concern keyed on prior-vs-current-root addressing) sits in the
//! service layer.

use nostr_sdk::prelude::nip44::v2::{decrypt_to_bytes, ConversationKey};
use nostr_sdk::prelude::{Event, Keys, PublicKey, SecretKey, Tag, Timestamp, UnsignedEvent};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use super::super::{ChannelId, CommunityId, Epoch};
use super::derive::{
    base_rekey_group_key, channel_rekey_group_key, control_signer_group_key, epoch_key_commitment,
    recipient_locator, GroupKey,
};
use super::stream::{self, OpenedStream, SealForm, StreamError};

/// Max recipients (blobs) Vector puts in ONE 3303 event when SENDING. Lower than
/// the spec's stated 120 because a v2 rekey rides the CORD-01 double-wrap (blob
/// array → encrypted seal → wrap, two NIP-44 base64 expansions): a 120-blob event
/// measures ~77 KB, over strfry's 64 KB `maxEventSize`, while 80 blobs measure
/// ~55 KB (a full one is size-guarded by test). The spec's 120 assumes a lighter
/// envelope — a CORD-06 erratum (see the divergence ledger). A larger recipient
/// set splits across chunk events.
pub const MAX_REKEY_BLOBS_PER_EVENT: usize = 80;

/// Max blobs Vector will ACCEPT in one received 3303 chunk (a DoS bound checked
/// after decrypt). Kept at the spec's stated 120 — higher than the send cap — so
/// a chunk minted by another client at the spec limit (and delivered by a relay
/// with a larger `maxEventSize`) still parses. An array over this is rejected.
pub const MAX_REKEY_BLOBS_RECEIVED: usize = 120;

const TAG_SCOPE: &str = "scope";
const TAG_NEW_EPOCH: &str = "newepoch";
const TAG_PREV_EPOCH: &str = "prevepoch";
const TAG_PREV_COMMIT: &str = "prevcommit";
const TAG_CHUNK: &str = "chunk";

/// What a rekey rotates (CORD-06 §1). The 32-byte scope id is stamped into every
/// blob's plaintext so a blob can't be spliced onto another coordinate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RekeyScope {
    /// A specific private channel being rekeyed.
    Channel(ChannelId),
    /// The community_root (a base rotation / Refounding) — the all-zero sentinel
    /// (a random channel id never collides with it).
    Root,
}

impl RekeyScope {
    /// The 32-byte scope id: the channel id, or the all-zero root sentinel.
    pub fn id32(&self) -> [u8; 32] {
        match self {
            RekeyScope::Channel(c) => c.0,
            RekeyScope::Root => [0u8; 32],
        }
    }

    fn to_hex(self) -> String {
        crate::simd::hex::bytes_to_hex_32(&self.id32())
    }

    fn from_hex(hex: &str) -> Option<RekeyScope> {
        if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let bytes = crate::simd::hex::hex_to_bytes_32(hex);
        Some(if bytes == [0u8; 32] {
            RekeyScope::Root
        } else {
            RekeyScope::Channel(ChannelId(bytes))
        })
    }
}

/// One located, wrapped rekey blob — the unit a 3303 event carries N of.
///
/// `locator` is the public [`recipient_locator`] hex (a lookup index — proves
/// nothing, D1); `wrapped` is the NIP-44 payload string whose plaintext is
/// `base64(scope_id ‖ epoch_be ‖ new_key)` (D5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RekeyBlob {
    pub locator: String,
    pub wrapped: String,
}

/// Errors from the rekey layer.
#[derive(Debug)]
pub enum RekeyError {
    Stream(StreamError),
    Crypto(String),
    /// The wrapped plaintext isn't the expected 72-byte layout.
    BadBlobLength(usize),
    /// A base blob's plaintext isn't one of the three fixed widths (CORD-06 §1):
    /// 72 (legacy pre-split), 104 (member), 136 (staff).
    BadBaseBlobWidth(usize),
    /// A 136-byte base blob's `new_control_root` doesn't derive to its
    /// `new_control_pk` — refused whole rather than adopting a plane split from
    /// its readers (CORD-06 §1).
    ControlPairMismatch,
    /// A Grant's `control_wrap` plaintext isn't the fixed 40 bytes (CORD-04 §3).
    BadControlWrapLength(usize),
    /// The blob's bound scope ≠ the coordinate it's being opened under (splice).
    ScopeSplice,
    /// The blob's bound epoch ≠ the coordinate it's being opened under (splice).
    EpochSplice,
    /// The rumor isn't a kind-3303 rekey.
    NotARekey(u16),
    /// A required tag is absent, duplicated, or malformed.
    BadTag(&'static str),
    /// `new_epoch <= prev_epoch` — a rotation must advance the chain.
    NonMonotonicEpoch,
    /// A chunk index is out of range (`i < 1`, `i > n`, or `n < 1`).
    BadChunkIndex,
    /// The blob array exceeds the cap.
    TooManyBlobs(usize),
}

impl std::fmt::Display for RekeyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RekeyError::Stream(e) => write!(f, "stream: {e}"),
            RekeyError::Crypto(e) => write!(f, "crypto: {e}"),
            RekeyError::BadBlobLength(n) => write!(f, "rekey blob plaintext is {n} bytes, expected 72"),
            RekeyError::BadBaseBlobWidth(n) => write!(f, "base rekey blob plaintext is {n} bytes, expected 72, 104 or 136"),
            RekeyError::ControlPairMismatch => write!(f, "base rekey blob control_root does not derive to its control_pk"),
            RekeyError::BadControlWrapLength(n) => write!(f, "control_wrap plaintext is {n} bytes, expected 40"),
            RekeyError::ScopeSplice => write!(f, "rekey blob scope binding mismatch (splice)"),
            RekeyError::EpochSplice => write!(f, "rekey blob epoch binding mismatch (splice)"),
            RekeyError::NotARekey(k) => write!(f, "rumor kind {k} is not a rekey"),
            RekeyError::BadTag(t) => write!(f, "missing/duplicate/malformed rekey tag: {t}"),
            RekeyError::NonMonotonicEpoch => write!(f, "rekey new_epoch must exceed prev_epoch"),
            RekeyError::BadChunkIndex => write!(f, "rekey chunk index out of range"),
            RekeyError::TooManyBlobs(n) => write!(f, "rekey carries {n} blobs, over the cap"),
        }
    }
}

impl std::error::Error for RekeyError {}

impl From<StreamError> for RekeyError {
    fn from(e: StreamError) -> Self {
        RekeyError::Stream(e)
    }
}

// ── The blob atom ────────────────────────────────────────────────────────────

/// The 72-byte bound plaintext: `scope_id[32] ‖ epoch_be[8] ‖ new_key[32]`.
/// Fixed-width, so no separators are needed to parse it unambiguously.
fn bound_plaintext(scope: RekeyScope, epoch: Epoch, new_key: &[u8; 32]) -> [u8; 72] {
    let mut pt = [0u8; 72];
    pt[..32].copy_from_slice(&scope.id32());
    pt[32..40].copy_from_slice(&epoch.0.to_be_bytes());
    pt[40..].copy_from_slice(new_key);
    pt
}

/// The base64 string a blob's NIP-44 layer actually encrypts (D5). Exposed so
/// the service-layer bunker path can `signer.nip44_encrypt(recipient, this)`.
pub fn bound_plaintext_b64(scope: RekeyScope, epoch: Epoch, new_key: &[u8; 32]) -> String {
    base64_simd::STANDARD.encode_to_string(bound_plaintext(scope, epoch, new_key))
}

/// Parse + verify a decrypted bound plaintext (the base64 already stripped),
/// checking scope+epoch strict-equal the coordinate it was opened under before
/// yielding `new_key`. Exposed for the bunker open path.
pub fn parse_bound_plaintext(pt: &[u8], scope: RekeyScope, epoch: Epoch) -> Result<[u8; 32], RekeyError> {
    if pt.len() != 72 {
        return Err(RekeyError::BadBlobLength(pt.len()));
    }
    if pt[..32] != scope.id32() {
        return Err(RekeyError::ScopeSplice);
    }
    let mut epoch_be = [0u8; 8];
    epoch_be.copy_from_slice(&pt[32..40]);
    if u64::from_be_bytes(epoch_be) != epoch.0 {
        return Err(RekeyError::EpochSplice);
    }
    let mut new_key = [0u8; 32];
    new_key.copy_from_slice(&pt[40..72]);
    Ok(new_key)
}

// ── The base-rotation blob forms (CORD-06 §1) ────────────────────────────────
//
// A base rotation's blob widens past the 72-byte channel layout to carry the
// next epoch's Control Plane keys (CORD-02 §2): every member's blob appends
// `new_control_pk[32]` (104 bytes), a staff recipient's additionally
// `new_control_root[32]` (136). The width declares the form; a 72-byte BASE
// blob is the legacy pre-split rotation — honored when reading old epochs,
// never minted by a compliant Rotator. Any other width is malformed.

/// What a base blob delivered (CORD-06 §1); the width declared the form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaseKeyDelivery {
    pub new_root: [u8; 32],
    /// The next epoch's Control Plane address — absent on a legacy 72-byte
    /// blob, whose acceptor folds that epoch's Control at the legacy
    /// member-derivable address instead (CORD-06 §3).
    pub control_pk: Option<[u8; 32]>,
    /// The staff write secret (136-byte form only), already verified to derive
    /// to `control_pk`.
    pub control_root: Option<[u8; 32]>,
}

fn bound_base_plaintext(epoch: Epoch, new_root: &[u8; 32], control_pk: &[u8; 32], control_root: Option<&[u8; 32]>) -> Vec<u8> {
    let mut pt = Vec::with_capacity(if control_root.is_some() { 136 } else { 104 });
    pt.extend_from_slice(&bound_plaintext(RekeyScope::Root, epoch, new_root));
    pt.extend_from_slice(control_pk);
    if let Some(cr) = control_root {
        pt.extend_from_slice(cr);
    }
    pt
}

/// The base64 string a BASE blob's NIP-44 layer encrypts (D5) — the 104-byte
/// member form, or the 136-byte staff form when `control_root` rides.
pub fn bound_base_plaintext_b64(epoch: Epoch, new_root: &[u8; 32], control_pk: &[u8; 32], control_root: Option<&[u8; 32]>) -> String {
    base64_simd::STANDARD.encode_to_string(bound_base_plaintext(epoch, new_root, control_pk, control_root))
}

/// Parse + verify a decrypted BASE blob plaintext (CORD-06 §1): 72 (legacy
/// pre-split), 104 (member), 136 (staff). The scope must be the all-zero base
/// sentinel and the epoch must match the coordinate (unspliceable), and a
/// 136-byte blob's `new_control_root` must derive to exactly its
/// `new_control_pk` (CORD-02 §5) — a mismatched pair refuses the WHOLE blob
/// rather than adopting a plane split from its readers.
///
/// A width ABOVE 136 is a future form this build predates. Refusing it would
/// re-create the pre-split brick (a member forks off at the rotation until
/// they update), so it degrades per CORD-06 §3's lenient grade instead: the
/// frozen 72-byte prefix yields the root (membership and every Chat plane
/// survive), the family's appended offsets yield the control pair when they
/// still verify (the derive check fails closed on a reordered layout), and
/// whatever the extra bytes govern freezes — a safe prompt to update, never a
/// fork. Widths between the defined forms fit no append-only extension and
/// stay malformed.
pub fn parse_bound_base_plaintext(pt: &[u8], community_id: &CommunityId, epoch: Epoch) -> Result<BaseKeyDelivery, RekeyError> {
    if !matches!(pt.len(), 72 | 104 | 136) && pt.len() < 137 {
        return Err(RekeyError::BadBaseBlobWidth(pt.len()));
    }
    let new_root = parse_bound_plaintext(&pt[..72], RekeyScope::Root, epoch)?;
    if pt.len() == 72 {
        return Ok(BaseKeyDelivery { new_root, control_pk: None, control_root: None });
    }
    let mut control_pk = [0u8; 32];
    control_pk.copy_from_slice(&pt[72..104]);
    if pt.len() == 104 {
        return Ok(BaseKeyDelivery { new_root, control_pk: Some(control_pk), control_root: None });
    }
    let mut control_root = [0u8; 32];
    control_root.copy_from_slice(&pt[104..136]);
    if control_signer_group_key(&control_root, community_id, epoch).pk().to_bytes() != control_pk {
        if pt.len() == 136 {
            return Err(RekeyError::ControlPairMismatch);
        }
        // Future form whose 104..136 bytes are no longer the secret: keep the
        // verified prefix fields, drop the unverifiable ones.
        return Ok(BaseKeyDelivery { new_root, control_pk: Some(control_pk), control_root: None });
    }
    Ok(BaseKeyDelivery { new_root, control_pk: Some(control_pk), control_root: Some(control_root) })
}

/// Build one BASE blob via a [`VectorSigner`] — the 104-byte member form, or
/// 136 with `control_root` for a staff recipient (CORD-04 §3). Mirrors
/// [`build_blob`]; the locator is the same public Root-scope locator.
pub async fn build_base_blob<S: crate::signer::VectorSigner + ?Sized>(
    signer: &S,
    rotator_xonly: &[u8; 32],
    recipient_pk: &PublicKey,
    epoch: Epoch,
    new_root: &[u8; 32],
    control_pk: &[u8; 32],
    control_root: Option<&[u8; 32]>,
) -> Result<RekeyBlob, RekeyError> {
    let inner_b64 = Zeroizing::new(bound_base_plaintext_b64(epoch, new_root, control_pk, control_root));
    let wrapped = signer
        .nip44_encrypt_async(recipient_pk, inner_b64.as_str())
        .await
        .map_err(|e| RekeyError::Crypto(e.to_string()))?;
    Ok(RekeyBlob {
        locator: blob_locator(rotator_xonly, &recipient_pk.to_bytes(), RekeyScope::Root, epoch),
        wrapped,
    })
}

/// Open a BASE blob addressed to me via a [`VectorSigner`]. Mirror of
/// [`open_blob`], but width-tolerant per CORD-06 §1 — the returned delivery
/// says which form arrived.
pub async fn open_base_blob<S: crate::signer::VectorSigner + ?Sized>(
    signer: &S,
    rotator_pk: &PublicKey,
    community_id: &CommunityId,
    epoch: Epoch,
    blob: &RekeyBlob,
) -> Result<BaseKeyDelivery, RekeyError> {
    let inner_b64 = Zeroizing::new(
        signer
            .nip44_decrypt_async(rotator_pk, &blob.wrapped)
            .await
            .map_err(|e| RekeyError::Crypto(e.to_string()))?,
    );
    let pt = Zeroizing::new(
        base64_simd::STANDARD
            .decode_to_vec(inner_b64.as_bytes())
            .map_err(|e| RekeyError::Crypto(e.to_string()))?,
    );
    parse_bound_base_plaintext(&pt, community_id, epoch)
}

// ── The Grant's control_wrap plaintext (CORD-04 §3) ──────────────────────────

/// `epoch_be[8] ‖ control_root[32]` — the staff write key as delivered inside a
/// staff-making Grant, NIP-44-encrypted under the granter↔member pairwise key
/// (the rekey-blob discipline: fixed width, the epoch INSIDE the ciphertext).
pub fn encode_control_wrap(epoch: Epoch, control_root: &[u8; 32]) -> [u8; 40] {
    let mut pt = [0u8; 40];
    pt[..8].copy_from_slice(&epoch.0.to_be_bytes());
    pt[8..].copy_from_slice(control_root);
    pt
}

/// The base64 string a control_wrap's NIP-44 layer encrypts (string-typed
/// signer surfaces, the D5 transport discipline).
pub fn control_wrap_b64(epoch: Epoch, control_root: &[u8; 32]) -> String {
    base64_simd::STANDARD.encode_to_string(encode_control_wrap(epoch, control_root))
}

/// Parse a decrypted 40-byte control_wrap plaintext. The caller verifies the
/// secret derives to the `control_pk` it holds for the named epoch — any
/// mismatch is dropped, never adopted (CORD-04 §3).
pub fn parse_control_wrap(pt: &[u8]) -> Result<(Epoch, [u8; 32]), RekeyError> {
    if pt.len() != 40 {
        return Err(RekeyError::BadControlWrapLength(pt.len()));
    }
    let mut epoch_be = [0u8; 8];
    epoch_be.copy_from_slice(&pt[..8]);
    let mut control_root = [0u8; 32];
    control_root.copy_from_slice(&pt[8..]);
    Ok((Epoch(u64::from_be_bytes(epoch_be)), control_root))
}

/// The public per-recipient locator (D1). Both parties compute it from public
/// keys alone; it addresses the blob and nothing more.
pub fn blob_locator(rotator_xonly: &[u8; 32], recipient_xonly: &[u8; 32], scope: RekeyScope, epoch: Epoch) -> String {
    crate::simd::hex::bytes_to_hex_32(&recipient_locator(rotator_xonly, recipient_xonly, &scope.id32(), epoch))
}

/// Build one blob with LOCAL keys (the bunker path drives the same wire via the
/// `_b64` helpers + a NIP-46 `nip44_encrypt`). The wrap is the pairwise
/// conversation key `ConversationKey::derive(rotator_sk, recipient_pk)`, so only
/// the recipient's identity key opens it.
pub fn build_blob_local(
    rotator_sk: &SecretKey,
    rotator_xonly: &[u8; 32],
    recipient_pk: &PublicKey,
    scope: RekeyScope,
    epoch: Epoch,
    new_key: &[u8; 32],
) -> Result<RekeyBlob, RekeyError> {
    let inner_b64 = Zeroizing::new(bound_plaintext_b64(scope, epoch, new_key));
    let ck = ConversationKey::derive(rotator_sk, recipient_pk).map_err(|e| RekeyError::Crypto(e.to_string()))?;
    let payload = crate::community::cipher::encrypt_with_random_nonce(&ck, inner_b64.as_bytes()).map_err(|e| RekeyError::Crypto(e.to_string()))?;
    Ok(RekeyBlob {
        locator: blob_locator(rotator_xonly, &recipient_pk.to_bytes(), scope, epoch),
        wrapped: base64_simd::STANDARD.encode_to_string(&payload),
    })
}

/// Open a blob addressed to me with LOCAL keys. Per D1 this does NOT check the
/// locator: the decrypt (only my identity key opens a blob wrapped to me by the
/// rotator) plus the bound scope/epoch ARE the authenticity boundary. A blob
/// relocated to a foreign locator still won't decrypt for a non-recipient, and
/// a spliced one fails the bound check.
pub fn open_blob_local(
    my_sk: &SecretKey,
    rotator_pk: &PublicKey,
    scope: RekeyScope,
    epoch: Epoch,
    blob: &RekeyBlob,
) -> Result<[u8; 32], RekeyError> {
    let ck = ConversationKey::derive(my_sk, rotator_pk).map_err(|e| RekeyError::Crypto(e.to_string()))?;
    let payload = base64_simd::STANDARD
        .decode_to_vec(blob.wrapped.as_bytes())
        .map_err(|e| RekeyError::Crypto(e.to_string()))?;
    let inner_b64 = Zeroizing::new(decrypt_to_bytes(&ck, &payload).map_err(|e| RekeyError::Crypto(e.to_string()))?);
    let pt = Zeroizing::new(
        base64_simd::STANDARD
            .decode_to_vec(inner_b64.as_slice())
            .map_err(|e| RekeyError::Crypto(e.to_string()))?,
    );
    parse_bound_plaintext(&pt, scope, epoch)
}

/// Build one blob via a [`VectorSigner`] (the bunker / NIP-55 path). Wire-identical
/// to [`build_blob_local`]: `signer.nip44_encrypt(recipient, bound_plaintext_b64)`
/// whose conversation key is ECDH(signer_identity, recipient) — the same key the
/// local path derives from the raw rotator secret. The `wrapped` field is the
/// standard NIP-44 payload string (D5), so both paths emit identical wire.
pub async fn build_blob<S: crate::signer::VectorSigner + ?Sized>(
    signer: &S,
    rotator_xonly: &[u8; 32],
    recipient_pk: &PublicKey,
    scope: RekeyScope,
    epoch: Epoch,
    new_key: &[u8; 32],
) -> Result<RekeyBlob, RekeyError> {
    let inner_b64 = Zeroizing::new(bound_plaintext_b64(scope, epoch, new_key));
    let wrapped = signer
        .nip44_encrypt_async(recipient_pk, inner_b64.as_str())
        .await
        .map_err(|e| RekeyError::Crypto(e.to_string()))?;
    Ok(RekeyBlob {
        locator: blob_locator(rotator_xonly, &recipient_pk.to_bytes(), scope, epoch),
        wrapped,
    })
}

/// Open a blob addressed to me via a [`VectorSigner`]. Mirror of [`open_blob_local`]:
/// `signer.nip44_decrypt(rotator, blob.wrapped)` yields the base64 bound plaintext,
/// then the scope/epoch bound check gates it. Per D1 the locator is NOT gated.
pub async fn open_blob<S: crate::signer::VectorSigner + ?Sized>(
    signer: &S,
    rotator_pk: &PublicKey,
    scope: RekeyScope,
    epoch: Epoch,
    blob: &RekeyBlob,
) -> Result<[u8; 32], RekeyError> {
    let inner_b64 = Zeroizing::new(
        signer
            .nip44_decrypt_async(rotator_pk, &blob.wrapped)
            .await
            .map_err(|e| RekeyError::Crypto(e.to_string()))?,
    );
    let pt = Zeroizing::new(
        base64_simd::STANDARD
            .decode_to_vec(inner_b64.as_bytes())
            .map_err(|e| RekeyError::Crypto(e.to_string()))?,
    );
    parse_bound_plaintext(&pt, scope, epoch)
}

/// Find my blob in a chunk's array by my public locator (the lookup step, D1).
/// `None` means this chunk doesn't carry my key — never a removal on its own
/// (only "removed" once ALL chunks are held and none has it).
pub fn find_my_blob<'a>(
    blobs: &'a [RekeyBlob],
    rotator_xonly: &[u8; 32],
    my_xonly: &[u8; 32],
    scope: RekeyScope,
    epoch: Epoch,
) -> Option<&'a RekeyBlob> {
    let want = blob_locator(rotator_xonly, my_xonly, scope, epoch);
    blobs.iter().find(|b| b.locator == want)
}

// ── The 3303 event (a v2 stream event) ───────────────────────────────────────

/// A parsed, seal-verified 3303 chunk. The `rotator` is the seal's real signer
/// (the ECDH counterparty AND the authority actor the caller gates on).
#[derive(Debug, Clone)]
pub struct RekeyChunk {
    pub rotator: PublicKey,
    pub scope: RekeyScope,
    pub new_epoch: Epoch,
    pub prev_epoch: Epoch,
    pub prev_commit: [u8; 32],
    /// This chunk's `(i, n)` — 1-based, `i <= n`.
    pub chunk: (u32, u32),
    pub blobs: Vec<RekeyBlob>,
    /// The rotator's `vac` (CORD-06 §Authority: "a rotation cites the Grant it
    /// acts under like any authority action"). `None` when the owner rotates.
    pub citation: Option<crate::community::edition::AuthorityCitation>,
}

/// The key that groups chunks of ONE rotation: `(rotator, scope_id, new_epoch,
/// prev_commit)`. Two rotators racing the same epoch, or one rotator over two
/// channels, never alias.
pub type RotationKey = ([u8; 32], [u8; 32], u64, [u8; 32]);

impl RekeyChunk {
    /// This chunk's [`RotationKey`].
    pub fn correlation(&self) -> RotationKey {
        (self.rotator.to_bytes(), self.scope.id32(), self.new_epoch.0, self.prev_commit)
    }
}

/// Build the unsigned 3303 rumor (rotator is the pubkey; the seal will carry the
/// signature). Enforces the monotonic-epoch and chunk-range invariants at mint.
#[allow(clippy::too_many_arguments)]
pub fn build_rekey_rumor(
    rotator: PublicKey,
    scope: RekeyScope,
    new_epoch: Epoch,
    prev_epoch: Epoch,
    prev_commit: &[u8; 32],
    blobs: &[RekeyBlob],
    chunk_i: u32,
    chunk_n: u32,
    at_secs: u64,
    citation: Option<&crate::community::edition::AuthorityCitation>,
) -> Result<UnsignedEvent, RekeyError> {
    if new_epoch.0 <= prev_epoch.0 {
        return Err(RekeyError::NonMonotonicEpoch);
    }
    if chunk_n < 1 || chunk_i < 1 || chunk_i > chunk_n {
        return Err(RekeyError::BadChunkIndex);
    }
    if blobs.len() > MAX_REKEY_BLOBS_PER_EVENT {
        return Err(RekeyError::TooManyBlobs(blobs.len()));
    }
    let content = serde_json::to_string(blobs).map_err(|e| RekeyError::Crypto(e.to_string()))?;
    let mut tags = vec![
        Tag::custom(TAG_SCOPE, [scope.to_hex()]),
        Tag::custom(TAG_NEW_EPOCH, [new_epoch.0.to_string()]),
        Tag::custom(TAG_PREV_EPOCH, [prev_epoch.0.to_string()]),
        Tag::custom(TAG_PREV_COMMIT, [crate::simd::hex::bytes_to_hex_32(prev_commit)]),
        Tag::custom(TAG_CHUNK, [chunk_i.to_string(), chunk_n.to_string()]),
    ];
    // CORD-06 §Authority: "a rotation cites the Grant it acts under like any
    // authority action (CORD-04's `vac`), so a just-demoted admin's rotation is
    // never honored by a lagging client." The owner cites nothing.
    if let Some(c) = citation {
        tags.push(c.to_tag());
    }
    // Rekeys fold by their tags, not time; still stamp created_at for the wire.
    Ok(stream::build_rumor_secs(super::kind::REKEY, rotator, &content, tags, at_secs))
}

/// The rekey group key for a CHANNEL rekey addressed under `addressing_root`.
/// The caller chooses the root: a STANDALONE channel rekey rides the CURRENT
/// root (`MANAGE_CHANNELS`); a channel rekey forced by a removal rides the PRIOR
/// root alongside the base rekey (D2 — inherits the removal's `BAN` authority,
/// and the prior-root address is exactly what distinguishes the two classes on
/// the wire so a base-fork loser can still open it).
pub fn channel_rekey_group(addressing_root: &[u8; 32], channel_id: &ChannelId, new_epoch: Epoch) -> GroupKey {
    channel_rekey_group_key(addressing_root, channel_id, new_epoch)
}

/// The rekey group key for a BASE rotation — always under the PRIOR root (the
/// one handle every retained member still holds through the rotation).
pub fn base_rekey_group(prior_root: &[u8; 32], community_id: &CommunityId, new_epoch: Epoch) -> GroupKey {
    base_rekey_group_key(prior_root, community_id, new_epoch)
}

/// Seal + wrap a 3303 rumor into its stream event at the rekey address. The seal
/// is ENCRYPTED (20013 — the rekey plane MUST NOT be plaintext-sealed) and
/// signed by the rotator; the wrap by the rekey group key.
pub fn seal_rekey_chunk(
    rumor: &UnsignedEvent,
    rekey_group: &GroupKey,
    rotator_keys: &Keys,
    wrap_at: Timestamp,
) -> Result<(Event, Keys), RekeyError> {
    let seal = stream::build_seal(rumor, SealForm::Encrypted, rekey_group, rotator_keys)?;
    Ok(stream::wrap_seal(&seal, rekey_group, stream::KIND_WRAP, wrap_at)?)
}

/// Split a full recipient blob set into 3303 chunk events (≤120 blobs each),
/// all sharing the rotation's `(scope, new_epoch, prev_commit)` so a receiver
/// correlates them. Local-keys convenience.
#[allow(clippy::too_many_arguments)]
pub fn build_rekey_chunks_local(
    rotator_keys: &Keys,
    rekey_group: &GroupKey,
    scope: RekeyScope,
    new_epoch: Epoch,
    prev_epoch: Epoch,
    prev_commit: &[u8; 32],
    blobs: &[RekeyBlob],
    at_secs: u64,
    citation: Option<&crate::community::edition::AuthorityCitation>,
) -> Result<Vec<Event>, RekeyError> {
    let groups: Vec<&[RekeyBlob]> = if blobs.is_empty() {
        vec![&[]]
    } else {
        blobs.chunks(MAX_REKEY_BLOBS_PER_EVENT).collect()
    };
    let n = groups.len() as u32;
    let mut out = Vec::with_capacity(groups.len());
    for (idx, group_blobs) in groups.iter().enumerate() {
        let rumor = build_rekey_rumor(
            rotator_keys.public_key(),
            scope,
            new_epoch,
            prev_epoch,
            prev_commit,
            group_blobs,
            idx as u32 + 1,
            n,
            at_secs,
            citation,
        )?;
        let (wrap, _) = seal_rekey_chunk(&rumor, rekey_group, rotator_keys, Timestamp::from_secs(at_secs))?;
        out.push(wrap);
    }
    Ok(out)
}

/// Signer-driven twin of [`build_rekey_chunks_local`] for bunker / NIP-55 accounts:
/// each chunk's encrypted seal signs through a [`VectorSigner`]. `rotator_pk` must
/// equal `my_public_key()`. Wire-identical to the local path.
#[allow(clippy::too_many_arguments)]
pub async fn build_rekey_chunks<S: crate::signer::VectorSigner + ?Sized>(
    signer: &S,
    rotator_pk: PublicKey,
    rekey_group: &GroupKey,
    scope: RekeyScope,
    new_epoch: Epoch,
    prev_epoch: Epoch,
    prev_commit: &[u8; 32],
    blobs: &[RekeyBlob],
    at_secs: u64,
    citation: Option<&crate::community::edition::AuthorityCitation>,
) -> Result<Vec<Event>, RekeyError> {
    let groups: Vec<&[RekeyBlob]> = if blobs.is_empty() {
        vec![&[]]
    } else {
        blobs.chunks(MAX_REKEY_BLOBS_PER_EVENT).collect()
    };
    let n = groups.len() as u32;
    let mut out = Vec::with_capacity(groups.len());
    for (idx, group_blobs) in groups.iter().enumerate() {
        let rumor = build_rekey_rumor(rotator_pk, scope, new_epoch, prev_epoch, prev_commit, group_blobs, idx as u32 + 1, n, at_secs, citation)?;
        let (wrap, _) = stream::seal_and_wrap_signed(signer, rotator_pk, &rumor, SealForm::Encrypted, rekey_group, stream::KIND_WRAP, Timestamp::from_secs(at_secs), &[]).await?;
        out.push(wrap);
    }
    Ok(out)
}

/// Parse a 3303 chunk from a seal-verified stream open. Rejects a non-3303
/// rumor, a plaintext seal (the rekey plane is encrypted-only), malformed or
/// duplicate machinery tags, a bad chunk range, and an over-cap blob array.
pub fn parse_rekey_chunk(opened: &OpenedStream) -> Result<RekeyChunk, RekeyError> {
    if opened.seal_form != SealForm::Encrypted {
        // A plaintext-sealed rekey would be a liftable public artifact — reject.
        return Err(RekeyError::Stream(StreamError::BadSealKind(stream::KIND_SEAL_PLAINTEXT)));
    }
    let rumor = &opened.rumor;
    if rumor.kind.as_u16() != super::kind::REKEY {
        return Err(RekeyError::NotARekey(rumor.kind.as_u16()));
    }
    let scope = RekeyScope::from_hex(&unique_tag(rumor, TAG_SCOPE)?.ok_or(RekeyError::BadTag(TAG_SCOPE))?)
        .ok_or(RekeyError::BadTag(TAG_SCOPE))?;
    let new_epoch = Epoch(parse_u64(rumor, TAG_NEW_EPOCH)?);
    let prev_epoch = Epoch(parse_u64(rumor, TAG_PREV_EPOCH)?);
    if new_epoch.0 <= prev_epoch.0 {
        return Err(RekeyError::NonMonotonicEpoch);
    }
    let prev_hex = unique_tag(rumor, TAG_PREV_COMMIT)?.ok_or(RekeyError::BadTag(TAG_PREV_COMMIT))?;
    if prev_hex.len() != 64 || !prev_hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(RekeyError::BadTag(TAG_PREV_COMMIT));
    }
    let prev_commit = crate::simd::hex::hex_to_bytes_32(&prev_hex);

    let (chunk_i, chunk_n) = parse_chunk(rumor)?;

    let blobs: Vec<RekeyBlob> = serde_json::from_str(&rumor.content).map_err(|_| RekeyError::BadTag("blobs"))?;
    if blobs.len() > MAX_REKEY_BLOBS_RECEIVED {
        return Err(RekeyError::TooManyBlobs(blobs.len()));
    }

    Ok(RekeyChunk {
        rotator: opened.author,
        scope,
        new_epoch,
        prev_epoch,
        prev_commit,
        chunk: (chunk_i, chunk_n),
        blobs,
        citation: crate::community::edition::AuthorityCitation::from_tags(&rumor.tags),
    })
}

// ── Continuity + removal + fork resolution (CORD-06 §2/§3) ───────────────────

/// The verdict of the prevcommit continuity check (CORD-06 §2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Continuity {
    /// The commitment matches the key I hold at `prev_epoch` — this rotation
    /// extends my chain; adopt it.
    Extends,
    /// `prev_epoch` is higher than the key I hold — I missed a rotation; fetch
    /// the gap first, don't adopt yet.
    Gap,
    /// The commitment doesn't match at the same epoch — a fork or garbage.
    Fork,
}

/// Check a rotation's `prev_commit` against the `(epoch, key)` I currently hold
/// for its scope. A match proves the rotation extends the very key I hold; a
/// higher `prev_epoch` means I'm behind; anything else is a fork/garbage.
pub fn check_continuity(chunk: &RekeyChunk, held_epoch: Epoch, held_key: &[u8; 32]) -> Continuity {
    if chunk.prev_epoch.0 == held_epoch.0 {
        if epoch_key_commitment(held_epoch, held_key) == chunk.prev_commit {
            Continuity::Extends
        } else {
            Continuity::Fork
        }
    } else if chunk.prev_epoch.0 > held_epoch.0 {
        Continuity::Gap
    } else {
        // prev_epoch < held: this rotation is older than where I am — a stale
        // fork; a settled epoch only ever heals DOWN to a sibling, never back.
        Continuity::Fork
    }
}

/// A collected rotation: all chunks sharing one correlation key, and whether the
/// set is complete (all `n` chunks present).
#[derive(Debug, Clone)]
pub struct Rotation {
    pub rotator: PublicKey,
    pub scope: RekeyScope,
    pub new_epoch: Epoch,
    pub prev_epoch: Epoch,
    pub prev_commit: [u8; 32],
    /// The union of every chunk's blobs.
    pub blobs: Vec<RekeyBlob>,
    /// Total chunk count `n` declared by the chunks.
    pub declared_chunks: u32,
    /// Distinct chunk indices actually held.
    pub held_chunks: std::collections::BTreeSet<u32>,
    /// The rotator's `vac`, taken from the first chunk seen (every chunk of one
    /// rotation carries the same citation — they share a signer and an action).
    pub citation: Option<crate::community::edition::AuthorityCitation>,
}

impl Rotation {
    /// True once every declared chunk index `1..=n` is held — the precondition
    /// for concluding removal (a missing chunk is "keep recovering", never a
    /// removal).
    pub fn is_complete(&self) -> bool {
        self.declared_chunks >= 1 && (1..=self.declared_chunks).all(|i| self.held_chunks.contains(&i))
    }

    /// This rotation's continuity against the `(epoch, key)` I hold for its scope
    /// — the [`check_continuity`] verdict at the aggregated-rotation level (same
    /// prevcommit test), so a follower can gate adoption without a raw chunk.
    pub fn continuity(&self, held_epoch: Epoch, held_key: &[u8; 32]) -> Continuity {
        if self.prev_epoch.0 == held_epoch.0 {
            if epoch_key_commitment(held_epoch, held_key) == self.prev_commit {
                Continuity::Extends
            } else {
                Continuity::Fork
            }
        } else if self.prev_epoch.0 > held_epoch.0 {
            Continuity::Gap
        } else {
            Continuity::Fork
        }
    }
}

/// Group a batch of parsed chunks into rotations by correlation key. Chunks whose
/// `n` disagrees with their siblings, or that repeat an index, are the caller's
/// concern to police; here the first-seen `n` per correlation wins and duplicate
/// indices are ignored (idempotent re-delivery).
pub fn collect_rotations(chunks: &[RekeyChunk]) -> Vec<Rotation> {
    use std::collections::BTreeMap;
    let mut by_key: BTreeMap<RotationKey, Rotation> = BTreeMap::new();
    for c in chunks {
        let entry = by_key.entry(c.correlation()).or_insert_with(|| Rotation {
            rotator: c.rotator,
            scope: c.scope,
            new_epoch: c.new_epoch,
            prev_epoch: c.prev_epoch,
            prev_commit: c.prev_commit,
            blobs: Vec::new(),
            declared_chunks: c.chunk.1,
            held_chunks: std::collections::BTreeSet::new(),
            citation: c.citation.clone(),
        });
        if entry.held_chunks.insert(c.chunk.0) {
            entry.blobs.extend(c.blobs.iter().cloned());
        }
    }
    by_key.into_values().collect()
}

/// Have I been removed by this rotation? Only answerable on a COMPLETE rotation
/// (all `n` chunks held): I'm removed iff none of the union's blobs carries my
/// locator. On an incomplete rotation the answer is `None` — keep recovering.
pub fn am_i_removed(rotation: &Rotation, my_xonly: &[u8; 32]) -> Option<bool> {
    if !rotation.is_complete() {
        return None;
    }
    let mine = find_my_blob(&rotation.blobs, &rotation.rotator.to_bytes(), my_xonly, rotation.scope, rotation.new_epoch);
    Some(mine.is_none())
}

/// Deterministic same-epoch fork winner (CORD-06 §3): among candidate rotations
/// at one continuity point, the one whose decrypted `new_key` is lexicographically
/// lowest wins. Every retained member decrypts its own blob from each fork and
/// computes the identical winner. Returns the index into `candidates` of the
/// winner, or `None` if the caller decrypted no candidate.
pub fn lowest_key_winner(candidate_keys: &[[u8; 32]]) -> Option<usize> {
    candidate_keys
        .iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| a.cmp(b))
        .map(|(i, _)| i)
}

// ── helpers ──────────────────────────────────────────────────────────────────

fn unique_tag(rumor: &UnsignedEvent, name: &'static str) -> Result<Option<String>, RekeyError> {
    let mut found: Option<String> = None;
    for t in rumor.tags.iter() {
        let s = t.as_slice();
        if s.len() >= 2 && s[0] == name {
            if found.is_some() {
                return Err(RekeyError::BadTag(name));
            }
            found = Some(s[1].clone());
        }
    }
    Ok(found)
}

fn parse_u64(rumor: &UnsignedEvent, name: &'static str) -> Result<u64, RekeyError> {
    let raw = unique_tag(rumor, name)?.ok_or(RekeyError::BadTag(name))?;
    // Spec-shaped decimal, not a bare `parse` — that takes "+2" and "02" as
    // epochs a stricter peer refuses (CORD-01 §5).
    if !crate::community::edition::is_tag_decimal(&raw) {
        return Err(RekeyError::BadTag(name));
    }
    raw.parse::<u64>().map_err(|_| RekeyError::BadTag(name))
}

fn parse_chunk(rumor: &UnsignedEvent) -> Result<(u32, u32), RekeyError> {
    let mut found: Option<(u32, u32)> = None;
    for t in rumor.tags.iter() {
        let s = t.as_slice();
        if s.len() >= 3 && s[0] == TAG_CHUNK {
            if found.is_some() {
                return Err(RekeyError::BadTag(TAG_CHUNK));
            }
            if !crate::community::edition::is_tag_decimal(&s[1]) || !crate::community::edition::is_tag_decimal(&s[2]) {
                return Err(RekeyError::BadTag(TAG_CHUNK));
            }
            let i: u32 = s[1].parse().map_err(|_| RekeyError::BadTag(TAG_CHUNK))?;
            let n: u32 = s[2].parse().map_err(|_| RekeyError::BadTag(TAG_CHUNK))?;
            found = Some((i, n));
        }
    }
    let (i, n) = found.ok_or(RekeyError::BadTag(TAG_CHUNK))?;
    if n < 1 || i < 1 || i > n {
        return Err(RekeyError::BadChunkIndex);
    }
    Ok((i, n))
}

#[cfg(test)]
mod tests {

    /// Wrap raw `Keys` as the polymorphic signer. `VectorSigner` pins its error to
    /// `SignerError`, which bare `Keys` doesn't satisfy, so tests go through the
    /// same enum the app uses.
    fn as_signer(k: &Keys) -> crate::signer::ActiveSigner {
        crate::signer::ActiveSigner::Keys(k.clone())
    }
    use super::*;

    fn keys(byte: u8) -> Keys {
        Keys::new(SecretKey::from_slice(&[byte; 32]).unwrap())
    }

    fn xonly(k: &Keys) -> [u8; 32] {
        k.public_key().to_bytes()
    }

    const CHAN: ChannelId = ChannelId([0x42u8; 32]);

    // ── blob atom ────────────────────────────────────────────────────────────

    #[test]
    fn bound_plaintext_layout_is_frozen() {
        let pt = bound_plaintext(RekeyScope::Root, Epoch(1), &[0xABu8; 32]);
        let expected = format!("{}{}{}", "00".repeat(32), "0000000000000001", "ab".repeat(32));
        assert_eq!(crate::simd::hex::bytes_to_hex_string(&pt), expected);
        let pt2 = bound_plaintext(RekeyScope::Channel(ChannelId([0x11u8; 32])), Epoch(0x0102), &[0xCDu8; 32]);
        let expected2 = format!("{}{}{}", "11".repeat(32), "0000000000000102", "cd".repeat(32));
        assert_eq!(crate::simd::hex::bytes_to_hex_string(&pt2), expected2);
    }

    #[test]
    fn blob_round_trips_both_scopes() {
        let rotator = keys(7);
        let recipient = keys(8);
        for (scope, epoch, key) in [
            (RekeyScope::Root, Epoch(1), [0xABu8; 32]),
            (RekeyScope::Channel(CHAN), Epoch(5), [0xCDu8; 32]),
        ] {
            let blob = build_blob_local(rotator.secret_key(), &xonly(&rotator), &recipient.public_key(), scope, epoch, &key).unwrap();
            let got = open_blob_local(recipient.secret_key(), &rotator.public_key(), scope, epoch, &blob).unwrap();
            assert_eq!(got, key, "the recipient recovers the fresh key");
        }
    }

    #[tokio::test]
    async fn signer_blob_is_wire_compatible_with_local_both_directions() {
        // A remote signer (here a plain Keys, which impls VectorSigner) must build
        // AND open blobs interchangeably with the raw-key path — the CORD-06 D5
        // "identical wire" guarantee the bunker/NIP-55 integration rests on.
        let rotator = keys(7);
        let recipient = keys(8);
        let scope = RekeyScope::Root;
        let epoch = Epoch(3);
        let key = [0x5Au8; 32];

        // local build -> signer open
        let blob_l = build_blob_local(rotator.secret_key(), &xonly(&rotator), &recipient.public_key(), scope, epoch, &key).unwrap();
        let got_s = open_blob(&as_signer(&recipient), &rotator.public_key(), scope, epoch, &blob_l).await.unwrap();
        assert_eq!(got_s, key, "signer opens a local-built blob");

        // signer build -> local open (+ identical public locator)
        let blob_s = build_blob(&as_signer(&rotator), &xonly(&rotator), &recipient.public_key(), scope, epoch, &key).await.unwrap();
        assert_eq!(blob_s.locator, blob_l.locator, "same public locator regardless of build path");
        let got_l = open_blob_local(recipient.secret_key(), &rotator.public_key(), scope, epoch, &blob_s).unwrap();
        assert_eq!(got_l, key, "local opens a signer-built blob");

        // signer build -> signer open
        let got_ss = open_blob(&as_signer(&recipient), &rotator.public_key(), scope, epoch, &blob_s).await.unwrap();
        assert_eq!(got_ss, key, "signer round-trips its own blob");
    }

    #[tokio::test]
    async fn signer_blob_bound_check_still_gates_scope_epoch_splice() {
        let rotator = keys(7);
        let recipient = keys(8);
        let blob = build_blob(&as_signer(&rotator), &xonly(&rotator), &recipient.public_key(), RekeyScope::Root, Epoch(1), &[9u8; 32]).await.unwrap();
        assert!(open_blob(&as_signer(&recipient), &rotator.public_key(), RekeyScope::Root, Epoch(2), &blob).await.is_err(), "epoch splice rejected");
        assert!(open_blob(&as_signer(&recipient), &rotator.public_key(), RekeyScope::Channel(CHAN), Epoch(1), &blob).await.is_err(), "scope splice rejected");
    }

    #[test]
    fn locator_is_public_and_computable_from_pubkeys_alone() {
        // D1: the locator derives from PUBLIC inputs, so the rotator computing a
        // recipient's slot and the recipient computing their own must agree — no
        // secret needed either side (bunker parity).
        let rotator = keys(7);
        let recipient = keys(8);
        let blob = build_blob_local(rotator.secret_key(), &xonly(&rotator), &recipient.public_key(), RekeyScope::Root, Epoch(2), &[1u8; 32]).unwrap();
        let recomputed = blob_locator(&xonly(&rotator), &xonly(&recipient), RekeyScope::Root, Epoch(2));
        assert_eq!(blob.locator, recomputed, "both sides compute the same public locator");
    }

    #[test]
    fn a_non_recipient_cannot_open_even_holding_the_public_locator() {
        // The D1 security relocation: the locator authenticates NOTHING (anyone
        // holding both npubs computes it), yet an outsider still can't open —
        // the pairwise decrypt is the gate, not the locator.
        let rotator = keys(7);
        let recipient = keys(8);
        let outsider = keys(9);
        let blob = build_blob_local(rotator.secret_key(), &xonly(&rotator), &recipient.public_key(), RekeyScope::Root, Epoch(1), &[2u8; 32]).unwrap();
        // The outsider can trivially recompute the (public) locator...
        assert_eq!(blob.locator, blob_locator(&xonly(&rotator), &xonly(&recipient), RekeyScope::Root, Epoch(1)));
        // ...but pairing (outsider_sk, rotator_pk) yields a different conversation
        // key, so the decrypt fails.
        assert!(open_blob_local(outsider.secret_key(), &rotator.public_key(), RekeyScope::Root, Epoch(1), &blob).is_err());
    }

    #[test]
    fn a_relocated_blob_still_opens_by_decrypt_not_locator() {
        // Because open ignores the locator (D1), corrupting the locator does NOT
        // break a legitimate recipient's open — the decrypt + bound check govern.
        // (Contrast v1, where a locator mismatch was a hard reject.) The FIND
        // step uses the locator; OPEN does not.
        let rotator = keys(7);
        let recipient = keys(8);
        let mut blob = build_blob_local(rotator.secret_key(), &xonly(&rotator), &recipient.public_key(), RekeyScope::Root, Epoch(1), &[3u8; 32]).unwrap();
        blob.locator = "ff".repeat(32);
        // Handed the blob directly (locator bypassed), the recipient still opens it.
        assert_eq!(
            open_blob_local(recipient.secret_key(), &rotator.public_key(), RekeyScope::Root, Epoch(1), &blob).unwrap(),
            [3u8; 32]
        );
        // But find_my_blob won't LOCATE it under the corrupted locator.
        assert!(find_my_blob(std::slice::from_ref(&blob), &xonly(&rotator), &xonly(&recipient), RekeyScope::Root, Epoch(1)).is_none());
    }

    #[test]
    fn scope_and_epoch_splices_are_rejected_on_open() {
        let rotator = keys(7);
        let recipient = keys(8);
        let blob = build_blob_local(rotator.secret_key(), &xonly(&rotator), &recipient.public_key(), RekeyScope::Root, Epoch(1), &[4u8; 32]).unwrap();
        // Opened under a different scope → bound scope mismatch.
        assert!(matches!(
            open_blob_local(recipient.secret_key(), &rotator.public_key(), RekeyScope::Channel(CHAN), Epoch(1), &blob),
            Err(RekeyError::ScopeSplice)
        ));
        // Opened under a different epoch → bound epoch mismatch.
        assert!(matches!(
            open_blob_local(recipient.secret_key(), &rotator.public_key(), RekeyScope::Root, Epoch(2), &blob),
            Err(RekeyError::EpochSplice)
        ));
    }

    #[test]
    fn wrapped_carries_base64_of_the_72_bytes_for_bunker_parity() {
        // D5: the NIP-44 layer's plaintext is base64(72 bytes) — a UTF-8 string a
        // NIP-46 signer can nip44_encrypt/decrypt. Prove the decrypted inner is
        // exactly that base64 string, matching the `_b64` helper the bunker uses.
        let rotator = keys(7);
        let recipient = keys(8);
        let blob = build_blob_local(rotator.secret_key(), &xonly(&rotator), &recipient.public_key(), RekeyScope::Root, Epoch(1), &[5u8; 32]).unwrap();
        let ck = ConversationKey::derive(recipient.secret_key(), &rotator.public_key()).unwrap();
        let payload = base64_simd::STANDARD.decode_to_vec(blob.wrapped.as_bytes()).unwrap();
        let inner = decrypt_to_bytes(&ck, &payload).unwrap();
        assert_eq!(String::from_utf8(inner).unwrap(), bound_plaintext_b64(RekeyScope::Root, Epoch(1), &[5u8; 32]));
    }

    #[test]
    fn distinct_recipients_and_scopes_get_distinct_locators() {
        let rotator = keys(7);
        let r1 = keys(8);
        let r2 = keys(9);
        assert_ne!(
            blob_locator(&xonly(&rotator), &xonly(&r1), RekeyScope::Root, Epoch(1)),
            blob_locator(&xonly(&rotator), &xonly(&r2), RekeyScope::Root, Epoch(1))
        );
        // Same pair, a base blob and a channel blob at one epoch don't collide.
        assert_ne!(
            blob_locator(&xonly(&rotator), &xonly(&r1), RekeyScope::Root, Epoch(1)),
            blob_locator(&xonly(&rotator), &xonly(&r1), RekeyScope::Channel(CHAN), Epoch(1))
        );
    }

    // ── 3303 event ─────────────────────────────────────────────────────────

    fn root() -> [u8; 32] {
        [0x55u8; 32]
    }

    #[test]
    fn channel_rekey_round_trips_through_the_stream() {
        let rotator = keys(1);
        let recipient = keys(8);
        let group = channel_rekey_group(&root(), &CHAN, Epoch(1));
        let key = [0xABu8; 32];
        let blob = build_blob_local(rotator.secret_key(), &xonly(&rotator), &recipient.public_key(), RekeyScope::Channel(CHAN), Epoch(1), &key).unwrap();
        let commit = epoch_key_commitment(Epoch(0), &[0xEEu8; 32]);
        let rumor = build_rekey_rumor(rotator.public_key(), RekeyScope::Channel(CHAN), Epoch(1), Epoch(0), &commit, &[blob.clone()], 1, 1, 100, None).unwrap();
        let (wrap, _) = seal_rekey_chunk(&rumor, &group, &rotator, Timestamp::from_secs(100)).unwrap();

        // The wrap is signed by the group key, not the rotator (no identity on the wire).
        assert_ne!(wrap.pubkey, rotator.public_key());
        assert_eq!(wrap.pubkey, group.pk());

        let opened = stream::open_wrap(&wrap, &group).unwrap();
        let chunk = parse_rekey_chunk(&opened).unwrap();
        assert_eq!(chunk.rotator, rotator.public_key(), "rotator recovered from the seal");
        assert!(matches!(chunk.scope, RekeyScope::Channel(c) if c.0 == CHAN.0));
        assert_eq!(chunk.new_epoch, Epoch(1));
        assert_eq!(chunk.prev_epoch, Epoch(0));
        assert_eq!(chunk.prev_commit, commit);
        assert_eq!(chunk.chunk, (1, 1));
        assert_eq!(chunk.blobs, vec![blob]);
    }

    #[test]
    fn base_rekey_addresses_under_the_prior_root() {
        // A base rotation rides the PRIOR root: a member holding the prior root
        // derives the same group key and opens it; a non-holder can't.
        let rotator = keys(1);
        let recipient = keys(8);
        let prior_root = [0x66u8; 32];
        let community = CommunityId([0x77u8; 32]);
        let new_root = [0x99u8; 32];
        let group = base_rekey_group(&prior_root, &community, Epoch(1));
        let blob = build_blob_local(rotator.secret_key(), &xonly(&rotator), &recipient.public_key(), RekeyScope::Root, Epoch(1), &new_root).unwrap();
        let commit = epoch_key_commitment(Epoch(0), &prior_root);
        let chunks = build_rekey_chunks_local(&rotator, &group, RekeyScope::Root, Epoch(1), Epoch(0), &commit, &[blob], 100, None).unwrap();
        assert_eq!(chunks.len(), 1);

        let opened = stream::open_wrap(&chunks[0], &group).unwrap();
        let chunk = parse_rekey_chunk(&opened).unwrap();
        assert!(matches!(chunk.scope, RekeyScope::Root));
        // The recipient recovers the NEW root from their blob.
        let mine = find_my_blob(&chunk.blobs, &chunk.rotator.to_bytes(), &xonly(&recipient), chunk.scope, chunk.new_epoch).unwrap();
        assert_eq!(open_blob_local(recipient.secret_key(), &chunk.rotator, chunk.scope, chunk.new_epoch, mine).unwrap(), new_root);

        // A non-holder of the prior root can't even open the wrap.
        let wrong = base_rekey_group(&[0u8; 32], &community, Epoch(1));
        assert!(stream::open_wrap(&chunks[0], &wrong).is_err());
    }

    #[test]
    fn a_full_send_chunk_stays_under_the_relay_size_limit() {
        // The send cap exists so one chunk fits a 64KB strfry event. A full chunk
        // must serialize under it or relays reject the event.
        let rotator = keys(1);
        let group = base_rekey_group(&root(), &CommunityId([9u8; 32]), Epoch(1));
        let blobs: Vec<RekeyBlob> = (0..MAX_REKEY_BLOBS_PER_EVENT)
            .map(|i| {
                let r = keys((i % 200 + 20) as u8);
                build_blob_local(rotator.secret_key(), &xonly(&rotator), &r.public_key(), RekeyScope::Root, Epoch(1), &[0xCDu8; 32]).unwrap()
            })
            .collect();
        let chunks = build_rekey_chunks_local(&rotator, &group, RekeyScope::Root, Epoch(1), Epoch(0), &[0u8; 32], &blobs, 100, None).unwrap();
        assert_eq!(chunks.len(), 1, "a full send chunk is exactly one event");
        assert!(chunks[0].as_json().len() <= 65_536, "a full chunk must fit a 64KB relay event");
    }

    #[test]
    fn oversize_recipient_set_splits_into_chunks() {
        let rotator = keys(1);
        let group = base_rekey_group(&root(), &CommunityId([9u8; 32]), Epoch(1));
        // One over the send cap → 2 chunks, labeled (1,2) and (2,2).
        let blobs: Vec<RekeyBlob> = (0..MAX_REKEY_BLOBS_PER_EVENT + 1)
            .map(|_| RekeyBlob { locator: "aa".repeat(32), wrapped: "x".into() })
            .collect();
        let chunks = build_rekey_chunks_local(&rotator, &group, RekeyScope::Root, Epoch(2), Epoch(1), &[0u8; 32], &blobs, 100, None).unwrap();
        assert_eq!(chunks.len(), 2);
        let parsed: Vec<RekeyChunk> = chunks.iter().map(|w| parse_rekey_chunk(&stream::open_wrap(w, &group).unwrap()).unwrap()).collect();
        assert_eq!(parsed[0].chunk, (1, 2));
        assert_eq!(parsed[1].chunk, (2, 2));
        assert_eq!(parsed[0].blobs.len(), MAX_REKEY_BLOBS_PER_EVENT);
        assert_eq!(parsed[1].blobs.len(), 1);
    }

    #[test]
    fn plaintext_sealed_rekey_is_rejected() {
        let rotator = keys(1);
        let group = channel_rekey_group(&root(), &CHAN, Epoch(1));
        let rumor = build_rekey_rumor(rotator.public_key(), RekeyScope::Channel(CHAN), Epoch(1), Epoch(0), &[0u8; 32], &[], 1, 1, 100, None).unwrap();
        let seal = stream::build_seal(&rumor, SealForm::Plaintext, &group, &rotator).unwrap();
        let (wrap, _) = stream::wrap_seal(&seal, &group, stream::KIND_WRAP, Timestamp::from_secs(1)).unwrap();
        let opened = stream::open_wrap(&wrap, &group).unwrap();
        assert!(parse_rekey_chunk(&opened).is_err(), "the rekey plane must be encrypted-sealed");
    }

    #[test]
    fn non_monotonic_epoch_is_refused_at_mint_and_on_parse() {
        let rotator = keys(1);
        assert!(matches!(
            build_rekey_rumor(rotator.public_key(), RekeyScope::Root, Epoch(1), Epoch(1), &[0u8; 32], &[], 1, 1, 100, None),
            Err(RekeyError::NonMonotonicEpoch)
        ));
    }

    #[test]
    fn bad_chunk_indices_are_refused() {
        let rotator = keys(1);
        for (i, n) in [(0u32, 1u32), (2, 1), (1, 0)] {
            assert!(
                matches!(build_rekey_rumor(rotator.public_key(), RekeyScope::Root, Epoch(1), Epoch(0), &[0u8; 32], &[], i, n, 100, None), Err(RekeyError::BadChunkIndex)),
                "chunk ({i},{n}) must be rejected"
            );
        }
    }

    // ── continuity + removal + fork ──────────────────────────────────────────

    fn chunk_at(rotator: &Keys, scope: RekeyScope, new_epoch: u64, prev_epoch: u64, prev_key: &[u8; 32], blobs: Vec<RekeyBlob>, i: u32, n: u32) -> RekeyChunk {
        RekeyChunk {
            rotator: rotator.public_key(),
            scope,
            new_epoch: Epoch(new_epoch),
            prev_epoch: Epoch(prev_epoch),
            prev_commit: epoch_key_commitment(Epoch(prev_epoch), prev_key),
            chunk: (i, n),
            blobs,
            citation: None,
        }
    }

    #[test]
    fn continuity_extends_gaps_and_forks() {
        let rotator = keys(1);
        let held = [0x33u8; 32];
        // Extends: prev_epoch == held epoch AND commitment matches the held key.
        let good = chunk_at(&rotator, RekeyScope::Root, 3, 2, &held, vec![], 1, 1);
        assert_eq!(check_continuity(&good, Epoch(2), &held), Continuity::Extends);
        // Gap: the rotation is FROM a higher epoch than I hold — I missed one.
        let ahead = chunk_at(&rotator, RekeyScope::Root, 5, 4, &held, vec![], 1, 1);
        assert_eq!(check_continuity(&ahead, Epoch(2), &held), Continuity::Gap);
        // Fork: same epoch but the commitment names a different prior key.
        let fork = chunk_at(&rotator, RekeyScope::Root, 3, 2, &[0x99u8; 32], vec![], 1, 1);
        assert_eq!(check_continuity(&fork, Epoch(2), &held), Continuity::Fork);
        // Fork: a rotation older than where I am (stale).
        let stale = chunk_at(&rotator, RekeyScope::Root, 2, 1, &held, vec![], 1, 1);
        assert_eq!(check_continuity(&stale, Epoch(2), &held), Continuity::Fork);
    }

    #[test]
    fn a_missing_chunk_is_never_a_removal() {
        // The core no-false-removal guarantee: until ALL n chunks are held, "am I
        // removed" is unanswerable, even if the chunks I DO hold lack my blob.
        let rotator = keys(1);
        let me = keys(8);
        // Two-chunk rotation; my blob is in chunk 2, which I haven't received.
        let my_blob = build_blob_local(rotator.secret_key(), &xonly(&rotator), &me.public_key(), RekeyScope::Root, Epoch(1), &[0xAAu8; 32]).unwrap();
        let c1 = chunk_at(&rotator, RekeyScope::Root, 1, 0, &[0u8; 32], vec![RekeyBlob { locator: "bb".repeat(32), wrapped: "x".into() }], 1, 2);
        let rots = collect_rotations(&[c1.clone()]);
        assert_eq!(rots.len(), 1);
        assert!(!rots[0].is_complete(), "one of two chunks held → incomplete");
        assert_eq!(am_i_removed(&rots[0], &xonly(&me)), None, "incomplete → keep recovering, never conclude removal");

        // Now chunk 2 (with my blob) arrives → complete, and I am NOT removed.
        let c2 = chunk_at(&rotator, RekeyScope::Root, 1, 0, &[0u8; 32], vec![my_blob.clone()], 2, 2);
        let rots = collect_rotations(&[c1, c2]);
        assert!(rots[0].is_complete());
        assert_eq!(am_i_removed(&rots[0], &xonly(&me)), Some(false));
        // And my key is recoverable from the union.
        let mine = find_my_blob(&rots[0].blobs, &rots[0].rotator.to_bytes(), &xonly(&me), RekeyScope::Root, Epoch(1)).unwrap();
        assert_eq!(open_blob_local(me.secret_key(), &rotator.public_key(), RekeyScope::Root, Epoch(1), mine).unwrap(), [0xAAu8; 32]);
    }

    #[test]
    fn a_complete_rotation_without_my_blob_is_a_removal() {
        let rotator = keys(1);
        let me = keys(8);
        let other = keys(9);
        // A complete 1-chunk rotation carrying only someone else's blob.
        let their_blob = build_blob_local(rotator.secret_key(), &xonly(&rotator), &other.public_key(), RekeyScope::Root, Epoch(1), &[0xBBu8; 32]).unwrap();
        let c = chunk_at(&rotator, RekeyScope::Root, 1, 0, &[0u8; 32], vec![their_blob], 1, 1);
        let rots = collect_rotations(&[c]);
        assert!(rots[0].is_complete());
        assert_eq!(am_i_removed(&rots[0], &xonly(&me)), Some(true), "complete rotation, no blob for me → removed");
    }

    #[test]
    fn collect_rotations_separates_concurrent_rotators_and_scopes() {
        // Two rotators racing the same epoch, plus a channel rekey at the same
        // numbers — three distinct rotations, never merged.
        let rot_a = keys(1);
        let rot_b = keys(2);
        let ca = chunk_at(&rot_a, RekeyScope::Root, 2, 1, &[7u8; 32], vec![], 1, 1);
        let cb = chunk_at(&rot_b, RekeyScope::Root, 2, 1, &[7u8; 32], vec![], 1, 1);
        let cc = chunk_at(&rot_a, RekeyScope::Channel(CHAN), 2, 1, &[7u8; 32], vec![], 1, 1);
        let rots = collect_rotations(&[ca, cb, cc]);
        assert_eq!(rots.len(), 3, "different rotator or scope ⇒ different rotation");
    }

    #[test]
    fn duplicate_chunk_delivery_is_idempotent() {
        let rotator = keys(1);
        let blob = RekeyBlob { locator: "aa".repeat(32), wrapped: "x".into() };
        let c = chunk_at(&rotator, RekeyScope::Root, 1, 0, &[0u8; 32], vec![blob.clone()], 1, 1);
        let rots = collect_rotations(&[c.clone(), c]);
        assert_eq!(rots.len(), 1);
        assert_eq!(rots[0].blobs.len(), 1, "re-delivering a chunk must not double its blobs");
    }

    // ── base blob forms + control_wrap (CORD-06 §1, CORD-04 §3) ──────────────

    #[test]
    fn base_blob_forms_are_width_declared() {
        let community = CommunityId([0x77u8; 32]);
        let new_root = [0xABu8; 32];
        let control_root = [0x5Cu8; 32];
        let epoch = Epoch(3);
        let control_pk = control_signer_group_key(&control_root, &community, epoch).pk().to_bytes();

        // 104-byte member form: root + pk, no secret.
        let member = bound_base_plaintext(epoch, &new_root, &control_pk, None);
        assert_eq!(member.len(), 104);
        let d = parse_bound_base_plaintext(&member, &community, epoch).unwrap();
        assert_eq!(d.new_root, new_root);
        assert_eq!(d.control_pk, Some(control_pk));
        assert_eq!(d.control_root, None);

        // 136-byte staff form carries the secret, verified against its own pk.
        let staff = bound_base_plaintext(epoch, &new_root, &control_pk, Some(&control_root));
        assert_eq!(staff.len(), 136);
        let d = parse_bound_base_plaintext(&staff, &community, epoch).unwrap();
        assert_eq!(d.control_root, Some(control_root));
        assert_eq!(d.control_pk, Some(control_pk));

        // Legacy 72-byte base form yields the root alone.
        let legacy = bound_plaintext(RekeyScope::Root, epoch, &new_root);
        let d = parse_bound_base_plaintext(&legacy, &community, epoch).unwrap();
        assert_eq!(d.new_root, new_root);
        assert_eq!(d.control_pk, None);
        assert_eq!(d.control_root, None);
    }

    #[test]
    fn base_blob_mismatched_control_pair_is_refused_whole() {
        let community = CommunityId([0x77u8; 32]);
        let control_pk = control_signer_group_key(&[0x5Cu8; 32], &community, Epoch(3)).pk().to_bytes();
        // A secret that does NOT derive to the carried pk — the whole blob is
        // dropped, the new_root included (never a partial adoption).
        let bad = bound_base_plaintext(Epoch(3), &[0xABu8; 32], &control_pk, Some(&[0x11u8; 32]));
        assert!(matches!(
            parse_bound_base_plaintext(&bad, &community, Epoch(3)),
            Err(RekeyError::ControlPairMismatch)
        ));
    }

    #[test]
    fn base_blob_rejects_any_other_width_and_splices() {
        let community = CommunityId([0x77u8; 32]);
        // Below and between the defined forms: no append-only extension fits.
        for n in [0usize, 71, 73, 100, 105, 135] {
            assert!(matches!(
                parse_bound_base_plaintext(&vec![0u8; n], &community, Epoch(1)),
                Err(RekeyError::BadBaseBlobWidth(m)) if m == n
            ));
        }
        // The scope and epoch bind INSIDE the ciphertext (unspliceable).
        let control_root = [0x5Cu8; 32];
        let pk = control_signer_group_key(&control_root, &community, Epoch(3)).pk().to_bytes();
        let pt = bound_base_plaintext(Epoch(3), &[0xABu8; 32], &pk, None);
        assert!(matches!(parse_bound_base_plaintext(&pt, &community, Epoch(4)), Err(RekeyError::EpochSplice)));
        let mut channel_scoped = pt.clone();
        channel_scoped[..32].copy_from_slice(&[0x42u8; 32]);
        assert!(matches!(parse_bound_base_plaintext(&channel_scoped, &community, Epoch(3)), Err(RekeyError::ScopeSplice)));
    }

    #[tokio::test]
    async fn base_blob_round_trips_member_and_staff_forms() {
        let rotator = keys(7);
        let member = keys(8);
        let staffer = keys(9);
        let community = CommunityId([0x77u8; 32]);
        let epoch = Epoch(2);
        let new_root = [0xEEu8; 32];
        let control_root = [0xDDu8; 32];
        let control_pk = control_signer_group_key(&control_root, &community, epoch).pk().to_bytes();

        let mb = build_base_blob(&as_signer(&rotator), &xonly(&rotator), &member.public_key(), epoch, &new_root, &control_pk, None).await.unwrap();
        let d = open_base_blob(&as_signer(&member), &rotator.public_key(), &community, epoch, &mb).await.unwrap();
        assert_eq!(d.new_root, new_root);
        assert_eq!(d.control_pk, Some(control_pk));
        assert_eq!(d.control_root, None, "a member blob never carries the secret");

        let sb = build_base_blob(&as_signer(&rotator), &xonly(&rotator), &staffer.public_key(), epoch, &new_root, &control_pk, Some(&control_root)).await.unwrap();
        let d = open_base_blob(&as_signer(&staffer), &rotator.public_key(), &community, epoch, &sb).await.unwrap();
        assert_eq!(d.control_root, Some(control_root));

        // The locator is the same public Root-scope slot the legacy form used.
        assert_eq!(mb.locator, blob_locator(&xonly(&rotator), &xonly(&member), RekeyScope::Root, epoch));
        // And a legacy 72-byte blob still opens through the base opener.
        let legacy = build_blob(&as_signer(&rotator), &xonly(&rotator), &member.public_key(), RekeyScope::Root, epoch, &new_root).await.unwrap();
        let d = open_base_blob(&as_signer(&member), &rotator.public_key(), &community, epoch, &legacy).await.unwrap();
        assert_eq!(d.new_root, new_root);
        assert_eq!(d.control_pk, None);
    }

    #[test]
    fn a_future_wider_base_blob_degrades_instead_of_forking() {
        // The pre-split lesson (CORD-06 §3): a width this build predates must
        // never park the member at the old epoch — extract the frozen prefix
        // and whatever appended fields still verify.
        let community = CommunityId([0x77u8; 32]);
        let new_root = [0xABu8; 32];
        let control_root = [0x5Cu8; 32];
        let epoch = Epoch(3);
        let control_pk = control_signer_group_key(&control_root, &community, epoch).pk().to_bytes();

        // A hypothetical 168-byte form that appended a field after the secret.
        let mut future = bound_base_plaintext(epoch, &new_root, &control_pk, Some(&control_root));
        future.extend_from_slice(&[0x99u8; 32]);
        let d = parse_bound_base_plaintext(&future, &community, epoch).unwrap();
        assert_eq!(d.new_root, new_root);
        assert_eq!(d.control_pk, Some(control_pk));
        assert_eq!(d.control_root, Some(control_root), "held offsets still verify → full function");

        // One that REPLACED the secret's bytes: the derive check fails closed
        // to the verified prefix, never a refusal (membership must survive).
        let mut reordered = bound_base_plaintext(epoch, &new_root, &control_pk, Some(&[0x44u8; 32]));
        reordered.extend_from_slice(&[0x99u8; 32]);
        let d = parse_bound_base_plaintext(&reordered, &community, epoch).unwrap();
        assert_eq!(d.new_root, new_root);
        assert_eq!(d.control_pk, Some(control_pk));
        assert_eq!(d.control_root, None, "an unverifiable secret is dropped, not adopted");

        // The prefix's splice bindings still gate a future form.
        assert!(parse_bound_base_plaintext(&future, &community, Epoch(4)).is_err());
    }

    #[test]
    fn control_wrap_layout_is_frozen_and_round_trips() {
        let pt = encode_control_wrap(Epoch(7), &[0xCDu8; 32]);
        assert_eq!(pt.len(), 40);
        let expected = format!("{}{}", "0000000000000007", "cd".repeat(32));
        assert_eq!(crate::simd::hex::bytes_to_hex_string(&pt), expected);
        let (epoch, root) = parse_control_wrap(&pt).unwrap();
        assert_eq!(epoch, Epoch(7));
        assert_eq!(root, [0xCDu8; 32]);
        for n in [0usize, 39, 41, 72] {
            assert!(matches!(parse_control_wrap(&vec![0u8; n]), Err(RekeyError::BadControlWrapLength(m)) if m == n));
        }
    }

    #[test]
    fn lowest_key_fork_winner_is_deterministic() {
        // CORD-06 §3: among candidates, the lexicographically lowest new key wins,
        // so every retained member converges. Input order must not change it.
        let keys_a = [[0x03u8; 32], [0x01u8; 32], [0x02u8; 32]];
        assert_eq!(lowest_key_winner(&keys_a), Some(1));
        let keys_b = [[0x01u8; 32], [0x02u8; 32], [0x03u8; 32]];
        assert_eq!(lowest_key_winner(&keys_b), Some(0));
        assert_eq!(lowest_key_winner(&[]), None);
    }
}
