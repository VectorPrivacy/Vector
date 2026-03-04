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
    ChatType, Profile,
    NOSTR_CLIENT, STATE, WRAPPER_ID_CACHE,
    services::event_handler::PreparedEvent,
};

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
    println!("[Boot] fetch_messages called (init={}, relay={:?})", init, relay_url);
    let client = NOSTR_CLIENT.get().expect("Nostr client not initialized");

    // Grab our pubkey
    let my_public_key = *crate::MY_PUBLIC_KEY.get().expect("Public key not initialized");

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

        // Load negentropy items — use 2-day window for fast reconnection sync
        let all_items = db::load_negentropy_items().unwrap_or_default();
        let quick_since = Timestamp::now().as_secs().saturating_sub(2 * 24 * 3600);
        let items: Vec<(EventId, Timestamp)> = all_items.iter()
            .filter(|(_, ts)| ts.as_secs() >= quick_since)
            .cloned()
            .collect();
        let filter = Filter::new()
            .pubkey(my_public_key)
            .kind(Kind::GiftWrap)
            .since(Timestamp::from_secs(quick_since));
        let sync_opts = nostr_sdk::SyncOptions::new()
            .direction(nostr_sdk::SyncDirection::Down)
            .initial_timeout(std::time::Duration::from_secs(3))
            .dry_run();

        let recon_result = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            relay.sync_with_items(filter, items, &sync_opts),
        ).await;

        let missing_ids: Vec<EventId> = match recon_result {
            Ok(Ok(recon)) => {
                let ids: Vec<EventId> = recon.remote.into_iter().collect();
                println!("[Sync] Single-relay {} reconciled in {:?}: {} missing",
                    url, recon_start.elapsed(), ids.len());
                ids
            }
            Ok(Err(e)) => {
                eprintln!("[Sync] Single-relay {} negentropy failed: {}", url, e);
                return;
            }
            Err(_) => {
                eprintln!("[Sync] Single-relay {} negentropy timed out (10s)", url);
                return;
            }
        };

        // Fetch + process missing events
        if !missing_ids.is_empty() {
            const BATCH_SIZE: usize = 500;
            for batch in missing_ids.chunks(BATCH_SIZE) {
                let f = Filter::new().ids(batch.to_vec()).kind(Kind::GiftWrap);
                match client.stream_events_from(
                    vec![url.clone()], f,
                    std::time::Duration::from_secs(30),
                ).await {
                    Ok(stream) => {
                        let client_clone = client.clone();
                        let prepared_stream = stream
                            .map(move |event| {
                                let c = client_clone.clone();
                                tokio::spawn(async move {
                                    crate::services::prepare_event(event, &c, my_public_key).await
                                })
                            })
                            .buffer_unordered(8);
                        tokio::pin!(prepared_stream);
                        while let Some(result) = prepared_stream.next().await {
                            if let Ok(prepared) = result {
                                crate::services::commit_prepared_event(prepared, false).await;
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("[Sync] Single-relay {} fetch error: {}", url, e);
                    }
                }
            }
        }

        // Also sync MLS group messages after single-relay reconnection
        if let Err(e) = crate::commands::mls::sync_mls_groups_now(None).await {
            eprintln!("[Single-Relay Sync] Failed to sync MLS groups: {}", e);
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
                    println!("[Startup] Set current account for SQL mode: {}", npub);
                }
            }

            // Load our DB (if we haven't already)
            if !state.db_loaded {
                // Load profiles, chats, MLS groups, and last messages in parallel (all are independent reads)
                let db_start = std::time::Instant::now();
                let (profiles_result, slim_chats_result, mls_groups_result, last_messages_result) = tokio::join!(
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
                        let r = db::load_mls_groups().await;
                        println!("[Boot]   load_mls_groups: {:?}", t.elapsed());
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
                tokio::spawn(async {
                    profile::cache_all_profile_images().await;
                });

                // Get the last messages map (single batch query result)
                let mut last_messages_map = last_messages_result.unwrap_or_default();

                // Process chats
                if let Ok(slim_chats) = slim_chats_result {
                    // Build HashSet of evicted MLS group IDs for O(1) lookup
                    let evicted_groups: std::collections::HashSet<&str> = mls_groups_result
                        .as_ref()
                        .map(|groups| groups.iter()
                            .filter(|g| g.evicted)
                            .map(|g| g.group_id.as_str())
                            .collect())
                        .unwrap_or_default();

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
                        // Skip evicted MLS groups (O(1) lookup)
                        if slim_chat.chat_type == ChatType::MlsGroup && evicted_groups.contains(slim_chat.id.as_str()) {
                            continue;
                        }

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

                // Check filesystem integrity for downloaded attachments (queries DB directly)
                tokio::spawn(async move {
                    if let Err(e) = db::check_downloaded_attachments_integrity().await {
                        eprintln!("[Integrity] Check failed: {}", e);
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
                handle.emit("init_finished", &InitPayload {
                    profiles: &slim_profiles,
                    chats: &serializable_chats,
                }).unwrap();
                println!("[Boot] Event emit in {:?}", emit_start.elapsed());
                println!("[Boot] Total init time: {:?}", boot_start.elapsed());
            }

            // Preload marketplace cache from SQLite → MARKETPLACE_STATE (non-blocking)
            // Ensures permission checks work before the user visits the Nexus tab,
            // then silently refreshes from the network in the background.
            tokio::spawn(async {
                crate::miniapps::marketplace::preload_marketplace_cache().await;
            });

            // Preload wrapper IDs for sync deduplication (non-blocking)
            // DB fallback in handle_event ensures correctness if this completes after sync starts
            tokio::spawn(async move {
                let t = std::time::Instant::now();
                let event_wrappers = db::load_recent_wrapper_ids(30).await.unwrap_or_default();
                let processed_wrappers = db::load_processed_wrappers().unwrap_or_default();
                let mut cache = WRAPPER_ID_CACHE.lock().await;
                let total = event_wrappers.len() + processed_wrappers.len();
                cache.load(event_wrappers);
                for w in processed_wrappers {
                    cache.insert(w);
                }
                println!("[Sync] wrapper_id cache loaded: {} entries ({:?})", total, t.elapsed());
            });

            state.is_syncing = true;
        }
    } // STATE lock released — no lock held during network operations

    // ========================================================================
    // Negentropy (NIP-77) set reconciliation — single-pass sync
    // ========================================================================

    let sync_start = std::time::Instant::now();
    let mut new_messages_count: u32 = 0;

    // Load our known wrapper IDs + timestamps for reconciliation fingerprinting
    let negentropy_items = db::load_negentropy_items().unwrap_or_default();
    let valid_ts_count = negentropy_items.iter().filter(|(_, ts)| ts.as_secs() > 0).count();
    println!("[Sync] Loaded {} negentropy items ({} with valid timestamps)",
        negentropy_items.len(), valid_ts_count);

    // Quick phase: last 2 days — tiny item set for near-instant reconciliation.
    // Shows recent offline messages within ~1s. Full archive sync runs in background after.
    let quick_since = Timestamp::now().as_secs().saturating_sub(2 * 24 * 3600);
    let quick_items: Vec<(EventId, Timestamp)> = negentropy_items.iter()
        .filter(|(_, ts)| ts.as_secs() >= quick_since)
        .cloned()
        .collect();
    let filter = Filter::new()
        .pubkey(my_public_key)
        .kind(Kind::GiftWrap)
        .since(Timestamp::from_secs(quick_since));
    println!("[Sync] Quick phase: {} items (last 2d), full: {}", quick_items.len(), negentropy_items.len());

    // Dry-run negentropy reconciliation — exchange fingerprints only
    // This identifies which events the relay has that we don't, without transferring data.
    let sync_opts = nostr_sdk::SyncOptions::new()
        .direction(nostr_sdk::SyncDirection::Down)
        .initial_timeout(std::time::Duration::from_secs(10))
        .dry_run();

    let reconcile_start = std::time::Instant::now();
    // Include ALL relays — not just connected ones. Relays still connecting will either
    // finish their handshake and reconcile within the 10s timeout, or timeout gracefully.
    // This avoids excluding late-connecting relays from the race entirely.
    let relay_map = client.relays().await;
    let all_relays: Vec<(RelayUrl, Relay)> = relay_map.iter()
        .map(|(url, relay)| (url.clone(), relay.clone()))
        .collect();
    drop(relay_map);
    println!("[Sync] Racing {} relay(s) for negentropy reconciliation", all_relays.len());

    // Phase 1: Race all relays — first to reconcile drives the primary sync.
    // Stragglers continue in the background and fill gaps if they have unique events.
    let mut relay_futs = futures_util::stream::FuturesUnordered::new();
    for (url, relay) in &all_relays {
        let url = url.clone();
        let relay = relay.clone();
        let f = filter.clone();
        let items = quick_items.clone();
        let opts = sync_opts.clone();
        relay_futs.push(async move {
            let result = tokio::time::timeout(
                std::time::Duration::from_secs(10),
                relay.sync_with_items(f, items, &opts),
            ).await;
            (url, result)
        });
    }

    // Drain until first successful reconciliation
    let mut primary_missing: Vec<EventId> = Vec::new();
    let mut primary_relay: Option<RelayUrl> = None;
    while let Some((url, result)) = relay_futs.next().await {
        match result {
            Ok(Ok(recon)) => {
                primary_missing = recon.remote.into_iter().collect();
                println!("[Sync] {} reconciled in {:?}: {} missing events",
                    url, reconcile_start.elapsed(), primary_missing.len());
                primary_relay = Some(url);
                break;
            }
            Ok(Err(e)) => {
                eprintln!("[Sync]   Relay {} failed: {}", url, e);
            }
            Err(_) => {
                eprintln!("[Sync]   Relay {} timed out (10s)", url);
            }
        }
    }

    // Spawn background task for remaining relays — they fill gaps silently
    if primary_relay.is_some() && !relay_futs.is_empty() {
        let primary_set: std::collections::HashSet<EventId> = primary_missing.iter().copied().collect();
        let bg_client = client.clone();
        tokio::spawn(async move {
            let mut extra_ids: Vec<EventId> = Vec::new();
            while let Some((url, result)) = relay_futs.next().await {
                match result {
                    Ok(Ok(recon)) => {
                        let new: Vec<EventId> = recon.remote.into_iter()
                            .filter(|id| !primary_set.contains(id))
                            .collect();
                        if !new.is_empty() {
                            println!("[Sync][BG] {} reconciled: {} additional missing events", url, new.len());
                            extra_ids.extend(new);
                        } else {
                            println!("[Sync][BG] {} reconciled: 0 additional", url);
                        }
                    }
                    Ok(Err(e)) => eprintln!("[Sync][BG] {} failed: {}", url, e),
                    Err(_) => eprintln!("[Sync][BG] {} timed out (10s)", url),
                }
            }

            // Fetch + process any extra events found by background relays
            if !extra_ids.is_empty() {
                println!("[Sync][BG] Fetching {} additional events from background relays", extra_ids.len());
                let relay_strs: Vec<String> = bg_client.relays().await.keys()
                    .map(|u| u.to_string()).collect();
                const BG_BATCH: usize = 500;
                for batch in extra_ids.chunks(BG_BATCH) {
                    let f = Filter::new().ids(batch.to_vec()).kind(Kind::GiftWrap);
                    match bg_client.stream_events_from(
                        relay_strs.clone(), f,
                        std::time::Duration::from_secs(30),
                    ).await {
                        Ok(stream) => {
                            tokio::pin!(stream);
                            let mut count = 0u32;
                            while let Some(event) = stream.next().await {
                                let prepared = crate::services::prepare_event(
                                    event, &bg_client, my_public_key,
                                ).await;
                                match &prepared {
                                    PreparedEvent::DedupSkip { wrapper_id_bytes, wrapper_created_at } => {
                                        if *wrapper_created_at > 0 {
                                            let _ = db::update_wrapper_timestamp(wrapper_id_bytes, *wrapper_created_at);
                                        }
                                    }
                                    PreparedEvent::ErrorSkip { wrapper_id_bytes, wrapper_created_at } => {
                                        let _ = db::save_processed_wrapper(wrapper_id_bytes, *wrapper_created_at);
                                    }
                                    _ => {
                                        if crate::services::commit_prepared_event(prepared, false).await {
                                            count += 1;
                                        }
                                    }
                                }
                            }
                            if count > 0 {
                                println!("[Sync][BG] {} new messages from background fetch", count);
                            }
                        }
                        Err(e) => eprintln!("[Sync][BG] Batch fetch error: {}", e),
                    }
                }
                println!("[Sync][BG] Background sync complete");
            }
        });
    }

    // Phase 2: Fetch primary missing events (drives progress bar)
    if !primary_missing.is_empty() && primary_relay.is_some() {
        let fetch_relay = primary_relay.unwrap().to_string();
        const BATCH_SIZE: usize = 500;
        let batches: Vec<&[EventId]> = primary_missing.chunks(BATCH_SIZE).collect();
        let batch_count = batches.len();
        println!("[Sync] Fetching {} missing events in {} batches",
            primary_missing.len(), batch_count);

        // Channel collects events from all concurrent batch fetches
        let (tx, rx) = tokio::sync::mpsc::channel::<Event>(1024);
        let sem = std::sync::Arc::new(tokio::sync::Semaphore::new(4));
        let mut fetch_handles = Vec::with_capacity(batch_count);

        for batch in &batches {
            let batch_ids: Vec<EventId> = batch.to_vec();
            let c = client.clone();
            let relay = fetch_relay.clone();
            let tx = tx.clone();
            let sem = sem.clone();
            fetch_handles.push(tokio::spawn(async move {
                let _permit = sem.acquire().await.unwrap();
                let f = Filter::new().ids(batch_ids).kind(Kind::GiftWrap);
                match c.stream_events_from(
                    vec![relay], f,
                    std::time::Duration::from_secs(5),
                ).await {
                    Ok(stream) => {
                        tokio::pin!(stream);
                        while let Some(event) = stream.next().await {
                            if tx.send(event).await.is_err() { break; }
                        }
                    }
                    Err(e) => {
                        eprintln!("[Sync] Batch fetch error: {}", e);
                    }
                }
            }));
        }
        drop(tx);

        // Dedup stream + process with 8-way parallel unwrap
        let mut total_events: u32 = 0;
        let mut dedup_skips: u32 = 0;
        let mut error_skips: u32 = 0;
        let mut unwrap_ns: u64 = 0;
        let mut parse_ns: u64 = 0;
        let mut commit_ns: u64 = 0;
        let fetch_start = std::time::Instant::now();

        let event_stream = futures_util::stream::unfold(
            (rx, std::collections::HashSet::<[u8; 32]>::new()),
            |(mut rx, mut seen)| async move {
                loop {
                    match rx.recv().await {
                        Some(event) => {
                            if seen.insert(event.id.to_bytes()) {
                                return Some((event, (rx, seen)));
                            }
                        }
                        None => return None,
                    }
                }
            },
        );

        let client_clone = client.clone();
        let prepared_stream = event_stream
            .map(move |event| {
                let c = client_clone.clone();
                tokio::spawn(async move {
                    crate::services::prepare_event(event, &c, my_public_key).await
                })
            })
            .buffer_unordered(8);
        tokio::pin!(prepared_stream);

        while let Some(result) = prepared_stream.next().await {
            total_events += 1;
            if let Ok(prepared) = result {
                match &prepared {
                    PreparedEvent::Processed { unwrap_ns: u, parse_ns: p, .. } => {
                        unwrap_ns += u;
                        parse_ns += p;
                    }
                    PreparedEvent::MlsWelcome { unwrap_ns: u, .. } => {
                        unwrap_ns += u;
                    }
                    PreparedEvent::DedupSkip { wrapper_id_bytes, wrapper_created_at } => {
                        dedup_skips += 1;
                        if *wrapper_created_at > 0 {
                            let _ = db::update_wrapper_timestamp(wrapper_id_bytes, *wrapper_created_at);
                        }
                        continue;
                    }
                    PreparedEvent::ErrorSkip { wrapper_id_bytes, wrapper_created_at } => {
                        let _ = db::save_processed_wrapper(wrapper_id_bytes, *wrapper_created_at);
                        error_skips += 1;
                        continue;
                    }
                }
                let t = std::time::Instant::now();
                if crate::services::commit_prepared_event(prepared, false).await {
                    new_messages_count += 1;
                }
                commit_ns += t.elapsed().as_nanos() as u64;
            }
        }

        for h in fetch_handles { h.abort(); }

        let unwrapped = total_events - dedup_skips - error_skips;
        if total_events > 0 {
            println!("[Sync] ──── Fetch + Processing ────");
            println!("[Sync]   Events:    {} fetched, {} dedup'd, {} unwrapped, {} errors",
                total_events, dedup_skips, unwrapped, error_skips);
            println!("[Sync]   Fetch:     {:.2?}", fetch_start.elapsed());
            if unwrapped > 0 {
                println!("[Sync]   Unwrap:    {:.2?} ({:.0?}/event avg)",
                    std::time::Duration::from_nanos(unwrap_ns),
                    std::time::Duration::from_nanos(unwrap_ns / unwrapped as u64));
            }
            println!("[Sync]   Parse:     {:.2?}", std::time::Duration::from_nanos(parse_ns));
            println!("[Sync]   Commit:    {:.2?}", std::time::Duration::from_nanos(commit_ns));
            println!("[Sync]   New msgs:  {}", new_messages_count);
        }
    }

    // Quick phase done — recent messages visible to user
    println!("[Sync] Quick phase: {:.2?}, {} new messages", sync_start.elapsed(), new_messages_count);

    // ========================================================================
    // Archive sync — full negentropy reconciliation (drives sync UI)
    // ========================================================================
    // Quick phase silently populated recent messages. The archive sync now
    // reconciles our full history with all relays using generous timeouts.
    {
        let bg_client = client.clone();
        let handle_bg = handle.clone();
        tokio::spawn(async move {
            let archive_start = std::time::Instant::now();
            let mut archive_new = 0u32;

            // Reload items (includes anything saved during quick phase)
            let items = db::load_negentropy_items().unwrap_or_default();
            println!("[Sync] Archive: negentropy with {} items", items.len());

            let filter = Filter::new()
                .pubkey(my_public_key)
                .kind(Kind::GiftWrap);
            let opts = nostr_sdk::SyncOptions::new()
                .direction(nostr_sdk::SyncDirection::Down)
                .initial_timeout(std::time::Duration::from_secs(45))
                .dry_run();

            let relay_map = bg_client.relays().await;
            let relays: Vec<(RelayUrl, Relay)> = relay_map.iter()
                .map(|(url, relay)| (url.clone(), relay.clone()))
                .collect();
            drop(relay_map);

            let mut all_missing: std::collections::HashSet<EventId> = std::collections::HashSet::new();
            let mut futs = futures_util::stream::FuturesUnordered::new();
            for (url, relay) in &relays {
                let url = url.clone();
                let relay = relay.clone();
                let f = filter.clone();
                let i = items.clone();
                let o = opts.clone();
                futs.push(async move {
                    let result = tokio::time::timeout(
                        std::time::Duration::from_secs(120),
                        relay.sync_with_items(f, i, &o),
                    ).await;
                    (url, result)
                });
            }

            while let Some((url, result)) = futs.next().await {
                match result {
                    Ok(Ok(recon)) => {
                        let count = recon.remote.len();
                        all_missing.extend(recon.remote);
                        println!("[Sync] Archive: {} reconciled: {} missing", url, count);
                    }
                    Ok(Err(e)) => eprintln!("[Sync] Archive: {} failed: {}", url, e),
                    Err(_) => eprintln!("[Sync] Archive: {} timed out (120s)", url),
                }
            }

            if !all_missing.is_empty() {
                let missing_total = all_missing.len() as u32;
                println!("[Sync] Archive: fetching {} events", missing_total);
                let ids: Vec<EventId> = all_missing.into_iter().collect();
                let relay_strs: Vec<String> = bg_client.relays().await.keys()
                    .map(|u| u.to_string()).collect();
                const BATCH: usize = 500;
                let mut processed = 0u32;
                for batch in ids.chunks(BATCH) {
                    let f = Filter::new().ids(batch.to_vec()).kind(Kind::GiftWrap);
                    match bg_client.stream_events_from(
                        relay_strs.clone(), f,
                        std::time::Duration::from_secs(30),
                    ).await {
                        Ok(stream) => {
                            tokio::pin!(stream);
                            while let Some(event) = stream.next().await {
                                let prepared = crate::services::prepare_event(
                                    event, &bg_client, my_public_key,
                                ).await;
                                processed += 1;
                                if processed % 250 == 0 {
                                    let _ = handle_bg.emit("sync_progress", serde_json::json!({
                                        "mode": "Syncing",
                                        "current": processed,
                                        "total": missing_total,
                                        "new_messages": archive_new,
                                    }));
                                }
                                match &prepared {
                                    PreparedEvent::DedupSkip { wrapper_id_bytes, wrapper_created_at } => {
                                        if *wrapper_created_at > 0 {
                                            let _ = db::update_wrapper_timestamp(wrapper_id_bytes, *wrapper_created_at);
                                        }
                                    }
                                    PreparedEvent::ErrorSkip { wrapper_id_bytes, wrapper_created_at } => {
                                        let _ = db::save_processed_wrapper(wrapper_id_bytes, *wrapper_created_at);
                                    }
                                    _ => {
                                        if crate::services::commit_prepared_event(prepared, false).await {
                                            archive_new += 1;
                                        }
                                    }
                                }
                            }
                        }
                        Err(e) => eprintln!("[Sync] Archive: batch fetch error: {}", e),
                    }
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
            {
                let mut cache = WRAPPER_ID_CACHE.lock().await;
                let cache_size = cache.len();
                cache.clear();
                println!("[Sync] Dumped wrapper cache (~{} KB freed)", (cache_size * 35) / 1024);
            }

            {
                let mut state = STATE.lock().await;
                state.is_syncing = false;
            }

            // Warm the file hash cache in the background (for attachment deduplication)
            tokio::task::spawn(async move {
                db::warm_file_hash_cache().await;
            });

            let _ = handle_bg.emit("sync_finished", ());

            // Post-sync: MLS groups + weekly vacuum
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            if let Err(e) = crate::commands::mls::sync_mls_groups_now(None).await {
                eprintln!("[MLS] Post-sync MLS group sync failed: {}", e);
            }
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            if let Err(e) = db::check_and_vacuum_if_needed().await {
                eprintln!("[Maintenance] Weekly VACUUM check failed: {}", e);
            }
        });
    }
}

// Handler list for this module (for reference):
// - queue_profile_sync
// - queue_chat_profiles_sync
// - refresh_profile_now
// - sync_all_profiles
// - is_scanning
// - fetch_messages
