//! Event database operations.
//!
//! This module handles:
//! - Saving events to the flat event architecture
//! - Reactions, edits, system events
//! - Message views with reactions composed
//! - Reply context population

use std::collections::HashMap;
use tauri::{AppHandle, Runtime};

use crate::{Message, Attachment, Reaction};
use crate::message::EditEntry;
use crate::crypto::{internal_encrypt, internal_decrypt};
use crate::stored_event::{StoredEvent, event_kind};
use super::{get_or_create_chat_id, SystemEventType};

// ============================================================================
// Event Storage Functions (Flat Event-Based Architecture)
// ============================================================================

/// Save a StoredEvent to the events table
///
/// This is the primary storage function for the flat event architecture.
/// All events (messages, reactions, attachments, etc.) are stored as flat rows.
pub async fn save_event<R: Runtime>(
    handle: &AppHandle<R>,
    event: &StoredEvent,
) -> Result<(), String> {
    let conn = crate::account_manager::get_db_connection(handle)?;

    // Serialize tags to JSON
    let tags_json = serde_json::to_string(&event.tags)
        .unwrap_or_else(|_| "[]".to_string());

    // For message and edit events, encrypt the content
    let content = if event.kind == event_kind::MLS_CHAT_MESSAGE
        || event.kind == event_kind::PRIVATE_DIRECT_MESSAGE
        || event.kind == event_kind::MESSAGE_EDIT
    {
        internal_encrypt(event.content.clone(), None).await
    } else {
        event.content.clone()
    };

    // Use INSERT OR REPLACE to update if event already exists (allows attachment updates)
    conn.execute(
        r#"
        INSERT OR REPLACE INTO events (
            id, kind, chat_id, user_id, content, tags, reference_id,
            created_at, received_at, mine, pending, failed, wrapper_event_id, npub
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
        "#,
        rusqlite::params![
            event.id,
            event.kind as i32,
            event.chat_id,
            event.user_id,
            content,
            tags_json,
            event.reference_id,
            event.created_at as i64,
            event.received_at as i64,
            event.mine as i32,
            event.pending as i32,
            event.failed as i32,
            event.wrapper_event_id,
            event.npub,
        ],
    ).map_err(|e| format!("Failed to save event: {}", e))?;

    crate::account_manager::return_db_connection(conn);
    Ok(())
}

/// Save a PIVX payment event to the events table
///
/// Resolves the chat_id from the conversation identifier and saves the event.
pub async fn save_pivx_payment_event<R: Runtime>(
    handle: &AppHandle<R>,
    conversation_id: &str,
    mut event: StoredEvent,
) -> Result<(), String> {
    // Resolve chat_id from conversation identifier
    let chat_id = get_or_create_chat_id(handle, conversation_id)?;
    event.chat_id = chat_id;

    // Save the event
    save_event(handle, &event).await
}

/// Save a system event (member joined/left) to the events table
///
/// Uses INSERT OR IGNORE for deduplication. Returns `true` if the event was
/// actually inserted (new), `false` if it already existed (duplicate).
/// Callers should only emit frontend events if this returns `true`.
pub async fn save_system_event_by_id<R: Runtime>(
    handle: &AppHandle<R>,
    event_id: &str,
    conversation_id: &str,
    event_type: SystemEventType,
    member_npub: &str,
    member_name: Option<&str>,
) -> Result<bool, String> {
    use crate::stored_event::event_kind;

    // Resolve chat_id from conversation identifier
    let chat_id = get_or_create_chat_id(handle, conversation_id)?;

    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    // Build the display content
    let display_name = member_name.unwrap_or(member_npub);
    let content = event_type.display_message(display_name);

    // Build tags for identification (store event_type as integer)
    let tags: Vec<Vec<String>> = vec![
        vec!["d".to_string(), "system-event".to_string()],
        vec!["event-type".to_string(), event_type.as_u8().to_string()],
        vec!["member".to_string(), member_npub.to_string()],
    ];

    let tags_json = serde_json::to_string(&tags)
        .map_err(|e| format!("Failed to serialize tags: {}", e))?;

    let conn = crate::account_manager::get_db_connection(handle)?;

    // Use INSERT OR IGNORE - returns 0 rows affected if duplicate
    let rows_affected = conn.execute(
        r#"
        INSERT OR IGNORE INTO events (
            id, kind, chat_id, user_id, content, tags, reference_id,
            created_at, received_at, mine, pending, failed, wrapper_event_id, npub
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
        "#,
        rusqlite::params![
            event_id,
            event_kind::APPLICATION_SPECIFIC as i32,
            chat_id,
            None::<i64>,
            content,
            tags_json,
            None::<String>,
            now_secs as i64,
            now_secs as i64,
            0, // mine = false
            0, // pending = false
            0, // failed = false
            None::<String>,
            member_npub,
        ],
    ).map_err(|e| format!("Failed to save system event: {}", e))?;

    crate::account_manager::return_db_connection(conn);

    // Return true if we actually inserted (not a duplicate)
    Ok(rows_affected > 0)
}

/// Get PIVX payment events for a chat
///
/// Returns all PIVX payment events (kind 30078 with d=pivx-payment tag) for a conversation.
pub async fn get_pivx_payments_for_chat<R: Runtime>(
    handle: &AppHandle<R>,
    conversation_id: &str,
) -> Result<Vec<StoredEvent>, String> {
    let conn = crate::account_manager::get_db_connection(handle)?;

    // Get chat_id from conversation identifier
    let chat_id: i64 = conn.query_row(
        "SELECT id FROM chats WHERE chat_identifier = ?1",
        rusqlite::params![conversation_id],
        |row| row.get(0)
    ).map_err(|_| "Chat not found")?;

    // Query events with kind=30078 and check for pivx-payment tag
    let payments = {
        let mut stmt = conn.prepare(
            r#"
            SELECT id, kind, chat_id, user_id, content, tags, reference_id,
                   created_at, received_at, mine, pending, failed, wrapper_event_id, npub
            FROM events
            WHERE chat_id = ?1 AND kind = ?2
            ORDER BY created_at ASC
            "#
        ).map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let rows = stmt.query_map(
            rusqlite::params![chat_id, event_kind::APPLICATION_SPECIFIC as i32],
            |row| {
                let tags_json: String = row.get(5)?;
                let tags: Vec<Vec<String>> = serde_json::from_str(&tags_json).unwrap_or_default();

                Ok(StoredEvent {
                    id: row.get(0)?,
                    kind: row.get::<_, i32>(1)? as u16,
                    chat_id: row.get(2)?,
                    user_id: row.get(3)?,
                    content: row.get(4)?,
                    tags,
                    reference_id: row.get(6)?,
                    created_at: row.get::<_, i64>(7)? as u64,
                    received_at: row.get::<_, i64>(8)? as u64,
                    mine: row.get::<_, i32>(9)? != 0,
                    pending: row.get::<_, i32>(10)? != 0,
                    failed: row.get::<_, i32>(11)? != 0,
                    wrapper_event_id: row.get(12)?,
                    npub: row.get(13)?,
                })
            }
        ).map_err(|e| format!("Failed to query events: {}", e))?;

        // Filter for PIVX payment events (d tag = "pivx-payment")
        let mut payments = Vec::new();
        for row in rows {
            let event = row.map_err(|e| format!("Failed to read event: {}", e))?;
            // Check if this is a PIVX payment (has d=pivx-payment tag)
            let is_pivx = event.tags.iter().any(|tag| {
                tag.len() >= 2 && tag[0] == "d" && tag[1] == "pivx-payment"
            });
            if is_pivx {
                payments.push(event);
            }
        }
        payments
    }; // stmt dropped here, releasing borrow on conn

    crate::account_manager::return_db_connection(conn);
    Ok(payments)
}

/// Get system events for a chat (member joined/left, etc.)
///
/// Returns all system events (kind 30078 with d=system-event tag) for a conversation.
pub async fn get_system_events_for_chat<R: Runtime>(
    handle: &AppHandle<R>,
    conversation_id: &str,
) -> Result<Vec<StoredEvent>, String> {
    let conn = crate::account_manager::get_db_connection(handle)?;

    // Get chat_id from conversation identifier
    let chat_id: i64 = conn.query_row(
        "SELECT id FROM chats WHERE chat_identifier = ?1",
        rusqlite::params![conversation_id],
        |row| row.get(0)
    ).map_err(|_| "Chat not found")?;

    // Query events with kind=30078 and check for system-event tag
    let events = {
        let mut stmt = conn.prepare(
            r#"
            SELECT id, kind, chat_id, user_id, content, tags, reference_id,
                   created_at, received_at, mine, pending, failed, wrapper_event_id, npub
            FROM events
            WHERE chat_id = ?1 AND kind = ?2
            ORDER BY created_at ASC
            "#
        ).map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let rows = stmt.query_map(
            rusqlite::params![chat_id, event_kind::APPLICATION_SPECIFIC as i32],
            |row| {
                let tags_json: String = row.get(5)?;
                let tags: Vec<Vec<String>> = serde_json::from_str(&tags_json).unwrap_or_default();

                Ok(StoredEvent {
                    id: row.get(0)?,
                    kind: row.get::<_, i32>(1)? as u16,
                    chat_id: row.get(2)?,
                    user_id: row.get(3)?,
                    content: row.get(4)?,
                    tags,
                    reference_id: row.get(6)?,
                    created_at: row.get::<_, i64>(7)? as u64,
                    received_at: row.get::<_, i64>(8)? as u64,
                    mine: row.get::<_, i32>(9)? != 0,
                    pending: row.get::<_, i32>(10)? != 0,
                    failed: row.get::<_, i32>(11)? != 0,
                    wrapper_event_id: row.get(12)?,
                    npub: row.get(13)?,
                })
            }
        ).map_err(|e| format!("Failed to query events: {}", e))?;

        // Filter for system events (d tag = "system-event")
        let mut system_events = Vec::new();
        for row in rows {
            let event = row.map_err(|e| format!("Failed to read event: {}", e))?;
            let is_system = event.tags.iter().any(|tag| {
                tag.len() >= 2 && tag[0] == "d" && tag[1] == "system-event"
            });
            if is_system {
                system_events.push(event);
            }
        }
        system_events
    }; // stmt dropped here, releasing borrow on conn

    crate::account_manager::return_db_connection(conn);
    Ok(events)
}

/// Save a reaction as a kind=7 event in the events table
///
/// Reactions are stored as separate events referencing the message they react to.
/// This is the Nostr-standard way to store reactions (NIP-25).
pub async fn save_reaction_event<R: Runtime>(
    handle: &AppHandle<R>,
    reaction: &Reaction,
    chat_id: i64,
    user_id: Option<i64>,
    mine: bool,
) -> Result<(), String> {
    let event = StoredEvent {
        id: reaction.id.clone(),
        kind: event_kind::REACTION,
        chat_id,
        user_id,
        content: reaction.emoji.clone(),
        tags: vec![
            vec!["e".to_string(), reaction.reference_id.clone()],
        ],
        reference_id: Some(reaction.reference_id.clone()),
        created_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        received_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0),
        mine,
        pending: false,
        failed: false,
        wrapper_event_id: None,
        npub: Some(reaction.author_id.clone()),
    };

    save_event(handle, &event).await
}

/// Save a message edit as a kind=16 event in the events table
///
/// Edit events reference the original message and contain the new content.
/// The content is encrypted just like DM content.
pub async fn save_edit_event<R: Runtime>(
    handle: &AppHandle<R>,
    edit_id: &str,
    message_id: &str,
    new_content: &str,
    chat_id: i64,
    user_id: Option<i64>,
    npub: &str,
) -> Result<(), String> {
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);

    let event = StoredEvent {
        id: edit_id.to_string(),
        kind: event_kind::MESSAGE_EDIT,
        chat_id,
        user_id,
        content: new_content.to_string(),
        tags: vec![
            vec!["e".to_string(), message_id.to_string(), "".to_string(), "edit".to_string()],
        ],
        reference_id: Some(message_id.to_string()),
        created_at: now_secs,
        received_at: now_ms,
        mine: true,
        pending: false,
        failed: false,
        wrapper_event_id: None,
        npub: Some(npub.to_string()),
    };

    save_event(handle, &event).await
}

/// Check if an event exists in the events table
pub fn event_exists<R: Runtime>(
    handle: &AppHandle<R>,
    event_id: &str,
) -> Result<bool, String> {
    let conn = crate::account_manager::get_db_connection(handle)?;

    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM events WHERE id = ?1)",
        rusqlite::params![event_id],
        |row| row.get(0),
    ).map_err(|e| format!("Failed to check event existence: {}", e))?;

    crate::account_manager::return_db_connection(conn);
    Ok(exists)
}

/// Get events for a chat with pagination
///
/// Returns events ordered by created_at descending (newest first).
/// Optionally filter by event kinds.
pub async fn get_events<R: Runtime>(
    handle: &AppHandle<R>,
    chat_id: i64,
    kinds: Option<&[u16]>,
    limit: usize,
    offset: usize,
) -> Result<Vec<StoredEvent>, String> {
    // Do all SQLite work synchronously in a block to avoid Send issues
    // Connection guard ensures connection is returned even on early error returns
    let events: Vec<StoredEvent> = {
        let conn = crate::account_manager::get_db_connection_guard(handle)?;

        // Build query based on whether kinds filter is provided
        if let Some(k) = kinds {
            // Build numbered placeholders for the IN clause
            // chat_id=?1, kinds=?2,?3,..., limit=?N, offset=?N+1
            let kind_placeholders: String = (0..k.len())
                .map(|i| format!("?{}", i + 2))
                .collect::<Vec<_>>()
                .join(",");
            let limit_param = k.len() + 2;
            let offset_param = k.len() + 3;

            let sql = format!(
                r#"
                SELECT id, kind, chat_id, user_id, content, tags, reference_id,
                       created_at, received_at, mine, pending, failed, wrapper_event_id, npub
                FROM events
                WHERE chat_id = ?1 AND kind IN ({})
                ORDER BY created_at DESC
                LIMIT ?{} OFFSET ?{}
                "#,
                kind_placeholders, limit_param, offset_param
            );

            let mut stmt = conn.prepare(&sql)
                .map_err(|e| format!("Failed to prepare events query: {}", e))?;

            // Use rusqlite params! macro with dynamic kinds
            match k.len() {
                1 => {
                    let rows = stmt.query_map(
                        rusqlite::params![chat_id, k[0] as i32, limit as i64, offset as i64],
                        parse_event_row
                    ).map_err(|e| format!("Failed to query events: {}", e))?;
                    rows.filter_map(|r| r.ok()).collect()
                },
                2 => {
                    let rows = stmt.query_map(
                        rusqlite::params![chat_id, k[0] as i32, k[1] as i32, limit as i64, offset as i64],
                        parse_event_row
                    ).map_err(|e| format!("Failed to query events: {}", e))?;
                    rows.filter_map(|r| r.ok()).collect()
                },
                3 => {
                    let rows = stmt.query_map(
                        rusqlite::params![chat_id, k[0] as i32, k[1] as i32, k[2] as i32, limit as i64, offset as i64],
                        parse_event_row
                    ).map_err(|e| format!("Failed to query events: {}", e))?;
                    rows.filter_map(|r| r.ok()).collect()
                },
                _ => return Err("Unsupported number of kinds".to_string()),
            }
        } else {
            let sql = r#"
                SELECT id, kind, chat_id, user_id, content, tags, reference_id,
                       created_at, received_at, mine, pending, failed, wrapper_event_id, npub
                FROM events
                WHERE chat_id = ?1
                ORDER BY created_at DESC
                LIMIT ?2 OFFSET ?3
            "#;

            let mut stmt = conn.prepare(sql)
                .map_err(|e| format!("Failed to prepare events query: {}", e))?;

            let rows = stmt.query_map(
                rusqlite::params![chat_id, limit as i64, offset as i64],
                parse_event_row
            ).map_err(|e| format!("Failed to query events: {}", e))?;
            rows.filter_map(|r| r.ok()).collect()
        }
        // conn guard dropped here, connection returned to pool
    };

    // Decrypt message content for text messages (this is the async part)
    let mut decrypted_events = Vec::with_capacity(events.len());
    for mut event in events {
        if event.kind == event_kind::MLS_CHAT_MESSAGE || event.kind == event_kind::PRIVATE_DIRECT_MESSAGE {
            event.content = internal_decrypt(event.content, None).await
                .unwrap_or_else(|_| "[Decryption failed]".to_string());
        }
        decrypted_events.push(event);
    }

    Ok(decrypted_events)
}

/// Helper function to parse a row into a StoredEvent
fn parse_event_row(row: &rusqlite::Row) -> rusqlite::Result<StoredEvent> {
    let tags_json: String = row.get(5)?;
    let tags: Vec<Vec<String>> = serde_json::from_str(&tags_json).unwrap_or_default();

    Ok(StoredEvent {
        id: row.get(0)?,
        kind: row.get::<_, i32>(1)? as u16,
        chat_id: row.get(2)?,
        user_id: row.get(3)?,
        content: row.get(4)?,
        tags,
        reference_id: row.get(6)?,
        created_at: row.get::<_, i64>(7)? as u64,
        received_at: row.get::<_, i64>(8)? as u64,
        mine: row.get::<_, i32>(9)? != 0,
        pending: row.get::<_, i32>(10)? != 0,
        failed: row.get::<_, i32>(11)? != 0,
        wrapper_event_id: row.get(12)?,
        npub: row.get(13)?,
    })
}

/// Get events that reference specific message IDs
///
/// Used to fetch reactions and attachments for a set of messages.
/// This is the core function for building materialized views.
pub async fn get_related_events<R: Runtime>(
    handle: &AppHandle<R>,
    reference_ids: &[String],
) -> Result<Vec<StoredEvent>, String> {
    if reference_ids.is_empty() {
        return Ok(Vec::new());
    }

    let conn = crate::account_manager::get_db_connection_guard(handle)?;

    let placeholders: String = reference_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!(
        r#"
        SELECT id, kind, chat_id, user_id, content, tags, reference_id,
               created_at, received_at, mine, pending, failed, wrapper_event_id, npub
        FROM events
        WHERE reference_id IN ({})
        ORDER BY created_at ASC
        "#,
        placeholders
    );

    let mut stmt = conn.prepare(&sql)
        .map_err(|e| format!("Failed to prepare related events query: {}", e))?;

    let params: Vec<&dyn rusqlite::ToSql> = reference_ids.iter()
        .map(|s| s as &dyn rusqlite::ToSql)
        .collect();

    let events: Vec<StoredEvent> = stmt.query_map(params.as_slice(), |row| {
        let tags_json: String = row.get(5)?;
        let tags: Vec<Vec<String>> = serde_json::from_str(&tags_json).unwrap_or_default();

        Ok(StoredEvent {
            id: row.get(0)?,
            kind: row.get::<_, i32>(1)? as u16,
            chat_id: row.get(2)?,
            user_id: row.get(3)?,
            content: row.get(4)?,
            tags,
            reference_id: row.get(6)?,
            created_at: row.get::<_, i64>(7)? as u64,
            received_at: row.get::<_, i64>(8)? as u64,
            mine: row.get::<_, i32>(9)? != 0,
            pending: row.get::<_, i32>(10)? != 0,
            failed: row.get::<_, i32>(11)? != 0,
            wrapper_event_id: row.get(12)?,
            npub: row.get(13)?,
        })
    })
    .map_err(|e| format!("Failed to query related events: {}", e))?
    .filter_map(|r| r.ok())
    .collect();

    Ok(events)
}

/// Context data for a replied-to message
struct ReplyContext {
    content: String,
    npub: Option<String>,
    has_attachment: bool,
}

/// Fetch reply context for a list of message IDs
/// Returns a HashMap of message_id -> ReplyContext
async fn get_reply_contexts<R: Runtime>(
    handle: &AppHandle<R>,
    message_ids: &[String],
) -> Result<HashMap<String, ReplyContext>, String> {
    if message_ids.is_empty() {
        return Ok(HashMap::new());
    }

    // Do all SQLite work synchronously in a block to avoid Send issues
    // Connection guard ensures connection is returned even on early error returns
    let (events, edits): (Vec<(String, i32, String, Option<String>)>, Vec<(String, String)>) = {
        let conn = crate::account_manager::get_db_connection_guard(handle)?;

        // Build placeholders for IN clause
        let placeholders: String = (0..message_ids.len())
            .map(|i| format!("?{}", i + 1))
            .collect::<Vec<_>>()
            .join(",");

        // Query original messages
        let sql = format!(
            r#"
            SELECT id, kind, content, npub
            FROM events
            WHERE id IN ({})
            "#,
            placeholders
        );

        let mut stmt = conn.prepare(&sql)
            .map_err(|e| format!("Failed to prepare reply context query: {}", e))?;

        // Build params as String refs for the query
        let params: Vec<&str> = message_ids.iter().map(|s| s.as_str()).collect();
        let params_dyn: Vec<&dyn rusqlite::ToSql> = params.iter().map(|s| s as &dyn rusqlite::ToSql).collect();

        let rows = stmt.query_map(params_dyn.as_slice(), |row| {
            Ok((
                row.get::<_, String>(0)?, // id
                row.get::<_, i32>(1)?,    // kind
                row.get::<_, String>(2)?, // content
                row.get::<_, Option<String>>(3)?, // npub
            ))
        }).map_err(|e| format!("Failed to query reply contexts: {}", e))?;

        let events_result: Vec<(String, i32, String, Option<String>)> = rows.filter_map(|r| r.ok()).collect();
        drop(stmt);

        // Query latest edits for these messages (most recent edit per message)
        let edit_sql = format!(
            r#"
            SELECT reference_id, content
            FROM events
            WHERE kind = {} AND reference_id IN ({})
            ORDER BY created_at DESC
            "#,
            event_kind::MESSAGE_EDIT,
            placeholders
        );

        let mut edit_stmt = conn.prepare(&edit_sql)
            .map_err(|e| format!("Failed to prepare edit query: {}", e))?;

        let edit_rows = edit_stmt.query_map(params_dyn.as_slice(), |row| {
            Ok((
                row.get::<_, String>(0)?, // reference_id (original message id)
                row.get::<_, String>(1)?, // content (edited content)
            ))
        }).map_err(|e| format!("Failed to query edits: {}", e))?;

        let edits_result: Vec<(String, String)> = edit_rows.filter_map(|r| r.ok()).collect();

        (events_result, edits_result)
        // conn guard dropped here, connection returned to pool
    };

    // Build a map of message_id -> latest edit content (first one since ordered DESC)
    let mut latest_edits: HashMap<String, String> = HashMap::new();
    for (ref_id, content) in edits {
        // Only keep the first (most recent) edit for each message
        latest_edits.entry(ref_id).or_insert(content);
    }

    // Process events and decrypt content (async part)
    let mut contexts = HashMap::new();
    for (id, kind, original_content, npub) in events {
        let has_attachment = kind == event_kind::FILE_ATTACHMENT as i32;

        // Use latest edit content if available, otherwise use original
        let content_to_decrypt = latest_edits.get(&id).cloned().unwrap_or(original_content);

        // Decrypt content for text messages (Kind 9 or Kind 14)
        let decrypted_content = if kind == event_kind::MLS_CHAT_MESSAGE as i32
            || kind == event_kind::PRIVATE_DIRECT_MESSAGE as i32
        {
            internal_decrypt(content_to_decrypt, None).await
                .unwrap_or_else(|_| "[Decryption failed]".to_string())
        } else {
            // File attachments don't have displayable content
            String::new()
        };

        contexts.insert(id, ReplyContext {
            content: decrypted_content,
            npub,
            has_attachment,
        });
    }

    Ok(contexts)
}

/// Populate reply context for a single message before emitting to frontend
/// This is used for real-time messages that don't go through get_message_views
pub async fn populate_reply_context<R: Runtime>(
    handle: &AppHandle<R>,
    message: &mut Message,
) -> Result<(), String> {
    if message.replied_to.is_empty() {
        return Ok(());
    }

    let contexts = get_reply_contexts(handle, &[message.replied_to.clone()]).await?;

    if let Some(ctx) = contexts.get(&message.replied_to) {
        message.replied_to_content = Some(ctx.content.clone());
        message.replied_to_npub = ctx.npub.clone();
        message.replied_to_has_attachment = Some(ctx.has_attachment);
    }

    Ok(())
}

/// Get message events with their reactions composed (materialized view)
///
/// This function performs a single efficient query to get messages and their
/// related events, then composes them into Message structs for the frontend.
pub async fn get_message_views<R: Runtime>(
    handle: &AppHandle<R>,
    chat_id: i64,
    limit: usize,
    offset: usize,
) -> Result<Vec<Message>, String> {
    // Step 1: Get message events (kind 9, 14, and 15)
    let message_kinds = [event_kind::MLS_CHAT_MESSAGE, event_kind::PRIVATE_DIRECT_MESSAGE, event_kind::FILE_ATTACHMENT];
    let message_events = get_events(handle, chat_id, Some(&message_kinds), limit, offset).await?;

    if message_events.is_empty() {
        return Ok(Vec::new());
    }

    // Step 2: Get related events (reactions, edits) for these messages
    let message_ids: Vec<String> = message_events.iter().map(|e| e.id.clone()).collect();
    let related_events = get_related_events(handle, &message_ids).await?;

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
                    let decrypted_content = internal_decrypt(event.content.clone(), None).await
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
        let conn = crate::account_manager::get_db_connection(handle)?;

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
        crate::account_manager::return_db_connection(conn);
    }

    // Step 4: Compose Message structs (with decryption and edit application)
    let mut messages = Vec::with_capacity(message_events.len());
    for event in message_events {
        // Calculate derived values before moving ownership
        let replied_to = event.get_reply_reference().unwrap_or("").to_string();
        let at = event.timestamp_ms();
        let reactions = reactions_by_msg.remove(&event.id).unwrap_or_default();

        // Get attachments from the lookup map (for kind=15 file messages)
        let attachments = attachments_by_msg.remove(&event.id).unwrap_or_default();

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

        let message = Message {
            id: event.id,
            content,
            replied_to,
            replied_to_content: None, // Populated below
            replied_to_npub: None,    // Populated below
            replied_to_has_attachment: None, // Populated below
            preview_metadata: None, // TODO: Parse from tags if needed
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
        let reply_contexts = get_reply_contexts(handle, &reply_ids).await?;

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

/// Get the last message for ALL chats in a single batch query.
///
/// This is optimized for app startup where we need one preview message per chat.
/// Uses ROW_NUMBER() OVER (PARTITION BY chat_id) to get the latest message per chat
/// in a single query, avoiding N separate queries.
///
/// Returns: HashMap<chat_identifier, Vec<Message>> (Vec will have 0 or 1 message)
pub async fn get_all_chats_last_messages<R: Runtime>(
    handle: &AppHandle<R>,
) -> Result<HashMap<String, Vec<Message>>, String> {
    // Step 1: Get the last message event for each chat using window function
    let message_events: Vec<(String, StoredEvent)> = {
        let conn = crate::account_manager::get_db_connection_guard(handle)?;

        // Use ROW_NUMBER to get the latest message per chat
        // Join with chats table to get chat_identifier
        let sql = r#"
            SELECT c.chat_identifier,
                   e.id, e.kind, e.chat_id, e.user_id, e.content, e.tags, e.reference_id,
                   e.created_at, e.received_at, e.mine, e.pending, e.failed, e.wrapper_event_id, e.npub
            FROM (
                SELECT *, ROW_NUMBER() OVER (PARTITION BY chat_id ORDER BY created_at DESC) as rn
                FROM events
                WHERE kind IN (?1, ?2, ?3)
            ) e
            JOIN chats c ON e.chat_id = c.id
            WHERE e.rn = 1
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
                let tags: Vec<Vec<String>> = serde_json::from_str(&tags_json).unwrap_or_default();

                let event = StoredEvent {
                    id: row.get(1)?,
                    kind: row.get::<_, i32>(2)? as u16,
                    chat_id: row.get(3)?,
                    user_id: row.get(4)?,
                    content: row.get(5)?,
                    tags,
                    reference_id: row.get(7)?,
                    created_at: row.get::<_, i64>(8)? as u64,
                    received_at: row.get::<_, i64>(9)? as u64,
                    mine: row.get::<_, i32>(10)? != 0,
                    pending: row.get::<_, i32>(11)? != 0,
                    failed: row.get::<_, i32>(12)? != 0,
                    wrapper_event_id: row.get(13)?,
                    npub: row.get(14)?,
                };
                Ok((chat_identifier, event))
            }
        ).map_err(|e| format!("Failed to query batch last messages: {}", e))?;

        rows.filter_map(|r| r.ok()).collect()
    };

    if message_events.is_empty() {
        return Ok(HashMap::new());
    }

    // Step 2: Get related events (reactions, edits) for all these messages
    let message_ids: Vec<String> = message_events.iter().map(|(_, e)| e.id.clone()).collect();
    let related_events = get_related_events(handle, &message_ids).await?;

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
                    let decrypted_content = internal_decrypt(event.content.clone(), None).await
                        .unwrap_or_else(|_| event.content.clone());
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
    let mut attachments_by_msg: HashMap<String, Vec<Attachment>> = HashMap::new();

    for (_, event) in &message_events {
        if event.kind != event_kind::FILE_ATTACHMENT && event.kind != event_kind::MLS_CHAT_MESSAGE {
            continue;
        }

        if let Some(attachments_json) = event.get_tag("attachments") {
            if let Ok(attachments) = serde_json::from_str::<Vec<Attachment>>(attachments_json) {
                if !attachments.is_empty() {
                    attachments_by_msg.insert(event.id.clone(), attachments);
                }
            }
        }
    }

    // Step 4: Compose into Message structs, grouped by chat_identifier
    let mut result: HashMap<String, Vec<Message>> = HashMap::new();

    for (chat_identifier, event) in message_events {
        let reactions = reactions_by_msg.remove(&event.id).unwrap_or_default();
        let attachments = attachments_by_msg.remove(&event.id).unwrap_or_default();

        // Get replied_to from tags
        let replied_to = event.get_tag("e")
            .map(|s| s.to_string())
            .unwrap_or_default();

        // Decrypt content
        let original_content = if event.kind == event_kind::MLS_CHAT_MESSAGE
            || event.kind == event_kind::PRIVATE_DIRECT_MESSAGE
        {
            internal_decrypt(event.content.clone(), None).await
                .unwrap_or_else(|_| event.content.clone())
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

        let message = Message {
            id: event.id,
            content,
            replied_to,
            replied_to_content: None,
            replied_to_npub: None,
            replied_to_has_attachment: None,
            preview_metadata: None,
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

    Ok(result)
}