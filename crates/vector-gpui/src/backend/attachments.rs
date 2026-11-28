//! Attachment downloads: fetched, opened and held in memory; one too large to hold, or any the
//! account keeps, goes to the media store sealed. Events match the Tauri client's.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::Instant;

use serde_json::json;
use vector_core::{DownloadError, DownloadStep, STATE, VectorCore, db};

/// Running downloads by id, with whether each was asked to stop.
static ACTIVE: LazyLock<Mutex<HashMap<String, bool>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// One download per attachment id; its stop retires with it.
struct Active(String);

impl Active {
    fn claim(id: &str) -> Option<Self> {
        let id = id.to_ascii_lowercase();
        let mut active = ACTIVE.lock().unwrap_or_else(|e| e.into_inner());
        if active.contains_key(&id) {
            return None;
        }
        active.insert(id.clone(), false);
        Some(Active(id))
    }

    fn cancelled(&self) -> bool {
        ACTIVE
            .lock()
            .unwrap()
            .get(&self.0)
            .copied()
            .unwrap_or(false)
    }
}

impl Drop for Active {
    fn drop(&mut self) {
        ACTIVE.lock().unwrap_or_else(|e| e.into_inner()).remove(&self.0);
    }
}

/// Whether any download runs: one that read the old setting could still keep a file.
pub fn any_running() -> bool {
    !ACTIVE.lock().unwrap_or_else(|e| e.into_inner()).is_empty()
}

/// Stop every running download: the account they belong to is leaving.
pub fn cancel_all() {
    ACTIVE
        .lock()
        .unwrap()
        .values_mut()
        .for_each(|stop| *stop = true);
}

/// Stop a download at its next chunk. True when one was running, so its outcome event is owed.
pub fn cancel(attachment_id: &str) -> bool {
    match ACTIVE
        .lock()
        .unwrap()
        .get_mut(&attachment_id.to_ascii_lowercase())
    {
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

/// A file that came without a size, asked of its server; the row learns it as an edit would.
pub async fn probe_size(msg_id: String, attachment_id: String) {
    if VectorCore.probe_attachment_size(&msg_id, &attachment_id).await.is_none() {
        return;
    }
    if let Some((chat_id, message)) = VectorCore.get_message(&msg_id).await {
        emit("message_update", json!({ "old_id": message.id, "message": message, "chat_id": chat_id }));
    }
}

/// What a download reports once every server its message names holds no such file.
pub const GONE_EVERYWHERE: &str = "No longer on any media server"; // i18n-exempt: sentinel, matched and shown translated

/// Whether every source the message itself names answers that it holds no such file. A guessed
/// mirror can rescue a file, never keep it in doubt, so only the named ones are asked.
async fn gone_everywhere(att: &vector_core::Attachment) -> bool {
    let Ok(client) = vector_core::net::build_http_client_with_options(None, Some(vector_core::net::transfer_stall()), true) else {
        return false;
    };
    let mut named = att.all_urls().peekable();
    if named.peek().is_none() {
        return false;
    }
    for url in named {
        if vector_core::net::validate_url_not_private(url).is_err() {
            return false;
        }
        let asked = vector_core::net::proxied_request(&client, reqwest::Method::GET, url).await.send();
        let Ok(Ok(res)) = tokio::time::timeout(std::time::Duration::from_secs(15), asked).await else {
            return false;
        };
        if !not_on_server(res).await {
            return false;
        }
    }
    true
}

/// A 404 or 410, from the server itself or passed on by the privacy proxy from its source.
async fn not_on_server(res: vector_core::net::Resp) -> bool {
    let status = res.status().as_u16();
    if matches!(status, 404 | 410) {
        return true;
    }
    status == 502
        && res.content_length().is_some_and(|n| n <= 4096)
        && res
            .json::<serde_json::Value>()
            .await
            .is_ok_and(|b| matches!(b.get("source_status").and_then(serde_json::Value::as_u64), Some(404 | 410)))
}

fn fail(chat: &str, msg_id: &str, attachment_id: &str, reason: &str) -> bool {
    vector_core::log_warn!("attachment {attachment_id} of {msg_id}: {reason}");
    emit(
        "attachment_download_result",
        json!({ "profile_id": chat, "msg_id": msg_id, "id": attachment_id, "success": false, "result": reason, "partial": partial(attachment_id) }),
    );
    false
}

/// The bytes a stopped download left to resume from: (bytes, total).
fn partial(attachment_id: &str) -> Option<(u64, Option<u64>)> {
    let part = vector_core::resume::part_for(attachment_id).ok()?;
    vector_core::resume::read_checkpoint(&part).map(|c| (c.offset, c.total))
}

/// Every download of this account stopped part-way, by attachment id.
pub fn paused() -> HashMap<String, (u64, Option<u64>)> {
    vector_core::resume::paused().into_iter().map(|(id, c)| (id, (c.offset, c.total))).collect()
}

pub async fn download(chat: String, msg_id: String, attachment_id: String, auto: bool) -> bool {
    let Some((chat_id, mut message)) = VectorCore.get_message(&msg_id).await else {
        return fail(&chat, &msg_id, &attachment_id, "Message not found"); // i18n-exempt: developer error
    };
    let Some(idx) = message
        .attachments
        .iter()
        .position(|a| a.id == attachment_id)
    else {
        return fail(&chat, &msg_id, &attachment_id, "Attachment not found"); // i18n-exempt: developer error
    };
    let attachment = message.attachments[idx].clone();
    if let Some(held) = vector_core::simd::hex::hex_to_bytes_32_checked(&attachment.id)
        .filter(vector_core::media::resident)
    {
        emit(
            "attachment_download_result",
            json!({ "profile_id": chat, "msg_id": msg_id, "id": attachment_id, "success": true, "resident": true, "result": vector_core::media::MediaRef::held(&held) }),
        );
        // Held already (fetched on receipt, or by another message): the row may not know yet.
        if !attachment.resident {
            let att = &mut message.attachments[idx];
            att.resident = true;
            att.verified = true;
            att.downloading = false;
            emit(
                "message_update",
                json!({ "old_id": message.id, "message": message, "chat_id": chat_id }),
            );
        }
        return true;
    }
    let author = if message.mine {
        None
    } else {
        message.npub.clone().or_else(|| Some(chat_id.clone()))
    };
    // Too large to hold: kept, so only once asked for, unless the account keeps files.
    if auto
        && !vector_core::media::ram::cache().admits(attachment.size)
        && !vector_core::media::keep_media()
    {
        emit(
            "attachment_download_result",
            json!({ "profile_id": chat, "msg_id": msg_id, "id": attachment_id, "success": false, "cancelled": true, "result": crate::t::file_too_large_to_hold().to_string() }),
        );
        return false;
    }
    let Some(active) = Active::claim(&attachment_id) else {
        return fail(
            &chat,
            &msg_id,
            &attachment_id,
            &crate::t::file_already_downloading(),
        );
    };

    emit(
        "attachment_download_progress",
        json!({ "id": attachment_id, "progress": -1, "bytesDownloaded": 0 }),
    );
    let stage = |stage: &str| {
        emit(
            "attachment_download_stage",
            json!({ "id": attachment_id, "stage": stage, "progress": null }),
        )
    };
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
                            emit(
                                "attachment_download_progress",
                                json!({ "id": attachment_id, "progress": pct, "bytesDownloaded": bytes, "bytesPerSec": rate }),
                            );
                        }
                    }
                    None if bytes - last_bytes >= 256 * 1024 => {
                        last_bytes = bytes;
                        emit(
                            "attachment_download_progress",
                            json!({ "id": attachment_id, "progress": -1, "bytesDownloaded": bytes, "bytesPerSec": rate }),
                        );
                    }
                    None => {}
                }
            }
            DownloadStep::Opening => stage(if attachment.key.is_empty() {
                "verifying"
            } else {
                "decrypting"
            }),
        }
        !active.cancelled()
    };
    // Files that go to disk anyway fetch through a checkpointed partial, so a stop resumes.
    let resumable = !vector_core::media::ram::cache().admits(attachment.size) || vector_core::media::keep_media();
    let fetch = async {
        if resumable {
            VectorCore.download_attachment_resumable(&attachment, author.as_deref(), report).await
        } else {
            VectorCore
                .download_attachment_reporting(&attachment, author.as_deref(), 1024 * 1024 * 1024, report)
                .await
        }
    };
    // A stalled link (Tor, I2P) may never report a chunk, so a stop can't wait for one.
    let stopped = async {
        while !active.cancelled() {
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        }
    };
    let got = tokio::select! {
        got = fetch => got,
        () = stopped => Err(DownloadError::Cancelled),
    };
    let bytes = match got {
        Ok(b) => b,
        Err(DownloadError::Cancelled) => {
            emit(
                "attachment_download_result",
                json!({ "profile_id": chat, "msg_id": msg_id, "id": attachment_id, "success": false, "cancelled": true, "result": DownloadError::Cancelled.to_string(), "partial": partial(&attachment_id) }),
            );
            return false;
        }
        Err(e @ (DownloadError::Unusable(_) | DownloadError::Unreachable(_))) if gone_everywhere(&attachment).await => {
            vector_core::log_warn!("attachment {attachment_id} of {msg_id}: {e}, and every named server says it is gone");
            emit(
                "attachment_download_result",
                json!({ "profile_id": chat, "msg_id": msg_id, "id": attachment_id, "success": false, "gone": true, "result": GONE_EVERYWHERE, "partial": partial(&attachment_id) }),
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
        let kept = vector_core::db::blocking_bound(move || {
            let r = vector_core::media::store::keep_sealed(
                &bytes,
                hash32,
                mime,
                vector_core::db::media_store::Origin::Sealed,
            );
            (bytes, r)
        })
        .await;
        match kept {
            Ok((bytes, Ok(()))) => bytes,
            Ok((_, Err(e))) => return fail(&chat, &msg_id, &attachment_id, &e),
            Err(e) => return fail(&chat, &msg_id, &attachment_id, &e),
        }
    } else {
        bytes
    };
    if in_memory && ram.insert(hash32, bytes, mime).is_err() && !keep {
        return fail(
            &chat,
            &msg_id,
            &attachment_id,
            &crate::t::file_memory_full(),
        );
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
    STATE
        .lock()
        .await
        .update_attachment(&chat_id, &msg_id, &attachment_id, |att| {
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
    emit(
        "message_update",
        json!({ "old_id": message.id, "message": message, "chat_id": chat_id }),
    );
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
