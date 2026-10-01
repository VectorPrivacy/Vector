//! Attachments: fetched and decrypted in memory, kept in OPFS, served by the
//! page's service worker at `/vfs/<path>`.

use serde_json::json;
use vector_core::{db, VectorCore, STATE};

use crate::emitter;

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

    emitter::emit(
        "attachment_download_progress",
        &json!({ "id": attachment_id, "progress": -1, "bytesDownloaded": 0 }),
    );
    let bytes = match VectorCore.download_attachment_from(&attachment, author.as_deref()).await {
        Ok(b) => b,
        Err(e) => return fail(&chat, &msg_id, &attachment_id, &e.to_string()),
    };
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
