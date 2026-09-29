//! Opening chats, reading history, sending.

use std::sync::Arc;

use serde_json::{json, Value};
use vector_core::db;
use vector_core::sending::{SendCallback, SendConfig};
use vector_core::{Message, STATE};

use crate::emitter;

/// Mirrors the desktop callback: optimistic bubble, then its sent/failed update.
struct WebSendCallback;

impl SendCallback for WebSendCallback {
    fn on_pending(&self, chat_id: &str, msg: &Message) {
        emitter::emit("message_new", &json!({ "message": msg, "chat_id": chat_id }));
    }

    fn on_sent(&self, chat_id: &str, old_id: &str, msg: &Message) {
        if old_id.starts_with("pending-") && old_id != msg.id {
            let pending_id = old_id.to_string();
            db::spawn_bound(async move {
                let _ = db::events::delete_event(&pending_id).await;
            });
        }
        emitter::emit("message_update", &json!({ "old_id": old_id, "message": msg, "chat_id": chat_id }));
    }

    fn on_failed(&self, chat_id: &str, old_id: &str, msg: &Message) {
        emitter::emit("message_update", &json!({ "old_id": old_id, "message": msg, "chat_id": chat_id }));
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
                    "at": event.created_at,
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
