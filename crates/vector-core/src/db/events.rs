//! Event storage — save_event for the flat event architecture.

use crate::stored_event::{StoredEvent, event_kind};
use crate::crypto::maybe_encrypt;

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
