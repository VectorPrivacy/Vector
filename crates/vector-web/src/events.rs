//! Live inbound events → the page. Core persists; these hooks paint.

use serde_json::json;
use vector_core::{db, InboundEventHandler, Message, VectorCore, STATE};

pub struct WebEventHandler;

/// The open chat reads what arrives in it.
async fn auto_mark_if_active(chat_id: &str, msg_id: &str) -> bool {
    if vector_core::state::get_active_chat().as_deref() != Some(chat_id) {
        return false;
    }
    let slim = {
        let mut state = STATE.lock().await;
        let Some(chat) = state.chats.iter_mut().find(|c| c.id == chat_id) else { return false };
        chat.last_read = vector_core::compact::encode_message_id(msg_id);
        state.get_chat(chat_id).map(|c| db::chats::SlimChatDB::from_chat(c, &state.interner))
    };
    let Some(slim) = slim else { return false };
    let _ = db::chats::save_slim_chat(&slim);
    vector_core::emit_event("chat_mark_read", &json!({ "chat_id": chat_id, "last_read": msg_id }));
    true
}

async fn refresh_unread(chat_id: &str, marked: bool) {
    if marked {
        STATE.lock().await.unread_clear(chat_id);
        return;
    }
    let count = db::events::unread_count_for_chat(chat_id).await.unwrap_or(0);
    let mut state = STATE.lock().await;
    if state.unread_seeded {
        state.unread_set(chat_id, count);
    }
}

/// Ask the page for a system notification; it shows one only while unfocused.
async fn notify(chat_id: &str, author: Option<&str>, content: &str, community_label: Option<String>) {
    let (rings, name, icon) = {
        let state = STATE.lock().await;
        let rings = state
            .get_chat(chat_id)
            .is_none_or(|c| vector_core::notify::ring_for_chat(c) == vector_core::notify::NotifyLevel::All);
        let profile = author.and_then(|a| state.get_profile(a));
        let name = profile
            .map(|p| {
                if !p.nickname().is_empty() {
                    p.nickname().to_string()
                } else if !p.display_name.is_empty() {
                    p.display_name.to_string()
                } else {
                    p.name.to_string()
                }
            })
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| "New Message".into());
        (rings, name, profile.map(|p| p.avatar_cached.to_string()).filter(|a| a.starts_with('/')))
    };
    if !rings {
        return;
    }
    let private = db::get_sql_setting("notif_content_privacy".into()).ok().flatten().is_some_and(|v| v == "true");
    let body = if private || content.is_empty() { "New message".to_string() } else { content.chars().take(200).collect() };
    let title = match community_label {
        Some(label) => format!("{name} · {label}"),
        None => name,
    };
    vector_core::emit_event("web_notify", &json!({ "chat_id": chat_id, "title": title, "body": body, "icon": icon }));
}

impl InboundEventHandler for WebEventHandler {
    fn on_dm_received(&self, chat_id: &str, msg: &Message, is_new: bool) {
        if !is_new {
            return;
        }
        let (chat_id, msg) = (chat_id.to_string(), msg.clone());
        db::spawn_bound(async move {
            if msg.mine {
                crate::messaging::mark_as_read(chat_id, None).await;
                return;
            }
            let marked = auto_mark_if_active(&chat_id, &msg.id).await;
            refresh_unread(&chat_id, marked).await;
            if !marked && vector_core::community::is_realtime_fresh(msg.at) {
                let body = if msg.attachments.is_empty() { msg.content.clone() } else { "Sent an attachment".into() };
                notify(&chat_id, Some(&chat_id), &body, None).await;
            }
        });
    }

    fn on_file_received(&self, chat_id: &str, msg: &Message, is_new: bool) {
        self.on_dm_received(chat_id, msg, is_new);
    }

    fn on_community_invite(&self, community_id: &str) {
        vector_core::emit_event("community_invite_received", &json!({ "community_id": community_id }));
    }

    fn on_community_message(&self, chat_id: &str, msg: &Message, _is_new: bool) {
        vector_core::traits::emit_message_new(chat_id, msg);
        let (chat_id, msg) = (chat_id.to_string(), msg.clone());
        db::spawn_bound(async move {
            let marked = auto_mark_if_active(&chat_id, &msg.id).await;
            refresh_unread(&chat_id, marked).await;
            if !marked && !msg.mine && vector_core::community::is_realtime_fresh(msg.at) {
                let label = STATE.lock().await.get_chat(&chat_id).map(|c| c.metadata.get_name().unwrap_or_default().to_string());
                notify(&chat_id, msg.npub.as_deref(), &msg.content, label).await;
            }
        });
    }

    fn on_community_update(&self, chat_id: &str, target_id: &str, msg: &Message) {
        vector_core::traits::emit_message_replaced(chat_id, target_id, msg);
    }

    fn on_community_removed(&self, chat_id: &str, target_id: &str) {
        vector_core::emit_event("message_removed", &json!({ "id": target_id, "chat_id": chat_id, "reason": "deleted" }));
    }

    fn on_community_presence(
        &self,
        chat_id: &str,
        npub: &str,
        joined: bool,
        event_id: &str,
        created_at: u64,
        invited_by: Option<&str>,
        invited_label: Option<&str>,
    ) {
        let (chat_id, npub, event_id) = (chat_id.to_string(), npub.to_string(), event_id.to_string());
        let (invited_by, invited_label) = (invited_by.map(str::to_string), invited_label.map(str::to_string));
        db::spawn_bound(async move {
            crate::community::apply_presence(
                &chat_id, &npub, joined, &event_id, created_at, invited_by.as_deref(), invited_label.as_deref(),
            )
            .await;
        });
    }

    fn on_webxdc_signal(&self, contact: &str, npub: &str, topic_id: &str, node_addr: Option<&str>, event_id: &str, created_at: u64) {
        crate::miniapps::on_signal(contact.into(), npub.into(), topic_id.into(), node_addr.map(Into::into), event_id.into(), created_at);
    }

    fn on_community_webxdc(&self, chat_id: &str, npub: &str, topic_id: &str, node_addr: Option<&str>, event_id: &str, created_at: u64) {
        crate::miniapps::on_signal(chat_id.into(), npub.into(), topic_id.into(), node_addr.map(Into::into), event_id.into(), created_at);
    }

    fn on_community_typing(&self, chat_id: &str, npub: &str, until: u64) {
        let (chat_id, npub) = (chat_id.to_string(), npub.to_string());
        db::spawn_bound(async move {
            let typers = STATE.lock().await.update_typing_and_get_active(&chat_id, &npub, until);
            vector_core::emit_event("typing-update", &json!({ "conversation_id": chat_id, "typers": typers }));
        });
    }

    fn on_community_self_removed(&self, community_id: &str) {
        vector_core::emit_event("community_kicked", &json!({ "community_id": community_id }));
    }

    fn on_community_refreshed(&self, community_id: &str) {
        let session = db::current_session();
        let community_id = community_id.to_string();
        db::spawn_bound(async move {
            let id = vector_core::community::CommunityId(vector_core::simd::hex::hex_to_bytes_32(&community_id));
            if let Ok(Some(c)) = db::community::load_community_v2(&id) {
                VectorCore.register_v2_chats(&c, &session).await;
            }
            vector_core::emit_event("community_refreshed", &json!({ "community_id": community_id }));
        });
    }

    fn on_community_dissolved(&self, community_id: &str) {
        vector_core::emit_event("community_refreshed", &json!({ "community_id": community_id }));
    }
}
