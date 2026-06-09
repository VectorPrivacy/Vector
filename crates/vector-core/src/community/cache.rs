//! Per-account RAM cache for Community sync state.
//!
//! Consolidates what were three scattered `LazyLock` statics (oldest-page cursor, history-start
//! floors, in-flight page de-dup) into ONE structure under ONE invalidation key: the session
//! generation. Any access after an account swap — which bumps `current_session_generation()` —
//! transparently resets the cache, so stale per-channel state can never bleed into the next
//! account. Holds the page cursors (oldest back-paging floor + newest `since` floor), history-start
//! flags, and in-flight page de-dup; future RAM-cache work (e.g. invite preload) layers onto the
//! same structure + invalidation discipline.

use std::collections::{HashMap, HashSet};
use std::sync::{LazyLock, Mutex, MutexGuard};

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

/// Drop all cached state (account swap / reset). Access-time generation checks self-reset too,
/// so this is an explicit belt-and-suspenders teardown.
pub fn clear() {
    *CACHE.lock().unwrap_or_else(|e| e.into_inner()) = CommunityCache {
        generation: crate::state::current_session_generation(),
        ..Default::default()
    };
}
