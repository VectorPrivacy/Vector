use std::sync::Arc;
use tokio::sync::Mutex;
use vector_core::{InboundEventHandler, Message};

#[derive(Clone, Debug, serde::Serialize)]
pub struct BufferedMessage {
    pub chat_id: String,
    pub is_group: bool,
    #[serde(flatten)]
    pub message: Message,
}

pub struct AgentEventHandler {
    buffer: Arc<Mutex<Vec<BufferedMessage>>>,
}

impl AgentEventHandler {
    pub fn new() -> (Self, Arc<Mutex<Vec<BufferedMessage>>>) {
        let buffer = Arc::new(Mutex::new(Vec::new()));
        (Self { buffer: buffer.clone() }, buffer)
    }

    /// Build a handler that writes into an EXISTING buffer — used when re-attaching `listen()`
    /// after an account swap, so the new session's events flow into the same buffer the MCP
    /// `get_new_messages` tool already reads from.
    pub fn with_buffer(buffer: Arc<Mutex<Vec<BufferedMessage>>>) -> Self {
        Self { buffer }
    }
}

/// Feeds Mini App peer signals to the sessions this agent joined.
pub struct XdcSignalSink;

impl InboundEventHandler for XdcSignalSink {
    // Peer signals keep joined Mini App sessions dialled to the players who open them.
    fn on_webxdc_signal(&self, contact: &str, npub: &str, topic: &str, node_addr: Option<&str>, event_id: &str, created_at: u64) {
        let (c, n, t, a, e) = (contact.to_string(), npub.to_string(), topic.to_string(), node_addr.map(str::to_string), event_id.to_string());
        vector_core::db::spawn_bound(async move {
            vector_core::xdc::on_signal(&c, &n, &t, a.as_deref(), &e, created_at).await;
        });
    }

    fn on_community_webxdc(&self, chat_id: &str, npub: &str, topic: &str, node_addr: Option<&str>, event_id: &str, created_at: u64) {
        self.on_webxdc_signal(chat_id, npub, topic, node_addr, event_id, created_at);
    }
}

impl InboundEventHandler for AgentEventHandler {
    fn on_webxdc_signal(&self, contact: &str, npub: &str, topic: &str, node_addr: Option<&str>, event_id: &str, created_at: u64) {
        XdcSignalSink.on_webxdc_signal(contact, npub, topic, node_addr, event_id, created_at);
    }

    fn on_community_webxdc(&self, chat_id: &str, npub: &str, topic: &str, node_addr: Option<&str>, event_id: &str, created_at: u64) {
        XdcSignalSink.on_community_webxdc(chat_id, npub, topic, node_addr, event_id, created_at);
    }

    fn on_dm_received(&self, chat_id: &str, msg: &Message, _is_new: bool) {
        let entry = BufferedMessage {
            chat_id: chat_id.to_string(),
            is_group: false,
            message: msg.clone(),
        };
        let buf = self.buffer.clone();
        vector_core::db::spawn_bound(async move {
            buf.lock().await.push(entry);
        });
    }

    fn on_community_message(&self, chat_id: &str, msg: &Message, is_new: bool) {
        // Live messages only: a relay replaying history sends them with is_new=false.
        if !is_new {
            return;
        }
        let entry = BufferedMessage {
            chat_id: chat_id.to_string(),
            is_group: true,
            message: msg.clone(),
        };
        let buf = self.buffer.clone();
        vector_core::db::spawn_bound(async move {
            buf.lock().await.push(entry);
        });
    }

    fn on_file_received(&self, chat_id: &str, msg: &Message, _is_new: bool) {
        let entry = BufferedMessage {
            chat_id: chat_id.to_string(),
            is_group: false,
            message: msg.clone(),
        };
        let buf = self.buffer.clone();
        vector_core::db::spawn_bound(async move {
            buf.lock().await.push(entry);
        });
    }
}
