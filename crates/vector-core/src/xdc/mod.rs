//! Mini App (WebXDC) realtime sessions for headless clients: a bot can join
//! the game a player shared, speak its protocol, and leave.
//!
//! A Mini App message carries a `webxdc-topic`, minted once at send time. Every
//! participant who opens it joins that topic on an Iroh gossip mesh
//! ([`mesh`]), and announces its node into the chat over Nostr ([`signal`]) so
//! the others can dial it. [`session::join`] does both for a headless peer;
//! what flows on the channel is the app's own protocol, opaque here.
//!
//! The realtime channel is ephemeral and best-effort, reaching only peers
//! online at the time.

pub mod mesh;
pub mod package;
pub mod session;
pub mod signal;
pub mod wire;

pub use session::{allow_outside_tor, allow_outside_transport, is_joined, join, join_with, JoinOptions, XdcEvent, XdcFrame, XdcPeer, XdcSender, XdcSession};

use crate::types::Attachment;

/// A Mini App message, as found by its realtime topic.
#[derive(Debug, Clone)]
pub struct AppMessage {
    /// The DM npub or Community channel id it was shared in.
    pub chat_id: String,
    pub message_id: String,
    pub attachment: Attachment,
}

/// The Mini App message in `chat_id` whose session runs on `topic`. Live state
/// first: a headless client's own sends live there, not in its database.
pub async fn find_by_topic_in(chat_id: &str, topic: &str) -> Option<AppMessage> {
    {
        let state = crate::state::STATE.lock().await;
        if let Some(chat) = state.chats.iter().find(|c| c.id == chat_id) {
            let hit = chat.messages.iter().find(|m| m.attachments.iter().any(|a| a.webxdc_topic.as_deref() == Some(topic)));
            if let Some(compact) = hit {
                let msg = compact.to_message(&state.interner);
                let attachment = msg.attachments.into_iter().find(|a| a.webxdc_topic.as_deref() == Some(topic))?;
                return Some(AppMessage { chat_id: chat_id.to_string(), message_id: msg.id, attachment });
            }
        }
    }
    let (message_id, attachment) = crate::db::attachments::find_by_webxdc_topic_in(chat_id, topic).ok().flatten()?;
    Some(AppMessage { chat_id: chat_id.to_string(), message_id, attachment })
}

/// Take in a peer signal from an inbound hook (`on_webxdc_signal` for a DM,
/// `on_community_webxdc` for a channel): persist it, and feed it to the session
/// on its topic if this account is in one. Returns the signal, or `None` for garbage.
pub async fn on_signal(
    chat_id: &str,
    npub: &str,
    topic: &str,
    node_addr: Option<&str>,
    event_id: &str,
    created_at: u64,
) -> Option<signal::Signal> {
    let sig = signal::ingest(chat_id, npub, topic, node_addr, event_id, created_at).await?;
    session::apply_signal(&sig).await;
    Some(sig)
}

/// Whether an attachment is a Mini App.
pub fn is_app(att: &Attachment) -> bool {
    att.extension.eq_ignore_ascii_case("xdc")
}
