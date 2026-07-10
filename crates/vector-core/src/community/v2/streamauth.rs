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
fn rekey_plane_keys(c: &CommunityV2) -> Vec<Keys> {
    use crate::community::Epoch;
    let mut out = vec![super::derive::base_rekey_group_key(&c.community_root, c.id(), Epoch(c.root_epoch.0.saturating_add(1))).keys().clone()];
    for ch in &c.channels {
        if ch.private {
            out.push(super::derive::channel_rekey_group_key(&c.community_root, &ch.id, Epoch(ch.epoch.0.saturating_add(1))).keys().clone());
        }
    }
    out
}

/// Forget every registered stream key (on session swap). The responder task exits
/// on its own when its `SessionGuard` invalidates.
pub fn clear() {
    REGISTRY.lock().unwrap_or_else(|e| e.into_inner()).clear();
    CHALLENGES.lock().unwrap_or_else(|e| e.into_inner()).clear();
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
                    CHALLENGES
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .insert(relay_url.clone(), challenge.clone());
                    if !is_empty() {
                        authenticate_streams(&client, &relay_url, &challenge).await;
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
        let challenge = CHALLENGES.lock().unwrap_or_else(|e| e.into_inner()).get(url).cloned();
        if let Some(challenge) = challenge {
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
