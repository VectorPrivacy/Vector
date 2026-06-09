//! Per-account RAM cache for Community sync state.
//!
//! Consolidates what were three scattered `LazyLock` statics (oldest-page cursor, history-start
//! floors, in-flight page de-dup) into ONE structure under ONE invalidation key: the session
//! generation. Any access after an account swap — which bumps `current_session_generation()` —
//! transparently resets the cache, so stale per-channel state can never bleed into the next
//! account. Holds the page cursors (oldest back-paging floor + newest `since` floor), history-start
//! flags, and in-flight page de-dup; future RAM-cache work (e.g. invite preload) layers onto the
//! same structure + invalidation discipline.

use nostr_sdk::Event;
use std::collections::{HashMap, HashSet};
use std::sync::{LazyLock, Mutex, MutexGuard};
use std::time::{Duration, Instant};

#[derive(Default)]
struct CommunityCache {
    /// Session generation this cache reflects; a mismatch on access means an account swap
    /// happened → the cache is reset before use.
    generation: u64,
    /// In-flight page fetches, keyed `"{channel_id}:{older|latest}"`. Anti-stampede: an eager
    /// user scrolling/clicking can't fire the same page twice — the duplicate no-ops.
    inflight: HashSet<String>,
    /// Channels whose network history-start has been reached (an older-page fetch found nothing
    /// strictly older than the cursor). Older-page requests for these go DB-only.
    history_start: HashSet<String>,
    /// Oldest OUTER (wire send-time) created_at, in seconds, fetched per channel. The relay
    /// filters `until` against the outer created_at, so the back-paging cursor MUST be on that
    /// clock — not the inner authored `at`, which a hostile member can backdate/post-date.
    oldest_cursor: HashMap<String, u64>,
    /// Newest OUTER created_at (seconds) seen on a LATEST-page fetch per channel. Used as `since`
    /// on the next latest fetch so a routine re-sync returns only genuinely-new events instead of
    /// re-downloading + re-decrypting the same newest page. Advanced ONLY by latest fetches (never
    /// older pages) — it means "nothing newer than this needs a top-fetch"; any below-page gap is
    /// a back-pagination concern, not a top-fetch one.
    newest_cursor: HashMap<String, u64>,
}

static CACHE: LazyLock<Mutex<CommunityCache>> = LazyLock::new(|| Mutex::new(CommunityCache::default()));

/// Lock the cache, transparently resetting it if the session generation advanced (account swap).
fn locked() -> MutexGuard<'static, CommunityCache> {
    let generation = crate::state::current_session_generation();
    // Poison-tolerant: this cache is pure optimization state, so recover a poisoned guard rather
    // than cascade-panic every future community sync.
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if cache.generation != generation {
        *cache = CommunityCache { generation, ..Default::default() };
    }
    cache
}

/// Claim an in-flight page fetch (key `"{channel_id}:{older|latest}"`). Returns `false` if one is
/// already running — the caller should no-op. Pair with [`end_page_fetch`].
pub fn try_begin_page_fetch(key: &str) -> bool {
    locked().inflight.insert(key.to_string())
}

/// Release an in-flight page-fetch claim (success or error).
pub fn end_page_fetch(key: &str) {
    locked().inflight.remove(key);
}

/// Has the channel's network history-start been reached? Older pages then stay DB-only.
pub fn is_at_history_start(channel_id: &str) -> bool {
    locked().history_start.contains(channel_id)
}

/// Mark the channel as having reached its network history-start.
pub fn mark_history_start(channel_id: &str) {
    locked().history_start.insert(channel_id.to_string());
}

/// Oldest OUTER created_at (seconds) fetched for the channel — the back-paging cursor.
pub fn oldest_cursor(channel_id: &str) -> Option<u64> {
    locked().oldest_cursor.get(channel_id).copied()
}

/// Advance the back-paging cursor to the oldest wire time this page returned (monotonic — only
/// ever steps further back).
pub fn advance_oldest_cursor(channel_id: &str, oldest_secs: u64) {
    let mut cache = locked();
    let slot = cache.oldest_cursor.entry(channel_id.to_string()).or_insert(oldest_secs);
    *slot = (*slot).min(oldest_secs);
}

/// Newest OUTER created_at (seconds) seen on a latest page for the channel — the `since` floor
/// for the next latest fetch. `None` before the first latest fetch this session (→ full newest page).
pub fn newest_cursor(channel_id: &str) -> Option<u64> {
    locked().newest_cursor.get(channel_id).copied()
}

/// Advance the latest-page `since` floor to the newest wire time this page returned (monotonic —
/// only ever steps forward). Call ONLY for latest-page fetches.
pub fn advance_newest_cursor(channel_id: &str, newest_secs: u64) {
    let mut cache = locked();
    let slot = cache.newest_cursor.entry(channel_id.to_string()).or_insert(newest_secs);
    *slot = (*slot).max(newest_secs);
}

/// Clear a channel's back-paging floors (history-start + oldest cursor) — e.g. after a
/// multi-epoch backfill makes older history reachable again.
pub fn clear_channel_floors(channel_id: &str) {
    let mut cache = locked();
    cache.history_start.remove(channel_id);
    cache.oldest_cursor.remove(channel_id);
}

// ── Invite preload ──────────────────────────────────────────────────────────
// Warmed-ahead-of-Join state: the primary channel's first page, fetched at invite-receive /
// public-preview time so a Join can open to a populated chat instead of a ~10s sync. RAM-only —
// nothing is persisted for a community the user hasn't joined, so a declined invite leaves no DB
// trace. Generation-stamped (cleared on account swap) + TTL'd + capped.

/// How long a warmed page stays promotable. Past this, Join falls back to a normal sync.
const PRELOAD_TTL: Duration = Duration::from_secs(120);
/// Max communities warmed at once (bounds memory; oldest evicted on overflow).
const PRELOAD_MAX: usize = 8;

/// How long the sync will adopt an in-flight (Pending) preload before giving up and fetching itself.
/// Generous: the preload fetch is itself relay-racing, so adopting it is never slower than firing a
/// parallel fetch — and a failed preload aborts (→ absent) so the sync falls back immediately, not
/// at the deadline.
const PRELOAD_ADOPT_TIMEOUT: Duration = Duration::from_secs(12);

enum PreloadState {
    /// A warm-up fetch is in flight. A Join can ADOPT it (await this result) instead of firing its
    /// own — so the speedup holds even when the user taps Join before the warm-up finished.
    Pending,
    /// The warmed page is ready to promote/adopt.
    Ready(Vec<Event>),
}

struct Preload {
    state: PreloadState,
    fetched_at: Instant,
    generation: u64,
}

static PRELOAD: LazyLock<Mutex<HashMap<String, Preload>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn preload_locked() -> MutexGuard<'static, HashMap<String, Preload>> {
    PRELOAD.lock().unwrap_or_else(|e| e.into_inner())
}

/// Mark a community's warm-up as in-flight (so a racing Join adopts it rather than double-fetching).
/// Evicts stale/cross-generation entries and, if over the cap, the oldest.
pub fn begin_preload(community_id: &str) {
    let generation = crate::state::current_session_generation();
    let mut map = preload_locked();
    map.retain(|_, p| p.generation == generation && p.fetched_at.elapsed() < PRELOAD_TTL);
    if map.len() >= PRELOAD_MAX {
        if let Some(oldest) = map.iter().min_by_key(|(_, p)| p.fetched_at).map(|(k, _)| k.clone()) {
            map.remove(&oldest);
        }
    }
    map.insert(
        community_id.to_string(),
        Preload { state: PreloadState::Pending, fetched_at: Instant::now(), generation },
    );
}

/// The warm-up fetch landed — make its page available to promote/adopt.
pub fn finish_preload(community_id: &str, page: Vec<Event>) {
    let generation = crate::state::current_session_generation();
    let mut map = preload_locked();
    if let Some(p) = map.get_mut(community_id) {
        p.state = PreloadState::Ready(page);
        p.fetched_at = Instant::now();
        p.generation = generation;
    }
}

/// The warm-up fetch failed/was cancelled — drop the entry so an adopter falls back immediately.
pub fn abort_preload(community_id: &str) {
    preload_locked().remove(community_id);
}

/// Non-blocking take for promotion at Accept: returns the page ONLY if already Ready, leaving a
/// still-Pending warm-up in place for the sync to adopt. `None` if absent / Pending / stale.
pub fn take_ready_preload(community_id: &str) -> Option<Vec<Event>> {
    let generation = crate::state::current_session_generation();
    let mut map = preload_locked();
    let fresh = matches!(map.get(community_id), Some(p)
        if p.generation == generation
            && p.fetched_at.elapsed() < PRELOAD_TTL
            && matches!(p.state, PreloadState::Ready(_)));
    if !fresh {
        return None;
    }
    match map.remove(community_id) {
        Some(Preload { state: PreloadState::Ready(page), .. }) => Some(page),
        _ => None,
    }
}

/// Adopt a community's warm-up as this sync's page: Ready → take it; Pending → await it (the
/// in-flight fetch IS the page, so this waits only the request's remaining time, never firing a
/// second); absent/stale/failed → `None` so the caller fetches normally. Polls at coarse granularity
/// (imperceptible vs. a fresh round-trip) to stay free of notification races.
pub async fn take_or_await_preload(community_id: &str) -> Option<Vec<Event>> {
    let deadline = Instant::now() + PRELOAD_ADOPT_TIMEOUT;
    loop {
        {
            let generation = crate::state::current_session_generation();
            let mut map = preload_locked();
            match map.get(community_id) {
                Some(p) if p.generation == generation && p.fetched_at.elapsed() < PRELOAD_TTL => {
                    if matches!(p.state, PreloadState::Ready(_)) {
                        return match map.remove(community_id) {
                            Some(Preload { state: PreloadState::Ready(page), .. }) => Some(page),
                            _ => None,
                        };
                    }
                    // Pending → keep waiting.
                }
                _ => return None, // absent / stale / aborted → fetch normally
            }
        }
        if Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Drop all cached state (account swap / reset). Access-time generation checks self-reset too,
/// so this is an explicit belt-and-suspenders teardown.
pub fn clear() {
    *PRELOAD.lock().unwrap_or_else(|e| e.into_inner()) = HashMap::new();
    *CACHE.lock().unwrap_or_else(|e| e.into_inner()) = CommunityCache {
        generation: crate::state::current_session_generation(),
        ..Default::default()
    };
}
