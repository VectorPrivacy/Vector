//! Attachments: fetched and decrypted in memory. What a bubble shows is held there; what is
//! kept goes to the origin's files sealed, and the page shows either as a blob.

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

/// Whether any attachment is downloading.
pub fn downloading() -> bool {
    ACTIVE.with(|a| !a.borrow().is_empty())
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
    let hash32 = vector_core::simd::hex::hex_to_bytes_32(&hash);
    let extension = attachment.extension.to_ascii_lowercase();
    let mime = vector_core::crypto::mime_from_extension(&extension);
    // What a bubble only shows is held in memory; anything else, or anything an account keeps,
    // goes to the store sealed.
    let ram = vector_core::media::ram::cache();
    let in_memory = vector_core::media::previewable(&extension) && ram.admits(bytes.len() as u64);
    let keep = !in_memory || vector_core::media::keep_media();
    if keep {
        if let Err(e) = vector_core::media::store::keep_sealed(&bytes, hash32, mime, vector_core::db::media_store::Origin::Sealed).await {
            return fail(&chat, &msg_id, &attachment_id, &e);
        }
    }
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
    {
        let mut state = STATE.lock().await;
        state.update_attachment(&chat_id, &msg_id, &attachment_id, |att| {
            att.set_id(hash32);
            att.set_downloading(false);
            att.set_verified(true);
            if keep {
                att.set_downloaded(true);
                att.set_path(String::new());
            }
        });
    }

    emitter::emit(
        "attachment_download_result",
        &json!({
            "profile_id": chat, "msg_id": msg_id, "old_id": attachment_id,
            "id": hash, "success": true, "resident": true, "result": vector_core::media::MediaRef::held(&hash32),
        }),
    );
    emitter::emit("message_update", &json!({ "old_id": message.id, "message": message, "chat_id": chat_id }));

    let _ = db::events::save_message(&chat_id, &message).await;
    true
}
