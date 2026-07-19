//! Message fetching and caching Tauri commands.
//!
//! This module handles:
//! - Paginated message retrieval from database
//! - Message view composition from events
//! - System events (member joined/left)
//! - Backend cache synchronization
//! - File hash indexing for deduplication

use tauri::{AppHandle, Manager, Runtime};

use crate::{db, STATE, Message};

// ============================================================================
// Message Retrieval Commands
// ============================================================================

/// Get paginated messages for a chat directly from the database
/// Also adds the messages to the backend state for cache synchronization
#[tauri::command]
pub async fn get_chat_messages_paginated<R: Runtime>(
    _handle: AppHandle<R>,
    chat_id: String,
    limit: usize,
    offset: usize,
) -> Result<Vec<Message>, String> {
    // Load messages from database
    let messages = db::get_chat_messages_paginated(&chat_id, limit, offset).await?;

    // Also add these messages to the backend state for cache synchronization
    // This ensures operations like fetch_msg_metadata can find the messages
    // Clone for return, move originals to batch (zero-copy in batch insert)
    let messages_for_return = messages.clone();

    if !messages.is_empty() {
        #[cfg(debug_assertions)]
        let start = std::time::Instant::now();
        let mut state = STATE.lock().await;

        // Use batch insert with zero-copy (moves the messages)
        let added = state.add_messages_to_chat_batch(&chat_id, messages);

        #[cfg(debug_assertions)]
        if added > 0 {
            state.cache_stats.insert_count += added as u64;
            state.cache_stats.record_insert(start.elapsed());
            let chats_clone = state.chats.clone();
            state.cache_stats.update_from_chats(&chats_clone);
            println!("[CacheStats] paginated load: added {} msgs in {:?}", added, start.elapsed());
            state.cache_stats.log();
        }
    }

    Ok(messages_for_return)
}

/// Get the total message count for a chat
#[tauri::command]
pub async fn get_chat_message_count<R: Runtime>(
    _handle: AppHandle<R>,
    chat_id: String,
) -> Result<usize, String> {
    db::get_chat_message_count(&chat_id).await
}

/// Get message views (composed from events table) for a chat
/// This is the new event-based approach that computes reactions from flat events
#[tauri::command]
pub async fn get_message_views<R: Runtime>(
    _handle: AppHandle<R>,
    chat_id: String,
    limit: usize,
    offset: usize,
) -> Result<Vec<Message>, String> {
    // Convert chat identifier to database ID
    let chat_int_id = db::get_chat_id_by_identifier(&chat_id)?;

    // Get materialized message views from events
    let messages = db::get_message_views(chat_int_id, limit, offset).await?;

    // Sync to backend state for cache compatibility (batch insert for efficiency)
    // Clone for return, move originals to batch (zero-copy in batch insert)
    let messages_for_return = messages.clone();

    if !messages.is_empty() {
        #[cfg(debug_assertions)]
        let start = std::time::Instant::now();
        let mut state = STATE.lock().await;

        // Use batch insert with zero-copy (moves the messages)
        let added = state.add_messages_to_chat_batch(&chat_id, messages);

        #[cfg(debug_assertions)]
        if added > 0 {
            state.cache_stats.insert_count += added as u64;
            state.cache_stats.record_insert(start.elapsed());
            let chats_clone = state.chats.clone();
            state.cache_stats.update_from_chats(&chats_clone);
            println!("[CacheStats] message_views load: added {} msgs in {:?}", added, start.elapsed());
            state.cache_stats.log();
        }
    }

    Ok(messages_for_return)
}

/// Get messages around a specific message ID (for scrolling to replied-to messages)
/// Loads messages from (target - context_before) to the most recent
#[tauri::command]
pub async fn get_messages_around_id<R: Runtime>(
    _handle: AppHandle<R>,
    chat_id: String,
    target_message_id: String,
    context_before: usize,
) -> Result<Vec<Message>, String> {
    // Snapshot the session before the DB await so a mid-load account swap can't write the prior
    // account's messages (and an auto-created chat) into the new account's STATE.
    let session = vector_core::state::SessionGuard::capture();
    let messages = db::get_messages_around_id(&chat_id, &target_message_id, context_before).await?;

    // Sync to backend state so fetch_msg_metadata and other functions can find these messages
    // Clone for return, move originals to batch (zero-copy in batch insert)
    let messages_for_return = messages.clone();

    if !messages.is_empty() && session.is_valid() {
        #[cfg(debug_assertions)]
        let start = std::time::Instant::now();
        let mut state = STATE.lock().await;

        // Use batch insert with zero-copy (moves the messages)
        let added = state.add_messages_to_chat_batch(&chat_id, messages);

        #[cfg(debug_assertions)]
        if added > 0 {
            state.cache_stats.insert_count += added as u64;
            state.cache_stats.record_insert(start.elapsed());
            let chats_clone = state.chats.clone();
            state.cache_stats.update_from_chats(&chats_clone);
            println!("[CacheStats] messages_around load: added {} msgs in {:?}", added, start.elapsed());
            state.cache_stats.log();
        }
    }

    Ok(messages_for_return)
}

/// Anchored (random-access) message window: `before` messages up to and
/// including `anchor_id`, plus `after` messages newer than it. O(window) load
/// regardless of how deep the anchor sits — used by jump-to-unread.
/// Errs if the anchor id isn't in the DB so the caller can fall back.
#[tauri::command]
pub async fn get_messages_around<R: Runtime>(
    _handle: AppHandle<R>,
    chat_id: String,
    anchor_id: String,
    before: usize,
    after: usize,
) -> Result<Vec<Message>, String> {
    // Snapshot the session before the DB await: a swap during the load must not write account A's
    // messages into account B's STATE (add_messages_to_chat_batch even creates the chat if missing).
    let session = vector_core::state::SessionGuard::capture();
    // Clamp the window. Trusted callers pass <=51; the cap bounds a hostile-frontend DoS (each row is
    // decrypted + composed + cloned into STATE) without affecting any real use.
    let before = before.min(512);
    let after = after.min(512);
    let messages = db::get_messages_around(&chat_id, &anchor_id, before, after).await?;

    // Sync to backend state so fetch_msg_metadata and friends can find these messages.
    let messages_for_return = messages.clone();

    if !messages.is_empty() && session.is_valid() {
        let mut state = STATE.lock().await;
        state.add_messages_to_chat_batch(&chat_id, messages);
    }

    Ok(messages_for_return)
}

// ============================================================================
// System Events Commands
// ============================================================================

/// Get system events (member joined/left, etc.) for a chat
/// Returns events in frontend-friendly format for rendering as timestamps
#[tauri::command]
pub async fn get_system_events<R: Runtime>(
    _handle: AppHandle<R>,
    conversation_id: String,
) -> Result<Vec<serde_json::Value>, String> {
    let events = db::get_system_events_for_chat(&conversation_id).await?;

    // Convert StoredEvents to frontend-friendly format
    let system_events: Vec<serde_json::Value> = events.iter().map(|event| {
        // Extract event type from tags (stored as integer)
        let event_type: u8 = event.tags.iter()
            .find(|tag| tag.len() >= 2 && tag[0] == "event-type")
            .and_then(|tag| tag[1].parse().ok())
            .unwrap_or(255); // 255 = unknown

        // Extract member npub from tags
        let member_npub = event.tags.iter()
            .find(|tag| tag.len() >= 2 && tag[0] == "member")
            .map(|tag| tag[1].clone())
            .unwrap_or_default();

        serde_json::json!({
            "id": event.id,
            "event_type": event_type,
            "content": event.content,
            "member_npub": member_npub,
            "at": event.created_at * 1000, // Convert to milliseconds for JS
        })
    }).collect();

    Ok(system_events)
}

// ============================================================================
// Cache Management Commands
// ============================================================================

/// Evict messages from the backend cache for a specific chat
/// Called by frontend when LRU eviction occurs to keep caches in sync
#[tauri::command]
pub async fn evict_chat_messages(chat_id: String, keep_count: usize) -> Result<(), String> {
    let mut state = STATE.lock().await;
    if let Some(chat) = state.chats.iter_mut().find(|c| c.id == chat_id) {
        let total = chat.message_count();
        if total > keep_count {
            // Keep only the last `keep_count` messages (most recent)
            let drain_count = total - keep_count;
            chat.messages.drain(0..drain_count);
            chat.messages.rebuild_index();
        }
    }
    Ok(())
}

// ============================================================================
// Unread Count Commands
// ============================================================================

/// Seed the in-RAM unread cache from the DB once per login. The full-scan `unread_counts` query runs
/// here and nowhere on the per-message badge path — after seeding, the badge is a pure RAM fold.
async fn ensure_unread_seeded() {
    if STATE.lock().await.unread_seeded {
        return;
    }
    // The DB read straddles an await; a swap could land, so guard the seed against writing account
    // A's counts into account B's freshly-swapped state.
    let session = vector_core::state::SessionGuard::capture();
    let counts = crate::db::unread_counts().await.unwrap_or_default();
    let mut state = STATE.lock().await;
    if !session.is_valid() {
        return;
    }
    if !state.unread_seeded {
        state.unread_seed(counts);
    }
}

/// Recompute ONE chat's unread straight from the DB (indexed single-chat query) and store it in the
/// cache. Used where an O(1) delta would be unsafe: a delete, a mark-unread retreat, or a batch
/// backfill that adds messages outside the live inbound path.
pub async fn reconcile_chat_unread(chat_id: &str) {
    let session = vector_core::state::SessionGuard::capture();
    let count = vector_core::db::events::unread_count_for_chat(chat_id).await.unwrap_or(0);
    let mut state = STATE.lock().await;
    if !session.is_valid() {
        return;
    }
    // Before the seed the whole map is (re)built from the DB anyway, so only patch a live cache.
    if state.unread_seeded {
        state.unread_set(chat_id, count);
    }
}

/// Update the window badge/overlay with the current unread message count
/// Returns the unread message count
#[tauri::command]
pub async fn update_unread_counter<R: Runtime>(handle: AppHandle<R>) -> u32 {
    // Fold the in-RAM unread cache (seeded once from the DB, maintained incrementally), applying the
    // cheap muted/blocked filters. No per-message DB scan: the heavy query ran once at seed time.
    ensure_unread_seeded().await;
    let unread_count = {
        let state = STATE.lock().await;
        state.sum_unread()
    };

    // Get the main window (only used on desktop for badge handling)
    #[allow(unused_variables)]
    if let Some(window) = handle.get_webview_window("main") {
        if unread_count > 0 {
            // Platform-specific badge/overlay handling
            #[cfg(target_os = "windows")]
            {
                // On Windows, use overlay icon instead of badge
                let icon = tauri::include_image!("./icons/icon_badge_notification.png");
                let _ = window.set_overlay_icon(Some(icon));
            }

            #[cfg(not(any(target_os = "windows", target_os = "ios", target_os = "android")))]
            {
                // On macOS, Linux, etc. use the badge if available
                let _ = window.set_badge_count(Some(unread_count as i64));
            }
        } else {
            // Clear badge/overlay when no unread messages
            #[cfg(target_os = "windows")]
            {
                // Remove the overlay icon on Windows
                let _ = window.set_overlay_icon(None);
            }

            #[cfg(not(any(target_os = "windows", target_os = "ios", target_os = "android")))]
            {
                // Clear the badge on other platforms
                let _ = window.set_badge_count(None);
            }
        }
    }

    unread_count
}

/// Per-chat unread counts straight from the DB (`chat_identifier` → count), for seeding the
/// chatlist badges at boot — correct even when only the last message per chat is in RAM. Chats
/// with 0 unread are omitted (the frontend treats a missing entry as 0).
#[tauri::command]
pub async fn get_unread_counts() -> std::collections::HashMap<String, u32> {
    // The frontend's authoritative per-chat badge fetch (boot + refresh). Reseed the cache from the
    // DB here so this call also HEALS any incremental drift; the per-message badge path stays a pure
    // in-RAM fold between these refreshes.
    let session = vector_core::state::SessionGuard::capture();
    let counts = crate::db::unread_counts().await.unwrap_or_default();
    let mut state = STATE.lock().await;
    if session.is_valid() {
        state.unread_seed(counts);
    }
    state.unread_snapshot()
}

/// Tell the backend which chat the user is actively watching, so inbound
/// messages in that chat auto-mark as read on arrival (no dock-badge bump,
/// no race with the FE's own `markAsRead`). Frontend sends `chat_id=None`
/// when the chat closes, the window blurs, or the user scrolls off-bottom.
#[tauri::command]
pub fn set_active_chat(chat_id: Option<String>) {
    vector_core::state::set_active_chat(chat_id);
}

// Handler list for this module (for reference):
// - get_chat_messages_paginated
// - get_chat_message_count
// - get_message_views
// - get_messages_around_id
// - get_system_events
// - evict_chat_messages
// - update_unread_counter
// - set_active_chat
