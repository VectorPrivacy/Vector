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

impl InboundEventHandler for AgentEventHandler {
    fn on_dm_received(&self, chat_id: &str, msg: &Message, _is_new: bool) {
        let entry = BufferedMessage {
            chat_id: chat_id.to_string(),
            is_group: false,
            message: msg.clone(),
        };
        let buf = self.buffer.clone();
        tokio::spawn(async move {
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
        tokio::spawn(async move {
            buf.lock().await.push(entry);
        });
    }
}
