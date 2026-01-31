//! Attachment handling Tauri commands.
//!
//! This module handles attachment operations:
//! - Blurhash preview generation and decoding
//! - Attachment download, decryption, and saving
//! - MLS attachment decryption (MIP-04)

use tauri::{AppHandle, Emitter, Manager, Runtime};

use crate::{STATE, TAURI_APP, ChatType, Attachment, Message};
use crate::{util, crypto, net, db, mls};
use crate::db::save_chat_messages;

// ============================================================================
// Helper Functions
// ============================================================================

/// Decrypt and save an attachment to disk
///
/// For MLS attachments (when group_id is present), uses MDK's MIP-04 decryption.
/// For DM attachments, uses explicit key/nonce with AES-GCM.
///
/// Returns the path to the decrypted file if successful, or an error message if unsuccessful
pub async fn decrypt_and_save_attachment<R: Runtime>(
    handle: &AppHandle<R>,
    encrypted_data: &[u8],
    attachment: &Attachment
) -> Result<std::path::PathBuf, String> {
    // Decrypt the attachment using the appropriate method
    let decrypted_data = if let Some(ref group_id) = attachment.group_id {
        // MLS attachment - use MDK's MIP-04 decryption
        decrypt_mls_attachment(handle, encrypted_data, attachment, group_id).await?
    } else {
        // DM attachment - use explicit key/nonce with AES-GCM
        crypto::decrypt_data(encrypted_data, &attachment.key, &attachment.nonce)
            .map_err(|e| e.to_string())?
    };

    // Calculate the hash of the decrypted file
    let file_hash = util::calculate_file_hash(&decrypted_data);

    // Choose the appropriate base directory based on platform
    let base_directory = if cfg!(target_os = "ios") {
        tauri::path::BaseDirectory::Document
    } else {
        tauri::path::BaseDirectory::Download
    };

    // Resolve the directory path using the determined base directory
    let dir = handle.path().resolve("vector", base_directory).unwrap();

    // Use hash-based filename
    let file_path = dir.join(format!("{}.{}", file_hash, attachment.extension));

    // Create the vector directory if it doesn't exist
    std::fs::create_dir_all(&dir).map_err(|e| format!("Failed to create directory: {}", e))?;

    // Save the file to disk
    std::fs::write(&file_path, decrypted_data).map_err(|e| format!("Failed to write file: {}", e))?;

    Ok(file_path)
}

/// Decrypt an MLS attachment using MDK's MIP-04 decryption
///
/// This derives the encryption key from the MLS group secret using the original file hash
/// and other metadata stored in the MediaReference.
async fn decrypt_mls_attachment<R: Runtime>(
    handle: &AppHandle<R>,
    encrypted_data: &[u8],
    attachment: &Attachment,
    group_id: &str,
) -> Result<Vec<u8>, String> {
    use mdk_core::encrypted_media::MediaReference;

    // Create MLS service
    let mls_service = mls::MlsService::new_persistent(handle)
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

    // Parse the engine group ID
    let engine_gid_bytes = hex::decode(&group_meta.engine_group_id)
        .map_err(|e| format!("Invalid engine_group_id hex: {}", e))?;
    let gid = mdk_core::GroupId::from_slice(&engine_gid_bytes);

    // Get MDK engine and media manager
    let mdk = mls_service.engine()
        .map_err(|e| format!("Failed to get MDK engine: {}", e))?;
    let media_manager = mdk.media_manager(gid);

    // Parse the original_hash from the attachment
    let original_hash_hex = attachment.original_hash.as_ref()
        .ok_or("MLS attachment missing original_hash")?;
    let original_hash_bytes = hex::decode(original_hash_hex)
        .map_err(|e| format!("Invalid original_hash hex: {}", e))?;
    let original_hash: [u8; 32] = original_hash_bytes.try_into()
        .map_err(|_| "Invalid original_hash length (expected 32 bytes)")?;

    // Parse the nonce from the attachment
    let nonce_bytes = hex::decode(&attachment.nonce)
        .map_err(|e| format!("Invalid nonce hex: {}", e))?;
    let nonce: [u8; 12] = nonce_bytes.try_into()
        .map_err(|_| "Invalid nonce length (expected 12 bytes)")?;

    // Use the stored filename if available (must match what was used during encryption!)
    // The filename is part of the AAD and must match exactly for decryption to succeed
    let filename = attachment.mls_filename.clone()
        .unwrap_or_else(|| format!("{}.{}", original_hash_hex, attachment.extension));

    // Determine MIME type from extension
    let raw_mime_type = util::mime_from_extension(&attachment.extension);

    // Normalize MIME type the same way as during encryption
    // MDK only accepts standard MIME types, so non-image files use octet-stream
    let mime_type = if raw_mime_type.starts_with("image/") {
        raw_mime_type
    } else {
        "application/octet-stream".to_string()
    };

    // Get scheme version from attachment (default to v2 if not stored)
    let scheme_version = attachment.scheme_version.clone()
        .unwrap_or_else(|| "mip04-v2".to_string());

    // Create a MediaReference for decryption
    let media_ref = MediaReference {
        url: attachment.url.clone(),
        original_hash,
        mime_type: mime_type.clone(),
        filename,
        dimensions: attachment.img_meta.as_ref().map(|m| (m.width, m.height)),
        scheme_version,
        nonce,
    };

    // Decrypt using MDK
    media_manager.decrypt_from_download(encrypted_data, &media_ref)
        .map_err(|e| format!("MIP-04 decryption failed: {}", e))
}

// ============================================================================
// Tauri Commands
// ============================================================================

/// Generate a blurhash preview for an attachment
#[tauri::command]
pub async fn generate_blurhash_preview(npub: String, msg_id: String) -> Result<String, String> {
    // Get the first attachment from the message by searching through chats
    let img_meta = {
        let state = STATE.lock().await;

        // Search through all chats to find the message
        let mut found_attachment = None;

        for chat in &state.chats {
            // Check if this is the target chat (works for both DMs and group chats)
            let is_target_chat = match &chat.chat_type {
                ChatType::MlsGroup => chat.id == npub,
                ChatType::DirectMessage => chat.has_participant(&npub),
            };

            if is_target_chat {
                // Look for the message in this chat
                if let Some(message) = chat.messages.iter().find(|m| m.id == msg_id) {
                    // Get the first attachment
                    if let Some(attachment) = message.attachments.first() {
                        found_attachment = attachment.img_meta.clone();
                        break;
                    }
                }
            }
        }

        found_attachment.ok_or_else(|| "No image attachment found".to_string())?
    };

    // Generate the Base64 image using the decode_blurhash_to_base64 function
    let base64_image = util::decode_blurhash_to_base64(
        &img_meta.blurhash,
        img_meta.width,
        img_meta.height,
        1.0 // Default punch value
    );

    Ok(base64_image)
}

/// Generic blurhash decoder - converts a blurhash string to a base64 data URL
/// Used by the GIF picker for placeholder backgrounds
#[tauri::command]
pub fn decode_blurhash(blurhash: String, width: u32, height: u32) -> String {
    util::decode_blurhash_to_base64(&blurhash, width, height, 1.0)
}

/// Download and decrypt an attachment
#[tauri::command]
pub async fn download_attachment(npub: String, msg_id: String, attachment_id: String) -> bool {
    let handle = TAURI_APP.get().unwrap();

    // Grab the attachment's metadata by searching through chats
    let attachment = {
        let mut state = STATE.lock().await;

        // Find the message and attachment in chats
        let mut found_attachment = None;
        for chat in &mut state.chats {
            // For group chats, npub is the group_id; for DMs, it's a participant npub
            let is_target_chat = match &chat.chat_type {
                ChatType::MlsGroup => chat.id == npub,
                ChatType::DirectMessage => chat.has_participant(&npub),
            };

            if is_target_chat {
                if let Some(message) = chat.messages.iter_mut().find(|m| m.id == msg_id) {
                    if let Some(attachment) = message.attachments.iter_mut().find(|a| a.id == attachment_id) {
                        // Check that we're not already downloading
                        if attachment.downloading {
                            return false;
                        }

                        // Check if file already exists on disk (downloaded but flag was wrong)
                        let base_directory = if cfg!(target_os = "ios") {
                            tauri::path::BaseDirectory::Document
                        } else {
                            tauri::path::BaseDirectory::Download
                        };

                        if let Ok(vector_dir) = handle.path().resolve("vector", base_directory) {
                            let file_path = vector_dir.join(format!("{}.{}", &attachment.id, &attachment.extension));
                            if file_path.exists() {
                                // File already exists! Update the state and return success
                                attachment.downloaded = true;
                                attachment.path = file_path.to_string_lossy().to_string();

                                // Emit success event
                                handle.emit("attachment_download_result", serde_json::json!({
                                    "profile_id": npub,
                                    "msg_id": msg_id,
                                    "id": attachment_id,
                                    "success": true,
                                    "result": file_path.to_string_lossy().to_string()
                                })).unwrap();

                                // Also update the database
                                let chat_id_for_db = chat.id().to_string();
                                let msg_id_clone = msg_id.clone();
                                let attachment_id_clone = attachment_id.clone();
                                let path_str = file_path.to_string_lossy().to_string();
                                drop(state); // Release lock before DB call

                                let _ = db::update_attachment_downloaded_status(
                                    handle,
                                    &chat_id_for_db,
                                    &msg_id_clone,
                                    &attachment_id_clone,
                                    true,
                                    &path_str
                                );

                                return true;
                            }
                        }

                        // Enable the downloading flag to prevent re-calls
                        attachment.downloading = true;
                        found_attachment = Some(attachment.clone());
                        break;
                    }
                }
            }
        }

        if found_attachment.is_none() {
            eprintln!("Attachment not found for download: {} in message {}", attachment_id, msg_id);
            return false;
        }

        found_attachment.unwrap()
    };

    // Begin our download progress events
    handle.emit("attachment_download_progress", serde_json::json!({
        "id": &attachment.id,
        "progress": 0
    })).unwrap();

    // Download the file - no timeout, allow large downloads to complete
    let encrypted_data = match net::download(&attachment.url, handle, &attachment.id, None).await {
        Ok(data) => data,
        Err(error) => {
            // Handle download error
            let mut state = STATE.lock().await;

            // Find and update the attachment status
            for chat in &mut state.chats {
                let is_target_chat = match &chat.chat_type {
                    ChatType::MlsGroup => chat.id == npub,
                    ChatType::DirectMessage => chat.has_participant(&npub),
                };

                if is_target_chat {
                    if let Some(message) = chat.messages.iter_mut().find(|m| m.id == msg_id) {
                        if let Some(attachment) = message.attachments.iter_mut().find(|a| a.id == attachment_id) {
                            attachment.downloading = false;
                            attachment.downloaded = false;
                            break;
                        }
                    }
                }
            }

            // Emit the error
            handle.emit("attachment_download_result", serde_json::json!({
                "profile_id": npub,
                "msg_id": msg_id,
                "id": attachment_id,
                "success": false,
                "result": error
            })).unwrap();
            return false;
        }
    };

    // Check if we got a reasonable amount of data
    if encrypted_data.len() < 16 {
        eprintln!("Downloaded file too small: {} bytes for attachment {}", encrypted_data.len(), attachment_id);
        let mut state = STATE.lock().await;

        // Find and update the attachment status
        for chat in &mut state.chats {
            let is_target_chat = match &chat.chat_type {
                ChatType::MlsGroup => chat.id == npub,
                ChatType::DirectMessage => chat.has_participant(&npub),
            };

            if is_target_chat {
                if let Some(message) = chat.messages.iter_mut().find(|m| m.id == msg_id) {
                    if let Some(attachment) = message.attachments.iter_mut().find(|a| a.id == attachment_id) {
                        attachment.downloading = false;
                        attachment.downloaded = false;
                        break;
                    }
                }
            }
        }

        // Emit a more helpful error
        let error_msg = format!("Downloaded file too small ({} bytes). URL may be invalid or expired.", encrypted_data.len());
        handle.emit("attachment_download_result", serde_json::json!({
            "profile_id": npub,
            "msg_id": msg_id,
            "id": attachment_id,
            "success": false,
            "result": error_msg
        })).unwrap();
        return false;
    }

    // Decrypt and save the file
    let result = decrypt_and_save_attachment(handle, &encrypted_data, &attachment).await;

    // Process the result
    match result {
        Err(error) => {
            // Check if this is a corrupted attachment (decryption failure)
            let is_decryption_error = error.contains("aead") || error.contains("decrypt");

            if is_decryption_error {
                eprintln!("Decryption failed for attachment {}: corrupted keys/data mismatch", attachment_id);
            }

            // Handle decryption/saving error
            let mut state = STATE.lock().await;

            // Find and update the attachment status
            let mut should_remove = false;
            for chat in &mut state.chats {
                let is_target_chat = match &chat.chat_type {
                    ChatType::MlsGroup => chat.id == npub,
                    ChatType::DirectMessage => chat.has_participant(&npub),
                };

                if is_target_chat {
                    if let Some(message) = chat.messages.iter_mut().find(|m| m.id == msg_id) {
                        if let Some(attachment) = message.attachments.iter_mut().find(|a| a.id == attachment_id) {
                            attachment.downloading = false;
                            attachment.downloaded = false;

                            // If it's a decryption error, mark for removal as it's corrupted
                            if is_decryption_error {
                                eprintln!("Marking corrupted attachment for removal: {}", attachment_id);
                                should_remove = true;
                            }
                            break;
                        }
                    }
                }
            }

            // Remove corrupted attachment if needed and save
            if should_remove {
                // Collect chat_id and messages to save
                let save_data: Option<(String, Vec<Message>)> = {
                    let mut result = None;
                    for chat in &mut state.chats {
                        let is_target_chat = match &chat.chat_type {
                            ChatType::MlsGroup => chat.id == npub,
                            ChatType::DirectMessage => chat.has_participant(&npub),
                        };

                        if is_target_chat {
                            let chat_id = chat.id().to_string();

                            if let Some(message) = chat.messages.iter_mut().find(|m| m.id == msg_id) {
                                let original_count = message.attachments.len();
                                message.attachments.retain(|a| a.id != attachment_id);
                                if message.attachments.len() < original_count {
                                    result = Some((chat_id, vec![message.clone()]));
                                }
                                break;
                            }
                        }
                    }
                    result
                };

                // Drop state and save
                drop(state);
                if let Some((chat_id, messages)) = save_data {
                    let _ = save_chat_messages(handle.clone(), &chat_id, &messages).await;
                }
            }

            // Emit the error
            handle.emit("attachment_download_result", serde_json::json!({
                "profile_id": npub,
                "msg_id": msg_id,
                "id": attachment_id,
                "success": false,
                "result": if should_remove {
                    "Corrupted attachment removed. Please re-send the file.".to_string()
                } else {
                    error
                }
            })).unwrap();
            return false;
        }
        Ok(hash_file_path) => {
            // Successfully decrypted and saved
            // Extract the hash from the filename (format: {hash}.{extension})
            let file_hash = hash_file_path.file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or(&attachment_id)
                .to_string();

            // Update state with successful download
            {
                let mut state = STATE.lock().await;

                // Find and update the attachment
                for chat in &mut state.chats {
                    let is_target_chat = match &chat.chat_type {
                        ChatType::MlsGroup => chat.id == npub,
                        ChatType::DirectMessage => chat.has_participant(&npub),
                    };

                    if is_target_chat {
                        if let Some(message) = chat.messages.iter_mut().find(|m| m.id == msg_id) {
                            if let Some(attachment_index) = message.attachments.iter().position(|a| a.id == attachment_id) {
                                let attachment = &mut message.attachments[attachment_index];
                                attachment.id = file_hash.clone(); // Update ID from nonce to hash
                                attachment.downloading = false;
                                attachment.downloaded = true;
                                attachment.path = hash_file_path.to_string_lossy().to_string(); // Update to hash-based path
                                break;
                            }
                        }
                    }
                }

                // Emit the finished download with both old and new IDs
                handle.emit("attachment_download_result", serde_json::json!({
                    "profile_id": npub,
                    "msg_id": msg_id,
                    "old_id": attachment_id,
                    "id": file_hash,
                    "success": true,
                })).unwrap();

                // Persist updated message/attachment metadata to the database
                if let Some(handle) = TAURI_APP.get() {
                    // Find and save only the updated message
                    let updated_chat = state.get_chat(&npub).unwrap();
                    let updated_message = {
                        updated_chat.messages.iter().find(|m| m.id == msg_id).cloned()
                    }.unwrap();

                    // Update the frontend state
                    handle.emit("message_update", serde_json::json!({
                        "old_id": &updated_message.id,
                        "message": updated_message.clone(),
                        "chat_id": updated_chat.id()
                    })).unwrap();

                    // Drop the STATE lock before performing async I/O
                    drop(state);

                    let _ = db::save_message(handle.clone(), &npub, &updated_message).await;
                }
            }

            true
        }
    }
}

// Handler list for this module (for reference):
// - generate_blurhash_preview
// - decode_blurhash
// - download_attachment
