//! Attachment handling Tauri commands.
//!
//! This module handles attachment operations:
//! - ThumbHash preview generation and decoding
//! - Attachment download, decryption, and saving

use std::collections::HashSet;
use std::sync::LazyLock;
use tokio::sync::Mutex;
use tauri::{AppHandle, Emitter, Runtime};

use crate::{STATE, TAURI_APP, ChatType, Attachment};
use crate::{util, net, db};
use crate::util::hex_string_to_bytes;

/// Global set of attachment IDs currently being downloaded.
/// Prevents duplicate download threads for the same file (deduplication).
pub(crate) static ACTIVE_DOWNLOADS: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

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

/// Decrypt and save an attachment to disk
///
/// Uses explicit key/nonce with AES-GCM (DM/Community attachments).
///
/// Returns (path, content_hash) if successful, or an error message if unsuccessful
pub async fn decrypt_and_save_attachment<R: Runtime>(
    _handle: &AppHandle<R>,
    encrypted_data: &[u8],
    attachment: &Attachment
) -> Result<(std::path::PathBuf, String), String> {
    if attachment.group_id.is_some() {
        return Err("Group chat attachments are no longer supported".to_string());
    }
    vector_core::crypto::decrypt_and_save_attachment(
        encrypted_data, &attachment.key, &attachment.nonce,
        &attachment.name, &attachment.extension,
    )
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
                ChatType::Community => chat.id == npub,
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

/// Open a downloaded file with the user's chosen app.
///
/// Android has no "reveal in folder", so this launches an ACTION_VIEW chooser
/// via the FileProvider (the idiomatic equivalent). Desktop continues to use
/// the opener plugin's `revealItemInDir` from the frontend, so this is a no-op
/// fallback there. Returns true if an app was launched.
#[tauri::command]
pub async fn open_attachment(path: String) -> Result<bool, String> {
    #[cfg(target_os = "android")]
    {
        ensure_path_in_download_dir(&path)?;
        crate::android::storage::open_file(&path)
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = path;
        Ok(false)
    }
}

/// Share a downloaded file via Android's share sheet (ACTION_SEND).
/// No-op on non-Android (desktop shares are handled elsewhere). Returns true
/// if the share sheet was launched.
#[tauri::command]
pub async fn share_attachment(path: String) -> Result<bool, String> {
    #[cfg(target_os = "android")]
    {
        ensure_path_in_download_dir(&path)?;
        crate::android::storage::share_file(&path)
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = path;
        Ok(false)
    }
}

/// Whether Vector's saved media is currently hidden from the device gallery
/// (Android only). Always false on desktop. Drives the Storage settings toggle.
#[tauri::command]
pub async fn get_gallery_hidden() -> Result<bool, String> {
    #[cfg(target_os = "android")]
    {
        Ok(crate::android::storage::gallery_hidden())
    }
    #[cfg(not(target_os = "android"))]
    {
        Ok(false)
    }
}

/// Hide or reveal Vector's saved media in the device gallery (Android only).
/// No-op on desktop. Runs off the async runtime since it touches the filesystem
/// and the MediaScanner.
#[tauri::command]
pub async fn set_gallery_hidden(hidden: bool) -> Result<(), String> {
    #[cfg(target_os = "android")]
    {
        tokio::task::spawn_blocking(move || crate::android::storage::set_gallery_hidden(hidden))
            .await
            .map_err(|e| format!("join error: {:?}", e))?
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = hidden;
        Ok(())
    }
}

/// Reject any path that doesn't resolve to a real file inside Vector's download
/// dir. Hardening: the open/share intents hand a content:// URI to other apps
/// via the FileProvider (which is scoped to all external storage), so a
/// compromised webview must not be able to surface arbitrary files. Canonical
/// comparison defeats `..` traversal and symlinks.
#[cfg(target_os = "android")]
fn ensure_path_in_download_dir(path: &str) -> Result<(), String> {
    let dl = std::fs::canonicalize(vector_core::db::get_download_dir())
        .map_err(|_| "download dir unavailable".to_string())?;
    let target = std::fs::canonicalize(path).map_err(|_| "file not found".to_string())?;
    if target.starts_with(&dl) {
        Ok(())
    } else {
        Err("path is outside the download directory".to_string())
    }
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
            ChatType::Community => chat.id == npub,
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

                        // Check if file already exists on disk (downloaded but flag was wrong).
                        // Use the same canonical dir the write path uses
                        // (vector-core download dir) so dedup looks where files
                        // actually land — not a divergent Tauri-resolved path.
                        {
                            let vector_dir = vector_core::db::get_download_dir();
                            // Check both hash-based and human-readable filenames
                            let hash_path = vector_dir.join(format!("{}.{}", util::bytes_to_hex_32(&attachment.id), &*attachment.extension));
                            let name_path = if !attachment.name.is_empty() {
                                Some(vector_dir.join(&*attachment.name))
                            } else {
                                None
                            };
                            let expected_hash = util::bytes_to_hex_32(&attachment.id);
                            // Reuse requires a content-hash match, whatever the filename:
                            // an ox-named file proves nothing by itself (ox is the
                            // sender's CLAIM), and the honest pipeline never writes
                            // digest-named files at all — so a digest id can never match
                            // and correctly falls through to a real download. Size gates
                            // the read so an obvious mismatch skips the full hash.
                            let content_matches = |p: &std::path::PathBuf| {
                                let size_ok = attachment.size == 0
                                    || std::fs::metadata(p).map(|m| m.len() == attachment.size).unwrap_or(false);
                                size_ok
                                    && std::fs::read(p)
                                        .map(|b| util::calculate_file_hash(&b) == expected_hash)
                                        .unwrap_or(false)
                            };
                            let file_path = if hash_path.exists() && content_matches(&hash_path) {
                                Some(hash_path)
                            } else {
                                name_path.filter(|p| p.exists() && content_matches(p))
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
    let attachment_hex_id = util::bytes_to_hex_32(&attachment.id);
    handle.emit("attachment_download_progress", serde_json::json!({
        "id": &attachment_hex_id,
        "progress": 0
    })).unwrap();

    // Download the file - no timeout, allow large downloads to complete
    let encrypted_data = match net::download(&*attachment.url, handle, &attachment_hex_id, None).await {
        Ok(data) => data,
        Err(error) => {
            vector_core::log_warn!(
                "[AttachmentDownload] failed: {} (msg {}, attachment {}) url {}",
                error, msg_id, attachment_id, &*attachment.url
            );
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

            // Index the file so it appears in the gallery / file managers now.
            #[cfg(target_os = "android")]
            crate::android::storage::scan_file(&path_str);

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

/// Reconcile in-memory STATE against the boot integrity check. Boot preloads messages into STATE (and
/// ships them to the frontend) BEFORE the integrity check runs, so a file that went missing while
/// Vector was closed leaves the preloaded message (e.g. the latest one) painting a broken image — the
/// DB was corrected but memory + UI weren't. For each message whose id is in `affected`, clear the
/// now-missing attachment in STATE and emit `message_update` so the frontend swaps the broken image
/// for the re-download affordance. No DB write — the integrity check already persisted the correction.
pub(crate) async fn reconcile_missing_attachments_in_state(affected: &[String]) {
    if affected.is_empty() {
        return;
    }
    let affected: HashSet<&str> = affected.iter().map(|s| s.as_str()).collect();
    let mut state = STATE.lock().await;
    for chat_idx in 0..state.chats.len() {
        // Pass 1 (mut): clear the missing attachments on matching messages.
        let mut updated_ids: Vec<String> = Vec::new();
        for msg in state.chats[chat_idx].messages.iter_mut() {
            if msg.attachments.is_empty() {
                continue;
            }
            let hex = msg.id_hex();
            if !affected.contains(hex.as_str()) {
                continue;
            }
            let mut changed = false;
            for att in msg.attachments.iter_mut() {
                if att.downloaded() && !att.path.is_empty()
                    && !std::path::Path::new(&*att.path).exists()
                {
                    att.set_downloaded(false);
                    att.set_downloading(false);
                    att.path = String::new().into_boxed_str();
                    changed = true;
                }
            }
            if changed {
                updated_ids.push(hex);
            }
        }
        if updated_ids.is_empty() {
            continue;
        }
        // Pass 2 (immut): serialize + emit (disjoint borrows of chats[idx] and interner).
        let chat_id = state.chats[chat_idx].id().to_string();
        for hex in &updated_ids {
            if let Some(m) = state.chats[chat_idx].messages.find_by_hex_id(hex) {
                let message = m.to_message(&state.interner);
                vector_core::emit_event("message_update", &serde_json::json!({
                    "old_id": &message.id,
                    "message": &message,
                    "chat_id": &chat_id,
                }));
            }
        }
    }
}

// Handler list for this module (for reference):
// - generate_thumbhash_preview
// - decode_thumbhash
// - download_attachment
