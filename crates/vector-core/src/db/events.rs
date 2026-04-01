//! Event storage — save_event for the flat event architecture.

use crate::stored_event::{StoredEvent, event_kind};
use crate::crypto::maybe_encrypt;
use crate::types::{Message, Reaction};

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
