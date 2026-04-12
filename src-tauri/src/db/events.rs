//! Event database operations.
//!
//! This module handles:
//! - Saving events to the flat event architecture
//! - Reactions, edits, system events
//! - Message views with reactions composed
//! - Reply context population

use std::collections::HashMap;

use crate::{Message, Attachment, Reaction};
use crate::message::EditEntry;
use crate::crypto::maybe_decrypt;
use crate::stored_event::{StoredEvent, event_kind};

// Delegates to vector-core
pub use vector_core::db::events::{
    save_event, event_exists, save_reaction_event,
    save_pivx_payment_event, save_edit_event, delete_event,
    save_system_event_by_id,
    get_events, get_related_events, populate_reply_context,
    get_reply_contexts,
};

// ============================================================================
// Event Storage Functions (Flat Event-Based Architecture)
// ============================================================================

// save_event: moved to vector-core::db::events (re-exported above)

// save_pivx_payment_event: moved to vector-core (re-exported above)

// save_system_event_by_id: moved to vector-core (re-exported above)

/// Get PIVX payment events for a chat
///
/// Returns all PIVX payment events (kind 30078 with d=pivx-payment tag) for a conversation.
// get_pivx_payments_for_chat: moved to vector-core (re-exported above)

// get_system_events_for_chat: moved to vector-core (re-exported above)

// save_reaction_event: moved to vector-core (re-exported above)

// save_edit_event, delete_event: moved to vector-core (re-exported above)

// event_exists: moved to vector-core (re-exported above)

/// Get events for a chat with pagination
///
/// Returns events ordered by created_at descending (newest first).
/// Optionally filter by event kinds.
// get_events + parse_event_row: moved to vector-core (re-exported above)

// get_related_events, get_reply_contexts, populate_reply_context: moved to vector-core (re-exported above)

/// Get message events with their reactions composed (materialized view)
///
/// This function performs a single efficient query to get messages and their
/// related events, then composes them into Message structs for the frontend.
pub async fn get_message_views(
    chat_id: i64,
    limit: usize,
    offset: usize,
) -> Result<Vec<Message>, String> {
    // Step 1: Get message events (kind 9, 14, and 15)
    let message_kinds = [event_kind::MLS_CHAT_MESSAGE, event_kind::PRIVATE_DIRECT_MESSAGE, event_kind::FILE_ATTACHMENT];
    let message_events = get_events(chat_id, Some(&message_kinds), limit, offset).await?;

    if message_events.is_empty() {
        return Ok(Vec::new());
    }

    // Step 2: Get related events (reactions, edits) for these messages
    let message_ids: Vec<String> = message_events.iter().map(|e| e.id.clone()).collect();
    let related_events = get_related_events(&message_ids).await?;

    // Group reactions and edits by message ID
    let mut reactions_by_msg: HashMap<String, Vec<Reaction>> = HashMap::new();
    let mut edits_by_msg: HashMap<String, Vec<(u64, String)>> = HashMap::new(); // (timestamp, content)

    for event in related_events {
        if let Some(ref_id) = &event.reference_id {
            match event.kind {
                k if k == event_kind::REACTION => {
                    let reaction = Reaction {
                        id: event.id.clone(),
                        reference_id: ref_id.clone(),
                        author_id: event.npub.clone().unwrap_or_default(),
                        emoji: event.content.clone(),
                    };
                    reactions_by_msg.entry(ref_id.clone()).or_default().push(reaction);
                }
                k if k == event_kind::MESSAGE_EDIT => {
                    // Edit content is encrypted, decrypt it here
                    let decrypted_content = maybe_decrypt(event.content.clone()).await
                        .unwrap_or_else(|_| event.content.clone());
                    let timestamp_ms = event.created_at * 1000; // Convert to ms
                    edits_by_msg.entry(ref_id.clone()).or_default().push((timestamp_ms, decrypted_content));
                }
                _ => {}
            }
        }
    }

    // Sort edits by timestamp (chronologically)
    for edits in edits_by_msg.values_mut() {
        edits.sort_by_key(|(ts, _)| *ts);
    }

    // Step 3: Parse attachments from event tags OR fall back to messages table
    // New events have attachments in tags, old migrated events need messages table lookup
    let mut attachments_by_msg: HashMap<String, Vec<Attachment>> = HashMap::new();
    let mut events_needing_legacy_lookup: Vec<String> = Vec::new();

    for event in &message_events {
        // Check for attachments in FILE_ATTACHMENT (kind 15) and MLS_CHAT_MESSAGE (kind 9) events
        // MLS groups use MIP-04 imeta attachments embedded in kind 9 messages
        if event.kind != event_kind::FILE_ATTACHMENT && event.kind != event_kind::MLS_CHAT_MESSAGE {
            continue;
        }

        // Try to parse attachments from the "attachments" tag (new events)
        if let Some(attachments_json) = event.get_tag("attachments") {
            if let Ok(attachments) = serde_json::from_str::<Vec<Attachment>>(attachments_json) {
                if !attachments.is_empty() {
                    attachments_by_msg.insert(event.id.clone(), attachments);
                    continue;
                }
            }
        }

        // No attachments tag found for kind 15 - this is an old migrated event, need legacy lookup
        if event.kind == event_kind::FILE_ATTACHMENT {
            events_needing_legacy_lookup.push(event.id.clone());
        }
    }

    // Fall back to messages table for old migrated events without attachments tag
    // NOTE: This is a legacy fallback - the messages table may have been dropped
    if !events_needing_legacy_lookup.is_empty() {
        let conn = crate::account_manager::get_db_connection_guard_static()?;

        // Check if messages table exists before querying it
        let has_messages_table: bool = conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='messages'",
            [],
            |row| row.get::<_, i32>(0)
        ).map(|count| count > 0).unwrap_or(false);

        if has_messages_table {
            for msg_id in &events_needing_legacy_lookup {
                if let Ok(attachments_json) = conn.query_row::<String, _, _>(
                    "SELECT attachments FROM messages WHERE id = ?1",
                    rusqlite::params![msg_id],
                    |row| row.get(0),
                ) {
                    if let Ok(attachments) = serde_json::from_str::<Vec<Attachment>>(&attachments_json) {
                        attachments_by_msg.insert(msg_id.to_string(), attachments);
                    }
                }
            }
        }
    
    }

    // Step 4: Compose Message structs (with decryption and edit application)
    let mut messages = Vec::with_capacity(message_events.len());
    for event in message_events {
        // Calculate derived values before moving ownership
        let replied_to = event.get_reply_reference().unwrap_or("").to_string();
        let at = event.timestamp_ms();
        let reactions = reactions_by_msg.remove(&event.id).unwrap_or_default();

        // Get attachments from the lookup map (for kind=15 file messages)
        let attachments: Vec<Attachment> = attachments_by_msg.remove(&event.id)
            .unwrap_or_default();

        // Get original content (already decrypted by get_events())
        let original_content = if event.kind == event_kind::FILE_ATTACHMENT {
            // File attachment content is just an encrypted hash - don't display
            String::new()
        } else {
            // Content already decrypted by get_events()
            event.content.clone()
        };

        // Check for edits and build edit history
        let (content, edited, edit_history) = if let Some(edits) = edits_by_msg.remove(&event.id) {
            // Build edit history: original + all edits
            let mut history = Vec::with_capacity(edits.len() + 1);

            // Add original as first entry
            history.push(EditEntry {
                content: original_content.clone(),
                edited_at: at,
            });

            // Add all edits
            for (edit_ts, edit_content) in &edits {
                history.push(EditEntry {
                    content: edit_content.clone(),
                    edited_at: *edit_ts,
                });
            }

            // Use the latest edit's content
            let latest_content = edits.last()
                .map(|(_, c)| c.clone())
                .unwrap_or(original_content);

            (latest_content, true, Some(history))
        } else {
            (original_content, false, None)
        };

        // Deserialize preview_metadata if present
        let preview_metadata = event.preview_metadata
            .and_then(|json| serde_json::from_str(&json).ok());

        let message = Message {
            id: event.id,
            content,
            replied_to,
            replied_to_content: None, // Populated below
            replied_to_npub: None,    // Populated below
            replied_to_has_attachment: None, // Populated below
            preview_metadata,
            attachments,
            reactions,
            at,
            pending: event.pending,
            failed: event.failed,
            mine: event.mine,
            npub: event.npub,
            wrapper_event_id: event.wrapper_event_id,
            edited,
            edit_history,
        };
        messages.push(message);
    }

    // Step 5: Fetch reply context for messages that have replies
    // Collect all replied_to IDs that are non-empty
    let reply_ids: Vec<String> = messages
        .iter()
        .filter(|m| !m.replied_to.is_empty())
        .map(|m| m.replied_to.clone())
        .collect();

    if !reply_ids.is_empty() {
        // Fetch the replied-to events from the database
        let reply_contexts = get_reply_contexts(&reply_ids).await?;

        // Populate reply context for each message
        for message in &mut messages {
            if !message.replied_to.is_empty() {
                if let Some(ctx) = reply_contexts.get(&message.replied_to) {
                    message.replied_to_content = Some(ctx.content.clone());
                    message.replied_to_npub = ctx.npub.clone();
                    message.replied_to_has_attachment = Some(ctx.has_attachment);
                }
            }
        }
    }

    Ok(messages)
}

/// Extract a single tag value from raw tags JSON without full Vec<Vec<String>> allocation.
/// Does a quick string check first — only parses JSON if the key pattern exists.
fn extract_tag_from_json(tags_json: &str, key: &str) -> Option<String> {
    // Fast path: skip empty tags "[]"
    if tags_json.len() <= 2 { return None; }
    // Quick check: look for ["key" pattern to avoid unnecessary parsing
    let pattern = format!("[\"{}\"", key);
    if !tags_json.contains(&pattern) { return None; }
    // Key likely present — do proper JSON parse to extract value
    let tags: Vec<Vec<String>> = serde_json::from_str(tags_json).ok()?;
    tags.into_iter()
        .find(|tag| tag.first().map(|s| s.as_str()) == Some(key))
        .and_then(|tag| tag.into_iter().nth(1))
}

/// Extract a NIP-10 reply reference from raw tags JSON.
/// Only returns the "e" tag value that has a "reply" marker at position 3,
/// matching the same logic as StoredEvent::get_reply_reference().
fn extract_reply_tag_from_json(tags_json: &str) -> Option<String> {
    if tags_json.len() <= 2 { return None; }
    if !tags_json.contains("[\"e\"") { return None; }
    let tags: Vec<Vec<String>> = serde_json::from_str(tags_json).ok()?;
    tags.into_iter()
        .find(|tag| {
            tag.first().map(|s| s.as_str()) == Some("e")
                && tag.get(3).map(|s| s.as_str()) == Some("reply")
        })
        .and_then(|tag| tag.into_iter().nth(1))
}

/// Get the last message for ALL chats in a single batch query.
///
/// This is optimized for app startup where we need one preview message per chat.
/// Uses correlated subquery with rowid join for fast per-chat lookups.
///
/// Returns: HashMap<chat_identifier, Vec<Message>> (Vec will have 0 or 1 message)
pub async fn get_all_chats_last_messages(
) -> Result<HashMap<String, Vec<Message>>, String> {
    let fn_start = std::time::Instant::now();

    // Step 1: Get the last message event for each chat
    // Uses rowid join (integer) instead of text PK join for faster lookups
    // Tags JSON stored raw - parsed on-demand in Steps 3/4 to avoid 111 upfront JSON parses
    let message_events: Vec<(String, StoredEvent, String)> = {
        let conn = crate::account_manager::get_db_connection_guard_static()?;

        let sql = r#"
            SELECT c.chat_identifier,
                   e.id, e.kind, e.chat_id, e.user_id, e.content, e.tags, e.reference_id,
                   e.created_at, e.received_at, e.mine, e.pending, e.failed, e.wrapper_event_id, e.npub, e.preview_metadata
            FROM chats c
            JOIN events e ON e.rowid = (
                SELECT e2.rowid FROM events e2
                WHERE e2.chat_id = c.id
                AND e2.kind IN (?1, ?2, ?3)
                ORDER BY e2.created_at DESC, e2.received_at DESC
                LIMIT 1
            )
        "#;

        let mut stmt = conn.prepare(sql)
            .map_err(|e| format!("Failed to prepare batch last messages query: {}", e))?;

        let rows = stmt.query_map(
            rusqlite::params![
                event_kind::MLS_CHAT_MESSAGE as i32,
                event_kind::PRIVATE_DIRECT_MESSAGE as i32,
                event_kind::FILE_ATTACHMENT as i32
            ],
            |row| {
                let chat_identifier: String = row.get(0)?;
                let tags_json: String = row.get(6)?;

                let event = StoredEvent {
                    id: row.get(1)?,
                    kind: row.get::<_, i32>(2)? as u16,
                    chat_id: row.get(3)?,
                    user_id: row.get(4)?,
                    content: row.get(5)?,
                    tags: Vec::new(), // Deferred - parsed on-demand from tags_json
                    reference_id: row.get(7)?,
                    created_at: row.get::<_, i64>(8)? as u64,
                    received_at: row.get::<_, i64>(9)? as u64,
                    mine: row.get::<_, i32>(10)? != 0,
                    pending: row.get::<_, i32>(11)? != 0,
                    failed: row.get::<_, i32>(12)? != 0,
                    wrapper_event_id: row.get(13)?,
                    npub: row.get(14)?,
                    preview_metadata: row.get(15)?,
                };
                Ok((chat_identifier, event, tags_json))
            }
        ).map_err(|e| format!("Failed to query batch last messages: {}", e))?;

        rows.filter_map(|r| r.ok()).collect()
    };
    println!("[Boot]     Step 1 (window query): {:?}", fn_start.elapsed());

    if message_events.is_empty() {
        return Ok(HashMap::new());
    }

    // Step 2: Get related events (reactions, edits) for all these messages
    let step2_start = std::time::Instant::now();
    let message_ids: Vec<String> = message_events.iter().map(|(_, e, _)| e.id.clone()).collect();
    let related_events = get_related_events(&message_ids).await?;
    println!("[Boot]     Step 2 (related events): {:?}", step2_start.elapsed());

    // Check encryption status once for the entire function
    let encryption_enabled = crate::crypto::is_encryption_enabled();

    // Group reactions and edits by message ID
    let mut reactions_by_msg: HashMap<String, Vec<Reaction>> = HashMap::new();
    let mut edits_by_msg: HashMap<String, Vec<(u64, String)>> = HashMap::new();

    for event in related_events {
        if let Some(ref_id) = &event.reference_id {
            match event.kind {
                k if k == event_kind::REACTION => {
                    let reaction = Reaction {
                        id: event.id.clone(),
                        reference_id: ref_id.clone(),
                        author_id: event.npub.clone().unwrap_or_default(),
                        emoji: event.content.clone(),
                    };
                    reactions_by_msg.entry(ref_id.clone()).or_default().push(reaction);
                }
                k if k == event_kind::MESSAGE_EDIT => {
                    let decrypted_content = if encryption_enabled {
                        match crate::crypto::internal_decrypt(event.content.clone(), None).await {
                            Ok(decrypted) => decrypted,
                            Err(_) => {
                                if crate::crypto::looks_encrypted(&event.content) {
                                    "[Decryption Failed]".to_string()
                                } else {
                                    event.content.clone()
                                }
                            }
                        }
                    } else {
                        event.content.clone()
                    };
                    let timestamp_ms = event.created_at * 1000;
                    edits_by_msg.entry(ref_id.clone()).or_default().push((timestamp_ms, decrypted_content));
                }
                _ => {}
            }
        }
    }

    // Sort edits by timestamp
    for edits in edits_by_msg.values_mut() {
        edits.sort_by_key(|(ts, _)| *ts);
    }

    // Step 3: Parse attachments from event tags
    let step3_start = std::time::Instant::now();
    let mut attachments_by_msg: HashMap<String, Vec<Attachment>> = HashMap::new();

    for (_, event, tags_json) in &message_events {
        if event.kind != event_kind::FILE_ATTACHMENT && event.kind != event_kind::MLS_CHAT_MESSAGE {
            continue;
        }

        if let Some(attachments_val) = extract_tag_from_json(tags_json, "attachments") {
            if let Ok(attachments) = serde_json::from_str::<Vec<Attachment>>(&attachments_val) {
                if !attachments.is_empty() {
                    attachments_by_msg.insert(event.id.clone(), attachments);
                }
            }
        }
    }

    println!("[Boot]     Step 3 (parse attachments): {:?}", step3_start.elapsed());

    // Step 4: Compose into Message structs, grouped by chat_identifier
    let step4_start = std::time::Instant::now();
    let mut result: HashMap<String, Vec<Message>> = HashMap::new();

    for (chat_identifier, event, tags_json) in message_events {
        let reactions = reactions_by_msg.remove(&event.id).unwrap_or_default();
        let attachments = attachments_by_msg.remove(&event.id).unwrap_or_default();

        // Get replied_to from tags (NIP-10: only "e" tags with "reply" marker)
        let replied_to = extract_reply_tag_from_json(&tags_json)
            .unwrap_or_default();

        // Decrypt content (encryption status cached above)
        let original_content = if event.kind == event_kind::MLS_CHAT_MESSAGE
            || event.kind == event_kind::PRIVATE_DIRECT_MESSAGE
        {
            if encryption_enabled {
                match crate::crypto::internal_decrypt(event.content.clone(), None).await {
                    Ok(decrypted) => decrypted,
                    Err(_) => {
                        if crate::crypto::looks_encrypted(&event.content) {
                            "[Decryption Failed]".to_string()
                        } else {
                            // Doesn't look encrypted — likely plaintext from crash recovery
                            event.content.clone()
                        }
                    }
                }
            } else {
                event.content.clone()
            }
        } else {
            String::new() // File attachments don't have displayable content
        };

        // Apply edits if any
        let (content, edited, edit_history) = if let Some(edits) = edits_by_msg.remove(&event.id) {
            let latest_content = edits.last()
                .map(|(_, c)| c.clone())
                .unwrap_or_else(|| original_content.clone());
            let history: Vec<EditEntry> = std::iter::once(EditEntry {
                content: original_content,
                edited_at: event.created_at * 1000,
            })
            .chain(edits.into_iter().map(|(ts, c)| EditEntry { content: c, edited_at: ts }))
            .collect();
            (latest_content, true, Some(history))
        } else {
            (original_content, false, None)
        };

        let at = event.created_at * 1000; // Convert to ms

        // Deserialize preview_metadata if present
        let preview_metadata = event.preview_metadata
            .and_then(|json| serde_json::from_str(&json).ok());

        let message = Message {
            id: event.id,
            content,
            replied_to,
            replied_to_content: None,
            replied_to_npub: None,
            replied_to_has_attachment: None,
            preview_metadata,
            attachments,
            reactions,
            at,
            pending: event.pending,
            failed: event.failed,
            mine: event.mine,
            npub: event.npub,
            wrapper_event_id: event.wrapper_event_id,
            edited,
            edit_history,
        };

        result.entry(chat_identifier).or_default().push(message);
    }

    println!("[Boot]     Step 4 (compose + decrypt): {:?}", step4_start.elapsed());
    println!("[Boot]     Total get_all_chats_last_messages: {:?}", fn_start.elapsed());
    Ok(result)
}