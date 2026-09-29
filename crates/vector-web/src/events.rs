//! Live inbound events → the page. Core persists; these hooks paint.

use serde_json::json;
use vector_core::{db, InboundEventHandler, Message, VectorCore, STATE};

pub struct WebEventHandler;

impl InboundEventHandler for WebEventHandler {
    fn on_community_invite(&self, community_id: &str) {
        vector_core::emit_event("community_invite_received", &json!({ "community_id": community_id }));
    }

    fn on_community_message(&self, chat_id: &str, msg: &Message, _is_new: bool) {
        vector_core::traits::emit_message_new(chat_id, msg);
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
