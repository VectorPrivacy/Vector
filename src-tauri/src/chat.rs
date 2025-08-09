use serde::{Deserialize, Serialize};
use crate::Message;
use std::collections::HashMap;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Chat {
    pub id: String,
    pub chat_type: ChatType,
    pub participants: Vec<String>, // List of npubs
    pub messages: Vec<Message>,
    pub last_read: String,
    pub created_at: u64,
    pub metadata: ChatMetadata,
    pub muted: bool,
}

impl Chat {
    pub fn new(id: String, chat_type: ChatType, participants: Vec<String>) -> Self {
        Self {
            id,
            chat_type,
            participants,
            messages: Vec::new(),
            last_read: String::new(),
            created_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            metadata: ChatMetadata::new(),
            muted: false,
        }
    }

    /// Create a new DM chat with another user
    pub fn new_dm(their_npub: String) -> Self {
        Self::new(their_npub.clone(), ChatType::DirectMessage, vec![their_npub])
    }

    /// Get the last message timestamp
    pub fn last_message_time(&self) -> Option<u64> {
        self.messages.last().map(|msg| msg.at)
    }

    /// Get a mutable message by ID
    pub fn get_message_mut(&mut self, id: &str) -> Option<&mut Message> {
        self.messages.iter_mut().find(|msg| msg.id == id)
    }

    /// Set the Last Received message as the "Last Read" message
    pub fn set_as_read(&mut self) -> bool {
        // Ensure we have at least one message received from others
        for msg in self.messages.iter().rev() {
            if !msg.mine {
                // Found the most recent message from others
                self.last_read = msg.id.clone();
                return true;
            }
        }
        
        // No messages from others, can't mark anything as read
        false
    }

    /// Add a Message to this Chat
    /// 
    /// This method internally checks for and avoids duplicate messages.
    pub fn internal_add_message(&mut self, message: Message) -> bool {
        // Make sure we don't add the same message twice
        if self.messages.iter().any(|m| m.id == message.id) {
            // Message is already known by the state
            return false;
        }

        // Fast path for common cases: newest or oldest messages
        if self.messages.is_empty() {
            // First message
            self.messages.push(message);
        } else if message.at >= self.messages.last().unwrap().at {
            // Common case 1: Latest message (append to end)
            self.messages.push(message);
        } else if message.at <= self.messages.first().unwrap().at {
            // Common case 2: Oldest message (insert at beginning)
            self.messages.insert(0, message);
        } else {
            // Less common case: Message belongs somewhere in the middle
            self.messages.insert(
                self.messages.binary_search_by(|m| m.at.cmp(&message.at)).unwrap_or_else(|idx| idx),
                message
            );
        }
        true
    }

    /// Add a Reaction - if it was not already added
    pub fn add_reaction(&mut self, reaction: crate::Reaction, message_id: &str) -> bool {
        // Find the message
        if let Some(msg) = self.get_message_mut(message_id) {
            // Make sure we don't add the same reaction twice
            if !msg.reactions.iter().any(|r| r.id == reaction.id) {
                msg.reactions.push(reaction);
                true
            } else {
                // Reaction was already added previously
                false
            }
        } else {
            false
        }
    }

    /// Get other participant for DM chats
    pub fn get_other_participant(&self, my_npub: &str) -> Option<String> {
        match self.chat_type {
            ChatType::DirectMessage => {
                self.participants.iter()
                    .find(|&p| p != my_npub)
                    .cloned()
            }
            // For other chat types, return None
        }
    }

    /// Check if this is a DM with a specific user
    pub fn is_dm_with(&self, npub: &str) -> bool {
        matches!(self.chat_type, ChatType::DirectMessage) && self.participants.contains(&npub.to_string())
    }

    /// Check if user is a participant in this chat
    pub fn has_participant(&self, npub: &str) -> bool {
        self.participants.contains(&npub.to_string())
    }

    // Getter methods for private fields
    pub fn id(&self) -> &String {
        &self.id
    }

    pub fn chat_type(&self) -> &ChatType {
        &self.chat_type
    }

    pub fn participants(&self) -> &Vec<String> {
        &self.participants
    }

    pub fn last_read(&self) -> &String {
        &self.last_read
    }

    pub fn created_at(&self) -> u64 {
        self.created_at
    }

    pub fn metadata(&self) -> &ChatMetadata {
        &self.metadata
    }

    pub fn muted(&self) -> bool {
        self.muted
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum ChatType {
    DirectMessage,
    // Future types can be added here
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct ChatMetadata {
    pub custom_fields: HashMap<String, String>, // For extensibility
}

impl ChatMetadata {
    pub fn new() -> Self {
        Self {
            custom_fields: HashMap::new(),
        }
    }
}

// Database structures for persistence
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SlimChat {
    pub id: String,
    pub chat_type: ChatType,
    pub participants: Vec<String>,
    pub last_read: String,
    pub created_at: u64,
    pub metadata: ChatMetadata,
    pub muted: bool,
}

impl From<&Chat> for SlimChat {
    fn from(chat: &Chat) -> Self {
        SlimChat {
            id: chat.id.clone(),
            chat_type: chat.chat_type.clone(),
            participants: chat.participants.clone(),
            last_read: chat.last_read.clone(),
            created_at: chat.created_at,
            metadata: chat.metadata.clone(),
            muted: chat.muted,
        }
    }
}

//// Marks a specific message as read for a chat.
/// Behavior:
///  - If message_id is Some(id): set chat.last_read = id.
///  - Else: call chat.set_as_read() to pick the last non-mine message.
///  - Persist the chat (outside the STATE lock) and update unread counter on success.
#[tauri::command]
pub async fn mark_as_read(chat_id: String, message_id: Option<String>) -> bool {
    // Apply the read change regardless of window focus; frontend intent is authoritative
    let handle = crate::TAURI_APP.get().unwrap();

    // Apply the read change to the specified chat
    let (result, chat_id_for_save) = {
        let mut state = crate::STATE.lock().await;
        let mut result = false;
        let mut chat_id_for_save: Option<String> = None;

        if let Some(chat) = state.chats.iter_mut().find(|c| c.id == chat_id) {
            if let Some(msg_id) = &message_id {
                // Explicit message -> set that as last_read
                chat.last_read = msg_id.clone();
                result = true;
                chat_id_for_save = Some(chat.id.clone());
            } else {
                // No explicit message -> fall back to set_as_read behaviour
                result = chat.set_as_read();
                if result {
                    chat_id_for_save = Some(chat.id.clone());
                }
            }
        }

        (result, chat_id_for_save)
    };

    // Update the unread counter and save to DB if the marking was successful
    if result {
        // Update the badge count
        crate::update_unread_counter(handle.clone()).await;

        // Save the updated chat to the DB
        if let Some(chat_id) = chat_id_for_save {
            // Get the updated chat to save its metadata (including last_read)
            let updated_chat = {
                let state = crate::STATE.lock().await;
                state.get_chat(&chat_id).cloned()
            };

            // Save to DB
            if let Some(chat) = updated_chat {
                let _ = crate::db_migration::save_chat(handle.clone(), &chat).await;
            }
        }
    }

    result
}