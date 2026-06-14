//! Message handling module.
//!
//! This module handles sending, receiving, and managing messages.

use nostr_sdk::prelude::*;
use tauri::Emitter;

use crate::net;
use crate::STATE;
use crate::util;
use crate::TAURI_APP;
use crate::nostr_client;

// Submodules
pub(crate) mod types;
mod compression;
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
    AttachmentFile, Reaction,
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

    let client = nostr_client().ok_or("Nostr client not initialized")?;
    let my_public_key = crate::my_public_key().ok_or("Public key not initialized")?;

    // NIP-30 custom-emoji tag — only valid when the content is `:shortcode:`
    // and we have a URL. Defensive: a bare emoji like "👍" stays untagged.
    let custom_emoji_tag = emoji_url.as_ref().and_then(|url| {
        if !emoji.starts_with(':') || !emoji.ends_with(':') || emoji.len() < 3 {
            return None;
        }
        let shortcode = &emoji[1..emoji.len() - 1];
        if shortcode.is_empty() || url.is_empty() { return None; }
        Some(Tag::custom(
            TagKind::custom("emoji"),
            [shortcode.to_string(), url.clone()],
        ))
    });

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
            let mut builder = EventBuilder::reaction(reaction_target, &emoji);
            if let Some(tag) = custom_emoji_tag.clone() {
                builder = builder.tag(tag);
            }
            let rumor = builder.build(my_public_key);
            let rumor_id = rumor.id.ok_or("Failed to get rumor ID")?.to_hex();
            
            // Send reaction to the receiver (routed to their inbox relays if available)
            crate::inbox_relays::send_gift_wrap(&client, &receiver_pubkey, rumor.clone(), [])
                .await
                .map_err(|e| e.to_string())?;
            
            // Self-wrap for recovery. Clone existing `client` (cheap Arc;
            // no re-fetch that could observe a different account) and
            // capture SessionGuard so a swap aborts before signing.
            let self_wrap_client = client.clone();
            let self_wrap_session = vector_core::state::SessionGuard::capture();
            tokio::spawn(async move {
                if !self_wrap_session.is_valid() { return; }
                let _ = self_wrap_client.gift_wrap(&my_public_key, rumor, []).await;
            });

            // Add reaction to local state (bech32 npub to match DB format and frontend strPubkey)
            let reaction = Reaction {
                id: rumor_id,
                reference_id: reference_id.clone(),
                author_id: my_public_key.to_bech32().unwrap_or_else(|_| my_public_key.to_hex()),
                emoji,
                emoji_url: emoji_url.clone(),
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
                    let _ = crate::db::save_message(&chat_id, &msg).await;
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
        ChatType::Community => {
            return Err("Reactions in Community channels are not yet supported".to_string());
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
    file_message(target_chat_id, String::new(), attachment_path, String::new()).await?;
    
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

    let client = nostr_client().ok_or("Nostr client not initialized")?;
    let my_public_key = crate::my_public_key().ok_or("Public key not initialized")?;
    let my_npub = my_public_key.to_bech32().map_err(|e| e.to_string())?;

    // Determine chat type and get db chat_id
    let (chat_type, db_chat_id) = {
        let state = STATE.lock().await;
        let chat = state.chats.iter().find(|c| c.id == chat_id)
            .ok_or_else(|| "Chat not found".to_string())?;
        let chat_type = chat.chat_type.clone();

        // Get db chat ID
        let db_chat_id = crate::db::get_chat_id_by_identifier(&chat_id)?;

        (chat_type, db_chat_id)
    };

    // Community channel edits ride their own envelope path (kind-3302 over the
    // Concord transport). Delegate rather than duplicate that pipeline.
    if matches!(chat_type, ChatType::Community) {
        crate::commands::community::edit_community_message(chat_id, message_id, new_content).await?;
        return Ok(String::new());
    }

    // NIP-30: resolve `:shortcode:` in the edited content against subscribed
    // packs so the edit carries `["emoji", ...]` tags — recipients render the
    // image, not literal text. Mirrors the send pipeline.
    let emoji_tags = vector_core::emoji_packs::resolve_outbound_emoji_tags(&new_content);

    // Build the edit rumor (reply ref + emoji tags)
    let reference_event = EventId::from_hex(&message_id).map_err(|e| e.to_string())?;
    let mut builder = EventBuilder::new(Kind::from_u16(event_kind::MESSAGE_EDIT), &new_content)
        .tag(Tag::event(reference_event));
    for et in &emoji_tags {
        builder = builder.tag(Tag::custom(
            TagKind::custom("emoji"),
            [et.shortcode.clone(), et.url.clone()],
        ));
    }
    let rumor = builder.build(my_public_key);
    let edit_id = rumor.id.ok_or("Failed to get edit rumor ID")?.to_hex();
    let created_at = rumor.created_at.as_secs();
    let edit_timestamp_ms = created_at * 1000;

    // Optimistic local echo BEFORE the network send (matches send_dm's on_pending):
    // the editor sees the rendered custom emoji instantly instead of waiting on the relay.
    let msg_for_emit = {
        let mut state = STATE.lock().await;
        state.update_message_in_chat(&chat_id, &message_id, |msg| {
            msg.apply_edit(new_content.clone(), edit_timestamp_ms, emoji_tags.clone());
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

    // Persist the edit (carrying its emoji tags so a reload still renders them)
    if TAURI_APP.get().is_some() {
        crate::db::save_edit_event(
            &edit_id,
            &message_id,
            &new_content,
            &emoji_tags,
            db_chat_id,
            None, // user_id derived from npub stored in event
            &my_npub,
        ).await?;
    }

    // DM gift-wrapped edit (Community already delegated above).
    let receiver_pubkey = PublicKey::from_bech32(&chat_id).map_err(|e| e.to_string())?;

    // Send edit to the receiver (routed to their inbox relays if available)
    crate::inbox_relays::send_gift_wrap(&client, &receiver_pubkey, rumor.clone(), [])
        .await
        .map_err(|e| e.to_string())?;

    // Self-wrap for recovery (same pattern as react_to_message).
    let self_wrap_client = client.clone();
    let self_wrap_session = vector_core::state::SessionGuard::capture();
    tokio::spawn(async move {
        if !self_wrap_session.is_valid() { return; }
        let _ = self_wrap_client.gift_wrap(&my_public_key, rumor, []).await;
    });

    Ok(edit_id)
}