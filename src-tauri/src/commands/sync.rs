//! Synchronization Tauri commands.
//!
//! This module handles:
//! - Message fetching and sync (fetch_messages, deep_rescan)
//! - Profile synchronization
//! - Sync status checking

use nostr_sdk::prelude::*;
use tauri::{AppHandle, Emitter, Manager, Runtime};

use crate::{
    db, profile, profile_sync,
    ChatType, Profile, SyncMode,
    NOSTR_CLIENT, STATE, WRAPPER_ID_CACHE,
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
/// This is the main sync loop that handles:
/// - Initial sync on app startup
/// - Forward sync (recent messages)
/// - Backward sync (historical messages)
/// - Single-relay sync for reconnection
#[tauri::command]
pub async fn fetch_messages<R: Runtime>(
    handle: AppHandle<R>,
    init: bool,
    relay_url: Option<String>
) {
    let client = NOSTR_CLIENT.get().expect("Nostr client not initialized");

    // Grab our pubkey
    let signer = client.signer().await.unwrap();
    let my_public_key = signer.get_public_key().await.unwrap();

    // If relay_url is provided, this is a single-relay sync that bypasses global state
    if relay_url.is_some() {
        // Single relay sync - always fetch last 2 days
        let now = Timestamp::now();
        let two_days_ago = now.as_secs() - (60 * 60 * 24 * 2);

        let filter = Filter::new()
            .pubkey(my_public_key)
            .kind(Kind::GiftWrap)
            .since(Timestamp::from_secs(two_days_ago))
            .until(now);

        // Fetch from specific relay only
        let mut events = client
            .stream_events_from(vec![relay_url.unwrap()], filter, std::time::Duration::from_secs(30))
            .await
            .unwrap();

        // Process events without affecting global sync state
        while let Some(event) = events.next().await {
            crate::services::handle_event(event, false).await;
        }

        // Also sync MLS group messages after single-relay reconnection
        if let Err(e) = crate::commands::mls::sync_mls_groups_now(None).await {
            eprintln!("[Single-Relay Sync] Failed to sync MLS groups: {}", e);
        }

        return; // Exit early for single-relay syncs
    }

    // Regular sync logic with global state management
    let (since_timestamp, until_timestamp) = {
        let mut state = STATE.lock().await;

        if init {
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

            // Load our DB (if we haven't already; i.e: our profile is the single loaded profile since login)
            if state.profiles.len() == 1 {
                // Load profiles, chats, MLS groups, and last messages in parallel (all are independent reads)
                let (profiles_result, slim_chats_result, mls_groups_result, last_messages_result) = tokio::join!(
                    db::get_all_profiles(&handle),
                    db::get_all_chats(&handle),
                    db::load_mls_groups(&handle),
                    db::get_all_chats_last_messages(&handle)
                );

                // Process profiles
                if let Ok(profiles) = profiles_result {
                    state.merge_db_profiles(profiles).await;
                }

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

                    // Build HashSet of existing profile IDs for O(1) lookup
                    let mut known_profiles: std::collections::HashSet<String> =
                        state.profiles.iter().map(|p| p.id.clone()).collect();

                    // Pre-allocate capacity for chats (avoids reallocations during push)
                    state.chats.reserve(slim_chats.len());

                    // Convert slim chats to full chats and merge last messages
                    #[cfg(debug_assertions)]
                    let start = std::time::Instant::now();
                    let mut total_messages = 0usize;

                    for slim_chat in slim_chats {
                        // Skip evicted MLS groups (O(1) lookup)
                        if slim_chat.chat_type == ChatType::MlsGroup && evicted_groups.contains(slim_chat.id.as_str()) {
                            continue;
                        }

                        let mut chat = slim_chat.to_chat();
                        let chat_id = chat.id().to_string();

                        // Ensure profiles exist for all chat participants (O(1) lookup)
                        for participant in chat.participants() {
                            if !known_profiles.contains(participant) {
                                let mut profile = Profile::new();
                                profile.id = participant.clone();
                                profile.mine = false;
                                state.profiles.push(profile);
                                known_profiles.insert(participant.clone());
                            }
                        }

                        // Get messages to add (if any)
                        let messages_to_add = last_messages_map.remove(&chat_id);

                        // Add messages to the chat using interner, then push
                        // This avoids double borrow by operating on local chat before adding to state
                        if let Some(messages) = messages_to_add {
                            total_messages += messages.len();
                            for message in messages {
                                chat.internal_add_message(message, &mut state.interner);
                            }
                        }

                        // Push the chat (now with messages) to state
                        state.chats.push(chat);
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
            }

            // Check filesystem integrity for downloaded attachments (queries DB directly)
            let handle_for_integrity = handle.clone();
            tokio::spawn(async move {
                if let Err(e) = db::check_downloaded_attachments_integrity(&handle_for_integrity).await {
                    eprintln!("[Integrity] Check failed: {}", e);
                }
            });

            // Preload caches in parallel
            let (id_cache_result, wrapper_ids_result) = tokio::join!(
                db::preload_id_caches(&handle),
                db::load_recent_wrapper_ids(&handle, 30)
            );

            if let Err(e) = id_cache_result {
                eprintln!("[Cache] Failed to preload ID caches: {}", e);
            }

            if let Ok(wrapper_ids) = wrapper_ids_result {
                let mut cache = WRAPPER_ID_CACHE.lock().await;
                cache.load(wrapper_ids);
            }

            // Send the state to frontend (convert chats to serializable format)
            let serializable_chats: Vec<_> = state.chats.iter()
                .map(|c| c.to_serializable(&state.interner))
                .collect();
            handle.emit("init_finished", serde_json::json!({
                "profiles": &state.profiles,
                "chats": serializable_chats
            })).unwrap();

            // ALWAYS begin with an initial sync of at least the last 2 days
            let now = Timestamp::now();

            state.is_syncing = true;
            state.sync_mode = SyncMode::ForwardSync;
            state.sync_empty_iterations = 0;
            state.sync_total_iterations = 0;

            // Initial 2-day window: now - 2 days → now
            let two_days_ago = now.as_secs() - (60 * 60 * 24 * 2);

            state.sync_window_start = two_days_ago;
            state.sync_window_end = now.as_secs();

            (
                Timestamp::from_secs(two_days_ago),
                now
            )
        } else if state.sync_mode == SyncMode::ForwardSync {
            // Forward sync (filling gaps from last message to now)
            let window_start = state.sync_window_start;

            // Adjust window for next iteration (go back in time in 2-day increments)
            let new_window_end = window_start;
            let new_window_start = window_start - (60 * 60 * 24 * 2); // Always 2 days

            // Update state with new window
            state.sync_window_start = new_window_start;
            state.sync_window_end = new_window_end;

            (
                Timestamp::from_secs(new_window_start),
                Timestamp::from_secs(new_window_end)
            )
        } else if state.sync_mode == SyncMode::BackwardSync {
            // Backward sync (historically old messages)
            let window_start = state.sync_window_start;

            // Move window backward in time in 2-day increments
            let new_window_end = window_start;
            let new_window_start = window_start - (60 * 60 * 24 * 2); // Always 2 days

            // Update state with new window
            state.sync_window_start = new_window_start;
            state.sync_window_end = new_window_end;

            (
                Timestamp::from_secs(new_window_start),
                Timestamp::from_secs(new_window_end)
            )
        } else if state.sync_mode == SyncMode::DeepRescan {
            // Deep rescan mode - scan backwards in 2-day increments until 30 days of no events
            let window_start = state.sync_window_start;

            // Move window backward in time in 2-day increments
            let new_window_end = window_start;
            let new_window_start = window_start - (60 * 60 * 24 * 2); // Always 2 days

            // Update state with new window
            state.sync_window_start = new_window_start;
            state.sync_window_end = new_window_end;

            (
                Timestamp::from_secs(new_window_start),
                Timestamp::from_secs(new_window_end)
            )
        } else {
            // Sync finished or in unknown state
            // Return dummy values, won't be used as we'll end sync
            (Timestamp::now(), Timestamp::now())
        }
    };

    // If sync is finished, emit the finished event and return
    {
        let state = STATE.lock().await;
        if state.sync_mode == SyncMode::Finished {
            // Only emit if this is not a single-relay sync
            if relay_url.is_none() {
                handle.emit("sync_finished", ()).unwrap();
            }
            return;
        }
    }

    // Emit our current "Sync Range" to the frontend (only for general syncs, not single-relay)
    if relay_url.is_none() {
        handle.emit("sync_progress", serde_json::json!({
            "since": since_timestamp.as_secs(),
            "until": until_timestamp.as_secs(),
            "mode": format!("{:?}", STATE.lock().await.sync_mode)
        })).unwrap();
    }

    // Fetch GiftWraps related to us within the time window
    let filter = Filter::new()
        .pubkey(my_public_key)
        .kind(Kind::GiftWrap)
        .since(since_timestamp)
        .until(until_timestamp);

    let mut event_stream = if let Some(url) = &relay_url {
        // Fetch from specific relay
        client
            .stream_events_from(vec![url], filter, std::time::Duration::from_secs(30))
            .await
            .unwrap()
    } else {
        // Fetch from all relays
        client
            .stream_events(filter, std::time::Duration::from_secs(60))
            .await
            .unwrap()
    };

    // Count total events fetched (for DeepRescan) and new messages added (for other modes)
    // We'll compute total count while iterating; placeholder will be set after loop
    let mut new_messages_count: u16 = 0;
    while let Some(event) = event_stream.next().await {
        // Count the amount of accepted (new) events
        if crate::services::handle_event(event, false).await {
            new_messages_count += 1;
        }
    }

    // After processing all events, total_events_count equals the number of processed events
    let total_events_count = new_messages_count as u16;
    let should_continue = {
        let mut state = STATE.lock().await;
        let mut continue_sync = true;

        // Increment total iterations counter
        state.sync_total_iterations += 1;

        // For DeepRescan, use total events count; for other modes, use new messages count
        let events_found = if state.sync_mode == SyncMode::DeepRescan {
            total_events_count
        } else {
            new_messages_count
        };

        // Update state based on if events were found
        if events_found > 0 {
            state.sync_empty_iterations = 0;
        } else {
            state.sync_empty_iterations += 1;
        }

        if state.sync_mode == SyncMode::ForwardSync {
            // Forward sync transitions to backward sync after:
            // 1. Finding messages and going 3 more iterations without messages, or
            // 2. Going 5 iterations without finding any messages
            let enough_empty_iterations = state.sync_empty_iterations >= 5;
            let found_then_empty = new_messages_count > 0 && state.sync_empty_iterations >= 3;

            if found_then_empty || enough_empty_iterations {
                // Time to switch mode - calculate oldest timestamp while holding lock
                let mut oldest_timestamp = None;

                // Check each chat's messages for oldest timestamp
                for chat in &state.chats {
                    if let Some(oldest_msg_time) = chat.last_message_time() {
                        match oldest_timestamp {
                            None => oldest_timestamp = Some(oldest_msg_time),
                            Some(current_oldest) => {
                                if oldest_msg_time < current_oldest {
                                    oldest_timestamp = Some(oldest_msg_time);
                                }
                            }
                        }
                    }
                }

                // Switch to backward sync mode
                state.sync_mode = SyncMode::BackwardSync;
                state.sync_empty_iterations = 0;
                state.sync_total_iterations = 0;

                if let Some(oldest_ts) = oldest_timestamp {
                    state.sync_window_end = oldest_ts;
                    state.sync_window_start = oldest_ts - (60 * 60 * 24 * 2); // 2 days before oldest
                } else {
                    // Still start backward sync, but from recent history
                    let now = Timestamp::now().as_secs();
                    let thirty_days_ago = now - (60 * 60 * 24 * 30);

                    state.sync_window_end = thirty_days_ago;
                    state.sync_window_start = thirty_days_ago - (60 * 60 * 24 * 2);
                }
            }
        } else if state.sync_mode == SyncMode::BackwardSync {
            // For backward sync, continue until:
            // No messages found for 5 consecutive iterations
            let enough_empty_iterations = state.sync_empty_iterations >= 5;

            if enough_empty_iterations {
                // We've completed backward sync
                state.sync_mode = SyncMode::Finished;
                continue_sync = false;
            }
        } else if state.sync_mode == SyncMode::DeepRescan {
            // For deep rescan, continue until:
            // No messages found for 15 consecutive iterations (30 days of no events)
            // Each iteration is 2 days, so 15 iterations = 30 days
            let enough_empty_iterations = state.sync_empty_iterations >= 15;

            if enough_empty_iterations {
                // We've completed deep rescan
                state.sync_mode = SyncMode::Finished;
                continue_sync = false;
            }
        } else {
            continue_sync = false; // Unknown state, stop syncing
        }

        continue_sync
    };

    if should_continue {
        // Keep synchronising
        if relay_url.is_none() {
            handle.emit("sync_slice_finished", ()).unwrap();
        }
    } else {
        // We're done with sync - update state first, then emit event
        {
            let mut state = STATE.lock().await;
            state.sync_mode = SyncMode::Finished;
            state.is_syncing = false;
            state.sync_empty_iterations = 0;
            state.sync_total_iterations = 0;
        } // Release lock before emitting event

        // Clear the wrapper_id cache - it's only needed during sync
        {
            let mut cache = WRAPPER_ID_CACHE.lock().await;
            let cache_size = cache.len();
            cache.clear();
            // Each entry: [u8; 32] in Vec (32 bytes) or HashSet (~48 bytes) ≈ 35 bytes average
            println!("[Startup] Sync Complete - Dumped NIP-59 Decryption Cache (~{} KB Memory freed)", (cache_size * 35) / 1024);
        }

        // Warm the file hash cache in the background (for attachment deduplication)
        // Only builds if there are attachments and cache wasn't already built during sync
        let handle_for_cache = handle.clone();
        tokio::task::spawn(async move {
            db::warm_file_hash_cache(&handle_for_cache).await;
        });

        if relay_url.is_none() {
            handle.emit("sync_finished", ()).unwrap();

            // Now that regular sync is complete and chats are loaded, sync MLS groups
            // This ensures chat data is in memory before MLS tries to sync participants
            let handle_clone = handle.clone();
            tokio::task::spawn(async move {
                // Small delay to ensure init_finished has been processed
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                if let Err(e) = crate::commands::mls::sync_mls_groups_now(None).await {
                    eprintln!("[MLS] Post-sync MLS group sync failed: {}", e);
                }

                // After MLS sync completes, check if weekly VACUUM is needed
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                if let Err(e) = db::check_and_vacuum_if_needed(&handle_clone).await {
                    eprintln!("[Maintenance] Weekly VACUUM check failed: {}", e);
                }
            });
        }
    }
}

/// Trigger a deep rescan of all messages from the network
/// Scans backwards in 2-day increments until 30 days of no events found
#[tauri::command]
pub async fn deep_rescan<R: Runtime>(handle: AppHandle<R>) -> Result<bool, String> {
    // Check if a scan is already in progress
    {
        let state = STATE.lock().await;
        if state.is_syncing {
            return Err("Already Scanning! Please wait for the current scan to finish.".to_string());
        }
    }

    // Start a deep rescan by forcing DeepRescan mode
    {
        let mut state = STATE.lock().await;
        let now = Timestamp::now();

        // Set up for deep rescan starting from now
        state.is_syncing = true;
        state.sync_mode = SyncMode::DeepRescan;
        state.sync_empty_iterations = 0;
        state.sync_total_iterations = 0;

        // Start with a 2-day window from now
        let two_days_ago = now.as_secs() - (60 * 60 * 24 * 2);
        state.sync_window_start = two_days_ago;
        state.sync_window_end = now.as_secs();
    }

    // Trigger the first fetch
    fetch_messages(handle, false, None).await;

    Ok(true)
}

// Handler list for this module (for reference):
// - queue_profile_sync
// - queue_chat_profiles_sync
// - refresh_profile_now
// - sync_all_profiles
// - is_scanning
// - fetch_messages
// - deep_rescan
