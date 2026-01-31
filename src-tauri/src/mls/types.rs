//! MLS types, errors, and desync tracking.
//!
//! This module contains:
//! - MlsError enum for error handling
//! - MlsGroupMetadata for group state
//! - EventCursor for sync tracking
//! - Desync detection helpers

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tokio::sync::Mutex as TokioMutex;
use once_cell::sync::Lazy;

/// Tracks consecutive processing failures per group for desync detection.
/// If a group has too many consecutive failures, it likely means the member
/// missed commits and is permanently desynced (can't decrypt new messages).
static GROUP_FAILURE_COUNTS: Lazy<TokioMutex<HashMap<String, u32>>> =
    Lazy::new(|| TokioMutex::new(HashMap::new()));

/// Threshold for consecutive failures before considering a group desynced.
/// After this many consecutive unprocessable/error events, we alert the user.
const DESYNC_FAILURE_THRESHOLD: u32 = 10;

// Note: We use immediate retries (no delay) within a sync cycle to resolve ordering issues.
// Events still unprocessable after 5 immediate passes will be retried on the NEXT sync cycle.
// This is non-blocking to avoid delaying sync of other groups.

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
    counts.insert(group_id.to_string(), 0);
}

/// MLS-specific error types following this crate's error style
#[derive(Debug)]
pub enum MlsError {
    NotInitialized,
    InvalidGroupId,
    InvalidKeyPackage,
    GroupNotFound,
    MemberNotFound,
    OutdatedKeyPackage(String), // Member npub with outdated keypackage
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

/// Check if a keypackage event has the required encoding tag (MIP-00/MIP-02).
/// Returns true if the event has ["encoding", "base64"] tag, false otherwise.
pub fn has_encoding_tag(event: &nostr_sdk::Event) -> bool {
    event.tags.iter().any(|tag| {
        let tag_vec: Vec<String> = tag.clone().to_vec();
        tag_vec.len() >= 2 && tag_vec[0] == "encoding" && tag_vec[1] == "base64"
    })
}

/// MLS group metadata stored encrypted in "mls_groups"
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MlsGroupMetadata {
    // Wire identifier used on the relay (wrapper 'h' tag). UI lists this value.
    pub group_id: String,
    // Engine identifier used locally by nostr-mls for group state lookups.
    // Backwards compatible with existing data via serde default.
    #[serde(default)]
    pub engine_group_id: String,
    pub creator_pubkey: String,
    pub name: String,
    pub avatar_ref: Option<String>,
    pub created_at: u64,
    pub updated_at: u64,
    // Flag indicating if we were evicted/kicked from this group
    // When true, we skip syncing this group (unless it's a new welcome/invite)
    #[serde(default)]
    pub evicted: bool,
}

/// Keypackage index entry stored in "mls_keypackage_index"
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