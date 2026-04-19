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

    fn on_group_message(&self, group_id: &str, msg: &Message) {
        let entry = BufferedMessage {
            chat_id: group_id.to_string(),
            is_group: true,
            message: msg.clone(),
        };
        let buf = self.buffer.clone();
        tokio::spawn(async move {
            buf.lock().await.push(entry);
        });
    }
}
