//! Chat types and management.
//!
//! This module provides:
//! - `Chat`: Core chat struct with compact message storage
//! - `SerializableChat`: Frontend-friendly format for Tauri communication
//! - `ChatType`, `ChatMetadata`: Supporting types

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::message::compact::{CompactMessage, CompactMessageVec, NpubInterner, encode_message_id, decode_message_id};
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
    /// Interned participant handles (indices into NpubInterner)
    pub participants: Vec<u16>,
    /// Compact message storage - O(log n) lookup by ID
    pub messages: CompactMessageVec,
    pub last_read: [u8; 32],
    pub created_at: u64,
    pub metadata: ChatMetadata,
    pub muted: bool,
    /// Typing participants (interned handle, expires_at), memory-only
    pub typing_participants: Vec<(u16, u64)>,
}

impl Chat {
    pub fn new(id: String, chat_type: ChatType, participants: Vec<u16>) -> Self {
        Self {
            id,
            chat_type,
            participants,
            messages: CompactMessageVec::new(),
            last_read: [0u8; 32],
            created_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            metadata: ChatMetadata::new(),
            muted: false,
            typing_participants: Vec::new(),
        }
    }

    /// Create a new DM chat
    pub fn new_dm(their_npub: String, interner: &mut NpubInterner) -> Self {
        let handle = interner.intern(&their_npub);
        Self::new(their_npub, ChatType::DirectMessage, vec![handle])
    }

    /// Create a new MLS group chat
    pub fn new_mls_group(group_id: String, participants: Vec<String>, interner: &mut NpubInterner) -> Self {
        let handles: Vec<u16> = participants.iter().map(|p| interner.intern(p)).collect();
        Self::new(group_id, ChatType::MlsGroup, handles)
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
                self.last_read = msg.id;
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

    /// Resolve participant handles to strings
    fn resolve_participants(&self, interner: &NpubInterner) -> Vec<String> {
        self.participants.iter()
            .filter_map(|&h| interner.resolve(h).map(|s| s.to_string()))
            .collect()
    }

    /// Convert to SerializableChat for frontend communication
    pub fn to_serializable(&self, interner: &NpubInterner) -> SerializableChat {
        SerializableChat {
            id: self.id.clone(),
            chat_type: self.chat_type.clone(),
            participants: self.resolve_participants(interner),
            messages: self.get_all_messages(interner),
            last_read: if self.last_read == [0u8; 32] {
                String::new()
            } else {
                decode_message_id(&self.last_read)
            },
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
            participants: self.resolve_participants(interner),
            messages: self.get_last_messages(n, interner),
            last_read: if self.last_read == [0u8; 32] {
                String::new()
            } else {
                decode_message_id(&self.last_read)
            },
            created_at: self.created_at,
            metadata: self.metadata.clone(),
            muted: self.muted,
        }
    }

    // ========================================================================
    // Chat Metadata & Participants
    // ========================================================================

    pub fn get_other_participant(&self, my_npub: &str, interner: &NpubInterner) -> Option<String> {
        match self.chat_type {
            ChatType::DirectMessage => {
                let my_handle = interner.lookup(my_npub);
                self.participants.iter()
                    .find(|&&h| Some(h) != my_handle)
                    .and_then(|&h| interner.resolve(h).map(|s| s.to_string()))
            }
            ChatType::MlsGroup => None,
        }
    }

    pub fn is_dm_with(&self, npub: &str, interner: &NpubInterner) -> bool {
        if !matches!(self.chat_type, ChatType::DirectMessage) {
            return false;
        }
        if let Some(handle) = interner.lookup(npub) {
            self.participants.contains(&handle)
        } else {
            false
        }
    }

    pub fn is_mls_group(&self) -> bool {
        matches!(self.chat_type, ChatType::MlsGroup)
    }

    pub fn has_participant(&self, npub: &str, interner: &NpubInterner) -> bool {
        if let Some(handle) = interner.lookup(npub) {
            self.participants.contains(&handle)
        } else {
            false
        }
    }

    pub fn get_active_typers(&self, interner: &NpubInterner) -> Vec<String> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        self.typing_participants
            .iter()
            .filter(|(_, expires_at)| *expires_at > now)
            .filter_map(|(handle, _)| interner.resolve(*handle).map(|s| s.to_string()))
            .collect()
    }

    pub fn update_typing_participant(&mut self, handle: u16, expires_at: u64) {
        // Update or insert
        if let Some(entry) = self.typing_participants.iter_mut().find(|(h, _)| *h == handle) {
            entry.1 = expires_at;
        } else {
            self.typing_participants.push((handle, expires_at));
        }
        // Prune expired
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        self.typing_participants.retain(|(_, exp)| *exp > now);
    }

    // Getters
    pub fn id(&self) -> &String { &self.id }
    pub fn chat_type(&self) -> &ChatType { &self.chat_type }
    pub fn participants(&self) -> &[u16] { &self.participants }
    pub fn last_read(&self) -> &[u8; 32] { &self.last_read }
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
        let handles: Vec<u16> = self.participants.iter().map(|p| interner.intern(p)).collect();
        let mut chat = Chat::new(self.id, self.chat_type, handles);
        chat.last_read = if self.last_read.is_empty() {
            [0u8; 32]
        } else {
            encode_message_id(&self.last_read)
        };
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
                chat.last_read = encode_message_id(msg_id);
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
            let slim = {
                let state = crate::STATE.lock().await;
                state.get_chat(&chat_id).map(|chat| {
                    crate::db::chats::SlimChatDB::from_chat(chat, &state.interner)
                })
            };
            if let Some(slim) = slim {
                let _ = crate::db::chats::save_slim_chat(handle.clone(), slim).await;
            }
        }
    }

    result
}
