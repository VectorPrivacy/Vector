//! Attachment handling Tauri commands.
//!
//! This module handles attachment operations:
//! - ThumbHash preview generation and decoding
//! - Attachment download, decryption, and saving
//! - MLS attachment decryption (MIP-04)

use std::collections::HashSet;
use std::sync::LazyLock;
use tokio::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager, Runtime};

use crate::{STATE, TAURI_APP, ChatType, Attachment};
use crate::{util, crypto, net, db, mls, simd};
use crate::util::hex_string_to_bytes;

/// Global set of attachment IDs currently being downloaded.
/// Prevents duplicate download threads for the same file (deduplication).
static ACTIVE_DOWNLOADS: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

/// RAII guard that removes an attachment ID from ACTIVE_DOWNLOADS when dropped.
/// Prevents leaking IDs if any code path panics or returns early.
struct ActiveDownloadGuard {
    id: String,
}

impl ActiveDownloadGuard {
    /// Try to insert `id` into the active set. Returns `Some(guard)` if inserted,
    /// `None` if already present (another download is in progress).
    async fn try_new(id: String) -> Option<Self> {
        let mut active = ACTIVE_DOWNLOADS.lock().await;
        if active.insert(id.clone()) {
            Some(Self { id })
        } else {
            None
        }
    }
}

impl Drop for ActiveDownloadGuard {
    fn drop(&mut self) {
        // Use try_lock to avoid blocking in drop (tokio Mutex).
        // In the rare case the lock is held, spawn a task to clean up.
        match ACTIVE_DOWNLOADS.try_lock() {
            Ok(mut active) => { active.remove(&self.id); }
            Err(_) => {
                let id = self.id.clone();
                tokio::spawn(async move {
                    ACTIVE_DOWNLOADS.lock().await.remove(&id);
                });
            }
        }
    }
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Sanitize a filename for protocol transmission and on-disk storage.
/// Permissive: allows spaces, accents, parentheses, unicode, etc.
/// Only strips characters that are dangerous for filesystems or security:
/// path separators (/ \), null bytes, and Windows-unsafe chars (: * ? " < > |).
/// Truncates the stem to 64 characters. Returns empty string if nothing valid remains.
pub(crate) fn sanitize_filename(name: &str) -> String {
    // Take only the final path component (strip any directory traversal)
    let base = name.rsplit('/').next().unwrap_or(name);
    let base = base.rsplit('\\').next().unwrap_or(base);

    // Strip characters dangerous to filesystems (path separators, null, Windows-unsafe)
    let sanitized: String = base.chars().filter(|c| {
        !matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\0')
    }).collect();

    // Strip leading/trailing dots and spaces
    let sanitized = sanitized.trim_matches(|c: char| c == '.' || c == ' ');

    if sanitized.is_empty() {
        return String::new();
    }

    // Truncate the stem to 64 characters (preserve extension)
    if let Some(dot_pos) = sanitized.rfind('.') {
        let stem = &sanitized[..dot_pos];
        let ext = &sanitized[dot_pos..]; // includes the dot
        if stem.len() > 64 {
            // Truncate at a char boundary
            let truncated = &stem[..stem.floor_char_boundary(64)];
            return format!("{}{}", truncated, ext);
        }
    } else if sanitized.len() > 64 {
        let truncated = &sanitized[..sanitized.floor_char_boundary(64)];
        return truncated.to_string();
    }

    sanitized.to_string()
}

/// Resolve a unique filename in the directory, appending -1, -2, etc. on collision.
pub(crate) fn resolve_unique_filename(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
    let candidate = dir.join(name);
    if !candidate.exists() {
        return candidate;
    }

    let stem = std::path::Path::new(name)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(name);
    let ext = std::path::Path::new(name)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("");

    // u32 counter — overflow is practically impossible (would require 4B+ same-named files)
    let mut counter = 1u32;
    loop {
        let suffixed = if ext.is_empty() {
            format!("{}-{}", stem, counter)
        } else {
            format!("{}-{}.{}", stem, counter, ext)
        };
        let candidate = dir.join(&suffixed);
        if !candidate.exists() {
            return candidate;
        }
        counter += 1;
    }
}

/// Decrypt and save an attachment to disk
///
/// For MLS attachments (when group_id is present), uses MDK's MIP-04 decryption.
/// For DM attachments, uses explicit key/nonce with AES-GCM.
///
/// Returns (path, content_hash) if successful, or an error message if unsuccessful
pub async fn decrypt_and_save_attachment<R: Runtime>(
    handle: &AppHandle<R>,
    encrypted_data: &[u8],
    attachment: &Attachment
) -> Result<(std::path::PathBuf, String), String> {
    // Decrypt the attachment using the appropriate method
    let decrypted_data = if let Some(ref group_id) = attachment.group_id {
        // MLS attachment - use MDK's MIP-04 decryption
        decrypt_mls_attachment(encrypted_data, attachment, group_id).await?
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

    // Use human-readable filename if available, otherwise fall back to hash-based
    let target_name = if attachment.name.is_empty() {
        format!("{}.{}", file_hash, attachment.extension)
    } else {
        attachment.name.clone()
    };

    // Create the vector directory if it doesn't exist
    std::fs::create_dir_all(&dir).map_err(|e| format!("Failed to create directory: {}", e))?;

    // If file with same name + same size + same hash already exists, reuse it (content dedup)
    let candidate = dir.join(&target_name);
    let already_exists = candidate.exists()
        && std::fs::metadata(&candidate).map(|m| m.len() == decrypted_data.len() as u64).unwrap_or(false)
        && std::fs::read(&candidate).map(|b| util::calculate_file_hash(&b) == file_hash).unwrap_or(false);

    if already_exists {
        return Ok((candidate, file_hash));
    }

    let file_path = resolve_unique_filename(&dir, &target_name);

    // Atomic write: write to temp file then rename, so the file is never 0 bytes
    // (prevents corrupted state if another thread reads concurrently on macOS APFS)
    let tmp_path = dir.join(format!(".{}.{}.tmp", file_hash, attachment.extension));
    std::fs::write(&tmp_path, &decrypted_data).map_err(|e| format!("Failed to write file: {}", e))?;
    std::fs::rename(&tmp_path, &file_path).map_err(|e| format!("Failed to rename file: {}", e))?;

    Ok((file_path, file_hash))
}

/// Decrypt an MLS attachment using MDK's MIP-04 decryption
///
/// This derives the encryption key from the MLS group secret using the original file hash
/// and other metadata stored in the MediaReference. MDK internally handles epoch fallback,
/// trying historical epoch secrets if the current epoch's key doesn't work.
async fn decrypt_mls_attachment(
    encrypted_data: &[u8],
    attachment: &Attachment,
    group_id: &str,
) -> Result<Vec<u8>, String> {
    use mdk_core::encrypted_media::MediaReference;

    // Create MLS service
    let mls_service = mls::MlsService::new_persistent_static()
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
    let engine_gid_bytes = hex_string_to_bytes(&group_meta.engine_group_id);
    let gid = mdk_core::GroupId::from_slice(&engine_gid_bytes);

    // Get MDK engine and media manager
    let mdk = mls_service.engine()
        .map_err(|e| format!("Failed to get MDK engine: {}", e))?;
    let media_manager = mdk.media_manager(gid);

    // Parse the original_hash from the attachment
    let original_hash_hex = attachment.original_hash.as_ref()
        .ok_or("MLS attachment missing original_hash")?;
    let original_hash_bytes = hex_string_to_bytes(original_hash_hex);
    let original_hash: [u8; 32] = original_hash_bytes.try_into()
        .map_err(|_| "Invalid original_hash length (expected 32 bytes)")?;

    // Parse the nonce from the attachment
    let nonce_bytes = hex_string_to_bytes(&attachment.nonce);
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
        mime_type,
        filename,
        dimensions: attachment.img_meta.as_ref().map(|m| (m.width, m.height)),
        scheme_version,
        nonce,
    };

    media_manager.decrypt_from_download(encrypted_data, &media_ref)
        .map_err(|e| format!("MIP-04 decryption failed: {}", e))
}

// ============================================================================
// Tauri Commands
// ============================================================================

/// Generate a thumbhash preview for an attachment
#[tauri::command]
pub async fn generate_thumbhash_preview(npub: String, msg_id: String) -> Result<String, String> {
    // Get the first attachment from the message by searching through chats
    let img_meta = {
        let state = STATE.lock().await;

        // Search through all chats to find the message
        let mut found_attachment = None;

        for chat in &state.chats {
            // Check if this is the target chat (works for both DMs and group chats)
            let is_target_chat = match &chat.chat_type {
                ChatType::MlsGroup => chat.id == npub,
                ChatType::DirectMessage => chat.has_participant(&npub, &state.interner),
            };

            if is_target_chat {
                // Look for the message in this chat
                if let Some(message) = chat.messages.find_by_hex_id(&msg_id) {
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

    // Generate the Base64 image using the decode_thumbhash_to_base64 function
    let base64_image = util::decode_thumbhash_to_base64(&img_meta.thumbhash);

    Ok(base64_image)
}

/// Generic thumbhash decoder - converts a thumbhash string to a base64 data URL
/// Used by the GIF picker for placeholder backgrounds
#[tauri::command]
pub fn decode_thumbhash(thumbhash: String) -> String {
    util::decode_thumbhash_to_base64(&thumbhash)
}

/// Download and decrypt an attachment
#[tauri::command]
pub async fn download_attachment(npub: String, msg_id: String, attachment_id: String) -> bool {
    // Check global download deduplication — prevent multiple threads for the same file.
    // The RAII guard automatically removes the ID when this function returns (or panics).
    let _download_guard = match ActiveDownloadGuard::try_new(attachment_id.clone()).await {
        Some(guard) => guard,
        None => return false, // Already downloading
    };

    let handle = TAURI_APP.get().unwrap();

    // Grab the attachment's metadata by searching through chats
    let attachment = {
        let mut state = STATE.lock().await;

        // Find the message and attachment in chats
        let mut found_attachment = None;
        // Find target chat index first (immutable scan)
        let target_idx = state.chats.iter().position(|chat| match &chat.chat_type {
            ChatType::MlsGroup => chat.id == npub,
            ChatType::DirectMessage => chat.has_participant(&npub, &state.interner),
        });
        // Then mutably access only that chat
        if let Some(chat) = target_idx.map(|i| &mut state.chats[i]) {
                if let Some(message) = chat.messages.find_by_hex_id_mut(&msg_id) {
                    if let Some(attachment) = message.attachments.iter_mut().find(|a| a.id_eq(&attachment_id)) {
                        // Check that we're not already downloading
                        if attachment.downloading() {
                            return false;
                        }

                        // Check if file already exists on disk (downloaded but flag was wrong)
                        let base_directory = if cfg!(target_os = "ios") {
                            tauri::path::BaseDirectory::Document
                        } else {
                            tauri::path::BaseDirectory::Download
                        };

                        if let Ok(vector_dir) = handle.path().resolve("vector", base_directory) {
                            // Check both hash-based and human-readable filenames
                            let hash_path = vector_dir.join(format!("{}.{}", simd::bytes_to_hex_32(&attachment.id), &*attachment.extension));
                            let name_path = if !attachment.name.is_empty() {
                                Some(vector_dir.join(&*attachment.name))
                            } else {
                                None
                            };
                            let file_path = if hash_path.exists() {
                                Some(hash_path)
                            } else {
                                name_path.filter(|p| p.exists())
                            };
                            if let Some(file_path) = file_path {
                                // File already exists! Update the state and return success
                                attachment.set_downloaded(true);
                                attachment.path = file_path.to_string_lossy().to_string().into_boxed_str();

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
                                    &chat_id_for_db,
                                    &msg_id_clone,
                                    &attachment_id_clone,
                                    true,
                                    &path_str
                                );

                                // Backfill other messages with the same attachment hash
                                let _ = db::backfill_attachment_downloaded_status(
                                    &attachment_id_clone,
                                    true,
                                    &path_str,
                                    &msg_id_clone,
                                );

                                return true;
                            }
                        }

                        // Enable the downloading flag to prevent re-calls
                        attachment.set_downloading(true);
                        found_attachment = Some(attachment.clone());
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
    let attachment_hex_id = simd::bytes_to_hex_32(&attachment.id);
    handle.emit("attachment_download_progress", serde_json::json!({
        "id": &attachment_hex_id,
        "progress": 0
    })).unwrap();

    // Download the file - no timeout, allow large downloads to complete
    let encrypted_data = match net::download(&*attachment.url, handle, &attachment_hex_id, None).await {
        Ok(data) => data,
        Err(error) => {
            // Handle download error
            let mut state = STATE.lock().await;
            state.update_attachment(&npub, &msg_id, &attachment_id, |att| {
                att.set_downloading(false);
                att.set_downloaded(false);
            });

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
        state.update_attachment(&npub, &msg_id, &attachment_id, |att| {
            att.set_downloading(false);
            att.set_downloaded(false);
        });
        drop(state);

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

    // Decrypt and save the file (convert CompactAttachment to Attachment for compatibility)
    let attachment_for_decrypt = attachment.to_attachment();
    let result = decrypt_and_save_attachment(handle, &encrypted_data, &attachment_for_decrypt).await;

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
            state.update_attachment(&npub, &msg_id, &attachment_id, |att| {
                att.set_downloading(false);
                att.set_downloaded(false);
            });

            // Log decryption errors but don't remove the attachment - allow retry
            if is_decryption_error {
                eprintln!("Decryption error for attachment {} - keeping for retry", attachment_id);
            }
            drop(state);

            // Emit the error
            handle.emit("attachment_download_result", serde_json::json!({
                "profile_id": npub,
                "msg_id": msg_id,
                "id": attachment_id,
                "success": false,
                "result": if is_decryption_error {
                    "Decryption failed - file may be corrupted".to_string()
                } else {
                    error
                }
            })).unwrap();
            return false;
        }
        Ok((hash_file_path, file_hash)) => {

            // Update state with successful download
            let path_str = hash_file_path.to_string_lossy().to_string();
            {
                let mut state = STATE.lock().await;
                state.update_attachment(&npub, &msg_id, &attachment_id, |att| {
                    // Update ID from nonce to hash
                    let hash_bytes = hex_string_to_bytes(&file_hash);
                    if hash_bytes.len() == 32 {
                        att.id.copy_from_slice(&hash_bytes);
                    }
                    att.set_downloading(false);
                    att.set_downloaded(true);
                    att.path = path_str.clone().into_boxed_str();
                });

                // Emit the finished download with both old and new IDs
                handle.emit("attachment_download_result", serde_json::json!({
                    "profile_id": npub,
                    "msg_id": msg_id,
                    "old_id": attachment_id,
                    "id": file_hash,
                    "success": true,
                    "result": &path_str,
                })).unwrap();

                // Persist updated message/attachment metadata to the database
                if let Some(handle) = TAURI_APP.get() {
                    // Find and save only the updated message (convert to Message for serialization)
                    let updated_chat = state.get_chat(&npub).unwrap();
                    let chat_id = updated_chat.id().clone();
                    let updated_message = updated_chat.messages.find_by_hex_id(&msg_id)
                        .map(|m| m.to_message(&state.interner))
                        .unwrap();

                    // Update the frontend state
                    handle.emit("message_update", serde_json::json!({
                        "old_id": &updated_message.id,
                        "message": &updated_message,
                        "chat_id": &chat_id
                    })).unwrap();

                    // In-memory backfill: update all other messages in this chat that share
                    // the same attachment hash, and push message_update events to the frontend.
                    // Two passes to satisfy the borrow checker (mut for update, then immut for serialize).
                    let hash_bytes = hex_string_to_bytes(&file_hash);
                    let mut backfilled_msg_ids: Vec<String> = Vec::new();
                    if let Some(chat_mut) = state.get_chat_mut(&npub) {
                        for compact_msg in chat_mut.messages.iter_mut() {
                            if compact_msg.id_hex() == msg_id { continue; }
                            let mut changed = false;
                            for att in compact_msg.attachments.iter_mut() {
                                if att.id == hash_bytes.as_slice() && !att.downloaded() {
                                    att.set_downloading(false);
                                    att.set_downloaded(true);
                                    att.path = path_str.clone().into_boxed_str();
                                    changed = true;
                                }
                            }
                            if changed {
                                backfilled_msg_ids.push(compact_msg.id_hex());
                            }
                        }
                    }
                    // Emit message_update for each backfilled message
                    if let Some(chat_ref) = state.get_chat(&npub) {
                        for backfill_id in &backfilled_msg_ids {
                            if let Some(compact_msg) = chat_ref.messages.find_by_hex_id(backfill_id) {
                                let backfill_msg = compact_msg.to_message(&state.interner);
                                handle.emit("message_update", serde_json::json!({
                                    "old_id": &backfill_msg.id,
                                    "message": &backfill_msg,
                                    "chat_id": &chat_id
                                })).unwrap();
                            }
                        }
                    }

                    // Drop the STATE lock before performing async I/O
                    drop(state);

                    let _ = db::save_message(&npub, &updated_message).await;

                    // Backfill other messages with the same attachment hash
                    let file_hash_clone = file_hash.clone();
                    let path_str_clone = path_str.clone();
                    let msg_id_clone = msg_id.clone();
                    let _ = db::backfill_attachment_downloaded_status(
                        &file_hash_clone,
                        true,
                        &path_str_clone,
                        &msg_id_clone,
                    );
                }
            }

            true
        }
    }
}

// Handler list for this module (for reference):
// - generate_thumbhash_preview
// - decode_thumbhash
// - download_attachment
