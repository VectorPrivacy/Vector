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
    let content = if event.kind == event_kind::CHAT_MESSAGE
        || event.kind == event_kind::PRIVATE_DIRECT_MESSAGE
        || event.kind == event_kind::MESSAGE_EDIT
    {
        maybe_encrypt(event.content.clone()).await
    } else {
        event.content.clone()
    };

    // UPSERT (not INSERT OR REPLACE) so a re-save (reaction/edit) UPDATES in place and PRESERVES the
    // rowid. get_messages_around's (created_at, received_at, rowid) cursor needs a stable final
    // tiebreak to page through same-timestamp bursts; INSERT OR REPLACE churns the rowid and drops rows.
    conn.execute(
        r#"
        INSERT INTO events (
            id, kind, chat_id, user_id, content, tags, reference_id,
            created_at, received_at, mine, pending, failed, wrapper_event_id, npub, preview_metadata
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
        ON CONFLICT(id) DO UPDATE SET
            kind = excluded.kind, chat_id = excluded.chat_id, user_id = excluded.user_id,
            content = excluded.content, tags = excluded.tags, reference_id = excluded.reference_id,
            created_at = excluded.created_at, received_at = excluded.received_at,
            mine = excluded.mine, pending = excluded.pending, failed = excluded.failed,
            wrapper_event_id = COALESCE(excluded.wrapper_event_id, events.wrapper_event_id),
            npub = excluded.npub, preview_metadata = excluded.preview_metadata
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

/// Extract persisted `["bot", npub]` routing tags (the write side lives in
/// `message_to_stored_event`).
fn extract_bot_tags(tags: &[Vec<String>]) -> Vec<String> {
    tags.iter()
        .filter(|t| t.len() >= 2 && t[0] == "bot")
        .map(|t| t[1].clone())
        .collect()
}

/// Parse the NIP-40 `["expiration", <unix secs>]` tag, if present. Drives the
/// self-destruct countdown + purge for messages rehydrated from the DB.
fn extract_expiration_tag(tags: &[Vec<String>]) -> Option<u64> {
    tags.iter()
        .find(|t| t.len() >= 2 && t[0] == "expiration")
        .and_then(|t| t[1].parse::<u64>().ok())
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

    // Bot routing targets (npubs) — persist so the passive "ran /cmd with
    // Bot" render survives a reload.
    for npub in &message.addressed_bots {
        tags.push(vec!["bot".to_string(), npub.clone()]);
    }

    // NIP-40 self-destruct expiry — persist so the countdown + purge survive a
    // reload. Rides the same tags column as every other message-shaped tag.
    if let Some(exp) = message.expiration {
        tags.push(vec!["expiration".to_string(), exp.to_string()]);
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
    emoji_tags: &[crate::types::EmojiTag],
    chat_id: i64,
    user_id: Option<i64>,
    npub: &str,
) -> Result<(), String> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH).unwrap();

    // Carry NIP-30 emoji tags so a reload renders the edit's custom emoji image
    // (the reload fold reads the latest edit's tags, not the original message's).
    let mut tags = vec![
        vec!["e".to_string(), message_id.to_string(), "".to_string(), "edit".to_string()],
    ];
    for et in emoji_tags {
        tags.push(vec!["emoji".to_string(), et.shortcode.clone(), et.url.clone()]);
    }

    let event = StoredEvent {
        id: edit_id.to_string(),
        kind: event_kind::MESSAGE_EDIT,
        chat_id,
        user_id,
        content: new_content.to_string(),
        tags,
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
    // If this row is a chat's read marker, retreat it to the newest surviving event before it FIRST.
    // A deleted marker would leave `last_read` dangling and collapse the unread anchor (badge stuck
    // at 99+). The UPDATE fires only when this chat's marker is exactly the row being deleted.
    if let Ok(Some((chat_row, at))) = conn.query_row(
        "SELECT chat_id, created_at FROM events WHERE id = ?1",
        rusqlite::params![event_id],
        |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
    ).optional() {
        conn.execute(
            "UPDATE chats SET last_read = COALESCE(( \
                 SELECT id FROM events WHERE chat_id = ?1 AND id != ?2 AND created_at <= ?3 \
                 ORDER BY created_at DESC, id DESC LIMIT 1), '') \
             WHERE id = ?1 AND last_read = ?2",
            rusqlite::params![chat_row, event_id, at],
        ).map_err(|e| format!("read-marker retreat: {e}"))?;
    }
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

/// The owning chat identifier, `mine` flag, and stored author (npub) of an event, or
/// `None` if the row (or DB) is absent. Lets delete-affordance resolution give paged-out
/// rows the same verdict as resident ones — residency is a cache detail, not a verdict.
pub fn event_delete_context(event_id: &str) -> Result<Option<(String, bool, Option<String>)>, String> {
    let conn = match super::get_db_connection_guard_static() {
        Ok(c) => c,
        Err(_) => return Ok(None),
    };
    conn.query_row(
        "SELECT c.chat_identifier, e.mine, e.npub \
         FROM events e JOIN chats c ON c.id = e.chat_id \
         WHERE e.id = ?1",
        rusqlite::params![event_id],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i32>(1)? != 0,
                row.get::<_, Option<String>>(2)?,
            ))
        },
    )
    .optional()
    .map_err(|e| format!("Failed to read event delete context: {}", e))
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
    // Must count the SAME kinds get_message_views returns (community chat 9, DM 14, file 15). A
    // narrower set under-counts vs. the rows actually loaded, which latches the frontend cache's
    // `isFullyLoaded` flag true and wedges the local back-pager — community channels then never
    // page DB history past the first screen.
    let count: i64 = conn.query_row(
        &format!(
            "SELECT COUNT(*) FROM events WHERE chat_id = ?1 AND kind IN ({}, {}, {})",
            event_kind::CHAT_MESSAGE, event_kind::PRIVATE_DIRECT_MESSAGE, event_kind::FILE_ATTACHMENT
        ),
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
        if event.kind == event_kind::CHAT_MESSAGE || event.kind == event_kind::PRIVATE_DIRECT_MESSAGE {
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
    /// Extension of the attachment, when the replied-to message is a file, so
    /// the reply quote can label the type even when the target is off-screen.
    pub extension: Option<String>,
}

/// Fetch reply context for a list of message IDs.
pub async fn get_reply_contexts(
    message_ids: &[String],
) -> Result<std::collections::HashMap<String, ReplyContext>, String> {
    use std::collections::HashMap;

    if message_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let (events, edits): (Vec<(String, i32, String, Option<String>, Option<String>)>, Vec<(String, String)>) = {
        let conn = super::get_db_connection_guard_static()?;

        let placeholders: String = (0..message_ids.len())
            .map(|i| format!("?{}", i + 1))
            .collect::<Vec<_>>()
            .join(",");

        // Query original messages (tags carry the file-type/name for attachment quotes)
        let sql = format!(
            "SELECT id, kind, content, npub, tags FROM events WHERE id IN ({})",
            placeholders
        );
        let mut stmt = conn.prepare(&sql)
            .map_err(|e| format!("Failed to prepare reply context query: {}", e))?;

        let params: Vec<&str> = message_ids.iter().map(|s| s.as_str()).collect();
        let params_dyn: Vec<&dyn rusqlite::ToSql> = params.iter().map(|s| s as &dyn rusqlite::ToSql).collect();

        let rows = stmt.query_map(params_dyn.as_slice(), |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i32>(1)?,
                row.get::<_, String>(2)?, row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?))
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
    for (id, kind, original_content, npub, tags) in events {
        let has_attachment = kind == event_kind::FILE_ATTACHMENT as i32;
        let content_to_decrypt = latest_edits.get(&id).cloned().unwrap_or(original_content);

        let decrypted_content = if kind == event_kind::CHAT_MESSAGE as i32
            || kind == event_kind::PRIVATE_DIRECT_MESSAGE as i32
        {
            crate::crypto::maybe_decrypt(content_to_decrypt).await
                .unwrap_or_else(|_| "[Decryption failed]".to_string())
        } else {
            String::new()
        };

        // Stored kind-15 tags carry the attachments as a JSON array under the "attachments" tag, each
        // entry with its own `extension` (the rumor's file-type/name tags are not re-stored). Pull the
        // first attachment's extension so the quote can show the file type.
        let extension = if has_attachment {
            tags.as_deref()
                .and_then(|t| serde_json::from_str::<Vec<Vec<String>>>(t).ok())
                .and_then(|parsed| parsed.into_iter()
                    .find(|t| t.first().map(|k| k == "attachments").unwrap_or(false))
                    .and_then(|t| t.into_iter().nth(1)))
                .and_then(|json| serde_json::from_str::<Vec<serde_json::Value>>(&json).ok())
                .and_then(|atts| atts.into_iter().next())
                .and_then(|a| a.get("extension").and_then(|e| e.as_str()).map(str::to_lowercase))
                .filter(|e| !e.is_empty())
        } else {
            None
        };

        contexts.insert(id, ReplyContext { content: decrypted_content, npub, has_attachment, extension });
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
        message.replied_to_attachment_extension = ctx.extension.clone();
    }

    Ok(())
}

/// Whether `event_id` is one of our own messages (`mine = 1`). A reply to our
/// own message is an implicit ping, so notifications treat it like a direct
/// @mention (breaks through a muted channel). Missing row → not ours → false.
pub fn is_own_event(event_id: &str) -> bool {
    let Ok(conn) = super::get_db_connection_guard_static() else {
        return false;
    };
    conn.query_row(
        "SELECT mine FROM events WHERE id = ?1",
        [event_id],
        |row| row.get::<_, i64>(0),
    )
    .map(|mine| mine == 1)
    .unwrap_or(false)
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


/// A stored reaction author written as 64-char hex (an early v2 ingest) reads
/// back as the npub the frontend contract expects — self-heals old rows with no
/// migration; a bech32 or unknown value passes through untouched.
fn normalize_reaction_author(author: String) -> String {
    if author.len() == 64 && author.bytes().all(|b| b.is_ascii_hexdigit()) {
        if let Ok(pk) = nostr_sdk::prelude::PublicKey::from_hex(&author) {
            use nostr_sdk::prelude::ToBech32;
            if let Ok(npub) = pk.to_bech32() {
                return npub;
            }
        }
    }
    author
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
    // Step 1: Get message events (kind 9, 14, 15)
    let message_kinds = [event_kind::CHAT_MESSAGE, event_kind::PRIVATE_DIRECT_MESSAGE, event_kind::FILE_ATTACHMENT];
    let message_events = get_events(chat_id, Some(&message_kinds), limit, offset).await?;

    compose_message_views(message_events).await
}

/// Compose Message views from already-fetched message events (kind 9/14/15):
/// fetch related reactions/edits, parse attachments, apply edits, resolve reply
/// context. Shared by `get_message_views` (offset pager) and `get_messages_around`
/// (anchored window). Input order is preserved in the output.
async fn compose_message_views(message_events: Vec<StoredEvent>) -> Result<Vec<Message>, String> {
    use std::collections::HashMap;

    if message_events.is_empty() {
        return Ok(Vec::new());
    }

    // Step 2: Get related events (reactions, edits)
    let message_ids: Vec<String> = message_events.iter().map(|e| e.id.clone()).collect();
    let related_events = get_related_events(&message_ids).await?;

    let mut reactions_by_msg: HashMap<String, Vec<Reaction>> = HashMap::new();
    let mut edits_by_msg: HashMap<String, Vec<(u64, String, Vec<crate::types::EmojiTag>)>> = HashMap::new();

    for event in related_events {
        if let Some(ref_id) = &event.reference_id {
            match event.kind {
                k if k == event_kind::REACTION => {
                    let emoji_url = extract_reaction_emoji_url(&event.tags, &event.content);
                    reactions_by_msg.entry(ref_id.clone()).or_default().push(Reaction {
                        id: event.id.clone(),
                        reference_id: ref_id.clone(),
                        author_id: normalize_reaction_author(event.npub.clone().unwrap_or_default()),
                        emoji: event.content.clone(),
                        emoji_url,
                    });
                }
                k if k == event_kind::MESSAGE_EDIT => {
                    let decrypted = crate::crypto::maybe_decrypt(event.content.clone()).await
                        .unwrap_or_else(|_| event.content.clone());
                    let edit_emoji = crate::types::EmojiTag::extract_from_stored(&event.tags);
                    edits_by_msg.entry(ref_id.clone()).or_default().push((event.created_at * 1000, decrypted, edit_emoji));
                }
                _ => {}
            }
        }
    }

    for edits in edits_by_msg.values_mut() {
        edits.sort_by_key(|(ts, _, _)| *ts);
    }

    // Step 3: Parse attachments from event tags (+ legacy messages table fallback)
    let mut attachments_by_msg: HashMap<String, Vec<Attachment>> = HashMap::new();
    let mut events_needing_legacy_lookup: Vec<String> = Vec::new();

    for event in &message_events {
        if event.kind != event_kind::FILE_ATTACHMENT && event.kind != event_kind::CHAT_MESSAGE {
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

        // Edits carry their own emoji tags; the newest edit's tags win so the
        // displayed (latest) content renders its custom emoji, not the original's.
        let original_emoji = crate::types::EmojiTag::extract_from_stored(&event.tags);
        let (content, edited, edit_history, emoji_tags) = if let Some(edits) = edits_by_msg.remove(&event.id) {
            let mut history = Vec::with_capacity(edits.len() + 1);
            history.push(crate::types::EditEntry { content: original_content.clone(), edited_at: at });
            for (ts, c, _) in &edits {
                history.push(crate::types::EditEntry { content: c.clone(), edited_at: *ts });
            }
            let (latest, latest_emoji) = edits.last()
                .map(|(_, c, e)| (c.clone(), e.clone()))
                .unwrap_or_else(|| (original_content.clone(), original_emoji.clone()));
            (latest, true, Some(history), latest_emoji)
        } else {
            (original_content, false, None, original_emoji)
        };

        let preview_metadata = event.preview_metadata
            .and_then(|json| serde_json::from_str(&json).ok());

        let addressed_bots = extract_bot_tags(&event.tags);
        let expiration = extract_expiration_tag(&event.tags);
        messages.push(Message {
            expiration,
            id: event.id, content, replied_to,
            replied_to_content: None, replied_to_npub: None, replied_to_has_attachment: None,
            replied_to_attachment_extension: None,
            preview_metadata, attachments, reactions, at,
            pending: event.pending, failed: event.failed, mine: event.mine,
            npub: event.npub, wrapper_event_id: event.wrapper_event_id,
            edited, edit_history,
            emoji_tags,
            addressed_bots,
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
                msg.replied_to_attachment_extension = ctx.extension.clone();
            }
        }
    }

    Ok(messages)
}

/// Anchored (random-access) message window: load `before` messages up to and
/// including the anchor, plus `after` messages strictly newer than it. O(window)
/// regardless of how deep the anchor sits in the chat — unlike the offset pager,
/// which is O(depth) to reach a far-back message.
///
/// Returns ASC by `created_at` (oldest first), composed with reactions/edits/
/// attachments. Errs if the anchor id isn't in the DB so the caller can fall back.
pub async fn get_messages_around(
    chat_id: i64,
    anchor_id: &str,
    before: usize,
    after: usize,
) -> Result<Vec<Message>, String> {
    let message_kinds = [event_kind::CHAT_MESSAGE, event_kind::PRIVATE_DIRECT_MESSAGE, event_kind::FILE_ATTACHMENT];

    let message_events: Vec<StoredEvent> = {
        let conn = super::get_db_connection_guard_static()?;

        // Resolve the anchor's FULL sort key (created_at, received_at, rowid). Paging by created_at
        // alone wedges on a wall of equal timestamps (a message burst): the query keeps returning the
        // same newest-N of the cluster, so back-paging stalls before reaching older history. The
        // (received_at, rowid) tiebreak — rowid being the unique final key — gives a strict total
        // order, so every page steps strictly past the previous, through any same-timestamp cluster.
        let (anchor_at, anchor_rt, anchor_rowid): (i64, i64, i64) = conn.query_row(
            "SELECT created_at, received_at, rowid FROM events WHERE id = ?1",
            rusqlite::params![anchor_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ).map_err(|e| format!("Anchor message not found: {}", e))?;

        // Kinds occupy ?2..?4; then ?5 created_at, ?6 received_at, ?7 rowid, ?8 limit.
        let kind_placeholders: String = (0..message_kinds.len())
            .map(|i| format!("?{}", i + 2))
            .collect::<Vec<_>>()
            .join(",");
        let cols = "id, kind, chat_id, user_id, content, tags, reference_id, \
                    created_at, received_at, mine, pending, failed, wrapper_event_id, npub, preview_metadata";

        // Older incl. anchor: strict key <= anchor key; newest-first then reverse to ASC.
        let older_sql = format!(
            "SELECT {} FROM events WHERE chat_id = ?1 AND kind IN ({}) \
             AND (created_at < ?5 OR (created_at = ?5 AND (received_at < ?6 \
                  OR (received_at = ?6 AND rowid <= ?7)))) \
             ORDER BY created_at DESC, received_at DESC, rowid DESC LIMIT ?8",
            cols, kind_placeholders
        );
        let mut older_stmt = conn.prepare(&older_sql)
            .map_err(|e| format!("Failed to prepare older window query: {}", e))?;
        let older_rows = older_stmt.query_map(
            rusqlite::params![
                chat_id,
                message_kinds[0] as i32, message_kinds[1] as i32, message_kinds[2] as i32,
                anchor_at, anchor_rt, anchor_rowid, before as i64
            ],
            parse_event_row,
        ).map_err(|e| format!("Failed to query older window: {}", e))?;
        let mut older: Vec<StoredEvent> = older_rows.filter_map(|r| r.ok()).collect();
        older.reverse(); // DESC -> ASC

        // Newer: strictly after the anchor key.
        let newer_sql = format!(
            "SELECT {} FROM events WHERE chat_id = ?1 AND kind IN ({}) \
             AND (created_at > ?5 OR (created_at = ?5 AND (received_at > ?6 \
                  OR (received_at = ?6 AND rowid > ?7)))) \
             ORDER BY created_at ASC, received_at ASC, rowid ASC LIMIT ?8",
            cols, kind_placeholders
        );
        let mut newer_stmt = conn.prepare(&newer_sql)
            .map_err(|e| format!("Failed to prepare newer window query: {}", e))?;
        let newer_rows = newer_stmt.query_map(
            rusqlite::params![
                chat_id,
                message_kinds[0] as i32, message_kinds[1] as i32, message_kinds[2] as i32,
                anchor_at, anchor_rt, anchor_rowid, after as i64
            ],
            parse_event_row,
        ).map_err(|e| format!("Failed to query newer window: {}", e))?;
        let newer: Vec<StoredEvent> = newer_rows.filter_map(|r| r.ok()).collect();

        older.into_iter().chain(newer).collect()
    };

    // Decrypt message content (mirror get_events).
    let mut decrypted = Vec::with_capacity(message_events.len());
    for mut event in message_events {
        if event.kind == event_kind::CHAT_MESSAGE || event.kind == event_kind::PRIVATE_DIRECT_MESSAGE {
            event.content = crate::crypto::maybe_decrypt(event.content).await
                .unwrap_or_else(|_| "[Decryption failed]".to_string());
        }
        decrypted.push(event);
    }

    compose_message_views(decrypted).await
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
                event_kind::CHAT_MESSAGE as i32,
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
    let mut edits_by_msg: HashMap<String, Vec<(u64, String, Vec<crate::types::EmojiTag>)>> = HashMap::new();

    for event in related_events {
        if let Some(ref_id) = &event.reference_id {
            match event.kind {
                k if k == event_kind::REACTION => {
                    let emoji_url = extract_reaction_emoji_url(&event.tags, &event.content);
                    reactions_by_msg.entry(ref_id.clone()).or_default().push(Reaction {
                        id: event.id.clone(), reference_id: ref_id.clone(),
                        author_id: normalize_reaction_author(event.npub.clone().unwrap_or_default()),
                        emoji: event.content.clone(),
                        emoji_url,
                    });
                }
                k if k == event_kind::MESSAGE_EDIT => {
                    let decrypted = crate::crypto::maybe_decrypt(event.content.clone()).await
                        .unwrap_or_else(|_| event.content.clone());
                    let edit_emoji = crate::types::EmojiTag::extract_from_stored(&event.tags);
                    edits_by_msg.entry(ref_id.clone()).or_default().push((event.created_at * 1000, decrypted, edit_emoji));
                }
                _ => {}
            }
        }
    }
    for edits in edits_by_msg.values_mut() {
        edits.sort_by_key(|(ts, _, _)| *ts);
    }

    // Step 3: Parse attachments
    let mut attachments_by_msg: HashMap<String, Vec<Attachment>> = HashMap::new();
    for (_, event, tags_json) in &message_events {
        if event.kind != event_kind::FILE_ATTACHMENT && event.kind != event_kind::CHAT_MESSAGE {
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
        let original_content = if event.kind == event_kind::CHAT_MESSAGE
            || event.kind == event_kind::PRIVATE_DIRECT_MESSAGE
        {
            crate::crypto::maybe_decrypt(event.content.clone()).await
                .unwrap_or_else(|_| "[Decryption failed]".to_string())
        } else {
            String::new()
        };

        let stored_tags = serde_json::from_str::<Vec<Vec<String>>>(&tags_json).unwrap_or_default();
        let original_emoji = crate::types::EmojiTag::extract_from_stored(&stored_tags);
        let addressed_bots = extract_bot_tags(&stored_tags);
        let expiration = extract_expiration_tag(&stored_tags);
        // Newest edit's emoji tags win so the latest content renders correctly.
        let (content, edited, edit_history, emoji_tags) = if let Some(edits) = edits_by_msg.remove(&event.id) {
            let (latest, latest_emoji) = edits.last()
                .map(|(_, c, e)| (c.clone(), e.clone()))
                .unwrap_or_else(|| (original_content.clone(), original_emoji.clone()));
            let history: Vec<crate::types::EditEntry> = std::iter::once(crate::types::EditEntry {
                content: original_content, edited_at: event.created_at * 1000,
            }).chain(edits.into_iter().map(|(ts, c, _)| crate::types::EditEntry { content: c, edited_at: ts }))
            .collect();
            (latest, true, Some(history), latest_emoji)
        } else {
            (original_content, false, None, original_emoji)
        };

        let preview_metadata = event.preview_metadata
            .and_then(|json| serde_json::from_str(&json).ok());

        result.entry(chat_identifier).or_default().push(Message {
            expiration,
            id: event.id, content, replied_to,
            replied_to_content: None, replied_to_npub: None, replied_to_has_attachment: None,
            replied_to_attachment_extension: None,
            preview_metadata, attachments, reactions, at: event.created_at * 1000,
            pending: event.pending, failed: event.failed, mine: event.mine,
            npub: event.npub, wrapper_event_id: event.wrapper_event_id,
            edited, edit_history,
            emoji_tags,
            addressed_bots,
        });
    }

    // Step 5: Reply context — the openChat pre-paint renders this boot last-message
    // synchronously (before the richer get_message_views load lands), so without
    // context here a reply shows its quote only on the second open.
    let reply_ids: Vec<String> = result.values()
        .flatten()
        .filter(|m| !m.replied_to.is_empty())
        .map(|m| m.replied_to.clone())
        .collect();

    if !reply_ids.is_empty() {
        let contexts = get_reply_contexts(&reply_ids).await?;
        for msg in result.values_mut().flatten() {
            if let Some(ctx) = contexts.get(&msg.replied_to) {
                msg.replied_to_content = Some(ctx.content.clone());
                msg.replied_to_npub = ctx.npub.clone();
                msg.replied_to_has_attachment = Some(ctx.has_attachment);
                msg.replied_to_attachment_extension = ctx.extension.clone();
            }
        }
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
    // The `last_read` anchor is kind-agnostic on purpose: a "read to here" marker can land on a
    // system event (kind 30078), and it must still cut the count by its timestamp, or the badge
    // wedges at a permanent 99+. Only the own-message anchor is kind-filtered.
    let mut stmt = conn
        .prepare(
            "SELECT c.chat_identifier, COUNT(*) AS unread \
             FROM events e JOIN chats c ON e.chat_id = c.id \
             WHERE e.kind IN (?1, ?2, ?3) AND e.mine = 0 \
               AND e.created_at > COALESCE(( \
                     SELECT MAX(e2.created_at) FROM events e2 \
                     WHERE e2.chat_id = c.id \
                       AND ((e2.mine = 1 AND e2.kind IN (?1, ?2, ?3)) OR e2.id = c.last_read)), 0) \
             GROUP BY c.chat_identifier",
        )
        .map_err(|e| format!("prepare unread_counts: {e}"))?;
    let rows = stmt
        .query_map(
            rusqlite::params![
                event_kind::CHAT_MESSAGE as i32,
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

/// What [`compute_unread_anchor`] decided a chat's read marker should become to surface its newest
/// contact message as unread. Computed from the full DB history (RAM may hold only a preview
/// message for an unopened community).
#[derive(Debug, PartialEq)]
pub enum UnreadMark {
    /// Nothing to surface: no contact message, or a strictly-newer own message (we spoke last).
    NoOp,
    /// Reset to the never-read anchor: the target is the chat's earliest message.
    Clear,
    /// Retreat `last_read` to this event id (the newest message in a strictly earlier second).
    Anchor(String),
}

/// Decide how to mark `chat_identifier` unread. Anchors on the newest message strictly before the
/// newest contact message's second — the count query compares whole seconds with a strict `>`, so a
/// same-second anchor would leave the target on the boundary and it would read as caught-up.
pub async fn compute_unread_anchor(chat_identifier: &str) -> Result<UnreadMark, String> {
    let conn = super::get_db_connection_guard_static()?;
    let (k0, k1, k2) = (
        event_kind::CHAT_MESSAGE as i32,
        event_kind::PRIVATE_DIRECT_MESSAGE as i32,
        event_kind::FILE_ATTACHMENT as i32,
    );
    // Newest non-mine message second (the target) and newest overall (to detect we spoke last).
    let (target_ts, newest_ts): (Option<i64>, Option<i64>) = conn
        .query_row(
            "SELECT MAX(CASE WHEN e.mine = 0 THEN e.created_at END), MAX(e.created_at) \
             FROM events e JOIN chats c ON e.chat_id = c.id \
             WHERE c.chat_identifier = ?1 AND e.kind IN (?2, ?3, ?4)",
            rusqlite::params![chat_identifier, k0, k1, k2],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|e| format!("unread anchor target: {e}"))?;

    let target_ts = match target_ts {
        Some(t) => t,
        None => return Ok(UnreadMark::NoOp), // no contact message to surface
    };
    if newest_ts.map_or(false, |n| n > target_ts) {
        return Ok(UnreadMark::NoOp); // a strictly-newer own message → we spoke last
    }

    let anchor_id: Option<String> = conn
        .query_row(
            "SELECT e.id FROM events e JOIN chats c ON e.chat_id = c.id \
             WHERE c.chat_identifier = ?1 AND e.kind IN (?2, ?3, ?4) AND e.created_at < ?5 \
             ORDER BY e.created_at DESC LIMIT 1",
            rusqlite::params![chat_identifier, k0, k1, k2, target_ts],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| format!("unread anchor prev: {e}"))?;

    Ok(match anchor_id {
        Some(id) => UnreadMark::Anchor(id),
        None => UnreadMark::Clear,
    })
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
        // Each test rebinds to a fresh per-account DB; the row-id caches are per-account, so a stale
        // entry (e.g. a shared author npub) would point into the prior test's DB and FK-fail the insert.
        crate::db::clear_id_caches();
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

    // Delete-affordance resolution must work on paged-out rows: the events table is the
    // fallback source for (chat, mine, author) when a message isn't STATE-resident.
    #[tokio::test]
    async fn event_delete_context_resolves_from_db() {
        let (_tmp, _guard) = init_test_db();
        let chat = "npub1contactdc";

        let mine_msg = Message { id: "dc_mine".into(), content: "x".into(), at: 1_000, mine: true, ..Default::default() };
        let theirs = Message {
            id: "dc_theirs".into(), content: "y".into(), at: 2_000, mine: false,
            npub: Some("npub1sender".to_string()),
            ..Default::default()
        };
        save_message(chat, &mine_msg).await.unwrap();
        save_message(chat, &theirs).await.unwrap();

        let (chat_id, mine, _author) = event_delete_context("dc_mine").unwrap().expect("own row resolves");
        assert_eq!(chat_id, chat);
        assert!(mine);

        let (chat_id, mine, author) = event_delete_context("dc_theirs").unwrap().expect("contact row resolves");
        assert_eq!(chat_id, chat);
        assert!(!mine);
        assert_eq!(author.as_deref(), Some("npub1sender"));

        assert!(event_delete_context("dc_absent").unwrap().is_none(), "unknown id is None, not an error");
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

    // Mark-as-unread anchors from the FULL DB history (a community row often holds only a preview
    // message in RAM). The anchor lands strictly before the target's second so the count query's
    // strict `>` still counts the newest contact message. Covers the community repro + edge cases.
    #[tokio::test]
    async fn compute_unread_anchor_covers_the_cases() {
        let (_tmp, _guard) = init_test_db();
        let mk = |id: &str, secs: u64, mine: bool| Message {
            id: id.into(), content: "x".into(), at: secs * 1000, mine,
            npub: (!mine).then(|| "npub1sender".to_string()),
            ..Default::default()
        };
        let unread = |chat: &'static str| async move {
            unread_counts().await.unwrap().get(chat).copied().unwrap_or(0)
        };
        let set_lr = |chat: &str, lr: &str| {
            let conn = crate::db::get_write_connection_guard_static().unwrap();
            conn.execute("UPDATE chats SET last_read = ?1 WHERE chat_identifier = ?2",
                rusqlite::params![lr, chat]).unwrap();
        };

        // (A) The community repro: we spoke long ago, they kept talking. Anchor = second-newest
        // contact message → exactly one unread, whatever the RAM cache held.
        let a = "npub1anchorA";
        save_message(a, &mk("a_mine", 1000, true)).await.unwrap();
        for i in 0..8u64 { save_message(a, &mk(&format!("a{i}"), 2000 + i, false)).await.unwrap(); }
        assert_eq!(compute_unread_anchor(a).await.unwrap(), UnreadMark::Anchor("a6".into()));
        set_lr(a, "a6");
        assert_eq!(unread(a).await, 1, "A: newest contact message is the sole unread");

        // (B) We spoke last → NoOp (no phantom badge, no snap-back jiggle).
        let b = "npub1anchorB";
        save_message(b, &mk("b0", 2000, false)).await.unwrap();
        save_message(b, &mk("b_mine", 2001, true)).await.unwrap();
        assert_eq!(compute_unread_anchor(b).await.unwrap(), UnreadMark::NoOp);

        // (C) Same-second tail: the two newest share a second. The anchor must skip to a strictly
        // earlier second, so both same-second messages surface instead of snapping back to read.
        let c = "npub1anchorC";
        save_message(c, &mk("c0", 3000, false)).await.unwrap();
        save_message(c, &mk("c1", 3005, false)).await.unwrap();
        save_message(c, &mk("c2", 3005, false)).await.unwrap();
        assert_eq!(compute_unread_anchor(c).await.unwrap(), UnreadMark::Anchor("c0".into()));
        set_lr(c, "c0");
        assert_eq!(unread(c).await, 2, "C: same-second tail both count");

        // (D) The newest contact message is the chat's first → Clear (never-read) → it still counts.
        let d = "npub1anchorD";
        save_message(d, &mk("d0", 4000, false)).await.unwrap();
        assert_eq!(compute_unread_anchor(d).await.unwrap(), UnreadMark::Clear);
        set_lr(d, "");
        assert_eq!(unread(d).await, 1, "D: lone contact message surfaces");

        // (E) No contact message at all (only our own) → NoOp.
        let e = "npub1anchorE";
        save_message(e, &mk("e_mine", 5000, true)).await.unwrap();
        assert_eq!(compute_unread_anchor(e).await.unwrap(), UnreadMark::NoOp);
    }

    // Regression: a "read to here" marker that lands on a system event (kind 30078, not a counted
    // kind, e.g. the windowed jump-reveal path marking off the raw tail) must still clear unread.
    // The anchor keys off the marker row's time whatever its kind, so it can't wedge at 99+.
    #[tokio::test]
    async fn unread_clears_when_last_read_is_a_system_event() {
        let (_tmp, _guard) = init_test_db();
        let chat = "npub1sysevtdm";
        let mk = |id: &str, secs: u64| Message {
            id: id.into(), content: "x".into(), at: secs * 1000, mine: false,
            npub: Some("npub1sender".to_string()), ..Default::default()
        };
        let unread = || async { unread_counts().await.unwrap().get(chat).copied().unwrap_or(0) };

        // 5 contact messages, never read → all unread. No own message, so the ONLY viable anchor
        // is last_read (this is the case that used to stick at a permanent count).
        for i in 0..5u64 {
            save_message(chat, &mk(&format!("m{i}"), 1000 + i)).await.unwrap();
        }
        assert_eq!(unread().await, 5, "never-read backlog");

        // A system event is the newest row (a join notification, after every contact message).
        save_system_event_at("sysev", chat, SystemEventType::MemberJoined, "npubX", None, 2000, None, None).await.unwrap();

        // last_read pinned to that system event (kind 30078) — the reported bad state.
        {
            let conn = crate::db::get_write_connection_guard_static().unwrap();
            conn.execute(
                "UPDATE chats SET last_read = ?1 WHERE chat_identifier = ?2",
                rusqlite::params!["sysev", chat],
            ).unwrap();
        }
        assert_eq!(unread().await, 0, "read marker on a system event still clears the badge");
    }

    // Deleting a message adjusts unread correctly: removing an UNREAD message drops the count by
    // one, removing the read MARKER retreats it to the prior message (never collapses to 99+), and
    // removing the last read message clears the marker without over-counting.
    #[tokio::test]
    async fn deleting_a_message_adjusts_unread_without_wedging() {
        let (_tmp, _guard) = init_test_db();
        let chat = "npub1delunread";
        let mk = |id: &str, secs: u64| Message {
            id: id.into(), content: "x".into(), at: secs * 1000, mine: false,
            npub: Some("npub1sender".to_string()), ..Default::default()
        };
        let unread = || async { unread_counts().await.unwrap().get(chat).copied().unwrap_or(0) };
        let set_marker = |id: &str| {
            let conn = crate::db::get_write_connection_guard_static().unwrap();
            conn.execute("UPDATE chats SET last_read = ?1 WHERE chat_identifier = ?2",
                rusqlite::params![id, chat]).unwrap();
        };
        let marker = || -> String {
            let conn = crate::db::get_db_connection_guard_static().unwrap();
            conn.query_row("SELECT last_read FROM chats WHERE chat_identifier = ?1",
                rusqlite::params![chat], |r| r.get::<_, String>(0)).unwrap()
        };

        // m0..m5, read up to m1 → m2,m3,m4,m5 unread.
        for i in 0..6u64 { save_message(chat, &mk(&format!("m{i}"), 1000 + i)).await.unwrap(); }
        set_marker("m1");
        assert_eq!(unread().await, 4, "m2..m5 unread");

        // Delete an UNREAD mid-block message → badge drops by one, marker untouched.
        delete_event("m3").await.unwrap();
        assert_eq!(unread().await, 3, "one unread deleted → badge minus one");
        assert_eq!(marker(), "m1", "deleting an unread message leaves the marker alone");

        // Delete the read MARKER → retreats to the prior surviving message (m0), unread unchanged.
        delete_event("m1").await.unwrap();
        assert_eq!(marker(), "m0", "marker retreats to the newest survivor before it");
        assert_eq!(unread().await, 3, "retreat keeps the count, no collapse to 99+");

        // Delete the last surviving read message → marker clears, still exactly the unread block.
        delete_event("m0").await.unwrap();
        assert_eq!(marker(), "", "no earlier survivor → marker clears");
        assert_eq!(unread().await, 3, "cleared marker counts only the true unread survivors");
    }

    // Edits are event-sourced for BOTH transports: a MESSAGE_EDIT event folds into the target's
    // history on reload (latest content + revisions + the edit's own emoji). Community used to
    // overwrite the row and lose all of this — this locks in the unified fold.
    #[tokio::test]
    async fn edit_event_folds_into_history_on_reload() {
        let (_tmp, _guard) = init_test_db();
        let chat = "channel_edit_fold";
        save_message(chat, &Message {
            id: "orig1".into(), content: "original".into(), at: 5_000_000,
            npub: Some("npub1author".into()), ..Default::default()
        }).await.unwrap();

        let cid = crate::db::id_cache::get_chat_id_by_identifier(chat).unwrap();
        let emoji = vec![crate::types::EmojiTag { shortcode: "wave".into(), url: "u/wave".into() }];
        save_edit_event("edit1", "orig1", "edited :wave:", &emoji, cid, None, "npub1author").await.unwrap();

        let m = get_message_views(cid, 50, 0).await.unwrap()
            .into_iter().find(|m| m.id == "orig1").expect("message reloaded");
        assert!(m.edited, "folded edit sets the edited flag");
        let h = m.edit_history.as_ref().expect("history reconstructed from the edit event");
        assert_eq!(h.len(), 2, "original + one edit");
        assert_eq!(h[0].content, "original");
        assert_eq!(h[1].content, "edited :wave:");
        assert_eq!(m.content, "edited :wave:", "latest revision is the displayed content");
        assert_eq!(m.emoji_tags.len(), 1, "the edit's own emoji folds onto the message");
        assert_eq!(m.emoji_tags[0].shortcode, "wave");
    }
}
