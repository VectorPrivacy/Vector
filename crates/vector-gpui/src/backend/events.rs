//! Live inbound events. Core persists them; these hooks keep read state and tell the UI.

use serde_json::json;
use vector_core::{InboundEventHandler, Message, STATE, db};

pub struct Handler;

/// The open chat reads what arrives in it.
async fn auto_mark_if_active(chat_id: &str, msg_id: &str) -> bool {
    if vector_core::state::get_active_chat().as_deref() != Some(chat_id) {
        return false;
    }
    let slim = {
        let mut state = STATE.lock().await;
        let Some(chat) = state.chats.iter_mut().find(|c| c.id == chat_id) else {
            return false;
        };
        // A "Mark as Unread" ends only by the user's own read, never a background one.
        if chat.unread_from != [0u8; 32] {
            return false;
        }
        chat.mark_read_at(vector_core::compact::encode_message_id(msg_id));
        state
            .get_chat(chat_id)
            .map(|c| db::chats::SlimChatDB::from_chat(c, &state.interner))
    };
    let Some(slim) = slim else { return false };
    let _ = db::chats::save_slim_chat(&slim);
    vector_core::emit_event(
        "chat_mark_read",
        &json!({ "chat_id": chat_id, "last_read": msg_id }),
    );
    true
}

async fn refresh_unread(chat_id: &str, marked: bool) {
    if marked {
        STATE.lock().await.unread_clear(chat_id);
    } else {
        let count = db::events::unread_count_for_chat(chat_id)
            .await
            .unwrap_or(0);
        let mut state = STATE.lock().await;
        if state.unread_seeded {
            state.unread_set(chat_id, count);
        }
    }
    vector_core::emit_event("unread_counts_stale", &json!({ "chats": [chat_id] }));
}

impl InboundEventHandler for Handler {
    fn on_dm_received(&self, chat_id: &str, msg: &Message, is_new: bool) {
        if !is_new {
            return;
        }
        let (chat_id, msg) = (chat_id.to_string(), msg.clone());
        db::spawn_bound(async move {
            if msg.mine {
                // Answered from another device: read here too.
                if super::messaging::mark_as_read(chat_id.clone(), None).await {
                    let last_read = STATE
                        .lock()
                        .await
                        .get_chat(&chat_id)
                        .map(|c| vector_core::compact::decode_message_id(&c.last_read));
                    if let Some(last_read) = last_read {
                        vector_core::emit_event(
                            "chat_mark_read",
                            &json!({ "chat_id": chat_id, "last_read": last_read }),
                        );
                    }
                }
                return;
            }
            let marked = auto_mark_if_active(&chat_id, &msg.id).await;
            refresh_unread(&chat_id, marked).await;
        });
    }

    fn on_file_received(&self, chat_id: &str, msg: &Message, is_new: bool) {
        self.on_dm_received(chat_id, msg, is_new);
    }

    fn on_community_invite(&self, community_id: &str) {
        vector_core::emit_event(
            "community_invite_received",
            &json!({ "community_id": community_id }),
        );
    }

    fn on_community_message(&self, chat_id: &str, msg: &Message, _is_new: bool) {
        vector_core::traits::emit_message_new(chat_id, msg);
        let (chat_id, msg) = (chat_id.to_string(), msg.clone());
        db::spawn_bound(async move {
            let marked = auto_mark_if_active(&chat_id, &msg.id).await;
            refresh_unread(&chat_id, marked).await;
        });
    }

    fn on_community_update(&self, chat_id: &str, target_id: &str, msg: &Message) {
        vector_core::traits::emit_message_replaced(chat_id, target_id, msg);
    }

    fn on_community_removed(&self, chat_id: &str, target_id: &str) {
        vector_core::emit_event(
            "message_removed",
            &json!({ "id": target_id, "chat_id": chat_id, "reason": "deleted" }),
        );
    }

    fn on_community_typing(&self, chat_id: &str, npub: &str, until: u64) {
        let (chat_id, npub) = (chat_id.to_string(), npub.to_string());
        db::spawn_bound(async move {
            let typers = STATE
                .lock()
                .await
                .update_typing_and_get_active(&chat_id, &npub, until);
            vector_core::emit_event(
                "typing-update",
                &json!({ "conversation_id": chat_id, "typers": typers }),
            );
        });
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
        let invited_by = invited_by.map(str::to_string);
        let invited_label = invited_label.map(str::to_string);
        db::spawn_bound(async move {
            presence(&chat_id, &npub, joined, &event_id, created_at, invited_by.as_deref(), invited_label.as_deref()).await;
        });
    }

    fn on_voice_presence(&self, channel_id: &str) {
        vector_core::emit_event("voice_presence", &json!({ "channel_id": channel_id }));
    }

    fn on_community_webxdc(&self, chat_id: &str, npub: &str, topic_id: &str, node_addr: Option<&str>, event_id: &str, created_at: u64) {
        self.on_webxdc_signal(chat_id, npub, topic_id, node_addr, event_id, created_at);
    }

    fn on_webxdc_signal(&self, contact: &str, npub: &str, topic_id: &str, node_addr: Option<&str>, event_id: &str, created_at: u64) {
        let (chat, npub, topic, event_id) = (contact.to_string(), npub.to_string(), topic_id.to_string(), event_id.to_string());
        let addr = node_addr.map(str::to_string);
        db::spawn_bound(async move {
            crate::miniapp::on_peer_signal(&chat, &npub, &topic, addr.as_deref(), &event_id, created_at).await;
        });
    }

    fn on_community_dissolved(&self, community_id: &str) {
        vector_core::emit_event(
            "community_refreshed",
            &json!({ "community_id": community_id }),
        );
    }

    fn on_subscription_ready(&self, _communities: usize) {
        super::community::request();
    }

    fn on_community_refreshed(&self, community_id: &str) {
        let session = db::current_session();
        let community_id = community_id.to_string();
        db::spawn_bound(async move {
            let id = vector_core::community::CommunityId(vector_core::simd::hex::hex_to_bytes_32(
                &community_id,
            ));
            if let Ok(Some(c)) = db::community::load_community_v2(&id) {
                vector_core::VectorCore
                    .register_v2_chats(&c, &session)
                    .await;
            }
            vector_core::emit_event(
                "community_refreshed",
                &json!({ "community_id": community_id }),
            );
        });
    }

    fn on_community_self_removed(&self, community_id: &str) {
        vector_core::emit_event("community_kicked", &json!({ "community_id": community_id }));
    }
}

/// A Community join or leave, saved as its system line and shown. The save dedups by event id,
/// so relay replays and our own echo stay silent.
async fn presence(
    channel_id: &str,
    npub: &str,
    joined: bool,
    event_id: &str,
    created_at: u64,
    invited_by: Option<&str>,
    invited_label: Option<&str>,
) {
    use vector_core::stored_event::SystemEventType;
    let et = if joined { SystemEventType::MemberJoined } else { SystemEventType::MemberLeft };
    let note = invited_by.map(|by| match invited_label {
        Some(l) if !l.is_empty() => format!("{by}|{l}"),
        _ => by.to_string(),
    });
    let inserted = db::events::save_system_event_at(event_id, channel_id, et, npub, note.as_deref(), created_at, invited_by, invited_label)
        .await
        .unwrap_or(false);
    if !inserted {
        return;
    }
    let nameless = STATE.lock().await.get_profile(npub).is_none_or(|p| {
        p.nickname().is_empty() && p.display_name().is_empty() && p.name().is_empty()
    });
    if nameless {
        vector_core::profile::sync::queue_profile_sync(npub.to_string(), vector_core::profile::sync::SyncPriority::High, false);
    }
    vector_core::emit_event(
        "system_event",
        &json!({
            "conversation_id": channel_id,
            "event_id": event_id,
            "event_type": et.as_u8(),
            "member_pubkey": npub,
            "created_at_ms": created_at.saturating_mul(1000),
        }),
    );
}
