//! Synchronization Tauri commands.
//!
//! This module handles:
//! - Message fetching and sync (fetch_messages, deep_rescan)
//! - Profile synchronization
//! - Sync status checking

use nostr_sdk::prelude::*;
use tauri::{AppHandle, Emitter, Manager, Runtime};

use crate::{
    db, mls, profile, profile_sync,
    ChatState, ChatType, Profile, SyncMode,
    NOSTR_CLIENT, STATE, WRAPPER_ID_CACHE,
};
use crate::db::save_chat_messages;

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

    profile_sync::queue_profile_sync(npub, sync_priority, force_refresh).await;
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
    profile_sync::refresh_profile_now(npub).await;
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
            let signer = client.signer().await.unwrap();
            let my_public_key = signer.get_public_key().await.unwrap();
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
            let mut needs_integrity_check = false;
            if state.profiles.len() == 1 {
                let profiles = db::get_all_profiles(&handle).await.unwrap();
                // Load our Profile Cache into the state
                state.merge_db_profiles(profiles).await;

                // Spawn background task to cache profile images for offline support
                tokio::spawn(async {
                    profile::cache_all_profile_images().await;
                });

                // Load chats and their messages from database
                let slim_chats_result = db::get_all_chats(&handle).await;
                if let Ok(slim_chats) = slim_chats_result {
                    // Load MLS groups to check for evicted status
                    let mls_groups: Option<Vec<mls::MlsGroupMetadata>> =
                        db::load_mls_groups(&handle).await.ok();

                    // Convert slim chats to full chats and load their messages
                    for slim_chat in slim_chats {
                        let mut chat = slim_chat.to_chat();

                        // Skip MLS group chats that are marked as evicted
                        // MLS group chat IDs are just the group_id (no prefix)
                        if chat.chat_type == ChatType::MlsGroup {
                            if let Some(ref groups) = mls_groups {
                                if let Some(group) = groups.iter().find(|g| g.group_id.as_str() == chat.id()) {
                                    if group.evicted {
                                        println!("[Startup] Skipping evicted MLS group chat: {}", chat.id());
                                        continue; // Skip this chat
                                    }
                                }
                            }
                        }

                        // Load only the last message for preview (optimization: full messages loaded on-demand by frontend)
                        let last_messages_result = db::get_chat_last_messages(&handle, &chat.id(), 1).await;
                        if let Ok(last_messages) = last_messages_result {
                            for message in last_messages {
                                // Check if this message has downloaded attachments (for integrity check)
                                if !needs_integrity_check && message.attachments.iter().any(|att| att.downloaded) {
                                    needs_integrity_check = true;
                                }
                                chat.internal_add_message(message);
                            }
                        } else {
                            eprintln!("Failed to load last message for chat {}: {:?}", chat.id(), last_messages_result);
                        }

                        // Ensure profiles exist for all chat participants
                        for participant in chat.participants() {
                            if state.get_profile(participant).is_none() {
                                // Create a basic profile for the participant
                                let mut profile = Profile::new();
                                profile.id = participant.clone();
                                profile.mine = false; // It's not our profile
                                state.profiles.push(profile);
                            }
                        }

                        // Add chat to state
                        state.chats.push(chat);

                        // Sort the chats by their last received message
                        state.chats.sort_by(|a, b| b.last_message_time().cmp(&a.last_message_time()));
                    }
                } else {
                    eprintln!("Failed to load chats from database: {:?}", slim_chats_result);
                }
            }

            if needs_integrity_check {
                // Clean up empty file attachments first
                cleanup_empty_file_attachments(&handle, &mut state).await;

                // Check integrity without dropping state
                check_attachment_filesystem_integrity(&handle, &mut state).await;

                // Preload ID caches for maximum performance
                if let Err(e) = db::preload_id_caches(&handle).await {
                    eprintln!("[Cache] Failed to preload ID caches: {}", e);
                }

                // Preload wrapper_event_ids for fast duplicate detection during sync
                // Load last 30 days of wrapper_ids to cover typical sync window
                if let Ok(wrapper_ids) = db::load_recent_wrapper_ids(&handle, 30).await {
                    let mut cache = WRAPPER_ID_CACHE.lock().await;
                    *cache = wrapper_ids;
                }

                // Send the state to our frontend to signal finalised init with a full state
                handle.emit("init_finished", serde_json::json!({
                    "profiles": &state.profiles,
                    "chats": &state.chats
                })).unwrap();
            } else {
                // Even if no integrity check needed, still clean up empty files
                cleanup_empty_file_attachments(&handle, &mut state).await;

                // Preload ID caches for maximum performance
                if let Err(e) = db::preload_id_caches(&handle).await {
                    eprintln!("[Cache] Failed to preload ID caches: {}", e);
                }

                // Preload wrapper_event_ids for fast duplicate detection during sync
                // Load last 30 days of wrapper_ids to cover typical sync window
                if let Ok(wrapper_ids) = db::load_recent_wrapper_ids(&handle, 30).await {
                    let mut cache = WRAPPER_ID_CACHE.lock().await;
                    *cache = wrapper_ids;
                }

                // No integrity check needed, send init immediately
                handle.emit("init_finished", serde_json::json!({
                    "profiles": &state.profiles,
                    "chats": &state.chats
                })).unwrap();
            }

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
            cache.shrink_to_fit();
            // Each entry: 64-char hex String (~88 bytes) + HashSet overhead (~48 bytes) ≈ 136 bytes
            println!("[Startup] Sync Complete - Dumped NIP-59 Decryption Cache (~{} KB Memory freed)", (cache_size * 136) / 1024);
        }

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

// ============================================================================
// Helper Functions (internal)
// ============================================================================

/// Removes attachments with empty file hash from all messages
/// Also removes messages that have ONLY corrupted attachments (no content)
/// This cleans up corrupted uploads that resulted in 0-byte files
async fn cleanup_empty_file_attachments<R: Runtime>(
    handle: &AppHandle<R>,
    state: &mut ChatState,
) {
    const EMPTY_FILE_HASH: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    let mut cleaned_count = 0;
    let mut chats_to_update = Vec::new();

    for chat in &mut state.chats {
        let mut chat_had_changes = false;

        // First pass: remove attachments with empty file hash
        for message in &mut chat.messages {
            let original_count = message.attachments.len();

            // Remove attachments with empty file hash in their URL
            message.attachments.retain(|attachment| {
                !attachment.url.contains(EMPTY_FILE_HASH)
            });

            let removed = original_count - message.attachments.len();
            if removed > 0 {
                cleaned_count += removed;
                chat_had_changes = true;
            }
        }

        // Second pass: remove messages that are now empty (no content, no attachments)
        let messages_before = chat.messages.len();
        chat.messages.retain(|message| {
            !message.content.is_empty() || !message.attachments.is_empty()
        });

        if chat.messages.len() < messages_before {
            chat_had_changes = true;
        }

        // If this chat had changes, save all its messages
        if chat_had_changes {
            chats_to_update.push((chat.id(), chat.messages.clone()));
        }
    }

    // Save updated chats to database
    for (chat_id, messages) in chats_to_update {
        if let Err(e) = save_chat_messages(handle.clone(), &chat_id, &messages).await {
            eprintln!("Failed to save chat after cleaning empty attachments: {}", e);
        }
    }

    if cleaned_count > 0 {
        eprintln!("Cleaned up {} empty file attachments", cleaned_count);
    }
}

/// Checks if downloaded attachments still exist on the filesystem
/// Sets downloaded=false for any missing files and updates the database
async fn check_attachment_filesystem_integrity<R: Runtime>(
    handle: &AppHandle<R>,
    state: &mut ChatState,
) {
    let mut total_checked = 0;
    let mut chats_with_updates = std::collections::HashMap::new();

    // Capture the starting timestamp
    let start_time = std::time::Instant::now();

    // First pass: count total attachments to check
    let mut total_attachments = 0;
    for chat in &state.chats {
        for message in &chat.messages {
            for attachment in &message.attachments {
                if attachment.downloaded {
                    total_attachments += 1;
                }
            }
        }
    }

    // Iterate through all chats and their messages with mutable access to update downloaded status
    for (chat_idx, chat) in state.chats.iter_mut().enumerate() {
        let mut updated_messages = Vec::new();

        for message in &mut chat.messages {
            let mut message_updated = false;

            for attachment in &mut message.attachments {
                // Only check attachments that are marked as downloaded
                if attachment.downloaded {
                    total_checked += 1;

                    // Emit progress every 2 attachments or on the last one, but only if process has taken >1 second
                    if (total_checked % 2 == 0 || total_checked == total_attachments) && start_time.elapsed().as_secs() >= 1 {
                        handle.emit("progress_operation", serde_json::json!({
                            "type": "progress",
                            "current": total_checked,
                            "total": total_attachments,
                            "message": "Checking file integrity"
                        })).unwrap();
                    }

                    // Check if the file exists on the filesystem
                    let file_path = std::path::Path::new(&attachment.path);
                    if !file_path.exists() {
                        // File is missing, set downloaded to false
                        attachment.downloaded = false;
                        message_updated = true;
                        attachment.path = String::new();
                    }
                }
            }

            // If any attachment in this message was updated, we need to save the message
            if message_updated {
                updated_messages.push(message.clone());
            }
        }

        // If any messages in this chat were updated, store them for database update
        if !updated_messages.is_empty() {
            chats_with_updates.insert(chat_idx, updated_messages);
        }
    }

    // Update database for any messages with missing attachments
    if !chats_with_updates.is_empty() {
        // Only emit progress if process has taken >1 second
        if start_time.elapsed().as_secs() >= 1 {
            handle.emit("progress_operation", serde_json::json!({
                "type": "progress",
                "total": chats_with_updates.len(),
                "current": 0,
                "message": "Updating database..."
            })).unwrap();
        }

        // Save updated messages for each chat that had changes
        let mut saved_count = 0;
        let total_chats = chats_with_updates.len();
        for (chat_idx, _updated_messages) in chats_with_updates {
            // Since we're iterating over existing indices, we know the chat exists
            let chat = &state.chats[chat_idx];
            let chat_id = chat.id().clone();

            // Save
            let all_messages = &chat.messages;
            if let Err(e) = save_chat_messages(handle.clone(), &chat_id, all_messages).await {
                eprintln!("Failed to update messages after filesystem check: {}", e);
            } else {
                saved_count += 1;
            }

            // Emit progress for database updates, but only if process has taken >1 second
            if ((saved_count) % 5 == 0 || saved_count == total_chats) && start_time.elapsed().as_secs() >= 1 {
                handle.emit("progress_operation", serde_json::json!({
                    "type": "progress",
                    "current": saved_count,
                    "total": total_chats,
                    "message": "Updating database"
                })).unwrap();
            }
        }
    }
}

// Handler list for this module (for reference):
// - queue_profile_sync
// - queue_chat_profiles_sync
// - refresh_profile_now
// - sync_all_profiles
// - is_scanning
// - fetch_messages
// - deep_rescan
