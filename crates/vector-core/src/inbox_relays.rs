//! NIP-17 Kind 10050 (DM Relay List) support.
//!
//! Fetches, caches, and publishes kind 10050 events so that DM gift wraps
//! are delivered to the recipient's preferred inbox relays.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Instant;

use nostr_sdk::prelude::*;
use std::sync::LazyLock;

use crate::state::nostr_client;

// ============================================================================
// Per-relay publish tracker — closes "dependent-event-races-parent" races
// ============================================================================
//
// Vector returns on the first relay ack so the UI can mark a message
// as "Sent" without waiting for stragglers. Other relays continue
// receiving the event in the background. Any operation that publishes
// a *dependent* event referencing the just-sent one (NIP-09 deletion,
// edit, reaction, reply, …) can race those background publishes: at
// a relay where the parent hasn't arrived yet, the dependent gets
// dropped or stored disconnected, and when the parent arrives later
// it stays without the dependent ever being applied.
//
// `EventPublishTracker` exposes a per-relay event stream of "parent
// successfully published to X". Dependent senders subscribe, drain
// relays that have already settled, then wait for stragglers and
// fire their dependent event to each one as soon as it confirms the
// parent. Every relay that ever received the parent gets the
// dependent in real time, and the user sees no UX latency on either
// the parent send or the dependent action.
//
// This pattern is generic: deletion is the first consumer, but rapid
// edits, self-reactions, and replies-to-just-sent all benefit. The
// tracker doesn't care what the event is or what the dependent
// operation does — it only knows "this parent landed at this relay".

/// Per-relay publish tracker. One per outbound event whose dependents
/// (deletions, edits, reactions, replies, ...) need to fire only
/// after the parent has actually landed at each individual relay.
pub struct EventPublishTracker {
    event_id: EventId,
    /// Successful relays in arrival order. Subscribers walk this with
    /// a cursor and wait on `notify` for new entries.
    successes: Mutex<Vec<RelayUrl>>,
    notify: tokio::sync::Notify,
    /// Relays still publishing. When this hits 0, the tracker
    /// removes itself from the global registry and any pending
    /// `next_success` waiters are woken so they observe end-of-stream.
    in_flight: AtomicUsize,
}

impl EventPublishTracker {
    fn new(event_id: EventId, initial_in_flight: usize) -> Arc<Self> {
        Arc::new(Self {
            event_id,
            successes: Mutex::new(Vec::new()),
            notify: tokio::sync::Notify::new(),
            in_flight: AtomicUsize::new(initial_in_flight),
        })
    }

    /// Called by a per-relay publish task on success.
    fn note_success(&self, url: RelayUrl) {
        self.successes.lock().unwrap().push(url);
        self.notify.notify_waiters();
    }

    /// Called by every per-relay task on completion (success OR fail).
    /// When the last in-flight task settles, drops the tracker from
    /// the global registry.
    fn note_settled(&self) {
        if self.in_flight.fetch_sub(1, Ordering::SeqCst) == 1 {
            self.notify.notify_waiters();
            PUBLISH_TRACKERS.lock().unwrap().remove(&self.event_id);
        }
    }

    /// Async iterator over successful relays. Yields each URL once,
    /// regardless of whether it settled before or after the call.
    /// Returns `None` when every spawned per-relay task has settled
    /// AND the cursor has consumed every success — i.e. the dependent
    /// sender has visited every relay that ever held the parent.
    pub async fn next_success(&self, cursor: &mut usize) -> Option<RelayUrl> {
        loop {
            // Pre-create the notified future BEFORE inspecting state
            // so a notify_waiters() that fires between the check and
            // the await doesn't get lost.
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();

            let (next, done) = {
                let successes = self.successes.lock().unwrap();
                let next = successes.get(*cursor).cloned();
                let done = self.in_flight.load(Ordering::SeqCst) == 0
                    && *cursor >= successes.len();
                (next, done)
            };

            if let Some(url) = next {
                *cursor += 1;
                return Some(url);
            }
            if done {
                return None;
            }

            notified.await;
        }
    }
}

/// Global registry of in-flight tracked publishes. Keyed by event id.
/// Trackers self-remove once all per-relay tasks settle.
static PUBLISH_TRACKERS: LazyLock<Mutex<HashMap<EventId, Arc<EventPublishTracker>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Look up the tracker for an event currently being published.
/// Returns `None` if the publish has fully settled (all relays done)
/// or if the tracker never existed (e.g. the event was sent in a
/// previous app session, or via a non-tracked send path). Dependent
/// senders fall back to a best-effort broadcast in that case.
pub fn get_publish_tracker(event_id: &EventId) -> Option<Arc<EventPublishTracker>> {
    PUBLISH_TRACKERS.lock().unwrap().get(event_id).cloned()
}

/// Spawn one publish task per resolved relay and register a tracker
/// keyed by the event id. Returns the join handles so the caller can
/// race them for first-ok or wait for all to settle as needed. The
/// spawned tasks continue updating the tracker after the caller
/// stops waiting on the handles.
///
/// Generic primitive — any send path that wants its event referenced
/// by a future dependent (deletions, edits, reactions, replies)
/// should publish via this helper so the dependent can later look up
/// the tracker via `get_publish_tracker(parent_id)`.
pub fn spawn_tracked_publish(
    resolved: Vec<(RelayUrl, Relay)>,
    event: Event,
) -> Vec<tokio::task::JoinHandle<(RelayUrl, Result<EventId, String>)>> {
    let event_id = event.id;
    // Zero relays → zero tasks → nobody ever calls note_settled; a registered tracker
    // would leak forever.
    if resolved.is_empty() {
        return Vec::new();
    }
    let tracker = EventPublishTracker::new(event_id, resolved.len());
    PUBLISH_TRACKERS.lock().unwrap().insert(event_id, tracker.clone());

    let mut handles = Vec::with_capacity(resolved.len());
    for (url, relay) in resolved {
        let event = event.clone();
        let tracker = tracker.clone();
        handles.push(tokio::spawn(async move {
            let result = relay
                .send_event(&event)
                .await
                .map_err(|e| e.to_string());
            if result.is_ok() {
                tracker.note_success(url.clone());
            }
            tracker.note_settled();
            (url, result)
        }));
    }
    handles
}

// ============================================================================
// Cache
// ============================================================================

/// How long cached relay lists stay valid before re-fetching.
const CACHE_TTL_SECS: u64 = 3600; // 1 hour

/// Shorter TTL for failed fetches so transient errors don't suppress routing too long.
const CACHE_TTL_ERROR_SECS: u64 = 60; // 1 minute

struct CachedRelays {
    relays: Vec<String>,
    fetched_at: Instant,
    /// Whether the fetch succeeded (true) or failed/timed out (false).
    /// Failed fetches use a shorter cache TTL.
    fetch_ok: bool,
}

static INBOX_RELAY_CACHE: LazyLock<Mutex<HashMap<PublicKey, CachedRelays>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Drop every cached recipient relay list — called by `reset_session()`.
/// The cache is recipient-keyed (so technically account-agnostic) but
/// grows unboundedly across sessions; the 1-hour TTL only reclaims
/// re-queried entries. Clear on swap to free memory and avoid
/// stale-data revivals.
pub fn clear_inbox_relay_cache() {
    if let Ok(mut cache) = INBOX_RELAY_CACHE.lock() {
        cache.clear();
    }
}

/// Per-key locks to prevent cache stampede (thundering herd).
/// When multiple messages target the same recipient with a cold cache, only the
/// first fetch runs — others wait on the per-key lock, then hit the cache.
/// Uses Weak references: the Mutex allocation is freed when Arc refcount drops.
/// HashMap entries are removed eagerly by a per-call drop guard (normal return,
/// cancellation, or panic unwind). Periodic retain() remains a fallback safety net.
static FETCH_LOCKS: LazyLock<Mutex<HashMap<PublicKey, Weak<tokio::sync::Mutex<()>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Counter for periodic fallback pruning of dead Weak entries in FETCH_LOCKS.
/// Prune every PRUNE_INTERVAL cache misses to avoid O(n) scans on every access.
/// This complements eager per-key cleanup after each completed call.
static PRUNE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Prune dead Weak entries every N cache misses. Lower = more CPU for pruning,
/// higher = more memory for stale entries. 100 is a good balance for production.
#[cfg(not(test))]
const PRUNE_INTERVAL: u64 = 100;

/// In tests, prune every access for deterministic behavior (tests rely on
/// immediate cleanup to verify pruning logic works correctly).
#[cfg(test)]
const PRUNE_INTERVAL: u64 = 1;

/// Drop-guard for eager per-key lock-map cleanup.
/// Runs on normal return and when the future is dropped (e.g. cancellation).
struct FetchLockEntryCleanup {
    pubkey: PublicKey,
    key_lock: Arc<tokio::sync::Mutex<()>>,
}

impl FetchLockEntryCleanup {
    fn new(pubkey: PublicKey, key_lock: Arc<tokio::sync::Mutex<()>>) -> Self {
        Self { pubkey, key_lock }
    }
}

impl Drop for FetchLockEntryCleanup {
    fn drop(&mut self) {
        let mut locks = match FETCH_LOCKS.lock() {
            Ok(locks) => locks,
            Err(_) => return, // fallback retain() handles stale entries later
        };

        let should_remove = match locks.get(&self.pubkey).and_then(|weak| weak.upgrade()) {
            Some(current) => {
                // upgrade() adds one temporary Arc. If strong_count == 2, only:
                // 1) this drop-guard's Arc, 2) upgrade() temporary Arc.
                // That means no other in-flight callers still hold this key lock.
                Arc::ptr_eq(&current, &self.key_lock) && Arc::strong_count(&current) == 2
            }
            None => false,
        };
        if should_remove {
            locks.remove(&self.pubkey);
        }
    }
}

// ============================================================================
// Fetch
// ============================================================================

/// Canonical string form for relay-url comparison. nostr-sdk canonicalises
/// differently between published-10050 strings and pool keys (trailing
/// slashes, default ports, case), so equality checks must go through this.
pub fn normalize_relay_url(s: &str) -> String {
    s.trim_end_matches('/').to_ascii_lowercase()
}

/// Relay-url targets for 10050 traffic: every READ-flagged pool relay plus
/// any pooled Discovery Relay (GOSSIP-flagged, so invisible to `has_read()`
/// — matched by url instead). Only urls actually in the pool are returned;
/// `fetch_events_from`/`send_event_to` error on unknown urls.
async fn inbox_query_targets(client: &Client) -> Vec<RelayUrl> {
    let discovery: HashSet<String> = crate::state::discovery_relay_iter()
        .map(normalize_relay_url)
        .collect();
    client
        .pool()
        .all_relays()
        .await
        .iter()
        .filter(|(url, relay)| {
            relay.flags().has_read() || discovery.contains(&normalize_relay_url(url.as_str()))
        })
        .map(|(url, _)| url.clone())
        .collect()
}

/// Result of a 10050 fetch: relays found, or whether the fetch itself failed.
struct FetchResult {
    relays: Vec<String>,
    /// `true` if the network request succeeded (even if no events were found).
    fetch_ok: bool,
}

/// Fetch a pubkey's kind 10050 relay list from the network. Queries the
/// Discovery Relays alongside our read relays: a recipient on another client
/// often publishes their list where our own relay set has no overlap.
async fn fetch_inbox_relays(client: &Client, pubkey: &PublicKey) -> FetchResult {
    let filter = Filter::new()
        .author(*pubkey)
        .kind(Kind::Custom(10050))
        .limit(1);

    let targets = inbox_query_targets(client).await;
    let fetched = if targets.is_empty() {
        client
            .fetch_events(filter, std::time::Duration::from_secs(5))
            .await
    } else {
        client
            .fetch_events_from(targets, filter, std::time::Duration::from_secs(5))
            .await
    };
    let events = match fetched {
        Ok(events) => events,
        Err(e) => {
            eprintln!("[InboxRelays] Failed to fetch 10050 for {}: {}", pubkey, e);
            return FetchResult { relays: Vec::new(), fetch_ok: false };
        }
    };

    // Replaceable event: several relays can answer with different revisions —
    // only the newest is the user's current list.
    let event = match events.into_iter().max_by_key(|e| e.created_at) {
        Some(e) => e,
        None => return FetchResult { relays: Vec::new(), fetch_ok: true },
    };

    FetchResult { relays: parse_relay_tags(&event.tags), fetch_ok: true }
}

/// Extract relay URLs from kind 10050 event tags.
/// Looks for `["relay", "wss://..."]` tag entries.
fn parse_relay_tags(tags: &Tags) -> Vec<String> {
    tags.iter()
        .filter_map(|tag| {
            let values: Vec<&str> = tag.as_slice().iter().map(|s| s.as_str()).collect();
            if values.len() >= 2 && values[0] == "relay" {
                Some(values[1].to_string())
            } else {
                None
            }
        })
        .collect()
}

/// Generic cache-with-lock implementation used by both production and test code.
/// Uses double-checked locking to prevent cache stampede: rapid requests to the
/// same pubkey serialize through a per-key lock, so only one fetch happens.
/// Different pubkeys never block each other.
async fn get_or_fetch_with_lock<F, Fut>(pubkey: &PublicKey, fetch_fn: F) -> Vec<String>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = FetchResult>,
{
    // Fast path: cache hit (no per-key lock needed, no pruning)
    {
        let cache = INBOX_RELAY_CACHE.lock().unwrap();
        if let Some(entry) = cache.get(pubkey) {
            let ttl = if entry.fetch_ok { CACHE_TTL_SECS } else { CACHE_TTL_ERROR_SECS };
            if entry.fetched_at.elapsed().as_secs() < ttl {
                return entry.relays.clone();
            }
        }
    }

    // Per-key lock — serializes fetches for the same pubkey only.
    // Uses Weak references + periodic pruning (every PRUNE_INTERVAL cache misses).
    let cleanup_guard = {
        let mut locks = FETCH_LOCKS.lock().unwrap();

        // Periodic cleanup: remove dead Weak entries every PRUNE_INTERVAL accesses.
        // Avoids O(n) scan in global critical section on every cache miss; instead
        // amortizes cost to O(n/PRUNE_INTERVAL) per miss under heavy fan-out.
        if PRUNE_COUNTER.fetch_add(1, Ordering::Relaxed) % PRUNE_INTERVAL == 0 {
            locks.retain(|_, weak| Weak::strong_count(weak) > 0);
        }

        let weak = locks.entry(*pubkey).or_insert_with(|| Weak::new());
        // Try to upgrade the weak reference; if it fails (Arc was dropped),
        // create a new Arc and update the map.
        let key_lock = match weak.upgrade() {
            Some(arc) => arc,
            None => {
                let new_arc = Arc::new(tokio::sync::Mutex::new(()));
                *weak = Arc::downgrade(&new_arc);
                new_arc
            }
        };
        // Wrap lock Arc in drop-guard so map cleanup runs even on cancellation.
        FetchLockEntryCleanup::new(*pubkey, key_lock)
    };
    let relays = {
        let _guard = cleanup_guard.key_lock.lock().await;

        // Double-check: another task may have filled the cache while we waited
        let cached_relays = {
            let cache = INBOX_RELAY_CACHE.lock().unwrap();
            if let Some(entry) = cache.get(pubkey) {
                let ttl = if entry.fetch_ok { CACHE_TTL_SECS } else { CACHE_TTL_ERROR_SECS };
                if entry.fetched_at.elapsed().as_secs() < ttl {
                    Some(entry.relays.clone())
                } else {
                    None
                }
            } else {
                None
            }
        };

        match cached_relays {
            Some(relays) => relays,
            None => {
                // We won the race — do the actual fetch
                let result = fetch_fn().await;

                // Store in cache (even empty/error results to avoid hammering relays)
                {
                    let mut cache = INBOX_RELAY_CACHE.lock().unwrap();
                    cache.insert(
                        *pubkey,
                        CachedRelays {
                            relays: result.relays.clone(),
                            fetched_at: Instant::now(),
                            fetch_ok: result.fetch_ok,
                        },
                    );
                }

                result.relays
            }
        }
    }; // per-key lock guard dropped here

    // Explicit drop on normal path. On cancellation/panic unwind this still runs
    // via Drop when the future is torn down.
    drop(cleanup_guard);
    relays
}

/// Get inbox relays for a pubkey, using cache when available.
async fn get_or_fetch_inbox_relays(client: &Client, pubkey: &PublicKey) -> Vec<String> {
    get_or_fetch_with_lock(pubkey, || fetch_inbox_relays(client, pubkey)).await
}

// ============================================================================
// Send helper
// ============================================================================

/// Parsed `TRUSTED_RELAYS` URLs — computed once on first access.
static TRUSTED_RELAY_URLS: LazyLock<Vec<RelayUrl>> = LazyLock::new(|| {
    crate::state::TRUSTED_RELAYS
        .iter()
        .filter_map(|s| RelayUrl::parse(s).ok())
        .collect()
});

/// Get the cached parsed trusted relay URLs.
pub fn trusted_relay_urls() -> Vec<RelayUrl> {
    TRUSTED_RELAY_URLS.clone()
}

/// Send an event to specific relays, returning as soon as the **first** relay
/// acknowledges success. Remaining relays continue sending in the background.
///
/// Uses `spawn_tracked_publish` under the hood, so every event published
/// here automatically registers an `EventPublishTracker` keyed by event
/// id. Dependent operations (NIP-09 deletions, edits, reactions, replies)
/// can look up the tracker via `get_publish_tracker(event_id)` and drive
/// per-relay dispatch only after each relay confirms the parent — closing
/// the publish/dependent race for any send that goes through here.
pub async fn send_event_first_ok(
    client: &Client,
    urls: Vec<RelayUrl>,
    event: &Event,
) -> Result<Output<EventId>, nostr_sdk::client::Error> {
    let pool = client.pool();
    let relays = pool.relays().await;
    let event_id = event.id;

    // Resolve URL -> Relay handles, filtering to relays we actually have
    let mut resolved: Vec<(RelayUrl, Relay)> = Vec::new();
    for url in urls {
        if let Some(relay) = relays.get(&url) {
            resolved.push((url, relay.clone()));
        }
    }

    if resolved.is_empty() {
        return client.send_event(event).await;
    }

    // Spawn tracked per-relay tasks. This registers a tracker so any
    // future dependent send (deletion, edit, reaction) can fire only
    // after each relay confirms the parent.
    let handles = spawn_tracked_publish(resolved, event.clone());

    // Race: return as soon as the first relay succeeds
    let mut output = Output {
        val: event_id,
        success: std::collections::HashSet::new(),
        failed: HashMap::new(),
    };

    let mut remaining = handles;
    while !remaining.is_empty() {
        let (result, _index, rest) = futures_util::future::select_all(remaining).await;
        remaining = rest;

        if let Ok((url, relay_result)) = result {
            match relay_result {
                Ok(_) => {
                    output.success.insert(url);
                    // First success — remaining spawned tasks continue in background
                    // updating the tracker as they settle. Dropping JoinHandles
                    // detaches but does NOT cancel them.
                    drop(remaining);
                    return Ok(output);
                }
                Err(e) => {
                    output.failed.insert(url, e);
                }
            }
        }
    }

    // All relays failed — return output so caller can inspect .failed
    Ok(output)
}

/// Send an event to all write-relays in the pool, returning as soon as the
/// **first** relay acknowledges success.
pub async fn send_event_pool_first_ok(
    client: &Client,
    event: &Event,
) -> Result<Output<EventId>, nostr_sdk::client::Error> {
    let pool = client.pool();
    let relays = pool.relays().await;
    let write_urls: Vec<RelayUrl> = relays
        .iter()
        .filter(|(_, r)| r.flags().has_write())
        .map(|(url, _)| url.clone())
        .collect();
    send_event_first_ok(&client, write_urls, event).await
}

/// Build a NIP-59 kind-1059 gift wrap from a sealed event, returning
/// **both** the signed wrap event and the ephemeral secp256k1 secret
/// used to sign it.
///
/// Wire-compatible with `EventBuilder::gift_wrap_from_seal` — other
/// clients cannot tell the wraps apart. The only difference is that we
/// keep the ephemeral key instead of dropping it on the floor, so the
/// user can later sign a NIP-09 deletion against the wrap event id and
/// have relays drop it. This is Vector's "delete from network" primitive.
pub fn wrap_with_retained_key(
    receiver: &PublicKey,
    seal: &Event,
    extra_tags: impl IntoIterator<Item = Tag>,
) -> Result<(Event, SecretKey), String> {
    use nostr_sdk::nips::nip44;
    use nostr_sdk::nips::nip59::RANGE_RANDOM_TIMESTAMP_TWEAK;

    if seal.kind != Kind::Seal {
        return Err(format!("expected Seal kind, got {:?}", seal.kind));
    }
    let keys = Keys::generate();
    let secret = keys.secret_key().clone();
    let content = nip44::encrypt(
        keys.secret_key(),
        receiver,
        seal.as_json(),
        nip44::Version::default(),
    )
    .map_err(|e| format!("nip44 encrypt: {}", e))?;
    let mut tags: Vec<Tag> = extra_tags.into_iter().collect();
    tags.push(Tag::public_key(*receiver));
    let event = EventBuilder::new(Kind::GiftWrap, content)
        .tags(tags)
        .custom_created_at(Timestamp::tweaked(RANGE_RANDOM_TIMESTAMP_TWEAK))
        .sign_with_keys(&keys)
        .map_err(|e| format!("sign wrap: {}", e))?;
    Ok((event, secret))
}

/// Outcome of a retained-key gift-wrap send. Caller is expected to
/// persist `wrap_event_id`, `wrap_secret`, and `targeted_relays` for
/// future deletion.
pub struct GiftWrapSendOutcome {
    pub output: Output<EventId>,
    pub wrap_event_id: EventId,
    pub wrap_secret: SecretKey,
    /// Relay URL set we attempted (inbox if known, pool write-relays as
    /// fallback). Deletion publishes the NIP-09 to this same set.
    pub targeted_relays: Vec<String>,
}

/// Send a gift-wrapped rumor to a recipient using a retained ephemeral
/// key. Routes to the recipient's inbox relays (kind 10050) when
/// available, falling back to pool write-relays otherwise.
///
/// Spawns one publish task per resolved relay and registers a
/// `EventPublishTracker` keyed by wrap event id so the deletion path
/// can fire NIP-09 to each relay as soon as that relay confirms the
/// wrap (closing the publish/delete race for fast deleters). Returns
/// the wrap event id, the ephemeral secret, and the relay set
/// attempted.
pub async fn send_gift_wrap_retained(
    client: &Client,
    recipient: &PublicKey,
    rumor: UnsignedEvent,
    extra_tags: impl IntoIterator<Item = Tag>,
) -> Result<GiftWrapSendOutcome, String> {
    let signer = client.signer().await.map_err(|e| e.to_string())?;
    let seal: Event = EventBuilder::seal(&signer, recipient, rumor)
        .await
        .map_err(|e| e.to_string())?
        .sign(&signer)
        .await
        .map_err(|e| e.to_string())?;
    let (event, secret) = wrap_with_retained_key(recipient, &seal, extra_tags)?;
    let wrap_event_id = event.id;

    // Resolve target relays: recipient's inbox relays (NIP-17) if they
    // advertise any, otherwise our pool's write-relays.
    let inbox_strs = get_or_fetch_inbox_relays(client, recipient).await;
    let targeted_strs: Vec<String> = if !inbox_strs.is_empty() {
        inbox_strs.clone()
    } else {
        let pool = client.pool();
        let relays = pool.relays().await;
        relays.iter()
            .filter(|(_, r)| r.flags().has_write())
            .map(|(url, _)| url.to_string())
            .collect()
    };
    // Resolve to live Relay handles in the pool. Strict HashMap lookup by
    // `RelayUrl` was missing visually-identical URLs because nostr-sdk
    // canonicalises differently between published-10050 strings and pool
    // keys (trailing slashes, default ports, case). Normalise both sides
    // and match on the canonical string form so e.g. `wss://relay.damus.io`
    // and `wss://relay.damus.io/` count as the same relay.
    use normalize_relay_url as normalize_url_for_match;
    let pool = client.pool();
    // all_relays(): GOSSIP-flagged pool members (discovery/community relays)
    // must count as pooled here — classifying one as transient would remove
    // it from the pool after the send, silently killing its real role.
    let pool_relays = pool.all_relays().await;
    let pool_norm: Vec<(String, RelayUrl, Relay)> = pool_relays.iter()
        .map(|(url, relay)| (
            normalize_url_for_match(&url.to_string()),
            url.clone(),
            relay.clone(),
        ))
        .collect();
    let mut resolved: Vec<(RelayUrl, Relay)> = targeted_strs
        .iter()
        .filter_map(|s| {
            let norm = normalize_url_for_match(s);
            pool_norm.iter()
                .find(|(pnorm, _, _)| pnorm == &norm)
                .map(|(_, url, relay)| (url.clone(), relay.clone()))
        })
        .collect();

    // On-demand connect: inbox relays not already in the pool are added +
    // connected just for this send, then removed afterwards (transient_added).
    // The recipient's inbox relays are theirs, not ours — keeping them would
    // pollute the pool, which the reconcile loop owns. Only for real inbox
    // relays; the pool-write fallback already targets live pool members.
    let mut transient_added: Vec<RelayUrl> = Vec::new();
    if !inbox_strs.is_empty() {
        for s in &targeted_strs {
            let norm = normalize_url_for_match(s);
            let in_pool = pool_norm.iter().any(|(p, _, _)| p == &norm);
            let already_added = transient_added.iter()
                .any(|u| normalize_url_for_match(&u.to_string()) == norm);
            if in_pool || already_added { continue; }

            let opts = crate::tor_aware_relay_options(RelayOptions::new().reconnect(false));
            if pool.add_relay(s.as_str(), opts).await.is_ok() {
                if let Ok(relay) = pool.relay(s.as_str()).await {
                    let _ = relay.try_connect(std::time::Duration::from_secs(6)).await;
                    transient_added.push(relay.url().clone());
                    resolved.push((relay.url().clone(), relay));
                }
            }
        }
        if !transient_added.is_empty() {
            crate::log_info!(
                "[InboxRelays] on-demand connected {} inbox relay(s) for {} (transient)",
                transient_added.len(),
                recipient,
            );
        }
    }

    // `resolved.is_empty()` implies no transient add succeeded (each success
    // pushes onto `resolved`), so this branch can't leak a transient relay.
    if resolved.is_empty() {
        // No matching relays in the pool — last-ditch broadcast via
        // client.send_event(). No tracker (no per-relay machinery).
        let output = client
            .send_event(&event)
            .await
            .map_err(|e| e.to_string())?;
        return Ok(GiftWrapSendOutcome {
            output,
            wrap_event_id,
            wrap_secret: secret,
            targeted_relays: targeted_strs,
        });
    }

    if !inbox_strs.is_empty() {
        println!(
            "[InboxRelays] Routing gift-wrap to {} inbox relays for {}",
            resolved.len(),
            recipient
        );
    }

    // Spawn tracked per-relay publish tasks. The tracker is keyed by
    // the wrap event id; the deletion path looks it up via
    // get_publish_tracker(wrap_event_id) and walks next_success() to
    // fire NIP-09 only at relays that have actually received the
    // wrap. The same primitive is used by any other operation whose
    // dependent event must arrive after the parent on each relay
    // (rapid edits, self-reactions, replies-to-just-sent).
    let handles = spawn_tracked_publish(resolved, event.clone());

    // Race for first-ok so the caller (and UI) sees "Sent" the
    // moment any one relay accepts. Remaining tasks continue in
    // the background, updating the tracker as they settle. The
    // dropped JoinHandles detach but do not cancel the tasks.
    let mut output = Output {
        val: wrap_event_id,
        success: HashSet::new(),
        failed: HashMap::new(),
    };
    let mut remaining = handles;
    while !remaining.is_empty() {
        let (result, _idx, rest) = futures_util::future::select_all(remaining).await;
        remaining = rest;
        if let Ok((url, relay_result)) = result {
            match relay_result {
                Ok(_) => {
                    output.success.insert(url);
                    drop(remaining);
                    break;
                }
                Err(e) => {
                    output.failed.insert(url, e.to_string());
                }
            }
        }
    }

    // Tear down transiently-added inbox relays — they belong to the
    // recipient, not us. Delivery has already raced to first-ok; one
    // confirmed inbox relay satisfies NIP-17, so cutting any still-in-flight
    // background publishes to the others is acceptable.
    for url in &transient_added {
        let _ = pool.remove_relay(url).await;
    }

    Ok(GiftWrapSendOutcome {
        output,
        wrap_event_id,
        wrap_secret: secret,
        targeted_relays: targeted_strs,
    })
}

/// Send a gift-wrapped rumor to a recipient, routing to their inbox relays
/// (kind 10050) when available. Falls back to pool broadcast if no inbox
/// relays are found or if targeted delivery fails entirely.
///
/// Returns as soon as the first relay acknowledges success — remaining relays
/// continue in the background. This minimises the time messages spend in
/// "pending" state.
///
/// Thin wrapper over `send_gift_wrap_retained`. Discards the retained
/// ephemeral key — use this for sends where future deletion is not
/// required (e.g. PIVX payment rumors). For user-facing DMs, prefer
/// `send_gift_wrap_retained` and persist the secret.
pub async fn send_gift_wrap(
    client: &Client,
    recipient: &PublicKey,
    rumor: UnsignedEvent,
    extra_tags: impl IntoIterator<Item = Tag>,
) -> Result<Output<EventId>, String> {
    let outcome = send_gift_wrap_retained(client, recipient, rumor, extra_tags).await?;
    Ok(outcome.output)
}

// ============================================================================
// Publish own inbox relays (sync-first, merge, never clobber)
// ============================================================================

/// KV key holding the relay urls Vector itself contributed to the published
/// kind 10050 (JSON array, normalized). Foreign entries (added by other
/// clients) are everything in the network list minus this set — they are
/// preserved verbatim on every publish and never removed by a Vector config
/// change.
const CONTRIBUTED_KEY: &str = "dm_relays_contributed";

/// Cap on foreign relay entries adopted into our published list. Bounds a
/// hostile or bloated remote list from being re-signed and amplified by us.
const MAX_FOREIGN_RELAYS: usize = 10;

fn load_contributed() -> HashSet<String> {
    crate::db::get_sql_setting(CONTRIBUTED_KEY.to_string())
        .ok()
        .flatten()
        .and_then(|json| serde_json::from_str::<Vec<String>>(&json).ok())
        .map(|v| v.into_iter().map(|s| normalize_relay_url(&s)).collect())
        .unwrap_or_default()
}

fn store_contributed(contributed: &[String]) {
    if let Ok(json) = serde_json::to_string(contributed) {
        let _ = crate::db::set_sql_setting(CONTRIBUTED_KEY.to_string(), json);
    }
}

/// The merged list to publish and whether the network needs updating.
struct MergePlan {
    /// Final relay list, original string forms preserved (remote order first,
    /// then our additions).
    list: Vec<String>,
    /// `true` if the final list differs (as a set) from the remote list.
    changed: bool,
    /// Normalized urls that count as Vector's contribution going forward:
    /// our current read relays minus anything another client also lists.
    contributed: Vec<String>,
}

/// Merge the network's current 10050 with our read relays. Foreign entries
/// (remote minus our previous contribution) always survive; entries we
/// contributed earlier but no longer read are dropped — a relay removed in
/// Vector's settings leaves the list, a relay added by another app never does.
fn merge_inbox_relays(
    remote: &[String],
    contributed_before: &HashSet<String>,
    ours: &[String],
) -> MergePlan {
    let mut seen: HashSet<String> = HashSet::new();
    let mut list: Vec<String> = Vec::new();
    let mut foreign_norm: HashSet<String> = HashSet::new();
    let mut dropped_foreign = 0usize;

    for url in remote {
        let norm = normalize_relay_url(url);
        if seen.contains(&norm) || contributed_before.contains(&norm) {
            continue;
        }
        if foreign_norm.len() >= MAX_FOREIGN_RELAYS {
            dropped_foreign += 1;
            continue;
        }
        seen.insert(norm.clone());
        foreign_norm.insert(norm);
        list.push(url.clone());
    }
    if dropped_foreign > 0 {
        crate::log_warn!(
            "[InboxRelays] remote 10050 over the {}-relay foreign cap, dropped {}",
            MAX_FOREIGN_RELAYS,
            dropped_foreign
        );
    }

    let mut contributed: Vec<String> = Vec::new();
    for url in ours {
        let norm = normalize_relay_url(url);
        if seen.insert(norm.clone()) {
            list.push(url.clone());
        }
        if !foreign_norm.contains(&norm) && !contributed.contains(&norm) {
            contributed.push(norm);
        }
    }

    // Publish only on an OWN diff: something of ours missing from the remote
    // list, or a previous contribution of ours still listed that we no longer
    // read. A foreign-cap trim alone must never drive a publish — two devices
    // straddling the cap would ping-pong trimmed/untrimmed lists forever.
    let remote_set: HashSet<String> = remote.iter().map(|s| normalize_relay_url(s)).collect();
    let ours_norm: HashSet<String> = ours.iter().map(|s| normalize_relay_url(s)).collect();
    let has_addition = ours_norm.iter().any(|n| !remote_set.contains(n));
    let has_removal = contributed_before
        .iter()
        .any(|n| remote_set.contains(n) && !ours_norm.contains(n));
    MergePlan { list, changed: has_addition || has_removal, contributed }
}

/// Fetch our OWN current 10050 from read + Discovery relays, with its
/// created_at. `Ok(None)` means the network answered and no list exists;
/// `Err` means we could not get a trustworthy answer (offline, nothing
/// connected) — callers must NOT publish on `Err`, that is exactly the blind
/// overwrite this module exists to stop.
pub async fn fetch_own_inbox_list(client: &Client) -> Result<Option<(Vec<String>, u64)>, String> {
    let me = crate::state::my_public_key().ok_or("no active pubkey")?;
    let targets = inbox_query_targets(client).await;
    if targets.is_empty() {
        return Err("no query targets in pool".to_string());
    }

    // Wait (bounded) for targets to be live: a fetch raced against boot-time
    // connection setup returns empty, which reads as "no list exists" and
    // triggers a bootstrap publish over a list we simply couldn't see yet.
    // A Discovery Relay counts for an early exit (they are the rendezvous
    // most likely to hold the list); otherwise any connected target at the
    // deadline is best-effort, and zero connected targets aborts the sync.
    let discovery: HashSet<String> = crate::state::discovery_relay_iter()
        .map(normalize_relay_url)
        .collect();
    let deadline = Instant::now() + std::time::Duration::from_secs(8);
    loop {
        let relays = client.pool().all_relays().await;
        let connected: Vec<&RelayUrl> = targets
            .iter()
            .filter(|url| {
                relays
                    .get(url)
                    .map(|r| r.status() == RelayStatus::Connected)
                    .unwrap_or(false)
            })
            .collect();
        let discovery_up = connected
            .iter()
            .any(|url| discovery.contains(&normalize_relay_url(url.as_str())));
        if discovery_up {
            break;
        }
        if Instant::now() >= deadline {
            if connected.is_empty() {
                return Err("no query target connected".to_string());
            }
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }

    let filter = Filter::new().author(me).kind(Kind::Custom(10050)).limit(1);
    let events = client
        .fetch_events_from(targets.clone(), filter, std::time::Duration::from_secs(6))
        .await
        .map_err(|e| e.to_string())?;
    // NIP-01 replaceable tie-break: newest created_at, lowest id on a tie.
    let newest = events
        .into_iter()
        .max_by(|a, b| a.created_at.cmp(&b.created_at).then(b.id.cmp(&a.id)))
        .map(|e| (parse_relay_tags(&e.tags), e.created_at.as_u64()));

    // "No list found" is only trustworthy when a Discovery Relay answered:
    // a list curated elsewhere may live on none of our own relays, and a
    // bootstrap publish over an unseen list is a PERMANENT clobber (the
    // bootstrap gets the newer created_at, so later syncs adopt it and
    // relays delete the old list). Pools with no Discovery Relay at all
    // (SDK/CLI) keep the any-target answer.
    if newest.is_none() {
        let has_discovery_target = targets
            .iter()
            .any(|url| discovery.contains(&normalize_relay_url(url.as_str())));
        if has_discovery_target {
            let relays = client.pool().all_relays().await;
            let discovery_answered = targets.iter().any(|url| {
                discovery.contains(&normalize_relay_url(url.as_str()))
                    && relays
                        .get(url)
                        .map(|r| r.status() == RelayStatus::Connected)
                        .unwrap_or(false)
            });
            if !discovery_answered {
                return Err(
                    "no 10050 found and no Discovery Relay connected; refusing to bootstrap"
                        .to_string(),
                );
            }
        }
    }
    Ok(newest)
}

/// KV key holding the created_at of the newest 10050 we have published or
/// applied locally. Inbound reconcile actions are gated on a STRICTLY newer
/// remote event: acting on a stale fetch would resurrect relays the user
/// already retired.
const LIST_SEEN_TS_KEY: &str = "dm_list_last_ts";

fn load_list_seen() -> u64 {
    crate::db::get_sql_setting(LIST_SEEN_TS_KEY.to_string())
        .ok()
        .flatten()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0)
}

/// Union urls into the contributed set. Adopted/revived relays must count as
/// OUR contribution immediately: retire only fires for contributed entries,
/// and a merge would otherwise re-add ("resurrect") a relay that a newer
/// remote list deliberately dropped.
pub fn note_contributed(urls: &[String]) {
    if urls.is_empty() {
        return;
    }
    let mut set = load_contributed();
    for url in urls {
        set.insert(normalize_relay_url(url));
    }
    let list: Vec<String> = set.into_iter().collect();
    store_contributed(&list);
}

/// Advance the list-freshness anchor (monotonic).
pub fn note_list_seen(ts: u64) {
    if ts > load_list_seen() {
        let _ = crate::db::set_sql_setting(LIST_SEEN_TS_KEY.to_string(), ts.to_string());
    }
}

/// Local actions needed to make the in-app relay list mirror a newer remote
/// 10050 (the Relays tab IS the DM Relay List, synced across devices/apps).
#[derive(Debug, Default, PartialEq)]
pub struct InboundReconcile {
    /// Remote entries unknown locally: add as enabled relays.
    pub adopt: Vec<String>,
    /// Locally-disabled entries a newer remote (re-)lists: re-enable.
    pub revive: Vec<String>,
    /// Entries we previously published that a newer remote dropped: another
    /// client removed them, disable locally.
    pub retire: Vec<String>,
}

/// Plan the inbound half of the sync. `ours` = locally enabled relay urls,
/// `declined` = locally known but disabled urls. Reads the contributed set
/// and freshness anchor from the account KV.
pub fn plan_inbound_reconcile(
    remote: &[String],
    remote_ts: u64,
    ours: &[String],
    declined: &[String],
) -> InboundReconcile {
    plan_inbound_reconcile_pure(
        remote,
        remote_ts,
        ours,
        declined,
        &load_contributed(),
        load_list_seen(),
    )
}

fn plan_inbound_reconcile_pure(
    remote: &[String],
    remote_ts: u64,
    ours: &[String],
    declined: &[String],
    contributed_before: &HashSet<String>,
    last_seen_ts: u64,
) -> InboundReconcile {
    if remote_ts <= last_seen_ts {
        return InboundReconcile::default();
    }
    let ours_norm: HashSet<String> = ours.iter().map(|s| normalize_relay_url(s)).collect();
    let declined_norm: HashSet<String> = declined.iter().map(|s| normalize_relay_url(s)).collect();
    let remote_norm: HashSet<String> = remote.iter().map(|s| normalize_relay_url(s)).collect();

    let mut seen: HashSet<String> = HashSet::new();
    let mut adopt: Vec<String> = Vec::new();
    let mut revive: Vec<String> = Vec::new();
    for url in remote {
        let norm = normalize_relay_url(url);
        if !seen.insert(norm.clone()) {
            continue;
        }
        if ours_norm.contains(&norm) {
            continue;
        }
        if declined_norm.contains(&norm) {
            revive.push(url.clone());
        } else if adopt.len() < MAX_FOREIGN_RELAYS
            && url.starts_with("wss://")
            && url.len() <= 256
        {
            adopt.push(url.clone());
        }
    }

    let retire: Vec<String> = ours
        .iter()
        .filter(|url| {
            let norm = normalize_relay_url(url);
            contributed_before.contains(&norm) && !remote_norm.contains(&norm)
        })
        .cloned()
        .collect();

    InboundReconcile { adopt, revive, retire }
}

/// Serializes publish_inbox_relays bodies. Overlapping runs (boot spawn vs a
/// debounced republish) can sign in inverted order — the stale list gets the
/// newer created_at and its stale contributed-set lands last in the KV.
static PUBLISH_MUTEX: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Sync our kind 10050 from the network, merge our readable relays into it,
/// and publish ONLY if that changes the list. Lists curated in other clients
/// are preserved: foreign entries are re-signed verbatim, never dropped.
/// Publishes to our write relays AND the Discovery Relays so the result is
/// findable by clients with no relay overlap.
pub async fn publish_inbox_relays(client: &Client) -> Result<(), String> {
    // A failed sync must skip the publish: publishing blind clobbers lists
    // curated in other clients.
    let remote = fetch_own_inbox_list(client).await?;
    publish_inbox_relays_synced(client, remote, None).await
}

/// Publish with an already-synced remote list (avoids a second network fetch
/// when the caller just ran the inbound reconcile). `ours_override` supplies
/// the caller's store-derived relay list: the live pool momentarily contains
/// relays that are not ours (a recipient's transient inbox relays mid-DM) and
/// can be missing ours (an adopted relay whose connect failed) — publishing
/// pool state would leak the former fleet-wide and oscillate the latter.
pub async fn publish_inbox_relays_synced(
    client: &Client,
    remote: Option<(Vec<String>, u64)>,
    ours_override: Option<Vec<String>>,
) -> Result<(), String> {
    let _serial = PUBLISH_MUTEX.lock().await;
    let session = crate::state::SessionGuard::capture();

    // Relays we read from — senders must write to these for us to receive DMs.
    let ours: Vec<String> = match ours_override {
        Some(list) => list,
        None => client
            .pool()
            .relays()
            .await
            .iter()
            .filter(|(_, relay)| relay.flags().has_read())
            .map(|(url, _)| url.to_string())
            .collect(),
    };

    let remote_found = remote.is_some();
    let (remote, remote_ts) = remote.unwrap_or_default();
    // A fetch can answer with an OLDER revision than one we've already seen
    // (the relay holding the newest missed the timeout). Merging against it
    // would republish removed-elsewhere entries at a newer created_at —
    // resurrection the inbound gate cannot defend against.
    if remote_found && remote_ts < load_list_seen() {
        return Err("stale 10050 fetch (older than last seen), skipping publish".to_string());
    }

    let plan = merge_inbox_relays(&remote, &load_contributed(), &ours);

    if !session.is_valid() {
        return Ok(());
    }
    store_contributed(&plan.contributed);
    if remote_found {
        note_list_seen(remote_ts);
    }

    if remote_found && !plan.changed {
        crate::log_info!(
            "[InboxRelays] kind 10050 already in sync ({} relay(s)), not publishing",
            plan.list.len()
        );
        return Ok(());
    }
    if plan.list.is_empty() && !remote_found {
        // Nothing to say and nothing to update.
        return Ok(());
    }

    let mut builder = EventBuilder::new(Kind::Custom(10050), "");
    for url in &plan.list {
        builder = builder.tag(Tag::custom(TagKind::custom("relay"), vec![url.clone()]));
    }
    let event = client
        .sign_event_builder(builder)
        .await
        .map_err(|e| format!("Failed to sign inbox relays: {}", e))?;

    if !session.is_valid() {
        return Ok(());
    }
    let pool_send = client.send_event(&event).await;

    // Copy to the pooled Discovery Relays (GOSSIP-flagged, so the pool-wide
    // send skipped them). Runs regardless of the pool send: a user with zero
    // write relays still gets their list onto the rendezvous points.
    let discovery: HashSet<String> = crate::state::DISCOVERY_RELAYS
        .iter()
        .map(|s| normalize_relay_url(s))
        .collect();
    let discovery_targets: Vec<RelayUrl> = client
        .pool()
        .all_relays()
        .await
        .iter()
        .filter(|(url, relay)| {
            !relay.flags().has_write() && discovery.contains(&normalize_relay_url(url.as_str()))
        })
        .map(|(url, _)| url.clone())
        .collect();
    let mut discovery_ok = false;
    if !discovery_targets.is_empty() {
        if let Ok(out) = client.send_event_to(discovery_targets, &event).await {
            discovery_ok = !out.success.is_empty();
        }
    }

    let pool_ok = matches!(&pool_send, Ok(out) if !out.success.is_empty());
    if !pool_ok {
        if !discovery_ok {
            return Err(match pool_send {
                Err(e) => format!("Failed to publish inbox relays: {}", e),
                Ok(_) => "Failed to publish inbox relays: no relay accepted it".to_string(),
            });
        }
        crate::log_warn!(
            "[InboxRelays] pool publish failed, list delivered via Discovery Relays only"
        );
    }
    // Anchor only on a confirmed landing in a still-current session: a
    // wrongly-advanced anchor gates future syncs off real network state.
    if session.is_valid() {
        note_list_seen(event.created_at.as_u64().max(remote_ts));
    }

    println!(
        "[InboxRelays] Published kind 10050 with {} relay(s) ({} foreign preserved)",
        plan.list.len(),
        plan.list.len().saturating_sub(plan.contributed.len())
    );
    Ok(())
}

/// Monotonic generation counter used to debounce republish calls.
/// Only the most recent spawn actually publishes; earlier ones exit early.
static REPUBLISH_GEN: AtomicU64 = AtomicU64::new(0);

/// Counts how many spawned tasks pass the generation gate (test-only).
#[cfg(test)]
static DEBOUNCE_PASS_COUNT: AtomicU64 = AtomicU64::new(0);

/// Republish kind 10050 in the background (debounced).
/// Called after relay config changes (add/remove/toggle/mode update).
/// Rapid successive calls coalesce into a single publish.
pub fn republish_inbox_relays_debounced() {
    let gen = REPUBLISH_GEN.fetch_add(1, Ordering::SeqCst) + 1;
    // REPUBLISH_GEN dedupes within a session; SessionGuard dedupes
    // across sessions. Without the guard, a swap during the 800ms
    // debounce window would publish account A's inbox-relay claim
    // signed by account B's client.
    let session = crate::state::SessionGuard::capture();
    tokio::spawn(async move {
        // Wait for the relay pool to settle; if another call arrives
        // during this window it will bump the generation and we'll exit.
        tokio::time::sleep(std::time::Duration::from_millis(800)).await;
        if REPUBLISH_GEN.load(Ordering::SeqCst) != gen {
            return; // superseded by a newer call
        }
        if !session.is_valid() {
            return; // swap occurred during the debounce window
        }
        #[cfg(test)]
        DEBOUNCE_PASS_COUNT.fetch_add(1, Ordering::SeqCst);
        let client = match nostr_client() {
            Some(c) => c,
            None => return,
        };
        if let Err(e) = publish_inbox_relays(&client).await {
            eprintln!("[InboxRelays] Failed to republish after config change: {}", e);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- Merge (sync-first publish) ----

    fn strs(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn norm_set(v: &[&str]) -> HashSet<String> {
        v.iter().map(|s| normalize_relay_url(s)).collect()
    }

    #[test]
    fn merge_preserves_foreign_entries() {
        let remote = strs(&["wss://other-app.example", "wss://alice.example"]);
        let ours = strs(&["wss://vector.example"]);
        let plan = merge_inbox_relays(&remote, &HashSet::new(), &ours);
        assert!(plan.changed);
        assert_eq!(plan.list, strs(&["wss://other-app.example", "wss://alice.example", "wss://vector.example"]));
        assert_eq!(plan.contributed, strs(&["wss://vector.example"]));
    }

    #[test]
    fn merge_noop_when_remote_covers_ours() {
        let remote = strs(&["wss://other-app.example", "wss://vector.example/"]);
        let ours = strs(&["wss://vector.example"]);
        let plan = merge_inbox_relays(&remote, &HashSet::new(), &ours);
        assert!(!plan.changed, "trailing-slash variants are the same relay");
        assert_eq!(plan.list.len(), 2);
    }

    #[test]
    fn merge_drops_only_our_own_removed_contribution() {
        // We contributed X earlier; the user removed it in Vector. Foreign F
        // stays, X leaves.
        let remote = strs(&["wss://foreign.example", "wss://x.example"]);
        let contributed = norm_set(&["wss://x.example"]);
        let ours = strs(&["wss://new.example"]);
        let plan = merge_inbox_relays(&remote, &contributed, &ours);
        assert!(plan.changed);
        assert_eq!(plan.list, strs(&["wss://foreign.example", "wss://new.example"]));
    }

    #[test]
    fn merge_never_clears_a_foreign_list() {
        // No readable relays of our own must NOT nuke another client's list.
        let remote = strs(&["wss://foreign.example"]);
        let plan = merge_inbox_relays(&remote, &HashSet::new(), &[]);
        assert!(!plan.changed);
        assert_eq!(plan.list, remote);
        assert!(plan.contributed.is_empty());
    }

    #[test]
    fn merge_contributed_excludes_foreign_overlap() {
        // At the MERGE layer a co-listed relay classifies foreign-first (a
        // bare publish never strips it). Propagating removals of co-listed
        // entries is the reconcile layer's job: it seeds co-ownership into
        // the contributed set for entries both remote-listed and locally
        // enabled (see reconcile_two_devices_propagates_default_disable).
        let remote = strs(&["wss://shared.example"]);
        let ours = strs(&["wss://shared.example", "wss://mine.example"]);
        let plan = merge_inbox_relays(&remote, &HashSet::new(), &ours);
        assert_eq!(plan.contributed, strs(&["wss://mine.example"]));
        let next = merge_inbox_relays(
            &plan.list,
            &plan.contributed.iter().cloned().collect(),
            &[],
        );
        assert!(next.list.contains(&"wss://shared.example".to_string()));
        assert!(!next.list.contains(&"wss://mine.example".to_string()));
    }

    #[test]
    fn merge_caps_foreign_bloat_without_publishing() {
        // The cap bounds what WE would re-sign, but a trim alone is not an
        // own diff — publishing it would ping-pong against any client
        // maintaining an 11+ list.
        let remote: Vec<String> = (0..30).map(|i| format!("wss://r{}.example", i)).collect();
        let plan = merge_inbox_relays(&remote, &HashSet::new(), &[]);
        assert_eq!(plan.list.len(), MAX_FOREIGN_RELAYS);
        assert!(!plan.changed, "a trim alone must not drive a publish");
    }

    #[test]
    fn merge_cap_applies_when_own_diff_publishes() {
        let remote: Vec<String> = (0..30).map(|i| format!("wss://r{}.example", i)).collect();
        let ours = strs(&["wss://mine.example"]);
        let plan = merge_inbox_relays(&remote, &HashSet::new(), &ours);
        assert!(plan.changed, "our addition is a real diff");
        assert_eq!(plan.list.len(), MAX_FOREIGN_RELAYS + 1);
        assert!(plan.list.contains(&"wss://mine.example".to_string()));
    }

    #[test]
    fn merge_two_devices_reach_fixpoint() {
        // Two devices with different configs alternating sync->merge->publish
        // must converge: bounded publishes, then permanent in-sync.
        let ours_a = strs(&["wss://a1.example", "wss://shared.example"]);
        let ours_b = strs(&["wss://b1.example", "wss://shared.example"]);
        let mut network = strs(&["wss://foreign.example"]);
        let mut contributed_a: HashSet<String> = HashSet::new();
        let mut contributed_b: HashSet<String> = HashSet::new();
        let mut publishes = 0;
        for round in 0..6 {
            for device in 0..2 {
                let (ours, contributed) = if device == 0 {
                    (&ours_a, &mut contributed_a)
                } else {
                    (&ours_b, &mut contributed_b)
                };
                let plan = merge_inbox_relays(&network, contributed, ours);
                *contributed = plan.contributed.iter().cloned().collect();
                if plan.changed {
                    publishes += 1;
                    network = plan.list;
                }
                if round >= 2 {
                    assert!(!plan.changed, "no publish after convergence (round {round})");
                }
            }
        }
        assert!(publishes <= 2, "one publish per device to converge, got {publishes}");
        for url in ["wss://foreign.example", "wss://a1.example", "wss://b1.example", "wss://shared.example"] {
            assert!(network.contains(&url.to_string()), "union must hold {url}");
        }
    }

    #[test]
    fn merge_first_run_publishes_ours() {
        let ours = strs(&["wss://a.example", "wss://b.example"]);
        let plan = merge_inbox_relays(&[], &HashSet::new(), &ours);
        assert!(plan.changed);
        assert_eq!(plan.list, ours);
        assert_eq!(plan.contributed, ours);
    }

    // ---- Inbound reconcile planner ----

    #[test]
    fn reconcile_stale_remote_is_a_no_op() {
        let remote = strs(&["wss://foreign.example"]);
        let plan = plan_inbound_reconcile_pure(&remote, 100, &[], &[], &HashSet::new(), 100);
        assert_eq!(plan, InboundReconcile::default(), "ts <= last_seen must not act");
    }

    #[test]
    fn reconcile_adopts_unknown_entries_capped_and_wss_only() {
        let mut remote: Vec<String> = (0..12).map(|i| format!("wss://r{}.example", i)).collect();
        remote.push("ws://plaintext.example".to_string());
        remote.push("http://nope.example".to_string());
        let plan = plan_inbound_reconcile_pure(&remote, 200, &[], &[], &HashSet::new(), 100);
        assert_eq!(plan.adopt.len(), MAX_FOREIGN_RELAYS);
        assert!(plan.adopt.iter().all(|u| u.starts_with("wss://")));
        assert!(plan.revive.is_empty() && plan.retire.is_empty());
    }

    #[test]
    fn reconcile_revives_locally_disabled_entry() {
        let remote = strs(&["wss://back.example"]);
        let declined = strs(&["wss://back.example/"]);
        let plan = plan_inbound_reconcile_pure(&remote, 200, &[], &declined, &HashSet::new(), 100);
        assert_eq!(plan.revive, strs(&["wss://back.example"]));
        assert!(plan.adopt.is_empty());
    }

    #[test]
    fn reconcile_retires_contributed_entry_dropped_by_newer_remote() {
        let remote = strs(&["wss://keep.example"]);
        let ours = strs(&["wss://keep.example", "wss://gone.example"]);
        let contributed = norm_set(&["wss://keep.example", "wss://gone.example"]);
        let plan = plan_inbound_reconcile_pure(&remote, 200, &ours, &[], &contributed, 100);
        assert_eq!(plan.retire, strs(&["wss://gone.example"]));
    }

    #[test]
    fn reconcile_never_retires_unpublished_local_addition() {
        // A relay added locally but not yet published is absent from any
        // remote; retiring it would erase fresh user intent.
        let remote = strs(&["wss://old.example"]);
        let ours = strs(&["wss://old.example", "wss://just-added.example"]);
        let contributed = norm_set(&["wss://old.example"]);
        let plan = plan_inbound_reconcile_pure(&remote, 200, &ours, &[], &contributed, 100);
        assert!(plan.retire.is_empty());
    }

    #[test]
    fn reconcile_two_devices_propagates_default_disable() {
        // Both devices ship the same defaults. The co-ownership seed (ours
        // that are also remote-listed join contributed) is what lets a
        // disable on one device propagate instead of being reverted.
        #[derive(Clone)]
        struct Device {
            ours: Vec<String>,
            declined: Vec<String>,
            contributed: HashSet<String>,
            last_seen: u64,
        }
        impl Device {
            fn new(defaults: &[&str]) -> Self {
                Device {
                    ours: strs(defaults),
                    declined: Vec::new(),
                    contributed: HashSet::new(),
                    last_seen: 0,
                }
            }
            /// Boot reconcile + publish, mirroring reconcile_dm_relay_list.
            fn sync(&mut self, network: &mut (Vec<String>, u64)) -> bool {
                let (remote, ts) = network.clone();
                for u in &self.ours {
                    if remote.iter().any(|r| normalize_relay_url(r) == normalize_relay_url(u)) {
                        self.contributed.insert(normalize_relay_url(u));
                    }
                }
                let plan = plan_inbound_reconcile_pure(
                    &remote, ts, &self.ours, &self.declined, &self.contributed, self.last_seen,
                );
                for u in &plan.retire {
                    self.ours.retain(|o| o != u);
                    self.declined.push(u.clone());
                }
                for u in &plan.revive {
                    self.declined.retain(|d| normalize_relay_url(d) != normalize_relay_url(u));
                    self.ours.push(u.clone());
                    self.contributed.insert(normalize_relay_url(u));
                }
                for u in &plan.adopt {
                    self.ours.push(u.clone());
                    self.contributed.insert(normalize_relay_url(u));
                }
                self.last_seen = self.last_seen.max(ts);
                let m = merge_inbox_relays(&remote, &self.contributed, &self.ours);
                self.contributed = m.contributed.iter().cloned().collect();
                if m.changed {
                    network.1 += 1;
                    network.0 = m.list;
                    self.last_seen = network.1;
                }
                m.changed
            }
        }

        const DEFAULTS: &[&str] = &["wss://d1.example", "wss://d2.example"];
        let mut network: (Vec<String>, u64) = (Vec::new(), 0);
        let mut a = Device::new(DEFAULTS);
        let mut b = Device::new(DEFAULTS);

        assert!(a.sync(&mut network), "first device bootstraps the list");
        assert!(!b.sync(&mut network), "second device is already in sync");

        // B disables d2 locally, publishes the removal.
        b.ours.retain(|u| u != "wss://d2.example");
        b.declined.push("wss://d2.example".to_string());
        assert!(b.sync(&mut network), "disable must publish");
        assert!(!network.0.contains(&"wss://d2.example".to_string()));

        // A's next boot retires d2 rather than re-adding it.
        assert!(!a.sync(&mut network), "A must adopt the removal, not republish d2");
        assert!(a.declined.contains(&"wss://d2.example".to_string()));
        assert!(!network.0.contains(&"wss://d2.example".to_string()), "no resurrection");

        // B re-enables d2: A revives it too.
        b.declined.retain(|u| u != "wss://d2.example");
        b.ours.push("wss://d2.example".to_string());
        assert!(b.sync(&mut network), "re-enable must publish");
        assert!(!a.sync(&mut network), "revive is inbound-only, no republish");
        assert!(a.ours.contains(&"wss://d2.example".to_string()), "A revives d2");

        // Fixpoint: quiet forever after.
        for _ in 0..3 {
            assert!(!a.sync(&mut network));
            assert!(!b.sync(&mut network));
        }
    }

    // ---- Tag parsing ----

    #[test]
    fn parse_relay_tags_extracts_urls() {
        let tags = Tags::from_list(vec![
            Tag::custom(TagKind::custom("relay"), vec!["wss://relay.example.com"]),
            Tag::custom(TagKind::custom("relay"), vec!["wss://other.example.com"]),
        ]);
        let result = parse_relay_tags(&tags);
        assert_eq!(result, vec![
            "wss://relay.example.com".to_string(),
            "wss://other.example.com".to_string(),
        ]);
    }

    #[test]
    fn parse_relay_tags_ignores_non_relay_tags() {
        let tags = Tags::from_list(vec![
            Tag::custom(TagKind::custom("relay"), vec!["wss://good.example.com"]),
            Tag::custom(TagKind::custom("p"), vec!["deadbeef"]),
            Tag::custom(TagKind::custom("e"), vec!["cafebabe"]),
        ]);
        let result = parse_relay_tags(&tags);
        assert_eq!(result, vec!["wss://good.example.com".to_string()]);
    }

    #[test]
    fn parse_relay_tags_empty() {
        let tags = Tags::new();
        let result = parse_relay_tags(&tags);
        assert!(result.is_empty());
    }

    #[test]
    fn parse_relay_tags_ignores_relay_tag_without_value() {
        // A ["relay"] tag with no URL should be skipped (len < 2)
        let tags = Tags::from_list(vec![
            Tag::custom(TagKind::custom("relay"), Vec::<String>::new()),
        ]);
        let result = parse_relay_tags(&tags);
        assert!(result.is_empty());
    }

    // ---- Cache ----

    fn test_pubkey() -> PublicKey {
        let keys = Keys::generate();
        keys.public_key()
    }

    // Serialize tests that mutate global cache/lock statics.
    static TEST_GLOBALS_LOCK: LazyLock<tokio::sync::Mutex<()>> =
        LazyLock::new(|| tokio::sync::Mutex::new(()));

    #[test]
    fn cache_stores_and_retrieves() {
        let _guard = TEST_GLOBALS_LOCK.blocking_lock();
        let pk = test_pubkey();
        let relays = vec!["wss://a.example.com".to_string()];

        {
            let mut cache = INBOX_RELAY_CACHE.lock().unwrap();
            cache.insert(pk, CachedRelays {
                relays: relays.clone(),
                fetched_at: Instant::now(),
                fetch_ok: true,
            });
        }

        let cache = INBOX_RELAY_CACHE.lock().unwrap();
        let entry = cache.get(&pk).unwrap();
        assert_eq!(entry.relays, relays);
        assert!(entry.fetch_ok);
        assert!(entry.fetched_at.elapsed().as_secs() < CACHE_TTL_SECS);
    }

    #[test]
    fn cache_expires_after_ttl() {
        let _guard = TEST_GLOBALS_LOCK.blocking_lock();
        let pk = test_pubkey();

        {
            let mut cache = INBOX_RELAY_CACHE.lock().unwrap();
            cache.insert(pk, CachedRelays {
                relays: vec!["wss://stale.example.com".to_string()],
                fetched_at: Instant::now() - std::time::Duration::from_secs(CACHE_TTL_SECS + 1),
                fetch_ok: true,
            });
        }

        let cache = INBOX_RELAY_CACHE.lock().unwrap();
        let entry = cache.get(&pk).unwrap();
        assert!(entry.fetched_at.elapsed().as_secs() >= CACHE_TTL_SECS);
    }

    #[test]
    fn cache_stores_empty_results() {
        let _guard = TEST_GLOBALS_LOCK.blocking_lock();
        let pk = test_pubkey();

        {
            let mut cache = INBOX_RELAY_CACHE.lock().unwrap();
            cache.insert(pk, CachedRelays {
                relays: vec![],
                fetched_at: Instant::now(),
                fetch_ok: true,
            });
        }

        let cache = INBOX_RELAY_CACHE.lock().unwrap();
        let entry = cache.get(&pk).unwrap();
        assert!(entry.relays.is_empty());
        assert!(entry.fetch_ok);
        assert!(entry.fetched_at.elapsed().as_secs() < CACHE_TTL_SECS);
    }

    #[test]
    fn cache_error_uses_short_ttl() {
        let _guard = TEST_GLOBALS_LOCK.blocking_lock();
        let pk = test_pubkey();

        {
            let mut cache = INBOX_RELAY_CACHE.lock().unwrap();
            cache.insert(pk, CachedRelays {
                relays: vec![],
                // Inserted 2 minutes ago — past the error TTL (60s) but within success TTL (3600s)
                fetched_at: Instant::now() - std::time::Duration::from_secs(120),
                fetch_ok: false,
            });
        }

        let cache = INBOX_RELAY_CACHE.lock().unwrap();
        let entry = cache.get(&pk).unwrap();
        assert!(!entry.fetch_ok);
        // Should be considered expired under error TTL
        assert!(entry.fetched_at.elapsed().as_secs() >= CACHE_TTL_ERROR_SECS);
        // But would still be valid under success TTL
        assert!(entry.fetched_at.elapsed().as_secs() < CACHE_TTL_SECS);
    }

    // ---- Concurrency / stampede prevention ----

    #[tokio::test]
    async fn concurrent_fetches_for_same_pubkey_serialize() {
        let _guard = TEST_GLOBALS_LOCK.lock().await;
        let pk = test_pubkey();

        // Clear cache so all tasks see a cold cache
        {
            let mut cache = INBOX_RELAY_CACHE.lock().unwrap();
            cache.remove(&pk);
        }

        let fetch_counter = Arc::new(AtomicU64::new(0));

        // Spawn 10 concurrent tasks all trying to fetch the same pubkey.
        // Uses production get_or_fetch_with_lock so this tests actual code path.
        let mut handles = vec![];
        for _ in 0..10 {
            let counter = fetch_counter.clone();
            let handle = tokio::spawn(async move {
                get_or_fetch_with_lock(&pk, || async {
                    counter.fetch_add(1, Ordering::SeqCst);
                    // Simulate network delay so concurrent tasks pile up
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    FetchResult {
                        relays: vec!["wss://test.example.com".to_string()],
                        fetch_ok: true,
                    }
                })
                .await
            });
            handles.push(handle);
        }

        // Wait for all tasks to complete
        let results = futures_util::future::join_all(handles).await;

        // All tasks should succeed and get the same result
        for result in &results {
            assert!(result.is_ok());
            let relays = result.as_ref().unwrap();
            assert_eq!(relays, &vec!["wss://test.example.com".to_string()]);
        }

        // CRITICAL: Only ONE fetch should have executed (others waited on lock + hit cache)
        assert_eq!(
            fetch_counter.load(Ordering::SeqCst),
            1,
            "Expected exactly 1 fetch for 10 concurrent requests to same pubkey"
        );

        let locks_after = {
            let locks = FETCH_LOCKS.lock().unwrap();
            locks.len()
        };
        assert_eq!(locks_after, 0, "Lock entry should be removed after all waiters complete");
    }

    #[tokio::test]
    async fn fetch_locks_do_not_accumulate_after_calls_complete() {
        let _guard = TEST_GLOBALS_LOCK.lock().await;

        // Verify that lock entries are removed eagerly when the last in-flight
        // caller for a key exits (true bounded growth, no idle-after-burst leak).

        let pk1 = test_pubkey();
        let pk2 = test_pubkey();
        let pk3 = test_pubkey();

        // Clear both cache and locks to avoid interference from other tests
        {
            let mut cache = INBOX_RELAY_CACHE.lock().unwrap();
            cache.clear();
        }
        {
            let mut locks = FETCH_LOCKS.lock().unwrap();
            locks.clear();
        }

        // Step 1: Fetch for pk1 (cache miss -> creates lock entry)
        get_or_fetch_with_lock(&pk1, || async {
            FetchResult {
                relays: vec!["wss://relay1.example.com".to_string()],
                fetch_ok: true,
            }
        })
        .await;

        // Single-call path: no waiters, so eager cleanup should remove key immediately.

        let locks_after_pk1 = {
            let locks = FETCH_LOCKS.lock().unwrap();
            locks.len()
        };
        assert_eq!(locks_after_pk1, 0, "No lock entries should remain after pk1 call");

        // Step 2: repeat with pk2
        get_or_fetch_with_lock(&pk2, || async {
            FetchResult {
                relays: vec!["wss://relay2.example.com".to_string()],
                fetch_ok: true,
            }
        })
        .await;

        let locks_after_pk2 = {
            let locks = FETCH_LOCKS.lock().unwrap();
            locks.len()
        };
        assert_eq!(locks_after_pk2, 0, "No lock entries should remain after pk2 call");

        // Step 3: repeat with pk3
        get_or_fetch_with_lock(&pk3, || async {
            FetchResult {
                relays: vec!["wss://relay3.example.com".to_string()],
                fetch_ok: true,
            }
        })
        .await;

        let locks_after_pk3 = {
            let locks = FETCH_LOCKS.lock().unwrap();
            locks.len()
        };
        assert_eq!(locks_after_pk3, 0, "No lock entries should remain after pk3 call");
    }

    #[tokio::test]
    async fn cancelled_fetch_cleans_up_lock_entry() {
        let _guard = TEST_GLOBALS_LOCK.lock().await;
        let pk = test_pubkey();

        {
            let mut cache = INBOX_RELAY_CACHE.lock().unwrap();
            cache.clear();
        }
        {
            let mut locks = FETCH_LOCKS.lock().unwrap();
            locks.clear();
        }

        let (started_tx, started_rx) = tokio::sync::oneshot::channel::<()>();
        let task_pk = pk;
        let handle = tokio::spawn(async move {
            get_or_fetch_with_lock(&task_pk, || async move {
                let _ = started_tx.send(());
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                FetchResult { relays: Vec::new(), fetch_ok: false }
            })
            .await
        });

        started_rx.await.expect("fetch closure should start before abort");
        handle.abort();
        let _ = handle.await;
        tokio::task::yield_now().await;

        let locks_after = {
            let locks = FETCH_LOCKS.lock().unwrap();
            locks.len()
        };
        assert_eq!(
            locks_after, 0,
            "Lock entry should be removed even if fetch task is cancelled"
        );
    }

    // ---- Debounce ----

    // `start_paused`: drive the 800ms debounce window on tokio's VIRTUAL clock, which auto-advances when
    // all tasks are parked. The spawned timers then resolve deterministically — no dependence on wall-clock
    // timing, so heavy parallel CPU load (e.g. the serialized vault stress tests) can't make the gate fire
    // after the test's wait and spuriously fail. (Was a real 1000ms sleep with only a 200ms margin.)
    #[tokio::test(start_paused = true)]
    async fn debounce_coalesces_rapid_calls_into_one() {
        // Snapshot counters before the burst.
        let gen_before = REPUBLISH_GEN.load(Ordering::SeqCst);
        let pass_before = DEBOUNCE_PASS_COUNT.load(Ordering::SeqCst);

        // Three rapid calls — only the last should survive the debounce gate.
        republish_inbox_relays_debounced();
        republish_inbox_relays_debounced();
        republish_inbox_relays_debounced();

        let gen_after = REPUBLISH_GEN.load(Ordering::SeqCst);
        assert_eq!(gen_after, gen_before + 3);

        // Past the 800ms window on the virtual clock (auto-advanced) so all spawned tasks resolve.
        tokio::time::sleep(std::time::Duration::from_millis(1000)).await;

        let pass_after = DEBOUNCE_PASS_COUNT.load(Ordering::SeqCst);
        // Exactly one task should have passed the generation gate.
        // (It then exits at nostr_client() since the client isn't
        // initialised in tests, but the coalescing behaviour is proven.)
        assert_eq!(pass_after - pass_before, 1);
    }
}
