//! Message sending functions.
//!
//! This module handles:
//! - Sending DM messages
//! - Paste message from clipboard
//! - Voice message sending

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::collections::HashMap;
use std::sync::LazyLock;
use nostr_sdk::prelude::*;
use tauri::{AppHandle, Emitter, Runtime};

/// Cancel flags for in-progress uploads, keyed by pending message ID.
pub(crate) static UPLOAD_CANCEL_FLAGS: LazyLock<std::sync::Mutex<HashMap<String, Arc<AtomicBool>>>> =
    LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));

#[cfg(not(target_os = "android"))]
use ::image::{ImageBuffer, Rgba};
#[cfg(not(target_os = "android"))]
use tauri_plugin_clipboard_manager::ClipboardExt;

use crate::{STATE, nostr_client};
use crate::util;
use crate::TAURI_APP;

use super::types::{AttachmentFile, ImageMetadata, Message};

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

#[derive(Clone, Copy)]
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
        bytes_sent: u64,
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
                "progress": percentage,
                "bytesSent": bytes_sent
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
        // Mid-flight persists (upload progress, previews) can land a DB row
        // under the optimistic pending id; the finalized message saves under
        // its real id, orphaning that row as a ghost duplicate on reload.
        if old_id.starts_with("pending-") && old_id != msg.id {
            let pending_id = old_id.to_string();
            let session = vector_core::state::SessionGuard::capture();
            tokio::spawn(async move {
                if !session.is_valid() { return; }
                let _ = vector_core::db::events::delete_event(&pending_id);
            });
        }
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
        // SessionGuard so a swap before the DB write doesn't persist
        // account A's outgoing message as a phantom chat row in B's DB.
        let session = vector_core::state::SessionGuard::capture();
        tokio::spawn(async move {
            if !session.is_valid() { return; }
            let _ = crate::db::save_message(&chat_id, &msg).await;
        });
    }
}

/// What removal action(s) are available to the user for a given
/// message? Used by the toolbar to gate the trash icon, pick the
/// right confirm copy + backend command, and decide on visual
/// affordance (full vs reduced opacity).
#[derive(serde::Serialize, Default)]
pub struct MessageDeleteOptions {
    /// User's own message — always shown a trash icon.
    pub mine: bool,
    /// Retained ephemeral wrap key exists, so we can do a true
    /// relay-level nuke (Layer 1). False on older messages or
    /// messages sent from a different device.
    pub has_retained_keys: bool,
    /// Message has at least one Blossom-uploaded attachment we can
    /// delete from the storage server even without retained wrap keys.
    pub has_attachments: bool,
    /// Not our message, but we have moderation authority over it (Community owner or
    /// an admin with MANAGE_MESSAGES who outranks the author) — show a "Hide"
    /// affordance that permanently cooperative-hides it for everyone.
    pub can_admin_hide: bool,
}

/// Load the owning Community of a channel. `Ok(None)` is a confident "not a
/// community channel" (or none stored locally); `Err(())` means a store read
/// failed, so no confident verdict exists.
fn load_community_for_channel(chat_id: &str) -> Result<Option<vector_core::community::Community>, ()> {
    let Some(cid) = vector_core::db::community::community_id_for_channel(chat_id).map_err(|_| ())? else {
        return Ok(None);
    };
    let bytes = vector_core::simd::hex::hex_to_bytes_32(&cid);
    vector_core::db::community::load_community(&vector_core::community::CommunityId(bytes)).map_err(|_| ())
}

/// Shared resolver behind `get_message_delete_options` and
/// `get_message_delete_meta_bulk` — one verdict path, so the hover toolbar and
/// the context menu can never disagree. STATE first, DB fallback for paged-out
/// rows (residency is a cache detail, not a verdict). Ids that can't be
/// confidently resolved — unknown everywhere, or any store lookup failed —
/// are OMITTED so callers retry later instead of caching a false verdict.
async fn resolve_delete_options(
    message_ids: &[String],
) -> HashMap<String, MessageDeleteOptions> {
    use nostr_sdk::EventId;
    use vector_core::ChatType;

    struct Ctx {
        /// `None` = paged-out row (chat type unknown) — check both key stores.
        chat_type: Option<ChatType>,
        chat_id: String,
        mine: bool,
        author: Option<String>,
        has_attachments: bool,
    }
    // One lock: snapshot the context for every resident id, then release before
    // the DB-bound authority/key lookups.
    let mut ctxs: HashMap<String, Ctx> = {
        let state = STATE.lock().await;
        message_ids
            .iter()
            .filter_map(|id| {
                state.find_message(id).map(|(chat, msg)| {
                    (
                        id.clone(),
                        Ctx {
                            chat_type: Some(chat.chat_type.clone()),
                            chat_id: chat.id.clone(),
                            mine: msg.mine,
                            author: msg.npub.clone(),
                            has_attachments: msg.attachments.iter().any(|a| !a.url.is_empty()),
                        },
                    )
                })
            })
            .collect()
    };
    // Paged-out rows: same context from the events table. `has_attachments` is
    // advisory popup copy only, so the fallback doesn't recompose attachments.
    for id in message_ids {
        if ctxs.contains_key(id) {
            continue;
        }
        if let Ok(Some((chat_id, mine, author))) = vector_core::db::events::event_delete_context(id) {
            ctxs.insert(
                id.clone(),
                Ctx { chat_type: None, chat_id, mine, author, has_attachments: false },
            );
        }
    }

    let me_pk = vector_core::state::my_public_key();
    // Memoise community loads across the batch (channel_id → load outcome). The memo keeps
    // Err distinct from Ok(None): a failed read must omit the verdict (below), not cache a
    // confident "no community" for every row of the channel.
    let mut community_cache: HashMap<String, Result<Option<vector_core::community::Community>, ()>> =
        HashMap::new();
    let mut out = HashMap::with_capacity(ctxs.len());

    for (id, ctx) in ctxs {
        // Moderation-hide: on someone ELSE's Community message, offer "Hide" iff we hold the
        // authority to actually publish it — MANAGE_MESSAGES + outrank the author (owner OR
        // admin). Mirrors the publish gate via the same shared `can_moderation_hide`, so the
        // button can't disagree with what the publish allows. A paged-out row (chat type
        // unknown) resolves through the same community lookup; non-community chats yield None.
        let maybe_community = matches!(ctx.chat_type, Some(ChatType::Community)) || ctx.chat_type.is_none();
        let can_admin_hide = if !ctx.mine && maybe_community {
            match (ctx.author.as_deref(), &me_pk) {
                (Some(author_hex), Some(me)) => {
                    let community = community_cache
                        .entry(ctx.chat_id.clone())
                        .or_insert_with(|| load_community_for_channel(&ctx.chat_id));
                    match community {
                        // Transient store failure: omit rather than cache a false "no Hide".
                        Err(()) => continue,
                        Ok(None) => false,
                        Ok(Some(c)) => vector_core::community::service::can_moderation_hide(
                            c,
                            &me.to_hex(),
                            author_hex,
                        ),
                    }
                }
                _ => false,
            }
        } else {
            false
        };

        let has_retained_keys = if ctx.mine {
            // A malformed hex id can't be a retained DM rumor: confident false, not an error.
            let dm_keys = || -> Result<bool, String> {
                match EventId::from_hex(&id) {
                    Ok(rid) => vector_core::db::nip17_keys::has_wrap_keys_for_rumor(&rid),
                    Err(_) => Ok(false),
                }
            };
            // Community channels retain a per-message ephemeral key on send; its presence
            // is exactly "can we do a real NIP-09 network delete" (full vs limited).
            let community_keys =
                || vector_core::db::community::get_message_key(&id).map(|k| k.is_some());
            let checked = match ctx.chat_type {
                Some(ChatType::DirectMessage) => dm_keys(),
                Some(ChatType::Community) => community_keys(),
                // Chat type unknown: an inner id exists in at most one store.
                None => match (community_keys(), dm_keys()) {
                    (Ok(true), _) | (_, Ok(true)) => Ok(true),
                    (Ok(false), Ok(false)) => Ok(false),
                    (Err(e), _) | (_, Err(e)) => Err(e),
                },
            };
            match checked {
                Ok(b) => b,
                // Transient store failure: omit rather than degrade to "limited".
                Err(_) => continue,
            }
        } else {
            false
        };

        out.insert(
            id,
            MessageDeleteOptions {
                mine: ctx.mine,
                has_retained_keys,
                has_attachments: ctx.has_attachments,
                can_admin_hide,
            },
        );
    }

    out
}

/// Surface what the toolbar should know about this message's
/// deletability. The toolbar uses these flags to gate the trash icon,
/// pick visual affordance (full / reduced opacity), and choose the
/// right confirm copy and backend command.
///
/// Always returns successfully — an unresolvable message returns the all-
/// false default (no delete UI; a reopen re-probes). The flags are advisory;
/// the backend commands themselves do whatever is actually possible at click time.
#[tauri::command]
pub async fn get_message_delete_options(message_id: String) -> Result<MessageDeleteOptions, String> {
    let mut map = resolve_delete_options(std::slice::from_ref(&message_id)).await;
    Ok(map.remove(&message_id).unwrap_or_default())
}

/// Backwards-compat shim: kept so older frontends/tests that call
/// `is_message_deletable` keep working. New code should use
/// `get_message_delete_options`.
#[tauri::command]
pub async fn is_message_deletable(message_id: String) -> Result<bool, String> {
    let opts = get_message_delete_options(message_id).await?;
    Ok(opts.mine)
}

/// The two backend-derived delete flags (the rest — `mine`, `has_attachments`,
/// pending/failed/download state — the toolbar reads live off the message).
#[derive(serde::Serialize)]
pub struct DeleteMeta {
    pub has_retained_keys: bool,
    pub can_admin_hide: bool,
}

/// Bulk variant of `get_message_delete_options`: resolves `has_retained_keys` +
/// `can_admin_hide` for a page of messages in one call, so the toolbar reads
/// them from a frontend cache instead of an IPC per hover. Same shared resolver
/// as the single-id command; unresolvable ids are omitted from the map (the
/// frontend leaves them uncached and re-queues later).
#[tauri::command]
pub async fn get_message_delete_meta_bulk(
    message_ids: Vec<String>,
) -> Result<std::collections::HashMap<String, DeleteMeta>, String> {
    let resolved = resolve_delete_options(&message_ids).await;
    Ok(resolved
        .into_iter()
        .map(|(id, o)| {
            (id, DeleteMeta { has_retained_keys: o.has_retained_keys, can_admin_hide: o.can_admin_hide })
        })
        .collect())
}

/// Delete an outbound DM from the network *and* locally.
///
/// DM: NIP-17 path — NIP-09 against every retained gift-wrap.
///
/// Only allows deletion of the user's own outbound messages
/// (`mine == true`). Messages without retained wrap keys (predate the
/// retention feature, sent from a different device) get a
/// distinguishable `NOT_DELETABLE` error so the frontend shows a clear
/// explanatory popup.
#[tauri::command]
pub async fn delete_own_message(message_id: String) -> Result<vector_core::DeleteOutcome, String> {
    use nostr_sdk::EventId;
    use vector_core::ChatType;

    // Confirm the message exists, is ours, and grab its chat type.
    let (chat_id, chat_type, is_mine) = {
        let state = STATE.lock().await;
        let (chat, msg) = state.find_message(&message_id)
            .ok_or_else(|| {
                eprintln!(
                    "[delete_own_message] message_id `{}` not found in STATE; chats={}",
                    message_id,
                    state.chats.len()
                );
                format!("Message not found (id: {})", message_id)
            })?;
        (chat.id.clone(), chat.chat_type.clone(), msg.mine)
    };
    if !is_mine {
        return Err("Cannot delete a message that isn't yours".to_string());
    }

    // Branch on chat type. The backend always does what it can —
    // retained-key relay nuke when available, cooperative-hide notice,
    // Blossom blob delete on attachments. The returned outcome tells
    // the frontend exactly which layers fired.
    let outcome = match chat_type {
        ChatType::DirectMessage => {
            let rumor_id = EventId::from_hex(&message_id)
                .map_err(|e| format!("Invalid message id: {}", e))?;
            vector_core::delete_own_dm(&rumor_id).await?
        }
        ChatType::Community => {
            return Err("Community channel messages are deleted via the Community service, not this path".to_string());
        }
    };

    // Remove from in-memory state.
    let removed = {
        let mut state = STATE.lock().await;
        state.remove_message(&message_id)
    };

    // Drop the local DB row.
    if removed.is_some() {
        if let Err(e) = crate::db::delete_event(&message_id).await {
            eprintln!("[delete_own_message] DB delete failed: {}", e);
        }
    }

    // Tell the frontend to drop the row.
    if let Some(handle) = TAURI_APP.get() {
        handle.emit("message_removed", serde_json::json!({
            "id": &message_id,
            "chat_id": &chat_id,
            "reason": "deleted"
        })).ok();
    }

    Ok(outcome)
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

    if let Some((chat_id, msg)) = removed {
        // Best-effort: drop the staged preview copy. Canonicalize both sides
        // so a stale or symlinked `att.path` can't follow out of download_dir.
        if let Ok(canonical_dl_dir) = std::fs::canonicalize(vector_core::db::get_download_dir()) {
            for att in &msg.attachments {
                if att.path.is_empty() { continue; }
                if let Ok(canonical_att) = std::fs::canonicalize(&att.path) {
                    if canonical_att.starts_with(&canonical_dl_dir) {
                        let _ = std::fs::remove_file(&canonical_att);
                    }
                }
            }
        }
        // Best-effort: free any blob already uploaded before the wrap failed.
        let remote_urls: Vec<String> = msg.attachments.iter()
            .filter_map(|a| if a.url.is_empty() { None } else { Some(a.url.to_string()) })
            .collect();
        if !remote_urls.is_empty() {
            // Best-effort blob cleanup — route through the active client
            // signer so bunker users sign auth events under their identity
            // instead of the client-key (which would fail with the server's
            // pubkey check).
            if let Some(client) = nostr_client() {
                if let Ok(signer) = client.signer().await {
                    vector_core::blossom::delete_blobs_best_effort(signer, remote_urls);
                }
            }
        }

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

    // Emit message_removed event so frontend removes the DOM element first.
    // Frontend image elements with `src=convertFileSrc(path)` hold a WebView
    // handle to the on-disk file — on Windows that's exclusive, so deleting
    // before DOM teardown completes raises ERROR_SHARING_VIOLATION. We emit
    // the event NOW (frontend starts its 300ms fade + remove) and defer the
    // file deletion below.
    if let Some((chat_id, msg)) = removed {
        if let Some(handle) = TAURI_APP.get() {
            handle.emit("message_removed", serde_json::json!({
                "id": &pending_id,
                "chat_id": &chat_id,
                "reason": "cancelled"
            })).ok();
        }

        // Deferred file cleanup: the upload thread doesn't keep the file open
        // (it uploads from in-memory bytes), but the WebView DOES via image
        // src. Spawn a task that:
        //   1. Sleeps 500ms — the frontend's message_removed handler does a
        //      ~300ms fade-out + DOM remove, which releases the WebView's
        //      file handle.
        //   2. Tries to remove the file. On Windows, retries with backoff for
        //      ERROR_SHARING_VIOLATION in case the WebView is slow to release.
        //   3. Scopes deletion to the in-app download dir so user-picked files
        //      elsewhere on disk are never touched.
        let attachments_to_delete: Vec<String> = msg.attachments
            .iter()
            .map(|a| a.path.clone())
            .filter(|p| !p.is_empty())
            .collect();

        if !attachments_to_delete.is_empty() {
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;

                let download_dir = vector_core::db::get_download_dir();
                let _ = std::fs::create_dir_all(&download_dir);
                let canonical_dl_dir = match std::fs::canonicalize(&download_dir) {
                    Ok(p) => p,
                    Err(_) => return,
                };

                for path in attachments_to_delete {
                    let canonical_path = match std::fs::canonicalize(std::path::Path::new(&path)) {
                        Ok(p) => p,
                        Err(_) => continue, // already gone, unreachable, or symlink-resolved out of bounds
                    };
                    if !canonical_path.starts_with(&canonical_dl_dir) {
                        continue; // out of scope: user-picked file from outside our dir
                    }

                    #[cfg(windows)]
                    {
                        // Retry on ERROR_SHARING_VIOLATION in case the WebView
                        // hasn't released the handle yet. Total budget ~2.25s.
                        if std::fs::remove_file(&canonical_path).is_err() {
                            for delay_ms in [50u64, 200, 500, 1500] {
                                tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                                if std::fs::remove_file(&canonical_path).is_ok() { break; }
                            }
                        }
                    }
                    #[cfg(not(windows))]
                    {
                        let _ = std::fs::remove_file(&canonical_path);
                    }
                }
            });
        }
    }

    Ok(())
}

/// Headless text-only reply: sends a DM.
/// Used by Android notification inline-reply (JNI).
#[allow(dead_code)]
pub async fn send_text_reply_headless(chat_id: &str, content: &str) -> Result<String, String> {
    let config = SendConfig::headless();
    let callback: Arc<dyn SendCallback> = Arc::new(TauriSendCallback);
    let result = vector_core::sending::send_dm(
        chat_id, content, None, &config, callback,
    ).await?;
    let event_id = result.event_id.unwrap_or(result.pending_id);

    crate::chat::mark_as_read_headless(chat_id).await;
    Ok(event_id)
}

#[tauri::command]
pub async fn message(receiver: String, content: String, replied_to: String, file: Option<AttachmentFile>) -> Result<MessageSendResult, String> {
    // Detect chat type early (needed for short-circuit)
    let is_group_chat = {
        let state = STATE.lock().await;
        if let Some(chat) = state.get_chat(&receiver) {
            chat.is_community()
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

    // Group chats are no longer supported.
    Err("Group chats are no longer supported".to_string())
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
    // Community channels route through the Concord file-bytes envelope; the DM `message`
    // command rejects channel ids. Mirrors how text/file sends fan out by chat type.
    let is_community = {
        let state = STATE.lock().await;
        match state.get_chat(&receiver) {
            Some(chat) => chat.is_community(),
            None => !receiver.starts_with("npub1"),
        }
    };
    if is_community {
        let reply = if replied_to.is_empty() { None } else { Some(replied_to) };
        // Empty-name attachment → renderer shows the voice player + transcription, not a file row.
        crate::commands::community::send_community_voice_bytes(receiver, bytes, reply).await?;
        // The Community path drives its own pending→sent lifecycle (no id to finalize).
        return Ok(MessageSendResult { pending_id: String::new(), event_id: None });
    }

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
