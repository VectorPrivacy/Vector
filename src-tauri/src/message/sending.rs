//! Message sending functions.
//!
//! This module handles:
//! - Sending DM and MLS group messages
//! - Paste message from clipboard
//! - Voice message sending
//! - MLS media encryption and upload

use std::sync::Arc;
use nostr_sdk::prelude::*;
use tauri::{AppHandle, Emitter, Manager, Runtime};

#[cfg(not(target_os = "android"))]
use ::image::{ImageBuffer, Rgba};
#[cfg(not(target_os = "android"))]
use tauri_plugin_clipboard_manager::ClipboardExt;

use crate::crypto;
use crate::db::{self, save_chat};
use crate::mls::MlsService;
use crate::util::calculate_file_hash;
use crate::net;
use crate::STATE;
use crate::util;
use crate::TAURI_APP;
use crate::NOSTR_CLIENT;
use crate::miniapps::realtime::{generate_topic_id, encode_topic_id};

use super::types::{AttachmentFile, ImageMetadata, Message, Attachment};

/// Helper function to mark message as failed and update frontend
async fn mark_message_failed(pending_id: Arc<String>, receiver: &str) {
    // Find the message in chats and mark it as failed
    let mut state = STATE.lock().await;

    // Search through all chats to find the message with this pending ID
    for chat in &mut state.chats {
        if chat.has_participant(receiver) {
            if let Some(message) = chat.messages.iter_mut().find(|m| m.id == *pending_id) {
                // Mark the message as failed
                message.failed = true;
                message.pending = false;

                // Update the frontend
                let handle = TAURI_APP.get().unwrap();
                handle.emit("message_update", serde_json::json!({
                    "old_id": pending_id.as_ref(),
                    "message": message,
                    "chat_id": receiver
                })).unwrap();

                // Save the failed message to our DB
                let message_to_save = message.clone();
                drop(state); // Release lock before async DB operation
                let _ = crate::db::save_message(handle.clone(), receiver, &message_to_save).await;
                break;
            }
        }
    }
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
async fn encrypt_and_upload_mls_media<R: Runtime>(
    handle: &AppHandle<R>,
    group_id: &str,
    file: &AttachmentFile,
    filename: &str,
    progress_callback: crate::blossom::ProgressCallback,
) -> Result<MlsMediaUploadResult, String> {
    use mdk_core::encrypted_media::MediaProcessingOptions;

    // Get the MDK engine and create media manager for this group
    let mls_service = MlsService::new_persistent(handle)
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
    let engine_gid_bytes = hex::decode(&group_meta.engine_group_id)
        .map_err(|e| format!("Invalid engine_group_id hex: {}", e))?;
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
    // Disable blurhash generation since we already have it in img_meta
    let options = MediaProcessingOptions {
        sanitize_exif: true,
        generate_blurhash: file.img_meta.is_none(), // Only generate if we don't have it
        max_dimension: None,
        max_file_size: None,
        max_filename_length: None,
    };

    // Encrypt the file using MDK's media_manager
    let mut upload = media_manager
        .encrypt_for_upload_with_options(&file.bytes, &mdk_mime_type, filename, &options)
        .map_err(|e| format!("MIP-04 encryption failed: {}", e))?;

    // Upload the encrypted data to Blossom
    let client = NOSTR_CLIENT.get().expect("Nostr client not initialized");
    let signer = client.signer().await
        .map_err(|e| format!("Failed to get signer: {}", e))?;
    let servers = crate::get_blossom_servers();

    let url = crate::blossom::upload_blob_with_progress_and_failover(
        signer,
        servers,
        Arc::new(std::mem::take(&mut upload.encrypted_data)),
        Some(&mime_type),
        progress_callback,
        Some(3),
        Some(std::time::Duration::from_secs(2)),
    ).await.map_err(|e| format!("Blossom upload failed: {}", e))?;

    // Create the imeta tag using MDK
    let imeta_tag = media_manager.create_imeta_tag(&upload, &url);

    // Convert nostr::Tag to nostr_sdk Tag
    // Both use the same underlying type, but we need to ensure compatibility
    let mut tag_values: Vec<String> = imeta_tag.to_vec();

    // Note: We keep the normalized MIME type (e.g., application/octet-stream) in the imeta tag
    // because MDK also validates MIME types when parsing on the receive side.
    // The receiver can identify file type from the extension in the filename.

    // Append our pre-generated blurhash and dimensions if available
    // MDK doesn't include these when generate_blurhash is false
    if let Some(ref img_meta) = file.img_meta {
        // Add blurhash if not already present
        if !tag_values.iter().any(|s| s.starts_with("blurhash ")) && !img_meta.blurhash.is_empty() {
            tag_values.push(format!("blurhash {}", img_meta.blurhash));
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
        original_hash: hex::encode(upload.original_hash),
        nonce: hex::encode(upload.nonce),
        scheme_version,
    })
}

#[tauri::command]
pub async fn message(receiver: String, content: String, replied_to: String, file: Option<AttachmentFile>) -> Result<bool, String> {
    // Immediately add the message to our state as "Pending" with an ID derived from the current nanosecond, we'll update it as either Sent (non-pending) or Failed in the future
    let current_time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap();
    // Create persistent pending_id that will live for the entire function
    let pending_id = Arc::new(String::from("pending-") + &current_time.as_nanos().to_string());
    // Grab our pubkey first (needed for npub in group chats)
    let client = NOSTR_CLIENT.get().expect("Nostr client not initialized");
    let signer = client.signer().await.unwrap();
    let my_public_key = signer.get_public_key().await.unwrap();

    let msg = Message {
        id: pending_id.as_ref().clone(),
        content,
        replied_to,
        replied_to_content: None, // Will be populated when loaded from DB
        replied_to_npub: None,
        replied_to_has_attachment: None,
        preview_metadata: None,
        at: current_time.as_millis() as u64,
        attachments: Vec::new(),
        reactions: Vec::new(),
        pending: true,
        failed: false,
        mine: true,
        npub: my_public_key.to_bech32().ok(), // Needed for group chats so replies show correct author
        wrapper_event_id: None, // Will be set when message is sent
        edited: false,
        edit_history: None,
    };

    // Detect if this is a group chat or DM
    // First check if a chat already exists and use its type
    // Otherwise, check if receiver is a valid bech32 npub (DM) or not (group)
    let is_group_chat = {
        let state = STATE.lock().await;
        if let Some(chat) = state.get_chat(&receiver) {
            // Chat exists, use its type
            chat.is_mls_group()
        } else {
            // Chat doesn't exist, detect by receiver format
            // If it's a valid npub (starts with "npub1"), it's a DM
            // Otherwise it's a group_id
            !receiver.starts_with("npub1")
        }
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
    let receiver_pubkey = if !is_group_chat {
        PublicKey::from_bech32(receiver.clone().as_str())
            .map_err(|e| format!("Invalid npub: {}", e))?
    } else {
        // For groups, we don't need a receiver_pubkey for the rumor
        // We'll use a placeholder that won't be used
        my_public_key
    };

    // Prepare the rumor
    let handle = TAURI_APP.get().unwrap();
    let mut rumor = if file.is_none() {
        // Send the text message to our frontend with appropriate event
        if is_group_chat {
            handle.emit("mls_message_new", serde_json::json!({
                "group_id": &receiver,
                "message": &msg
            })).unwrap();
        } else {
            handle.emit("message_new", serde_json::json!({
                "message": &msg,
                "chat_id": &receiver
            })).unwrap();
        }

        // Text Message
        if !is_group_chat {
            EventBuilder::private_msg_rumor(receiver_pubkey, msg.content)
        } else {
            // For MLS groups, use Kind 9 (White Noise compatible)
            EventBuilder::new(Kind::from_u16(crate::stored_event::event_kind::MLS_CHAT_MESSAGE), msg.content)
        }
    } else {
        let attached_file = file.unwrap();

        // Calculate the file hash first (before encryption)
        let file_hash = calculate_file_hash(&*attached_file.bytes);

        // The SHA-256 hash of an empty file - we should never reuse this
        const EMPTY_FILE_HASH: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

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
            let hash_file_path = dir.join(&filename);
            std::fs::write(&hash_file_path, &*attached_file.bytes).unwrap();

            // Create progress callback for MLS upload
            let pending_id_for_callback = Arc::clone(&pending_id);
            let handle_for_callback = handle.clone();
            let progress_callback: crate::blossom::ProgressCallback = std::sync::Arc::new(move |percentage, _bytes| {
                if let Some(pct) = percentage {
                    handle_for_callback.emit("attachment_upload_progress", serde_json::json!({
                        "id": pending_id_for_callback.as_ref(),
                        "progress": pct
                    })).unwrap();
                }
                Ok(())
            });

            // Encrypt and upload using MIP-04 (always fresh - no deduplication for MLS)
            let mls_upload_result = match encrypt_and_upload_mls_media(
                handle,
                &receiver,
                &attached_file,
                &filename,
                progress_callback,
            ).await {
                Ok(result) => result,
                Err(e) => {
                    eprintln!("[MIP-04 Error] MLS media upload failed: {}", e);
                    mark_message_failed(Arc::clone(&pending_id), &receiver).await;
                    return Err(format!("Failed to upload MLS media: {}", e));
                }
            };

            // Update the pending message with the uploaded attachment
            {
                let pending_id_for_update = Arc::clone(&pending_id);
                let mut state = STATE.lock().await;
                let message = state.chats.iter_mut()
                    .find(|chat| chat.id() == &receiver)
                    .and_then(|chat| chat.messages.iter_mut().find(|m| m.id == *pending_id_for_update))
                    .unwrap();

                // Add the Attachment in-state
                message.attachments.push(Attachment {
                    id: file_hash.clone(),
                    key: String::new(),  // MLS uses derived keys, not explicit
                    nonce: mls_upload_result.nonce.clone(),
                    extension: attached_file.extension.clone(),
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
                });

                // Emit update to frontend
                handle.emit("mls_message_new", serde_json::json!({
                    "group_id": &receiver,
                    "message": &message
                })).unwrap();
            }

            // Build Kind 9 event with text content and imeta tag
            let mut mls_rumor = EventBuilder::new(
                Kind::from_u16(crate::stored_event::event_kind::MLS_CHAT_MESSAGE),
                msg.content.clone()
            ).tag(mls_upload_result.imeta_tag);

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
            {
                let mut state = STATE.lock().await;
                if let Some(message) = state.chats.iter_mut()
                    .find(|chat| chat.id() == &receiver)
                    .and_then(|chat| chat.messages.iter_mut().find(|m| m.id == *pending_id))
                {
                    // Update with actual event ID
                    let old_id = message.id.clone();
                    message.id = event_id.clone();
                    message.pending = false;

                    // Emit update to frontend
                    handle.emit("message_update", serde_json::json!({
                        "old_id": old_id,
                        "message": message,
                        "chat_id": &receiver
                    })).unwrap();

                    // Save to database
                    let message_to_save = message.clone();
                    drop(state);
                    let _ = crate::db::save_message(handle.clone(), &receiver, &message_to_save).await;
                }
            }

            return Ok(true);
        }

        // ============================================================
        // DM ATTACHMENTS: Continue with existing Kind 15 approach
        // ============================================================

        // Check for existing attachment with same hash across all profiles BEFORE encrypting
        // BUT: Never reuse empty file hashes - always force a new upload
        let existing_attachment = if file_hash == EMPTY_FILE_HASH {
            None
        } else {
            let mut found_attachment: Option<(String, Attachment)> = None;
            
            // First, search through in-memory state (fastest check)
            {
                let state = STATE.lock().await;
                for chat in &state.chats {
                    for message in &chat.messages {
                        for attachment in &message.attachments {
                            if attachment.id == file_hash && !attachment.url.is_empty() {
                                // Found a matching attachment with a valid URL
                                // For DMs, use first participant; for groups, use chat ID
                                let chat_identifier = if let Some(participant_id) = chat.participants.first() {
                                    participant_id.clone()
                                } else {
                                    // Group chat - use the chat ID itself
                                    chat.id.clone()
                                };
                                found_attachment = Some((chat_identifier, attachment.clone()));
                                break;
                            }
                        }
                        if found_attachment.is_some() {
                            break;
                        }
                    }
                    if found_attachment.is_some() {
                        break;
                    }
                }
            }
            
            // Fallback: check database index if not found in memory (covers all stored attachments)
            if found_attachment.is_none() {
                if let Ok(index) = db::build_file_hash_index(handle).await {
                    if let Some(attachment_ref) = index.get(&file_hash) {
                        // Found in database index - convert AttachmentRef to Attachment
                        found_attachment = Some((attachment_ref.chat_id.clone(), Attachment {
                            id: attachment_ref.hash.clone(),
                            url: attachment_ref.url.clone(),
                            key: attachment_ref.key.clone(),
                            nonce: attachment_ref.nonce.clone(),
                            extension: attachment_ref.extension.clone(),
                            size: attachment_ref.size,
                            path: String::new(),
                            img_meta: None,
                            downloading: false,
                            downloaded: false,
                            webxdc_topic: None,   // Not stored in attachment index
                            group_id: None,       // DM attachments don't use MLS encryption
                            original_hash: None,  // Not stored in attachment index
                            scheme_version: None, // DM attachments don't use MIP-04
                            mls_filename: None,   // DM attachments don't use MIP-04
                        }));
                    }
                }
            }
            
            found_attachment
        };

        // Determine if we need to encrypt based on whether we'll reuse an existing attachment
        let will_reuse_existing = if let Some((_, ref existing)) = existing_attachment {
            // Check if URL contains empty hash - never reuse those
            const EMPTY_FILE_HASH: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
            if existing.url.contains(EMPTY_FILE_HASH) {
                false
            } else {
                // Check if URL is live
                match net::check_url_live(&existing.url).await {
                    Ok(is_live) => is_live,
                    Err(_) => false
                }
            }
        } else {
            false
        };

        // Only encrypt if we won't reuse an existing attachment
        let (params, enc_file) = if will_reuse_existing {
            // Skip encryption for duplicate files - we'll reuse existing encryption params
            (crypto::EncryptionParams { key: String::new(), nonce: String::new() }, Vec::new())
        } else {
            // Encrypt the attachment - either it's new or the existing URL is dead
            let params = crypto::generate_encryption_params();
            let enc_file = crypto::encrypt_data(&*attached_file.bytes, &params).unwrap();
            (params, enc_file)
        };

        // For WebXDC (.xdc) files, generate topic ID upfront so it's available immediately
        // This needs to be outside the block so it's available when building the Nostr event
        let webxdc_topic = if attached_file.extension.to_lowercase() == "xdc" {
            let topic_id = generate_topic_id();
            Some(encode_topic_id(&topic_id))
        } else {
            None
        };

        // Update the attachment in-state
        {
            // Use a clone of the Arc for this block
            let pending_id_clone = Arc::clone(&pending_id);
            
            // Retrieve the Pending Message
            let mut state = STATE.lock().await;
            let message = state.chats.iter_mut()
                .find(|chat| {
                    // For DMs, check if receiver is a participant
                    // For MLS groups, check if receiver matches the chat ID
                    chat.id() == &receiver || chat.has_participant(&receiver)
                })
                .and_then(|chat| chat.messages.iter_mut().find(|m| m.id == *pending_id_clone))
                .unwrap();

            // Choose the appropriate base directory based on platform
            let base_directory = if cfg!(target_os = "ios") {
                tauri::path::BaseDirectory::Document
            } else {
                tauri::path::BaseDirectory::Download
            };

            // Resolve the directory path using the determined base directory
            let dir = handle.path().resolve("vector", base_directory).unwrap();

            // Store the hash-based file name on-disk for future reference
            let hash_file_path = dir.join(format!("{}.{}", &file_hash, &attached_file.extension));

            // Create the vector directory if it doesn't exist
            std::fs::create_dir_all(&dir).unwrap();

            // Save the hash-named file
            std::fs::write(&hash_file_path, &*attached_file.bytes).unwrap();

            // Determine encryption params and file size based on whether we found an existing attachment
            let (attachment_key, attachment_nonce, file_size) = if let Some((_, ref existing)) = existing_attachment {
                // Reuse existing encryption params
                (existing.key.clone(), existing.nonce.clone(), existing.size)
            } else {
                // Use new encryption params and encrypted file size
                (params.key.clone(), params.nonce.clone(), enc_file.len() as u64)
            };

            // Add the Attachment in-state (with our local path, to prevent re-downloading it accidentally from server)
            message.attachments.push(Attachment {
                // Use SHA256 hash as the ID
                id: file_hash.clone(),
                key: attachment_key,
                nonce: attachment_nonce,
                extension: attached_file.extension.clone(),
                url: String::new(),
                path: hash_file_path.to_string_lossy().to_string(),
                size: file_size,
                img_meta: attached_file.img_meta.clone(),
                downloading: false,
                downloaded: true,
                webxdc_topic: webxdc_topic.clone(),
                group_id: if is_group_chat { Some(receiver.clone()) } else { None },
                original_hash: Some(file_hash.clone()),
                scheme_version: None, // DM attachments use explicit key/nonce, not MIP-04
                mls_filename: None,   // DM attachments use explicit key/nonce, not MIP-04
            });

            // Send the pending file upload to our frontend with appropriate event
            // This provides immediate UI feedback for the sender
            if is_group_chat {
                handle.emit("mls_message_new", serde_json::json!({
                    "group_id": &receiver,
                    "message": &message
                })).unwrap();
            } else {
                handle.emit("message_new", serde_json::json!({
                    "message": &message,
                    "chat_id": &receiver
                })).unwrap();
            }
        }

        // Format a Mime Type from the file extension
        let mime_type = util::mime_from_extension(&attached_file.extension);

        // Check if we found an existing attachment with the same hash
        let mut should_upload = true;
        let attachment_rumor = if let Some((_found_profile_id, existing_attachment)) = existing_attachment {
            // Never reuse URLs with the empty file hash
            const EMPTY_FILE_HASH: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
            let is_empty_hash = existing_attachment.url.contains(EMPTY_FILE_HASH);
            
            // Verify the URL is still live before reusing (but skip if it's an empty hash)
            let url_is_live = if is_empty_hash {
                false
            } else {
                match net::check_url_live(&existing_attachment.url).await {
                    Ok(is_live) => is_live,
                    Err(_) => false // Treat errors as dead URL
                }
            };
            
            if url_is_live {
                should_upload = false;
                
                // Update our pending message with the existing URL
                {
                    let pending_id_for_update = Arc::clone(&pending_id);
                    let mut state = STATE.lock().await;
                    let message = state.chats.iter_mut()
                        .find(|chat| chat.id() == &receiver || chat.has_participant(&receiver))
                        .and_then(|chat| chat.messages.iter_mut().find(|m| m.id == *pending_id_for_update))
                        .unwrap();
                    if let Some(attachment) = message.attachments.last_mut() {
                        attachment.url = existing_attachment.url.clone();
                    }
                }
                
                // Create the attachment rumor with the existing URL
                let mut attachment_rumor = EventBuilder::new(Kind::from_u16(15), existing_attachment.url);
                
                // Only add p-tag for DMs, not for MLS groups
                if !is_group_chat {
                    attachment_rumor = attachment_rumor.tag(Tag::public_key(receiver_pubkey));
                }
                
                // Append decryption keys and file metadata (using existing attachment's params)
                attachment_rumor = attachment_rumor
                    .tag(Tag::custom(TagKind::custom("file-type"), [mime_type.as_str()]))
                    .tag(Tag::custom(TagKind::custom("size"), [existing_attachment.size.to_string()]))
                    .tag(Tag::custom(TagKind::custom("encryption-algorithm"), ["aes-gcm"]))
                    .tag(Tag::custom(TagKind::custom("decryption-key"), [existing_attachment.key.as_str()]))
                    .tag(Tag::custom(TagKind::custom("decryption-nonce"), [existing_attachment.nonce.as_str()]))
                    .tag(Tag::custom(TagKind::custom("ox"), [file_hash.clone()]));
                
                // Append image metadata if available
                if let Some(ref img_meta) = attached_file.img_meta {
                    attachment_rumor = attachment_rumor
                        .tag(Tag::custom(TagKind::custom("blurhash"), [&img_meta.blurhash]))
                        .tag(Tag::custom(TagKind::custom("dim"), [format!("{}x{}", img_meta.width, img_meta.height)]));
                }

                // For WebXDC (.xdc) files, use the topic ID from the attachment (generated earlier)
                if let Some(ref topic_encoded) = webxdc_topic {
                    attachment_rumor = attachment_rumor
                        .tag(Tag::custom(TagKind::custom("webxdc-topic"), [topic_encoded.clone()]));
                }

                attachment_rumor
            } else {
                // URL is dead, need to upload
                should_upload = true;
                EventBuilder::new(Kind::from_u16(15), String::new()) // Placeholder
            }
        } else {
            // No existing attachment found
            EventBuilder::new(Kind::from_u16(15), String::new()) // Placeholder
        };
        
        // Final attachment rumor - either reused or newly uploaded
        let final_attachment_rumor = if should_upload {
            // Upload the file to the server
            let client = NOSTR_CLIENT.get().expect("Nostr client not initialized");
            let signer = client.signer().await.unwrap();
            let servers = crate::get_blossom_servers();
            let file_size = enc_file.len();
            // Clone the Arc outside the closure for use inside a seperate-threaded progress callback
            let pending_id_for_callback = Arc::clone(&pending_id);
            // Create a progress callback for file uploads
            let progress_callback: crate::blossom::ProgressCallback = std::sync::Arc::new(move |percentage, _bytes| {
                    if let Some(pct) = percentage {
                        handle.emit("attachment_upload_progress", serde_json::json!({
                            "id": pending_id_for_callback.as_ref(),
                            "progress": pct
                        })).unwrap();
                    }
                Ok(())
            });

            // Upload the file with progress, retries, and automatic server failover
            match crate::blossom::upload_blob_with_progress_and_failover(signer.clone(), servers, Arc::new(enc_file), Some(mime_type.as_str()), progress_callback, Some(3), Some(std::time::Duration::from_secs(2))).await {
                Ok(url) => {
                    // Update our pending message with the uploaded URL
                    {
                        let pending_id_for_url_update = Arc::clone(&pending_id);
                        let mut state = STATE.lock().await;
                        let message = state.chats.iter_mut()
                            .find(|chat| chat.id() == &receiver || chat.has_participant(&receiver))
                            .and_then(|chat| chat.messages.iter_mut().find(|m| m.id == *pending_id_for_url_update))
                            .unwrap();
                        if let Some(attachment) = message.attachments.last_mut() {
                            attachment.url = url.clone();
                        }
                    }
                    
                    // Create the attachment rumor
                    let mut attachment_rumor = EventBuilder::new(Kind::from_u16(15), url);
                    
                    // Only add p-tag for DMs, not for MLS groups
                    if !is_group_chat {
                        attachment_rumor = attachment_rumor.tag(Tag::public_key(receiver_pubkey));
                    }

                    // Append decryption keys and file metadata
                    attachment_rumor = attachment_rumor
                        .tag(Tag::custom(TagKind::custom("file-type"), [mime_type.as_str()]))
                        .tag(Tag::custom(TagKind::custom("size"), [file_size.to_string()]))
                        .tag(Tag::custom(TagKind::custom("encryption-algorithm"), ["aes-gcm"]))
                        .tag(Tag::custom(TagKind::custom("decryption-key"), [params.key.as_str()]))
                        .tag(Tag::custom(TagKind::custom("decryption-nonce"), [params.nonce.as_str()]))
                        .tag(Tag::custom(TagKind::custom("ox"), [file_hash.clone()]));

                    // Append image metadata if available
                    if let Some(ref img_meta) = attached_file.img_meta {
                        attachment_rumor = attachment_rumor
                            .tag(Tag::custom(TagKind::custom("blurhash"), [&img_meta.blurhash]))
                            .tag(Tag::custom(TagKind::custom("dim"), [format!("{}x{}", img_meta.width, img_meta.height)]));
                    }

                    // For WebXDC (.xdc) files, use the topic ID from the attachment (generated earlier)
                    if let Some(ref topic_encoded) = webxdc_topic {
                        attachment_rumor = attachment_rumor
                            .tag(Tag::custom(TagKind::custom("webxdc-topic"), [topic_encoded.clone()]));
                    }

                    attachment_rumor
                },
                Err(e) => {
                    // The file upload failed: so we mark the message as failed and notify of an error
                    mark_message_failed(Arc::clone(&pending_id), &receiver).await;
                    // Return the error
                    eprintln!("[Blossom Error] Upload failed: {}", e);
                    return Err(format!("Failed to upload file: {}", e));
                }
            }
        } else {
            // We already have a valid attachment_rumor from the reuse logic
            attachment_rumor
        };
        
        // Return the final attachment rumor as the main rumor
        final_attachment_rumor
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
        match crate::mls::send_mls_message(&receiver, built_rumor.clone(), Some(pending_id.to_string())).await {
            Ok(_) => return Ok(true),
            Err(e) => {
                eprintln!("Failed to send MLS message: {:?}", e);
                return Ok(false);
            }
        }
    } else {
        // DM - use NIP-17 giftwrap
        // Send message to the real receiver with retry logic
        let mut send_attempts = 0;
        const MAX_ATTEMPTS: u32 = 12;
        const RETRY_DELAY: u64 = 5; // 5 seconds

        let mut final_output = None;

        while send_attempts < MAX_ATTEMPTS {
            send_attempts += 1;
            
            match client
                .gift_wrap(&receiver_pubkey, built_rumor.clone(), [])
                .await
            {
                Ok(output) => {
                    // Check if at least one relay acknowledged the message
                    if !output.success.is_empty() {
                        // Success! Message was acknowledged by at least one relay
                        // Extract wrapper_event_id BEFORE moving output
                        let wrapper_id = output.id().to_hex();
                        final_output = Some(output);
                        
                        // Immediately update frontend and save to DB
                        // This provides faster visual feedback without waiting for the self-send
                        {
                            let pending_id_for_early_update = Arc::clone(&pending_id);
                            let mut state = STATE.lock().await;
                            if let Some(msg) = state.chats.iter_mut()
                                .find(|chat| chat.id() == &receiver || chat.has_participant(&receiver))
                                .and_then(|chat| chat.messages.iter_mut().find(|m| m.id == *pending_id_for_early_update))
                            {
                                // Update the message ID and clear pending state
                                msg.id = rumor_id.to_hex();
                                msg.pending = false;
                                msg.wrapper_event_id = Some(wrapper_id);
                                
                                // Emit update to frontend for immediate visual feedback
                                let handle = TAURI_APP.get().unwrap();
                                let _ = handle.emit("message_update", serde_json::json!({
                                    "old_id": pending_id_for_early_update.as_ref(),
                                    "message": &msg,
                                    "chat_id": &receiver
                                }));
                                
                                // Save to DB immediately (don't wait for self-send)
                                let message_to_save = msg.clone();
                                let chat_to_save = state.get_chat(&receiver).cloned();
                                drop(state); // Release lock before async DB operations
                                
                                if let Some(chat) = chat_to_save {
                                    let _ = save_chat(handle.clone(), &chat).await;
                                    let _ = crate::db::save_message(handle.clone(), &receiver, &message_to_save).await;
                                }
                            }
                        }
                        
                        break;
                    } else if output.failed.is_empty() {
                        // No success but also no failures - this might be a temporary network issue
                        // Continue retrying
                    } else {
                        // We have failures but no successes
                        if send_attempts == MAX_ATTEMPTS {
                            // Final attempt failed
                            mark_message_failed(Arc::clone(&pending_id), &receiver).await;
                            return Ok(false);
                        }
                    }
                    
                    // If we're here and haven't reached max attempts, wait before retrying
                    if send_attempts < MAX_ATTEMPTS {
                        tokio::time::sleep(tokio::time::Duration::from_secs(RETRY_DELAY)).await;
                    }
                }
                Err(e) => {
                    // Network or other error - log and retry if we haven't exceeded attempts
                    eprintln!("Failed to send message (attempt {}/{}): {:?}", send_attempts, MAX_ATTEMPTS, e);
                    
                    if send_attempts == MAX_ATTEMPTS {
                        // Final attempt failed
                        mark_message_failed(Arc::clone(&pending_id), &receiver).await;
                        return Ok(false);
                    }
                    
                    // Wait before retrying
                    tokio::time::sleep(tokio::time::Duration::from_secs(RETRY_DELAY)).await;
                }
            }
        }
        
        // If we get here without final_output, all attempts failed
        if final_output.is_none() {
            mark_message_failed(Arc::clone(&pending_id), &receiver).await;
            return Ok(false);
        }

        // Send message to our own public key, to allow for message recovering
        let _ = client
            .gift_wrap(&my_public_key, built_rumor, [])
            .await;

        Ok(true)
    }
}

#[tauri::command]
pub async fn paste_message<R: Runtime>(handle: AppHandle<R>, receiver: String, replied_to: String, transparent: bool) -> Result<bool, String> {
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

    // Generate image metadata with Blurhash and dimensions
    let img_meta: Option<ImageMetadata> = util::generate_blurhash_from_rgba(
        img.as_raw(),
        img.width(),
        img.height()
    ).map(|blurhash| ImageMetadata {
        blurhash,
        width: img.width(),
        height: img.height(),
    });

    // Generate an Attachment File
    let attachment_file = AttachmentFile {
        bytes: Arc::new(encoded_bytes),
        img_meta,
        extension: extension.to_string(),
    };

    // Message the file to the intended user
    message(receiver, String::new(), replied_to, Some(attachment_file)).await
}

#[tauri::command]
pub async fn voice_message(receiver: String, replied_to: String, bytes: Vec<u8>) -> Result<bool, String> {
    // Generate an Attachment File
    let attachment_file = AttachmentFile {
        bytes: Arc::new(bytes),
        img_meta: None,
        extension: String::from("wav")
    };

    // Message the file to the intended user
    message(receiver, String::new(), replied_to, Some(attachment_file)).await
}
