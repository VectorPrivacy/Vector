//! Attachment handling Tauri commands.
//!
//! This module handles attachment operations:
//! - ThumbHash preview generation and decoding
//! - Attachment download, decryption, and saving

use std::collections::HashSet;
use std::sync::LazyLock;
use tokio::sync::Mutex;
use tauri::Emitter;

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
            // A stop only ever reaches an id this set already holds, so clearing an
            // earlier attempt's stop here cannot swallow one meant for this attempt.
            net::clear_transfer_cancel(&id);
            Some(Self { id })
        } else {
            None
        }
    }
}

impl Drop for ActiveDownloadGuard {
    fn drop(&mut self) {
        // Retire the stop with the download that owned it, so a cancelled file is
        // not still cancelled the next time someone asks for it.
        net::clear_transfer_cancel(&self.id);
        // Use try_lock to avoid blocking in drop (tokio Mutex).
        // In the rare case the lock is held, spawn a task to clean up.
        match ACTIVE_DOWNLOADS.try_lock() {
            Ok(mut active) => { active.remove(&self.id); }
            Err(_) => {
                let id = self.id.clone();
                // spawn-detached: removes an id from the process-wide in-flight download set.
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
            // The chat id for either type; a DM also answers to a participant's npub.
            let is_target_chat = chat.id == npub || match &chat.chat_type {
                ChatType::Community => false,
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

/// Whether Vector may hand an `.apk` to the system installer yet (Android only;
/// always true on desktop, which has no such gate). Lets the UI explain the
/// settings trip BEFORE the tap, rather than dumping the user into Settings
/// unannounced — the platform offers no runtime prompt for this one.
#[tauri::command]
pub async fn can_install_apks() -> Result<bool, String> {
    #[cfg(target_os = "android")]
    {
        Ok(crate::android::storage::can_install_apks())
    }
    #[cfg(not(target_os = "android"))]
    {
        Ok(true)
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

/// Where partial downloads live inside the account's directory.
const PARTIAL_DIR: &str = "partial-downloads";

/// Partials untouched this long are abandoned, not paused.
const PARTIAL_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(7 * 24 * 3600);

/// Remove abandoned partials and their checkpoints.
fn sweep_stale_partials(dir: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let now = std::time::SystemTime::now();
    for entry in entries.flatten() {
        let stale = entry
            .metadata()
            .and_then(|m| m.modified())
            .map(|t| now.duration_since(t).unwrap_or_default() > PARTIAL_MAX_AGE)
            .unwrap_or(false);
        if stale {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Why a finished download can't be used.
enum Verdict {
    /// These bytes aren't this blob: another source may still serve it.
    BadSource(String),
    /// Nothing another source could fix.
    Fatal(String),
}

/// Check a finished download against its content address and the sender's claim,
/// decrypting it into `staging` in the same pass. Returns the plaintext's path,
/// hash and length. Runs on a blocking thread: each step is a pass over the file.
fn verify_download(
    part: &std::path::Path,
    staging: &std::path::Path,
    url: &str,
    attachment: &Attachment,
) -> Result<(std::path::PathBuf, String, u64), Verdict> {
    use vector_core::crypto::stream::{decrypt_file, hash_file};
    let address = vector_core::blossom::blob_hash(url);
    let len = std::fs::metadata(part).map(|m| m.len()).map_err(|e| Verdict::Fatal(e.to_string()))?;
    if len < 16 {
        return Err(Verdict::BadSource(format!("Downloaded file too small ({} bytes)", len)));
    }

    // Plaintext public blob (no decryption tags): the content address and the
    // sender's `ox` claim are the only integrity checks there are.
    if attachment.key.is_empty() || attachment.nonce.is_empty() {
        let (hash, len) = hash_file(part).map_err(Verdict::Fatal)?;
        let claim = attachment.original_hash.as_deref();
        if address.as_deref().is_some_and(|a| a != hash) || claim.is_some_and(|c| c != hash) {
            return Err(Verdict::BadSource("Source served corrupt bytes".to_string()));
        }
        return Ok((part.to_path_buf(), hash, len));
    }

    match decrypt_file(part, staging, &attachment.key, &attachment.nonce) {
        Ok(done) => {
            if address.as_deref().is_some_and(|a| a != done.source_sha256) {
                let _ = std::fs::remove_file(staging);
                return Err(Verdict::BadSource("Source served corrupt bytes".to_string()));
            }
            Ok((staging.to_path_buf(), done.output_sha256, done.output_len))
        }
        Err(e) if e.contains("aead") => {
            // Bytes that match their address but won't decrypt are the right blob
            // with the wrong key; anything else is a bad copy.
            let verified = address.is_some_and(|a| hash_file(part).map(|(h, _)| h == a).unwrap_or(false));
            if verified {
                Err(Verdict::Fatal("Decryption failed - file may be corrupted".to_string()))
            } else {
                Err(Verdict::BadSource("Decryption failed - file may be corrupted".to_string()))
            }
        }
        Err(e) => Err(Verdict::Fatal(e)),
    }
}

/// This account's paused downloads: each partial's attachment id, how far it got
/// and how large the file is, for the Resume state a restart would otherwise lose.
#[tauri::command]
pub async fn get_paused_downloads() -> std::collections::HashMap<String, serde_json::Value> {
    let mut out = std::collections::HashMap::new();
    let Ok(dir) = vector_core::db::current_account_dir().map(|d| d.join(PARTIAL_DIR)) else { return out };
    let Ok(entries) = std::fs::read_dir(&dir) else { return out };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("part") {
            continue;
        }
        let Some(id) = path.file_stem().and_then(|s| s.to_str()) else { continue };
        if let Some(at) = net::read_checkpoint(&path).filter(|c| c.offset > 0) {
            out.insert(id.to_string(), serde_json::json!({ "offset": at.offset, "total": at.total }));
        }
    }
    out
}

/// Stop an in-progress attachment download.
///
/// Returns whether a live download was flagged. The download owns its outcome event,
/// so a `false` means nothing was running and the caller should not wait for one.
#[tauri::command]
pub async fn cancel_download(attachment_id: String) -> bool {
    let active = ACTIVE_DOWNLOADS.lock().await;
    if !active.contains(&attachment_id) {
        return false;
    }
    net::cancel_transfer(&attachment_id);
    true
}

/// Download and decrypt an attachment
#[tauri::command]
pub async fn download_attachment(npub: String, msg_id: String, attachment_id: String) -> bool {
    vector_core::db::scoped(async move {
        let handle = TAURI_APP.get().unwrap();
        // The multi-source walk (mirrors + hash-swap) can run for minutes against
        // dead hosts — an account swap mid-walk must never write this account's
        // download into the swapped-in account's STATE/DB.
        // The UI raises its spinner on invoke and only lowers it on this event, so an
        // exit that emits nothing spins forever with no progress and no error — and
        // every retry (or reload) takes the same silent path. Any refusal must speak,
        // AND persist: Copy Logs is the after-the-fact reporting channel, and a
        // refusal that only paints the UI leaves nothing to diagnose from.
        let fail = |reason: &str| {
            vector_core::log_net_fail!(
                "[AttachmentDownload] refused: {} (msg {}, attachment {})",
                reason, msg_id, attachment_id
            );
            handle.emit("attachment_download_result", serde_json::json!({
                "profile_id": &npub,
                "msg_id": &msg_id,
                "id": &attachment_id,
                "success": false,
                "result": reason
            })).ok();
            false
        };

        // Check global download deduplication — prevent multiple threads for the same file.
        // The RAII guard automatically removes the ID when this function returns (or panics).
        let _download_guard = match ActiveDownloadGuard::try_new(attachment_id.clone()).await {
            Some(guard) => guard,
            // The in-flight download owns the outcome event; staying silent here would
            // strand THIS caller's spinner if it's a duplicate of a stale render.
            None => return fail("This file is already downloading"),
        };

        // Grab the attachment's metadata from STATE — the single read surface. On a
        // miss, refill STATE from the DB ONCE and retry: STATE is authoritative, the DB
        // is consulted only to repopulate it. So a message that reached the UI without a
        // STATE hydration still downloads instead of hanging on a spinner.
        let (attachment, msg_is_mine, msg_author_npub) = {
            let mut refilled = false;
            loop {
            let mut state = STATE.lock().await;

            // Find the message and attachment in chats. Alongside the attachment,
            // capture whose blob this is (mine / author npub) — the BUD-03
            // hash-swap needs to know whose server list can plausibly hold it.
            let mut found_attachment = None;
            // Find target chat index first (immutable scan)
            let target_idx = state.chats.iter().position(|chat| match &chat.chat_type {
                ChatType::Community => chat.id == npub,
                // A DM's id IS the counterparty npub — match it first; the participant
                // check alone strands chats whose roster was persisted empty.
                ChatType::DirectMessage => chat.id == npub || chat.has_participant(&npub, &state.interner),
            });
            // Then mutably access only that chat
            if let Some(chat) = target_idx.map(|i| &mut state.chats[i]) {
                    if let Some(message) = chat.messages.find_by_hex_id_mut(&msg_id) {
                        let msg_mine = message.is_mine();
                        let msg_npub_idx = message.npub_idx;
                        if let Some(attachment) = message.attachments.iter_mut().find(|a| a.id_eq(&attachment_id)) {
                            // Check that we're not already downloading
                            if attachment.downloading() {
                                return fail("This file is already downloading");
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
                                    // The wire `size` is the CIPHERTEXT length; the disk file is
                                    // plaintext, 16 AES-GCM tag bytes shorter. Accept either form
                                    // (plaintext references carry the plaintext size) — an exact
                                    // equality here kept this whole branch dead and every
                                    // re-received file downloading bytes it already had.
                                    let size_ok = attachment.size == 0
                                        || std::fs::metadata(p)
                                            .map(|m| m.len() == attachment.size || m.len() + 16 == attachment.size)
                                            .unwrap_or(false);
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
                                // Third candidate: wherever the ledger says these bytes already
                                // landed — a collision-suffixed download, or a file we SENT from
                                // an arbitrary path. Rows are claims; content_matches still
                                // hash-verifies before anything is trusted.
                                let file_path = file_path.or_else(|| {
                                    vector_core::db::attachments::downloaded_paths_by_hash(&expected_hash)
                                        .unwrap_or_default()
                                        .into_iter()
                                        .map(std::path::PathBuf::from)
                                        .find(|p| p.exists() && content_matches(p))
                                });
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
                            found_attachment = Some((attachment.clone(), msg_mine, msg_npub_idx));
                        }
                    }
            }

            if let Some((att, mine, npub_idx)) = found_attachment {
                let author = state.interner.resolve(npub_idx).map(|s| s.to_string());
                break (att, mine, author);
            }
            drop(state);

            if !refilled {
                refilled = true;
                // Tripwire: rendered-implies-resident is the pipeline invariant (every
                // window served to the frontend round-trips through STATE), so a miss
                // here means some path violated it — worth a persisted line every time.
                vector_core::log_net_fail!(
                    "[AttachmentDownload] STATE miss for a rendered attachment (msg {}) — refilling from DB",
                    &msg_id[..msg_id.len().min(8)]
                );
                // STATE missed: refill this chat's window from the DB (STATE is authoritative,
                // the message is durable there) and retry. Guard the DB-read → STATE-write
                // against a mid-download account swap.
                if let Ok(msgs) = db::get_messages_around(&npub, &msg_id, 8, 8).await {
                    if !msgs.is_empty() {
                        let mut state = STATE.lock().await;
                        state.add_messages_to_chat_batch(&npub, msgs);
                    }
                }
                continue;
            }

            // Missed even after a DB refill: the message genuinely isn't in the DB, or
            // its attachment id doesn't match — nothing to download either way. The UI
            // is rendering it regardless, so say so instead of leaving a live spinner.
            vector_core::log_warn!(
                "[AttachmentDownload] not resolvable: chat {} msg {} attachment {} — rendered by the UI but absent from STATE and the DB",
                &npub[..npub.len().min(12)], &msg_id[..msg_id.len().min(8)], &attachment_id[..attachment_id.len().min(8)]
            );
            return fail("Attachment not found, reopen the chat and retry");
            }
        };

        let attachment_hex_id = util::bytes_to_hex_32(&attachment.id);

        // The ciphertext streams into this account's partial file and survives any
        // failure behind a checkpoint, so a retry, a mirror or the next launch picks
        // up where the bytes stopped. Content-addressed sources serve identical bytes,
        // so one partial serves them all.
        let part = match vector_core::db::current_account_dir() {
            Ok(dir) => dir.join(PARTIAL_DIR).join(format!("{}.part", attachment_hex_id)),
            Err(e) => {
                let mut state = STATE.lock().await;
                state.update_attachment(&npub, &msg_id, &attachment_id, |att| att.set_downloading(false));
                drop(state);
                return fail(&e);
            }
        };
        if let Some(dir) = part.parent() {
            sweep_stale_partials(dir);
        }
        let resumed = net::read_checkpoint(&part).unwrap_or_default();
        handle.emit("attachment_download_progress", serde_json::json!({
            "id": &attachment_hex_id,
            "progress": resumed.total.filter(|&t| t > 0).map(|t| resumed.offset * 100 / t).unwrap_or(0),
            "bytesDownloaded": resumed.offset,
        })).unwrap();

        // Walk the sources: primary URL first, then the BUD-04 `fallback` mirrors,
        // then BUD-03 hash-swap candidates from the author's server list. Per
        // source, transient network failures (dead connect, stalled or interrupted
        // stream) retry with backoff — media servers flake; permanent refusals
        // (blob gone, size cap) advance to the next source immediately. Bytes are
        // verified against the URL's content address and decrypted PER SOURCE: a
        // 2xx carrying the wrong bytes (lying middlebox, truncated body, corrupt
        // blob) is a dead source, not a dead download — advancing through the
        // mirrors on bad bytes is the redundancy they exist to provide.
        let attachment_for_decrypt = attachment.to_attachment();
        let mut candidates: Vec<String> = vec![attachment.url.to_string()];
        if let Some(fbs) = attachment.fallback_urls.as_deref() {
            candidates.extend(fbs.iter().map(|u| u.to_string()));
        }
        let mut saved: Option<(std::path::PathBuf, String)> = None;
        let mut last_error: String = "Download failed".to_string();
        let mut hash_swap_tried = false;
        let mut i = 0;
        while i < candidates.len() {
            // A stop outranks the walk: a dead source must not buy the next one a
            // fresh round of timeouts after the user already said no.
            if net::transfer_cancelled(&attachment_hex_id) {
                break;
            }
            let url = candidates[i].clone();

            // Fetch this source's bytes, retrying transient failures with backoff.
            // Each retry resumes from the checkpoint rather than the first byte.
            let mut attempt: u32 = 0;
            let fetched = loop {
                let reporter = net::TauriProgressReporter::new(handle, &attachment_hex_id);
                match net::download_to_file(&url, &part, &reporter).await {
                    Ok(_) => break true,
                    Err(error) => {
                        if error == net::TRANSFER_CANCELLED {
                            break false;
                        }
                        attempt += 1;
                        last_error = error.to_string();
                        let permanent = matches!(
                            error,
                            "Media server returned an error status"
                                | "File exceeds the maximum download size"
                        );
                        if !permanent && attempt < 3 {
                            vector_core::log_warn!(
                                "[AttachmentDownload] attempt {} failed for {} — retrying: {}",
                                attempt, url, error
                            );
                            tokio::time::sleep(std::time::Duration::from_secs(2 * attempt as u64)).await;
                            continue;
                        }
                        break false;
                    }
                }
            };

            if fetched {
                let (src, url_for_check, att) = (part.clone(), url.clone(), attachment_for_decrypt.clone());
                let staging = vector_core::db::get_download_dir().join(format!(".{}.download", attachment_hex_id));
                let outcome = tokio::task::spawn_blocking(move || verify_download(&src, &staging, &url_for_check, &att))
                    .await
                    .unwrap_or_else(|e| Err(Verdict::Fatal(e.to_string())));
                match outcome {
                    Ok((ready, file_hash, len)) => {
                        let (name, extension) = (attachment_for_decrypt.name.clone(), attachment_for_decrypt.extension.clone());
                        let hash = file_hash.clone();
                        let placed = tokio::task::spawn_blocking(move || {
                            vector_core::crypto::stream::place_download(&ready, &hash, len, &name, &extension)
                        })
                        .await
                        .unwrap_or_else(|e| Err(e.to_string()));
                        net::discard_partial(&part);
                        match placed {
                            Ok(path) => saved = Some((path, file_hash)),
                            Err(e) => last_error = e,
                        }
                    }
                    Err(Verdict::BadSource(reason)) => {
                        // These bytes are wrong for this blob; the next source starts clean.
                        vector_core::log_net_fail!("[AttachmentDownload] {} — bad source: {}", reason, url);
                        net::discard_partial(&part);
                        last_error = reason;
                    }
                    Err(Verdict::Fatal(reason)) => {
                        // Verified ciphertext that won't decrypt is a key/nonce problem no
                        // mirror can fix; filesystem errors likewise end the walk.
                        net::discard_partial(&part);
                        vector_core::log_net_fail!(
                            "[AttachmentDownload] terminal failure: {} (msg {}, attachment {}, source {})",
                            reason, msg_id, attachment_id, url
                        );
                        let mut state = STATE.lock().await;
                        state.update_attachment(&npub, &msg_id, &attachment_id, |att| {
                            att.set_downloading(false);
                            att.set_downloaded(false);
                        });
                        drop(state);
                        handle.emit("attachment_download_result", serde_json::json!({
                            "profile_id": npub,
                            "msg_id": msg_id,
                            "id": attachment_id,
                            "success": false,
                            "result": reason
                        })).unwrap();
                        return false;
                    }
                }
            }

            if saved.is_some() {
                break;
            }

            i += 1;
            if i == candidates.len() && !hash_swap_tried {
                hash_swap_tried = true;
                // BUD-03 hash-swap, the last resort: every embedded URL died,
                // but the author's advertised servers may still hold the blob
                // under the same content-address. DMs resolve the author to
                // the chat partner (message npub is None in 1:1 chats).
                let servers = vector_core::blossom_servers::author_swap_servers(
                    Some(msg_author_npub.as_deref().unwrap_or(&npub)),
                    msg_is_mine,
                )
                .await;
                let extra = vector_core::blossom::hash_swap_candidates(&attachment.url, &servers);
                let fresh: Vec<String> = extra
                    .into_iter()
                    .filter(|c| !candidates.iter().any(|e| e == c))
                    .collect();
                if !fresh.is_empty() {
                    vector_core::log_net_fail!(
                        "[AttachmentDownload] all {} embedded source(s) dead — hash-swapping onto {} author server(s)",
                        candidates.len(), fresh.len()
                    );
                    candidates.extend(fresh);
                }
            } else if i < candidates.len() {
                vector_core::log_net_fail!(
                    "[AttachmentDownload] source {}/{} exhausted ({}) — trying mirror: {}",
                    i, candidates.len(), last_error, candidates[i]
                );
            }
        }

        if saved.is_none() && net::transfer_cancelled(&attachment_hex_id) {
            vector_core::log_info!(
                "[AttachmentDownload] stopped by the user (msg {}, attachment {})",
                msg_id, attachment_id
            );
            // A stop is a choice, not an outage: nothing is kept to resume.
            net::discard_partial(&part);
            let mut state = STATE.lock().await;
            state.update_attachment(&npub, &msg_id, &attachment_id, |att| {
                att.set_downloading(false);
                att.set_downloaded(false);
            });
            drop(state);
            handle.emit("attachment_download_result", serde_json::json!({
                "profile_id": npub,
                "msg_id": msg_id,
                "id": attachment_id,
                "success": false,
                "cancelled": true,
                "result": net::TRANSFER_CANCELLED
            })).ok();
            return false;
        }

        let Some((hash_file_path, file_hash)) = saved else {
            // Every source is dead for now. Whatever arrived stays behind its
            // checkpoint, and the outcome says how far a resume would start.
            vector_core::log_net_fail!(
                "[AttachmentDownload] failed: {} (msg {}, attachment {}) after {} source(s), url {}",
                last_error, msg_id, attachment_id, candidates.len(), &*attachment.url
            );
            let mut state = STATE.lock().await;
            state.update_attachment(&npub, &msg_id, &attachment_id, |att| {
                att.set_downloading(false);
                att.set_downloaded(false);
            });
            drop(state);
            let paused = net::read_checkpoint(&part).filter(|c| c.offset > 0);
            handle.emit("attachment_download_result", serde_json::json!({
                "profile_id": npub,
                "msg_id": msg_id,
                "id": attachment_id,
                "success": false,
                "result": last_error,
                "resumeFrom": paused.map(|c| c.offset),
                "total": paused.and_then(|c| c.total),
            })).unwrap();
            return false;
        };

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
                // Chat/message can vanish mid-download (deletion race, swap
                // remnants) — skip persistence rather than panic.
                let found = state.get_chat(&npub).and_then(|chat| {
                    let chat_id = chat.id().clone();
                    chat.messages.find_by_hex_id(&msg_id)
                        .map(|m| (chat_id, m.to_message(&state.interner)))
                });
                let Some((chat_id, updated_message)) = found else {
                    drop(state);
                    return true;
                };

                // Update the frontend state
                handle.emit("message_update", serde_json::json!({
                    "old_id": &updated_message.id,
                    "message": &updated_message,
                    "chat_id": &chat_id
                })).unwrap();

                // In-memory backfill: update EVERY resident message sharing the
                // attachment hash — across ALL chats, not just this one. The same
                // file forwarded between a DM and a Community (smart-forward's
                // whole point) has copies in both; the DB backfill below covers
                // disk, but a chat already loaded in STATE would keep offering a
                // download for a file that's on disk until reopened.
                // Two passes to satisfy the borrow checker (mut for update, then immut for serialize).
                let hash_bytes = hex_string_to_bytes(&file_hash);
                let mut backfilled: Vec<(String, String)> = Vec::new(); // (chat_id, msg_id)
                for chat_mut in state.chats.iter_mut() {
                    let cid = chat_mut.id.clone();
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
                            backfilled.push((cid.clone(), compact_msg.id_hex()));
                        }
                    }
                }
                // Emit message_update for each backfilled message, into ITS chat.
                for (backfill_chat, backfill_id) in &backfilled {
                    if let Some(chat_ref) = state.get_chat(backfill_chat) {
                        if let Some(compact_msg) = chat_ref.messages.find_by_hex_id(backfill_id) {
                            let backfill_msg = compact_msg.to_message(&state.interner);
                            handle.emit("message_update", serde_json::json!({
                                "old_id": &backfill_msg.id,
                                "message": &backfill_msg,
                                "chat_id": backfill_chat
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
    })
    .await
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

