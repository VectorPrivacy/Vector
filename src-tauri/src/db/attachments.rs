//! Attachment database operations.
//!
//! This module handles:
//! - Paginated message queries
//! - Attachment download status updates

use crate::Message;
use super::{get_chat_id_by_identifier, get_message_views};

/// Get paginated messages for a chat (newest first, with offset)
/// This allows loading messages on-demand instead of all at once
///
/// Parameters:
/// - chat_id: The chat identifier (npub for DMs, group_id for groups)
/// - limit: Maximum number of messages to return
/// - offset: Number of messages to skip from the newest
///
/// Returns messages in chronological order (oldest first within the batch)
/// NOTE: This now uses the events table via get_message_views
pub async fn get_chat_messages_paginated(
    chat_id: &str,
    limit: usize,
    offset: usize,
) -> Result<Vec<Message>, String> {
    // Get integer chat ID
    let chat_int_id = get_chat_id_by_identifier(chat_id)?;
    // Use the events-based message views
    get_message_views(chat_int_id, limit, offset).await
}

// Moved to vector-core: message_exists_in_db, wrapper_event_exists, update_wrapper_event_id, load_recent_wrapper_ids, save/load/update wrappers, load_negentropy_items, get_chat_message_count

/// Get messages around a specific message ID
/// Returns messages from (target - context_before) to the most recent
/// This is used for scrolling to old replied-to messages
pub async fn get_messages_around_id(
    chat_id: &str,
    target_message_id: &str,
    context_before: usize,
) -> Result<Vec<Message>, String> {
    let chat_int_id = get_chat_id_by_identifier(chat_id)?;

    // First, find the timestamp of the target message (don't require chat_id match in case of edge cases)
    let target_timestamp: i64 = {
        let conn = crate::account_manager::get_db_connection_guard_static()?;
        // Try to find in the specified chat first
        let ts_result = conn.query_row(
            "SELECT created_at FROM events WHERE id = ?1 AND chat_id = ?2",
            rusqlite::params![target_message_id, chat_int_id],
            |row| row.get(0)
        );

        let ts = match ts_result {
            Ok(t) => t,
            Err(_) => {
                // Message not found in specified chat, try finding it anywhere
                conn.query_row(
                    "SELECT created_at FROM events WHERE id = ?1",
                    rusqlite::params![target_message_id],
                    |row| row.get(0)
                ).map_err(|e| format!("Target message not found in any chat: {}", e))?
            }
        };
    
        ts
    };

    // Count how many messages are older than the target in this chat
    let older_count: i64 = {
        let conn = crate::account_manager::get_db_connection_guard_static()?;
        let count = conn.query_row(
            "SELECT COUNT(*) FROM events WHERE chat_id = ?1 AND kind IN (9, 14, 15) AND created_at < ?2",
            rusqlite::params![chat_int_id, target_timestamp],
            |row| row.get(0)
        ).map_err(|e| format!("Failed to count older messages: {}", e))?;
    
        count
    };

    // Get total message count for this chat
    let total_count = super::get_chat_message_count(chat_id).await?;

    // Calculate the starting position (from oldest = 0)
    // We want messages from (target - context_before) to the newest
    let start_position = (older_count as usize).saturating_sub(context_before);

    // get_message_views uses ORDER BY created_at DESC, so:
    // - offset 0 = newest message
    // - To get messages from position P to newest with DESC ordering, use offset=0, limit=(total - P)
    let limit = total_count.saturating_sub(start_position);

    // offset = 0 to start from the newest and get all messages back to start_position
    get_message_views(chat_int_id, limit, 0).await
}

/// Update the downloaded status of an attachment — a single-row UPDATE keyed by (event id, hash)
/// against the dedicated attachments table.
pub fn update_attachment_downloaded_status(
    _chat_id: &str,  // legacy param; the table is keyed by event id directly
    msg_id: &str,
    attachment_id: &str,
    downloaded: bool,
    path: &str,
) -> Result<(), String> {
    vector_core::db::attachments::set_attachment_downloaded(msg_id, attachment_id, downloaded, path)
}

/// Mark every OTHER message sharing this file hash as downloaded to the same path (download-sharing).
/// Now an indexed `WHERE hash = ?` update instead of a `LIKE '%hash%'` scan. Returns the affected
/// event ids so the caller can reconcile in-memory STATE.
pub fn backfill_attachment_downloaded_status(
    attachment_hash: &str,
    _downloaded: bool, // always a download-share (mark downloaded); kept for signature stability
    path: &str,
    exclude_msg_id: &str,
) -> Result<Vec<String>, String> {
    vector_core::db::attachments::backfill_downloaded_by_hash(attachment_hash, path, exclude_msg_id)
}

/// Check all downloaded attachments in the database for missing files.
/// Updates the database directly for any files that no longer exist.
/// Returns (total_checked, missing_count, elapsed_time, affected_event_ids). The caller reconciles
/// the affected ids against in-memory STATE — boot preloads messages BEFORE this runs, so a missing
/// file on a preloaded (e.g. latest) message would otherwise stay a broken image until a full reload.
pub async fn check_downloaded_attachments_integrity(
) -> Result<(usize, usize, std::time::Duration, Vec<String>), String> {
    let start = std::time::Instant::now();

    // One indexed read of the downloaded attachments (WHERE downloaded=1) — no per-event JSON parse.
    let downloaded = vector_core::db::attachments::downloaded_attachment_paths()?;

    let mut total_checked = 0;
    let mut missing_count = 0;
    let mut affected_ids: Vec<String> = Vec::new();
    for (event_id, hash, path) in downloaded {
        total_checked += 1;
        if !std::path::Path::new(&path).exists() {
            vector_core::db::attachments::clear_attachment_download(&event_id, &hash).ok();
            missing_count += 1;
            if !affected_ids.contains(&event_id) {
                affected_ids.push(event_id);
            }
        }
    }

    let elapsed = start.elapsed();
    println!(
        "[Integrity] Checked {} downloaded attachments in {:?}, {} missing files updated",
        total_checked, elapsed, missing_count
    );

    Ok((total_checked, missing_count, elapsed, affected_ids))
}
