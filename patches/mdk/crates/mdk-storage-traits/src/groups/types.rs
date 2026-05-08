//! Types for the groups module

use std::collections::BTreeSet;
use std::fmt;
use std::str::FromStr;

use crate::messages::types::Message;
use crate::{GroupId, Secret};
use nostr::{EventId, PublicKey, RelayUrl, Timestamp};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::error::GroupError;

/// Tracks whether and when a self-update (key rotation) is needed or was
/// last performed for this group.
///
/// - `Required`: The member must perform a self-update (e.g., after joining
///   via welcome per MIP-02). Maps to `0` in storage.
/// - `CompletedAt(Timestamp)`: The last self-update (or group creation) was
///   at this time. Used for periodic rotation staleness checks (MIP-00).
///   Maps to a non-zero timestamp in storage.
///
/// Every group always has a self-update state — group creators start with
/// `CompletedAt(now)` since creating a group with a fresh key is effectively
/// the first rotation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SelfUpdateState {
    /// A self-update is required (post-join obligation per MIP-02).
    Required,
    /// The last self-update was successfully merged at this timestamp.
    CompletedAt(Timestamp),
}

impl Serialize for SelfUpdateState {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Required => serializer.serialize_u64(0),
            Self::CompletedAt(ts) => serializer.serialize_u64(ts.as_secs()),
        }
    }
}

impl<'de> Deserialize<'de> for SelfUpdateState {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let secs: u64 = u64::deserialize(deserializer)?;
        match secs {
            0 => Ok(Self::Required),
            _ => Ok(Self::CompletedAt(Timestamp::from_secs(secs))),
        }
    }
}

/// The state of the group, this matches the MLS group state
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum GroupState {
    /// The group is active
    Active,
    /// The group is inactive, this is used for groups that users have left or for welcome messages that have been declined
    Inactive,
    /// The group is pending, this is used for groups that users are invited to but haven't joined yet
    Pending,
}

impl fmt::Display for GroupState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl GroupState {
    /// Get as `&str`
    pub fn as_str(&self) -> &str {
        match self {
            Self::Active => "active",
            Self::Inactive => "inactive",
            Self::Pending => "pending",
        }
    }
}

impl FromStr for GroupState {
    type Err = GroupError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "active" => Ok(Self::Active),
            "inactive" => Ok(Self::Inactive),
            "pending" => Ok(Self::Pending),
            _ => Err(GroupError::InvalidParameters(format!(
                "Invalid group state: {}",
                s
            ))),
        }
    }
}

impl Serialize for GroupState {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for GroupState {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s: String = String::deserialize(deserializer)?;
        Self::from_str(&s).map_err(serde::de::Error::custom)
    }
}

/// An MDK group
///
/// Stores metadata about the group
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Group {
    /// This is the MLS group ID, this will serve as the PK in the DB and doesn't change
    pub mls_group_id: GroupId,
    /// This is the group_id used in published Nostr events, it can change over time
    pub nostr_group_id: [u8; 32],
    /// UTF-8 encoded (same value as the NostrGroupDataExtension)
    pub name: String,
    /// UTF-8 encoded (same value as the NostrGroupDataExtension)
    pub description: String,
    /// Hash of the image (same value as the NostrGroupDataExtension)
    pub image_hash: Option<[u8; 32]>,
    /// Secret key of the image
    pub image_key: Option<Secret<[u8; 32]>>,
    /// Nonce used to encrypt the image
    pub image_nonce: Option<Secret<[u8; 12]>>,
    /// Hex encoded (same value as the NostrGroupDataExtension)
    pub admin_pubkeys: BTreeSet<PublicKey>,
    /// Hex encoded Nostr event ID of the last message in the group
    pub last_message_id: Option<EventId>,
    /// Timestamp of the last message in the group (sender's `created_at`)
    pub last_message_at: Option<Timestamp>,
    /// Timestamp when the last message was processed/received by this client
    ///
    /// This is used as a secondary sort key when `last_message_at` values are equal,
    /// matching the `messages()` query ordering (`created_at DESC, processed_at DESC, id DESC`).
    pub last_message_processed_at: Option<Timestamp>,
    /// Epoch of the group
    pub epoch: u64,
    /// The state of the group
    pub state: GroupState,
    /// Self-update (key rotation) tracking state.
    ///
    /// See [`SelfUpdateState`] for the possible values and their meanings.
    pub self_update_state: SelfUpdateState,
}

impl Group {
    /// Updates the group's last-message metadata if `message` should appear
    /// before the current last message in display order.
    ///
    /// Display order is `created_at DESC, processed_at DESC, id DESC`,
    /// matching the [`crate::groups::GroupStorage::messages()`] query.
    ///
    /// Returns `true` if the fields were updated.
    pub fn update_last_message_if_newer(&mut self, message: &Message) -> bool {
        let dominated = match (
            self.last_message_at,
            self.last_message_processed_at,
            self.last_message_id,
        ) {
            // No existing last message — always update.
            (None, _, _) => true,
            // All three fields present — canonical comparison.
            (Some(existing_at), Some(existing_processed_at), Some(existing_id)) => {
                Message::compare_display_keys(
                    message.created_at,
                    message.processed_at,
                    message.id,
                    existing_at,
                    existing_processed_at,
                    existing_id,
                )
                .is_gt()
            }
            // Backfilled data: created_at exists but processed_at is missing.
            // If the new message ties on created_at it wins (it has a real processed_at).
            (Some(existing_at), None, _) => message.created_at >= existing_at,
            // processed_at exists but id is missing (unlikely but safe fallback).
            (Some(existing_at), Some(_), None) => message.created_at > existing_at,
        };

        if dominated {
            self.last_message_at = Some(message.created_at);
            self.last_message_processed_at = Some(message.processed_at);
            self.last_message_id = Some(message.id);
        }
        dominated
    }
}

/// An MDK group relay
///
/// Stores a relay URL and the MLS group ID it belongs to
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct GroupRelay {
    /// The relay URL
    pub relay_url: RelayUrl,
    /// The MLS group ID
    pub mls_group_id: GroupId,
}

/// Exporter secrets for each epoch of a group
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct GroupExporterSecret {
    /// The MLS group ID
    pub mls_group_id: GroupId,
    /// The epoch
    pub epoch: u64,
    /// The secret
    pub secret: Secret<[u8; 32]>,
}

#[cfg(test)]
mod tests {
    use crate::messages::types::MessageState;
    use nostr::{Kind, Tags, UnsignedEvent};
    use serde_json::json;

    use super::*;

    fn make_test_group() -> Group {
        Group {
            mls_group_id: GroupId::from_slice(&[1, 2, 3]),
            nostr_group_id: [0u8; 32],
            name: "Test".to_string(),
            description: String::new(),
            image_hash: None,
            image_key: None,
            image_nonce: None,
            admin_pubkeys: BTreeSet::new(),
            last_message_id: None,
            last_message_at: None,
            last_message_processed_at: None,
            epoch: 0,
            state: GroupState::Active,
            self_update_state: SelfUpdateState::Required,
        }
    }

    fn make_test_message(created_at: u64, processed_at: u64, id_byte: u8) -> Message {
        let pubkey =
            PublicKey::from_hex("8a9de562cbbed225b6ea0118dd3997a02df92c0bffd2224f71081a7450c3e549")
                .unwrap();
        let ca = Timestamp::from(created_at);
        let pa = Timestamp::from(processed_at);
        Message {
            id: EventId::from_slice(&[id_byte; 32]).unwrap(),
            pubkey,
            kind: Kind::from(1u16),
            mls_group_id: GroupId::from_slice(&[1, 2, 3]),
            created_at: ca,
            processed_at: pa,
            content: String::new(),
            tags: Tags::new(),
            event: UnsignedEvent::new(pubkey, ca, Kind::from(1u16), Tags::new(), String::new()),
            wrapper_event_id: EventId::all_zeros(),
            epoch: None,
            state: MessageState::Processed,
        }
    }

    #[test]
    fn test_update_last_message_if_newer_no_previous() {
        let mut group = make_test_group();
        let msg = make_test_message(100, 105, 1);
        assert!(group.update_last_message_if_newer(&msg));
        assert_eq!(group.last_message_at, Some(Timestamp::from(100u64)));
        assert_eq!(
            group.last_message_processed_at,
            Some(Timestamp::from(105u64))
        );
        assert_eq!(group.last_message_id, Some(msg.id));
    }

    #[test]
    fn test_update_last_message_if_newer_newer_created_at_wins() {
        let mut group = make_test_group();
        let old = make_test_message(100, 105, 1);
        group.update_last_message_if_newer(&old);

        let newer = make_test_message(200, 201, 2);
        assert!(group.update_last_message_if_newer(&newer));
        assert_eq!(group.last_message_at, Some(Timestamp::from(200u64)));
    }

    #[test]
    fn test_update_last_message_if_newer_older_created_at_loses() {
        let mut group = make_test_group();
        let current = make_test_message(200, 205, 5);
        group.update_last_message_if_newer(&current);

        // Even though this was processed much later, it has an older created_at
        let older = make_test_message(100, 999, 9);
        assert!(!group.update_last_message_if_newer(&older));
        assert_eq!(group.last_message_at, Some(Timestamp::from(200u64)));
    }

    #[test]
    fn test_update_last_message_if_newer_processed_at_tiebreaker() {
        let mut group = make_test_group();
        // First message: created_at=100, processed right away at t=101
        let first = make_test_message(100, 101, 5);
        group.update_last_message_if_newer(&first);

        // Second message: also created_at=100, but processed later at t=110
        let second = make_test_message(100, 110, 3);
        assert!(group.update_last_message_if_newer(&second));
        assert_eq!(
            group.last_message_processed_at,
            Some(Timestamp::from(110u64))
        );
        assert_eq!(group.last_message_id, Some(second.id));
    }

    #[test]
    fn test_update_last_message_if_newer_id_tiebreaker() {
        let mut group = make_test_group();
        let first = make_test_message(100, 105, 1);
        group.update_last_message_if_newer(&first);

        // Same created_at and processed_at, larger id wins
        let second = make_test_message(100, 105, 5);
        assert!(group.update_last_message_if_newer(&second));
        assert_eq!(group.last_message_id, Some(second.id));
    }

    #[test]
    fn test_update_last_message_if_newer_backfilled_data() {
        // Simulates a group upgraded from before processed_at existed (has created_at but
        // no processed_at). A new message with the same created_at should win because it
        // has a real processed_at.
        let mut group = make_test_group();
        group.last_message_at = Some(Timestamp::from(100u64));
        group.last_message_id = Some(EventId::from_slice(&[1u8; 32]).unwrap());
        // processed_at is None (backfilled)

        let msg = make_test_message(100, 105, 2);
        assert!(
            group.update_last_message_if_newer(&msg),
            "Should update when processed_at was missing (backfilled data)"
        );
        assert_eq!(
            group.last_message_processed_at,
            Some(Timestamp::from(105u64))
        );
    }

    #[test]
    fn test_update_last_message_review_scenario() {
        // Scenario from PR review by erskingardner:
        // Message A: created_at=100, processed_at=101, id=5
        // Message B: created_at=100, processed_at=102, id=3
        // B should win because processed_at=102 > processed_at=101
        let mut group = make_test_group();
        let msg_a = make_test_message(100, 101, 5);
        group.update_last_message_if_newer(&msg_a);

        let msg_b = make_test_message(100, 102, 3);
        assert!(
            group.update_last_message_if_newer(&msg_b),
            "Message B should win: higher processed_at"
        );
        assert_eq!(group.last_message_id, Some(msg_b.id));
    }

    #[test]
    fn test_group_state_from_str() {
        assert_eq!(GroupState::from_str("active").unwrap(), GroupState::Active);
        assert_eq!(
            GroupState::from_str("inactive").unwrap(),
            GroupState::Inactive
        );

        let err = GroupState::from_str("invalid").unwrap_err();
        match err {
            GroupError::InvalidParameters(msg) => {
                assert!(msg.contains("Invalid group state: invalid"));
            }
            _ => panic!("Expected InvalidParameters error"),
        }
    }

    #[test]
    fn test_group_state_to_string() {
        assert_eq!(GroupState::Active.to_string(), "active");
        assert_eq!(GroupState::Inactive.to_string(), "inactive");
    }

    #[test]
    fn test_group_state_serialization() {
        let active = GroupState::Active;
        let serialized = serde_json::to_string(&active).unwrap();
        assert_eq!(serialized, r#""active""#);

        let inactive = GroupState::Inactive;
        let serialized = serde_json::to_string(&inactive).unwrap();
        assert_eq!(serialized, r#""inactive""#);
    }

    #[test]
    fn test_group_state_deserialization() {
        let active: GroupState = serde_json::from_str(r#""active""#).unwrap();
        assert_eq!(active, GroupState::Active);

        let inactive: GroupState = serde_json::from_str(r#""inactive""#).unwrap();
        assert_eq!(inactive, GroupState::Inactive);
    }

    #[test]
    fn test_group_serialization() {
        // Simple test to ensure Group can be serialized
        let group = Group {
            mls_group_id: GroupId::from_slice(&[1, 2, 3]),
            nostr_group_id: [0u8; 32],
            name: "Test Group".to_string(),
            description: "Test Description".to_string(),
            image_hash: None,
            image_key: None,
            image_nonce: None,
            admin_pubkeys: BTreeSet::new(),
            last_message_id: None,
            last_message_at: None,
            last_message_processed_at: None,
            epoch: 0,
            state: GroupState::Active,
            self_update_state: SelfUpdateState::Required,
        };

        let serialized = serde_json::to_value(&group).unwrap();
        assert_eq!(serialized["mls_group_id"]["value"]["vec"], json!([1, 2, 3]));
        assert_eq!(
            serialized["nostr_group_id"],
            json!([
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0
            ])
        );
        assert_eq!(serialized["name"], json!("Test Group"));
        assert_eq!(serialized["description"], json!("Test Description"));
        assert_eq!(serialized["state"], json!("active"));
    }

    #[test]
    fn test_group_exporter_secret_serialization() {
        let secret = GroupExporterSecret {
            mls_group_id: GroupId::from_slice(&[1, 2, 3]),
            epoch: 42,
            secret: Secret::new([0u8; 32]),
        };

        let serialized = serde_json::to_value(&secret).unwrap();
        assert_eq!(serialized["mls_group_id"]["value"]["vec"], json!([1, 2, 3]));
        assert_eq!(serialized["epoch"], json!(42));
        assert_eq!(
            serialized["secret"],
            json!([
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0
            ])
        );

        // Test deserialization
        let deserialized: GroupExporterSecret = serde_json::from_value(serialized).unwrap();
        assert_eq!(deserialized.epoch, 42);
        assert_eq!(*deserialized.secret, [0u8; 32]);
    }

    #[test]
    fn test_group_relay_serialization() {
        let relay = GroupRelay {
            relay_url: RelayUrl::from_str("wss://relay.example.com").unwrap(),
            mls_group_id: GroupId::from_slice(&[1, 2, 3]),
        };

        let serialized = serde_json::to_value(&relay).unwrap();
        assert_eq!(serialized["relay_url"], json!("wss://relay.example.com"));
        assert_eq!(serialized["mls_group_id"]["value"]["vec"], json!([1, 2, 3]));

        // Test deserialization
        let deserialized: GroupRelay = serde_json::from_value(serialized).unwrap();
        assert_eq!(
            deserialized.relay_url.to_string(),
            "wss://relay.example.com"
        );
    }

    #[test]
    fn test_self_update_state_serde_roundtrip() {
        // Required serializes to 0
        let val = serde_json::to_value(SelfUpdateState::Required).unwrap();
        assert_eq!(val, json!(0));
        let rt: SelfUpdateState = serde_json::from_value(val).unwrap();
        assert_eq!(rt, SelfUpdateState::Required);

        // CompletedAt serializes to the timestamp seconds
        let ts = Timestamp::from_secs(1_700_000_000);
        let val = serde_json::to_value(SelfUpdateState::CompletedAt(ts)).unwrap();
        assert_eq!(val, json!(1_700_000_000));
        let rt: SelfUpdateState = serde_json::from_value(val).unwrap();
        assert_eq!(rt, SelfUpdateState::CompletedAt(ts));
    }
}
