//! Chat types and management.
//!
//! This module provides:
//! - `Chat`: Core chat struct with compact message storage
//! - `SerializableChat`: Frontend-friendly format for Tauri communication
//! - `ChatType`, `ChatMetadata`: Supporting types

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::message::compact::{CompactMessage, CompactMessageVec, NpubInterner};
use crate::Message;

// ============================================================================
// Chat (Internal Storage)
// ============================================================================

/// Chat with compact message storage for memory efficiency.
///
/// Messages are stored as `CompactMessage` with binary IDs and interned npubs.
/// Use `to_serializable()` to convert to frontend-friendly format.
#[derive(Clone, Debug)]
pub struct Chat {
    pub id: String,
    pub chat_type: ChatType,
    pub participants: Vec<String>,
    /// Compact message storage - O(log n) lookup by ID
    pub messages: CompactMessageVec,
    pub last_read: String,
    pub created_at: u64,
    pub metadata: ChatMetadata,
    pub muted: bool,
    /// Typing participants (npub -> expires_at), memory-only
    pub typing_participants: HashMap<String, u64>,
}

impl Chat {
    pub fn new(id: String, chat_type: ChatType, participants: Vec<String>) -> Self {
        Self {
            id,
            chat_type,
            participants,
            messages: CompactMessageVec::new(),
            last_read: String::new(),
            created_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            metadata: ChatMetadata::new(),
            muted: false,
            typing_participants: HashMap::new(),
        }
    }

    /// Create a new DM chat
    pub fn new_dm(their_npub: String) -> Self {
        Self::new(their_npub.clone(), ChatType::DirectMessage, vec![their_npub])
    }

    /// Create a new MLS group chat
    pub fn new_mls_group(group_id: String, participants: Vec<String>) -> Self {
        Self::new(group_id, ChatType::MlsGroup, participants)
    }

    // ========================================================================
    // Message Access (Compact)
    // ========================================================================

    /// Number of messages
    #[inline]
    pub fn message_count(&self) -> usize {
        self.messages.len()
    }

    /// Check if chat has no messages
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    /// Get last message timestamp
    #[inline]
    pub fn last_message_time(&self) -> Option<u64> {
        self.messages.last_timestamp()
    }

    /// Check if a message exists by ID - O(log n)
    #[inline]
    pub fn has_message(&self, id: &str) -> bool {
        self.messages.contains_hex_id(id)
    }

    /// Get a compact message by ID - O(log n)
    #[inline]
    pub fn get_compact_message(&self, id: &str) -> Option<&CompactMessage> {
        self.messages.find_by_hex_id(id)
    }

    /// Get a mutable compact message by ID - O(log n)
    #[inline]
    pub fn get_compact_message_mut(&mut self, id: &str) -> Option<&mut CompactMessage> {
        self.messages.find_by_hex_id_mut(id)
    }

    /// Get a message by ID, converting to full Message format - O(log n)
    pub fn get_message(&self, id: &str, interner: &NpubInterner) -> Option<Message> {
        self.messages.find_by_hex_id(id)
            .map(|cm| cm.to_message(interner))
    }

    /// Iterate over compact messages (no conversion, supports .rev())
    #[inline]
    pub fn iter_compact(&self) -> std::slice::Iter<'_, CompactMessage> {
        self.messages.iter()
    }

    /// Get all messages as full Message format (for serialization)
    pub fn get_all_messages(&self, interner: &NpubInterner) -> Vec<Message> {
        self.messages.iter()
            .map(|cm| cm.to_message(interner))
            .collect()
    }

    /// Get last N messages as full Message format
    pub fn get_last_messages(&self, n: usize, interner: &NpubInterner) -> Vec<Message> {
        let len = self.messages.len();
        let start = len.saturating_sub(n);
        self.messages.messages()[start..]
            .iter()
            .map(|cm| cm.to_message(interner))
            .collect()
    }

    // ========================================================================
    // Message Mutation
    // ========================================================================

    /// Add a Message, converting to compact format - O(log n) duplicate check
    pub fn add_message(&mut self, message: Message, interner: &mut NpubInterner) -> bool {
        let compact = CompactMessage::from_message(&message, interner);
        self.messages.insert(compact)
    }

    /// Add a pre-converted CompactMessage directly
    #[inline]
    pub fn add_compact_message(&mut self, message: CompactMessage) -> bool {
        self.messages.insert(message)
    }

    /// Set the last non-mine message as read
    pub fn set_as_read(&mut self) -> bool {
        for msg in self.messages.iter().rev() {
            if !msg.flags.is_mine() {
                self.last_read = msg.id_hex();
                return true;
            }
        }
        false
    }

    // ========================================================================
    // Compatibility Methods (for gradual migration)
    // ========================================================================

    /// Legacy: Add message (calls add_message internally)
    /// Used during migration - prefer add_message() with explicit interner
    pub fn internal_add_message(&mut self, message: Message, interner: &mut NpubInterner) -> bool {
        self.add_message(message, interner)
    }

    /// Get mutable message by ID (returns compact, caller must handle)
    #[inline]
    pub fn get_message_mut(&mut self, id: &str) -> Option<&mut CompactMessage> {
        self.get_compact_message_mut(id)
    }

    // ========================================================================
    // Serialization
    // ========================================================================

    /// Convert to SerializableChat for frontend communication
    pub fn to_serializable(&self, interner: &NpubInterner) -> SerializableChat {
        SerializableChat {
            id: self.id.clone(),
            chat_type: self.chat_type.clone(),
            participants: self.participants.clone(),
            messages: self.get_all_messages(interner),
            last_read: self.last_read.clone(),
            created_at: self.created_at,
            metadata: self.metadata.clone(),
            muted: self.muted,
        }
    }

    /// Convert to SerializableChat with only the last N messages (for efficiency)
    pub fn to_serializable_with_last_n(&self, n: usize, interner: &NpubInterner) -> SerializableChat {
        SerializableChat {
            id: self.id.clone(),
            chat_type: self.chat_type.clone(),
            participants: self.participants.clone(),
            messages: self.get_last_messages(n, interner),
            last_read: self.last_read.clone(),
            created_at: self.created_at,
            metadata: self.metadata.clone(),
            muted: self.muted,
        }
    }

    // ========================================================================
    // Chat Metadata & Participants
    // ========================================================================

    pub fn get_other_participant(&self, my_npub: &str) -> Option<String> {
        match self.chat_type {
            ChatType::DirectMessage => {
                self.participants.iter()
                    .find(|&p| p != my_npub)
                    .cloned()
            }
            ChatType::MlsGroup => None,
        }
    }

    pub fn is_dm_with(&self, npub: &str) -> bool {
        matches!(self.chat_type, ChatType::DirectMessage)
            && self.participants.iter().any(|p| p == npub)
    }

    pub fn is_mls_group(&self) -> bool {
        matches!(self.chat_type, ChatType::MlsGroup)
    }

    pub fn has_participant(&self, npub: &str) -> bool {
        self.participants.iter().any(|p| p == npub)
    }

    pub fn get_active_typers(&self) -> Vec<String> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        self.typing_participants
            .iter()
            .filter(|(_, &expires_at)| expires_at > now)
            .map(|(npub, _)| npub.clone())
            .collect()
    }

    pub fn update_typing_participant(&mut self, npub: String, expires_at: u64) {
        self.typing_participants.insert(npub, expires_at);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        self.typing_participants.retain(|_, &mut exp| exp > now);
    }

    // Getters
    pub fn id(&self) -> &String { &self.id }
    pub fn chat_type(&self) -> &ChatType { &self.chat_type }
    pub fn participants(&self) -> &Vec<String> { &self.participants }
    pub fn last_read(&self) -> &String { &self.last_read }
    pub fn created_at(&self) -> u64 { self.created_at }
    pub fn metadata(&self) -> &ChatMetadata { &self.metadata }
    pub fn muted(&self) -> bool { self.muted }
}

// ============================================================================
// SerializableChat (Frontend Communication)
// ============================================================================

/// Serializable chat format for Tauri commands and emit().
///
/// This is what the frontend receives. Create via `chat.to_serializable(interner)`.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SerializableChat {
    pub id: String,
    pub chat_type: ChatType,
    pub participants: Vec<String>,
    pub messages: Vec<Message>,
    pub last_read: String,
    pub created_at: u64,
    pub metadata: ChatMetadata,
    pub muted: bool,
}

impl SerializableChat {
    /// Convert to a Chat with compact storage (for loading from DB)
    #[allow(clippy::wrong_self_convention)]
    pub fn to_chat(self, interner: &mut NpubInterner) -> Chat {
        let mut chat = Chat::new(self.id, self.chat_type, self.participants);
        chat.last_read = self.last_read;
        chat.created_at = self.created_at;
        chat.metadata = self.metadata;
        chat.muted = self.muted;

        for msg in self.messages {
            chat.add_message(msg, interner);
        }

        chat
    }
}

// ============================================================================
// Supporting Types
// ============================================================================

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum ChatType {
    DirectMessage,
    MlsGroup,
}

impl ChatType {
    pub fn to_i32(&self) -> i32 {
        match self {
            ChatType::DirectMessage => 0,
            ChatType::MlsGroup => 1,
        }
    }

    pub fn from_i32(value: i32) -> Self {
        match value {
            1 => ChatType::MlsGroup,
            _ => ChatType::DirectMessage,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct ChatMetadata {
    pub custom_fields: HashMap<String, String>,
}

impl ChatMetadata {
    pub fn new() -> Self {
        Self { custom_fields: HashMap::new() }
    }

    pub fn set_name(&mut self, name: String) {
        self.custom_fields.insert("name".to_string(), name);
    }

    pub fn get_name(&self) -> Option<&str> {
        self.custom_fields.get("name").map(|s| s.as_str())
    }

    pub fn set_member_count(&mut self, count: usize) {
        self.custom_fields.insert("member_count".to_string(), count.to_string());
    }

    pub fn get_member_count(&self) -> Option<usize> {
        self.custom_fields.get("member_count").and_then(|s| s.parse().ok())
    }
}

// ============================================================================
// Tauri Commands
// ============================================================================

/// Marks a specific message as read for a chat.
#[tauri::command]
pub async fn mark_as_read(chat_id: String, message_id: Option<String>) -> bool {
    let handle = crate::TAURI_APP.get().unwrap();

    let (result, chat_id_for_save) = {
        let mut state = crate::STATE.lock().await;
        let mut result = false;
        let mut chat_id_for_save: Option<String> = None;

        if let Some(chat) = state.chats.iter_mut().find(|c| c.id == chat_id) {
            if let Some(msg_id) = &message_id {
                chat.last_read = msg_id.clone();
                result = true;
                chat_id_for_save = Some(chat.id.clone());
            } else {
                result = chat.set_as_read();
                if result {
                    chat_id_for_save = Some(chat.id.clone());
                }
            }
        }

        (result, chat_id_for_save)
    };

    if result {
        crate::commands::messaging::update_unread_counter(handle.clone()).await;

        if let Some(chat_id) = chat_id_for_save {
            let chat_to_save = {
                let state = crate::STATE.lock().await;
                state.get_chat(&chat_id).cloned()
            };

            if let Some(chat) = chat_to_save {
                let _ = crate::db::save_chat(handle.clone(), &chat).await;
            }
        }
    }

    result
}
