//! Message handling module.
//!
//! This module handles sending, receiving, and managing messages.

use nostr_sdk::prelude::*;
use tauri::Emitter;

use crate::net;
use crate::STATE;
use crate::util;
use crate::TAURI_APP;
use crate::NOSTR_CLIENT;

// Submodules
mod types;
mod compression;
mod sending;
mod files;
pub mod compact;

// Re-exports (use * for Tauri commands to include generated __cmd__ macros)
pub use sending::*;
pub use files::*;
pub use types::{
    Message, ImageMetadata, Attachment,
    AttachmentFile, Reaction, EditEntry,
};
#[allow(unused_imports)]
pub use compact::{
    CompactMessage, CompactMessageVec, CompactReaction, CompactAttachment,
    AttachmentFlags, MessageFlags, NpubInterner, NO_NPUB,
};

/// Protocol-agnostic reaction function that works for both DMs and Group Chats
#[tauri::command]
pub async fn react_to_message(reference_id: String, chat_id: String, emoji: String) -> Result<bool, String> {
    use crate::chat::ChatType;
    
    let client = NOSTR_CLIENT.get().expect("Nostr client not initialized");
    let signer = client.signer().await.map_err(|e| e.to_string())?;
    let my_public_key = signer.get_public_key().await.map_err(|e| e.to_string())?;
    
    // Determine chat type
    let state = STATE.lock().await;
    let chat = state.chats.iter().find(|c| c.id == chat_id)
        .ok_or_else(|| "Chat not found".to_string())?;
    let chat_type = chat.chat_type.clone();
    drop(state);
    
    match chat_type {
        ChatType::DirectMessage => {
            // For DMs, send gift-wrapped reaction
            let reference_event = EventId::from_hex(&reference_id).map_err(|e| e.to_string())?;
            let receiver_pubkey = PublicKey::from_bech32(&chat_id).map_err(|e| e.to_string())?;
            
            // Build NIP-25 Reaction rumor
            let reaction_target = nostr_sdk::nips::nip25::ReactionTarget {
                event_id: reference_event,
                public_key: receiver_pubkey,
                coordinate: None,
                kind: Some(Kind::PrivateDirectMessage),
                relay_hint: None,
            };
            let rumor = EventBuilder::reaction(reaction_target, &emoji)
                .build(my_public_key);
            let rumor_id = rumor.id.ok_or("Failed to get rumor ID")?.to_hex();
            
            // Send reaction to the receiver (routed to their inbox relays if available)
            crate::inbox_relays::send_gift_wrap(client, &receiver_pubkey, rumor.clone(), [])
                .await
                .map_err(|e| e.to_string())?;
            
            // Send reaction to ourselves for recovery
            client
                .gift_wrap(&my_public_key, rumor, [])
                .await
                .map_err(|e| e.to_string())?;
            
            // Add reaction to local state
            let reaction = Reaction {
                id: rumor_id,
                reference_id: reference_id.clone(),
                author_id: my_public_key.to_hex(),
                emoji,
            };
            
            let msg_for_save = {
                let mut state = STATE.lock().await;
                // Use helper that handles interner access via split borrowing
                if let Some((chat_id, was_added)) = state.add_reaction_to_message(&reference_id, reaction) {
                    if was_added {
                        state.find_message(&reference_id)
                            .map(|(_, msg)| (chat_id, msg))
                    } else { None }
                } else { None }
            };

            if let Some((chat_id, msg)) = msg_for_save {
                if let Some(handle) = TAURI_APP.get() {
                    let _ = crate::db::save_message(handle.clone(), &chat_id, &msg).await;
                    let _ = handle.emit("message_update", serde_json::json!({
                        "old_id": &reference_id,
                        "message": &msg,
                        "chat_id": &chat_id
                    }));
                    return Ok(true);
                }
            }

            Ok(false)
        }
        ChatType::MlsGroup => {
            // For group chats, send reaction through MLS
            let reference_event = EventId::from_hex(&reference_id).map_err(|e| e.to_string())?;
            
            // Build reaction rumor manually (simpler than using the builder for group chats)
            let rumor = EventBuilder::new(Kind::Reaction, &emoji)
                .tag(Tag::event(reference_event))
                .build(my_public_key);
            let rumor_id = rumor.id.ok_or("Failed to get rumor ID")?.to_hex();
            
            // Send through MLS
            crate::mls::send_mls_message(&chat_id, rumor, None).await?;
            
            // Add reaction to local state
            let reaction = Reaction {
                id: rumor_id,
                reference_id: reference_id.clone(),
                author_id: my_public_key.to_hex(),
                emoji,
            };
            
            let msg_for_save = {
                let mut state = STATE.lock().await;
                // Use helper that handles interner access via split borrowing
                if let Some((returned_chat_id, was_added)) = state.add_reaction_to_message(&reference_id, reaction) {
                    if was_added {
                        state.find_message(&reference_id)
                            .map(|(_, msg)| (returned_chat_id, msg))
                    } else { None }
                } else { None }
            };

            if let Some((chat_id_clone, msg)) = msg_for_save {
                if let Some(handle) = TAURI_APP.get() {
                    let _ = crate::db::save_message(handle.clone(), &chat_id_clone, &msg).await;
                    let _ = handle.emit("message_update", serde_json::json!({
                        "old_id": &reference_id,
                        "message": &msg,
                        "chat_id": &chat_id_clone
                    }));
                    return Ok(true);
                }
            }

            Ok(false)
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

    // Extract URLs from the message
    const MAX_URLS_TO_TRY: usize = 3;
    let urls = util::extract_https_urls(&text);
    if urls.is_empty() {
        return false;
    }

    // Only try the first few URLs
    for url in urls.into_iter().take(MAX_URLS_TO_TRY) {
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
                        let _ = crate::db::save_message(handle.clone(), &chat_id, &msg).await;
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
    file_message(target_chat_id, String::new(), attachment_path).await?;
    
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
    use crate::stored_event::event_kind;

    let client = NOSTR_CLIENT.get().expect("Nostr client not initialized");
    let signer = client.signer().await.map_err(|e| e.to_string())?;
    let my_public_key = signer.get_public_key().await.map_err(|e| e.to_string())?;
    let my_npub = my_public_key.to_bech32().map_err(|e| e.to_string())?;

    // Determine chat type and get db chat_id
    let (chat_type, db_chat_id) = {
        let state = STATE.lock().await;
        let chat = state.chats.iter().find(|c| c.id == chat_id)
            .ok_or_else(|| "Chat not found".to_string())?;
        let chat_type = chat.chat_type.clone();

        // Get db chat ID
        let handle = TAURI_APP.get().ok_or("App handle not available")?;
        let db_chat_id = crate::db::get_chat_id_by_identifier(handle, &chat_id)?;

        (chat_type, db_chat_id)
    };

    // Build the edit rumor
    let reference_event = EventId::from_hex(&message_id).map_err(|e| e.to_string())?;
    let rumor = EventBuilder::new(Kind::from_u16(event_kind::MESSAGE_EDIT), &new_content)
        .tag(Tag::event(reference_event))
        .build(my_public_key);
    let edit_id = rumor.id.ok_or("Failed to get edit rumor ID")?.to_hex();
    let created_at = rumor.created_at.as_secs();

    match chat_type {
        ChatType::DirectMessage => {
            // For DMs, send gift-wrapped edit
            let receiver_pubkey = PublicKey::from_bech32(&chat_id).map_err(|e| e.to_string())?;

            // Send edit to the receiver (routed to their inbox relays if available)
            crate::inbox_relays::send_gift_wrap(client, &receiver_pubkey, rumor.clone(), [])
                .await
                .map_err(|e| e.to_string())?;

            // Send edit to ourselves for recovery
            client
                .gift_wrap(&my_public_key, rumor, [])
                .await
                .map_err(|e| e.to_string())?;
        }
        ChatType::MlsGroup => {
            // For group chats, send edit through MLS
            crate::mls::send_mls_message(&chat_id, rumor, None).await?;
        }
    }

    // Save edit event to database
    if let Some(handle) = TAURI_APP.get() {
        crate::db::save_edit_event(
            handle,
            &edit_id,
            &message_id,
            &new_content,
            db_chat_id,
            None, // user_id derived from npub stored in event
            &my_npub,
        ).await?;
    }

    // Update local state
    let edit_timestamp_ms = created_at * 1000;
    let msg_for_emit = {
        let mut state = STATE.lock().await;
        state.update_message_in_chat(&chat_id, &message_id, |msg| {
            msg.apply_edit(new_content, edit_timestamp_ms);
            msg.preview_metadata = None;
        })
    };

    if let Some(msg) = msg_for_emit {
        if let Some(handle) = TAURI_APP.get() {
            handle.emit("message_update", serde_json::json!({
                "old_id": &message_id,
                "message": msg,
                "chat_id": &chat_id
            })).ok();
        }
    }

    Ok(edit_id)
}