//! Message sending — NIP-17 gift-wrapped DMs (text and file attachments).
//!
//! This is the core send pipeline used by all Vector interfaces (GUI, CLI, SDK).
//! The Tauri app calls these functions through thin command wrappers.

use std::sync::Arc;
use nostr_sdk::prelude::*;

use crate::state::{NOSTR_CLIENT, MY_SECRET_KEY, MY_PUBLIC_KEY, STATE};
use crate::types::{Message, Attachment};
use crate::crypto;
use crate::traits::emit_event;

/// Result of sending a message.
#[derive(serde::Serialize, Clone, Debug)]
pub struct SendResult {
    /// The pending ID used while sending
    pub pending_id: String,
    /// The real event ID after successful send (None if failed)
    pub event_id: Option<String>,
    /// The chat ID (receiver npub for DMs)
    pub chat_id: String,
}

/// Send a NIP-17 gift-wrapped text DM.
///
/// Flow: build rumor → gift-wrap → send to inbox relays → update state → save to DB.
/// This is the same pipeline used by Vector GUI.
pub async fn send_dm(
    receiver_npub: &str,
    content: &str,
    reply_to: Option<&str>,
) -> Result<SendResult, String> {
    let client = NOSTR_CLIENT.get().ok_or("Not logged in")?;
    let my_pk = *MY_PUBLIC_KEY.get().ok_or("Public key not set")?;

    // Generate pending ID from nanosecond timestamp
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH).unwrap();
    let pending_id = format!("pending-{}", now.as_nanos());

    // Parse receiver
    let receiver = PublicKey::from_bech32(receiver_npub)
        .map_err(|e| format!("Invalid npub: {}", e))?;

    // Build pending message and add to state
    let msg = Message {
        id: pending_id.clone(),
        content: content.to_string(),
        replied_to: reply_to.unwrap_or("").to_string(),
        at: now.as_millis() as u64,
        pending: true,
        mine: true,
        npub: my_pk.to_bech32().ok(),
        ..Default::default()
    };

    {
        let mut state = STATE.lock().await;
        state.add_message_to_participant(receiver_npub, msg.clone());
    }

    // Emit to UI
    emit_event("message_new", &serde_json::json!({
        "message": &msg,
        "chat_id": receiver_npub
    }));

    // Build the rumor
    let milliseconds = now.as_millis() % 1000;
    let mut rumor = EventBuilder::private_msg_rumor(receiver, content);

    // Add reply tag if present
    if let Some(reply_id) = reply_to {
        if !reply_id.is_empty() {
            rumor = rumor.tag(Tag::custom(
                TagKind::e(),
                [reply_id.to_string(), String::new(), "reply".to_string()],
            ));
        }
    }

    // Add millisecond precision tag
    let rumor = rumor.tag(Tag::custom(TagKind::custom("ms"), [milliseconds.to_string()]));
    let built_rumor = rumor.build(my_pk);
    let event_id = built_rumor.id.ok_or("Rumor has no id")?.to_hex();

    // Send via gift-wrap (uses inbox relay resolution)
    match crate::inbox_relays::send_gift_wrap(client, &receiver, built_rumor, []).await {
        Ok(_) => {
            // Success — finalize pending message
            let msg_for_save = {
                let mut state = STATE.lock().await;
                state.finalize_pending_message(receiver_npub, &pending_id, &event_id)
            };

            if let Some((old_id, finalized_msg)) = msg_for_save {
                emit_event("message_update", &serde_json::json!({
                    "old_id": old_id,
                    "message": &finalized_msg,
                    "chat_id": receiver_npub
                }));
            }

            Ok(SendResult {
                pending_id,
                event_id: Some(event_id),
                chat_id: receiver_npub.to_string(),
            })
        }
        Err(e) => {
            // Failed — mark message as failed
            let result = {
                let mut state = STATE.lock().await;
                state.update_message(&pending_id, |msg| {
                    msg.set_failed(true);
                    msg.set_pending(false);
                })
            };

            if let Some((chat_id, failed_msg)) = result {
                emit_event("message_update", &serde_json::json!({
                    "old_id": &pending_id,
                    "message": &failed_msg,
                    "chat_id": &chat_id
                }));
            }

            Err(format!("Failed to send DM: {}", e))
        }
    }
}

/// Send a NIP-17 gift-wrapped file attachment DM.
///
/// Flow: read file → encrypt AES-256-GCM → upload to Blossom → build Kind 15 event
/// → gift-wrap → send to inbox relays → update state.
pub async fn send_file_dm(
    receiver_npub: &str,
    file_bytes: Arc<Vec<u8>>,
    filename: &str,
    extension: &str,
    content: Option<&str>,
) -> Result<SendResult, String> {
    let client = NOSTR_CLIENT.get().ok_or("Not logged in")?;
    let my_pk = *MY_PUBLIC_KEY.get().ok_or("Public key not set")?;
    let keys = MY_SECRET_KEY.to_keys().ok_or("Keys not available")?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH).unwrap();
    let pending_id = format!("pending-{}", now.as_nanos());

    let receiver = PublicKey::from_bech32(receiver_npub)
        .map_err(|e| format!("Invalid npub: {}", e))?;

    // Calculate file hash
    let file_hash = crypto::sha256_hex(&file_bytes);

    // Encrypt the file with AES-256-GCM
    let params = crypto::generate_encryption_params();
    let encrypted = crypto::encrypt_data(&file_bytes, &params)?;
    let encrypted_size = encrypted.len() as u64;

    // Build pending message with attachment
    let attachment = Attachment {
        id: file_hash.clone(),
        key: params.key.clone(),
        nonce: params.nonce.clone(),
        extension: extension.to_string(),
        name: filename.to_string(),
        url: String::new(), // Will be set after upload
        path: String::new(),
        size: encrypted_size,
        img_meta: None,
        downloading: false,
        downloaded: true,
        ..Default::default()
    };

    let msg = Message {
        id: pending_id.clone(),
        content: content.unwrap_or("").to_string(),
        at: now.as_millis() as u64,
        pending: true,
        mine: true,
        npub: my_pk.to_bech32().ok(),
        attachments: vec![attachment.clone()],
        ..Default::default()
    };

    {
        let mut state = STATE.lock().await;
        state.add_message_to_participant(receiver_npub, msg.clone());
    }

    emit_event("message_new", &serde_json::json!({
        "message": &msg,
        "chat_id": receiver_npub
    }));

    // Upload to Blossom
    let mime_type = crypto::mime_from_extension(extension);
    let servers = crate::state::get_blossom_servers();

    let no_op_progress: crate::blossom::ProgressCallback = Arc::new(|_, _| Ok(()));

    let upload_url = crate::blossom::upload_blob_with_progress_and_failover(
        keys.clone(),
        servers,
        Arc::new(encrypted),
        Some(mime_type),
        no_op_progress,
        Some(3),
        Some(std::time::Duration::from_secs(2)),
        None,
    ).await.map_err(|e| {
        // Mark as failed on upload error
        let pending_id = pending_id.clone();
        tokio::spawn(async move {
            let result = {
                let mut state = STATE.lock().await;
                state.update_message(&pending_id, |msg| {
                    msg.set_failed(true);
                    msg.set_pending(false);
                })
            };
            if let Some((chat_id, failed_msg)) = result {
                emit_event("message_update", &serde_json::json!({
                    "old_id": &pending_id,
                    "message": &failed_msg,
                    "chat_id": &chat_id
                }));
            }
        });
        format!("Upload failed: {}", e)
    })?;

    // Build Kind 15 file event
    let mut file_rumor = EventBuilder::new(Kind::from_u16(15), &upload_url)
        .tag(Tag::public_key(receiver))
        .tag(Tag::custom(TagKind::custom("file-type"), [mime_type]))
        .tag(Tag::custom(TagKind::custom("size"), [encrypted_size.to_string()]))
        .tag(Tag::custom(TagKind::custom("encryption-algorithm"), ["aes-gcm"]))
        .tag(Tag::custom(TagKind::custom("decryption-key"), [params.key.as_str()]))
        .tag(Tag::custom(TagKind::custom("decryption-nonce"), [params.nonce.as_str()]))
        .tag(Tag::custom(TagKind::custom("ox"), [file_hash.clone()]));

    if !filename.is_empty() {
        file_rumor = file_rumor.tag(Tag::custom(TagKind::custom("name"), [filename]));
    }

    let milliseconds = now.as_millis() % 1000;
    file_rumor = file_rumor.tag(Tag::custom(TagKind::custom("ms"), [milliseconds.to_string()]));

    let built_rumor = file_rumor.build(my_pk);
    let event_id = built_rumor.id.ok_or("Rumor has no id")?.to_hex();

    // Send via gift-wrap
    match crate::inbox_relays::send_gift_wrap(client, &receiver, built_rumor, []).await {
        Ok(_) => {
            // Update attachment URL in state
            {
                let mut state = STATE.lock().await;
                state.update_message(&pending_id, |msg| {
                    if let Some(att) = msg.attachments.last_mut() {
                        att.url = upload_url.clone().into_boxed_str();
                    }
                });
                state.finalize_pending_message(receiver_npub, &pending_id, &event_id);
            }

            emit_event("message_update", &serde_json::json!({
                "old_id": &pending_id,
                "message": &msg,
                "chat_id": receiver_npub
            }));

            Ok(SendResult {
                pending_id,
                event_id: Some(event_id),
                chat_id: receiver_npub.to_string(),
            })
        }
        Err(e) => {
            let result = {
                let mut state = STATE.lock().await;
                state.update_message(&pending_id, |msg| {
                    msg.set_failed(true);
                    msg.set_pending(false);
                })
            };
            if let Some((chat_id, failed_msg)) = result {
                emit_event("message_update", &serde_json::json!({
                    "old_id": &pending_id,
                    "message": &failed_msg,
                    "chat_id": &chat_id
                }));
            }
            Err(format!("Failed to send file DM: {}", e))
        }
    }
}
