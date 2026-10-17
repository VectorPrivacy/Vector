//! Vector Communities, on the Concord v2 (CORD) protocol in [`v2`], plus the pieces
//! every layer shares: ids, epochs, images, transport, roles and moderation.

pub mod attachments;
pub mod cache;
pub mod cipher;
pub mod message_delete;
pub mod policy;
pub mod moderation;
pub mod edition;
pub mod version;
pub mod raid;
pub mod rank;
pub mod roles;
pub mod transport;
pub mod v2;

use rand::RngCore;
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, ZeroizeOnDrop};

/// How fresh an inner send time must be to surface as a REALTIME event (ms).
/// Generous against sender clock skew and relay push latency; tiny against the
/// hours-to-months staleness of a relay-to-relay backfill replay.
pub const REALTIME_FRESH_WINDOW_MS: u64 = 300_000;

/// Whether an inner send time is fresh enough to surface as REALTIME. The live
/// subscriptions are limit-0 (never history), but a relay backfilling from a
/// peer ACCEPTS old events as new writes and pushes them to matching live subs
/// — without this gate a month-old join or mention surfaces as if it just
/// happened (bots welcome long-gone joiners, answer stale mentions). Gates only
/// the "new" surfaces (`is_new`, bot events, pings); folding and persistence
/// are NEVER gated on freshness.
pub fn is_realtime_fresh(at_ms: u64) -> bool {
    let now_ms = web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    at_ms.saturating_add(REALTIME_FRESH_WINDOW_MS) >= now_ms
}

/// Route an inbound Community event, live or a slow relay's straggler.
pub async fn dispatch_inbound(
    event: nostr_sdk::prelude::Event,
    handler: std::sync::Arc<dyn crate::InboundEventHandler>,
) {
    v2::realtime::dispatch_event(event, handler).await;
}

/// A Community's stable identity = a random 32-byte opaque id (NOT a
/// timestamp-encoding snowflake, which would leak creation time).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CommunityId(pub [u8; 32]);

impl CommunityId {
    /// Lowercase hex — the addressable-event `d`-tag form.
    pub fn to_hex(&self) -> String {
        crate::simd::hex::bytes_to_hex_32(&self.0)
    }
}

/// A Channel's stable identity within a Community. Same opaque-random rule as
/// [`CommunityId`]; doubles as the addressable metadata `d`-tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ChannelId(pub [u8; 32]);

impl ChannelId {
    /// Lowercase hex, the form used in the inner channel-binding tag.
    pub fn to_hex(&self) -> String {
        crate::simd::hex::bytes_to_hex_32(&self.0)
    }
}

/// The epoch counter — the read-access clock ("two clocks"). Bumps only on a
/// rekey; stamped explicitly even when it is 0, so multi-channel and rotation stay
/// additive (forward-compat hook #1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Epoch(pub u64);

/// A 32-byte symmetric channel secret — the raw NIP-44 v2 `ConversationKey`
/// material. Zeroized on drop; never logged.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct ChannelKey(pub [u8; 32]);

impl ChannelKey {
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

// Deliberately no Debug: a channel secret must never reach a log line.
impl core::fmt::Debug for ChannelKey {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("ChannelKey(<redacted>)")
    }
}

/// The all-zero hex scope id for server-root-scoped epoch keys (`RekeyScope::ServerRoot`
/// uses the all-zero `id32` sentinel). A `ChannelId` is random-32, so it can never collide with
/// this — letting one `community_epoch_keys` table hold both channel keys and the base/server-root
/// key keyed by `(scope_id, epoch)`.
pub const SERVER_ROOT_SCOPE_HEX: &str =
    "0000000000000000000000000000000000000000000000000000000000000000";

/// The epoch-key-archive scope for a v2 `control_root` (CORD-02 §2): non-hex on
/// purpose, so it collides with neither a channel id nor the root sentinel.
/// Reserves the pair beside the root at mint (retry idempotency) and archives
/// the secret per epoch.
pub const CONTROL_ROOT_SCOPE: &str = "control-root";

/// The bytes behind an id written exactly as [`CommunityId::to_hex`] writes one, so a
/// byte-keyed cache answers only for strings it would have matched character for character.
pub(crate) fn id32_from_hex(hex: &str) -> Option<[u8; 32]> {
    crate::simd::hex::hex_to_bytes_32_checked(hex).filter(|_| !hex.bytes().any(|b| b.is_ascii_uppercase()))
}

/// 32 cryptographically-random bytes (OsRng).
pub(crate) fn random_32() -> [u8; 32] {
    let mut b = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut b);
    b
}

/// A reference to an encrypted image blob (community logo/banner), using the same
/// technique as NIP-17 file attachments: a fresh random AES-GCM key+nonce encrypts the
/// image, the ciphertext is uploaded to Blossom, and this reference travels inside
/// ServerRoot-sealed metadata. So possession of the server-root key (every member) gates
/// the image, and there is no key reuse across images.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommunityImage {
    /// Blossom URL of the encrypted blob.
    pub url: String,
    /// Hex AES-GCM key (per-image, random).
    pub key: String,
    /// Hex AES-GCM nonce.
    pub nonce: String,
    /// SHA-256 of the plaintext image (integrity check + local cache key).
    pub hash: String,
    /// File extension hint (e.g. "png", "jpg").
    #[serde(default)]
    pub ext: String,
}

/// Community icons and banners are raster only, sent and shown, like custom emoji.
pub const SVG_REFUSED: &str = "SVG isn't supported for community icons or banners";

/// Reads an image pointer field so a malformed one from another client drops alone, rather
/// than failing the whole metadata document and taking the name and description with it.
pub(crate) fn lenient_image<'de, D, T>(d: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned,
{
    let value = Option::<serde_json::Value>::deserialize(d)?;
    Ok(value.and_then(|v| serde_json::from_value(v).ok()))
}

/// Protocol cap on a Community's relay set (§ transport). More relays are needless and
/// amplify resource + metadata-exposure cost; 5 gives redundancy without centralisation.
/// Enforced by truncate-on-read at every Community/CommunityInvite construction boundary, so a
/// hostile or legacy bundle degrades to ≤5 distinct relays rather than being honored or rejected.
pub const MAX_COMMUNITY_RELAYS: usize = 5;

/// Dedupe (order-preserving) + truncate a relay set to [`MAX_COMMUNITY_RELAYS`]. Dedup first so the
/// cap means "up to 5 DISTINCT relays" — a bundle padding one relay 5× can't waste the budget.
pub fn cap_relays(relays: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(relays.len().min(MAX_COMMUNITY_RELAYS));
    for r in relays {
        if out.len() >= MAX_COMMUNITY_RELAYS {
            break;
        }
        if seen.insert(r.clone()) {
            out.push(r);
        }
    }
    out
}

#[cfg(test)]
mod freshness_tests {
    use super::*;

    // The realtime gate: a just-sent event is fresh; a relay-backfill replay
    // (hours to months old) and far-future junk are not "new" surfaces.
    #[test]
    fn realtime_freshness_window() {
        let now_ms = web_time::SystemTime::now()
            .duration_since(web_time::UNIX_EPOCH).unwrap().as_millis() as u64;
        assert!(is_realtime_fresh(now_ms), "just-sent is fresh");
        assert!(is_realtime_fresh(now_ms - REALTIME_FRESH_WINDOW_MS / 2), "modest delay is fresh");
        assert!(!is_realtime_fresh(now_ms - 3_600_000), "an hour-old replay is not");
        assert!(!is_realtime_fresh(now_ms.saturating_sub(30 * 24 * 3_600_000)), "a month-old replay is not");
        assert!(is_realtime_fresh(now_ms + 60_000), "sender clock slightly ahead still fresh");
    }
}
