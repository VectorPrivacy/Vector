//! Chat types and management — compact message storage.
//!
//! `Chat` uses `CompactMessageVec` internally for memory efficiency.
//! Use `to_serializable()` to convert to frontend-friendly `SerializableChat`.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::compact::{CompactMessage, CompactMessageVec, NpubInterner, encode_message_id, decode_message_id};
use crate::types::Message;

// ============================================================================
// Chat (Internal Storage)
// ============================================================================

#[derive(Clone, Debug)]
pub struct Chat {
    pub id: String,
    pub chat_type: ChatType,
    pub participants: Vec<u16>,
    pub messages: CompactMessageVec,
    pub last_read: [u8; 32],
    pub created_at: u64,
    pub metadata: ChatMetadata,
    pub muted: bool,
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

    pub fn new_dm(their_npub: String, interner: &mut NpubInterner) -> Self {
        let handle = interner.intern(&their_npub);
        Self::new(their_npub, ChatType::DirectMessage, vec![handle])
    }

    pub fn new_mls_group(group_id: String, participants: Vec<String>, interner: &mut NpubInterner) -> Self {
        let handles: Vec<u16> = participants.iter().map(|p| interner.intern(p)).collect();
        Self::new(group_id, ChatType::MlsGroup, handles)
    }

    // ========================================================================
    // Message Access
    // ========================================================================

    #[inline]
    pub fn message_count(&self) -> usize { self.messages.len() }

    #[inline]
    pub fn is_empty(&self) -> bool { self.messages.is_empty() }

    #[inline]
    pub fn last_message_time(&self) -> Option<u64> { self.messages.last_timestamp() }

    #[inline]
    pub fn has_message(&self, id: &str) -> bool { self.messages.contains_hex_id(id) }

    #[inline]
    pub fn get_compact_message(&self, id: &str) -> Option<&CompactMessage> {
        self.messages.find_by_hex_id(id)
    }

    #[inline]
    pub fn get_compact_message_mut(&mut self, id: &str) -> Option<&mut CompactMessage> {
        self.messages.find_by_hex_id_mut(id)
    }

    pub fn get_message(&self, id: &str, interner: &NpubInterner) -> Option<Message> {
        self.messages.find_by_hex_id(id).map(|cm| cm.to_message(interner))
    }

    #[inline]
    pub fn iter_compact(&self) -> std::slice::Iter<'_, CompactMessage> {
        self.messages.iter()
    }

    pub fn get_all_messages(&self, interner: &NpubInterner) -> Vec<Message> {
        self.messages.iter().map(|cm| cm.to_message(interner)).collect()
    }

    pub fn get_last_messages(&self, n: usize, interner: &NpubInterner) -> Vec<Message> {
        let len = self.messages.len();
        let start = len.saturating_sub(n);
        self.messages.messages()[start..].iter().map(|cm| cm.to_message(interner)).collect()
    }

    // ========================================================================
    // Message Mutation
    // ========================================================================

    pub fn add_message(&mut self, message: Message, interner: &mut NpubInterner) -> bool {
        let compact = CompactMessage::from_message(&message, interner);
        self.messages.insert(compact)
    }

    #[inline]
    pub fn add_compact_message(&mut self, message: CompactMessage) -> bool {
        self.messages.insert(message)
    }

    pub fn set_as_read(&mut self) -> bool {
        for msg in self.messages.iter().rev() {
            if !msg.flags.is_mine() {
                self.last_read = msg.id;
                return true;
            }
        }
        false
    }

    pub fn internal_add_message(&mut self, message: Message, interner: &mut NpubInterner) -> bool {
        self.add_message(message, interner)
    }

    #[inline]
    pub fn get_message_mut(&mut self, id: &str) -> Option<&mut CompactMessage> {
        self.get_compact_message_mut(id)
    }

    // ========================================================================
    // Serialization
    // ========================================================================

    fn resolve_participants(&self, interner: &NpubInterner) -> Vec<String> {
        self.participants.iter()
            .filter_map(|&h| interner.resolve(h).map(|s| s.to_string()))
            .collect()
    }

    pub fn to_serializable(&self, interner: &NpubInterner) -> SerializableChat {
        SerializableChat {
            id: self.id.clone(),
            chat_type: self.chat_type.clone(),
            participants: self.resolve_participants(interner),
            messages: self.get_all_messages(interner),
            last_read: if self.last_read == [0u8; 32] { String::new() } else { decode_message_id(&self.last_read) },
            created_at: self.created_at,
            metadata: self.metadata.clone(),
            muted: self.muted,
        }
    }

    pub fn to_serializable_with_last_n(&self, n: usize, interner: &NpubInterner) -> SerializableChat {
        SerializableChat {
            id: self.id.clone(),
            chat_type: self.chat_type.clone(),
            participants: self.resolve_participants(interner),
            messages: self.get_last_messages(n, interner),
            last_read: if self.last_read == [0u8; 32] { String::new() } else { decode_message_id(&self.last_read) },
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
        matches!(self.chat_type, ChatType::DirectMessage)
            && interner.lookup(npub).map_or(false, |h| self.participants.contains(&h))
    }

    pub fn is_mls_group(&self) -> bool { matches!(self.chat_type, ChatType::MlsGroup) }

    pub fn has_participant(&self, npub: &str, interner: &NpubInterner) -> bool {
        interner.lookup(npub).map_or(false, |h| self.participants.contains(&h))
    }

    pub fn get_active_typers(&self, interner: &NpubInterner) -> Vec<String> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
        self.typing_participants.iter()
            .filter(|(_, exp)| *exp > now)
            .filter_map(|(h, _)| interner.resolve(*h).map(|s| s.to_string()))
            .collect()
    }

    pub fn update_typing_participant(&mut self, handle: u16, expires_at: u64) {
        if let Some(entry) = self.typing_participants.iter_mut().find(|(h, _)| *h == handle) {
            entry.1 = expires_at;
        } else {
            self.typing_participants.push((handle, expires_at));
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
        self.typing_participants.retain(|(_, exp)| *exp > now);
    }

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
    pub fn to_chat(self, interner: &mut NpubInterner) -> Chat {
        let handles: Vec<u16> = self.participants.iter().map(|p| interner.intern(p)).collect();
        let mut chat = Chat::new(self.id, self.chat_type, handles);
        chat.last_read = if self.last_read.is_empty() { [0u8; 32] } else { encode_message_id(&self.last_read) };
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
        match self { ChatType::DirectMessage => 0, ChatType::MlsGroup => 1 }
    }
    pub fn from_i32(value: i32) -> Self {
        match value { 1 => ChatType::MlsGroup, _ => ChatType::DirectMessage }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct ChatMetadata {
    pub custom_fields: HashMap<String, String>,
}

impl ChatMetadata {
    pub fn new() -> Self { Self { custom_fields: HashMap::new() } }

    pub fn set_name(&mut self, name: String) { self.custom_fields.insert("name".to_string(), name); }
    pub fn get_name(&self) -> Option<&str> { self.custom_fields.get("name").map(|s| s.as_str()) }
    pub fn set_member_count(&mut self, count: usize) { self.custom_fields.insert("member_count".to_string(), count.to_string()); }
    pub fn get_member_count(&self) -> Option<usize> { self.custom_fields.get("member_count").and_then(|s| s.parse().ok()) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Message;
    use crate::compact::NpubInterner;
    use crate::simd::hex::bytes_to_hex_32;

    // ========================================================================
    // Helpers
    // ========================================================================

    /// Create a deterministic 64-char hex ID from a u8 seed.
    /// First byte is always >= 0x10 to avoid the pending ID marker (0x01).
    fn make_hex_id(seed: u8) -> String {
        let mut bytes = [seed; 32];
        bytes[0] = seed.wrapping_add(0x10) | 0x10; // never 0x00 or 0x01
        bytes[1] = seed.wrapping_mul(37);
        bytes_to_hex_32(&bytes)
    }

    /// Build a test Message with the given parameters.
    fn make_message(id_seed: u8, content: &str, timestamp_ms: u64, mine: bool) -> Message {
        Message {
            id: make_hex_id(id_seed),
            content: content.to_string(),
            at: timestamp_ms,
            mine,
            ..Default::default()
        }
    }

    // ========================================================================
    // Chat Construction
    // ========================================================================

    #[test]
    fn new_dm_creates_correct_type() {
        let mut interner = NpubInterner::new();
        let chat = Chat::new_dm("npub1alice".to_string(), &mut interner);

        assert_eq!(chat.id, "npub1alice", "DM chat id should be the peer's npub");
        assert_eq!(chat.chat_type, ChatType::DirectMessage, "should be DirectMessage type");
        assert_eq!(chat.participants.len(), 1, "DM should have one participant");
        assert!(chat.is_empty(), "new chat should have no messages");
        assert!(!chat.muted, "new chat should not be muted");
        assert_eq!(chat.last_read, [0u8; 32], "last_read should be zeroed");
    }

    #[test]
    fn new_mls_group_with_participants() {
        let mut interner = NpubInterner::new();
        let participants = vec![
            "npub1alice".to_string(),
            "npub1bob".to_string(),
            "npub1charlie".to_string(),
        ];
        let chat = Chat::new_mls_group("grp_abc".to_string(), participants, &mut interner);

        assert_eq!(chat.id, "grp_abc", "group chat id should match");
        assert_eq!(chat.chat_type, ChatType::MlsGroup, "should be MlsGroup type");
        assert_eq!(chat.participants.len(), 3, "should have 3 participants");
        assert!(chat.is_mls_group(), "is_mls_group() should return true");
    }

    #[test]
    fn new_chat_has_creation_timestamp() {
        let mut interner = NpubInterner::new();
        let chat = Chat::new_dm("npub1peer".to_string(), &mut interner);

        // created_at should be recent (within last 5 seconds)
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
        assert!(
            chat.created_at >= now - 5 && chat.created_at <= now + 1,
            "created_at ({}) should be close to now ({})",
            chat.created_at, now
        );
    }

    // ========================================================================
    // Message Operations
    // ========================================================================

    #[test]
    fn add_message_and_get_message_roundtrip() {
        let mut interner = NpubInterner::new();
        let mut chat = Chat::new_dm("npub1peer".to_string(), &mut interner);

        let msg = make_message(1, "hello world", 1700000000000, false);
        let msg_id = msg.id.clone();

        let added = chat.add_message(msg, &mut interner);
        assert!(added, "message should be added successfully");

        let retrieved = chat.get_message(&msg_id, &interner)
            .expect("message should be retrievable");
        assert_eq!(retrieved.content, "hello world", "content should roundtrip");
        assert_eq!(retrieved.id, msg_id, "id should roundtrip");
    }

    #[test]
    fn add_message_dedup() {
        let mut interner = NpubInterner::new();
        let mut chat = Chat::new_dm("npub1peer".to_string(), &mut interner);

        let msg1 = make_message(1, "first", 1700000000000, false);
        let msg2 = make_message(1, "duplicate id", 1700000001000, false);

        assert!(chat.add_message(msg1, &mut interner), "first add should succeed");
        assert!(!chat.add_message(msg2, &mut interner), "duplicate ID should be rejected");
        assert_eq!(chat.message_count(), 1, "should have only one message");
    }

    #[test]
    fn message_count_and_is_empty() {
        let mut interner = NpubInterner::new();
        let mut chat = Chat::new_dm("npub1peer".to_string(), &mut interner);

        assert!(chat.is_empty(), "new chat should be empty");
        assert_eq!(chat.message_count(), 0, "new chat should have 0 messages");

        chat.add_message(make_message(1, "a", 1700000000000, false), &mut interner);
        assert!(!chat.is_empty(), "chat with message should not be empty");
        assert_eq!(chat.message_count(), 1, "should have 1 message");

        chat.add_message(make_message(2, "b", 1700000001000, false), &mut interner);
        assert_eq!(chat.message_count(), 2, "should have 2 messages");
    }

    #[test]
    fn has_message_check() {
        let mut interner = NpubInterner::new();
        let mut chat = Chat::new_dm("npub1peer".to_string(), &mut interner);

        let msg = make_message(1, "test", 1700000000000, false);
        let msg_id = msg.id.clone();
        chat.add_message(msg, &mut interner);

        assert!(chat.has_message(&msg_id), "added message should be found");
        assert!(!chat.has_message(&make_hex_id(99)), "unknown id should not be found");
    }

    #[test]
    fn get_all_messages_returns_all() {
        let mut interner = NpubInterner::new();
        let mut chat = Chat::new_dm("npub1peer".to_string(), &mut interner);

        for i in 0..5u8 {
            chat.add_message(
                make_message(i, &format!("msg {}", i), 1700000000000 + i as u64 * 1000, false),
                &mut interner,
            );
        }

        let all = chat.get_all_messages(&interner);
        assert_eq!(all.len(), 5, "should return all 5 messages");
    }

    #[test]
    fn get_last_messages_returns_tail() {
        let mut interner = NpubInterner::new();
        let mut chat = Chat::new_dm("npub1peer".to_string(), &mut interner);

        for i in 0..10u8 {
            chat.add_message(
                make_message(i, &format!("msg {}", i), 1700000000000 + i as u64 * 1000, false),
                &mut interner,
            );
        }

        let last3 = chat.get_last_messages(3, &interner);
        assert_eq!(last3.len(), 3, "should return exactly 3 messages");
        assert_eq!(last3[0].content, "msg 7", "first of last 3 should be msg 7");
        assert_eq!(last3[2].content, "msg 9", "last should be msg 9");
    }

    #[test]
    fn last_message_time_tracks_newest() {
        let mut interner = NpubInterner::new();
        let mut chat = Chat::new_dm("npub1peer".to_string(), &mut interner);

        assert!(chat.last_message_time().is_none(), "empty chat should have no last time");

        chat.add_message(make_message(1, "a", 1700000001000, false), &mut interner);
        let t1 = chat.last_message_time().expect("should have a timestamp");

        chat.add_message(make_message(2, "b", 1700000005000, false), &mut interner);
        let t2 = chat.last_message_time().expect("should have a timestamp");

        assert!(t2 > t1, "last_message_time should increase with newer messages");
    }

    // ========================================================================
    // set_as_read
    // ========================================================================

    #[test]
    fn set_as_read_marks_last_non_mine() {
        let mut interner = NpubInterner::new();
        let mut chat = Chat::new_dm("npub1peer".to_string(), &mut interner);

        let msg_theirs = make_message(1, "from them", 1700000001000, false);
        let msg_mine = make_message(2, "from me", 1700000002000, true);
        let msg_theirs2 = make_message(3, "from them again", 1700000003000, false);
        let last_their_id = msg_theirs2.id.clone();

        chat.add_message(msg_theirs, &mut interner);
        chat.add_message(msg_mine, &mut interner);
        chat.add_message(msg_theirs2, &mut interner);

        let marked = chat.set_as_read();
        assert!(marked, "set_as_read should succeed when there are non-mine messages");

        let expected_bytes = crate::compact::encode_message_id(&last_their_id);
        assert_eq!(
            chat.last_read, expected_bytes,
            "last_read should point to the last non-mine message"
        );
    }

    #[test]
    fn set_as_read_all_mine_returns_false() {
        let mut interner = NpubInterner::new();
        let mut chat = Chat::new_dm("npub1peer".to_string(), &mut interner);

        chat.add_message(make_message(1, "mine 1", 1700000001000, true), &mut interner);
        chat.add_message(make_message(2, "mine 2", 1700000002000, true), &mut interner);

        let marked = chat.set_as_read();
        assert!(!marked, "set_as_read should return false when all messages are mine");
        assert_eq!(chat.last_read, [0u8; 32], "last_read should remain zeroed");
    }

    // ========================================================================
    // Serialization Roundtrip
    // ========================================================================

    #[test]
    fn to_serializable_and_back_via_to_chat() {
        let mut interner = NpubInterner::new();
        let participants = vec!["npub1alice".to_string(), "npub1bob".to_string()];
        let mut chat = Chat::new_mls_group("grp_test".to_string(), participants.clone(), &mut interner);

        chat.metadata.set_name("Test Group".to_string());
        chat.muted = true;

        // Add messages
        chat.add_message(make_message(1, "hello", 1700000001000, false), &mut interner);
        chat.add_message(make_message(2, "world", 1700000002000, true), &mut interner);

        // Set last_read
        chat.set_as_read();

        // Serialize to SerializableChat
        let serializable = chat.to_serializable(&interner);
        assert_eq!(serializable.id, "grp_test", "serialized id should match");
        assert_eq!(serializable.chat_type, ChatType::MlsGroup, "serialized type should match");
        assert_eq!(serializable.participants.len(), 2, "should have 2 participants");
        assert_eq!(serializable.messages.len(), 2, "should have 2 messages");
        assert!(serializable.muted, "muted should be preserved");
        assert_eq!(
            serializable.metadata.get_name(),
            Some("Test Group"),
            "metadata name should be preserved"
        );

        // Convert back to Chat
        let mut interner2 = NpubInterner::new();
        let restored = serializable.to_chat(&mut interner2);

        assert_eq!(restored.id, "grp_test", "restored id should match");
        assert_eq!(restored.chat_type, ChatType::MlsGroup, "restored type should match");
        assert_eq!(restored.participants.len(), 2, "restored participants count should match");
        assert_eq!(restored.message_count(), 2, "restored message count should match");
        assert!(restored.muted, "restored muted should be true");
        assert_ne!(restored.last_read, [0u8; 32], "restored last_read should be non-zero");
    }

    #[test]
    fn to_serializable_with_last_n() {
        let mut interner = NpubInterner::new();
        let mut chat = Chat::new_dm("npub1peer".to_string(), &mut interner);

        for i in 0..10u8 {
            chat.add_message(
                make_message(i, &format!("msg {}", i), 1700000000000 + i as u64 * 1000, false),
                &mut interner,
            );
        }

        let serialized = chat.to_serializable_with_last_n(3, &interner);
        assert_eq!(serialized.messages.len(), 3, "should only include last 3 messages");
    }

    // ========================================================================
    // Participants
    // ========================================================================

    #[test]
    fn has_participant_check() {
        let mut interner = NpubInterner::new();
        let chat = Chat::new_mls_group(
            "grp1".to_string(),
            vec!["npub1alice".to_string(), "npub1bob".to_string()],
            &mut interner,
        );

        assert!(
            chat.has_participant("npub1alice", &interner),
            "alice should be a participant"
        );
        assert!(
            chat.has_participant("npub1bob", &interner),
            "bob should be a participant"
        );
        assert!(
            !chat.has_participant("npub1charlie", &interner),
            "charlie should not be a participant"
        );
    }

    #[test]
    fn is_dm_with_check() {
        let mut interner = NpubInterner::new();
        let chat = Chat::new_dm("npub1alice".to_string(), &mut interner);

        assert!(
            chat.is_dm_with("npub1alice", &interner),
            "should be a DM with alice"
        );
        assert!(
            !chat.is_dm_with("npub1bob", &interner),
            "should not be a DM with bob"
        );
    }

    #[test]
    fn is_dm_with_returns_false_for_group() {
        let mut interner = NpubInterner::new();
        let chat = Chat::new_mls_group(
            "grp1".to_string(),
            vec!["npub1alice".to_string()],
            &mut interner,
        );

        assert!(
            !chat.is_dm_with("npub1alice", &interner),
            "MLS group should not match is_dm_with even if participant matches"
        );
    }

    #[test]
    fn get_other_participant_dm() {
        let mut interner = NpubInterner::new();
        // In a DM, the participant list typically has the other person
        let chat = Chat::new_dm("npub1bob".to_string(), &mut interner);

        let other = chat.get_other_participant("npub1alice", &interner);
        assert_eq!(
            other, Some("npub1bob".to_string()),
            "should return bob as the other participant"
        );
    }

    #[test]
    fn get_other_participant_returns_none_for_group() {
        let mut interner = NpubInterner::new();
        let chat = Chat::new_mls_group(
            "grp1".to_string(),
            vec!["npub1alice".to_string(), "npub1bob".to_string()],
            &mut interner,
        );

        assert!(
            chat.get_other_participant("npub1alice", &interner).is_none(),
            "MLS group should return None for get_other_participant"
        );
    }

    // ========================================================================
    // Typing Participants
    // ========================================================================

    #[test]
    fn typing_participants_with_expiry() {
        let mut interner = NpubInterner::new();
        let mut chat = Chat::new_dm("npub1peer".to_string(), &mut interner);

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();

        // One active, one expired
        let active_handle = interner.intern("npub1active");
        let expired_handle = interner.intern("npub1expired");

        chat.update_typing_participant(active_handle, now + 300);
        chat.update_typing_participant(expired_handle, now - 10);

        let active = chat.get_active_typers(&interner);
        assert_eq!(active.len(), 1, "only the active typer should be returned");
        assert_eq!(active[0], "npub1active", "active typer should be npub1active");
    }

    #[test]
    fn update_typing_participant_refreshes() {
        let mut interner = NpubInterner::new();
        let mut chat = Chat::new_dm("npub1peer".to_string(), &mut interner);

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();

        let handle = interner.intern("npub1typer");
        chat.update_typing_participant(handle, now + 100);
        chat.update_typing_participant(handle, now + 500);

        // Should still be one entry, not duplicated
        let active = chat.get_active_typers(&interner);
        assert_eq!(active.len(), 1, "refreshed typer should not duplicate");
    }

    #[test]
    fn typing_participants_empty_initially() {
        let interner = NpubInterner::new();
        let chat = Chat::new("testchat".to_string(), ChatType::DirectMessage, vec![]);

        let active = chat.get_active_typers(&interner);
        assert!(active.is_empty(), "new chat should have no typers");
    }

    // ========================================================================
    // ChatType
    // ========================================================================

    #[test]
    fn chat_type_i32_roundtrip() {
        assert_eq!(ChatType::from_i32(ChatType::DirectMessage.to_i32()), ChatType::DirectMessage);
        assert_eq!(ChatType::from_i32(ChatType::MlsGroup.to_i32()), ChatType::MlsGroup);
        assert_eq!(
            ChatType::from_i32(999), ChatType::DirectMessage,
            "unknown i32 should default to DirectMessage"
        );
    }

    // ========================================================================
    // ChatMetadata
    // ========================================================================

    #[test]
    fn chat_metadata_name_and_member_count() {
        let mut meta = ChatMetadata::new();

        assert!(meta.get_name().is_none(), "new metadata should have no name");
        assert!(meta.get_member_count().is_none(), "new metadata should have no member count");

        meta.set_name("My Group".to_string());
        meta.set_member_count(42);

        assert_eq!(meta.get_name(), Some("My Group"), "name should be set");
        assert_eq!(meta.get_member_count(), Some(42), "member count should be set");
    }

    // ========================================================================
    // Accessor Methods
    // ========================================================================

    #[test]
    fn accessor_methods_work() {
        let mut interner = NpubInterner::new();
        let mut chat = Chat::new_dm("npub1test".to_string(), &mut interner);
        chat.muted = true;

        assert_eq!(chat.id(), "npub1test");
        assert_eq!(*chat.chat_type(), ChatType::DirectMessage);
        assert_eq!(chat.participants().len(), 1);
        assert_eq!(*chat.last_read(), [0u8; 32]);
        assert!(chat.created_at() > 0);
        assert!(chat.muted());
        assert_eq!(*chat.metadata(), ChatMetadata::new());
    }
}
