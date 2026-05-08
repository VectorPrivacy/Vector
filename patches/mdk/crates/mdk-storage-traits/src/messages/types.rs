//! Types for the messages module

use std::cmp::Ordering;
use std::fmt;
use std::str::FromStr;

use crate::GroupId;
#[allow(unused_imports)] // Referenced in doc links
use crate::groups::types::Group;
use nostr::event::Kind;
use nostr::{EventId, PublicKey, Tags, Timestamp, UnsignedEvent};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::error::MessageError;

/// A processed message, this stores data about whether we have processed a message or not
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ProcessedMessage {
    /// The event id of the processed message
    pub wrapper_event_id: EventId,
    /// The event id of the rumor event (kind 445 group message)
    pub message_event_id: Option<EventId>,
    /// The timestamp of when the message was processed
    pub processed_at: Timestamp,
    /// The epoch when this message was processed (None for backward compatibility)
    pub epoch: Option<u64>,
    /// The MLS group ID this message belongs to (for epoch-scoped queries)
    pub mls_group_id: Option<GroupId>,
    /// The state of the message
    pub state: ProcessedMessageState,
    /// The reason the message failed to be processed
    pub failure_reason: Option<String>,
}

/// This is the processed rumor message that represents a message in a group
/// We store the deconstructed messages but also the UnsignedEvent.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Message {
    /// The event id of the message
    pub id: EventId,
    /// The pubkey of the author of the message
    pub pubkey: PublicKey,
    /// The kind of the message
    pub kind: Kind,
    /// The MLS group id of the message
    pub mls_group_id: GroupId,
    /// The created at timestamp of the message (from the rumor event)
    pub created_at: Timestamp,
    /// The timestamp when this message was processed/received by this client.
    /// This is useful for clients that want to display messages in the order
    /// they were received locally, rather than in the order they were created
    /// (which may differ due to clock skew between devices).
    pub processed_at: Timestamp,
    /// The content of the message
    pub content: String,
    /// The tags of the message
    pub tags: Tags,
    /// The event that contains the message
    pub event: UnsignedEvent,
    /// The event id of the 1059 event that contained the message
    pub wrapper_event_id: EventId,
    /// The epoch when this message was decrypted/processed (None for backward compatibility)
    pub epoch: Option<u64>,
    /// The state of the message
    pub state: MessageState,
}

impl Message {
    /// Compares two messages for display ordering.
    ///
    /// Messages are sorted in descending order by:
    /// 1. `created_at` (newest first)
    /// 2. `processed_at` (most recently processed first, as a tiebreaker)
    /// 3. `id` (largest ID first, for deterministic ordering)
    ///
    /// This ordering is the single source of truth used by all storage
    /// implementations (both in-memory and SQLite) and by the last-message
    /// update logic in `mdk-core`.
    ///
    /// Returns [`Ordering::Greater`] if `self` should appear **before** `other`
    /// in a newest-first list.
    pub fn display_order_cmp(&self, other: &Self) -> Ordering {
        Self::compare_display_keys(
            self.created_at,
            self.processed_at,
            self.id,
            other.created_at,
            other.processed_at,
            other.id,
        )
    }

    /// Compares display-order keys without requiring full [`Message`] structs.
    ///
    /// This is useful when the caller only has the raw fields (e.g. from
    /// [`crate::groups::types::Group::last_message_at`] / [`crate::groups::types::Group::last_message_processed_at`] /
    /// [`crate::groups::types::Group::last_message_id`]).
    ///
    /// Returns [`Ordering::Greater`] when the first set of keys (`a_*`) should
    /// appear **before** the second set (`b_*`) in a newest-first list.
    pub fn compare_display_keys(
        a_created_at: Timestamp,
        a_processed_at: Timestamp,
        a_id: EventId,
        b_created_at: Timestamp,
        b_processed_at: Timestamp,
        b_id: EventId,
    ) -> Ordering {
        a_created_at
            .cmp(&b_created_at)
            .then_with(|| a_processed_at.cmp(&b_processed_at))
            .then_with(|| a_id.cmp(&b_id))
    }

    /// Compares two messages for processed-at-first ordering.
    ///
    /// Messages are sorted in descending order by:
    /// 1. `processed_at` (most recently received first)
    /// 2. `created_at` (newest first, as a tiebreaker)
    /// 3. `id` (largest ID first, for deterministic ordering)
    ///
    /// This ordering prioritises local reception time, making it useful
    /// when clients want to avoid visual reordering caused by clock skew
    /// between senders.
    ///
    /// Returns [`Ordering::Greater`] if `self` should appear **before** `other`
    /// in a newest-first list.
    pub fn processed_at_order_cmp(&self, other: &Self) -> Ordering {
        Self::compare_processed_at_keys(
            self.processed_at,
            self.created_at,
            self.id,
            other.processed_at,
            other.created_at,
            other.id,
        )
    }

    /// Compares processed-at-first ordering keys without requiring full [`Message`] structs.
    ///
    /// Returns [`Ordering::Greater`] when the first set of keys (`a_*`) should
    /// appear **before** the second set (`b_*`) in a newest-first list.
    pub fn compare_processed_at_keys(
        a_processed_at: Timestamp,
        a_created_at: Timestamp,
        a_id: EventId,
        b_processed_at: Timestamp,
        b_created_at: Timestamp,
        b_id: EventId,
    ) -> Ordering {
        a_processed_at
            .cmp(&b_processed_at)
            .then_with(|| a_created_at.cmp(&b_created_at))
            .then_with(|| a_id.cmp(&b_id))
    }
}

/// The state of the message
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MessageState {
    /// The message was created successfully and stored but we don't yet know if it was published to relays.
    Created,
    /// The message was successfully processed and stored in the database
    Processed,
    /// The message was deleted by the original sender - via a delete event
    Deleted,
    /// The epoch was rolled back, content may be invalid and needs reprocessing
    EpochInvalidated,
}

impl fmt::Display for MessageState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl MessageState {
    /// Get as `&str`
    pub fn as_str(&self) -> &str {
        match self {
            Self::Created => "created",
            Self::Processed => "processed",
            Self::Deleted => "deleted",
            Self::EpochInvalidated => "epoch_invalidated",
        }
    }
}

impl FromStr for MessageState {
    type Err = MessageError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "created" => Ok(Self::Created),
            "processed" => Ok(Self::Processed),
            "deleted" => Ok(Self::Deleted),
            "epoch_invalidated" => Ok(Self::EpochInvalidated),
            _ => Err(MessageError::InvalidParameters(format!(
                "Invalid message state: {}",
                s
            ))),
        }
    }
}

impl Serialize for MessageState {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for MessageState {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s: String = String::deserialize(deserializer)?;
        Self::from_str(&s).map_err(serde::de::Error::custom)
    }
}

/// The Processing State of the message,
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ProcessedMessageState {
    /// The processed message (and message) was created successfully and stored but we don't yet know if it was published to relays.
    /// This state only happens when you are sending a message. Since we can't decrypt messages from ourselves in MLS groups,
    /// once we see this message we mark it as processed but skip the rest of the processing.
    Created,
    /// The message was successfully processed and stored in the database
    Processed,
    /// The message was a commit message and we have already processed it. We can't decrypt messages from ourselves in MLS groups so we need to skip this processing.
    ProcessedCommit,
    /// The message failed to be processed and stored in the database
    Failed,
    /// The epoch was rolled back, message needs reprocessing
    EpochInvalidated,
    /// The message previously failed but is now eligible for retry after a rollback.
    /// This state is set by the rollback flow when group state has been corrected,
    /// allowing messages that failed due to stale epoch keys to be reprocessed.
    Retryable,
}

impl fmt::Display for ProcessedMessageState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl ProcessedMessageState {
    /// Get as `&str`
    pub fn as_str(&self) -> &str {
        match self {
            Self::Created => "created",
            Self::Processed => "processed",
            Self::ProcessedCommit => "processed_commit",
            Self::Failed => "failed",
            Self::EpochInvalidated => "epoch_invalidated",
            Self::Retryable => "retryable",
        }
    }
}

impl FromStr for ProcessedMessageState {
    type Err = MessageError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "created" => Ok(Self::Created),
            "processed" => Ok(Self::Processed),
            "processed_commit" => Ok(Self::ProcessedCommit),
            "failed" => Ok(Self::Failed),
            "epoch_invalidated" => Ok(Self::EpochInvalidated),
            "retryable" => Ok(Self::Retryable),
            _ => Err(MessageError::InvalidParameters(format!(
                "Invalid processed message state: {}",
                s
            ))),
        }
    }
}

impl Serialize for ProcessedMessageState {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ProcessedMessageState {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s: String = String::deserialize(deserializer)?;
        Self::from_str(&s).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use std::cmp::Ordering;

    use serde_json::json;

    use super::*;

    #[test]
    fn test_compare_display_keys_created_at_wins() {
        let id_a = EventId::from_slice(&[1u8; 32]).unwrap();
        let id_b = EventId::from_slice(&[2u8; 32]).unwrap();

        // Message a: created at t=200, processed at t=201
        // Message b: created at t=100, processed at t=300 (received much later)
        // a wins because created_at is the primary sort key
        let result = Message::compare_display_keys(
            Timestamp::from(200u64),
            Timestamp::from(201u64),
            id_a,
            Timestamp::from(100u64),
            Timestamp::from(300u64),
            id_b,
        );
        assert_eq!(result, Ordering::Greater);
    }

    #[test]
    fn test_compare_display_keys_processed_at_tiebreaker() {
        let id_a = EventId::from_slice(&[1u8; 32]).unwrap();
        let id_b = EventId::from_slice(&[2u8; 32]).unwrap();

        // Both created at t=100, but a was processed later (t=120 vs t=105)
        let result = Message::compare_display_keys(
            Timestamp::from(100u64),
            Timestamp::from(120u64),
            id_a,
            Timestamp::from(100u64),
            Timestamp::from(105u64),
            id_b,
        );
        assert_eq!(result, Ordering::Greater);
    }

    #[test]
    fn test_compare_display_keys_id_tiebreaker() {
        let id_small = EventId::from_slice(&[1u8; 32]).unwrap();
        let id_large = EventId::from_slice(&[2u8; 32]).unwrap();

        // Same created_at and processed_at, larger id wins
        let result = Message::compare_display_keys(
            Timestamp::from(100u64),
            Timestamp::from(105u64),
            id_large,
            Timestamp::from(100u64),
            Timestamp::from(105u64),
            id_small,
        );
        assert_eq!(result, Ordering::Greater);
    }

    #[test]
    fn test_compare_display_keys_equal() {
        let id = EventId::from_slice(&[1u8; 32]).unwrap();

        let result = Message::compare_display_keys(
            Timestamp::from(100u64),
            Timestamp::from(105u64),
            id,
            Timestamp::from(100u64),
            Timestamp::from(105u64),
            id,
        );
        assert_eq!(result, Ordering::Equal);
    }

    #[test]
    fn test_compare_display_keys_scenario_from_review() {
        // Scenario from PR review comment by erskingardner:
        // Message A: created_at=100, processed_at=101, id=5
        // Message B: created_at=100, processed_at=102, id=3
        // B should win because processed_at=102 > processed_at=101
        let id_a = EventId::from_slice(&[5u8; 32]).unwrap();
        let id_b = EventId::from_slice(&[3u8; 32]).unwrap();

        let result = Message::compare_display_keys(
            Timestamp::from(100u64),
            Timestamp::from(101u64),
            id_a,
            Timestamp::from(100u64),
            Timestamp::from(102u64),
            id_b,
        );
        assert_eq!(
            result,
            Ordering::Less,
            "Message B should win: same created_at but higher processed_at"
        );
    }

    #[test]
    fn test_compare_processed_at_keys_processed_at_wins() {
        let id_a = EventId::from_slice(&[1u8; 32]).unwrap();
        let id_b = EventId::from_slice(&[2u8; 32]).unwrap();

        // Message a: processed at t=300, created at t=100
        // Message b: processed at t=200, created at t=200
        // a wins because processed_at is the primary sort key
        let result = Message::compare_processed_at_keys(
            Timestamp::from(300u64),
            Timestamp::from(100u64),
            id_a,
            Timestamp::from(200u64),
            Timestamp::from(200u64),
            id_b,
        );
        assert_eq!(result, Ordering::Greater);
    }

    #[test]
    fn test_compare_processed_at_keys_created_at_tiebreaker() {
        let id_a = EventId::from_slice(&[1u8; 32]).unwrap();
        let id_b = EventId::from_slice(&[2u8; 32]).unwrap();

        // Both processed at t=100, but a was created later (t=120 vs t=105)
        let result = Message::compare_processed_at_keys(
            Timestamp::from(100u64),
            Timestamp::from(120u64),
            id_a,
            Timestamp::from(100u64),
            Timestamp::from(105u64),
            id_b,
        );
        assert_eq!(result, Ordering::Greater);
    }

    #[test]
    fn test_compare_processed_at_keys_id_tiebreaker() {
        let id_small = EventId::from_slice(&[1u8; 32]).unwrap();
        let id_large = EventId::from_slice(&[2u8; 32]).unwrap();

        // Same processed_at and created_at, larger id wins
        let result = Message::compare_processed_at_keys(
            Timestamp::from(100u64),
            Timestamp::from(105u64),
            id_large,
            Timestamp::from(100u64),
            Timestamp::from(105u64),
            id_small,
        );
        assert_eq!(result, Ordering::Greater);
    }

    #[test]
    fn test_compare_processed_at_keys_equal() {
        let id = EventId::from_slice(&[1u8; 32]).unwrap();

        let result = Message::compare_processed_at_keys(
            Timestamp::from(100u64),
            Timestamp::from(105u64),
            id,
            Timestamp::from(100u64),
            Timestamp::from(105u64),
            id,
        );
        assert_eq!(result, Ordering::Equal);
    }

    #[test]
    fn test_compare_processed_at_keys_ignores_created_at_when_processed_at_differs() {
        // Scenario: Message A has higher created_at but lower processed_at
        // In processed_at-first ordering, B should win because it was processed later
        let id_a = EventId::from_slice(&[5u8; 32]).unwrap();
        let id_b = EventId::from_slice(&[3u8; 32]).unwrap();

        let result = Message::compare_processed_at_keys(
            Timestamp::from(100u64), // a processed_at
            Timestamp::from(200u64), // a created_at (higher!)
            id_a,
            Timestamp::from(150u64), // b processed_at (higher)
            Timestamp::from(50u64),  // b created_at (lower)
            id_b,
        );
        assert_eq!(
            result,
            Ordering::Less,
            "Message B should win: higher processed_at despite lower created_at"
        );
    }

    #[test]
    fn test_message_state_from_str() {
        assert_eq!(
            MessageState::from_str("created").unwrap(),
            MessageState::Created
        );
        assert_eq!(
            MessageState::from_str("processed").unwrap(),
            MessageState::Processed
        );
        assert_eq!(
            MessageState::from_str("deleted").unwrap(),
            MessageState::Deleted
        );
        assert_eq!(
            MessageState::from_str("epoch_invalidated").unwrap(),
            MessageState::EpochInvalidated
        );

        let err = MessageState::from_str("invalid").unwrap_err();
        match err {
            MessageError::InvalidParameters(msg) => {
                assert!(msg.contains("Invalid message state: invalid"));
            }
            _ => panic!("Expected InvalidParameters error"),
        }
    }

    #[test]
    fn test_message_state_to_string() {
        assert_eq!(MessageState::Created.to_string(), "created");
        assert_eq!(MessageState::Processed.to_string(), "processed");
        assert_eq!(MessageState::Deleted.to_string(), "deleted");
        assert_eq!(
            MessageState::EpochInvalidated.to_string(),
            "epoch_invalidated"
        );
    }

    #[test]
    fn test_message_state_serialization() {
        let created = MessageState::Created;
        let serialized = serde_json::to_string(&created).unwrap();
        assert_eq!(serialized, r#""created""#);

        let processed = MessageState::Processed;
        let serialized = serde_json::to_string(&processed).unwrap();
        assert_eq!(serialized, r#""processed""#);

        let deleted = MessageState::Deleted;
        let serialized = serde_json::to_string(&deleted).unwrap();
        assert_eq!(serialized, r#""deleted""#);

        let epoch_invalidated = MessageState::EpochInvalidated;
        let serialized = serde_json::to_string(&epoch_invalidated).unwrap();
        assert_eq!(serialized, r#""epoch_invalidated""#);
    }

    #[test]
    fn test_message_state_deserialization() {
        let created: MessageState = serde_json::from_str(r#""created""#).unwrap();
        assert_eq!(created, MessageState::Created);

        let processed: MessageState = serde_json::from_str(r#""processed""#).unwrap();
        assert_eq!(processed, MessageState::Processed);

        let deleted: MessageState = serde_json::from_str(r#""deleted""#).unwrap();
        assert_eq!(deleted, MessageState::Deleted);

        let epoch_invalidated: MessageState =
            serde_json::from_str(r#""epoch_invalidated""#).unwrap();
        assert_eq!(epoch_invalidated, MessageState::EpochInvalidated);

        // Test invalid state
        let result = serde_json::from_str::<MessageState>(r#""invalid""#);
        assert!(result.is_err());
    }

    #[test]
    fn test_message_serialization() {
        // Create a message to test serialization
        let pubkey =
            PublicKey::from_hex("8a9de562cbbed225b6ea0118dd3997a02df92c0bffd2224f71081a7450c3e549")
                .unwrap();
        let now = Timestamp::now();
        let message = Message {
            id: EventId::all_zeros(),
            pubkey,
            kind: Kind::MlsGroupMessage,
            mls_group_id: GroupId::from_slice(&[1, 2, 3, 4]),
            created_at: now,
            processed_at: now,
            content: "Test message".to_string(),
            tags: Tags::new(),
            event: UnsignedEvent::new(
                pubkey,
                now,
                Kind::MlsGroupMessage,
                Tags::new(),
                "Test message".to_string(),
            ),
            wrapper_event_id: EventId::all_zeros(),
            epoch: Some(5),
            state: MessageState::Created,
        };

        let serialized = serde_json::to_value(&message).unwrap();
        assert_eq!(serialized["state"], json!("created"));
        assert_eq!(serialized["content"], json!("Test message"));
        assert_eq!(serialized["epoch"], json!(5));
    }

    #[test]
    fn test_processed_message_state_from_str() {
        assert_eq!(
            ProcessedMessageState::from_str("created").unwrap(),
            ProcessedMessageState::Created
        );
        assert_eq!(
            ProcessedMessageState::from_str("processed").unwrap(),
            ProcessedMessageState::Processed
        );
        assert_eq!(
            ProcessedMessageState::from_str("processed_commit").unwrap(),
            ProcessedMessageState::ProcessedCommit
        );
        assert_eq!(
            ProcessedMessageState::from_str("failed").unwrap(),
            ProcessedMessageState::Failed
        );
        assert_eq!(
            ProcessedMessageState::from_str("epoch_invalidated").unwrap(),
            ProcessedMessageState::EpochInvalidated
        );
        assert_eq!(
            ProcessedMessageState::from_str("retryable").unwrap(),
            ProcessedMessageState::Retryable
        );

        let err = ProcessedMessageState::from_str("invalid").unwrap_err();
        match err {
            MessageError::InvalidParameters(msg) => {
                assert!(msg.contains("Invalid processed message state: invalid"));
            }
            _ => panic!("Expected InvalidParameters error"),
        }
    }

    #[test]
    fn test_processed_message_state_to_string() {
        assert_eq!(ProcessedMessageState::Created.to_string(), "created");
        assert_eq!(ProcessedMessageState::Processed.to_string(), "processed");
        assert_eq!(
            ProcessedMessageState::ProcessedCommit.to_string(),
            "processed_commit"
        );
        assert_eq!(ProcessedMessageState::Failed.to_string(), "failed");
        assert_eq!(
            ProcessedMessageState::EpochInvalidated.to_string(),
            "epoch_invalidated"
        );
        assert_eq!(ProcessedMessageState::Retryable.to_string(), "retryable");
    }

    #[test]
    fn test_processed_message_state_serialization() {
        let created = ProcessedMessageState::Created;
        let serialized = serde_json::to_string(&created).unwrap();
        assert_eq!(serialized, r#""created""#);

        let processed = ProcessedMessageState::Processed;
        let serialized = serde_json::to_string(&processed).unwrap();
        assert_eq!(serialized, r#""processed""#);

        let processed_commit = ProcessedMessageState::ProcessedCommit;
        let serialized = serde_json::to_string(&processed_commit).unwrap();
        assert_eq!(serialized, r#""processed_commit""#);

        let failed = ProcessedMessageState::Failed;
        let serialized = serde_json::to_string(&failed).unwrap();
        assert_eq!(serialized, r#""failed""#);

        let epoch_invalidated = ProcessedMessageState::EpochInvalidated;
        let serialized = serde_json::to_string(&epoch_invalidated).unwrap();
        assert_eq!(serialized, r#""epoch_invalidated""#);

        let retryable = ProcessedMessageState::Retryable;
        let serialized = serde_json::to_string(&retryable).unwrap();
        assert_eq!(serialized, r#""retryable""#);
    }

    #[test]
    fn test_processed_message_state_deserialization() {
        let created: ProcessedMessageState = serde_json::from_str(r#""created""#).unwrap();
        assert_eq!(created, ProcessedMessageState::Created);

        let processed: ProcessedMessageState = serde_json::from_str(r#""processed""#).unwrap();
        assert_eq!(processed, ProcessedMessageState::Processed);

        let processed_commit: ProcessedMessageState =
            serde_json::from_str(r#""processed_commit""#).unwrap();
        assert_eq!(processed_commit, ProcessedMessageState::ProcessedCommit);

        let failed: ProcessedMessageState = serde_json::from_str(r#""failed""#).unwrap();
        assert_eq!(failed, ProcessedMessageState::Failed);

        let epoch_invalidated: ProcessedMessageState =
            serde_json::from_str(r#""epoch_invalidated""#).unwrap();
        assert_eq!(epoch_invalidated, ProcessedMessageState::EpochInvalidated);

        let retryable: ProcessedMessageState = serde_json::from_str(r#""retryable""#).unwrap();
        assert_eq!(retryable, ProcessedMessageState::Retryable);

        // Test invalid state
        let result = serde_json::from_str::<ProcessedMessageState>(r#""invalid""#);
        assert!(result.is_err());
    }

    #[test]
    fn test_processed_message_serialization() {
        // Create a processed message to test serialization
        let processed_message = ProcessedMessage {
            wrapper_event_id: EventId::all_zeros(),
            message_event_id: None,
            processed_at: Timestamp::now(),
            epoch: Some(5),
            mls_group_id: Some(GroupId::from_slice(&[1, 2, 3, 4])),
            state: ProcessedMessageState::Processed,
            failure_reason: None,
        };

        let serialized = serde_json::to_value(&processed_message).unwrap();
        assert_eq!(serialized["state"], json!("processed"));
        assert_eq!(serialized["failure_reason"], json!(null));
        assert_eq!(serialized["epoch"], json!(5));

        // Create a failed message with a reason
        let failed_message = ProcessedMessage {
            wrapper_event_id: EventId::all_zeros(),
            message_event_id: Some(EventId::all_zeros()),
            processed_at: Timestamp::now(),
            epoch: None,
            mls_group_id: None,
            state: ProcessedMessageState::Failed,
            failure_reason: Some("Decryption failed".to_string()),
        };

        let serialized = serde_json::to_value(&failed_message).unwrap();
        assert_eq!(serialized["state"], json!("failed"));
        assert_eq!(serialized["failure_reason"], json!("Decryption failed"));
        assert!(serialized["message_event_id"].is_string());
    }

    #[test]
    fn test_processed_message_deserialization() {
        // Test with epoch set and mls_group_id null
        let json_str = r#"{
            "wrapper_event_id": "0000000000000000000000000000000000000000000000000000000000000000",
            "message_event_id": null,
            "processed_at": 1677721600,
            "epoch": 5,
            "mls_group_id": null,
            "state": "processed",
            "failure_reason": null
        }"#;

        let processed_message: ProcessedMessage = serde_json::from_str(json_str).unwrap();
        assert_eq!(processed_message.state, ProcessedMessageState::Processed);
        assert_eq!(processed_message.failure_reason, None);
        assert_eq!(processed_message.epoch, Some(5));
        assert_eq!(processed_message.mls_group_id, None);

        // Test with failed state and all optional fields null
        let json_str = r#"{
            "wrapper_event_id": "0000000000000000000000000000000000000000000000000000000000000000",
            "message_event_id": "0000000000000000000000000000000000000000000000000000000000000000",
            "processed_at": 1677721600,
            "epoch": null,
            "mls_group_id": null,
            "state": "failed",
            "failure_reason": "Decryption failed"
        }"#;

        let failed_message: ProcessedMessage = serde_json::from_str(json_str).unwrap();
        assert_eq!(failed_message.state, ProcessedMessageState::Failed);
        assert_eq!(
            failed_message.failure_reason,
            Some("Decryption failed".to_string())
        );
        assert_eq!(failed_message.epoch, None);
        assert_eq!(failed_message.mls_group_id, None);
    }
}
