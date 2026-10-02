//! Attachments: fetched and decrypted in memory, kept in OPFS, served by the
//! page's service worker at `/vfs/<path>`.

use std::cell::RefCell;
use std::collections::HashSet;

use serde_json::json;
use vector_core::{db, DownloadError, DownloadStep, VectorCore, STATE};

use crate::emitter;

thread_local! {
    static ACTIVE: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
    static CANCELLED: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
}

/// One download per attachment id; its stop, if any, retires with it.
struct Active(String);

impl Active {
    fn claim(id: &str) -> Option<Self> {
        let id = id.to_ascii_lowercase();
        ACTIVE.with(|a| a.borrow_mut().insert(id.clone())).then(|| Active(id))
    }
}

impl Drop for Active {
    fn drop(&mut self) {
        ACTIVE.with(|a| a.borrow_mut().remove(&self.0));
        CANCELLED.with(|c| c.borrow_mut().remove(&self.0));
    }
}

/// Stop a download at its next chunk. True when one was running, so its outcome event is owed.
pub fn cancel_download(attachment_id: &str) -> bool {
    let id = attachment_id.to_ascii_lowercase();
    if !ACTIVE.with(|a| a.borrow().contains(&id)) {
        return false;
    }
    CANCELLED.with(|c| c.borrow_mut().insert(id));
    true
}

fn cancelled(id: &str) -> bool {
    CANCELLED.with(|c| c.borrow().contains(id))
}

fn fail(chat: &str, msg_id: &str, attachment_id: &str, reason: &str) -> bool {
    emitter::emit(
        "attachment_download_result",
        &json!({ "profile_id": chat, "msg_id": msg_id, "id": attachment_id, "success": false, "result": reason }),
    );
    false
}

/// Where a blob lives on the web: content-addressed, so every message
/// carrying the same file shares one copy.
fn blob_path(hash: &str, extension: &str) -> std::path::PathBuf {
    let ext = if extension.is_empty() { "bin" } else { extension };
    db::get_download_dir().join(format!("{hash}.{ext}"))
}

pub async fn download_attachment(chat: String, msg_id: String, attachment_id: String) -> bool {
    let Some((chat_id, mut message)) = VectorCore.get_message(&msg_id).await else {
        return fail(&chat, &msg_id, &attachment_id, "Message not found");
    };
    let Some(idx) = message.attachments.iter().position(|a| a.id == attachment_id) else {
        return fail(&chat, &msg_id, &attachment_id, "Attachment not found");
    };
    let attachment = message.attachments[idx].clone();
    let author = if message.mine { None } else { message.npub.clone().or_else(|| Some(chat_id.clone())) };
    let Some(active) = Active::claim(&attachment_id) else {
        return fail(&chat, &msg_id, &attachment_id, "This file is already downloading");
    };

    emitter::emit(
        "attachment_download_progress",
        &json!({ "id": attachment_id, "progress": -1, "bytesDownloaded": 0 }),
    );
    let stage = |stage: &str| {
        emitter::emit("attachment_download_stage", &json!({ "id": attachment_id, "stage": stage, "progress": null }));
    };
    // As desktop reports it: on each whole percent, with the average rate so far. A source
    // that declares no length is measured against the size the message declared.
    let started = web_time::Instant::now();
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
                            emitter::emit(
                                "attachment_download_progress",
                                &json!({ "id": attachment_id, "progress": pct, "bytesDownloaded": bytes, "bytesPerSec": rate }),
                            );
                        }
                    }
                    None if bytes - last_bytes >= 256 * 1024 => {
                        last_bytes = bytes;
                        emitter::emit(
                            "attachment_download_progress",
                            &json!({ "id": attachment_id, "progress": -1, "bytesDownloaded": bytes, "bytesPerSec": rate }),
                        );
                    }
                    None => {}
                }
            }
            DownloadStep::Opening => stage(if attachment.key.is_empty() { "verifying" } else { "decrypting" }),
        }
        !cancelled(&active.0)
    };
    let bytes = match VectorCore.download_attachment_reporting(&attachment, author.as_deref(), 256 * 1024 * 1024, report).await {
        Ok(b) => b,
        Err(DownloadError::Cancelled) => {
            emitter::emit(
                "attachment_download_result",
                &json!({
                    "profile_id": chat, "msg_id": msg_id, "id": attachment_id,
                    "success": false, "cancelled": true, "result": DownloadError::Cancelled.to_string(),
                }),
            );
            return false;
        }
        Err(e) => return fail(&chat, &msg_id, &attachment_id, &e.to_string()),
    };
    stage("saving");
    let hash = vector_core::crypto::sha256_hex(&bytes);
    let path = blob_path(&hash, &attachment.extension);
    if let Err(e) = vector_core::webfiles::write(&path, &bytes).await {
        return fail(&chat, &msg_id, &attachment_id, &e);
    }
    let path_str = path.to_string_lossy().to_string();

    {
        let att = &mut message.attachments[idx];
        att.id = hash.clone();
        att.downloaded = true;
        att.downloading = false;
        att.path = path_str.clone();
    }
    {
        let mut state = STATE.lock().await;
        state.update_attachment(&chat_id, &msg_id, &attachment_id, |att| {
            let id = vector_core::simd::hex::hex_to_bytes_32(&hash);
            att.id.copy_from_slice(&id);
            att.set_downloading(false);
            att.set_downloaded(true);
            att.path = path_str.clone().into_boxed_str();
        });
    }

    emitter::emit(
        "attachment_download_result",
        &json!({
            "profile_id": chat, "msg_id": msg_id, "old_id": attachment_id,
            "id": hash, "success": true, "result": path_str,
        }),
    );
    emitter::emit("message_update", &json!({ "old_id": message.id, "message": message, "chat_id": chat_id }));

    let _ = db::events::save_message(&chat_id, &message).await;
    let _ = db::attachments::backfill_downloaded_by_hash(&hash, &path_str, &msg_id);
    true
}
