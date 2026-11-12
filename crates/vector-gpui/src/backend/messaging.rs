//! Reading history, marking read and sending.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use serde_json::json;
use vector_core::sending::{SendCallback, SendConfig};
use vector_core::{Message, STATE, db};

/// Cancel flags for uploads in flight, by pending id.
static UPLOAD_CANCEL: LazyLock<Mutex<HashMap<String, Arc<AtomicBool>>>> =
    LazyLock::new(Default::default);
/// A failure's reason, from `on_failed_reason` to the `on_failed` that follows it.
static FAIL_REASONS: LazyLock<Mutex<HashMap<String, String>>> = LazyLock::new(Default::default);

/// Stop every running upload and forget the failures: the account they belong to is leaving.
pub fn cancel_all_uploads() {
    let mut uploads = UPLOAD_CANCEL.lock().unwrap();
    uploads
        .values()
        .for_each(|flag| flag.store(true, std::sync::atomic::Ordering::Release));
    uploads.clear();
    FAIL_REASONS.lock().unwrap().clear();
}

pub fn cancel_upload(pending_id: &str) {
    if let Some(flag) = UPLOAD_CANCEL.lock().unwrap().get(pending_id) {
        flag.store(true, Ordering::Relaxed);
    }
}

/// The optimistic bubble, upload progress, then its sent or failed update: the desktop's.
#[derive(Clone, Copy)]
pub struct Callback;

impl SendCallback for Callback {
    fn on_pending(&self, chat_id: &str, msg: &Message) {
        if !msg.attachments.is_empty() {
            UPLOAD_CANCEL
                .lock()
                .unwrap()
                .insert(msg.id.clone(), Arc::new(AtomicBool::new(false)));
        }
        vector_core::traits::emit_message_new(chat_id, msg);
    }

    fn cancel_token(&self, pending_id: &str) -> Option<Arc<AtomicBool>> {
        UPLOAD_CANCEL.lock().unwrap().get(pending_id).cloned()
    }

    fn on_upload_progress(
        &self,
        pending_id: &str,
        percentage: u8,
        bytes_sent: u64,
    ) -> Result<(), String> {
        if self
            .cancel_token(pending_id)
            .is_some_and(|f| f.load(Ordering::Relaxed))
        {
            return Err("Upload cancelled".into());
        }
        vector_core::emit_event(
            "attachment_upload_progress",
            &json!({ "id": pending_id, "progress": percentage, "bytesSent": bytes_sent }),
        );
        Ok(())
    }

    fn on_upload_stage(&self, pending_id: &str, stage: &str, pct: Option<u8>) {
        vector_core::emit_event(
            "attachment_upload_stage",
            &json!({ "id": pending_id, "stage": stage, "progress": pct }),
        );
    }

    fn on_upload_complete(&self, chat_id: &str, pending_id: &str, attachment_id: &str, url: &str) {
        vector_core::emit_event(
            "attachment_update",
            &json!({ "chat_id": chat_id, "message_id": pending_id, "attachment_id": attachment_id, "url": url }),
        );
    }

    fn on_sent(&self, chat_id: &str, old_id: &str, msg: &Message) {
        UPLOAD_CANCEL.lock().unwrap().remove(old_id);
        if old_id.starts_with("pending-") && old_id != msg.id {
            let pending_id = old_id.to_string();
            db::spawn_bound(async move {
                let _ = db::events::delete_event(&pending_id).await;
            });
        }
        vector_core::traits::emit_message_replaced(chat_id, old_id, msg);
    }

    fn on_failed_reason(&self, _chat_id: &str, old_id: &str, reason: &str) {
        FAIL_REASONS
            .lock()
            .unwrap()
            .insert(old_id.to_string(), reason.to_string());
    }

    fn on_failed(&self, chat_id: &str, old_id: &str, msg: &Message) {
        UPLOAD_CANCEL.lock().unwrap().remove(old_id);
        let reason = FAIL_REASONS.lock().unwrap().remove(old_id);
        vector_core::emit_event(
            "message_update",
            &json!({ "old_id": old_id, "message": msg, "chat_id": chat_id, "reason": reason }),
        );
    }

    fn on_persist(&self, chat_id: &str, msg: &Message) {
        let (chat_id, msg) = (chat_id.to_string(), msg.clone());
        db::spawn_bound(async move {
            let _ = db::events::save_message(&chat_id, &msg).await;
        });
    }
}

/// Send `content` to a DM; `replied_to` quotes a message when not empty.
/// `bot` names the bot a `/` command is for; only a Community message carries the tag.
pub async fn send(receiver: String, content: String, replied_to: String, bot: Option<String>) -> Result<(), String> {
    if !receiver.starts_with("npub1") {
        return super::community::send(receiver, content, replied_to, bot).await;
    }
    let config = SendConfig {
        expiration: vector_core::self_destruct::resolve_send_expiry(&receiver),
        ..SendConfig::gui()
    };
    let reply = (!replied_to.is_empty()).then_some(replied_to.as_str());
    vector_core::sending::send_dm(&receiver, &content, reply, &config, Arc::new(Callback))
        .await
        .map(|_| ())
}

/// A page of a chat's messages, newest first, as the timeline renders them.
pub async fn message_views(
    chat_id: String,
    limit: usize,
    offset: usize,
) -> Result<Vec<Message>, String> {
    let id = db::id_cache::get_chat_id_by_identifier(&chat_id)?;
    db::events::get_message_views(id, limit, offset).await
}

/// Messages newer than `id` and the chat's total, for the scrollbar.
pub fn message_position(chat_id: &str, id: &str) -> Option<(usize, usize)> {
    let chat = db::id_cache::get_chat_id_by_identifier(chat_id).ok()?;
    db::events::get_message_position(chat, id).ok().flatten().map(|p| (p.newer, p.total))
}

/// When the message `newer` places from the newest was sent (ms).
pub fn message_time_at(chat_id: &str, newer: usize) -> Option<u64> {
    let chat = db::id_cache::get_chat_id_by_identifier(chat_id).ok()?;
    db::events::get_message_time_at(chat, newer).ok().flatten()
}

pub fn message_count(chat_id: &str) -> usize {
    db::id_cache::get_chat_id_by_identifier(chat_id)
        .and_then(db::events::get_chat_message_count)
        .unwrap_or(0)
}

/// The chat the user has open: what arrives there is read on arrival.
pub fn set_active_chat(chat_id: Option<String>) {
    vector_core::state::set_active_chat(chat_id);
}

pub async fn mark_as_read(chat_id: String, message_id: Option<String>) -> bool {
    let slim = {
        let mut state = STATE.lock().await;
        let Some(idx) = state.chats.iter().position(|c| c.id == chat_id) else {
            return false;
        };
        let changed = match &message_id {
            Some(id) => {
                state.chats[idx].mark_read_at(vector_core::compact::encode_message_id(id));
                true
            }
            None => state.chats[idx].set_as_read(),
        };
        if !changed {
            return false;
        }
        state.unread_clear(&chat_id);
        db::chats::SlimChatDB::from_chat(&state.chats[idx], &state.interner)
    };
    let _ = db::chats::save_slim_chat(&slim);
    vector_core::emit_event("unread_counts_stale", &json!({ "chats": [chat_id] }));
    true
}

/// Unread counts per chat, from the database: boot holds only each chat's last message.
pub async fn unread_counts() -> HashMap<String, u32> {
    let counts = db::events::unread_counts().await.unwrap_or_default();
    let state = STATE.lock().await;
    state
        .chats
        .iter()
        .filter_map(|c| {
            let n = counts.get(&c.id).copied().unwrap_or(0);
            (n > 0).then(|| (c.id.clone(), n as u32))
        })
        .collect()
}

/// A chat as state holds it, for one the UI hasn't seen yet.
pub async fn chat_snapshot(chat_id: String) -> Option<vector_core::SerializableChat> {
    let state = STATE.lock().await;
    state
        .get_chat(&chat_id)
        .map(|c| c.to_serializable(&state.interner))
}

/// A file DM's settings: the chat's self-destruct lifespan rides along and is stamped once
/// the upload is done, so the clock starts at publish.
fn file_send_config(receiver: &str) -> SendConfig {
    SendConfig {
        self_destruct_secs: vector_core::self_destruct::chat_duration_secs(receiver),
        ..SendConfig::gui()
    }
}

const IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "tiff", "tif", "ico"];

/// Send a picked file to a DM. An image is compressed (reusing `precompressed`, the preview's
/// own run, when it fits the choice) or stripped of metadata; anything else streams from disk.
pub async fn send_file(
    receiver: String,
    replied_to: String,
    path: std::path::PathBuf,
    name_override: String,
    compress: bool,
    keep_metadata: bool,
    precompressed: Option<crate::message::types::CachedCompressedImage>,
) -> Result<(), String> {
    if !receiver.starts_with("npub1") {
        return Err("Files can't be sent to a Community channel from this client yet".into());
    }
    let reply = (!replied_to.is_empty()).then_some(replied_to.as_str());
    let config = file_send_config(&receiver);
    let extension = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    let mut name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string();
    if !name_override.is_empty() {
        let clean = vector_core::crypto::sanitize_filename(&name_override);
        if !clean.is_empty() {
            name = clean;
        }
    }
    if !IMAGE_EXTS.contains(&extension.as_str()) {
        match std::fs::metadata(&path) {
            Ok(m) if m.len() > 0 => {}
            Ok(_) => return Err("File is empty (0 bytes)".into()),
            Err(e) => return Err(format!("Failed to read file metadata: {e}")),
        }
        let source = vector_core::sending::FileSource::Path(path);
        return vector_core::sending::send_file_dm_from(
            &receiver,
            source,
            &name,
            &extension,
            None,
            None,
            reply,
            &config,
            Arc::new(Callback),
        )
        .await
        .map(|_| ());
    }
    let (ext, file) = (extension.clone(), path.clone());
    let processed = tokio::task::spawn_blocking(move || {
        let bytes =
            Arc::new(std::fs::read(&file).map_err(|e| format!("Couldn't read the file: {e}"))?);
        let out = crate::message::compression::process_image_for_send(
            bytes,
            &ext,
            compress,
            keep_metadata,
            precompressed,
        );
        crate::message::compression::forget_fitted();
        out
    })
    .await
    .map_err(|e| e.to_string())??;
    // A compressed file's name follows its new format, so the receiver opens it as one.
    if processed.extension != extension {
        name = crate::message::video_compression::renamed(&name, &processed.extension);
    }
    vector_core::sending::send_file_dm_with_meta(
        &receiver,
        processed.bytes,
        &name,
        &processed.extension,
        processed.img_meta,
        None,
        reply,
        &config,
        Arc::new(Callback),
    )
    .await
    .map(|_| ())
}

/// The preview's background compression of a picked image.
pub fn precompress(
    path: &std::path::Path,
) -> Result<crate::message::types::CachedCompressedImage, String> {
    crate::message::precompress(path)
}

/// Whether a picked image carries strip-worthy metadata (location, camera, date).
pub fn has_metadata(path: &std::path::Path) -> bool {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    if matches!(ext.as_str(), "jpg" | "jpeg" | "png") {
        return std::fs::File::open(path)
            .ok()
            .and_then(|mut f| crate::shared::image::header_has_metadata(&mut f, &ext))
            .unwrap_or(false);
    }
    std::fs::read(path).is_ok_and(|b| crate::shared::image::image_bytes_have_metadata(&b, &ext))
}

/// `before` messages up to and including `anchor`, then `after` past it, oldest first.
pub async fn messages_around(
    chat_id: String,
    anchor: String,
    before: usize,
    after: usize,
) -> Result<Vec<Message>, String> {
    let id = db::id_cache::get_chat_id_by_identifier(&chat_id)?;
    db::events::get_messages_around(id, &anchor, before, after).await
}

/// The stopped recording, encoded once and sent as a nameless `.wav`.
pub async fn send_voice(receiver: String, replied_to: String) -> Result<(), String> {
    if !receiver.starts_with("npub1") {
        return Err("Voice messages can't be sent to a Community channel from this client yet".into());
    }
    let pending = vector_media::voice::AudioRecorder::global()
        .take_pending()
        .ok_or("No pending recording")?;
    let _ = vector_media::audio_engine::AudioEngine::get().map(|e| e.stop(pending.source_id));
    let wav = tokio::task::spawn_blocking(move || pending.encode_wav())
        .await
        .map_err(|e| e.to_string())??;
    let reply = (!replied_to.is_empty()).then_some(replied_to.as_str());
    let config = file_send_config(&receiver);
    vector_core::sending::send_file_dm_with_meta(
        &receiver,
        Arc::new(wav),
        "",
        "wav",
        None,
        None,
        reply,
        &config,
        Arc::new(Callback),
    )
    .await
    .map(|_| ())
}

/// The open chat's bot commands, as the `/` picker lists them.
pub async fn chat_commands(chat: String) -> crate::commands::Snapshot {
    let snap = vector_core::VectorCore.get_chat_commands(&chat).await;
    crate::commands::Snapshot {
        bots: snap.bots,
        commands: snap.commands.iter().flat_map(|b| crate::commands::bot_commands(&b.bot, &b.commands)).collect(),
        fresh: snap.fresh,
    }
}
