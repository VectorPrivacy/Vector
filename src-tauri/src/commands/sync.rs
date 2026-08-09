//! Synchronization Tauri commands.
//!
//! This module handles:
//! - Message fetching via NIP-77 negentropy set reconciliation (fetch_messages)
//! - Profile synchronization
//! - Sync status checking

use futures_util::StreamExt;
use nostr_sdk::prelude::*;
use tauri::{AppHandle, Emitter, Manager, Runtime};

use crate::{
    db, profile, profile_sync,
    Profile,
    nostr_client, STATE, WRAPPER_ID_CACHE,
};

/// Committed sync messages buffer up to this many before a batched-transaction flush
/// (see `BatchingPersist`) — bounds the STATE-visible-but-unpersisted window while keeping
/// the per-commit transaction overhead amortized.
const PERSIST_BATCH: usize = 100;

/// NIP-59 backdates gift-wrap `created_at` up to 2 days; any "since newest held
/// wrap" window must include that slack or backdated wraps slip past it.
const NIP59_BACKDATE_SLACK: u64 = 2 * 24 * 3600;

/// Hard cap for windowed REQ catch-ups (reconcile fallback + no-NIP-77 relays).
const FALLBACK_LIMIT: usize = 256;

// ============================================================================
// Profile Sync Commands
// ============================================================================

/// Queue a profile for synchronization with specified priority.
#[tauri::command]
pub async fn queue_profile_sync(npub: String, priority: String, force_refresh: bool) -> Result<(), String> {
    let sync_priority = match priority.as_str() {
        "critical" => profile_sync::SyncPriority::Critical,
        "high" => profile_sync::SyncPriority::High,
        "medium" => profile_sync::SyncPriority::Medium,
        "low" => profile_sync::SyncPriority::Low,
        _ => return Err(format!("Invalid priority: {}", priority)),
    };

    profile_sync::queue_profile_sync(npub, sync_priority, force_refresh);
    Ok(())
}

/// Queue all profiles in a chat for synchronization.
#[tauri::command]
pub async fn queue_chat_profiles_sync(chat_id: String, is_opening: bool) -> Result<(), String> {
    profile_sync::queue_chat_profiles(chat_id, is_opening).await;
    Ok(())
}

/// Immediately refresh a specific profile.
#[tauri::command]
pub async fn refresh_profile_now(npub: String) -> Result<(), String> {
    profile_sync::refresh_profile_now(npub);
    Ok(())
}

/// Sync all known profiles.
#[tauri::command]
pub async fn sync_all_profiles() -> Result<(), String> {
    profile_sync::sync_all_profiles().await;
    Ok(())
}

/// Check if a sync/scan operation is currently in progress
#[tauri::command]
pub async fn is_scanning() -> bool {
    let state = STATE.lock().await;
    state.is_syncing
}

// ============================================================================
// Message Sync Commands
// ============================================================================

/// One bounded retry for AUTH-gating relays (Ditto and kin): the FIRST gated
/// request per connection is rejected `auth-required` while the relay issues
/// its NIP-42 challenge; the client's authenticator answers it in the
/// background, so a short-delay retry succeeds. Anything else (including a
/// second auth refusal) propagates unchanged.
async fn with_auth_retry<T, E, F, Fut>(mut op: F) -> Result<T, E>
where
    E: std::fmt::Display,
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, E>>,
{
    match op().await {
        Err(e) if e.to_string().contains("auth-required") => {
            tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
            op().await
        }
        other => other,
    }
}

/// Windowed incremental REQ catch-up — the sync path for relays that can't
/// negentropy, and the recovery path when no relay reconciles. Same
/// prepare → commit pipeline as the reconcile fetches, bounded by `since` and
/// [`FALLBACK_LIMIT`]. Returns (events seen, new messages).
async fn windowed_req_catchup(
    client: &Client,
    relay_urls: Vec<String>,
    my_public_key: PublicKey,
    since_secs: u64,
    is_new: bool,
) -> (u32, u32) {
    let filter = Filter::new()
        .pubkey(my_public_key)
        .kind(Kind::GiftWrap)
        .since(Timestamp::from_secs(since_secs))
        .limit(FALLBACK_LIMIT);
    let inner = crate::services::event_handler::TauriEventHandler;
    let batcher = vector_core::event_handler::BatchingPersist::new(&inner);
    let mut seen: std::collections::HashSet<[u8; 32]> = std::collections::HashSet::new();
    let (mut fetched, mut new_count) = (0u32, 0u32);

    match client.stream_events(nostr_sdk::prelude::ReqTarget::manual(
        relay_urls.into_iter().map(|u| (u, vec![filter.clone()])),
    ))
    .timeout(vector_core::relay_request_timeout(std::time::Duration::from_secs(20)))
    .await {
        Ok(stream) => {
            tokio::pin!(stream);
            while let Some((_relay, res)) = stream.next().await {
                let Ok(event) = res else { continue };
                if !seen.insert(event.id.to_bytes()) { continue; }
                fetched += 1;
                let prepared = vector_core::event_handler::prepare_event(
                    event, client, my_public_key,
                ).await;
                if crate::services::tauri_commit_prepared_event_with(prepared, is_new, &batcher).await {
                    new_count += 1;
                }
                if batcher.buffered() >= PERSIST_BATCH {
                    batcher.flush().await;
                }
            }
            batcher.flush().await;
        }
        Err(e) => eprintln!("[Sync] REQ catch-up failed: {}", e),
    }
    (fetched, new_count)
}

/// Fetch one relay's full since-window to GENUINE EOSE, paging past the
/// relay's limit cap. Returns `Some((events, complete))` — `complete` only
/// when the final page came back short with a real EOSE, the sole proof a
/// cursor may advance on. `None` = the first page failed even after the
/// AUTH-gate retry (gating relays CLOSE the first-touch REQ; the SDK answers
/// the challenge in the background, so one delayed retry succeeds).
async fn fetch_relay_window(
    client: &Client,
    url: &str,
    my_pk: PublicKey,
    since_secs: u64,
    page_cap: usize,
    budget: std::time::Duration,
) -> Option<(Vec<Event>, bool)> {
    const MAX_PAGES: usize = 4;
    let mut out: Vec<Event> = Vec::new();
    let mut seen: std::collections::HashSet<EventId> = std::collections::HashSet::new();
    let mut until: Option<u64> = None;
    for page in 0..MAX_PAGES {
        let mut f = Filter::new()
            .pubkey(my_pk)
            .kind(Kind::GiftWrap)
            .since(Timestamp::from_secs(since_secs))
            .limit(page_cap);
        if let Some(u) = until {
            f = f.until(Timestamp::from_secs(u));
        }
        use vector_core::community::transport::EoseFail;
        let evs = match vector_core::community::transport::fetch_relay_eose_filters(
            client, url, vec![f.clone()], budget,
        )
        .await
        {
            Ok(evs) => evs,
            // Retry ONLY the AUTH-gate CLOSED (the SDK answers the challenge
            // in the background; one delayed retry passes). A burned deadline
            // retried blind would double a dead relay's cost.
            Err(EoseFail::Closed) if page == 0 => {
                tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
                match vector_core::community::transport::fetch_relay_eose_filters(
                    client, url, vec![f], budget,
                )
                .await
                {
                    Ok(evs) => evs,
                    Err(_) => return None,
                }
            }
            Err(_) if page == 0 => return None,
            // Mid-drain failure: keep what EOSE'd, but it is NOT complete.
            Err(_) => return Some((out, false)),
        };
        let fresh = evs.iter().filter(|e| !seen.contains(&e.id)).count();
        let short = evs.len() < page_cap;
        let oldest = evs.iter().map(|e| e.created_at.as_secs()).min();
        for e in evs {
            if seen.insert(e.id) {
                out.push(e);
            }
        }
        if short {
            // A short page proves nothing when the relay's ENFORCED limit is
            // below our cap (it truncates and honestly EOSEs). One tiny
            // confirming REQ below the oldest event must come back empty
            // before "complete" — the proof cursors advance on.
            if out.is_empty() {
                return Some((out, true));
            }
            let Some(oldest_all) = out.iter().map(|e| e.created_at.as_secs()).min() else {
                return Some((out, true));
            };
            if oldest_all <= since_secs {
                return Some((out, true));
            }
            let confirm = Filter::new()
                .pubkey(my_pk)
                .kind(Kind::GiftWrap)
                .since(Timestamp::from_secs(since_secs))
                .until(Timestamp::from_secs(oldest_all - 1))
                .limit(1);
            return match vector_core::community::transport::fetch_relay_eose_filters(
                client, url, vec![confirm], budget,
            )
            .await
            {
                Ok(rest) if rest.is_empty() => Some((out, true)),
                Ok(rest) => {
                    // Truncating relay: keep walking from below our floor.
                    for e in rest {
                        if seen.insert(e.id) {
                            out.push(e);
                        }
                    }
                    until = Some(oldest_all - 1);
                    continue;
                }
                Err(_) => Some((out, false)),
            };
        }
        // Full page: walk older. Inclusive boundary — dedup absorbs overlap;
        // a full page of nothing-fresh is a same-second wall we step past.
        match oldest {
            Some(o) => until = Some(if fresh == 0 { o.saturating_sub(1) } else { o }),
            None => return Some((out, true)),
        }
    }
    Some((out, false))
}

/// Free the sync-window dedup cache. EVERY completion path must call this —
/// a skipped dump leaves the cache resident until the next boot.
async fn dump_wrapper_cache() {
    let mut cache = WRAPPER_ID_CACHE.lock().await;
    let cache_size = cache.len();
    cache.clear();
    println!("[Sync] Dumped wrapper cache (~{} KB freed)", (cache_size * 35) / 1024);
}

/// Fetch messages from relays and sync to local state
///
/// Uses NIP-77 negentropy set reconciliation:
/// - Quick phase: 2-day window for near-instant recent messages
/// - Archive phase: full reconciliation in background
/// - Single-relay reconnection sync
#[tauri::command]
pub async fn fetch_messages<R: Runtime>(
    handle: AppHandle<R>,
    init: bool,
    relay_url: Option<String>
) {
    vector_core::db::scoped(async move {
        println!("[Boot] fetch_messages called (init={}, relay={:?})", init, relay_url);
        // Return type is `()` — silently early-exit on no-session.
        let Some(client) = nostr_client() else {
            eprintln!("[Boot] fetch_messages aborted: no active session");
            return;
        };
        let Some(my_public_key) = crate::my_public_key() else {
            eprintln!("[Boot] fetch_messages aborted: no public key");
            return;
        };

        // One-time (per-account) migration of legacy app-private downloads into the
        // public "Vector" media dir. Runs here — after account selection so the DB
        // is the active account's, before the frontend loads messages for display.
        // Blocking file I/O (potentially thousands of files) goes on a blocking
        // thread; awaited so display gates on it. It emits "download_migration"
        // progress events for the boot overlay.
        #[cfg(target_os = "android")]
        {
            let _ = tokio::task::spawn_blocking(crate::android::storage::migrate_old_downloads).await;
        }

        // Single-relay reconnection sync — uses negentropy just like the main sync
        if let Some(url) = relay_url {
            let recon_start = std::time::Instant::now();

            // Look up the Relay object for this URL
            let relay_map = client.relays().await;
            let relay = relay_map.iter()
                .find(|(u, _)| u.to_string() == url)
                .map(|(_, r)| r.clone());
            drop(relay_map);

            let Some(relay) = relay else {
                eprintln!("[Sync] Single-relay sync: relay {} not found in pool", url);
                return;
            };

            // Pin to the session whose items/pubkey drive this reconcile — captured BEFORE the
            // reconcile so a swap during it invalidates the whole fetch+commit pipeline.
            let recon_session = vector_core::db::current_session();

            // A fresh no-NIP-77 verdict means reconciling is doomed — catch up with
            // a windowed REQ so the reconnect still recovers missed messages.
            if vector_core::negentropy::neg_supported_cached(&url) == Some(false) {
                let since = Timestamp::now().as_secs().saturating_sub(NIP59_BACKDATE_SLACK);
                let (fetched, new) = windowed_req_catchup(
                    &client, vec![url.clone()], my_public_key, since, true,
                ).await;
                println!("[Sync] Single-relay {} REQ catch-up (no NIP-77): {} events, {} new",
                    url, fetched, new);
                return;
            }

            // Load negentropy items — 2-day window for fast reconnection sync,
            // stretched back to the relay's cursor after a longer outage.
            let all_items = db::load_negentropy_items().unwrap_or_default();
            let recon_anchor = Timestamp::now().as_secs();
            let cursor = vector_core::negentropy::reconcile_cursor(&url);
            let quick_since = {
                let base = recon_anchor.saturating_sub(2 * 24 * 3600);
                // Cursored: (cursor − slack) alone — see the boot quick phase.
                cursor
                    .map(|c| c.saturating_sub(NIP59_BACKDATE_SLACK))
                    .unwrap_or(base)
            };
            let items: Vec<(EventId, Timestamp)> = all_items.iter()
                .filter(|(_, ts)| ts.as_secs() >= quick_since)
                .cloned()
                .collect();
            let filter = Filter::new()
                .pubkey(my_public_key)
                .kind(Kind::GiftWrap)
                .since(Timestamp::from_secs(quick_since));
            // Tor-aware: 3s is fine for a clearnet fingerprint round trip and always
            // expires mid-circuit. A failure here `return`s, silently skipping this
            // relay's entire reconnect catch-up, so a too-tight budget loses messages.
            let neg_budget = vector_core::relay_request_timeout(std::time::Duration::from_secs(3));
            let sync_opts = nostr_sdk::prelude::SyncOptions::new()
                .direction(nostr_sdk::prelude::SyncDirection::Down)
                .initial_timeout(neg_budget)
                .dry_run();

            let recon_result = tokio::time::timeout(
                // Keeps the original 7s of slack over the inner budget, so the inner
                // error surfaces instead of this blunt outer timeout.
                neg_budget + std::time::Duration::from_secs(7),
                with_auth_retry(|| async {
                    relay.sync(filter.clone()).items(items.clone()).opts(sync_opts.clone()).await
                }),
            ).await;

            let missing_ids: Vec<EventId> = match recon_result {
                Ok(Ok(recon)) => {
                    let ids: Vec<EventId> = recon.remote.into_iter().collect();
                    println!("[Sync] Single-relay {} reconciled in {:?}: {} missing",
                        url, recon_start.elapsed(), ids.len());
                    vector_core::negentropy::record_neg_support(&url, true);
                    if ids.is_empty() && cursor.is_some() {
                        vector_core::negentropy::advance_reconcile_cursor(&url, recon_anchor);
                    }
                    ids
                }
                Ok(Err(e)) => {
                    eprintln!("[Sync] Single-relay {} negentropy failed: {}", url, e);
                    // Deterministic refusals ONLY: this path runs a 3s budget over
                    // a stretched item set — a connected timeout here says the
                    // budget was small, not that the relay lacks NIP-77.
                    if recon_session.is_live()
                        && vector_core::negentropy::classify_neg_sync_error(&e.to_string(), false)
                            == Some(false)
                    {
                        println!("[Sync] {} marked no-NIP-77 for 24h", url);
                        vector_core::negentropy::record_neg_support(&url, false);
                    }
                    // A reconnect catch-up must never end empty-handed: whatever
                    // felled the reconcile, the windowed REQ still recovers the
                    // disconnect gap.
                    let since = Timestamp::now().as_secs().saturating_sub(NIP59_BACKDATE_SLACK);
                    let (fetched, new) = windowed_req_catchup(
                        &client, vec![url.clone()], my_public_key, since, true,
                    ).await;
                    println!("[Sync] Single-relay {} REQ catch-up: {} events, {} new",
                        url, fetched, new);
                    return;
                }
                Err(_) => {
                    eprintln!("[Sync] Single-relay {} negentropy timed out after {:?}", url, neg_budget);
                    let since = Timestamp::now().as_secs().saturating_sub(NIP59_BACKDATE_SLACK);
                    let (fetched, new) = windowed_req_catchup(
                        &client, vec![url.clone()], my_public_key, since, true,
                    ).await;
                    println!("[Sync] Single-relay {} REQ catch-up: {} events, {} new",
                        url, fetched, new);
                    return;
                }
            };

            // Fetch + process missing events
            if !missing_ids.is_empty() {
                let recon_inner = crate::services::event_handler::TauriEventHandler;
                let recon_batcher = vector_core::event_handler::BatchingPersist::new(&recon_inner);
                const BATCH_SIZE: usize = 500;
                for batch in missing_ids.chunks(BATCH_SIZE) {
                    // Boot sync is minutes of relay traffic and decryption.
                    // A swap means nobody is waiting for it.
                    if vector_core::db::session_stopped() {
                        break;
                    }
                    let f = Filter::new().ids(batch.to_vec()).kind(Kind::GiftWrap).pubkey(my_public_key);
                    match client.stream_events(nostr_sdk::prelude::ReqTarget::manual(vec![url.clone()].into_iter().map(|u| (u, vec![f.clone()]))))
                    .timeout(std::time::Duration::from_secs(30),
                    ).await {
                        Ok(stream) => {
                            let client_clone = client.clone();
                            let prepared_stream = stream
                                .filter_map(|(_relay, res)| async move { res.ok() })
                                .map(move |event| {
                                    let c = client_clone.clone();
                                    vector_core::db::spawn_bound(async move {
                                        vector_core::event_handler::prepare_event(event, &c, my_public_key).await
                                    })
                                })
                                .buffer_unordered(8);
                            tokio::pin!(prepared_stream);
                            while let Some(result) = prepared_stream.next().await {
                                if vector_core::db::session_stopped() {
                                    break;
                                }
                                if let Ok(prepared) = result {
                                    // `is_new: true` — this is the mid-session reconnect catch-up
                                    // (gated on `!is_syncing`, never the initial sync), so these
                                    // arrived while we were disconnected and are new to the user.
                                    // Committing them as not-new makes notifications and the unread
                                    // badge depend on whether this sync or the live subscription won
                                    // the race for a given message. `DedupSkip` on the wrapper cache
                                    // stops the loser from notifying twice.
                                    crate::services::tauri_commit_prepared_event_with(prepared, true, &recon_batcher).await;
                                    if recon_batcher.buffered() >= PERSIST_BATCH {
                                        recon_batcher.flush().await;
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            eprintln!("[Sync] Single-relay {} fetch error: {}", url, e);
                        }
                    }
                }
                recon_batcher.flush().await;
            }

            return;
        }

        // Negentropy-based sync: single-pass reconciliation replaces windowed scanning
        // Only the init=true path does a full sync; init=false (frontend continuation) is a no-op
        if !init {
            return;
        }

        {
            let boot_start = std::time::Instant::now();
            let mut state = STATE.lock().await;
            println!("[Boot] STATE.lock acquired in {:?}", boot_start.elapsed());

            {
                // Set current account for SQL mode if profile database exists
                // This must be done BEFORE loading chats/messages so SQL mode is active
                let npub = my_public_key.to_bech32().unwrap();

                let app_data = handle.path().app_data_dir().ok();
                if let Some(data_dir) = app_data {
                    let profile_db = data_dir.join(&npub).join("vector.db");
                    if profile_db.exists() {
                        let _ = crate::account_manager::set_current_account(npub.clone());
                        let _ = vector_core::db::init_database(&npub);
                        println!("[Startup] Set current account for SQL mode: {}", npub);
                        // Dial community relay sockets NOW, overlapping the DB
                        // load below — by volley time they're up and the gating
                        // relays' challenges are already remembered.
                        vector_core::db::spawn_bound(async move {
                            vector_core::community::transport::prewarm_held_communities().await;
                        });
                    }
                }

                // Load our DB (if we haven't already)
                if !state.db_loaded {
                    // Load profiles, chats, and last messages in parallel (all are independent reads)
                    let db_start = std::time::Instant::now();
                    let (profiles_result, slim_chats_result, last_messages_result) = tokio::join!(
                        async {
                            let t = std::time::Instant::now();
                            let r = db::get_all_profiles().await;
                            println!("[Boot]   get_all_profiles: {:?}", t.elapsed());
                            r
                        },
                        async {
                            let t = std::time::Instant::now();
                            let r = db::get_all_chats().await;
                            println!("[Boot]   get_all_chats: {:?}", t.elapsed());
                            r
                        },
                        async {
                            let t = std::time::Instant::now();
                            let r = db::get_all_chats_last_messages().await;
                            println!("[Boot]   get_all_chats_last_messages: {:?}", t.elapsed());
                            r
                        }
                    );
                    println!("[Boot] Parallel DB load in {:?}", db_start.elapsed());

                    // Process profiles
                    let merge_start = std::time::Instant::now();
                    if let Ok(profiles) = profiles_result {
                        state.merge_db_profiles(profiles, &npub);
                    }
                    println!("[Boot] Profile merge in {:?}", merge_start.elapsed());

                    // Spawn background task to cache profile images for offline support
                    vector_core::db::spawn_bound(async move {
                        profile::cache_all_profile_images().await;
                    });

                    // Get the last messages map (single batch query result)
                    let mut last_messages_map = last_messages_result.unwrap_or_default();

                    // Process chats
                    if let Ok(slim_chats) = slim_chats_result {
                        // Build HashSet of existing profile handles for O(1) lookup
                        let mut known_profiles: std::collections::HashSet<u16> =
                            state.profiles.iter().map(|p| p.id).collect();

                        // Pre-allocate capacity for chats (avoids reallocations during push)
                        state.chats.reserve(slim_chats.len());

                        // Convert slim chats to full chats and merge last messages
                        #[cfg(debug_assertions)]
                        let start = std::time::Instant::now();
                        #[cfg(debug_assertions)]
                        let mut total_messages = 0usize;

                        for slim_chat in slim_chats {
                            let mut chat = slim_chat.to_chat(&mut state.interner);
                            let chat_id = chat.id().to_string();

                            // Ensure profiles exist for all chat participants (O(1) lookup)
                            for &handle in chat.participants() {
                                if !known_profiles.contains(&handle) {
                                    if let Some(npub) = state.interner.resolve(handle).map(|s| s.to_string()) {
                                        let profile = Profile::new();
                                        state.insert_or_replace_profile(&npub, profile);
                                        known_profiles.insert(handle);
                                    }
                                }
                            }

                            // Get messages to add (if any)
                            let messages_to_add = last_messages_map.remove(&chat_id);

                            // Check if this chat already exists in STATE (e.g. created by concurrent event processing)
                            let existing_idx = state.chats.iter().position(|c| c.id == chat_id);

                            if let Some(idx) = existing_idx {
                                // Merge DB-loaded messages into the existing chat
                                if let Some(messages) = messages_to_add {
                                    #[cfg(debug_assertions)]
                                    { total_messages += messages.len(); }
                                    // Deref MutexGuard for split field borrow
                                    let s = &mut *state;
                                    for message in messages {
                                        s.chats[idx].internal_add_message(message, &mut s.interner);
                                    }
                                }
                            } else {
                                // New chat — add messages then push
                                if let Some(messages) = messages_to_add {
                                    #[cfg(debug_assertions)]
                                    { total_messages += messages.len(); }
                                    for message in messages {
                                        chat.internal_add_message(message, &mut state.interner);
                                    }
                                }
                                state.chats.push(chat);
                            }
                        }

                        // Sort chats by last message time (do once at the end, not per-chat)
                        state.chats.sort_by(|a, b| b.last_message_time().cmp(&a.last_message_time()));

                        // Record startup load timing (debug builds only)
                        #[cfg(debug_assertions)]
                        {
                            let elapsed = start.elapsed();
                            if total_messages > 0 {
                                state.cache_stats.insert_count = total_messages as u64;
                                state.cache_stats.record_insert(elapsed);
                            }
                            let chats_clone = state.chats.clone();
                            state.cache_stats.update_from_chats(&chats_clone);
                            println!("[CacheStats] startup load: {} chats, {} msgs in {:?}", state.chats.len(), total_messages, elapsed);
                            state.cache_stats.log();
                        }
                    } else {
                        eprintln!("Failed to load chats from database: {:?}", slim_chats_result);
                    }

                    state.db_loaded = true;

                    // Orphan REPORT: a Community chat row whose communities row is GONE (partial
                    // teardown from older builds) renders as a ghost — every community command
                    // starts at load_community and errors "not found".
                    //
                    // Deliberately reports and never deletes. `delete_chat` drops the row AND its
                    // events, so any wrong verdict here is unrecoverable message loss, while the
                    // condition it fixes is a cosmetic row from teardown paths that now clean up
                    // properly. A permanent risk of that size does not buy a vanishing defect;
                    // if this ever fires, the log is the signal to fix the source (or to offer a
                    // user-initiated removal, which can't be triggered by a boot-time misread).
                    {
                        let mut held = std::collections::HashSet::new();
                        let mut trustworthy = true;
                        match vector_core::db::community::list_community_ids() {
                            Ok(ids) => {
                                for id in ids {
                                    match vector_core::db::community::load_community(&id) {
                                        // Ok(None) is a legitimate "not held" — only Err is unsafe.
                                        Ok(Some(c)) => held.extend(c.channels.iter().map(|ch| ch.id.to_hex())),
                                        Ok(None) => {}
                                        Err(e) => {
                                            eprintln!("[Boot] orphan sweep: community read failed ({e}) — skipping the sweep");
                                            trustworthy = false;
                                            break;
                                        }
                                    }
                                }
                            }
                            Err(e) => {
                                eprintln!("[Boot] orphan sweep: community list failed ({e}) — skipping the sweep");
                                trustworthy = false;
                            }
                        }
                        let orphans: Vec<String> = if trustworthy {
                            state
                                .chats
                                .iter()
                                .filter(|c| matches!(c.chat_type, vector_core::chat::ChatType::Community) && !held.contains(&c.id))
                                .map(|c| c.id.clone())
                                .collect()
                        } else {
                            Vec::new()
                        };
                        if !orphans.is_empty() {
                            // Truncated ids only — a full channel id plus its community membership
                            // is exactly the correlation the planes exist to hide.
                            let ids: Vec<&str> = orphans.iter().map(|id| &id[..id.len().min(8)]).collect();
                            vector_core::log_warn!(
                                "[Boot] {} community chat row(s) have no communities row: {:?} — reported, NOT removed",
                                orphans.len(),
                                ids
                            );
                        }
                    }

                    // Check filesystem integrity for downloaded attachments (queries DB directly), then
                    // reconcile any missing files against in-memory STATE + the frontend — boot preloads
                    // messages before this runs, so a missing file on a preloaded message would otherwise
                    // stay a broken image until a full reload.
                    vector_core::db::spawn_bound(async move {
                        match db::check_downloaded_attachments_integrity().await {
                            Ok((_, missing, _, affected)) if missing > 0 => {
                                crate::commands::attachments::reconcile_missing_attachments_in_state(&affected).await;
                            }
                            Ok(_) => {}
                            Err(e) => eprintln!("[Integrity] Check failed: {}", e),
                        }
                    });

                    // Preload ID caches (fast, needed for serialization)
                    let cache_start = std::time::Instant::now();
                    if let Err(e) = db::preload_id_caches().await {
                        eprintln!("[Cache] Failed to preload ID caches: {}", e);
                    }
                    println!("[Boot] preload_id_caches in {:?}", cache_start.elapsed());

                    // Send the state to frontend (convert to serializable formats at boundary)
                    let serialize_start = std::time::Instant::now();
                    let serializable_chats: Vec<_> = state.chats.iter()
                        .map(|c| c.to_serializable(&state.interner))
                        .collect();
                    let slim_profiles: Vec<db::SlimProfile> = state.profiles.iter()
                        .map(|p| db::SlimProfile::from_profile(p, &state.interner))
                        .collect();
                    println!("[Boot] Serialization in {:?}", serialize_start.elapsed());

                    #[derive(serde::Serialize)]
                    struct InitPayload<'a> {
                        profiles: &'a [db::SlimProfile],
                        chats: &'a [crate::chat::SerializableChat],
                    }

                    let emit_start = std::time::Instant::now();
                    // A failed emit must not panic the boot task — the sync below still has to run.
                    if let Err(e) = handle.emit("init_finished", &InitPayload {
                        profiles: &slim_profiles,
                        chats: &serializable_chats,
                    }) {
                        eprintln!("[Boot] init_finished emit failed: {e}");
                    }
                    println!("[Boot] Event emit in {:?}", emit_start.elapsed());
                    println!("[Boot] Total init time: {:?}", boot_start.elapsed());
                }

                // Preload marketplace cache from SQLite → MARKETPLACE_STATE (non-blocking)
                // Ensures permission checks work before the user visits the Nexus tab,
                // then silently refreshes from the network in the background.
                vector_core::db::spawn_bound(async {
                    crate::miniapps::marketplace::preload_marketplace_cache().await;
                });

                // Preload wrapper IDs for sync deduplication (non-blocking).
                // Bounded to what the planned reconciles can touch: cursored relays
                // reconcile only from (cursor − slack), so the full-history load is
                // justified solely by a bootstrap candidate (NEG-capable relay with
                // no cursor). Older wraps that still arrive dedup through the DB
                // fallback in handle_event.
                let wrapper_client = client.clone();
                vector_core::db::spawn_bound(async move {
                    let t = std::time::Instant::now();
                    let mut full_load = false;
                    let mut bound = Timestamp::now().as_secs().saturating_sub(7 * 24 * 3600);
                    for url in wrapper_client.relays().await.keys() {
                        let u = url.as_str();
                        // No-NIP-77 relays only ever get windowed REQs — they can
                        // neither bootstrap nor widen the bound.
                        if vector_core::negentropy::neg_supported_cached(u) == Some(false) {
                            continue;
                        }
                        match vector_core::negentropy::reconcile_cursor(u) {
                            Some(c) => bound = bound.min(c.saturating_sub(NIP59_BACKDATE_SLACK)),
                            None => {
                                full_load = true;
                                break;
                            }
                        }
                    }
                    let event_wrappers = db::load_recent_wrapper_ids(30).await.unwrap_or_default();
                    let processed_wrappers = if full_load {
                        db::load_processed_wrappers().unwrap_or_default()
                    } else {
                        db::load_processed_wrappers_since(bound).unwrap_or_default()
                    };
                    // Re-validate after the DB reads — a swap mid-boot must not repopulate the
                    // just-cleared cache with the prior account's wrapper ids.
                    // Fast cursor boots can finish before this load completes;
                    // populating then would leave the cache resident with nobody
                    // left to dump it.
                    if !STATE.lock().await.is_syncing { return; }
                    let mut cache = WRAPPER_ID_CACHE.lock().await;
                    let total = event_wrappers.len() + processed_wrappers.len();
                    cache.load(event_wrappers);
                    for w in processed_wrappers {
                        cache.insert(w);
                    }
                    println!(
                        "[Sync] wrapper_id cache loaded: {} entries{} ({:?})",
                        total,
                        if full_load { " (full — bootstrap pending)" } else { "" },
                        t.elapsed()
                    );
                });

                state.is_syncing = true;
            }
        } // STATE lock released — no lock held during network operations

        // Community boot sweep — initiated HERE in Rust (not from JS) so it runs CONCURRENTLY with the DM
        // negentropy phase below, which routinely takes 10s+. Communities must not wait on it. Detached:
        // the sweep windows itself (3 in flight) and emits message_new as pages land. init_finished was
        // already emitted above, so the frontend holds the Community chat rows before any page arrives.
        // std::sync::Arc<crate::db::Session> captured before the spawn boundary (swap-safe); the sweep re-captures internally.
        vector_core::db::spawn_bound(async move {
            let _ = crate::commands::community::sync_communities_boot().await;
        });

        // ========================================================================
        // Negentropy (NIP-77) DM quick phase
        // ========================================================================

        let sync_start = std::time::Instant::now();

        // Connection gate for sync attempts: an unreachable relay costs this, not
        // a full fetch budget. Tor floors it at the request floor (there the
        // connection genuinely IS the slow part).
        let connect_allowance = vector_core::relay_request_timeout(std::time::Duration::from_secs(3))
            .min(std::time::Duration::from_secs(15));
        let _dm_new_messages = async {
        let mut new_messages_count: u32 = 0;

        // Pins the quick phase's batched persists to the session that started them — a swap
        // mid-drain drops the unflushed buffer instead of writing it into the next account.
        let quick_session = vector_core::db::current_session();

        // ── EOSE quick sync ──────────────────────────────────────────────────
        // One since-bounded REQ per relay, all concurrent, read to GENUINE EOSE
        // via `fetch_relay_eose` — the plain fetch stack returns Ok(collected) on
        // timeout/CLOSED alike, which would forge the completeness proof cursors
        // advance on. No fingerprint exchange — with per-relay cursors the window
        // is small enough that re-sending the NIP-59 overlap costs less than a
        // negentropy round trip. Negentropy remains the archive bootstrap's tool.
        let quick_since = Timestamp::now().as_secs().saturating_sub(7 * 24 * 3600);
        let sync_anchor = Timestamp::now().as_secs();
        // A full page may mean relay-cap truncation — no EOSE-completeness claim,
        // so no cursor advance; the deep pass owns anything beyond it.
        const QUICK_PAGE_CAP: usize = 500;
        let quick_budget = vector_core::relay_request_timeout(std::time::Duration::from_secs(10));

        let _ = handle.emit("sync_progress", serde_json::json!({ "mode": "Reconciling" }));

        let relay_map = client.relays().await;
        let mut relay_futs = futures_util::stream::FuturesUnordered::new();
        for (url, relay) in relay_map.iter() {
            // READ-flagged relays ONLY (has_read, not can_read): the prewarmed
            // community/discovery relays are GOSSIP-flagged and must never see a
            // REQ naming the user's identity pubkey — v2 planes are pseudonymous
            // precisely so a community relay can't link them to an npub. Also
            // keeps an empty gossip relay's instant EOSE from starting the
            // straggler grace or masking sync_unreachable.
            if !relay.capabilities().has_read() {
                continue;
            }
            let url = url.clone();
            let relay = relay.clone();
            let c = client.clone();
            let cursor = vector_core::negentropy::reconcile_cursor(url.as_str());
            let had_cursor = cursor.is_some();
            // Proven-through-cursor: the window covers (cursor − slack) → now and
            // nothing more. A stale cursor stretches it on its own; a 7d floor
            // here would permanently re-download a week of wraps per relay per
            // boot (fingerprints were free — EOSE-REQ pays in full events). The
            // floor belongs to CURSOR-LESS relays only.
            let since = cursor
                .map(|c| c.saturating_sub(NIP59_BACKDATE_SLACK))
                .unwrap_or(quick_since);
            relay_futs.push(async move {
                if !vector_core::negentropy::wait_connected(&relay, connect_allowance).await {
                    return (url, None, had_cursor);
                }
                let t = std::time::Instant::now();
                match fetch_relay_window(&c, url.as_str(), my_public_key, since, QUICK_PAGE_CAP, quick_budget)
                    .await
                {
                    Some((evs, complete)) => {
                        println!(
                            "[Sync] {} EOSE in {:?}: {} event(s) in window (complete={})",
                            url,
                            t.elapsed(),
                            evs.len(),
                            complete
                        );
                        (url, Some((evs, complete)), had_cursor)
                    }
                    None => {
                        eprintln!("[Sync] {} quick REQ: no EOSE (timeout/closed)", url);
                        (url, None, had_cursor)
                    }
                }
            });
        }
        drop(relay_map);

        let quick_inner = crate::services::event_handler::TauriEventHandler;
        let batcher = vector_core::event_handler::BatchingPersist::new(&quick_inner);
        let mut flushes_ok = true;
        let mut any_eose = false;
        let mut fetched = 0u32;
        let mut seen: std::collections::HashSet<[u8; 32]> = std::collections::HashSet::new();
        // Relays whose EOSE covered the whole window — cursor advance candidates
        // once the final flush lands.
        let mut clean_relays: Vec<RelayUrl> = Vec::new();
        // After the first EOSE, stragglers get a short grace then detach to the
        // background — one slow relay must not hold sync_finished (its events and
        // cursor advance still land, just off the boot path).
        const QUICK_STRAGGLER_GRACE: std::time::Duration = std::time::Duration::from_secs(2);
        let mut first_eose: Option<std::time::Instant> = None;
        loop {
            let next = match first_eose {
                Some(t0) => {
                    let Some(left) = QUICK_STRAGGLER_GRACE.checked_sub(t0.elapsed()) else { break };
                    match tokio::time::timeout(left, relay_futs.next()).await {
                        Ok(n) => n,
                        Err(_) => break,
                    }
                }
                None => relay_futs.next().await,
            };
            let Some((url, result, had_cursor)) = next else { break };
            let Some((events, complete)) = result else { continue };
            any_eose = true;
            first_eose.get_or_insert_with(std::time::Instant::now);
            for event in events {
                if !seen.insert(event.id.to_bytes()) {
                    continue;
                }
                fetched += 1;
                let prepared = vector_core::event_handler::prepare_event(
                    event, &client, my_public_key,
                ).await;
                if crate::services::tauri_commit_prepared_event_with(prepared, false, &batcher).await {
                    new_messages_count += 1;
                }
                if batcher.buffered() >= PERSIST_BATCH {
                    flushes_ok &= batcher.try_flush().await.is_ok();
                }
            }
            // Cursor birth stays archive-only: a windowed pass cannot vouch for
            // history, so only already-cursored relays advance here.
            if complete && had_cursor {
                clean_relays.push(url);
            }
            // Re-anchor the grace: it budgets WIRE wait, and the decrypt time we
            // just spent on this page must not count against the next relay.
            if first_eose.is_some() {
                first_eose = Some(std::time::Instant::now());
            }
        }
        flushes_ok &= batcher.try_flush().await.is_ok();

        // Detached stragglers: same pipeline, own batcher/flush gate, off-path.
        if !relay_futs.is_empty() {
            println!("[Sync] detaching {} quick straggler(s)", relay_futs.len());
            let det_client = client.clone();
            vector_core::db::spawn_bound(async move {
                let inner = crate::services::event_handler::TauriEventHandler;
                let batcher = vector_core::event_handler::BatchingPersist::new(&inner);
                let mut ok = true;
                let mut seen: std::collections::HashSet<[u8; 32]> = std::collections::HashSet::new();
                let mut clean: Vec<RelayUrl> = Vec::new();
                let mut futs = relay_futs;
                while let Some((url, result, had_cursor)) = futs.next().await {
                    let Some((events, complete)) = result else { continue };
                    let mut n = 0u32;
                    for event in events {
                        if !seen.insert(event.id.to_bytes()) {
                            continue;
                        }
                        let prepared = vector_core::event_handler::prepare_event(
                            event, &det_client, my_public_key,
                        ).await;
                        if crate::services::tauri_commit_prepared_event_with(prepared, false, &batcher).await {
                            n += 1;
                        }
                        if batcher.buffered() >= PERSIST_BATCH {
                            ok &= batcher.try_flush().await.is_ok();
                        }
                    }
                    if n > 0 {
                        println!("[Sync][BG] quick straggler {}: {} new", url, n);
                    }
                    if complete && had_cursor {
                        clean.push(url);
                    }
                }
                ok &= batcher.try_flush().await.is_ok();
                if ok {
                    for url in &clean {
                        vector_core::negentropy::advance_reconcile_cursor(url.as_str(), sync_anchor);
                    }
                }
            });
        }

        // EOSE + everything in the window committed = the same proof a
        // zero-missing reconcile gave: relay and ledger agree through the anchor.
        if flushes_ok && quick_session.is_live() {
            for url in &clean_relays {
                vector_core::negentropy::advance_reconcile_cursor(url.as_str(), sync_anchor);
            }
        }

        // Nothing EOSE'd and nothing arrived: the pool is unreachable, not empty —
        // say so, or this is indistinguishable from having no mail.
        if !any_eose && fetched == 0 {
            let _ = handle.emit("sync_unreachable", serde_json::json!({
                "relays": client.relays().await.len(),
            }));
        }

        // Quick phase done — recent messages visible to user
        println!("[Sync] Quick phase: {:.2?}, {} new messages", sync_start.elapsed(), new_messages_count);

        new_messages_count
        }.await;

        // Deferred bootstrap: merge own kind 10063, then probe unknown servers.
        // Runs after Quick Sync so it can't contend for boot-window bandwidth.
        {
            let bg_client = client.clone();
            let session = vector_core::db::current_session();
            vector_core::db::spawn_bound(async move {
                match vector_core::blossom_servers::fetch_and_merge_own_list(&bg_client, my_public_key).await {
                    Ok(0) => {}
                    Ok(n) => vector_core::log_info!("[BlossomServers] Bootstrap merged {} server(s)", n),
                    Err(e) => vector_core::log_warn!("[BlossomServers] Bootstrap fetch failed: {}", e),
                }

                if !session.is_live() { return; }
                // Route through the active client signer (covers both local
                // and bunker accounts).
                let _client = match crate::nostr_client() { Some(c) => c, None => return };
                let signer = match vector_core::signer::active_signer() { Ok(s) => s, Err(_) => return };
                let enabled_servers = vector_core::state::get_blossom_servers();
                match vector_core::blossom::probe_servers_for_octet_stream(
                    signer, enabled_servers,
                ).await {
                    Ok(0) => {}
                    Ok(n) => {
                        vector_core::log_info!("[Blossom Probe] Probed {} unknown server(s)", n);
                        vector_core::traits::emit_event("blossom_capabilities_updated", &());
                    }
                    Err(e) => vector_core::log_warn!("[Blossom Probe] Probe pass failed: {}", e),
                }
            });
        }

        // ========================================================================
        // Archive sync — full negentropy reconciliation (drives sync UI)
        // ========================================================================
        // Quick phase silently populated recent messages. The archive sync now
        // reconciles our full history with all relays using generous timeouts.
        {
            let bg_client = client.clone();
            // Bound to the account that started it. Its writes and its sync-progress
            // events follow that account; the remaining guard below is about not
            // burning relay bandwidth on a sync nobody is waiting for any more.
            let archive_session = vector_core::db::current_session();
            vector_core::db::spawn_bound(async move {
                let archive_start = std::time::Instant::now();
                let mut archive_new = 0u32;

                // The archive is a BOOTSTRAP, not a routine phase: a relay with a
                // reconcile cursor was already covered by the quick pass (whose
                // window stretches back to that cursor), so only cursor-less
                // relays ever justify a full-history reconcile. When none exist,
                // sync completes right here — no gate wait, no 150k-item exchange.
                let relay_map = bg_client.relays().await;
                let established = relay_map.keys()
                    .any(|u| vector_core::negentropy::reconcile_cursor(u.as_str()).is_some());
                let candidates: Vec<(RelayUrl, Relay)> = relay_map.iter()
                    .filter(|(url, _)| {
                        // Cursor-less AND NEG-capable — a no-NIP-77 relay can never
                        // bootstrap this way, so it must not keep the phase alive.
                        vector_core::negentropy::reconcile_cursor(url.as_str()).is_none()
                            && vector_core::negentropy::neg_supported_cached(url.as_str()) != Some(false)
                    })
                    .map(|(url, relay)| (url.clone(), relay.clone()))
                    .collect();
                drop(relay_map);

                // Established accounts (any relay already cursored) never hold the
                // "syncing" state on a bootstrap: those exist for NEW relays and
                // run silently — one doomed candidate must not pin the UI for 45s.
                // A FRESH install keeps gating: there the bootstrap IS the initial
                // sync and drives the progress bar.
                let mut completed_early = false;
                if established && !candidates.is_empty() {
                    {
                        let mut state = STATE.lock().await;
                        state.is_syncing = false;
                    }
                    vector_core::emit_event("sync_finished", &());
                    completed_early = true;
                    println!("[Sync] Sync complete — {} relay bootstrap(s) continue in background",
                        candidates.len());
                }

                if candidates.is_empty() {
                    println!("[Sync] Archive: no bootstrap candidates (all relays cursored or no-NIP-77)");
                    dump_wrapper_cache().await;
                    let mut state = STATE.lock().await;
                    state.is_syncing = false;
                    drop(state);
                    vector_core::emit_event("sync_finished", &());
                    return;
                }


                // Reload items (includes anything saved during quick phase)
                let items = db::load_negentropy_items().unwrap_or_default();
                println!("[Sync] Archive: negentropy with {} items", items.len());

                let filter = Filter::new()
                    .pubkey(my_public_key)
                    .kind(Kind::GiftWrap);
                // Generous budgets ON PURPOSE: a bootstrap's first NEG frame is the
                // relay walking its entire matching set to build fingerprints — a
                // slow box can need minutes. Killing it early means paying that
                // walk every boot and never banking the cursor that would end the
                // retries; one slow completion is strictly cheaper. The UI never
                // waits on this (established accounts complete before the drain,
                // fresh installs detach stragglers after the first success).
                let opts = nostr_sdk::prelude::SyncOptions::new()
                    .direction(nostr_sdk::prelude::SyncDirection::Down)
                    .initial_timeout(std::time::Duration::from_secs(300))
                    .dry_run();

                // No-NIP-77 relays would silently burn the full bootstrap budget here —
                // the single biggest boot-time waster this phase ever had.
                let no_neg: std::collections::HashSet<String> = candidates.iter()
                    .filter(|(u, _)| vector_core::negentropy::neg_supported_cached(u.as_str()) == Some(false))
                    .map(|(u, _)| u.to_string())
                    .collect();
                // A relay that refused the 7-day set refuses this larger one identically;
                // asking again just burns the bootstrap budget. Timeouts are not in this set.
                let relays: Vec<(RelayUrl, Relay)> = candidates.into_iter()
                    .filter(|(url, _)| !no_neg.contains(&url.to_string()))
                    .collect();
                if !no_neg.is_empty() {
                    println!("[Sync] Archive: skipping {} no-NIP-77 relay(s): {:?}",
                        no_neg.len(), no_neg);
                }
                if relays.is_empty() {
                    println!("[Sync] Archive: no eligible relays, skipping");
                    dump_wrapper_cache().await;
                    if !completed_early {
                        let mut state = STATE.lock().await;
                        state.is_syncing = false;
                        drop(state);
                        vector_core::emit_event("sync_finished", &());
                    }
                    return;
                }

                // Anchor before any reconcile begins — see the quick phase's twin.
                let archive_anchor = Timestamp::now().as_secs();
                let mut all_missing: std::collections::HashSet<EventId> = std::collections::HashSet::new();
                // Reconciled clean but reported missing events: their first cursor
                // is only earned once every requested event arrives AND persists.
                let mut cursor_pending: Vec<String> = Vec::new();
                let mut futs = futures_util::stream::FuturesUnordered::new();
                for (url, relay) in &relays {
                    let url = url.clone();
                    let relay = relay.clone();
                    let f = filter.clone();
                    let i = items.clone();
                    let o = opts.clone();
                    futs.push(async move {
                        if !vector_core::negentropy::wait_connected(&relay, connect_allowance).await {
                            return (url, None);
                        }
                        let mut result = tokio::time::timeout(
                            std::time::Duration::from_secs(480),
                            with_auth_retry(|| async {
                                relay.sync(f.clone()).items(i.clone()).opts(o.clone()).await
                            }),
                        ).await;
                        // A broadcast-lag kill lands AFTER the relay paid its
                        // tree-build; one immediate retry rides the now-warm cache
                        // instead of forfeiting minutes of server work.
                        if matches!(&result, Ok(Err(e)) if e.to_string().contains("lagged")) {
                            println!("[Sync] Archive: {} lagged — retrying once on the warm cache", url);
                            result = tokio::time::timeout(
                                std::time::Duration::from_secs(480),
                                with_auth_retry(|| async {
                                    relay.sync(f.clone()).items(i.clone()).opts(o.clone()).await
                                }),
                            ).await;
                        }
                        (url, Some(result))
                    });
                }

                // Drain with a grace window: once ONE relay has reconciled, the
                // rest get 10s to land before they detach to the background — a
                // single relay stalling on its first frame must not hold
                // sync_finished (and the whole "syncing" UI) hostage for 45s+.
                const ARCHIVE_STRAGGLER_GRACE: std::time::Duration = std::time::Duration::from_secs(10);
                let mut first_success: Option<std::time::Instant> = None;
                loop {
                    let next = match first_success {
                        Some(t0) => {
                            let Some(left) = ARCHIVE_STRAGGLER_GRACE.checked_sub(t0.elapsed()) else { break };
                            match tokio::time::timeout(left, futs.next()).await {
                                Ok(n) => n,
                                Err(_) => break,
                            }
                        }
                        None => futs.next().await,
                    };
                    let Some((url, result)) = next else { break };
                    let Some(result) = result else {
                        eprintln!("[Sync] Archive: {} skipped: not connected", url);
                        continue;
                    };
                    match result {
                        Ok(Ok(recon)) => {
                            let count = recon.remote.len();
                            println!("[Sync] Archive: {} reconciled: {} missing", url, count);
                            vector_core::negentropy::record_neg_support(url.as_str(), true);
                            if count == 0 {
                                // Full history verified against the ledger —
                                // this relay's cursor is born here.
                                vector_core::negentropy::advance_reconcile_cursor(url.as_str(), archive_anchor);
                            } else {
                                cursor_pending.push(url.to_string());
                            }
                            all_missing.extend(recon.remote);
                            first_success.get_or_insert_with(std::time::Instant::now);
                        }
                        Ok(Err(e)) => {
                            eprintln!("[Sync] Archive: {} failed: {}", url, e);
                            // `connected: false` — a full-history reconcile can
                            // legitimately outrun its timeout, so only the SDK's
                            // deterministic refusals classify here.
                            if archive_session.is_live()
                                && vector_core::negentropy::classify_neg_sync_error(&e.to_string(), false)
                                    == Some(false)
                            {
                                println!("[Sync] Archive: {} marked no-NIP-77 for 24h", url);
                                vector_core::negentropy::record_neg_support(url.as_str(), false);
                            }
                        }
                        Err(_) => eprintln!("[Sync] Archive: {} timed out (bootstrap budget)", url),
                    }
                }

                // Detach unresolved stragglers — their finds still land through the
                // same prepare → commit pipeline (wrapper-cache dedup has a DB
                // fallback after the cache dump), they just stop gating completion.
                if !futs.is_empty() {
                    println!("[Sync] Archive: detaching {} straggler relay(s)", futs.len());
                    let det_client = bg_client.clone();
                    let det_session = vector_core::db::current_session();
                    let primary_set: std::collections::HashSet<EventId> =
                        all_missing.iter().copied().collect();
                    vector_core::db::spawn_bound(async move {
                        let mut extra: Vec<EventId> = Vec::new();
                        while let Some((url, result)) = futs.next().await {
                            let Some(result) = result else {
                                eprintln!("[Sync][BG] Archive straggler {} skipped: not connected", url);
                                continue;
                            };
                            match result {
                                Ok(Ok(recon)) => {
                                    vector_core::negentropy::record_neg_support(url.as_str(), true);
                                    // Cursor birth only on the zero-missing proof;
                                    // a straggler that found events spans two fetch
                                    // pipelines, so it re-bootstraps next boot.
                                    if recon.remote.is_empty() {
                                        vector_core::negentropy::advance_reconcile_cursor(url.as_str(), archive_anchor);
                                    }
                                    let new: Vec<EventId> = recon.remote.into_iter()
                                        .filter(|id| !primary_set.contains(id))
                                        .collect();
                                    println!("[Sync][BG] Archive straggler {}: {} additional missing", url, new.len());
                                    extra.extend(new);
                                }
                                Ok(Err(e)) => {
                                    eprintln!("[Sync][BG] Archive straggler {} failed: {}", url, e);
                                    if det_session.is_live()
                                        && vector_core::negentropy::classify_neg_sync_error(&e.to_string(), false)
                                            == Some(false)
                                    {
                                        vector_core::negentropy::record_neg_support(url.as_str(), false);
                                    }
                                }
                                Err(_) => eprintln!("[Sync][BG] Archive straggler {} timed out (bootstrap budget)", url),
                            }
                        }
                        if extra.is_empty() { return; }

                        println!("[Sync][BG] Fetching {} events from archive stragglers", extra.len());
                        let relay_strs: Vec<String> = det_client.relays().await.keys()
                            .map(|u| u.to_string()).collect();
                        let det_inner = crate::services::event_handler::TauriEventHandler;
                        let det_batcher = vector_core::event_handler::BatchingPersist::new(&det_inner);
                        for batch in extra.chunks(500) {
                            if vector_core::db::session_stopped() {
                                break;
                            }
                            let f = Filter::new().ids(batch.to_vec()).kind(Kind::GiftWrap).pubkey(my_public_key);
                            match det_client.stream_events(nostr_sdk::prelude::ReqTarget::manual(
                                relay_strs.clone().into_iter().map(|u| (u, vec![f.clone()])),
                            ))
                            .timeout(std::time::Duration::from_secs(30))
                            .await {
                                Ok(stream) => {
                                    tokio::pin!(stream);
                                    while let Some((_relay, res)) = stream.next().await {
                                        let Ok(event) = res else { continue };
                                        let prepared = vector_core::event_handler::prepare_event(
                                            event, &det_client, my_public_key,
                                        ).await;
                                        crate::services::tauri_commit_prepared_event_with(prepared, false, &det_batcher).await;
                                        if det_batcher.buffered() >= PERSIST_BATCH {
                                            det_batcher.flush().await;
                                        }
                                    }
                                }
                                Err(e) => eprintln!("[Sync][BG] Archive straggler fetch error: {}", e),
                            }
                        }
                        det_batcher.flush().await;
                        println!("[Sync][BG] Archive stragglers complete");
                    });
                }

                if !all_missing.is_empty() {
                    let missing_total = all_missing.len() as u32;
                    println!("[Sync] Archive: fetching {} events", missing_total);
                    let ids: Vec<EventId> = all_missing.into_iter().collect();
                    let relay_strs: Vec<String> = bg_client.relays().await.keys()
                        .map(|u| u.to_string()).collect();
                    let archive_inner = crate::services::event_handler::TauriEventHandler;
                    let archive_batcher = vector_core::event_handler::BatchingPersist::new(&archive_inner);
                    const BATCH: usize = 500;
                    let mut processed = 0u32;
                    // Receipt is only counted for REQUESTED ids: the pool-wide
                    // fetch can yield off-filter events (any of our other wraps
                    // passes kind+#p on a sloppy relay), and those must not pad
                    // the coverage proof for a relay whose events never arrived.
                    let want: std::collections::HashSet<EventId> = ids.iter().copied().collect();
                    let mut received: std::collections::HashSet<EventId> = std::collections::HashSet::new();
                    // A cursor born over an unledgered batch skips those events
                    // forever — a failed flush must veto the birth below.
                    let mut flushes_ok = true;
                    for batch in ids.chunks(BATCH) {
                        if vector_core::db::session_stopped() {
                            break;
                        }
                        let f = Filter::new().ids(batch.to_vec()).kind(Kind::GiftWrap).pubkey(my_public_key);
                        match bg_client.stream_events(nostr_sdk::prelude::ReqTarget::manual(relay_strs.clone().into_iter().map(|u| (u, vec![f.clone()]))))
                    .timeout(std::time::Duration::from_secs(30),
                        ).await {
                            Ok(stream) => {
                                tokio::pin!(stream);
                                while let Some((_relay, res)) = stream.next().await {
                                    let Ok(event) = res else { continue };
                                    if want.contains(&event.id) {
                                        received.insert(event.id);
                                    }
                                    let prepared = vector_core::event_handler::prepare_event(
                                        event, &bg_client, my_public_key,
                                    ).await;
                                    processed += 1;
                                    if processed % 250 == 0 {
                                        vector_core::emit_event("sync_progress", &serde_json::json!({
                                            "mode": "Syncing",
                                            "current": processed,
                                            "total": missing_total,
                                            "new_messages": archive_new,
                                        }));
                                    }
                                    if crate::services::tauri_commit_prepared_event_with(prepared, false, &archive_batcher).await {
                                        archive_new += 1;
                                    }
                                    if archive_batcher.buffered() >= PERSIST_BATCH {
                                        flushes_ok &= archive_batcher.try_flush().await.is_ok();
                                    }
                                }
                            }
                            Err(e) => eprintln!("[Sync] Archive: batch fetch error: {}", e),
                        }
                    }
                    flushes_ok &= archive_batcher.try_flush().await.is_ok();

                    // Bootstrap cursors are earned only when every requested event
                    // arrived AND every batch landed: commit/skip both ledger the
                    // wrapper, but only through a flush that succeeded — a birth
                    // over a lost batch would skip those events forever.
                    if flushes_ok && received.len() == ids.len() {
                        for u in &cursor_pending {
                            vector_core::negentropy::advance_reconcile_cursor(u, archive_anchor);
                        }
                    } else if !cursor_pending.is_empty() {
                        println!("[Sync] Archive: {}/{} received, flushes_ok={} — cursor birth deferred to next boot",
                            received.len(), ids.len(), flushes_ok);
                    }
                } else {
                    println!("[Sync] Archive: no missing events");
                }

                // ════════════════════════════════════════════
                // Sync complete — cleanup + notify frontend
                // ════════════════════════════════════════════

                println!("[Sync] ══════════════ SYNC COMPLETE ══════════════");
                println!("[Sync]   Archive:     {:.2?}", archive_start.elapsed());
                println!("[Sync]   Archive new: {}", archive_new);
                println!("[Sync] ════════════════════════════════════════════");

                // Clear the wrapper_id cache — only needed during sync for dedup
                dump_wrapper_cache().await;

                if !completed_early {
                    {
                        let mut state = STATE.lock().await;
                        state.is_syncing = false;
                    }
                    vector_core::emit_event("sync_finished", &());
                }

                // Resolve + cache our own badges AFTER boot/init settles — not during.
                // The claim's holding relay (often the user's own) is saturated through
                // the DM archive + concurrent community sweep, so a fetch
                // fired now routinely misses and trips the multi-hour re-check cooldown.
                // A short settle delay lets the pool go quiet first, giving the 10s
                // fetch a clean shot. (On-demand profile checks are the other,
                // self-persisting path — see badges.rs.)
                vector_core::db::spawn_bound(async move {
                    tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                    vector_core::badges::refresh_own_badges().await;
                    vector_core::badges::refresh_own_bug_hunter().await;
                    // Through emit_event, not the handle: the badges belong to the
                    // account this task began under, and a swap during that half
                    // minute means they are not the badges now on screen.
                    vector_core::emit_event("badges_updated", &serde_json::json!({
                        "vector": vector_core::badges::has_vector_badge(),
                        "tier": vector_core::badges::effective_tier(),
                        "bug_hunter": vector_core::badges::bug_hunter_tier(),
                    }));
                });

                // Post-sync: weekly vacuum + daily planner-stats refresh.
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                if let Err(e) = db::check_and_vacuum_if_needed().await {
                    eprintln!("[Maintenance] Weekly VACUUM check failed: {}", e);
                }
                if let Err(e) = db::check_and_optimize_if_needed().await {
                    eprintln!("[Maintenance] Daily optimize check failed: {}", e);
                }
            });
        }
    })
    .await
}

// Handler list for this module (for reference):
// - queue_profile_sync
// - queue_chat_profiles_sync
// - refresh_profile_now
// - sync_all_profiles
// - is_scanning
// - fetch_messages
