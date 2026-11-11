//! Attachment downloads: fetched, opened and held in memory; one too large to hold, or any the
//! account keeps, goes to the media store sealed. Events match the Tauri client's.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::Instant;

use serde_json::json;
use vector_core::{DownloadError, DownloadStep, STATE, VectorCore, db};

/// Running downloads by id, with whether each was asked to stop.
static ACTIVE: LazyLock<Mutex<HashMap<String, bool>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// One download per attachment id; its stop retires with it.
struct Active(String);

impl Active {
    fn claim(id: &str) -> Option<Self> {
        let id = id.to_ascii_lowercase();
        let mut active = ACTIVE.lock().unwrap();
        if active.contains_key(&id) {
            return None;
        }
        active.insert(id.clone(), false);
        Some(Active(id))
    }

    fn cancelled(&self) -> bool {
        ACTIVE.lock().unwrap().get(&self.0).copied().unwrap_or(false)
    }
}

impl Drop for Active {
    fn drop(&mut self) {
        ACTIVE.lock().unwrap().remove(&self.0);
    }
}

/// Stop a download at its next chunk. True when one was running, so its outcome event is owed.
pub fn cancel(attachment_id: &str) -> bool {
    match ACTIVE.lock().unwrap().get_mut(&attachment_id.to_ascii_lowercase()) {
        Some(stop) => {
            *stop = true;
            true
        }
        None => false,
    }
}

fn emit(event: &str, payload: serde_json::Value) {
    vector_core::emit_event(event, &payload);
}

fn fail(chat: &str, msg_id: &str, attachment_id: &str, reason: &str) -> bool {
    emit("attachment_download_result", json!({ "profile_id": chat, "msg_id": msg_id, "id": attachment_id, "success": false, "result": reason }));
    false
}

pub async fn download(chat: String, msg_id: String, attachment_id: String, auto: bool) -> bool {
    let Some((chat_id, mut message)) = VectorCore.get_message(&msg_id).await else {
        return fail(&chat, &msg_id, &attachment_id, "Message not found");
    };
    let Some(idx) = message.attachments.iter().position(|a| a.id == attachment_id) else {
        return fail(&chat, &msg_id, &attachment_id, "Attachment not found");
    };
    let attachment = message.attachments[idx].clone();
    if let Some(held) = vector_core::simd::hex::hex_to_bytes_32_checked(&attachment.id).filter(vector_core::media::resident) {
        emit(
            "attachment_download_result",
            json!({ "profile_id": chat, "msg_id": msg_id, "id": attachment_id, "success": true, "resident": true, "result": vector_core::media::MediaRef::held(&held) }),
        );
        return true;
    }
    let author = if message.mine { None } else { message.npub.clone().or_else(|| Some(chat_id.clone())) };
    // Too large to hold: kept, so only once asked for, unless the account keeps files.
    if auto && !vector_core::media::ram::cache().admits(attachment.size) && !vector_core::media::keep_media() {
        emit(
            "attachment_download_result",
            json!({ "profile_id": chat, "msg_id": msg_id, "id": attachment_id, "success": false, "cancelled": true, "result": "Too large to hold without keeping it" }),
        );
        return false;
    }
    let Some(active) = Active::claim(&attachment_id) else {
        return fail(&chat, &msg_id, &attachment_id, "This file is already downloading");
    };

    emit("attachment_download_progress", json!({ "id": attachment_id, "progress": -1, "bytesDownloaded": 0 }));
    let stage = |stage: &str| emit("attachment_download_stage", json!({ "id": attachment_id, "stage": stage, "progress": null }));
    // On each whole percent, with the average rate so far. A source that declares no length is
    // measured against the size the message declared.
    let started = Instant::now();
    let declared = (attachment.size > 0).then_some(attachment.size);
    let mut last_pct: i64 = -1;
    let mut last_bytes = 0u64;
    let report = |step: DownloadStep| {
        match step {
            DownloadStep::Received { bytes, total } => {
                if bytes == 0 {
                    last_pct = -1;
                    last_bytes = 0;
                }
                let secs = started.elapsed().as_secs_f64();
                let rate = (secs > 0.0 && bytes > 0).then(|| bytes as f64 / secs);
                match total.or(declared).filter(|&t| t > 0) {
                    Some(t) => {
                        let pct = (bytes.saturating_mul(100) / t).min(100) as i64;
                        if pct > last_pct {
                            last_pct = pct;
                            emit("attachment_download_progress", json!({ "id": attachment_id, "progress": pct, "bytesDownloaded": bytes, "bytesPerSec": rate }));
                        }
                    }
                    None if bytes - last_bytes >= 256 * 1024 => {
                        last_bytes = bytes;
                        emit("attachment_download_progress", json!({ "id": attachment_id, "progress": -1, "bytesDownloaded": bytes, "bytesPerSec": rate }));
                    }
                    None => {}
                }
            }
            DownloadStep::Opening => stage(if attachment.key.is_empty() { "verifying" } else { "decrypting" }),
        }
        !active.cancelled()
    };
    let bytes = match VectorCore.download_attachment_reporting(&attachment, author.as_deref(), 1024 * 1024 * 1024, report).await {
        Ok(b) => b,
        Err(DownloadError::Cancelled) => {
            emit(
                "attachment_download_result",
                json!({ "profile_id": chat, "msg_id": msg_id, "id": attachment_id, "success": false, "cancelled": true, "result": DownloadError::Cancelled.to_string() }),
            );
            return false;
        }
        Err(e) => return fail(&chat, &msg_id, &attachment_id, &e.to_string()),
    };
    stage("saving");
    let hash = vector_core::crypto::sha256_hex(&bytes);
    let hash32 = vector_core::simd::hex::hex_to_bytes_32(&hash);
    let mime = vector_core::crypto::mime_from_extension(&attachment.extension.to_ascii_lowercase());
    let ram = vector_core::media::ram::cache();
    let in_memory = ram.admits(bytes.len() as u64);
    let keep = !in_memory || vector_core::media::keep_media();
    let bytes = if keep {
        let kept = tokio::task::spawn_blocking(move || {
            let r = vector_core::media::store::keep_sealed(&bytes, hash32, mime, vector_core::db::media_store::Origin::Sealed);
            (bytes, r)
        })
        .await;
        match kept {
            Ok((bytes, Ok(()))) => bytes,
            Ok((_, Err(e))) => return fail(&chat, &msg_id, &attachment_id, &e),
            Err(e) => return fail(&chat, &msg_id, &attachment_id, &e.to_string()),
        }
    } else {
        bytes
    };
    if in_memory && ram.insert(hash32, bytes, mime).is_err() && !keep {
        return fail(&chat, &msg_id, &attachment_id, "Too much media is open to show this one");
    }

    {
        let att = &mut message.attachments[idx];
        att.id = hash.clone();
        att.downloaded = keep;
        att.verified = true;
        att.resident = true;
        att.downloading = false;
        att.path = String::new();
    }
    STATE.lock().await.update_attachment(&chat_id, &msg_id, &attachment_id, |att| {
        att.set_id(hash32);
        att.set_downloading(false);
        att.set_verified(true);
        if keep {
            att.set_downloaded(true);
            att.set_path(String::new());
        }
    });

    emit(
        "attachment_download_result",
        json!({ "profile_id": chat, "msg_id": msg_id, "old_id": attachment_id, "id": hash, "success": true, "resident": true, "result": vector_core::media::MediaRef::held(&hash32) }),
    );
    emit("message_update", json!({ "old_id": message.id, "message": message, "chat_id": chat_id }));
    let _ = db::events::save_message(&chat_id, &message).await;
    true
}

/// The bytes behind a downloaded attachment: its held copy, else its file.
pub async fn bytes(hash: Option<[u8; 32]>, path: String) -> Option<Vec<u8>> {
    if let Some(h) = hash
        && let Some((b, _)) = vector_core::media::held_bytes(&h).await
    {
        return Some(b);
    }
    if path.is_empty() {
        return None;
    }
    tokio::fs::read(path).await.ok()
}
