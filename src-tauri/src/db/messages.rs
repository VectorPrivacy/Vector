//! Message database operations.
//!
//! This module handles:
//! - Saving messages to the flat event architecture
//! - Converting messages to stored events
//! - Batch message saving

use tauri::{AppHandle, Runtime};

use crate::Message;
use crate::stored_event::{StoredEvent, event_kind};
use super::{get_or_create_chat_id, get_or_create_user_id, save_event, event_exists, save_reaction_event};

/// Save a single message to the database (efficient for incremental updates)
///
/// This function saves to the `events` table using the flat event architecture.
/// The old `messages` table is no longer written to - it only exists for
/// backward compatibility with migrated data (read-only fallback).
pub async fn save_message<R: Runtime>(
    handle: AppHandle<R>,
    chat_id: &str,
    message: &Message
) -> Result<(), String> {
    // Get or create integer chat ID
    let chat_int_id = get_or_create_chat_id(&handle, chat_id)?;

    // Get or create integer user ID from npub
    let user_int_id = if let Some(ref npub_str) = message.npub {
        get_or_create_user_id(&handle, npub_str)?
    } else {
        None
    };

    // Save to events table (flat event architecture)
    let event = message_to_stored_event(message, chat_int_id, user_int_id);
    save_event(&handle, &event).await?;

    // Also save any reactions as separate kind=7 events
    for reaction in &message.reactions {
        // Check if reaction event already exists to avoid duplicates
        if !event_exists(&handle, &reaction.id)? {
            let user_id = get_or_create_user_id(&handle, &reaction.author_id)?;
            let is_mine = if let Ok(current_npub) = crate::account_manager::get_current_account() {
                reaction.author_id == current_npub
            } else {
                false
            };
            save_reaction_event(&handle, reaction, chat_int_id, user_id, is_mine).await?;
        }
    }

    Ok(())
}

/// Convert a Message to a StoredEvent for the flat event architecture
fn message_to_stored_event(message: &Message, chat_id: i64, user_id: Option<i64>) -> StoredEvent {
    // Determine event kind based on whether message has attachments
    let kind = if !message.attachments.is_empty() {
        event_kind::FILE_ATTACHMENT
    } else {
        event_kind::PRIVATE_DIRECT_MESSAGE
    };

    // Build tags array
    let mut tags: Vec<Vec<String>> = Vec::new();

    // Add millisecond precision tag
    let ms = message.at % 1000;
    if ms > 0 {
        tags.push(vec!["ms".to_string(), ms.to_string()]);
    }

    // Add reply reference tag if present
    if !message.replied_to.is_empty() {
        tags.push(vec![
            "e".to_string(),
            message.replied_to.clone(),
            "".to_string(),
            "reply".to_string(),
        ]);
    }

    // Add attachments as JSON tag for file messages
    if !message.attachments.is_empty() {
        if let Ok(attachments_json) = serde_json::to_string(&message.attachments) {
            tags.push(vec!["attachments".to_string(), attachments_json]);
        }
    }

    // Serialize preview_metadata if present
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
        created_at: message.at / 1000, // Convert ms to seconds
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

/// Save multiple messages for a specific chat (batch operation)
///
/// This uses the flat event architecture - each message is stored as an event.
/// Reactions are stored as separate kind=7 events.
pub async fn save_chat_messages<R: Runtime>(
    handle: AppHandle<R>,
    chat_id: &str,
    messages: &[Message]
) -> Result<(), String> {
    // Skip if no messages to save
    if messages.is_empty() {
        return Ok(());
    }

    // Save each message using the event-based save_message function
    for message in messages {
        if let Err(e) = save_message(handle.clone(), chat_id, message).await {
            eprintln!("Failed to save message {}: {}", &message.id[..8.min(message.id.len())], e);
        }
    }

    Ok(())
}