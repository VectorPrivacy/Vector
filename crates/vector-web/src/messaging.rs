//! Opening chats, reading history, sending.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde_json::{json, Value};
use vector_core::db;
use vector_core::sending::{SendCallback, SendConfig};
use vector_core::{Message, STATE};

use crate::emitter;

thread_local! {
    /// Cancel flags for uploads in flight, by pending id.
    static UPLOAD_CANCEL: std::cell::RefCell<std::collections::HashMap<String, Arc<AtomicBool>>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
    /// A failure's reason, from `on_failed_reason` to the `on_failed` that follows it at once.
    static FAIL_REASONS: std::cell::RefCell<std::collections::HashMap<String, String>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

pub fn cancel_upload(pending_id: &str) {
    UPLOAD_CANCEL.with(|m| {
        if let Some(flag) = m.borrow().get(pending_id) {
            flag.store(true, Ordering::Relaxed);
        }
    });
}

/// Mirrors the desktop callback: optimistic bubble, upload progress, then its sent/failed update.
#[derive(Clone, Copy)]
pub struct WebSendCallback;

impl SendCallback for WebSendCallback {
    fn on_pending(&self, chat_id: &str, msg: &Message) {
        if !msg.attachments.is_empty() {
            UPLOAD_CANCEL.with(|m| m.borrow_mut().insert(msg.id.clone(), Arc::new(AtomicBool::new(false))));
        }
        emitter::emit("message_new", &json!({ "message": msg, "chat_id": chat_id }));
    }

    fn cancel_token(&self, pending_id: &str) -> Option<Arc<AtomicBool>> {
        UPLOAD_CANCEL.with(|m| m.borrow().get(pending_id).cloned())
    }

    fn on_upload_progress(&self, pending_id: &str, percentage: u8, bytes_sent: u64) -> Result<(), String> {
        if self.cancel_token(pending_id).is_some_and(|f| f.load(Ordering::Relaxed)) {
            return Err("Upload cancelled".into());
        }
        emitter::emit("attachment_upload_progress", &json!({ "id": pending_id, "progress": percentage, "bytesSent": bytes_sent }));
        Ok(())
    }

    fn on_upload_stage(&self, pending_id: &str, stage: &str, pct: Option<u8>) {
        emitter::emit("attachment_upload_stage", &json!({ "id": pending_id, "stage": stage, "progress": pct }));
    }

    fn on_upload_complete(&self, chat_id: &str, pending_id: &str, attachment_id: &str, url: &str) {
        emitter::emit(
            "attachment_update",
            &json!({ "chat_id": chat_id, "message_id": pending_id, "attachment_id": attachment_id, "url": url }),
        );
    }

    fn on_sent(&self, chat_id: &str, old_id: &str, msg: &Message) {
        UPLOAD_CANCEL.with(|m| m.borrow_mut().remove(old_id));
        if old_id.starts_with("pending-") && old_id != msg.id {
            let pending_id = old_id.to_string();
            db::spawn_bound(async move {
                let _ = db::events::delete_event(&pending_id).await;
            });
        }
        emitter::emit("message_update", &json!({ "old_id": old_id, "message": msg, "chat_id": chat_id }));
    }

    fn on_failed_reason(&self, _chat_id: &str, old_id: &str, reason: &str) {
        FAIL_REASONS.with(|m| m.borrow_mut().insert(old_id.to_string(), reason.to_string()));
    }

    fn on_failed(&self, chat_id: &str, old_id: &str, msg: &Message) {
        UPLOAD_CANCEL.with(|m| m.borrow_mut().remove(old_id));
        let reason = FAIL_REASONS.with(|m| m.borrow_mut().remove(old_id));
        emitter::emit("message_update", &json!({ "old_id": old_id, "message": msg, "chat_id": chat_id, "reason": reason }));
    }

    fn on_persist(&self, chat_id: &str, msg: &Message) {
        let chat_id = chat_id.to_string();
        let msg = msg.clone();
        db::spawn_bound(async move {
            let _ = db::events::save_message(&chat_id, &msg).await;
        });
    }
}

pub async fn message(receiver: String, content: String, replied_to: String) -> Result<Value, String> {
    if !receiver.starts_with("npub1") {
        return Err("Communities are not available on Vector Web yet".into());
    }
    let config = SendConfig {
        expiration: vector_core::self_destruct::resolve_send_expiry(&receiver),
        ..SendConfig::gui()
    };
    let reply = (!replied_to.is_empty()).then_some(replied_to.as_str());
    let callback: Arc<dyn SendCallback> = Arc::new(WebSendCallback);
    let result = vector_core::sending::send_dm(&receiver, &content, reply, &config, callback).await?;
    Ok(json!({ "pending_id": result.pending_id, "event_id": result.event_id }))
}

pub async fn send_file(
    receiver: String,
    replied_to: String,
    bytes: Arc<Vec<u8>>,
    name: String,
    extension: String,
    img_meta: Option<vector_core::types::ImageMetadata>,
) -> Result<Value, String> {
    let config = SendConfig {
        self_destruct_secs: vector_core::self_destruct::chat_duration_secs(&receiver),
        ..SendConfig::gui()
    };
    let reply = (!replied_to.is_empty()).then_some(replied_to.as_str());
    let result = vector_core::sending::send_file_dm_with_meta(
        &receiver, bytes, &name, &extension, img_meta, None, reply, &config, Arc::new(WebSendCallback),
    )
    .await?;
    Ok(json!({ "pending_id": result.pending_id, "event_id": result.event_id }))
}

pub async fn get_message_views(chat_id: &str, limit: usize, offset: usize) -> Result<Vec<Message>, String> {
    let id = db::id_cache::get_chat_id_by_identifier(chat_id)?;
    db::events::get_message_views(id, limit, offset).await
}

pub fn get_chat_message_count(chat_id: &str) -> Result<usize, String> {
    let id = db::id_cache::get_chat_id_by_identifier(chat_id)?;
    db::events::get_chat_message_count(id)
}

pub fn get_system_events(conversation_id: &str) -> Result<Value, String> {
    let events = db::events::get_system_events_for_chat(conversation_id)?;
    Ok(Value::Array(
        events
            .iter()
            .map(|event| {
                let tag = |name: &str| event.tags.iter().find(|t| t.len() >= 2 && t[0] == name).map(|t| t[1].clone());
                json!({
                    "id": event.id,
                    "event_type": tag("event-type").and_then(|v| v.parse::<u8>().ok()).unwrap_or(255),
                    "content": event.content,
                    "member_npub": tag("member").unwrap_or_default(),
                    "at": event.created_at * 1000,
                })
            })
            .collect(),
    ))
}

pub async fn mark_as_read(chat_id: String, message_id: Option<String>) -> bool {
    let slim = {
        let mut state = STATE.lock().await;
        let Some(idx) = state.chats.iter().position(|c| c.id == chat_id) else { return false };
        let changed = match &message_id {
            Some(id) => {
                state.chats[idx].last_read = vector_core::compact::encode_message_id(id);
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
    true
}

pub async fn unread_total() -> u32 {
    db::events::unread_counts().await.map(|m| m.values().sum()).unwrap_or(0)
}
