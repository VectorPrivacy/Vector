//! Attachment database operations.
//!
//! This module handles:
//! - AttachmentRef for file deduplication
//! - Building file hash indexes
//! - Paginated message queries
//! - Wrapper event ID tracking for deduplication
//! - Attachment download status updates

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Runtime};
use std::collections::HashMap;

use crate::{Message, Attachment};
use super::{get_chat_id_by_identifier, get_message_views};

/// Lightweight attachment reference for file deduplication
/// Contains only the data needed to reuse an existing upload
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AttachmentRef {
    /// The SHA256 hash of the original file (used as ID)
    pub hash: String,
    /// The message ID containing this attachment
    pub message_id: String,
    /// The chat ID containing this message
    pub chat_id: String,
    /// The encrypted file URL on the server
    pub url: String,
    /// The encryption key
    pub key: String,
    /// The encryption nonce
    pub nonce: String,
    /// The file extension
    pub extension: String,
    /// The encrypted file size
    pub size: u64,
}

/// Build a file hash index from all attachments in the database
/// This is used for file deduplication without loading full message content
/// Returns a HashMap of file_hash -> AttachmentRef
pub async fn build_file_hash_index<R: Runtime>(
    handle: &AppHandle<R>,
) -> Result<HashMap<String, AttachmentRef>, String> {
    use crate::stored_event::event_kind;

    let mut index: HashMap<String, AttachmentRef> = HashMap::new();

    // Use guard - connection returned automatically after query, before heavy processing
    let attachment_data: Vec<(String, String, String)> = {
        let conn = crate::account_manager::get_db_connection_guard(handle)?;

        // Query file attachment events (kind=15) from the events table
        // Attachments are stored in the tags field as JSON
        let mut stmt = conn.prepare(
            "SELECT e.id, c.chat_identifier, e.tags
             FROM events e
             JOIN chats c ON e.chat_id = c.id
             WHERE e.kind = ?1"
        ).map_err(|e| format!("Failed to prepare attachment query: {}", e))?;

        let rows = stmt.query_map(rusqlite::params![event_kind::FILE_ATTACHMENT], |row| {
            Ok((
                row.get::<_, String>(0)?, // event_id (message_id)
                row.get::<_, String>(1)?, // chat_identifier
                row.get::<_, String>(2)?, // tags JSON
            ))
        }).map_err(|e| format!("Failed to query attachments: {}", e))?;

        // Collect immediately to consume the iterator while stmt is still alive
        let result: Result<Vec<_>, _> = rows.collect();
        result.map_err(|e| format!("Failed to collect attachment rows: {}", e))?
        // conn guard dropped here, connection returned to pool
    };

    // Process the collected data
    const EMPTY_FILE_HASH: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    for (message_id, chat_id, tags_json) in attachment_data {
        // Parse tags to find the "attachments" tag
        let tags: Vec<Vec<String>> = serde_json::from_str(&tags_json).unwrap_or_default();

        // Find the attachments tag: ["attachments", "<json>"]
        let attachments_json = tags.iter()
            .find(|tag| tag.first().map(|s| s.as_str()) == Some("attachments"))
            .and_then(|tag| tag.get(1))
            .map(|s| s.as_str())
            .unwrap_or("[]");

        // Parse the attachments JSON
        let attachments: Vec<crate::Attachment> = serde_json::from_str(attachments_json)
            .unwrap_or_default();

        // Add each attachment to the index (skip empty hashes and empty URLs)
        for attachment in attachments {
            if !attachment.id.is_empty()
                && attachment.id != EMPTY_FILE_HASH
                && !attachment.url.is_empty()
            {
                index.insert(attachment.id.clone(), AttachmentRef {
                    hash: attachment.id,
                    message_id: message_id.clone(),
                    chat_id: chat_id.clone(),
                    url: attachment.url,
                    key: attachment.key,
                    nonce: attachment.nonce,
                    extension: attachment.extension,
                    size: attachment.size,
                });
            }
        }
    }

    Ok(index)
}

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
pub async fn get_chat_messages_paginated<R: Runtime>(
    handle: &AppHandle<R>,
    chat_id: &str,
    limit: usize,
    offset: usize,
) -> Result<Vec<Message>, String> {
    // Get integer chat ID
    let chat_int_id = get_chat_id_by_identifier(handle, chat_id)?;
    // Use the events-based message views
    get_message_views(handle, chat_int_id, limit, offset).await
}

/// Get the total message count for a chat
/// This is useful for the frontend to know how many messages exist without loading them all
pub async fn get_chat_message_count<R: Runtime>(
    handle: &AppHandle<R>,
    chat_id: &str,
) -> Result<usize, String> {
    let conn = crate::account_manager::get_db_connection(handle)?;

    // Get integer chat ID from identifier
    let chat_int_id: i64 = conn.query_row(
        "SELECT id FROM chats WHERE chat_identifier = ?1",
        rusqlite::params![chat_id],
        |row| row.get(0)
    ).map_err(|e| format!("Chat not found: {}", e))?;

    // Count message events (kind 9 = MLS chat, kind 14 = DM, kind 15 = file) from events table
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM events WHERE chat_id = ?1 AND kind IN (9, 14, 15)",
        rusqlite::params![chat_int_id],
        |row| row.get(0)
    ).map_err(|e| format!("Failed to count messages: {}", e))?;

    crate::account_manager::return_db_connection(conn);

    Ok(count as usize)
}

/// Get messages around a specific message ID
/// Returns messages from (target - context_before) to the most recent
/// This is used for scrolling to old replied-to messages
pub async fn get_messages_around_id<R: Runtime>(
    handle: &AppHandle<R>,
    chat_id: &str,
    target_message_id: &str,
    context_before: usize,
) -> Result<Vec<Message>, String> {
    let chat_int_id = get_chat_id_by_identifier(handle, chat_id)?;

    // First, find the timestamp of the target message (don't require chat_id match in case of edge cases)
    let target_timestamp: i64 = {
        let conn = crate::account_manager::get_db_connection(handle)?;
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
        crate::account_manager::return_db_connection(conn);
        ts
    };

    // Count how many messages are older than the target in this chat
    let older_count: i64 = {
        let conn = crate::account_manager::get_db_connection(handle)?;
        let count = conn.query_row(
            "SELECT COUNT(*) FROM events WHERE chat_id = ?1 AND kind IN (9, 14, 15) AND created_at < ?2",
            rusqlite::params![chat_int_id, target_timestamp],
            |row| row.get(0)
        ).map_err(|e| format!("Failed to count older messages: {}", e))?;
        crate::account_manager::return_db_connection(conn);
        count
    };

    // Get total message count for this chat
    let total_count = get_chat_message_count(handle, chat_id).await?;

    // Calculate the starting position (from oldest = 0)
    // We want messages from (target - context_before) to the newest
    let start_position = (older_count as usize).saturating_sub(context_before);

    // get_message_views uses ORDER BY created_at DESC, so:
    // - offset 0 = newest message
    // - To get messages from position P to newest with DESC ordering, use offset=0, limit=(total - P)
    let limit = total_count.saturating_sub(start_position);

    // offset = 0 to start from the newest and get all messages back to start_position
    get_message_views(handle, chat_int_id, limit, 0).await
}

/// Check if a message/event exists in the database by its ID
/// This is used to prevent duplicate processing during sync
pub async fn message_exists_in_db<R: Runtime>(
    handle: &AppHandle<R>,
    message_id: &str,
) -> Result<bool, String> {
    // Try to get a database connection - if it fails, we're not using DB mode
    let conn = match crate::account_manager::get_db_connection(handle) {
        Ok(c) => c,
        Err(_) => return Ok(false), // No DB, let in-memory check handle it
    };

    // Check in events table (unified storage)
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM events WHERE id = ?1)",
        rusqlite::params![message_id],
        |row| row.get(0)
    ).map_err(|e| format!("Failed to check event existence: {}", e))?;

    crate::account_manager::return_db_connection(conn);

    Ok(exists)
}

/// Check if a wrapper (giftwrap) event ID exists in the database
/// This allows skipping the expensive unwrap operation for already-processed events
pub async fn wrapper_event_exists<R: Runtime>(
    handle: &AppHandle<R>,
    wrapper_event_id: &str,
) -> Result<bool, String> {
    // Try to get a database connection - if it fails, we're not using DB mode
    let conn = match crate::account_manager::get_db_connection(handle) {
        Ok(c) => c,
        Err(_) => return Ok(false), // No DB, can't check
    };

    // Check in events table (unified storage)
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM events WHERE wrapper_event_id = ?1)",
        rusqlite::params![wrapper_event_id],
        |row| row.get(0)
    ).map_err(|e| format!("Failed to check wrapper event existence: {}", e))?;

    crate::account_manager::return_db_connection(conn);

    Ok(exists)
}

/// Update the wrapper event ID for an existing event
/// This is called when we process an event that was previously stored without its wrapper ID
/// Returns: Ok(true) if updated, Ok(false) if event already had a wrapper_id (duplicate giftwrap)
pub async fn update_wrapper_event_id<R: Runtime>(
    handle: &AppHandle<R>,
    event_id: &str,
    wrapper_event_id: &str,
) -> Result<bool, String> {
    // Try to get a database connection - if it fails, we're not using DB mode
    let conn = match crate::account_manager::get_db_connection(handle) {
        Ok(c) => c,
        Err(_) => return Ok(false), // No DB, nothing to update
    };

    // Update in events table (unified storage)
    let rows_updated = match conn.execute(
        "UPDATE events SET wrapper_event_id = ?1 WHERE id = ?2 AND (wrapper_event_id IS NULL OR wrapper_event_id = '')",
        rusqlite::params![wrapper_event_id, event_id],
    ) {
        Ok(n) => n,
        Err(e) => {
            crate::account_manager::return_db_connection(conn);
            return Err(format!("Failed to update wrapper event ID: {}", e));
        }
    };

    crate::account_manager::return_db_connection(conn);

    // Returns true if backfill succeeded, false if event already has a wrapper_id (duplicate giftwrap)
    Ok(rows_updated > 0)
}

/// Load recent wrapper_event_ids into a HashSet for fast duplicate detection
/// This preloads wrapper_ids from the last N days to avoid SQL queries during sync
pub async fn load_recent_wrapper_ids<R: Runtime>(
    handle: &AppHandle<R>,
    days: u64,
) -> Result<std::collections::HashSet<String>, String> {
    let mut wrapper_ids = std::collections::HashSet::new();

    // Try to get a database connection - if it fails, we're not using DB mode
    let conn = match crate::account_manager::get_db_connection(handle) {
        Ok(c) => c,
        Err(_) => return Ok(wrapper_ids), // No DB, return empty set
    };

    // Calculate timestamp for N days ago (in seconds, matching events.created_at)
    let cutoff_secs = (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs())
        .saturating_sub(days * 24 * 60 * 60);

    // Query all wrapper_event_ids from recent events
    let result: Result<Vec<String>, _> = {
        let mut stmt = conn.prepare(
            "SELECT wrapper_event_id FROM events
             WHERE wrapper_event_id IS NOT NULL
             AND wrapper_event_id != ''
             AND created_at >= ?1"
        ).map_err(|e| format!("Failed to prepare wrapper_id query: {}", e))?;

        let rows = stmt.query_map(rusqlite::params![cutoff_secs as i64], |row| {
            row.get::<_, String>(0)
        }).map_err(|e| format!("Failed to query wrapper_ids: {}", e))?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("Failed to collect wrapper_ids: {}", e))
    };

    crate::account_manager::return_db_connection(conn);

    match result {
        Ok(ids) => {
            for id in ids {
                wrapper_ids.insert(id);
            }
            Ok(wrapper_ids)
        }
        Err(_) => {
            Ok(wrapper_ids) // Return empty set on error, will fall back to DB queries
        }
    }
}

/// Update the downloaded status of an attachment in the database
pub fn update_attachment_downloaded_status<R: Runtime>(
    handle: &AppHandle<R>,
    _chat_id: &str,  // No longer needed - we query by event ID directly
    msg_id: &str,
    attachment_id: &str,
    downloaded: bool,
    path: &str,
) -> Result<(), String> {
    let conn = crate::account_manager::get_db_connection_guard(handle)?;

    // Get the current tags JSON from the events table
    let tags_json: String = conn.query_row(
        "SELECT tags FROM events WHERE id = ?1",
        rusqlite::params![msg_id],
        |row| row.get(0)
    ).map_err(|e| format!("Event not found: {}", e))?;

    // Parse the tags
    let mut tags: Vec<Vec<String>> = serde_json::from_str(&tags_json).unwrap_or_default();

    // Find the "attachments" tag
    let attachments_tag_idx = tags.iter().position(|tag| {
        tag.first().map(|s| s.as_str()) == Some("attachments")
    });

    let attachments_json = attachments_tag_idx
        .and_then(|idx| tags.get(idx))
        .and_then(|tag| tag.get(1))
        .map(|s| s.as_str())
        .unwrap_or("[]");

    // Parse and update the attachment
    let mut attachments: Vec<Attachment> = serde_json::from_str(attachments_json).unwrap_or_default();

    if let Some(att) = attachments.iter_mut().find(|a| a.id == attachment_id) {
        att.downloaded = downloaded;
        att.downloading = false;
        att.path = path.to_string();
    } else {
        return Err("Attachment not found in event".to_string());
    }

    // Serialize the updated attachments back to JSON
    let updated_attachments_json = serde_json::to_string(&attachments)
        .map_err(|e| format!("Failed to serialize attachments: {}", e))?;

    // Update the tags array - either update existing "attachments" tag or add new one
    if let Some(idx) = attachments_tag_idx {
        tags[idx] = vec!["attachments".to_string(), updated_attachments_json];
    } else {
        tags.push(vec!["attachments".to_string(), updated_attachments_json]);
    }

    // Serialize the tags back to JSON
    let updated_tags_json = serde_json::to_string(&tags)
        .map_err(|e| format!("Failed to serialize tags: {}", e))?;

    // Update the event in the database
    conn.execute(
        "UPDATE events SET tags = ?1 WHERE id = ?2",
        rusqlite::params![updated_tags_json, msg_id],
    ).map_err(|e| format!("Failed to update event: {}", e))?;

    Ok(())
}
