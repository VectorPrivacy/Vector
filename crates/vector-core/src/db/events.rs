//! Event storage — save_event for the flat event architecture.

use crate::stored_event::{StoredEvent, event_kind};
use rusqlite::OptionalExtension;
use crate::crypto::maybe_encrypt;
use crate::types::{Message, Attachment, Reaction};

/// Save a StoredEvent to the events table.
///
/// Primary storage function for the flat event architecture.
/// Conditionally encrypts message/edit content based on user setting.
/// Uses INSERT OR REPLACE with COALESCE to preserve existing wrapper_event_id.
pub async fn save_event(event: &StoredEvent) -> Result<(), String> {
    let conn = super::get_write_connection_guard_static()?;

    let tags_json = serde_json::to_string(&event.tags)
        .unwrap_or_else(|_| "[]".to_string());

    // Conditionally encrypt message/edit content
    let content = if event.kind == event_kind::MLS_CHAT_MESSAGE
        || event.kind == event_kind::PRIVATE_DIRECT_MESSAGE
        || event.kind == event_kind::MESSAGE_EDIT
    {
        maybe_encrypt(event.content.clone()).await
    } else {
        event.content.clone()
    };

    conn.execute(
        r#"
        INSERT OR REPLACE INTO events (
            id, kind, chat_id, user_id, content, tags, reference_id,
            created_at, received_at, mine, pending, failed, wrapper_event_id, npub, preview_metadata
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
            COALESCE(?13, (SELECT wrapper_event_id FROM events WHERE id = ?1)),
            ?14, ?15)
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
            event.preview_metadata,
        ],
    ).map_err(|e| format!("Failed to save event: {}", e))?;

    Ok(())
}

/// Check if an event exists in the database.
pub fn event_exists(event_id: &str) -> Result<bool, String> {
    let conn = super::get_db_connection_guard_static()?;
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM events WHERE id = ?1)",
        rusqlite::params![event_id],
        |row| row.get(0),
    ).map_err(|e| format!("Failed to check event existence: {}", e))
}

/// Save a reaction as a kind=7 event referencing the message.
pub async fn save_reaction_event(
    reaction: &Reaction,
    chat_id: i64,
    user_id: Option<i64>,
    mine: bool,
    wrapper_event_id: Option<String>,
) -> Result<(), String> {
    // Persist the NIP-30 emoji tag alongside the `e` reference so the
    // image URL survives reload — pure `arrEmojiPacks` lookup would
    // fail when the user hasn't yet opened the picker or unsubscribed.
    let mut tags: Vec<Vec<String>> = vec![
        vec!["e".to_string(), reaction.reference_id.clone()],
    ];
    if let Some(url) = &reaction.emoji_url {
        if reaction.emoji.starts_with(':') && reaction.emoji.ends_with(':') && reaction.emoji.len() >= 3 {
            let shortcode = &reaction.emoji[1..reaction.emoji.len() - 1];
            if !shortcode.is_empty() && !url.is_empty() {
                tags.push(vec!["emoji".to_string(), shortcode.to_string(), url.clone()]);
            }
        }
    }
    let event = StoredEvent {
        id: reaction.id.clone(),
        kind: event_kind::REACTION,
        chat_id,
        user_id,
        content: reaction.emoji.clone(),
        tags,
        reference_id: Some(reaction.reference_id.clone()),
        created_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs()).unwrap_or(0),
        received_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64).unwrap_or(0),
        mine,
        pending: false,
        failed: false,
        wrapper_event_id,
        npub: Some(reaction.author_id.clone()),
        preview_metadata: None,
    };
    save_event(&event).await
}

// ============================================================================
// save_message — Message → StoredEvent → DB
// ============================================================================

/// Save a single message to the database.
///
/// Converts Message to StoredEvent and saves via the flat event architecture.
/// Also saves reactions as separate kind=7 events.
pub async fn save_message(chat_id: &str, message: &Message) -> Result<(), String> {
    let chat_int_id = super::id_cache::get_or_create_chat_id(chat_id)?;

    let user_int_id = if let Some(ref npub_str) = message.npub {
        super::id_cache::get_or_create_user_id(npub_str)?
    } else {
        None
    };

    let event = message_to_stored_event(message, chat_int_id, user_int_id);
    save_event(&event).await?;

    // Save reactions as separate kind=7 events
    for reaction in &message.reactions {
        if !event_exists(&reaction.id)? {
            let user_id = super::id_cache::get_or_create_user_id(&reaction.author_id)?;
            let is_mine = super::get_current_account()
                .map(|npub| reaction.author_id == npub)
                .unwrap_or(false);
            save_reaction_event(reaction, chat_int_id, user_id, is_mine, None).await?;
        }
    }

    Ok(())
}

/// Convert a Message to a StoredEvent.
fn message_to_stored_event(message: &Message, chat_id: i64, user_id: Option<i64>) -> StoredEvent {
    let kind = if !message.attachments.is_empty() {
        event_kind::FILE_ATTACHMENT
    } else {
        event_kind::PRIVATE_DIRECT_MESSAGE
    };

    let mut tags: Vec<Vec<String>> = Vec::new();

    // Millisecond precision tag
    let ms = message.at % 1000;
    if ms > 0 {
        tags.push(vec!["ms".to_string(), ms.to_string()]);
    }

    // Reply reference
    if !message.replied_to.is_empty() {
        tags.push(vec![
            "e".to_string(),
            message.replied_to.clone(),
            "".to_string(),
            "reply".to_string(),
        ]);
    }

    // Attachments as JSON tag
    if !message.attachments.is_empty() {
        if let Ok(json) = serde_json::to_string(&message.attachments) {
            tags.push(vec!["attachments".to_string(), json]);
        }
    }

    // NIP-30 emoji tags — persist so reload from DB still renders the
    // custom emoji image instead of the literal `:shortcode:`.
    for et in &message.emoji_tags {
        tags.push(vec!["emoji".to_string(), et.shortcode.clone(), et.url.clone()]);
    }

    let preview_metadata = message.preview_metadata.as_ref()
        .and_then(|m| serde_json::to_string(m).ok());

    StoredEvent {
        id: message.id.clone(),
        kind,
        chat_id,
        user_id,
        content: message.content.clone(),
        tags,
        reference_id: None,
        created_at: message.at / 1000,
        received_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0),
        mine: message.mine,
        pending: message.pending,
        failed: message.failed,
        wrapper_event_id: message.wrapper_event_id.clone(),
        npub: message.npub.clone(),
        preview_metadata,
    }
}

/// Save a PIVX payment event, resolving chat_id from conversation identifier.
pub async fn save_pivx_payment_event(
    conversation_id: &str,
    mut event: StoredEvent,
) -> Result<(), String> {
    event.chat_id = super::id_cache::get_or_create_chat_id(conversation_id)?;
    save_event(&event).await
}

/// Save a system event (member joined/left/removed) with dedup.
/// Returns true if inserted, false if duplicate.
pub async fn save_system_event_by_id(
    event_id: &str,
    conversation_id: &str,
    event_type: crate::stored_event::SystemEventType,
    member_npub: &str,
    member_name: Option<&str>,
) -> Result<bool, String> {
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs()).unwrap_or(0);
    save_system_event_at(event_id, conversation_id, event_type, member_npub, member_name, now_secs, None, None).await
}

/// Like [`save_system_event_by_id`] but stamps `created_at` from the event's own authenticated timestamp
/// (clamped to not exceed local now, since the inner author sets it) so a HISTORICALLY-synced presence
/// (join/leave) sorts at the time it happened, not at ingest-time now. `received_at` stays local now.
pub async fn save_system_event_at(
    event_id: &str,
    conversation_id: &str,
    event_type: crate::stored_event::SystemEventType,
    member_npub: &str,
    member_name: Option<&str>,
    created_at_secs: u64,
    // Join attribution (public invites): who minted the link the member joined via, and its label.
    // Stored as queryable tags so per-link join counts fall out of a tag scan.
    invited_by: Option<&str>,
    invited_label: Option<&str>,
) -> Result<bool, String> {
    let chat_id = super::id_cache::get_or_create_chat_id(conversation_id)?;

    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs()).unwrap_or(0);
    // Author-set timestamp: clamp forward so a future-dated event can't jump ahead of real activity.
    let created_at = created_at_secs.min(now_secs);

    let display_name = member_name.unwrap_or(member_npub);
    let content = event_type.display_message(display_name);

    let mut tags: Vec<Vec<String>> = vec![
        vec!["d".to_string(), "system-event".to_string()],
        vec!["event-type".to_string(), event_type.as_u8().to_string()],
        vec!["member".to_string(), member_npub.to_string()],
    ];
    if let Some(by) = invited_by {
        tags.push(vec!["invited-by".to_string(), by.to_string()]);
        if let Some(l) = invited_label.filter(|l| !l.is_empty()) {
            tags.push(vec!["invited-label".to_string(), l.to_string()]);
        }
    }
    let tags_json = serde_json::to_string(&tags)
        .map_err(|e| format!("Failed to serialize tags: {}", e))?;

    let conn = super::get_write_connection_guard_static()?;
    let rows = conn.execute(
        r#"INSERT OR IGNORE INTO events (
            id, kind, chat_id, user_id, content, tags, reference_id,
            created_at, received_at, mine, pending, failed, wrapper_event_id, npub
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)"#,
        rusqlite::params![
            event_id,
            event_kind::APPLICATION_SPECIFIC as i32,
            chat_id, None::<i64>, content, tags_json, None::<String>,
            created_at as i64, now_secs as i64,
            0, 0, 0, None::<String>, member_npub,
        ],
    ).map_err(|e| format!("Failed to save system event: {}", e))?;

    Ok(rows > 0)
}

/// Save a message edit as a kind=16 event referencing the original message.
pub async fn save_edit_event(
    edit_id: &str,
    message_id: &str,
    new_content: &str,
    chat_id: i64,
    user_id: Option<i64>,
    npub: &str,
) -> Result<(), String> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH).unwrap();

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
        created_at: now.as_secs(),
        received_at: now.as_millis() as u64,
        mine: true,
        pending: false,
        failed: false,
        wrapper_event_id: None,
        npub: Some(npub.to_string()),
        preview_metadata: None,
    };

    save_event(&event).await
}

/// Delete an event from the events table by ID.
pub async fn delete_event(event_id: &str) -> Result<(), String> {
    let conn = super::get_write_connection_guard_static()?;
    conn.execute(
        "DELETE FROM events WHERE id = ?1",
        rusqlite::params![event_id],
    ).map_err(|e| format!("Failed to delete event: {}", e))?;
    Ok(())
}

/// The stored author (npub) of an event, or `None` if the row (or DB) is absent. Lets the
/// out-of-window moderation-hide path authorize against a paged-out message's real author.
pub fn event_author(event_id: &str) -> Result<Option<String>, String> {
    let conn = match super::get_db_connection_guard_static() {
        Ok(c) => c,
        Err(_) => return Ok(None),
    };
    conn.query_row(
        "SELECT npub FROM events WHERE id = ?1",
        rusqlite::params![event_id],
        |row| row.get::<_, Option<String>>(0),
    )
    .optional()
    .map(|o| o.flatten())
    .map_err(|e| format!("Failed to read event author: {}", e))
}

/// Check if a message/event exists in the database. Returns false if DB unavailable.
pub fn message_exists_in_db(message_id: &str) -> Result<bool, String> {
    let conn = match super::get_db_connection_guard_static() {
        Ok(c) => c,
        Err(_) => return Ok(false),
    };
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM events WHERE id = ?1)",
        rusqlite::params![message_id],
        |row| row.get(0),
    ).map_err(|e| format!("Failed to check event existence: {}", e))
}

/// Check if a wrapper (giftwrap) event ID exists. Returns false if DB unavailable.
pub fn wrapper_event_exists(wrapper_event_id: &str) -> Result<bool, String> {
    let conn = match super::get_db_connection_guard_static() {
        Ok(c) => c,
        Err(_) => return Ok(false),
    };
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM events WHERE wrapper_event_id = ?1)",
        rusqlite::params![wrapper_event_id],
        |row| row.get(0),
    ).map_err(|e| format!("Failed to check wrapper event existence: {}", e))
}

/// Update the wrapper event ID for an existing event.
/// Returns true if updated, false if event already had a wrapper_id.
pub fn update_wrapper_event_id(event_id: &str, wrapper_event_id: &str) -> Result<bool, String> {
    let conn = match super::get_write_connection_guard_static() {
        Ok(c) => c,
        Err(_) => return Ok(false),
    };
    let rows = conn.execute(
        "UPDATE events SET wrapper_event_id = ?1 WHERE id = ?2 AND (wrapper_event_id IS NULL OR wrapper_event_id = '')",
        rusqlite::params![wrapper_event_id, event_id],
    ).map_err(|e| format!("Failed to update wrapper event ID: {}", e))?;
    Ok(rows > 0)
}

/// Get message count for a chat.
pub fn get_chat_message_count(chat_id: i64) -> Result<usize, String> {
    let conn = super::get_db_connection_guard_static()?;
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM events WHERE chat_id = ?1 AND kind IN (14, 15)",
        rusqlite::params![chat_id],
        |row| row.get(0),
    ).map_err(|e| format!("Failed to count messages: {}", e))?;
    Ok(count as usize)
}

/// Get PIVX payment events for a chat.
pub fn get_pivx_payments_for_chat(conversation_id: &str) -> Result<Vec<StoredEvent>, String> {
    let conn = super::get_db_connection_guard_static()?;
    let chat_id: i64 = conn.query_row(
        "SELECT id FROM chats WHERE chat_identifier = ?1",
        rusqlite::params![conversation_id], |row| row.get(0)
    ).map_err(|_| "Chat not found")?;

    let mut stmt = conn.prepare(
        "SELECT id, kind, chat_id, user_id, content, tags, reference_id, \
         created_at, received_at, mine, pending, failed, wrapper_event_id, npub \
         FROM events WHERE chat_id = ?1 AND kind = ?2 ORDER BY created_at ASC, received_at ASC"
    ).map_err(|e| format!("Failed to prepare: {}", e))?;

    let rows = stmt.query_map(
        rusqlite::params![chat_id, event_kind::APPLICATION_SPECIFIC as i32],
        |row| {
            let tags_json: String = row.get(5)?;
            let tags: Vec<Vec<String>> = serde_json::from_str(&tags_json).unwrap_or_default();
            Ok(StoredEvent {
                id: row.get(0)?, kind: row.get::<_, i32>(1)? as u16,
                chat_id: row.get(2)?, user_id: row.get(3)?, content: row.get(4)?,
                tags, reference_id: row.get(6)?,
                created_at: row.get::<_, i64>(7)? as u64, received_at: row.get::<_, i64>(8)? as u64,
                mine: row.get::<_, i32>(9)? != 0, pending: row.get::<_, i32>(10)? != 0,
                failed: row.get::<_, i32>(11)? != 0, wrapper_event_id: row.get(12)?,
                npub: row.get(13)?, preview_metadata: None,
            })
        }
    ).map_err(|e| format!("Failed to query: {}", e))?;

    let mut payments = Vec::new();
    for row in rows {
        let event = row.map_err(|e| format!("Failed to read event: {}", e))?;
        if event.tags.iter().any(|t| t.len() >= 2 && t[0] == "d" && t[1] == "pivx-payment") {
            payments.push(event);
        }
    }
    Ok(payments)
}

/// Get system events (member joined/left) for a chat.
pub fn get_system_events_for_chat(conversation_id: &str) -> Result<Vec<StoredEvent>, String> {
    let conn = super::get_db_connection_guard_static()?;
    let chat_id: i64 = conn.query_row(
        "SELECT id FROM chats WHERE chat_identifier = ?1",
        rusqlite::params![conversation_id], |row| row.get(0)
    ).map_err(|_| "Chat not found")?;

    let mut stmt = conn.prepare(
        "SELECT id, kind, chat_id, user_id, content, tags, reference_id, \
         created_at, received_at, mine, pending, failed, wrapper_event_id, npub \
         FROM events WHERE chat_id = ?1 AND kind = ?2 ORDER BY created_at ASC, received_at ASC"
    ).map_err(|e| format!("Failed to prepare: {}", e))?;

    let rows = stmt.query_map(
        rusqlite::params![chat_id, event_kind::APPLICATION_SPECIFIC as i32],
        |row| {
            let tags_json: String = row.get(5)?;
            let tags: Vec<Vec<String>> = serde_json::from_str(&tags_json).unwrap_or_default();
            Ok(StoredEvent {
                id: row.get(0)?, kind: row.get::<_, i32>(1)? as u16,
                chat_id: row.get(2)?, user_id: row.get(3)?, content: row.get(4)?,
                tags, reference_id: row.get(6)?,
                created_at: row.get::<_, i64>(7)? as u64, received_at: row.get::<_, i64>(8)? as u64,
                mine: row.get::<_, i32>(9)? != 0, pending: row.get::<_, i32>(10)? != 0,
                failed: row.get::<_, i32>(11)? != 0, wrapper_event_id: row.get(12)?,
                npub: row.get(13)?, preview_metadata: None,
            })
        }
    ).map_err(|e| format!("Failed to query: {}", e))?;

    let mut events = Vec::new();
    for row in rows {
        let event = row.map_err(|e| format!("Failed to read event: {}", e))?;
        if event.tags.iter().any(|t| t.len() >= 2 && t[0] == "d" && t[1] == "system-event") {
            events.push(event);
        }
    }
    Ok(events)
}

// ============================================================================
// Event Read Operations
// ============================================================================

/// Helper to parse a SQLite row into a StoredEvent.
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
        preview_metadata: row.get(14)?,
    })
}

/// Get events for a chat with pagination, optionally filtered by kind.
/// Message/edit content is decrypted via maybe_decrypt.
pub async fn get_events(
    chat_id: i64,
    kinds: Option<&[u16]>,
    limit: usize,
    offset: usize,
) -> Result<Vec<StoredEvent>, String> {
    let events: Vec<StoredEvent> = {
        let conn = super::get_db_connection_guard_static()?;

        if let Some(k) = kinds {
            let kind_placeholders: String = (0..k.len())
                .map(|i| format!("?{}", i + 2))
                .collect::<Vec<_>>()
                .join(",");
            let limit_param = k.len() + 2;
            let offset_param = k.len() + 3;

            let sql = format!(
                "SELECT id, kind, chat_id, user_id, content, tags, reference_id, \
                 created_at, received_at, mine, pending, failed, wrapper_event_id, npub, preview_metadata \
                 FROM events WHERE chat_id = ?1 AND kind IN ({}) \
                 ORDER BY created_at DESC, received_at DESC \
                 LIMIT ?{} OFFSET ?{}",
                kind_placeholders, limit_param, offset_param
            );

            let mut stmt = conn.prepare(&sql)
                .map_err(|e| format!("Failed to prepare events query: {}", e))?;

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
            let mut stmt = conn.prepare(
                "SELECT id, kind, chat_id, user_id, content, tags, reference_id, \
                 created_at, received_at, mine, pending, failed, wrapper_event_id, npub, preview_metadata \
                 FROM events WHERE chat_id = ?1 \
                 ORDER BY created_at DESC, received_at DESC \
                 LIMIT ?2 OFFSET ?3"
            ).map_err(|e| format!("Failed to prepare events query: {}", e))?;

            let rows = stmt.query_map(
                rusqlite::params![chat_id, limit as i64, offset as i64],
                parse_event_row
            ).map_err(|e| format!("Failed to query events: {}", e))?;
            rows.filter_map(|r| r.ok()).collect()
        }
    };

    // Decrypt message content
    let mut decrypted = Vec::with_capacity(events.len());
    for mut event in events {
        if event.kind == event_kind::MLS_CHAT_MESSAGE || event.kind == event_kind::PRIVATE_DIRECT_MESSAGE {
            event.content = crate::crypto::maybe_decrypt(event.content).await
                .unwrap_or_else(|_| "[Decryption failed]".to_string());
        }
        decrypted.push(event);
    }

    Ok(decrypted)
}

/// Get events that reference specific message IDs (reactions, edits).
pub async fn get_related_events(
    reference_ids: &[String],
) -> Result<Vec<StoredEvent>, String> {
    if reference_ids.is_empty() {
        return Ok(Vec::new());
    }

    let conn = super::get_db_connection_guard_static()?;

    let placeholders: String = reference_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!(
        "SELECT id, kind, chat_id, user_id, content, tags, reference_id, \
         created_at, received_at, mine, pending, failed, wrapper_event_id, npub, preview_metadata \
         FROM events WHERE reference_id IN ({}) \
         ORDER BY created_at ASC, received_at ASC",
        placeholders
    );

    let mut stmt = conn.prepare(&sql)
        .map_err(|e| format!("Failed to prepare related events query: {}", e))?;

    let params: Vec<&dyn rusqlite::ToSql> = reference_ids.iter()
        .map(|s| s as &dyn rusqlite::ToSql)
        .collect();

    let events: Vec<StoredEvent> = stmt.query_map(params.as_slice(), parse_event_row)
        .map_err(|e| format!("Failed to query related events: {}", e))?
        .filter_map(|r| r.ok())
        .collect();

    Ok(events)
}

/// Context data for a replied-to message.
pub struct ReplyContext {
    pub content: String,
    pub npub: Option<String>,
    pub has_attachment: bool,
}

/// Fetch reply context for a list of message IDs.
pub async fn get_reply_contexts(
    message_ids: &[String],
) -> Result<std::collections::HashMap<String, ReplyContext>, String> {
    use std::collections::HashMap;

    if message_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let (events, edits): (Vec<(String, i32, String, Option<String>)>, Vec<(String, String)>) = {
        let conn = super::get_db_connection_guard_static()?;

        let placeholders: String = (0..message_ids.len())
            .map(|i| format!("?{}", i + 1))
            .collect::<Vec<_>>()
            .join(",");

        // Query original messages
        let sql = format!(
            "SELECT id, kind, content, npub FROM events WHERE id IN ({})",
            placeholders
        );
        let mut stmt = conn.prepare(&sql)
            .map_err(|e| format!("Failed to prepare reply context query: {}", e))?;

        let params: Vec<&str> = message_ids.iter().map(|s| s.as_str()).collect();
        let params_dyn: Vec<&dyn rusqlite::ToSql> = params.iter().map(|s| s as &dyn rusqlite::ToSql).collect();

        let rows = stmt.query_map(params_dyn.as_slice(), |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i32>(1)?,
                row.get::<_, String>(2)?, row.get::<_, Option<String>>(3)?))
        }).map_err(|e| format!("Failed to query reply contexts: {}", e))?;
        let events_result: Vec<_> = rows.filter_map(|r| r.ok()).collect();
        drop(stmt);

        // Query latest edits
        let edit_sql = format!(
            "SELECT reference_id, content FROM events \
             WHERE kind = {} AND reference_id IN ({}) \
             ORDER BY created_at DESC, received_at DESC",
            event_kind::MESSAGE_EDIT, placeholders
        );
        let mut edit_stmt = conn.prepare(&edit_sql)
            .map_err(|e| format!("Failed to prepare edit query: {}", e))?;
        let edit_rows = edit_stmt.query_map(params_dyn.as_slice(), |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        }).map_err(|e| format!("Failed to query edits: {}", e))?;
        let edits_result: Vec<_> = edit_rows.filter_map(|r| r.ok()).collect();

        (events_result, edits_result)
    };

    // Build latest edit map (first = most recent since ordered DESC)
    let mut latest_edits: HashMap<String, String> = HashMap::new();
    for (ref_id, content) in edits {
        latest_edits.entry(ref_id).or_insert(content);
    }

    // Decrypt and build contexts
    let mut contexts = HashMap::new();
    for (id, kind, original_content, npub) in events {
        let has_attachment = kind == event_kind::FILE_ATTACHMENT as i32;
        let content_to_decrypt = latest_edits.get(&id).cloned().unwrap_or(original_content);

        let decrypted_content = if kind == event_kind::MLS_CHAT_MESSAGE as i32
            || kind == event_kind::PRIVATE_DIRECT_MESSAGE as i32
        {
            crate::crypto::maybe_decrypt(content_to_decrypt).await
                .unwrap_or_else(|_| "[Decryption failed]".to_string())
        } else {
            String::new()
        };

        contexts.insert(id, ReplyContext { content: decrypted_content, npub, has_attachment });
    }

    Ok(contexts)
}

/// Populate reply context for a single message.
/// Used for real-time messages that don't go through get_message_views.
pub async fn populate_reply_context(message: &mut Message) -> Result<(), String> {
    if message.replied_to.is_empty() {
        return Ok(());
    }

    let contexts = get_reply_contexts(&[message.replied_to.clone()]).await?;

    if let Some(ctx) = contexts.get(&message.replied_to) {
        message.replied_to_content = Some(ctx.content.clone());
        message.replied_to_npub = ctx.npub.clone();
        message.replied_to_has_attachment = Some(ctx.has_attachment);
    }

    Ok(())
}

// ============================================================================
// Message Views — compose full Messages from events + reactions + edits
// ============================================================================

/// Extract a single tag value from raw tags JSON without full allocation.
fn extract_tag_from_json(tags_json: &str, key: &str) -> Option<String> {
    if tags_json.len() <= 2 { return None; }
    let pattern = format!("[\"{}\"", key);
    if !tags_json.contains(&pattern) { return None; }
    let tags: Vec<Vec<String>> = serde_json::from_str(tags_json).ok()?;
    tags.into_iter()
        .find(|tag| tag.first().map(|s| s.as_str()) == Some(key))
        .and_then(|tag| tag.into_iter().nth(1))
}

/// Extract the NIP-30 `["emoji", shortcode, url]` URL from a stored
/// reaction's tags. The reaction's content must be `:shortcode:` form
/// and the matching tag's shortcode must agree — otherwise we get the
/// URL of a stray emoji tag that doesn't actually represent the
/// reaction's chosen emoji.
fn extract_reaction_emoji_url(tags: &[Vec<String>], content: &str) -> Option<String> {
    if !content.starts_with(':') || !content.ends_with(':') || content.len() < 3 {
        return None;
    }
    let sc = &content[1..content.len() - 1];
    tags.iter().find_map(|t| {
        if t.len() >= 3 && t[0] == "emoji" && t[1] == sc {
            Some(t[2].clone())
        } else {
            None
        }
    })
}

/// Extract a NIP-10 reply reference ("e" tag with "reply" marker at position 3).
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

/// Get message events with reactions, edits, and attachments composed.
///
/// This is the main "get messages" function. Queries events, fetches related
/// reactions/edits, parses attachments, applies edits, resolves reply context.
pub async fn get_message_views(
    chat_id: i64,
    limit: usize,
    offset: usize,
) -> Result<Vec<Message>, String> {
    use std::collections::HashMap;

    // Step 1: Get message events (kind 9, 14, 15)
    let message_kinds = [event_kind::MLS_CHAT_MESSAGE, event_kind::PRIVATE_DIRECT_MESSAGE, event_kind::FILE_ATTACHMENT];
    let message_events = get_events(chat_id, Some(&message_kinds), limit, offset).await?;

    if message_events.is_empty() {
        return Ok(Vec::new());
    }

    // Step 2: Get related events (reactions, edits)
    let message_ids: Vec<String> = message_events.iter().map(|e| e.id.clone()).collect();
    let related_events = get_related_events(&message_ids).await?;

    let mut reactions_by_msg: HashMap<String, Vec<Reaction>> = HashMap::new();
    let mut edits_by_msg: HashMap<String, Vec<(u64, String)>> = HashMap::new();

    for event in related_events {
        if let Some(ref_id) = &event.reference_id {
            match event.kind {
                k if k == event_kind::REACTION => {
                    let emoji_url = extract_reaction_emoji_url(&event.tags, &event.content);
                    reactions_by_msg.entry(ref_id.clone()).or_default().push(Reaction {
                        id: event.id.clone(),
                        reference_id: ref_id.clone(),
                        author_id: event.npub.clone().unwrap_or_default(),
                        emoji: event.content.clone(),
                        emoji_url,
                    });
                }
                k if k == event_kind::MESSAGE_EDIT => {
                    let decrypted = crate::crypto::maybe_decrypt(event.content.clone()).await
                        .unwrap_or_else(|_| event.content.clone());
                    edits_by_msg.entry(ref_id.clone()).or_default().push((event.created_at * 1000, decrypted));
                }
                _ => {}
            }
        }
    }

    for edits in edits_by_msg.values_mut() {
        edits.sort_by_key(|(ts, _)| *ts);
    }

    // Step 3: Parse attachments from event tags (+ legacy messages table fallback)
    let mut attachments_by_msg: HashMap<String, Vec<Attachment>> = HashMap::new();
    let mut events_needing_legacy_lookup: Vec<String> = Vec::new();

    for event in &message_events {
        if event.kind != event_kind::FILE_ATTACHMENT && event.kind != event_kind::MLS_CHAT_MESSAGE {
            continue;
        }
        if let Some(json) = event.get_tag("attachments") {
            if let Ok(atts) = serde_json::from_str::<Vec<Attachment>>(json) {
                if !atts.is_empty() {
                    attachments_by_msg.insert(event.id.clone(), atts);
                    continue;
                }
            }
        }
        if event.kind == event_kind::FILE_ATTACHMENT {
            events_needing_legacy_lookup.push(event.id.clone());
        }
    }

    // Legacy fallback: old migrated events without attachments tag
    if !events_needing_legacy_lookup.is_empty() {
        if let Ok(conn) = super::get_db_connection_guard_static() {
            let has_messages: bool = conn.query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='messages'",
                [], |row| row.get::<_, i32>(0)
            ).map(|c| c > 0).unwrap_or(false);

            if has_messages {
                for msg_id in &events_needing_legacy_lookup {
                    if let Ok(json) = conn.query_row::<String, _, _>(
                        "SELECT attachments FROM messages WHERE id = ?1",
                        rusqlite::params![msg_id], |row| row.get(0),
                    ) {
                        if let Ok(atts) = serde_json::from_str::<Vec<Attachment>>(&json) {
                            attachments_by_msg.insert(msg_id.to_string(), atts);
                        }
                    }
                }
            }
        }
    }

    // Step 4: Compose Message structs
    let mut messages = Vec::with_capacity(message_events.len());
    for event in message_events {
        let replied_to = event.get_reply_reference().unwrap_or("").to_string();
        let at = event.timestamp_ms();
        let reactions = reactions_by_msg.remove(&event.id).unwrap_or_default();
        let attachments = attachments_by_msg.remove(&event.id).unwrap_or_default();

        let original_content = if event.kind == event_kind::FILE_ATTACHMENT {
            String::new()
        } else {
            event.content.clone()
        };

        let (content, edited, edit_history) = if let Some(edits) = edits_by_msg.remove(&event.id) {
            let mut history = Vec::with_capacity(edits.len() + 1);
            history.push(crate::types::EditEntry { content: original_content.clone(), edited_at: at });
            for (ts, c) in &edits {
                history.push(crate::types::EditEntry { content: c.clone(), edited_at: *ts });
            }
            let latest = edits.last().map(|(_, c)| c.clone()).unwrap_or(original_content);
            (latest, true, Some(history))
        } else {
            (original_content, false, None)
        };

        let preview_metadata = event.preview_metadata
            .and_then(|json| serde_json::from_str(&json).ok());

        let emoji_tags = crate::types::EmojiTag::extract_from_stored(&event.tags);
        messages.push(Message {
            id: event.id, content, replied_to,
            replied_to_content: None, replied_to_npub: None, replied_to_has_attachment: None,
            preview_metadata, attachments, reactions, at,
            pending: event.pending, failed: event.failed, mine: event.mine,
            npub: event.npub, wrapper_event_id: event.wrapper_event_id,
            edited, edit_history,
            emoji_tags,
        });
    }

    // Step 5: Reply context
    let reply_ids: Vec<String> = messages.iter()
        .filter(|m| !m.replied_to.is_empty())
        .map(|m| m.replied_to.clone())
        .collect();

    if !reply_ids.is_empty() {
        let contexts = get_reply_contexts(&reply_ids).await?;
        for msg in &mut messages {
            if let Some(ctx) = contexts.get(&msg.replied_to) {
                msg.replied_to_content = Some(ctx.content.clone());
                msg.replied_to_npub = ctx.npub.clone();
                msg.replied_to_has_attachment = Some(ctx.has_attachment);
            }
        }
    }

    Ok(messages)
}

/// Get the last message for ALL chats in a single batch query.
/// Optimized for app startup (chat list sidebar).
pub async fn get_all_chats_last_messages() -> Result<std::collections::HashMap<String, Vec<Message>>, String> {
    use std::collections::HashMap;

    // Step 1: Query last message per chat via correlated subquery
    let message_events: Vec<(String, StoredEvent, String)> = {
        let conn = super::get_db_connection_guard_static()?;
        let mut stmt = conn.prepare(
            "SELECT c.chat_identifier, \
             e.id, e.kind, e.chat_id, e.user_id, e.content, e.tags, e.reference_id, \
             e.created_at, e.received_at, e.mine, e.pending, e.failed, e.wrapper_event_id, e.npub, e.preview_metadata \
             FROM chats c JOIN events e ON e.rowid = ( \
                 SELECT e2.rowid FROM events e2 WHERE e2.chat_id = c.id \
                 AND e2.kind IN (?1, ?2, ?3) \
                 ORDER BY e2.created_at DESC, e2.received_at DESC LIMIT 1) \
             WHERE c.chat_type != 1"
        ).map_err(|e| format!("Failed to prepare: {}", e))?;

        let rows = stmt.query_map(
            rusqlite::params![
                event_kind::MLS_CHAT_MESSAGE as i32,
                event_kind::PRIVATE_DIRECT_MESSAGE as i32,
                event_kind::FILE_ATTACHMENT as i32
            ],
            |row| {
                let chat_id: String = row.get(0)?;
                let tags_json: String = row.get(6)?;
                let event = StoredEvent {
                    id: row.get(1)?, kind: row.get::<_, i32>(2)? as u16,
                    chat_id: row.get(3)?, user_id: row.get(4)?, content: row.get(5)?,
                    tags: Vec::new(), // Deferred — parsed on-demand
                    reference_id: row.get(7)?,
                    created_at: row.get::<_, i64>(8)? as u64, received_at: row.get::<_, i64>(9)? as u64,
                    mine: row.get::<_, i32>(10)? != 0, pending: row.get::<_, i32>(11)? != 0,
                    failed: row.get::<_, i32>(12)? != 0, wrapper_event_id: row.get(13)?,
                    npub: row.get(14)?, preview_metadata: row.get(15)?,
                };
                Ok((chat_id, event, tags_json))
            }
        ).map_err(|e| format!("Failed to query: {}", e))?;
        rows.filter_map(|r| r.ok()).collect()
    };

    if message_events.is_empty() {
        return Ok(HashMap::new());
    }

    // Step 2: Related events (reactions, edits)
    let message_ids: Vec<String> = message_events.iter().map(|(_, e, _)| e.id.clone()).collect();
    let related_events = get_related_events(&message_ids).await?;

    let mut reactions_by_msg: HashMap<String, Vec<Reaction>> = HashMap::new();
    let mut edits_by_msg: HashMap<String, Vec<(u64, String)>> = HashMap::new();

    for event in related_events {
        if let Some(ref_id) = &event.reference_id {
            match event.kind {
                k if k == event_kind::REACTION => {
                    let emoji_url = extract_reaction_emoji_url(&event.tags, &event.content);
                    reactions_by_msg.entry(ref_id.clone()).or_default().push(Reaction {
                        id: event.id.clone(), reference_id: ref_id.clone(),
                        author_id: event.npub.clone().unwrap_or_default(),
                        emoji: event.content.clone(),
                        emoji_url,
                    });
                }
                k if k == event_kind::MESSAGE_EDIT => {
                    let decrypted = crate::crypto::maybe_decrypt(event.content.clone()).await
                        .unwrap_or_else(|_| event.content.clone());
                    edits_by_msg.entry(ref_id.clone()).or_default().push((event.created_at * 1000, decrypted));
                }
                _ => {}
            }
        }
    }
    for edits in edits_by_msg.values_mut() {
        edits.sort_by_key(|(ts, _)| *ts);
    }

    // Step 3: Parse attachments
    let mut attachments_by_msg: HashMap<String, Vec<Attachment>> = HashMap::new();
    for (_, event, tags_json) in &message_events {
        if event.kind != event_kind::FILE_ATTACHMENT && event.kind != event_kind::MLS_CHAT_MESSAGE {
            continue;
        }
        if let Some(val) = extract_tag_from_json(tags_json, "attachments") {
            if let Ok(atts) = serde_json::from_str::<Vec<Attachment>>(&val) {
                if !atts.is_empty() {
                    attachments_by_msg.insert(event.id.clone(), atts);
                }
            }
        }
    }

    // Step 4: Compose Messages grouped by chat_identifier
    let mut result: HashMap<String, Vec<Message>> = HashMap::new();

    for (chat_identifier, event, tags_json) in message_events {
        let reactions = reactions_by_msg.remove(&event.id).unwrap_or_default();
        let attachments = attachments_by_msg.remove(&event.id).unwrap_or_default();
        let replied_to = extract_reply_tag_from_json(&tags_json).unwrap_or_default();

        // Decrypt content
        let original_content = if event.kind == event_kind::MLS_CHAT_MESSAGE
            || event.kind == event_kind::PRIVATE_DIRECT_MESSAGE
        {
            crate::crypto::maybe_decrypt(event.content.clone()).await
                .unwrap_or_else(|_| "[Decryption failed]".to_string())
        } else {
            String::new()
        };

        let (content, edited, edit_history) = if let Some(edits) = edits_by_msg.remove(&event.id) {
            let latest = edits.last().map(|(_, c)| c.clone()).unwrap_or_else(|| original_content.clone());
            let history: Vec<crate::types::EditEntry> = std::iter::once(crate::types::EditEntry {
                content: original_content, edited_at: event.created_at * 1000,
            }).chain(edits.into_iter().map(|(ts, c)| crate::types::EditEntry { content: c, edited_at: ts }))
            .collect();
            (latest, true, Some(history))
        } else {
            (original_content, false, None)
        };

        let preview_metadata = event.preview_metadata
            .and_then(|json| serde_json::from_str(&json).ok());

        let emoji_tags = serde_json::from_str::<Vec<Vec<String>>>(&tags_json)
            .map(|t| crate::types::EmojiTag::extract_from_stored(&t))
            .unwrap_or_default();
        result.entry(chat_identifier).or_default().push(Message {
            id: event.id, content, replied_to,
            replied_to_content: None, replied_to_npub: None, replied_to_has_attachment: None,
            preview_metadata, attachments, reactions, at: event.created_at * 1000,
            pending: event.pending, failed: event.failed, mine: event.mine,
            npub: event.npub, wrapper_event_id: event.wrapper_event_id,
            edited, edit_history,
            emoji_tags,
        });
    }

    Ok(result)
}

/// Per-chat unread count, computed straight from the DB so it's correct even when only the last
/// message is in RAM (the boot state). Mirrors the in-memory walk-back exactly: unread = non-mine
/// messages newer than the most recent "anchor" (our own message OR the `last_read` marker,
/// whichever is latest). A never-read chat (empty `last_read`, no own message) counts all its
/// non-mine messages. Returns `chat_identifier → count`; chats with 0 unread are omitted.
/// Muted/blocked filtering is left to the caller (it lives in RAM state, cheaply).
pub async fn unread_counts() -> Result<std::collections::HashMap<String, u32>, String> {
    let conn = super::get_db_connection_guard_static()?;
    let mut stmt = conn
        .prepare(
            "SELECT c.chat_identifier, COUNT(*) AS unread \
             FROM events e JOIN chats c ON e.chat_id = c.id \
             WHERE e.kind IN (?1, ?2, ?3) AND e.mine = 0 \
               AND e.created_at > COALESCE(( \
                     SELECT MAX(e2.created_at) FROM events e2 \
                     WHERE e2.chat_id = c.id AND e2.kind IN (?1, ?2, ?3) \
                       AND (e2.mine = 1 OR e2.id = c.last_read)), 0) \
             GROUP BY c.chat_identifier",
        )
        .map_err(|e| format!("prepare unread_counts: {e}"))?;
    let rows = stmt
        .query_map(
            rusqlite::params![
                event_kind::MLS_CHAT_MESSAGE as i32,
                event_kind::PRIVATE_DIRECT_MESSAGE as i32,
                event_kind::FILE_ATTACHMENT as i32
            ],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as u32)),
        )
        .map_err(|e| format!("query unread_counts: {e}"))?;
    let mut out = std::collections::HashMap::new();
    for r in rows.flatten() {
        out.insert(r.0, r.1);
    }
    Ok(out)
}

/// Batch save messages for a chat.
pub async fn save_chat_messages(chat_id: &str, messages: &[Message]) -> Result<(), String> {
    if messages.is_empty() {
        return Ok(());
    }
    for message in messages {
        if let Err(e) = save_message(chat_id, message).await {
            eprintln!("Failed to save message {}: {}", &message.id[..8.min(message.id.len())], e);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stored_event::SystemEventType;

    static TEST_COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(71000);

    fn make_test_npub(n: u32) -> String {
        const BECH32: &[u8] = b"qpzry9x8gf2tvdw0s3jn54khce6mua7l";
        let mut payload = vec![b'q'; 58];
        let mut x = n as u64;
        let mut i = 58;
        while x > 0 && i > 0 {
            i -= 1;
            payload[i] = BECH32[(x as usize) % 32];
            x /= 32;
        }
        format!("npub1{}", std::str::from_utf8(&payload).unwrap())
    }

    fn init_test_db() -> (tempfile::TempDir, std::sync::MutexGuard<'static, ()>) {
        let guard = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        crate::db::close_database();
        let tmp = tempfile::tempdir().unwrap();
        let n = TEST_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let account = make_test_npub(n);
        std::fs::create_dir_all(tmp.path().join(&account)).unwrap();
        crate::db::set_app_data_dir(tmp.path().to_path_buf());
        crate::db::set_current_account(account.clone()).unwrap();
        crate::db::init_database(&account).unwrap();
        (tmp, guard)
    }

    fn now_secs() -> u64 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
    }

    // C-H2: a presence (join/leave) persisted from HISTORY must keep its authenticated timestamp so it
    // sorts where it happened, not at ingest-time "now"; a future-dated one is clamped so it can't jump
    // ahead of real activity.
    #[tokio::test]
    async fn system_event_stamps_authenticated_time_clamped_to_now() {
        let (_tmp, _guard) = init_test_db();
        let chat = "channel_ch2_timestamp";
        let before = now_secs();
        let past = before - 100_000;

        save_system_event_at("ev_past", chat, SystemEventType::MemberJoined, "npubX", None, past, None, None).await.unwrap();
        save_system_event_at("ev_future", chat, SystemEventType::MemberJoined, "npubX", None, before + 100_000, None, None).await.unwrap();
        let after = now_secs();

        let evs = get_system_events_for_chat(chat).unwrap();
        let past_ev = evs.iter().find(|e| e.id == "ev_past").expect("past event saved");
        assert_eq!(past_ev.created_at, past, "historical join keeps its real (authenticated) timestamp");

        let fut_ev = evs.iter().find(|e| e.id == "ev_future").expect("future event saved");
        assert!(fut_ev.created_at >= before && fut_ev.created_at <= after,
            "future-dated event clamped to local now ({} not in {}..={})", fut_ev.created_at, before, after);
    }

    // The reported bug: unread must accumulate across a restart (when only the last message per
    // chat is in RAM). The DB cutoff count must mirror the in-memory walk-back exactly.
    #[tokio::test]
    async fn unread_counts_match_walk_back_semantics() {
        let (_tmp, _guard) = init_test_db();
        let chat = "npub1contactdm";
        // `at` is ms (created_at = at/1000); use distinct seconds.
        let mk = |id: &str, secs: u64, mine: bool| Message {
            id: id.into(), content: "x".into(), at: secs * 1000, mine,
            npub: (!mine).then(|| "npub1sender".to_string()),
            ..Default::default()
        };
        let unread = || async { unread_counts().await.unwrap().get(chat).copied().unwrap_or(0) };

        // 6 contact messages, never opened/read, no own reply → all 6 unread.
        for i in 0..6u64 {
            save_message(chat, &mk(&format!("m{i}"), 1000 + i, false)).await.unwrap();
        }
        assert_eq!(unread().await, 6, "never-read backlog counts all 6");

        // 2 more arrive → 8, NOT replaced by 2 (the exact reported symptom).
        save_message(chat, &mk("m6", 2000, false)).await.unwrap();
        save_message(chat, &mk("m7", 2001, false)).await.unwrap();
        assert_eq!(unread().await, 8, "6 backlog + 2 new = 8");

        // Our own reply clears it (walk-back stops at the newest mine).
        save_message(chat, &mk("mine", 2002, true)).await.unwrap();
        assert_eq!(unread().await, 0, "own message = read up to here");

        // A contact message after our send is unread again.
        save_message(chat, &mk("m8", 2003, false)).await.unwrap();
        assert_eq!(unread().await, 1, "one new after our send");

        // last_read marker advances the cutoff just like an own message.
        {
            let conn = crate::db::get_write_connection_guard_static().unwrap();
            conn.execute(
                "UPDATE chats SET last_read = ?1 WHERE chat_identifier = ?2",
                rusqlite::params!["m8", chat],
            ).unwrap();
        }
        assert_eq!(unread().await, 0, "last_read=m8 clears all");
        save_message(chat, &mk("m9", 2004, false)).await.unwrap();
        assert_eq!(unread().await, 1, "one arrival after last_read");
    }
}
