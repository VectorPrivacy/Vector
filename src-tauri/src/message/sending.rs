//! Message sending functions.
//!
//! This module handles:
//! - Sending DM and MLS group messages
//! - Paste message from clipboard
//! - Voice message sending
//! - MLS media encryption and upload

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::collections::HashMap;
use std::sync::LazyLock;
use nostr_sdk::prelude::*;
use tauri::{AppHandle, Emitter, Manager, Runtime};

/// Cancel flags for in-progress uploads, keyed by pending message ID.
static UPLOAD_CANCEL_FLAGS: LazyLock<std::sync::Mutex<HashMap<String, Arc<AtomicBool>>>> =
    LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));

#[cfg(not(target_os = "android"))]
use ::image::{ImageBuffer, Rgba};
#[cfg(not(target_os = "android"))]
use tauri_plugin_clipboard_manager::ClipboardExt;

use crate::mls::MlsService;
use crate::util::{bytes_to_hex_string, hex_string_to_bytes};
use crate::util::calculate_file_hash;
use crate::STATE;
use crate::util;
use crate::TAURI_APP;
use crate::miniapps::realtime::{generate_topic_id, encode_topic_id};

use super::types::{AttachmentFile, ImageMetadata, Message, Attachment};

use vector_core::sending::{SendCallback, SendConfig};

/// Result of sending a message, returned to frontend for state update
#[derive(serde::Serialize)]
pub struct MessageSendResult {
    /// The pending ID that was used while sending
    pub pending_id: String,
    /// The real event ID after successful send (None if failed)
    pub event_id: Option<String>,
}

// ============================================================================
// TauriSendCallback — Bridges vector-core send events to Tauri frontend
// ============================================================================

pub struct TauriSendCallback;

impl SendCallback for TauriSendCallback {
    fn on_pending(&self, chat_id: &str, msg: &Message) {
        // Register cancel flag for file uploads (keyed by pending_id)
        if !msg.attachments.is_empty() {
            let mut flags = UPLOAD_CANCEL_FLAGS.lock().unwrap();
            flags.insert(msg.id.clone(), Arc::new(AtomicBool::new(false)));
        }

        if let Some(handle) = TAURI_APP.get() {
            handle.emit("message_new", serde_json::json!({
                "message": msg,
                "chat_id": chat_id
            })).ok();
        }
    }

    fn on_upload_progress(
        &self,
        pending_id: &str,
        percentage: u8,
        _bytes_sent: u64,
    ) -> Result<(), String> {
        // Check cancel flag
        {
            let flags = UPLOAD_CANCEL_FLAGS.lock().unwrap();
            if let Some(flag) = flags.get(pending_id) {
                if flag.load(Ordering::Relaxed) {
                    return Err("Upload cancelled".to_string());
                }
            }
        }

        if let Some(handle) = TAURI_APP.get() {
            handle.emit("attachment_upload_progress", serde_json::json!({
                "id": pending_id,
                "progress": percentage
            })).ok();
        }
        Ok(())
    }

    fn on_upload_complete(&self, chat_id: &str, pending_id: &str, attachment_id: &str, url: &str) {
        if let Some(handle) = TAURI_APP.get() {
            handle.emit("attachment_update", serde_json::json!({
                "chat_id": chat_id,
                "message_id": pending_id,
                "attachment_id": attachment_id,
                "url": url,
            })).ok();
        }
    }

    fn on_sent(&self, chat_id: &str, old_id: &str, msg: &Message) {
        UPLOAD_CANCEL_FLAGS.lock().unwrap().remove(old_id);
        if let Some(handle) = TAURI_APP.get() {
            handle.emit("message_update", serde_json::json!({
                "old_id": old_id,
                "message": msg,
                "chat_id": chat_id
            })).ok();
        }
    }

    fn on_failed(&self, chat_id: &str, old_id: &str, msg: &Message) {
        UPLOAD_CANCEL_FLAGS.lock().unwrap().remove(old_id);
        if let Some(handle) = TAURI_APP.get() {
            handle.emit("message_update", serde_json::json!({
                "old_id": old_id,
                "message": msg,
                "chat_id": chat_id
            })).ok();
        }
    }

    fn on_persist(&self, chat_id: &str, msg: &Message) {
        let chat_id = chat_id.to_string();
        let msg = msg.clone();
        tokio::spawn(async move {
            let _ = crate::db::save_message(&chat_id, &msg).await;
        });
    }
}

/// Helper function to mark message as failed and update frontend
async fn mark_message_failed(pending_id: Arc<String>, _receiver: &str) {
    let result = {
        let mut state = STATE.lock().await;
        state.update_message(&pending_id, |msg| {
            msg.set_failed(true);
            msg.set_pending(false);
        })
    };

    if let Some((chat_id, msg)) = result {
        let handle = TAURI_APP.get().unwrap();
        handle.emit("message_update", serde_json::json!({
            "old_id": pending_id.as_ref(),
            "message": &msg,
            "chat_id": &chat_id
        })).ok();
        let _ = crate::db::save_message(&chat_id, &msg).await;
    }
}

/// Delete a failed message from state and database.
/// Only allows deletion of messages with `failed == true` (security guard).
#[tauri::command]
pub async fn delete_failed_message(message_id: String) -> Result<(), String> {
    // Verify failed flag and remove in a single lock to prevent TOCTOU races
    let removed = {
        let mut state = STATE.lock().await;
        let is_failed = state.find_message(&message_id)
            .map(|(_, msg)| msg.failed)
            .unwrap_or(false);
        if !is_failed {
            None
        } else {
            state.remove_message(&message_id)
        }
    };

    if let Some((chat_id, _)) = removed {
        // Delete from database
        if let Err(e) = crate::db::delete_event(&message_id).await {
            eprintln!("[delete_failed_message] DB delete failed: {}", e);
        }

        // Emit message_removed event so frontend removes the DOM element
        if let Some(handle) = TAURI_APP.get() {
            handle.emit("message_removed", serde_json::json!({
                "id": &message_id,
                "chat_id": &chat_id,
                "reason": "deleted"
            })).ok();
        }
    } else {
        return Err("Message is not failed or does not exist".to_string());
    }

    Ok(())
}

/// Cancel an in-progress file upload by setting its cancel flag.
/// Removes the pending message from state and emits `message_removed`.
#[tauri::command]
pub async fn cancel_upload(pending_id: String) -> Result<(), String> {
    // Set the cancel flag if upload is still in progress
    let was_in_progress = {
        let flags = UPLOAD_CANCEL_FLAGS.lock().unwrap();
        if let Some(flag) = flags.get(&pending_id) {
            flag.store(true, Ordering::Relaxed);
            true
        } else {
            false
        }
    };

    // Only remove the pending message if the upload was actually in progress
    // (avoids removing a successfully-uploaded message during the relay-send window)
    if !was_in_progress {
        return Ok(());
    }

    let removed = {
        let mut state = STATE.lock().await;
        state.remove_message(&pending_id)
    };

    // Emit message_removed event so frontend removes the DOM element
    if let Some((chat_id, _msg)) = removed {
        if let Some(handle) = TAURI_APP.get() {
            handle.emit("message_removed", serde_json::json!({
                "id": &pending_id,
                "chat_id": &chat_id,
                "reason": "cancelled"
            })).ok();
        }
    }

    Ok(())
}

/// Result of MLS media encryption using MIP-04
#[derive(Debug)]
pub struct MlsMediaUploadResult {
    /// The imeta tag to include in the Kind 9 event
    pub imeta_tag: Tag,
    /// The uploaded URL for storing in attachment
    pub url: String,
    /// Encrypted file size
    pub encrypted_size: u64,
    /// Original file hash (hex)
    pub original_hash: String,
    /// Encryption nonce (hex) - required for MLS decryption
    pub nonce: String,
    /// MIP-04 scheme version (e.g., "mip04-v2")
    pub scheme_version: String,
}

/// Encrypt and upload a file using MIP-04 for MLS groups
///
/// This uses the MDK's EncryptedMediaManager to encrypt files with keys derived
/// from the MLS group secret, creating a White Noise-compatible imeta tag.
async fn encrypt_and_upload_mls_media(
    group_id: &str,
    file: &AttachmentFile,
    filename: &str,
    progress_callback: crate::blossom::ProgressCallback,
    cancel_flag: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
) -> Result<MlsMediaUploadResult, String> {
    use mdk_core::encrypted_media::MediaProcessingOptions;

    // Get the MDK engine and create media manager for this group
    let mls_service = MlsService::new_persistent_static()
        .map_err(|e| format!("Failed to create MLS service: {}", e))?;

    // Look up the group metadata to get the engine_group_id
    let groups = mls_service.read_groups().await
        .map_err(|e| format!("Failed to read groups: {}", e))?;
    let group_meta = groups.iter()
        .find(|g| g.group_id == group_id)
        .ok_or_else(|| format!("Group not found: {}", group_id))?;

    if group_meta.engine_group_id.is_empty() {
        return Err("Group has no engine_group_id".to_string());
    }

    // Parse the engine group ID (this is what MDK uses internally)
    let engine_gid_bytes = hex_string_to_bytes(&group_meta.engine_group_id);
    let gid = mdk_core::GroupId::from_slice(&engine_gid_bytes);

    // Get MDK engine and media manager
    let mdk = mls_service.engine()
        .map_err(|e| format!("Failed to get MDK engine: {}", e))?;
    let media_manager = mdk.media_manager(gid);

    // Determine MIME type from file extension
    let mime_type = util::mime_from_extension(&file.extension);

    // Normalize MIME type for MDK - it has strict validation and rejects many types
    // Use the original for images (which MDK handles well), otherwise use octet-stream
    let mdk_mime_type = if mime_type.starts_with("image/") {
        mime_type.clone()
    } else {
        "application/octet-stream".to_string()
    };

    // Configure media processing options
    // Disable blurhash generation — we generate thumbhash ourselves
    let options = MediaProcessingOptions {
        sanitize_exif: true,
        generate_blurhash: false,
        max_dimension: None,
        max_file_size: None,
        max_filename_length: None,
    };

    // Encrypt the file using MDK's media_manager
    let mut upload = media_manager
        .encrypt_for_upload_with_options(&file.bytes, &mdk_mime_type, filename, &options)
        .map_err(|e| format!("MIP-04 encryption failed: {}", e))?;

    // Upload the encrypted data to Blossom
    let signer = crate::MY_SECRET_KEY.to_keys().expect("Keys not initialized");
    let servers = crate::get_blossom_servers();

    let url = crate::blossom::upload_blob_with_progress_and_failover(
        signer,
        servers,
        Arc::new(std::mem::take(&mut upload.encrypted_data)),
        Some(&mime_type),
        progress_callback,
        Some(3),
        Some(std::time::Duration::from_secs(2)),
        cancel_flag,
    ).await.map_err(|e| format!("Blossom upload failed: {}", e))?;

    // Create the imeta tag using MDK
    let imeta_tag = media_manager.create_imeta_tag(&upload, &url);

    // Convert nostr::Tag to nostr_sdk Tag
    // Both use the same underlying type, but we need to ensure compatibility
    let mut tag_values: Vec<String> = imeta_tag.to_vec();

    // Note: We keep the normalized MIME type (e.g., application/octet-stream) in the imeta tag
    // because MDK also validates MIME types when parsing on the receive side.
    // The receiver can identify file type from the extension in the filename.

    // Append our pre-generated thumbhash and dimensions if available
    if let Some(ref img_meta) = file.img_meta {
        // Add thumbhash if not already present
        if !tag_values.iter().any(|s| s.starts_with("thumbhash ")) && !img_meta.thumbhash.is_empty() {
            tag_values.push(format!("thumbhash {}", img_meta.thumbhash));
        }
        // Add dimensions if not already present
        if !tag_values.iter().any(|s| s.starts_with("dim ")) {
            tag_values.push(format!("dim {}x{}", img_meta.width, img_meta.height));
        }
    }

    // Ensure size is included in the imeta tag (required for auto-download on receive side)
    if !tag_values.iter().any(|s| s.starts_with("size ")) && upload.encrypted_size > 0 {
        tag_values.push(format!("size {}", upload.encrypted_size));
    }

    let sdk_imeta_tag = Tag::parse(&tag_values)
        .map_err(|e| format!("Failed to parse imeta tag: {}", e))?;

    // Extract scheme version from the imeta tag (MDK uses DEFAULT_SCHEME_VERSION)
    // The tag format includes "v mip04-v2" or similar
    let scheme_version = tag_values.iter()
        .find(|s| s.starts_with("v "))
        .map(|s| s.strip_prefix("v ").unwrap_or("mip04-v2").to_string())
        .unwrap_or_else(|| "mip04-v2".to_string());

    Ok(MlsMediaUploadResult {
        imeta_tag: sdk_imeta_tag,
        url,
        encrypted_size: upload.encrypted_size,
        original_hash: bytes_to_hex_string(&upload.original_hash),
        nonce: bytes_to_hex_string(&upload.nonce),
        scheme_version,
    })
}

/// Headless text-only reply: sends a DM or MLS message.
/// Used by Android notification inline-reply (JNI).
#[allow(dead_code)]
pub async fn send_text_reply_headless(chat_id: &str, content: &str) -> Result<String, String> {
    let is_group = {
        let state = STATE.lock().await;
        state.get_chat(chat_id).map_or(!chat_id.starts_with("npub1"), |c| c.is_mls_group())
    };

    let event_id = if is_group {
        // MLS path stays local
        let my_public_key = *crate::MY_PUBLIC_KEY.get().ok_or("Public key not initialized")?;
        let milliseconds = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap()
            .as_millis() % 1000;

        let rumor = EventBuilder::new(
            Kind::from_u16(crate::stored_event::event_kind::MLS_CHAT_MESSAGE),
            content,
        )
        .tag(Tag::custom(TagKind::custom("ms"), [milliseconds.to_string()]))
        .build(my_public_key);

        let rumor_id = rumor.id.ok_or("Rumor has no id")?.to_hex();
        crate::mls::send_mls_message(chat_id, rumor, None).await?;

        // Add to STATE after successful MLS send
        let msg = Message {
            id: rumor_id.clone(),
            content: content.to_string(),
            at: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64,
            mine: true,
            npub: my_public_key.to_bech32().ok(),
            ..Default::default()
        };
        {
            let mut state = STATE.lock().await;
            state.create_or_get_mls_group_chat(chat_id, vec![]);
            state.add_message_to_chat(chat_id, msg.clone());
        }
        if let Some(handle) = TAURI_APP.get() {
            handle.emit("mls_message_new", serde_json::json!({
                "group_id": chat_id,
                "message": &msg
            })).ok();
        }
        let _ = crate::db::save_message(chat_id, &msg).await;

        rumor_id
    } else {
        // DM path: delegate to vector-core
        let config = SendConfig::headless();
        let callback: Arc<dyn SendCallback> = Arc::new(TauriSendCallback);
        let result = vector_core::sending::send_dm(
            chat_id, content, None, &config, callback,
        ).await?;
        result.event_id.unwrap_or(result.pending_id)
    };

    crate::chat::mark_as_read_headless(chat_id).await;
    Ok(event_id)
}

#[tauri::command]
pub async fn message(receiver: String, content: String, replied_to: String, file: Option<AttachmentFile>) -> Result<MessageSendResult, String> {
    // Detect chat type early (needed for short-circuit)
    let is_group_chat = {
        let state = STATE.lock().await;
        if let Some(chat) = state.get_chat(&receiver) {
            chat.is_mls_group()
        } else {
            !receiver.starts_with("npub1")
        }
    };

    // DM: delegate entirely to vector-core
    if !is_group_chat {
        let config = SendConfig::gui();
        let callback: Arc<dyn SendCallback> = Arc::new(TauriSendCallback);

        return if let Some(ref attached_file) = file {
            // File DM: vector-core handles encrypt + upload + send
            let result = vector_core::sending::send_file_dm(
                &receiver, Arc::clone(&attached_file.bytes),
                &attached_file.name, &attached_file.extension,
                if content.is_empty() { None } else { Some(&content) },
                &config, callback.clone(),
            ).await?;
            Ok(MessageSendResult { pending_id: result.pending_id, event_id: result.event_id })
        } else {
            // Text DM
            let reply: Option<&str> = if replied_to.is_empty() { None } else { Some(&replied_to) };
            let result = vector_core::sending::send_dm(
                &receiver, &content, reply, &config, callback,
            ).await?;
            Ok(MessageSendResult { pending_id: result.pending_id, event_id: result.event_id })
        };
    }

    // === MLS groups only below this point ===

    let current_time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap();
    let pending_id = Arc::new(String::from("pending-") + &current_time.as_nanos().to_string());
    let my_public_key = *crate::MY_PUBLIC_KEY.get().expect("Public key not initialized");

    let msg = Message {
        id: pending_id.as_ref().clone(),
        content,
        replied_to,
        replied_to_content: None,
        replied_to_npub: None,
        replied_to_has_attachment: None,
        preview_metadata: None,
        at: current_time.as_millis() as u64,
        attachments: Vec::new(),
        reactions: Vec::new(),
        pending: true,
        failed: false,
        mine: true,
        npub: my_public_key.to_bech32().ok(),
        wrapper_event_id: None,
        edited: false,
        edit_history: None,
    };

    // Add message to appropriate chat type
    {
        let mut state = STATE.lock().await;
        if is_group_chat {
            // For groups, create or get the MLS group chat
            state.create_or_get_mls_group_chat(&receiver, vec![]);
            state.add_message_to_chat(&receiver, msg.clone());
        } else {
            // For DMs, use the existing participant-based method
            state.add_message_to_participant(&receiver, msg.clone());
        }
    }

    // For DMs, convert the Bech32 String to a PublicKey
    // For groups, we'll handle it differently below
    let _receiver_pubkey = if !is_group_chat {
        PublicKey::from_bech32(receiver.clone().as_str())
            .map_err(|e| format!("Invalid npub: {}", e))?
    } else {
        // For groups, we don't need a receiver_pubkey for the rumor
        // We'll use a placeholder that won't be used
        my_public_key
    };

    // Prepare the rumor
    let handle = TAURI_APP.get().unwrap();
    let mut rumor = if let Some(attached_file) = file {

        // Calculate the file hash first (before encryption)
        let file_hash = calculate_file_hash(&*attached_file.bytes);

        // ============================================================
        // MLS GROUP ATTACHMENTS: Use MIP-04 encryption with imeta tags
        // ============================================================
        // NOTE: Unlike DMs, MLS attachments are NOT deduplicated across sends.
        // MLS uses forward secrecy - when members join/leave, the group secret
        // changes (new epoch). Files encrypted with old keys can't be decrypted
        // by new members. Each send must re-encrypt with the current group key.
        // ============================================================
        if is_group_chat {
            // For WebXDC (.xdc) files, generate topic ID upfront
            let webxdc_topic = if attached_file.extension.to_lowercase() == "xdc" {
                let topic_id = generate_topic_id();
                Some(encode_topic_id(&topic_id))
            } else {
                None
            };

            // Prepare filename for the upload
            let filename = format!("{}.{}", &file_hash, &attached_file.extension);

            // Store the file locally first
            let base_directory = if cfg!(target_os = "ios") {
                tauri::path::BaseDirectory::Document
            } else {
                tauri::path::BaseDirectory::Download
            };
            let dir = handle.path().resolve("vector", base_directory).unwrap();
            std::fs::create_dir_all(&dir).unwrap();
            // Use human-readable name on disk if available, otherwise hash-based
            let on_disk_name = if attached_file.name.is_empty() {
                filename.clone()
            } else {
                attached_file.name.clone()
            };
            let candidate = dir.join(&on_disk_name);
            let already_exists = candidate.exists()
                && std::fs::metadata(&candidate).map(|m| m.len() == attached_file.bytes.len() as u64).unwrap_or(false)
                && std::fs::read(&candidate).map(|b| util::calculate_file_hash(&b) == file_hash).unwrap_or(false);
            let hash_file_path = if already_exists {
                candidate
            } else {
                let path = crate::commands::attachments::resolve_unique_filename(&dir, &on_disk_name);
                // Atomic write: write to temp file then rename
                let tmp_path = dir.join(format!(".{}.tmp", &filename));
                std::fs::write(&tmp_path, &*attached_file.bytes).map_err(|e| {
                    let _ = std::fs::remove_file(&tmp_path);
                    format!("Failed to write temp file: {}", e)
                })?;
                std::fs::rename(&tmp_path, &path).map_err(|e| {
                    let _ = std::fs::remove_file(&tmp_path);
                    format!("Failed to rename temp file: {}", e)
                })?;
                path
            };

            // Add the attachment to the pending message with the local path immediately,
            // so the frontend can show a preview (with lowered opacity + progress bar)
            // while the upload is in progress — matching the DM behavior.
            {
                let preview_attachment = Attachment {
                    id: file_hash.clone(),
                    key: String::new(),
                    nonce: String::new(),
                    extension: attached_file.extension.clone(),
                    name: attached_file.name.clone(),
                    url: String::new(), // No URL yet — upload hasn't started
                    path: hash_file_path.to_string_lossy().to_string(),
                    size: attached_file.bytes.len() as u64,
                    img_meta: attached_file.img_meta.clone(),
                    downloading: false,
                    downloaded: true, // Local file exists, so frontend can preview it
                    webxdc_topic: webxdc_topic.clone(),
                    group_id: Some(receiver.clone()),
                    original_hash: Some(file_hash.clone()),
                    scheme_version: None,
                    mls_filename: Some(filename.clone()),
                };
                let compact_att = crate::message::CompactAttachment::from_attachment_owned(preview_attachment);

                let mut state = STATE.lock().await;
                state.add_attachment_to_message(&receiver, &pending_id, compact_att);

                // Emit to frontend so the upload preview is visible immediately
                if let Some(msg) = state.update_message_in_chat(&receiver, &pending_id, |_| {}) {
                    handle.emit("mls_message_new", serde_json::json!({
                        "group_id": &receiver,
                        "message": msg
                    })).ok();
                }
            }

            // Create cancel flag for this upload
            let cancel_flag = Arc::new(AtomicBool::new(false));
            {
                let mut flags = UPLOAD_CANCEL_FLAGS.lock().unwrap();
                flags.insert(pending_id.to_string(), Arc::clone(&cancel_flag));
            }

            // Create progress callback for MLS upload
            let pending_id_for_callback = Arc::clone(&pending_id);
            let handle_for_callback = handle.clone();
            let progress_callback: crate::blossom::ProgressCallback = std::sync::Arc::new(move |percentage, _bytes| {
                if let Some(pct) = percentage {
                    handle_for_callback.emit("attachment_upload_progress", serde_json::json!({
                        "id": pending_id_for_callback.as_ref(),
                        "progress": pct
                    })).ok();
                }
                Ok(())
            });

            // Encrypt and upload using MIP-04 (always fresh - no deduplication for MLS)
            let mls_upload_result = match encrypt_and_upload_mls_media(
                &receiver,
                &attached_file,
                &filename,
                progress_callback,
                Some(Arc::clone(&cancel_flag)),
            ).await {
                Ok(result) => {
                    // Remove cancel flag on success
                    UPLOAD_CANCEL_FLAGS.lock().unwrap().remove(pending_id.as_ref());
                    result
                }
                Err(e) => {
                    // Remove cancel flag on error
                    UPLOAD_CANCEL_FLAGS.lock().unwrap().remove(pending_id.as_ref());

                    // If cancelled, the cancel_upload command already cleaned up state
                    if e.contains("Upload cancelled") {
                        return Err(e);
                    }

                    eprintln!("[MIP-04 Error] MLS media upload failed: {}", e);
                    mark_message_failed(Arc::clone(&pending_id), &receiver).await;
                    return Err(format!("Failed to upload MLS media: {}", e));
                }
            };

            // Replace the preview attachment with the final one (adds URL, nonce, etc.)
            {
                let final_attachment = Attachment {
                    id: file_hash.clone(),
                    key: String::new(),
                    nonce: mls_upload_result.nonce.clone(),
                    extension: attached_file.extension.clone(),
                    name: attached_file.name.clone(),
                    url: mls_upload_result.url.clone(),
                    path: hash_file_path.to_string_lossy().to_string(),
                    size: mls_upload_result.encrypted_size,
                    img_meta: attached_file.img_meta.clone(),
                    downloading: false,
                    downloaded: true,
                    webxdc_topic: webxdc_topic.clone(),
                    group_id: Some(receiver.clone()),
                    original_hash: Some(mls_upload_result.original_hash.clone()),
                    scheme_version: Some(mls_upload_result.scheme_version.clone()),
                    mls_filename: Some(filename.clone()),
                };
                let compact_att = crate::message::CompactAttachment::from_attachment_owned(final_attachment);

                let mut state = STATE.lock().await;
                // Replace the preview attachment (not push a second one)
                if let Some(msg) = state.update_message_in_chat(&receiver, &pending_id, |m| {
                    if let Some(att) = m.attachments.last_mut() {
                        *att = compact_att;
                    }
                }) {
                    handle.emit("mls_message_new", serde_json::json!({
                        "group_id": &receiver,
                        "message": msg
                    })).ok();
                }
            }

            // Build Kind 9 event with text content and imeta tag
            let mut mls_rumor = EventBuilder::new(
                Kind::from_u16(crate::stored_event::event_kind::MLS_CHAT_MESSAGE),
                msg.content.clone()
            ).tag(mls_upload_result.imeta_tag);

            // Add filename tag if available
            if !attached_file.name.is_empty() {
                mls_rumor = mls_rumor.tag(Tag::custom(
                    TagKind::custom("name"),
                    [attached_file.name.as_str()]
                ));
            }

            // Add webxdc-topic if this is an XDC file
            if let Some(ref topic_encoded) = webxdc_topic {
                mls_rumor = mls_rumor.tag(Tag::custom(
                    TagKind::custom("webxdc-topic"),
                    [topic_encoded.clone()]
                ));
            }

            // Add reply reference if present
            if !msg.replied_to.is_empty() {
                mls_rumor = mls_rumor.tag(Tag::custom(
                    TagKind::e(),
                    [msg.replied_to.clone(), String::from(""), String::from("reply")],
                ));
            }

            // Add millisecond precision tag
            let final_time = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap();
            let milliseconds = final_time.as_millis() % 1000;
            mls_rumor = mls_rumor.tag(Tag::custom(
                TagKind::custom("ms"),
                [milliseconds.to_string()],
            ));

            // Build the rumor
            let built_rumor = mls_rumor.build(my_public_key);
            let event_id = built_rumor.id.expect("UnsignedEvent should have id after build").to_hex();

            // Send via MLS using the existing send_mls_message function
            crate::mls::send_mls_message(&receiver, built_rumor, Some(pending_id.to_string())).await
                .map_err(|e| format!("Failed to send MLS message: {}", e))?;

            // Update message state to non-pending (send_mls_message handles failures)
            let msg_for_save = {
                let mut state = STATE.lock().await;
                state.finalize_pending_message(&receiver, &pending_id, &event_id)
            };

            if let Some((old_id, msg)) = msg_for_save {
                handle.emit("message_update", serde_json::json!({
                    "old_id": old_id,
                    "message": &msg,
                    "chat_id": &receiver
                })).ok();
                let _ = crate::db::save_message(&receiver, &msg).await;
            }

            return Ok(MessageSendResult {
                pending_id: pending_id.to_string(),
                event_id: Some(event_id),
            });
        }

        // DM file attachments are now handled by vector-core (short-circuited above).
        // This code is only reachable for MLS group file attachments.
        unreachable!("DM file attachments should be handled by vector-core short-circuit")
    } else {
        // MLS text message (DM text is short-circuited above)
        handle.emit("mls_message_new", serde_json::json!({
            "group_id": &receiver,
            "message": &msg
        })).ok();

        EventBuilder::new(Kind::from_u16(crate::stored_event::event_kind::MLS_CHAT_MESSAGE), msg.content)
    };

    // If a reply reference is included, add the tag
    if !msg.replied_to.is_empty() {
        rumor = rumor.tag(Tag::custom(
            TagKind::e(),
            [msg.replied_to, String::from(""), String::from("reply")],
        ));
    }

    // Get fresh timestamp with milliseconds right before giftwrapping
    let final_time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap();
    let milliseconds = final_time.as_millis() % 1000;

    // Add millisecond precision tag for accurate message ordering
    rumor = rumor.tag(Tag::custom(
        TagKind::custom("ms"),
        [milliseconds.to_string()],
    ));

    // Build the rumor with our key (unsigned)
    let built_rumor = rumor.build(my_public_key);
    let rumor_id = built_rumor.id.unwrap();

    // Route to appropriate protocol handler
    if is_group_chat {
        // MLS Group Chat - send through MLS engine
        // Note: send_mls_message handles all state management internally:
        // - Uses the pending message we created above (via pending_id)
        // - Updates message ID when processed
        // - Marks as success/failure after network confirmation
        // - Saves to database
        return match crate::mls::send_mls_message(&receiver, built_rumor.clone(), Some(pending_id.to_string())).await {
            Ok(_) => Ok(MessageSendResult {
                pending_id: pending_id.to_string(),
                event_id: Some(rumor_id.to_hex()),
            }),
            Err(e) => {
                eprintln!("Failed to send MLS message: {:?}", e);
                Ok(MessageSendResult {
                    pending_id: pending_id.to_string(),
                    event_id: None,
                })
            }
        };
    } else {
        // DM
        let config = SendConfig::gui();
        let callback: Arc<dyn SendCallback> = Arc::new(TauriSendCallback);
        let result = vector_core::sending::send_rumor_dm(
            &receiver, &pending_id, built_rumor, &config, callback,
        ).await;

        match result {
            Ok(r) => Ok(MessageSendResult {
                pending_id: r.pending_id,
                event_id: r.event_id,
            }),
            Err(_) => Ok(MessageSendResult {
                pending_id: pending_id.to_string(),
                event_id: None,
            }),
        }
    }
}

#[tauri::command]
pub async fn paste_message<R: Runtime>(handle: AppHandle<R>, receiver: String, replied_to: String, transparent: bool) -> Result<MessageSendResult, String> {
    // Platform-specific clipboard reading
    #[cfg(target_os = "android")]
    let img = {
        let _ = &handle; // Unused on Android
        use crate::android::clipboard::read_image_from_clipboard;
        read_image_from_clipboard()?
    };

    #[cfg(not(target_os = "android"))]
    let img = {
        let tauri_img = handle.clipboard().read_image()
            .map_err(|e| format!("Failed to read clipboard: {:?}", e))?;

        // Get RGBA data - this returns &[u8], not a Result
        let rgba_data = tauri_img.rgba();

        // Convert to ImageBuffer
        ImageBuffer::<Rgba<u8>, Vec<u8>>::from_raw(
            tauri_img.width(),
            tauri_img.height(),
            rgba_data.to_vec()
        ).ok_or_else(|| "Failed to create image buffer".to_string())?
    };

    // Get original pixels
    let original_pixels = img.as_raw();

    // Windows: check if clipboard corrupted alpha channel (all values near zero)
    let mut _transparency_bug_search = false;
    #[cfg(target_os = "windows")]
    {
        _transparency_bug_search = util::has_all_alpha_near_zero(original_pixels);
    }

    // For non-transparent images: set alpha to opaque
    let pixels = if !transparent || _transparency_bug_search {
        let mut modified = original_pixels.to_vec();
        util::set_all_alpha_opaque(&mut modified);
        std::borrow::Cow::Owned(modified)
    } else {
        std::borrow::Cow::Borrowed(original_pixels)
    };

    // Encode image, choosing PNG (with alpha) or JPEG (without)
    let encoded = crate::shared::image::encode_rgba_auto(&pixels, img.width(), img.height(), 85)?;
    let (encoded_bytes, extension) = (encoded.bytes, encoded.extension);

    // Generate image metadata with ThumbHash and dimensions
    let img_meta: Option<ImageMetadata> = util::generate_thumbhash_from_rgba(
        img.as_raw(),
        img.width(),
        img.height()
    ).map(|thumbhash| ImageMetadata {
        thumbhash,
        width: img.width(),
        height: img.height(),
    });

    // Generate an Attachment File
    let attachment_file = AttachmentFile {
        bytes: Arc::new(encoded_bytes),
        img_meta,
        extension: extension.to_string(),
        name: String::new(),
    };

    // Message the file to the intended user
    message(receiver, String::new(), replied_to, Some(attachment_file)).await
}

pub async fn voice_message(receiver: String, replied_to: String, bytes: Vec<u8>) -> Result<MessageSendResult, String> {
    // Generate an Attachment File
    let attachment_file = AttachmentFile {
        bytes: Arc::new(bytes),
        img_meta: None,
        extension: String::from("wav"),
        name: String::new(),
    };

    // Message the file to the intended user
    message(receiver, String::new(), replied_to, Some(attachment_file)).await
}
