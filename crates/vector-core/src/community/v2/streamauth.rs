//! Concord v2 stream-key NIP-42 authentication.
//!
//! Every v2 plane is kind-1059 traffic addressed to a DERIVED per-stream pubkey
//! (control, guestbook, per-channel chat, rekey, dissolved) — never the user's
//! own identity. Relays that gate kind 1059 behind NIP-42 (ditto-relay's default
//! `AUTH_KINDS=4,1059`) require that EVERY `authors` entry in a kind-1059 REQ be
//! an authenticated pubkey on the connection, and reply
//! `CLOSED auth-required: all authors must be authenticated` otherwise. The
//! user's login can't satisfy that — the stream address isn't their pubkey — so
//! an unauthenticated client reads back ZERO events and a join's control-plane
//! verify (or any community fetch) fails closed.
//!
//! The fix (mirroring Armada's `streamAuth`): the client HOLDS the stream secret
//! keys (derived from the `community_root` / channel keys it already stores), so
//! it can NIP-42-authenticate AS each stream by signing an extra kind-22242 AUTH
//! event per stream against the relay's challenge. This module is the registry
//! of stream keys the client currently holds plus the challenge responder; the
//! connection ends up authenticated as the user AND every stream it will query.
//!
//! Signing is local (raw derived keys) — it never touches the account signer /
//! bunker.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex};

use nostr_sdk::prelude::{Client, ClientMessage, EventBuilder, Keys, RelayPoolNotification, RelayUrl};

use super::community::CommunityV2;

/// stream pubkey (x-only bytes) → the derived Keys that authenticate it.
static REGISTRY: LazyLock<Mutex<HashMap<[u8; 32], Keys>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
/// Whether the persistent challenge responder is running this session (idempotent spawn).
static RESPONDER_RUNNING: AtomicBool = AtomicBool::new(false);
/// Each relay's last NIP-42 challenge. A challenge stays valid for the connection
/// lifetime, and a gating relay issues it ONCE (on the first gated REQ) — so keys
/// registered AFTER that frame can only authenticate by replaying the remembered
/// challenge; waiting for a fresh one would wait forever, and one unauthenticated
/// author fails a whole REQ's gate. A stale entry (relay reconnected since) is
/// harmless: the relay ignores it and the next gated REQ re-challenges.
static CHALLENGES: LazyLock<Mutex<HashMap<RelayUrl, String>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// Register a batch of stream keys (idempotent). Returns how many were NEW.
pub fn register(keys: impl IntoIterator<Item = Keys>) -> usize {
    let mut reg = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    let mut added = 0;
    for k in keys {
        if reg.insert(k.public_key().to_bytes(), k).is_none() {
            added += 1;
        }
    }
    added
}

/// Register every plane a community currently exposes: control, guestbook, the
/// dissolved plane, and each readable channel's Chat Plane (a keyless private
/// channel has no readable plane yet, so it's skipped — its rekey plane keys up
/// first). Called at join and on every follow so a rotated address is covered.
pub fn register_community(c: &CommunityV2) -> usize {
    let mut keys: Vec<Keys> = vec![
        super::derive::control_group_key(&c.community_root, c.id(), c.root_epoch).keys().clone(),
        super::derive::guestbook_group_key(&c.community_root, c.id(), c.root_epoch).keys().clone(),
        super::derive::dissolved_group_key(c.id()).keys().clone(),
    ];
    for ch in &c.channels {
        if ch.private && ch.key.is_none() {
            continue;
        }
        let (secret, epoch) = c.channel_secret(ch);
        keys.push(super::derive::channel_group_key(&secret, &ch.id, epoch).keys().clone());
    }
    // The next-epoch rekey planes we subscribe to (base + each private channel).
    for pk_keys in rekey_plane_keys(c) {
        keys.push(pk_keys);
    }
    register(keys)
}

/// The rekey-plane Keys a community watches (base next-epoch + each private
/// channel's next-epoch), mirroring `realtime::rekey_authors` so an AUTH-gating
/// relay serves the rotation crates too.
///
/// Channel planes fan across the SAME addressing roots `follow_rekeys` queries
/// (current + archived priors, CORD-06 D2 — a removal-forced channel rekey
/// rides the PRIOR root). Registration must be UPFRONT and complete: a gating
/// relay issues its NIP-42 challenge once per connection, so a key registered
/// after the connection authed can never authenticate — a current-root-only
/// registration left the prior-root crate plane CLOSED and wedged the channel
/// at its old epoch while the base advanced.
fn rekey_plane_keys(c: &CommunityV2) -> Vec<Keys> {
    use crate::community::Epoch;
    let mut out = vec![super::derive::base_rekey_group_key(&c.community_root, c.id(), Epoch(c.root_epoch.0.saturating_add(1))).keys().clone()];
    let cid_hex = crate::simd::hex::bytes_to_hex_32(&c.id().0);
    let roots = super::service::channel_rekey_addressing_roots(c.community_root, &cid_hex);
    for ch in &c.channels {
        if ch.private {
            for root in &roots {
                out.push(super::derive::channel_rekey_group_key(root, &ch.id, Epoch(ch.epoch.0.saturating_add(1))).keys().clone());
            }
        }
    }
    out
}

/// Record a relay's challenge, returning true when its VALUE changed — a new
/// value means a new connection (NIP-42 challenges live for one connection), so
/// any sub REQ the pool re-applied before this auth was gate-CLOSED and needs a
/// re-send. A re-delivered identical challenge is the same connection: auth is
/// re-sent (idempotent) but no resubscribe is triggered.
fn remember_challenge(relay: &RelayUrl, challenge: &str) -> bool {
    CHALLENGES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(relay.clone(), challenge.to_string())
        .as_deref()
        != Some(challenge)
}

/// The challenge this relay's connection last issued, if seen.
fn remembered_challenge(relay: &RelayUrl) -> Option<String> {
    CHALLENGES.lock().unwrap_or_else(|e| e.into_inner()).get(relay).cloned()
}

/// Last responder-driven resubscribe per relay, bounding the challenge→auth→
/// resubscribe reaction to one per [`RESUB_COOLDOWN`] window per relay.
static RESUB_AT: LazyLock<Mutex<HashMap<RelayUrl, std::time::Instant>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
const RESUB_COOLDOWN: std::time::Duration = std::time::Duration::from_secs(30);

/// True (and stamps now) if this relay hasn't been resubscribed within the
/// cooldown window; false while one is still fresh.
fn resub_cooldown_elapsed(relay: &RelayUrl) -> bool {
    let mut map = RESUB_AT.lock().unwrap_or_else(|e| e.into_inner());
    let now = std::time::Instant::now();
    match map.get(relay) {
        Some(at) if now.duration_since(*at) < RESUB_COOLDOWN => false,
        _ => {
            map.insert(relay.clone(), now);
            true
        }
    }
}

/// Forget every registered stream key (on session swap). The responder task exits
/// on its own when its `SessionGuard` invalidates.
pub fn clear() {
    REGISTRY.lock().unwrap_or_else(|e| e.into_inner()).clear();
    CHALLENGES.lock().unwrap_or_else(|e| e.into_inner()).clear();
    RESUB_AT.lock().unwrap_or_else(|e| e.into_inner()).clear();
    RESPONDER_RUNNING.store(false, Ordering::SeqCst);
}

/// Whether we hold any stream keys (skip the whole dance when not).
pub fn is_empty() -> bool {
    REGISTRY.lock().unwrap_or_else(|e| e.into_inner()).is_empty()
}

/// Sign a NIP-42 AUTH (kind-22242) event for EVERY registered stream key against
/// `challenge` + `relay`. Local raw-key signing; a malformed key is skipped.
fn sign_all(challenge: &str, relay: &RelayUrl) -> Vec<nostr_sdk::Event> {
    let reg = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    reg.values()
        .filter_map(|keys| EventBuilder::auth(challenge, relay.clone()).sign_with_keys(keys).ok())
        .collect()
}

/// Authenticate every registered stream key on `relay` against `challenge`. Each
/// rides a NIP-42 `AUTH` client message (NOT an `EVENT` publish — relays reject a
/// bare kind-22242 event). Best-effort per key. Returns how many were sent.
async fn authenticate_streams(client: &Client, relay: &RelayUrl, challenge: &str) -> usize {
    let events = sign_all(challenge, relay);
    let Ok(r) = client.pool().relay(relay.clone()).await else {
        return 0;
    };
    let mut sent = 0;
    for ev in events {
        if r.send_msg(ClientMessage::auth(ev)).is_ok() {
            sent += 1;
        }
    }
    sent
}

/// Ensure the persistent stream-AUTH responder is running for this session
/// (idempotent). It watches the client's notification stream and, on EVERY relay
/// AUTH challenge, authenticates as all registered stream keys on that relay.
///
/// AUTH-gating relays (Ditto) challenge on the GATED REQ — not on connect — and
/// nostr-sdk retries the REQ after auth, so once this responder is running a
/// normal community fetch/subscribe just works: the REQ triggers the challenge,
/// this answers it with the stream keys, and the retry reads the plane. The
/// user's own login rides nostr-sdk's built-in auto-auth. Exits on session swap.
pub fn ensure_responder(client: &Client) {
    if RESPONDER_RUNNING.swap(true, Ordering::SeqCst) {
        return; // already running this session
    }
    let client = client.clone();
    let session = crate::state::SessionGuard::capture();
    tokio::spawn(async move {
        let mut notifications = client.notifications();
        while let Ok(n) = notifications.recv().await {
            if !session.is_valid() {
                break;
            }
            if let RelayPoolNotification::Message { relay_url, message } = n {
                if let nostr_sdk::RelayMessage::Auth { challenge } = message {
                    let challenge = challenge.into_owned();
                    let fresh_connection = remember_challenge(&relay_url, &challenge);
                    if !is_empty() {
                        authenticate_streams(&client, &relay_url, &challenge).await;
                        // A NEW challenge value means a new connection — the pool's
                        // re-applied sub REQ raced this auth and got gate-CLOSED, and
                        // the relay won't challenge again. Re-send our subs now that
                        // the streams are authenticated. Cooldown-bounded so a relay
                        // minting endless challenges can't drive a resubscribe loop.
                        if fresh_connection && resub_cooldown_elapsed(&relay_url) {
                            super::realtime::resubscribe_relay(&client, &relay_url).await;
                        }
                    }
                }
            }
        }
        RESPONDER_RUNNING.store(false, Ordering::SeqCst);
    });
}

/// Prepare gated relays for an imminent fetch: make sure the community's stream
/// keys are registered and the responder is live, so the fetch's REQ-triggered
/// challenge is answered. A no-op when no client is connected (offline tests).
pub fn prime(community: &CommunityV2) {
    register_community(community);
    if let Some(client) = crate::state::nostr_client() {
        ensure_responder(&client);
    }
}

/// Prime the connection AUTH on `relays` before a live subscription: a
/// subscription (unlike a fetch) isn't auto-retried after the AUTH gate, so the
/// socket must already be authenticated as EVERY stream the sub's `authors` will
/// name — one unauthenticated key fails the whole REQ. Two passes:
///
/// 1. Replay each relay's REMEMBERED challenge for all registered keys — a gating
///    relay challenges once per connection, so keys registered after that frame
///    (a control fold that revealed new channels) would otherwise never auth.
/// 2. A cheap gated fetch, which on a fresh/reconnected socket triggers the
///    challenge the responder answers (and nostr-sdk retries the fetch after).
///
/// No-op with no registered keys / no relays.
pub async fn prime_auth(client: &Client, relays: &[String]) {
    if is_empty() || relays.is_empty() {
        return;
    }
    ensure_responder(client);
    let urls: Vec<RelayUrl> = relays.iter().filter_map(|r| RelayUrl::parse(r).ok()).collect();
    if urls.is_empty() {
        return;
    }
    for url in &urls {
        if let Some(challenge) = remembered_challenge(url) {
            authenticate_streams(client, url, &challenge).await;
        }
    }
    let authors: Vec<nostr_sdk::PublicKey> = {
        let reg = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
        reg.keys().filter_map(|pk| nostr_sdk::PublicKey::from_slice(pk).ok()).collect()
    };
    if authors.is_empty() {
        return;
    }
    let filter = nostr_sdk::Filter::new()
        .kind(nostr_sdk::Kind::Custom(super::stream::KIND_WRAP))
        .authors(authors)
        .limit(1);
    // Bounded so a dead relay can't stall the subscription refresh behind it.
    let _ = tokio::time::timeout(std::time::Duration::from_secs(8), client.fetch_events_from(urls, filter, std::time::Duration::from_secs(6))).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::community::v2::control::{genesis, CommunityMetadata};
    use crate::community::v2::community::{ChannelV2, CommunityV2};
    use crate::community::{ChannelId, Epoch};
    use nostr_sdk::prelude::Keys;

    /// A community with one public, one keyed-private, and one KEYLESS-private
    /// channel — the three registration classes.
    fn community_with_channel_mix() -> CommunityV2 {
        let owner = Keys::generate();
        let g = genesis(&owner, CommunityMetadata { name: "auth-test".into(), ..Default::default() }, 1_000).unwrap();
        let mut c = CommunityV2::from_genesis(&g, "auth-test", None, vec!["wss://gated.example".into()], 0);
        c.channels.push(ChannelV2 { id: ChannelId([2u8; 32]), name: "keyed-private".into(), private: true, key: Some([7u8; 32]), epoch: Epoch(3), voice: None, meta_custom: None, meta_extra: Default::default() });
        c.channels.push(ChannelV2 { id: ChannelId([3u8; 32]), name: "keyless-private".into(), private: true, key: None, epoch: Epoch(0), voice: None, meta_custom: None, meta_extra: Default::default() });
        c
    }

    fn registered(pk: &nostr_sdk::prelude::PublicKey) -> bool {
        REGISTRY.lock().unwrap_or_else(|e| e.into_inner()).contains_key(&pk.to_bytes())
    }

    /// The registry covers every plane a member must authenticate AS — and a
    /// keyless private channel (no readable plane yet) is skipped, while its
    /// NEXT-epoch rekey plane (the entry point for its key) is covered.
    #[test]
    fn register_community_covers_planes_and_skips_keyless() {
        let c = community_with_channel_mix();
        let added = register_community(&c);
        assert!(added >= 6, "control+guestbook+dissolved+public chat+keyed chat+rekeys = at least 6 new keys, got {added}");

        use super::super::derive;
        assert!(registered(&derive::control_group_key(&c.community_root, c.id(), c.root_epoch).pk()));
        assert!(registered(&derive::guestbook_group_key(&c.community_root, c.id(), c.root_epoch).pk()));
        assert!(registered(&derive::dissolved_group_key(c.id()).pk()));
        // Public channel: chat plane derives from the community root.
        let public = &c.channels[0];
        let (secret, epoch) = c.channel_secret(public);
        assert!(registered(&derive::channel_group_key(&secret, &public.id, epoch).pk()));
        // Keyed private channel: chat plane derives from its own key + epoch.
        let keyed = &c.channels[1];
        assert!(registered(&derive::channel_group_key(&[7u8; 32], &keyed.id, Epoch(3)).pk()));
        // KEYLESS private channel: no readable plane — deriving from the root
        // would address the PUBLIC plane, so it must NOT be registered.
        let keyless = &c.channels[2];
        assert!(!registered(&derive::channel_group_key(&c.community_root, &keyless.id, Epoch(0)).pk()));
        // Rekey planes: base next-epoch + each PRIVATE channel's next-epoch.
        assert!(registered(&derive::base_rekey_group_key(&c.community_root, c.id(), Epoch(c.root_epoch.0 + 1)).pk()));
        assert!(registered(&derive::channel_rekey_group_key(&c.community_root, &keyed.id, Epoch(4)).pk()));

        // Idempotent: a second registration adds nothing.
        assert_eq!(register_community(&c), 0);
    }

    /// The PIECE-2 regression: a key registered AFTER the connection's one
    /// challenge was consumed still authenticates, because the challenge is
    /// remembered and `sign_all` covers every CURRENTLY-registered key.
    #[test]
    fn late_registered_keys_sign_against_the_remembered_challenge() {
        let relay = RelayUrl::parse("wss://late-keys.example").unwrap();
        let early = Keys::generate();
        register([early.clone()]);
        // The connection's single challenge arrives while only `early` exists.
        assert!(remember_challenge(&relay, "challenge-1"), "first sighting is a fresh connection");
        // A control fold reveals a new channel → its plane key registers late.
        let late = Keys::generate();
        register([late.clone()]);
        // The replay path (prime_auth pass 1) must sign for BOTH keys.
        let challenge = remembered_challenge(&relay).expect("challenge was remembered");
        let events = sign_all(&challenge, &relay);
        let signers: Vec<_> = events.iter().map(|e| e.pubkey).collect();
        assert!(signers.contains(&early.public_key()));
        assert!(signers.contains(&late.public_key()));
    }

    /// AUTH events must be NIP-42-shaped: kind 22242, challenge + relay tags,
    /// valid signature by the stream key.
    #[test]
    fn signed_auth_events_are_nip42_shaped() {
        let relay = RelayUrl::parse("wss://shape.example").unwrap();
        let key = Keys::generate();
        register([key.clone()]);
        let events = sign_all("shape-challenge", &relay);
        let ev = events.iter().find(|e| e.pubkey == key.public_key()).expect("signed by the registered key");
        assert_eq!(ev.kind, nostr_sdk::Kind::Authentication);
        assert!(ev.verify().is_ok(), "signature + id must verify");
        let tag_values: Vec<String> = ev.tags.iter().filter_map(|t| t.content().map(String::from)).collect();
        assert!(tag_values.iter().any(|v| v == "shape-challenge"), "carries the challenge tag");
    }

    /// A NEW challenge value = a new connection (triggers the resubscribe); the
    /// SAME value re-delivered = the same connection (auth only, no resubscribe).
    #[test]
    fn challenge_value_change_detects_a_new_connection() {
        let relay = RelayUrl::parse("wss://conn-detect.example").unwrap();
        assert!(remember_challenge(&relay, "c1"), "first sighting");
        assert!(!remember_challenge(&relay, "c1"), "same value = same connection");
        assert!(remember_challenge(&relay, "c2"), "new value = reconnected");
        assert_eq!(remembered_challenge(&relay).as_deref(), Some("c2"), "memory holds the newest");
    }

    /// The responder-driven resubscribe is cooldown-bounded per relay, so a
    /// relay minting endless fresh challenges can't drive a resubscribe loop.
    #[test]
    fn resubscribe_cooldown_bounds_the_reaction() {
        let relay = RelayUrl::parse("wss://cooldown.example").unwrap();
        assert!(resub_cooldown_elapsed(&relay), "first trigger passes");
        assert!(!resub_cooldown_elapsed(&relay), "immediate repeat is suppressed");
        let other = RelayUrl::parse("wss://cooldown-other.example").unwrap();
        assert!(resub_cooldown_elapsed(&other), "cooldown is per-relay");
    }
}
