//! Per-account RAM cache for Community sync state.
//!
//! Holds the page cursors (oldest back-paging floor + newest `since` floor), history-start flags,
//! in-flight page de-dup, and the invite preload. All of it is keyed by this account's channel
//! ids, so it lives on the account's session and goes when that does — there is no invalidation
//! step to get right.

use nostr_sdk::prelude::Event;
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Default)]
struct CommunityCache {
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

struct CacheKey;

/// Run `f` against this account's cache.
///
/// A closure rather than a returned guard, so the lock provably cannot be held
/// across an await: this is pure optimisation state and every use is a lookup
/// or an insert. Poison-tolerant for the same reason — a panicking caller must
/// not cascade into every future community sync.
fn with_cache<R>(f: impl FnOnce(&mut CommunityCache) -> R) -> R {
    let cache = crate::db::current_session().scoped::<CacheKey, Mutex<CommunityCache>>();
    let mut guard = cache.lock().unwrap_or_else(|e| e.into_inner());
    f(&mut guard)
}

/// Claim an in-flight page fetch (key `"{channel_id}:{older|latest}"`). Returns `false` if one is
/// already running — the caller should no-op. Pair with [`end_page_fetch`].
pub fn try_begin_page_fetch(key: &str) -> bool {
    with_cache(|c| c.inflight.insert(key.to_string()))
}

/// Release an in-flight page-fetch claim (success or error).
pub fn end_page_fetch(key: &str) {
    with_cache(|c| c.inflight.remove(key));
}

/// Has the channel's network history-start been reached? Older pages then stay DB-only.
pub fn is_at_history_start(channel_id: &str) -> bool {
    with_cache(|c| c.history_start.contains(channel_id))
}

/// Mark the channel as having reached its network history-start.
pub fn mark_history_start(channel_id: &str) {
    with_cache(|c| c.history_start.insert(channel_id.to_string()));
}

/// Oldest OUTER created_at (seconds) fetched for the channel — the back-paging cursor.
pub fn oldest_cursor(channel_id: &str) -> Option<u64> {
    with_cache(|c| c.oldest_cursor.get(channel_id).copied())
}

/// Advance the back-paging cursor to the oldest wire time this page returned (monotonic — only
/// ever steps further back).
pub fn advance_oldest_cursor(channel_id: &str, oldest_secs: u64) {
    with_cache(|c| {
        let slot = c.oldest_cursor.entry(channel_id.to_string()).or_insert(oldest_secs);
        *slot = (*slot).min(oldest_secs);
    });
}

/// Newest OUTER created_at (seconds) seen on a latest page for the channel — the `since` floor
/// for the next latest fetch. `None` before the first latest fetch this session (→ full newest page).
pub fn newest_cursor(channel_id: &str) -> Option<u64> {
    with_cache(|c| c.newest_cursor.get(channel_id).copied())
}

/// Advance the latest-page `since` floor to the newest wire time this page returned (monotonic —
/// only ever steps forward). Call ONLY for latest-page fetches.
pub fn advance_newest_cursor(channel_id: &str, newest_secs: u64) {
    with_cache(|c| {
        let slot = c.newest_cursor.entry(channel_id.to_string()).or_insert(newest_secs);
        *slot = (*slot).max(newest_secs);
    });
}

/// Clear a channel's back-paging floors (history-start + oldest cursor) — e.g. after a
/// multi-epoch backfill makes older history reachable again.
pub fn clear_channel_floors(channel_id: &str) {
    with_cache(|c| {
        c.history_start.remove(channel_id);
        c.oldest_cursor.remove(channel_id);
    });
}

/// Drop ALL of a channel's sync state (floors + the latest-page `since` cursor) — community
/// teardown. A surviving `since` cursor makes a same-session REJOIN sync "since I left"
/// instead of cold, so the rejoined chat opens empty despite plenty of history.
pub fn clear_channel_sync_state(channel_id: &str) {
    with_cache(|c| {
        c.history_start.remove(channel_id);
        c.oldest_cursor.remove(channel_id);
        c.newest_cursor.remove(channel_id);
    });
}

// ── Invite preload ──────────────────────────────────────────────────────────
// Warmed-ahead-of-Join state: the primary channel's first page, fetched at invite-receive /
// public-preview time so a Join can open to a populated chat instead of a ~10s sync. RAM-only —
// nothing is persisted for a community the user hasn't joined, so a declined invite leaves no DB
// trace. Session-scoped + TTL'd + capped.

/// How long a warmed page stays promotable. Past this, Join falls back to a normal sync.
pub(crate) const PRELOAD_TTL: Duration = Duration::from_secs(120);
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
}

struct PreloadKey;

fn with_preload<R>(f: impl FnOnce(&mut HashMap<String, Preload>) -> R) -> R {
    let map = crate::db::current_session().scoped::<PreloadKey, Mutex<HashMap<String, Preload>>>();
    let mut guard = map.lock().unwrap_or_else(|e| e.into_inner());
    f(&mut guard)
}

/// Mark a community's warm-up as in-flight (so a racing Join adopts it rather than double-fetching).
/// Evicts expired entries and, if over the cap, the oldest.
pub fn begin_preload(community_id: &str) {
    with_preload(|map| {
        map.retain(|_, p| p.fetched_at.elapsed() < PRELOAD_TTL);
        if map.len() >= PRELOAD_MAX {
            if let Some(oldest) = map.iter().min_by_key(|(_, p)| p.fetched_at).map(|(k, _)| k.clone()) {
                map.remove(&oldest);
            }
        }
        map.insert(
            community_id.to_string(),
            Preload { state: PreloadState::Pending, fetched_at: Instant::now() },
        );
    });
}

/// The warm-up fetch landed — make its page available to promote/adopt.
pub fn finish_preload(community_id: &str, page: Vec<Event>) {
    with_preload(|map| {
        if let Some(p) = map.get_mut(community_id) {
            p.state = PreloadState::Ready(page);
            p.fetched_at = Instant::now();
        }
    });
}

/// The warm-up fetch failed/was cancelled — drop the entry so an adopter falls back immediately.
pub fn abort_preload(community_id: &str) {
    with_preload(|map| map.remove(community_id));
}

/// Non-blocking take for promotion at Accept: returns the page ONLY if already Ready, leaving a
/// still-Pending warm-up in place for the sync to adopt. `None` if absent / Pending / stale.
pub fn take_ready_preload(community_id: &str) -> Option<Vec<Event>> {
    with_preload(|map| {
        let fresh = matches!(map.get(community_id), Some(p)
            if p.fetched_at.elapsed() < PRELOAD_TTL && matches!(p.state, PreloadState::Ready(_)));
        if !fresh {
            return None;
        }
        match map.remove(community_id) {
            Some(Preload { state: PreloadState::Ready(page), .. }) => Some(page),
            _ => None,
        }
    })
}

/// Adopt a community's warm-up as this sync's page: Ready → take it; Pending → await it (the
/// in-flight fetch IS the page, so this waits only the request's remaining time, never firing a
/// second); absent/stale/failed → `None` so the caller fetches normally. Polls at coarse granularity
/// (imperceptible vs. a fresh round-trip) to stay free of notification races.
pub async fn take_or_await_preload(community_id: &str) -> Option<Vec<Event>> {
    let deadline = Instant::now() + PRELOAD_ADOPT_TIMEOUT;
    loop {
        let adopted = with_preload(|map| match map.get(community_id) {
            Some(p) if p.fetched_at.elapsed() < PRELOAD_TTL => {
                if matches!(p.state, PreloadState::Ready(_)) {
                    return match map.remove(community_id) {
                        Some(Preload { state: PreloadState::Ready(page), .. }) => Some(Some(page)),
                        _ => Some(None),
                    };
                }
                None // Pending → keep waiting.
            }
            _ => Some(None), // absent / stale / aborted → fetch normally
        });
        if let Some(outcome) = adopted {
            return outcome;
        }
        if Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Drop all cached state. A swap needs no call: this belongs to the account's
/// session and goes with it. Kept for callers that want a cold cache within one
/// account (tests, an explicit resync).
pub fn clear() {
    with_preload(|map| map.clear());
    with_cache(|c| *c = CommunityCache::default());
}
