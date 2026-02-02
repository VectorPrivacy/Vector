//! Flat Event Storage Module
//!
//! This module provides the generic event storage layer aligned with Nostr's protocol model.
//! All events (messages, reactions, attachments, etc.) are stored as flat rows in the database,
//! with relationships computed at query/render time.
//!
//! ## Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────┐
//! │ STORAGE LAYER (this module)                                    │
//! │ - Stores raw events as-is, including unknown types             │
//! │ - Schema: id, kind, content, tags, timestamp, pubkey, etc.     │
//! │ - Can sync/store events Vector doesn't understand yet          │
//! └─────────────────────────────────────────────────────────────────┘
//!                               ↓
//! ┌─────────────────────────────────────────────────────────────────┐
//! │ PROCESSING LAYER (rumor.rs)                                    │
//! │ - Transforms raw events → typed structs                        │
//! │ - Unknown kind? → UnknownEvent (renders as placeholder)        │
//! │ - New event type = add enum variant + processing logic         │
//! └─────────────────────────────────────────────────────────────────┘
//!                               ↓
//! ┌─────────────────────────────────────────────────────────────────┐
//! │ DISPLAY LAYER (message.rs - unchanged)                         │
//! │ - Message, Reaction, Attachment, etc.                          │
//! │ - Standardized interface for UI rendering                      │
//! │ - Materialized views: compose events → Message with reactions  │
//! └─────────────────────────────────────────────────────────────────┘
//! ```
//!
//! ## Benefits
//!
//! - **Protocol alignment**: Matches Nostr's event-centric model
//! - **Future-proof**: Unknown event types are stored, not dropped
//! - **Easy extensibility**: New event types need no schema changes
//! - **Uniform storage**: DMs, MLS groups, and public events all stored the same way

use serde::{Deserialize, Serialize};

/// Nostr event kinds used in Vector
///
/// These are the standard Nostr kinds plus Vector-specific extensions.
/// Unknown kinds are stored but rendered as placeholders.
pub mod event_kind {
    /// MLS Group Chat Message (text content) - White Noise compatible
    /// Used for text messages inside MLS groups (Kind 9)
    pub const MLS_CHAT_MESSAGE: u16 = 9;
    /// NIP-14: Private Direct Message (text content)
    /// Used for DMs and legacy MLS messages
    pub const PRIVATE_DIRECT_MESSAGE: u16 = 14;
    /// Vector-specific: File attachment with encryption metadata
    pub const FILE_ATTACHMENT: u16 = 15;
    /// Vector-specific: Message edit (references original message, contains new content)
    pub const MESSAGE_EDIT: u16 = 16;
    /// NIP-25: Emoji reaction
    pub const REACTION: u16 = 7;
    /// NIP-78: Application-specific data (typing indicators, peer ads, etc.)
    pub const APPLICATION_SPECIFIC: u16 = 30078;
    /// MLS Welcome message
    pub const MLS_WELCOME: u16 = 443;
    /// MLS Group message (wrapper)
    pub const MLS_GROUP_MESSAGE: u16 = 444;
    /// MLS Key Package
    pub const MLS_KEY_PACKAGE: u16 = 443;
}

/// A stored event - the flat, protocol-aligned storage format
///
/// This struct represents any Nostr event after unwrapping/decryption.
/// It's designed to store events generically, allowing Vector to:
/// - Store unknown event types for future compatibility
/// - Query events efficiently with parsed fields
/// - Reconstruct typed display objects (Message, Reaction) at render time
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredEvent {
    /// Event ID (hex string, 64 chars)
    pub id: String,

    /// Nostr event kind (14=DM, 7=reaction, 15=file, etc.)
    pub kind: u16,

    /// Database chat ID (foreign key)
    pub chat_id: i64,

    /// Database user ID of sender (foreign key, optional)
    pub user_id: Option<i64>,

    /// Event content (encrypted for messages, emoji for reactions, URL for files)
    pub content: String,

    /// Nostr-style tags as JSON array
    /// Example: [["e", "abc123", "", "reply"], ["p", "pubkey"]]
    pub tags: Vec<Vec<String>>,

    /// Parsed reference ID for quick lookups
    /// - For reactions: the message ID being reacted to
    /// - For attachments: the message ID they belong to (if separate)
    /// - For messages: None
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reference_id: Option<String>,

    /// Event creation timestamp (Unix seconds)
    pub created_at: u64,

    /// When we received this event (Unix milliseconds)
    pub received_at: u64,

    /// Whether this event is from the current user
    pub mine: bool,

    /// Whether this event is pending confirmation (outgoing only)
    #[serde(default)]
    pub pending: bool,

    /// Whether sending this event failed (outgoing only)
    #[serde(default)]
    pub failed: bool,

    /// Outer giftwrap event ID for deduplication during sync
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wrapper_event_id: Option<String>,

    /// Sender's npub (for group chats where sender varies)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub npub: Option<String>,
}

impl StoredEvent {
    /// Create a new StoredEvent with required fields
    pub fn new(id: String, kind: u16, chat_id: i64, content: String, created_at: u64) -> Self {
        Self {
            id,
            kind,
            chat_id,
            user_id: None,
            content,
            tags: Vec::new(),
            reference_id: None,
            created_at,
            received_at: current_timestamp_ms(),
            mine: false,
            pending: false,
            failed: false,
            wrapper_event_id: None,
            npub: None,
        }
    }

    /// Check if this is a message event (text or file)
    pub fn is_message(&self) -> bool {
        self.kind == event_kind::MLS_CHAT_MESSAGE
            || self.kind == event_kind::PRIVATE_DIRECT_MESSAGE
            || self.kind == event_kind::FILE_ATTACHMENT
    }

    /// Check if this is a reaction event
    pub fn is_reaction(&self) -> bool {
        self.kind == event_kind::REACTION
    }

    /// Check if this is a known event type
    pub fn is_known_kind(&self) -> bool {
        matches!(
            self.kind,
            event_kind::MLS_CHAT_MESSAGE
                | event_kind::PRIVATE_DIRECT_MESSAGE
                | event_kind::FILE_ATTACHMENT
                | event_kind::REACTION
                | event_kind::APPLICATION_SPECIFIC
        )
    }

    /// Get a tag value by key (first match)
    pub fn get_tag(&self, key: &str) -> Option<&str> {
        self.tags
            .iter()
            .find(|tag| tag.first().map(|s| s.as_str()) == Some(key))
            .and_then(|tag| tag.get(1))
            .map(|s| s.as_str())
    }

    /// Get all tag values for a key
    pub fn get_tags(&self, key: &str) -> Vec<&str> {
        self.tags
            .iter()
            .filter(|tag| tag.first().map(|s| s.as_str()) == Some(key))
            .filter_map(|tag| tag.get(1))
            .map(|s| s.as_str())
            .collect()
    }

    /// Get the reply reference (e tag with "reply" marker)
    pub fn get_reply_reference(&self) -> Option<&str> {
        self.tags
            .iter()
            .find(|tag| {
                tag.first().map(|s| s.as_str()) == Some("e")
                    && tag.get(3).map(|s| s.as_str()) == Some("reply")
            })
            .and_then(|tag| tag.get(1))
            .map(|s| s.as_str())
    }

    /// Get millisecond-precision timestamp
    /// Combines created_at (seconds) with "ms" tag if present
    pub fn timestamp_ms(&self) -> u64 {
        if let Some(ms_str) = self.get_tag("ms") {
            if let Ok(ms) = ms_str.parse::<u64>() {
                if ms <= 999 {
                    return self.created_at * 1000 + ms;
                }
            }
        }
        self.created_at * 1000
    }
}

/// Get current timestamp in milliseconds
fn current_timestamp_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Builder for creating StoredEvent from rumor processing
#[derive(Debug, Default)]
pub struct StoredEventBuilder {
    id: String,
    kind: u16,
    chat_id: i64,
    user_id: Option<i64>,
    content: String,
    tags: Vec<Vec<String>>,
    reference_id: Option<String>,
    created_at: u64,
    mine: bool,
    pending: bool,
    failed: bool,
    wrapper_event_id: Option<String>,
    npub: Option<String>,
}

impl StoredEventBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn id(mut self, id: impl Into<String>) -> Self {
        self.id = id.into();
        self
    }

    pub fn kind(mut self, kind: u16) -> Self {
        self.kind = kind;
        self
    }

    pub fn chat_id(mut self, chat_id: i64) -> Self {
        self.chat_id = chat_id;
        self
    }

    pub fn user_id(mut self, user_id: Option<i64>) -> Self {
        self.user_id = user_id;
        self
    }

    pub fn content(mut self, content: impl Into<String>) -> Self {
        self.content = content.into();
        self
    }

    pub fn tags(mut self, tags: Vec<Vec<String>>) -> Self {
        self.tags = tags;
        self
    }

    pub fn reference_id(mut self, reference_id: Option<String>) -> Self {
        self.reference_id = reference_id;
        self
    }

    pub fn created_at(mut self, created_at: u64) -> Self {
        self.created_at = created_at;
        self
    }

    pub fn mine(mut self, mine: bool) -> Self {
        self.mine = mine;
        self
    }

    pub fn pending(mut self, pending: bool) -> Self {
        self.pending = pending;
        self
    }

    pub fn failed(mut self, failed: bool) -> Self {
        self.failed = failed;
        self
    }

    pub fn wrapper_event_id(mut self, wrapper_event_id: Option<String>) -> Self {
        self.wrapper_event_id = wrapper_event_id;
        self
    }

    pub fn npub(mut self, npub: Option<String>) -> Self {
        self.npub = npub;
        self
    }

    pub fn build(self) -> StoredEvent {
        StoredEvent {
            id: self.id,
            kind: self.kind,
            chat_id: self.chat_id,
            user_id: self.user_id,
            content: self.content,
            tags: self.tags,
            reference_id: self.reference_id,
            created_at: self.created_at,
            received_at: current_timestamp_ms(),
            mine: self.mine,
            pending: self.pending,
            failed: self.failed,
            wrapper_event_id: self.wrapper_event_id,
            npub: self.npub,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stored_event_new() {
        let event = StoredEvent::new(
            "abc123".to_string(),
            event_kind::PRIVATE_DIRECT_MESSAGE,
            1,
            "Hello world".to_string(),
            1234567890,
        );

        assert_eq!(event.id, "abc123");
        assert_eq!(event.kind, 14);
        assert!(event.is_message());
        assert!(!event.is_reaction());
        assert!(event.is_known_kind());
    }

    #[test]
    fn test_get_tag() {
        let mut event = StoredEvent::new(
            "abc123".to_string(),
            event_kind::PRIVATE_DIRECT_MESSAGE,
            1,
            "Hello".to_string(),
            1234567890,
        );
        event.tags = vec![
            vec!["e".to_string(), "ref123".to_string(), "".to_string(), "reply".to_string()],
            vec!["ms".to_string(), "500".to_string()],
        ];

        assert_eq!(event.get_tag("ms"), Some("500"));
        assert_eq!(event.get_reply_reference(), Some("ref123"));
    }

    #[test]
    fn test_timestamp_ms() {
        let mut event = StoredEvent::new(
            "abc123".to_string(),
            event_kind::PRIVATE_DIRECT_MESSAGE,
            1,
            "Hello".to_string(),
            1234567890,
        );

        // Without ms tag
        assert_eq!(event.timestamp_ms(), 1234567890000);

        // With ms tag
        event.tags = vec![vec!["ms".to_string(), "456".to_string()]];
        assert_eq!(event.timestamp_ms(), 1234567890456);
    }

    #[test]
    fn test_unknown_kind() {
        let event = StoredEvent::new(
            "abc123".to_string(),
            65535, // Unknown kind (max u16 value)
            1,
            "Unknown content".to_string(),
            1234567890,
        );

        assert!(!event.is_message());
        assert!(!event.is_reaction());
        assert!(!event.is_known_kind());
    }

    #[test]
    fn test_builder() {
        let event = StoredEventBuilder::new()
            .id("abc123")
            .kind(event_kind::REACTION)
            .chat_id(1)
            .content("👍")
            .reference_id(Some("msg456".to_string()))
            .mine(true)
            .build();

        assert_eq!(event.id, "abc123");
        assert!(event.is_reaction());
        assert_eq!(event.reference_id, Some("msg456".to_string()));
        assert!(event.mine);
    }
}
