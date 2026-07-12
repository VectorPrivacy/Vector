//! Message handling module.
//!
//! This module handles sending, receiving, and managing messages.

use nostr_sdk::prelude::*;
use tauri::Emitter;

use crate::net;
use crate::STATE;
use crate::util;
use crate::TAURI_APP;

// Submodules
pub(crate) mod types;
pub(crate) mod compression;
pub(crate) mod sending;
pub(crate) mod files;

/// Per-session message-content caches that must be cleared on session reset.
/// Holds plaintext upload buffers, compressed-image cache, pending zip path,
/// upload-cancel flags, etc. — all of which would leak across accounts.
pub(crate) async fn clear_all_message_caches() {
    if let Ok(mut f) = files::JS_FILE_CACHE.lock() { *f = None; }
    { *files::JS_COMPRESSION_CACHE.lock().await = None; }
    if let Ok(mut p) = files::PENDING_ZIP_PATH.lock() { *p = None; }
    if let Ok(mut a) = types::ANDROID_FILE_CACHE.lock() { a.clear(); }
    { types::COMPRESSION_CACHE.lock().await.clear(); }
    // Drop any pending COMPRESSION_NOTIFY entries. These are
    // content-hash-keyed so they aren't correctness-critical, but they
    // accumulate `Arc<Notify>` allocations across the process lifetime
    // and abandoning them on session reset is the right cleanup point.
    { types::COMPRESSION_NOTIFY.lock().await.clear(); }
}

#[inline]
pub(crate) fn upload_cancel_flags()
    -> &'static std::sync::Mutex<std::collections::HashMap<String, std::sync::Arc<std::sync::atomic::AtomicBool>>>
{
    &sending::UPLOAD_CANCEL_FLAGS
}

// Re-exports (use * for Tauri commands to include generated __cmd__ macros)
pub use sending::*;
pub use files::*;
pub use types::{
    AttachmentFile,
};

/// Protocol-agnostic reaction function that works for both DMs and Group Chats.
/// `emoji_url` carries the NIP-30 image URL when reacting with a custom-pack
/// emoji — the reaction content stays `:shortcode:` and an `["emoji",
/// shortcode, url]` tag is attached so any spec-aware client renders the image.
#[tauri::command]
pub async fn react_to_message(
    reference_id: String,
    chat_id: String,
    emoji: String,
    emoji_url: Option<String>,
) -> Result<bool, String> {
    use crate::chat::ChatType;

    // Community reactions ride their own command; only DMs reach this path.
    let chat_type = {
        let state = STATE.lock().await;
        let chat = state.chats.iter().find(|c| c.id == chat_id)
            .ok_or_else(|| "Chat not found".to_string())?;
        chat.chat_type.clone()
    };

    match chat_type {
        ChatType::DirectMessage => {
            // Single source of truth — vector-core owns the reaction pipeline
            // (gift-wrap + self-wrap + optimistic state + persist + message_update emit).
            vector_core::VectorCore
                .send_reaction(&chat_id, &reference_id, &emoji, emoji_url.as_deref())
                .await
                .map(|_| true)
                .map_err(|e| e.to_string())
        }
        ChatType::Community => {
            Err("Reactions in Community channels are not yet supported".to_string())
        }
    }
}

#[tauri::command]
pub async fn fetch_msg_metadata(chat_id: String, msg_id: String) -> bool {
    // Find the message we're extracting metadata from
    let text = {
        let state = STATE.lock().await;
        let chat_idx = state.chats.iter().position(|c| c.id == chat_id);
        if let Some(idx) = chat_idx {
            state.chats[idx].messages.find_by_hex_id(&msg_id)
                .map(|m| m.content.clone())
        } else { None }
    };

    // Message might not be in backend state (e.g., loaded via get_messages_around_id)
    let text = match text {
        Some(t) => t,
        None => return false,
    };

    // Extract URLs from the message. Markdown links contribute their DESTINATION
    // only: [https://trusted.com](https://evil.io) must never preview the claimed
    // site while the click goes elsewhere.
    const MAX_URLS_TO_TRY: usize = 3;
    let urls = util::extract_https_urls(&vector_core::net::strip_md_link_claims(&text));
    if urls.is_empty() {
        return false;
    }

    // Only try the first few URLs
    for url in urls.into_iter().take(MAX_URLS_TO_TRY) {
        // Community invite links render as a dedicated in-chat card — an OG preview
        // would stack a duplicate website-style card under it.
        if url.starts_with("https://vectorapp.io/invite")
            || url.starts_with("https://www.vectorapp.io/invite")
        {
            continue;
        }
        match net::fetch_site_metadata(&url).await {
            Ok(metadata) => {
                let has_content = metadata.og_title.is_some()
                    || metadata.og_description.is_some()
                    || metadata.og_image.is_some()
                    || metadata.title.is_some()
                    || metadata.description.is_some();

                // Extracted metadata!
                if has_content {
                    // Update message with metadata
                    let msg_for_save = {
                        let mut state = STATE.lock().await;
                        state.update_message_in_chat(&chat_id, &msg_id, |msg| {
                            msg.preview_metadata = Some(Box::new(metadata));
                        })
                    };

                    if let Some(msg) = msg_for_save {
                        let handle = TAURI_APP.get().unwrap();
                        handle.emit("message_update", serde_json::json!({
                            "old_id": &msg_id,
                            "message": &msg,
                            "chat_id": &chat_id
                        })).unwrap();
                        let _ = crate::db::save_message(&chat_id, &msg).await;
                        return true;
                    }
                }
            }
            Err(_) => continue,
        }
    }
    false
}

/// Forward an attachment from one message to a different chat
/// This is used for "Play & Invite" functionality in Mini Apps
/// Returns the new message ID if successful
#[tauri::command]
pub async fn forward_attachment(
    source_msg_id: String,
    source_attachment_id: String,
    target_chat_id: String,
) -> Result<String, String> {
    // Find the source message and attachment
    let attachment_path = {
        let state = STATE.lock().await;
        
        // Search through all chats to find the message
        let mut found_path: Option<String> = None;
        for chat in &state.chats {
            if let Some(msg) = chat.messages.find_by_hex_id(&source_msg_id) {
                // Find the attachment in the message
                if let Some(attachment) = msg.attachments.iter().find(|a| a.id_eq(&source_attachment_id)) {
                    if !attachment.path.is_empty() && attachment.downloaded() {
                        found_path = Some(attachment.path.to_string());
                    }
                }
                break;
            }
        }
        
        found_path.ok_or_else(|| "Attachment not found or not downloaded".to_string())?
    };
    
    // Send the file to the target chat using the existing file_message function
    // The hash-based reuse will automatically avoid re-uploading
    file_message(target_chat_id, String::new(), attachment_path, false, String::new()).await?;
    
    // Return success - the new message ID will be emitted via the normal message flow
    Ok("forwarded".to_string())
}

/// Edit a message by sending an edit event
/// Returns the edit event ID if successful
#[tauri::command]
pub async fn edit_message(
    message_id: String,
    chat_id: String,
    new_content: String,
) -> Result<String, String> {
    use crate::chat::ChatType;

    // Determine chat type to route Community edits to their own pipeline.
    let chat_type = {
        let state = STATE.lock().await;
        let chat = state.chats.iter().find(|c| c.id == chat_id)
            .ok_or_else(|| "Chat not found".to_string())?;
        chat.chat_type.clone()
    };

    // Community channel edits ride their own envelope path (kind-3302 over the
    // Concord transport). Delegate rather than duplicate that pipeline.
    if matches!(chat_type, ChatType::Community) {
        crate::commands::community::edit_community_message(chat_id, message_id, new_content).await?;
        return Ok(String::new());
    }

    // DM edits — vector-core owns the kind-16 edit pipeline (optimistic echo +
    // persist + gift-wrap + self-wrap).
    vector_core::VectorCore
        .edit_dm(&chat_id, &message_id, &new_content)
        .await
        .map_err(|e| e.to_string())
}