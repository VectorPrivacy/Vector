//! Reading history, marking read and sending.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use serde_json::json;
use vector_core::sending::{SendCallback, SendConfig};
use vector_core::{db, Message, STATE};

/// Cancel flags for uploads in flight, by pending id.
static UPLOAD_CANCEL: LazyLock<Mutex<HashMap<String, Arc<AtomicBool>>>> = LazyLock::new(Default::default);
/// A failure's reason, from `on_failed_reason` to the `on_failed` that follows it.
static FAIL_REASONS: LazyLock<Mutex<HashMap<String, String>>> = LazyLock::new(Default::default);

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
            UPLOAD_CANCEL.lock().unwrap().insert(msg.id.clone(), Arc::new(AtomicBool::new(false)));
        }
        vector_core::traits::emit_message_new(chat_id, msg);
    }

    fn cancel_token(&self, pending_id: &str) -> Option<Arc<AtomicBool>> {
        UPLOAD_CANCEL.lock().unwrap().get(pending_id).cloned()
    }

    fn on_upload_progress(&self, pending_id: &str, percentage: u8, bytes_sent: u64) -> Result<(), String> {
        if self.cancel_token(pending_id).is_some_and(|f| f.load(Ordering::Relaxed)) {
            return Err("Upload cancelled".into());
        }
        vector_core::emit_event("attachment_upload_progress", &json!({ "id": pending_id, "progress": percentage, "bytesSent": bytes_sent }));
        Ok(())
    }

    fn on_upload_stage(&self, pending_id: &str, stage: &str, pct: Option<u8>) {
        vector_core::emit_event("attachment_upload_stage", &json!({ "id": pending_id, "stage": stage, "progress": pct }));
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
        FAIL_REASONS.lock().unwrap().insert(old_id.to_string(), reason.to_string());
    }

    fn on_failed(&self, chat_id: &str, old_id: &str, msg: &Message) {
        UPLOAD_CANCEL.lock().unwrap().remove(old_id);
        let reason = FAIL_REASONS.lock().unwrap().remove(old_id);
        vector_core::emit_event("message_update", &json!({ "old_id": old_id, "message": msg, "chat_id": chat_id, "reason": reason }));
    }

    fn on_persist(&self, chat_id: &str, msg: &Message) {
        let (chat_id, msg) = (chat_id.to_string(), msg.clone());
        db::spawn_bound(async move {
            let _ = db::events::save_message(&chat_id, &msg).await;
        });
    }
}

/// Send `content` to a DM; `replied_to` quotes a message when not empty.
pub async fn send(receiver: String, content: String, replied_to: String) -> Result<(), String> {
    if !receiver.starts_with("npub1") {
        return Err("Communities can't be written to from this client yet".into());
    }
    let config = SendConfig { expiration: vector_core::self_destruct::resolve_send_expiry(&receiver), ..SendConfig::gui() };
    let reply = (!replied_to.is_empty()).then_some(replied_to.as_str());
    vector_core::sending::send_dm(&receiver, &content, reply, &config, Arc::new(Callback)).await.map(|_| ())
}

/// A page of a chat's messages, newest first, as the timeline renders them.
pub async fn message_views(chat_id: String, limit: usize, offset: usize) -> Result<Vec<Message>, String> {
    let id = db::id_cache::get_chat_id_by_identifier(&chat_id)?;
    db::events::get_message_views(id, limit, offset).await
}

pub fn message_count(chat_id: &str) -> usize {
    db::id_cache::get_chat_id_by_identifier(chat_id).and_then(db::events::get_chat_message_count).unwrap_or(0)
}

/// The chat the user has open: what arrives there is read on arrival.
pub fn set_active_chat(chat_id: Option<String>) {
    vector_core::state::set_active_chat(chat_id);
}

pub async fn mark_as_read(chat_id: String, message_id: Option<String>) -> bool {
    let slim = {
        let mut state = STATE.lock().await;
        let Some(idx) = state.chats.iter().position(|c| c.id == chat_id) else { return false };
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
    state.get_chat(&chat_id).map(|c| c.to_serializable(&state.interner))
}
