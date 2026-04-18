//! MLS types, errors, and desync tracking.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tokio::sync::Mutex as TokioMutex;
use std::sync::LazyLock;

// ============================================================================
// MLS Group Types — 3-struct split
// ============================================================================

/// Protocol identity + state. What the MLS engine and relay protocol need.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MlsGroup {
    /// Wire identifier used on the relay (wrapper 'h' tag). UI lists this value.
    pub group_id: String,
    /// Engine identifier used locally by nostr-mls for group state lookups.
    #[serde(default)]
    pub engine_group_id: String,
    pub creator_pubkey: String,
    pub created_at: u64,
    pub updated_at: u64,
    /// Flag indicating if we were evicted/kicked from this group.
    /// When true, we skip syncing this group (unless it's a new welcome/invite).
    #[serde(default)]
    pub evicted: bool,
}

/// Display info. What the UI needs to render the group.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MlsGroupProfile {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub avatar_ref: Option<String>,
    #[serde(default)]
    pub avatar_cached: Option<String>,
}

/// Combined group metadata — used for DB serialization and frontend.
/// Replaces the old monolithic `MlsGroupMetadata`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MlsGroupFull {
    #[serde(flatten)]
    pub group: MlsGroup,
    #[serde(flatten)]
    pub profile: MlsGroupProfile,
}

impl std::ops::Deref for MlsGroupFull {
    type Target = MlsGroup;
    fn deref(&self) -> &MlsGroup { &self.group }
}

impl std::ops::DerefMut for MlsGroupFull {
    fn deref_mut(&mut self) -> &mut MlsGroup { &mut self.group }
}

/// Convert group metadata to frontend-friendly JSON (seconds → milliseconds).
pub fn metadata_to_frontend(meta: &MlsGroupFull) -> serde_json::Value {
    serde_json::json!({
        "group_id": meta.group.group_id,
        "engine_group_id": meta.group.engine_group_id,
        "creator_pubkey": meta.group.creator_pubkey,
        "name": meta.profile.name,
        "description": meta.profile.description,
        "avatar_ref": meta.profile.avatar_ref,
        "avatar_cached": meta.profile.avatar_cached,
        "created_at": meta.group.created_at.saturating_mul(1000),
        "updated_at": meta.group.updated_at.saturating_mul(1000),
        "evicted": meta.group.evicted,
    })
}

// ============================================================================
// Supporting Types
// ============================================================================

/// Keypackage index entry stored in "mls_keypackages"
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyPackageIndexEntry {
    pub owner_pubkey: String,
    pub device_id: String,
    pub keypackage_ref: String,
    pub fetched_at: u64,
    pub expires_at: u64,
}

/// Event cursor tracking for a group stored in "mls_event_cursors"
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventCursor {
    pub last_seen_event_id: String,
    pub last_seen_at: u64,
}

// ============================================================================
// MLS Error
// ============================================================================

#[derive(Debug)]
pub enum MlsError {
    NotInitialized,
    InvalidGroupId,
    InvalidKeyPackage,
    GroupNotFound,
    MemberNotFound,
    OutdatedKeyPackage(String),
    StorageError(String),
    NetworkError(String),
    CryptoError(String),
    NostrMlsError(String),
}

impl std::fmt::Display for MlsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MlsError::NotInitialized => write!(f, "MLS service not initialized"),
            MlsError::InvalidGroupId => write!(f, "Invalid group ID"),
            MlsError::InvalidKeyPackage => write!(f, "Invalid key package"),
            MlsError::GroupNotFound => write!(f, "Group not found"),
            MlsError::MemberNotFound => write!(f, "Member not found"),
            MlsError::OutdatedKeyPackage(name) => write!(
                f,
                "{} has an outdated keypackage. They need to update their app and reconnect.",
                name
            ),
            MlsError::StorageError(e) => write!(f, "Storage error: {}", e),
            MlsError::NetworkError(e) => write!(f, "Network error: {}", e),
            MlsError::CryptoError(e) => write!(f, "Crypto error: {}", e),
            MlsError::NostrMlsError(e) => write!(f, "Nostr MLS error: {}", e),
        }
    }
}

impl std::error::Error for MlsError {}

// ============================================================================
// Utility
// ============================================================================

/// Check if a keypackage event has the required encoding tag (MIP-00/MIP-02).
pub fn has_encoding_tag(event: &nostr_sdk::Event) -> bool {
    event.tags.iter().any(|tag| {
        let slice = tag.as_slice();
        slice.len() >= 2 && slice[0] == "encoding" && slice[1] == "base64"
    })
}

// ============================================================================
// Desync Tracking
// ============================================================================

/// Tracks consecutive processing failures per group for desync detection.
static GROUP_FAILURE_COUNTS: LazyLock<TokioMutex<HashMap<String, u32>>> =
    LazyLock::new(|| TokioMutex::new(HashMap::new()));

/// Threshold for consecutive failures before considering a group desynced.
const DESYNC_FAILURE_THRESHOLD: u32 = 10;

/// Record a processing failure for a group and check if it's likely desynced.
/// Returns true if the group appears to be desynced (threshold exceeded).
pub async fn record_group_failure(group_id: &str) -> bool {
    let mut counts = GROUP_FAILURE_COUNTS.lock().await;
    let count = counts.entry(group_id.to_string()).or_insert(0);
    *count += 1;
    *count >= DESYNC_FAILURE_THRESHOLD
}

/// Record a successful processing for a group (resets failure count).
pub async fn record_group_success(group_id: &str) {
    let mut counts = GROUP_FAILURE_COUNTS.lock().await;
    counts.remove(group_id);
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mls_group_full_serde_round_trip() {
        let full = MlsGroupFull {
            group: MlsGroup {
                group_id: "abc123".into(),
                engine_group_id: "eng456".into(),
                creator_pubkey: "npub1creator".into(),
                created_at: 1700000000,
                updated_at: 1700001000,
                evicted: false,
            },
            profile: MlsGroupProfile {
                name: "Test Group".into(),
                description: Some("A test group".into()),
                avatar_ref: Some("https://blossom.example/avatar".into()),
                avatar_cached: None,
            },
        };

        let json = serde_json::to_string(&full).unwrap();
        let decoded: MlsGroupFull = serde_json::from_str(&json).unwrap();

        assert_eq!(decoded.group.group_id, "abc123");
        assert_eq!(decoded.group.engine_group_id, "eng456");
        assert_eq!(decoded.group.creator_pubkey, "npub1creator");
        assert_eq!(decoded.group.created_at, 1700000000);
        assert_eq!(decoded.group.updated_at, 1700001000);
        assert!(!decoded.group.evicted);
        assert_eq!(decoded.profile.name, "Test Group");
        assert_eq!(decoded.profile.description.as_deref(), Some("A test group"));
        assert_eq!(decoded.profile.avatar_ref.as_deref(), Some("https://blossom.example/avatar"));
        assert!(decoded.profile.avatar_cached.is_none());
    }

    #[test]
    fn mls_group_full_flattened_serde() {
        // Verify flattened serialization produces flat JSON (no nested objects)
        let full = MlsGroupFull {
            group: MlsGroup {
                group_id: "g1".into(),
                engine_group_id: "e1".into(),
                creator_pubkey: "pk".into(),
                created_at: 100,
                updated_at: 200,
                evicted: true,
            },
            profile: MlsGroupProfile {
                name: "Name".into(),
                description: None,
                avatar_ref: None,
                avatar_cached: None,
            },
        };

        let json: serde_json::Value = serde_json::to_value(&full).unwrap();
        // Should be flat — group_id at top level, not nested under "group"
        assert_eq!(json["group_id"], "g1");
        assert_eq!(json["name"], "Name");
        assert_eq!(json["evicted"], true);
        assert!(json.get("group").is_none()); // NOT nested
        assert!(json.get("profile").is_none()); // NOT nested
    }

    #[test]
    fn mls_group_full_deserialize_legacy() {
        // Backwards compat: old data without engine_group_id or evicted should deserialize
        let legacy_json = r#"{
            "group_id": "old_group",
            "creator_pubkey": "pk",
            "name": "Legacy",
            "created_at": 50,
            "updated_at": 60
        }"#;

        let decoded: MlsGroupFull = serde_json::from_str(legacy_json).unwrap();
        assert_eq!(decoded.group.group_id, "old_group");
        assert_eq!(decoded.group.engine_group_id, ""); // serde default
        assert!(!decoded.group.evicted); // serde default
        assert!(decoded.profile.description.is_none()); // serde default
        assert!(decoded.profile.avatar_cached.is_none()); // serde default
    }

    #[test]
    fn metadata_to_frontend_converts_timestamps() {
        let full = MlsGroupFull {
            group: MlsGroup {
                group_id: "g".into(),
                engine_group_id: "e".into(),
                creator_pubkey: "c".into(),
                created_at: 1700000000,
                updated_at: 1700001000,
                evicted: false,
            },
            profile: MlsGroupProfile {
                name: "N".into(),
                description: None,
                avatar_ref: None,
                avatar_cached: None,
            },
        };

        let json = metadata_to_frontend(&full);
        assert_eq!(json["created_at"], 1700000000000u64); // seconds → millis
        assert_eq!(json["updated_at"], 1700001000000u64);
    }

    #[test]
    fn mls_group_full_deref() {
        let full = MlsGroupFull {
            group: MlsGroup {
                group_id: "gid".into(),
                engine_group_id: "eid".into(),
                creator_pubkey: "cpk".into(),
                created_at: 0,
                updated_at: 0,
                evicted: true,
            },
            profile: MlsGroupProfile {
                name: "My Group".into(),
                description: None,
                avatar_ref: None,
                avatar_cached: None,
            },
        };

        // Deref to MlsGroup — identity fields accessible directly
        assert_eq!(full.group_id, "gid");
        assert_eq!(full.engine_group_id, "eid");
        assert_eq!(full.creator_pubkey, "cpk");
        assert!(full.evicted);
        // Profile fields via .profile
        assert_eq!(full.profile.name, "My Group");
    }

    #[test]
    fn event_cursor_serde() {
        let cursor = EventCursor {
            last_seen_event_id: "abc123".into(),
            last_seen_at: 1700000000,
        };
        let json = serde_json::to_string(&cursor).unwrap();
        let decoded: EventCursor = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.last_seen_event_id, "abc123");
        assert_eq!(decoded.last_seen_at, 1700000000);
    }

    #[test]
    fn key_package_index_entry_serde() {
        let entry = KeyPackageIndexEntry {
            owner_pubkey: "npub1test".into(),
            device_id: "device-1".into(),
            keypackage_ref: "ref123".into(),
            fetched_at: 1000,
            expires_at: 2000,
        };
        let json = serde_json::to_string(&entry).unwrap();
        let decoded: KeyPackageIndexEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.owner_pubkey, "npub1test");
        assert_eq!(decoded.device_id, "device-1");
        assert_eq!(decoded.expires_at, 2000);
    }

    #[test]
    fn mls_error_display() {
        assert_eq!(MlsError::NotInitialized.to_string(), "MLS service not initialized");
        assert_eq!(MlsError::GroupNotFound.to_string(), "Group not found");
        assert!(MlsError::OutdatedKeyPackage("Alice".into()).to_string().contains("Alice"));
        assert!(MlsError::StorageError("disk full".into()).to_string().contains("disk full"));
    }

    #[tokio::test]
    async fn desync_tracking_threshold() {
        // Reset any state
        record_group_success("test_desync").await;

        // Should not be desynced before threshold
        for _ in 0..9 {
            assert!(!record_group_failure("test_desync").await);
        }

        // 10th failure should trigger desync
        assert!(record_group_failure("test_desync").await);

        // Reset and verify
        record_group_success("test_desync").await;
        assert!(!record_group_failure("test_desync").await);
    }

    #[tokio::test]
    async fn desync_tracking_isolated_per_group() {
        record_group_success("group_a").await;
        record_group_success("group_b").await;

        // Fail group_a 9 times
        for _ in 0..9 {
            record_group_failure("group_a").await;
        }

        // group_b should still be fine
        assert!(!record_group_failure("group_b").await);

        // group_a should trigger on 10th
        assert!(record_group_failure("group_a").await);
    }
}
