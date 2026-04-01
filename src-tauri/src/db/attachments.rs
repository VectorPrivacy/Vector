//! Attachment database operations.
//!
//! This module handles:
//! - Paginated message queries
//! - Attachment download status updates

use crate::{Message, Attachment};
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

/// Update the downloaded status of an attachment in the database
pub fn update_attachment_downloaded_status(
    _chat_id: &str,  // No longer needed - we query by event ID directly
    msg_id: &str,
    attachment_id: &str,
    downloaded: bool,
    path: &str,
) -> Result<(), String> {
    let conn = crate::account_manager::get_db_connection_guard_static()?;

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

/// Backfill all other messages in the database that share the same attachment hash.
/// When one message's attachment is downloaded, other messages with the same file hash
/// should also be marked as downloaded with the same path, since they share the same file.
pub fn backfill_attachment_downloaded_status(
    attachment_hash: &str,
    downloaded: bool,
    path: &str,
    exclude_msg_id: &str,
) -> Result<usize, String> {
    let conn = crate::account_manager::get_db_connection_guard_static()?;

    // Find all events that contain this attachment hash in their tags
    let mut stmt = conn.prepare(
        "SELECT id, tags FROM events WHERE kind = 15 AND id != ?1 AND tags LIKE ?2 ESCAPE '\\'"
    ).map_err(|e| format!("Failed to prepare backfill query: {}", e))?;

    let escaped_hash = attachment_hash.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_");
    let pattern = format!("%{}%", escaped_hash);
    let rows: Vec<(String, String)> = stmt.query_map(
        rusqlite::params![exclude_msg_id, pattern],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    ).map_err(|e| format!("Failed to query for backfill: {}", e))?
    .filter_map(|r| r.ok())
    .collect();

    let mut updated_count = 0;

    for (event_id, tags_json) in rows {
        let mut tags: Vec<Vec<String>> = match serde_json::from_str(&tags_json) {
            Ok(t) => t,
            Err(_) => continue,
        };

        let attachments_tag_idx = tags.iter().position(|tag| {
            tag.first().map(|s| s.as_str()) == Some("attachments")
        });

        let attachments_json = attachments_tag_idx
            .and_then(|idx| tags.get(idx))
            .and_then(|tag| tag.get(1))
            .map(|s| s.as_str())
            .unwrap_or("[]");

        let mut attachments: Vec<Attachment> = match serde_json::from_str(attachments_json) {
            Ok(a) => a,
            Err(_) => continue,
        };

        let mut modified = false;
        for att in attachments.iter_mut() {
            if att.id == attachment_hash && !att.downloaded {
                att.downloaded = downloaded;
                att.downloading = false;
                att.path = path.to_string();
                modified = true;
            }
        }

        if !modified {
            continue;
        }

        let updated_attachments_json = match serde_json::to_string(&attachments) {
            Ok(j) => j,
            Err(_) => continue,
        };

        if let Some(idx) = attachments_tag_idx {
            tags[idx] = vec!["attachments".to_string(), updated_attachments_json];
        }

        let updated_tags_json = match serde_json::to_string(&tags) {
            Ok(j) => j,
            Err(_) => continue,
        };

        match conn.execute(
            "UPDATE events SET tags = ?1 WHERE id = ?2",
            rusqlite::params![updated_tags_json, event_id],
        ) {
            Ok(_) => updated_count += 1,
            Err(e) => eprintln!("Failed to backfill attachment status for event {}: {}", event_id, e),
        }
    }

    Ok(updated_count)
}

/// Check all downloaded attachments in the database for missing files.
/// Updates the database directly for any files that no longer exist.
/// Returns (total_checked, missing_count, elapsed_time).
pub async fn check_downloaded_attachments_integrity(
) -> Result<(usize, usize, std::time::Duration), String> {
    let start = std::time::Instant::now();

    // Query all events with file attachments that have downloaded files
    // Using JSON extract to filter only events with downloaded attachments
    let events_with_downloaded: Vec<(String, String)> = {
        let conn = crate::account_manager::get_db_connection_guard_static()?;

        // Query all file attachment events - we'll filter in Rust for downloaded=true
        // This is more reliable than JSON filtering in SQLite
        let mut stmt = conn.prepare(
            "SELECT id, tags FROM events WHERE kind = 15"
        ).map_err(|e| format!("Failed to prepare integrity query: {}", e))?;

        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        }).map_err(|e| format!("Failed to query attachments: {}", e))?;

        rows.filter_map(|r| r.ok()).collect()
    };

    let mut total_checked = 0;
    let mut missing_count = 0;
    let mut updates: Vec<(String, String)> = Vec::new(); // (event_id, updated_tags_json)

    for (event_id, tags_json) in events_with_downloaded {
        let mut tags: Vec<Vec<String>> = serde_json::from_str(&tags_json).unwrap_or_default();

        let attachments_tag_idx = tags.iter().position(|tag| {
            tag.first().map(|s| s.as_str()) == Some("attachments")
        });

        let Some(idx) = attachments_tag_idx else { continue };
        let Some(attachments_json) = tags.get(idx).and_then(|t| t.get(1)) else { continue };

        let mut attachments: Vec<crate::Attachment> = serde_json::from_str(attachments_json)
            .unwrap_or_default();

        let mut modified = false;
        for att in &mut attachments {
            if att.downloaded && !att.path.is_empty() {
                total_checked += 1;
                if !std::path::Path::new(&att.path).exists() {
                    att.downloaded = false;
                    att.path = String::new();
                    modified = true;
                    missing_count += 1;
                }
            }
        }

        if modified {
            let updated_attachments_json = serde_json::to_string(&attachments)
                .map_err(|e| format!("Failed to serialize: {}", e))?;
            tags[idx] = vec!["attachments".to_string(), updated_attachments_json];
            let updated_tags_json = serde_json::to_string(&tags)
                .map_err(|e| format!("Failed to serialize tags: {}", e))?;
            updates.push((event_id, updated_tags_json));
        }
    }

    // Batch update all modified events
    if !updates.is_empty() {
        let conn = crate::account_manager::get_db_connection_guard_static()?;
        for (event_id, tags_json) in updates {
            conn.execute(
                "UPDATE events SET tags = ?1 WHERE id = ?2",
                rusqlite::params![tags_json, event_id],
            ).ok(); // Ignore individual errors
        }
    }

    let elapsed = start.elapsed();
    println!(
        "[Integrity] Checked {} downloaded attachments in {:?}, {} missing files updated",
        total_checked, elapsed, missing_count
    );

    Ok((total_checked, missing_count, elapsed))
}
