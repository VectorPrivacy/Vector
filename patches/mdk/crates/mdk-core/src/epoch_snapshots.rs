//! Epoch snapshot management for commit race resolution.
//!
//! This module provides the [`EpochSnapshotManager`] which tracks storage snapshots
//! taken before applying commits. When a "better" commit arrives late (per MIP-03
//! ordering rules), the manager can rollback to a previous snapshot and apply the
//! correct winner.
//!
//! See the MIP-03 specification for details on commit ordering:
//! 1. Earliest timestamp wins
//! 2. Lexicographically smallest event ID breaks ties

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::sync::Mutex;
use std::time::Instant;

use mdk_storage_traits::{GroupId, MdkStorageError, MdkStorageProvider};
use nostr::EventId;

use crate::Error;

/// Metadata about a snapshot taken before applying a commit
#[derive(Clone)]
pub struct EpochSnapshot {
    /// The group ID
    pub group_id: GroupId,
    /// The epoch *before* the commit was applied (the state captured in the snapshot)
    pub epoch: u64,
    /// The ID of the commit that was applied *after* this snapshot was taken.
    /// This is the "incumbent" winner that we might want to replace.
    pub applied_commit_id: EventId,
    /// The timestamp of the applied commit (for MIP-03 comparison)
    pub applied_commit_ts: u64,
    /// When the snapshot was created
    pub created_at: Instant,
    /// The unique name of the snapshot in storage
    pub snapshot_name: String,
}

impl fmt::Debug for EpochSnapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Note: snapshot_name contains group_id hex, so we redact it too
        write!(
            f,
            "EpochSnapshot {{ group_id: [REDACTED], epoch: {}, applied_commit_id: {:?}, applied_commit_ts: {}, snapshot_name: [REDACTED] }}",
            self.epoch, self.applied_commit_id, self.applied_commit_ts,
        )
    }
}

struct EpochSnapshotManagerInner {
    /// Snapshots per group, ordered by epoch (oldest first)
    snapshots: HashMap<GroupId, VecDeque<EpochSnapshot>>,
    /// Groups that have been hydrated from storage (only relevant for persistent backends)
    hydrated_groups: HashSet<GroupId>,
}

impl fmt::Debug for EpochSnapshotManagerInner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let total_snapshots: usize = self.snapshots.values().map(|q| q.len()).sum();
        write!(
            f,
            "EpochSnapshotManagerInner {{ groups: {}, total_snapshots: {} }}",
            self.snapshots.len(),
            total_snapshots
        )
    }
}

/// Manages epoch snapshots for rollback support
pub struct EpochSnapshotManager {
    inner: Mutex<EpochSnapshotManagerInner>,
    retention_count: usize,
}

impl fmt::Debug for EpochSnapshotManager {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EpochSnapshotManager")
            .field("inner", &"[REDACTED]")
            .field("retention_count", &self.retention_count)
            .finish()
    }
}

impl EpochSnapshotManager {
    /// Create a new snapshot manager
    pub fn new(retention_count: usize) -> Self {
        Self {
            inner: Mutex::new(EpochSnapshotManagerInner {
                snapshots: HashMap::new(),
                hydrated_groups: HashSet::new(),
            }),
            retention_count,
        }
    }

    /// Ensure a group's snapshots have been loaded from storage.
    ///
    /// For persistent backends (SQLite), this loads existing snapshots into memory
    /// so the manager knows about them after a restart. For memory backends, this
    /// is a no-op since all snapshots are already in memory.
    ///
    /// This method must be called at the start of any operation that reads or
    /// modifies the snapshot queue for a group.
    fn ensure_hydrated<S: MdkStorageProvider>(&self, storage: &S, group_id: &GroupId) {
        // Memory backend doesn't need hydration - all snapshots are already tracked
        if !storage.backend().is_persistent() {
            return;
        }

        let mut inner = self.inner.lock().unwrap();

        // Skip if already hydrated
        if inner.hydrated_groups.contains(group_id) {
            return;
        }

        // Load snapshots from storage
        let stored_snapshots = match storage.list_group_snapshots(group_id) {
            Ok(snapshots) => snapshots,
            Err(e) => {
                tracing::warn!("Failed to load snapshots for hydration: {}", e);
                // Mark as hydrated anyway to avoid repeated failures
                inner.hydrated_groups.insert(group_id.clone());
                return;
            }
        };

        // Parse snapshot names and populate the queue
        // Format: "snap_{group_id_hex}_{epoch}_{commit_id_hex}"
        let queue = inner.snapshots.entry(group_id.clone()).or_default();

        for (snapshot_name, created_at_unix) in stored_snapshots {
            if let Some(snapshot) =
                Self::parse_snapshot_name(&snapshot_name, group_id, created_at_unix)
            {
                queue.push_back(snapshot);
            } else {
                tracing::warn!(
                    "Failed to parse snapshot name during hydration: {}",
                    snapshot_name
                );
            }
        }

        // Enforce retention limit after hydration
        while queue.len() > self.retention_count {
            if let Some(old_snap) = queue.pop_front() {
                let _ = storage.release_group_snapshot(&old_snap.group_id, &old_snap.snapshot_name);
            }
        }

        inner.hydrated_groups.insert(group_id.clone());
    }

    /// Parse a snapshot name to reconstruct EpochSnapshot metadata.
    ///
    /// The format is: "snap_{group_id_hex}_{epoch}_{commit_id_hex}"
    fn parse_snapshot_name(
        snapshot_name: &str,
        group_id: &GroupId,
        _created_at_unix: u64,
    ) -> Option<EpochSnapshot> {
        let parts: Vec<&str> = snapshot_name.split('_').collect();
        // Expected: ["snap", "{group_id_hex}", "{epoch}", "{commit_id_hex}"]
        if parts.len() != 4 || parts[0] != "snap" {
            return None;
        }

        let epoch: u64 = parts[2].parse().ok()?;
        let commit_id = EventId::parse(parts[3]).ok()?;

        // We don't have the original timestamp, so we use 0 as a placeholder.
        // The comparison logic doesn't use applied_commit_ts from hydrated snapshots
        // for the "better candidate" check - that info is lost. But the snapshot
        // can still be used for rollback if explicitly requested.
        Some(EpochSnapshot {
            group_id: group_id.clone(),
            epoch,
            applied_commit_id: commit_id,
            applied_commit_ts: 0, // Unknown after restart - hydrated snapshots should not be used for is_better_candidate
            created_at: Instant::now(), // Placeholder - not used for comparisons
            snapshot_name: snapshot_name.to_string(),
        })
    }

    /// Create a snapshot before applying a commit
    pub fn create_snapshot<S: MdkStorageProvider>(
        &self,
        storage: &S,
        group_id: &GroupId,
        current_epoch: u64,
        commit_id: &EventId,
        commit_ts: u64,
    ) -> Result<String, Error> {
        // Ensure we have loaded any existing snapshots from storage
        self.ensure_hydrated(storage, group_id);

        // Generate a unique snapshot name
        let snapshot_name = format!(
            "snap_{}_{}_{}",
            hex::encode(group_id.as_slice()),
            current_epoch,
            commit_id.to_hex()
        );

        // Create the snapshot in storage
        storage
            .create_group_snapshot(group_id, &snapshot_name)
            .map_err(Error::Storage)?;

        // Record metadata
        let snapshot = EpochSnapshot {
            group_id: group_id.clone(),
            epoch: current_epoch,
            applied_commit_id: *commit_id,
            applied_commit_ts: commit_ts,
            created_at: Instant::now(),
            snapshot_name: snapshot_name.clone(),
        };

        let mut inner = self.inner.lock().unwrap();
        let queue = inner.snapshots.entry(group_id.clone()).or_default();
        queue.push_back(snapshot);

        // Prune if needed (deferred slightly, or do it now)
        // We prune strictly greater than retention count.
        // If retention is 5, we keep 5 snapshots.
        while queue.len() > self.retention_count {
            if let Some(old_snap) = queue.pop_front() {
                // Best effort release
                let _ = storage.release_group_snapshot(&old_snap.group_id, &old_snap.snapshot_name);
            }
        }

        Ok(snapshot_name)
    }

    /// Check if a candidate commit is "better" than the one we applied for this epoch.
    /// Returns true if we should rollback.
    pub fn is_better_candidate<S: MdkStorageProvider>(
        &self,
        storage: &S,
        group_id: &GroupId,
        candidate_epoch: u64,
        candidate_ts: u64,
        candidate_id: &EventId,
    ) -> bool {
        // Ensure we have loaded any existing snapshots from storage
        self.ensure_hydrated(storage, group_id);

        let inner = self.inner.lock().unwrap();

        if let Some(queue) = inner.snapshots.get(group_id)
            && let Some(snapshot) = queue.iter().find(|s| s.epoch == candidate_epoch)
        {
            // Skip comparison for hydrated snapshots (applied_commit_ts == 0) since
            // we don't have the original timestamp info after restart
            if snapshot.applied_commit_ts == 0 {
                return false;
            }
            // Compare according to MIP-03
            // 1. Earliest timestamp wins
            if candidate_ts < snapshot.applied_commit_ts {
                return true;
            }
            if candidate_ts > snapshot.applied_commit_ts {
                return false;
            }

            // 2. ID tiebreaker (lexicographically smallest ID wins)
            // If candidate ID is smaller than applied ID, candidate wins
            if candidate_id.to_hex() < snapshot.applied_commit_id.to_hex() {
                return true;
            }
        }

        false
    }

    /// Rollback to the snapshot for the given epoch.
    /// This restores the state to `target_epoch`.
    pub fn rollback_to_epoch<S: MdkStorageProvider>(
        &self,
        storage: &S,
        group_id: &GroupId,
        target_epoch: u64,
    ) -> Result<(), Error> {
        // Ensure we have loaded any existing snapshots from storage
        self.ensure_hydrated(storage, group_id);

        let mut inner = self.inner.lock().unwrap();

        if let Some(queue) = inner.snapshots.get_mut(group_id) {
            // Find the snapshot
            if let Some(index) = queue.iter().position(|s| s.epoch == target_epoch) {
                let snapshot = &queue[index];

                // Perform rollback (this consumes the snapshot)
                storage
                    .rollback_group_to_snapshot(group_id, &snapshot.snapshot_name)
                    .map_err(Error::Storage)?;

                // Remove all snapshots from index onwards from our tracking
                // The rollback already consumed the target snapshot, but we need to
                // release any snapshots that were created after it
                let removed = queue.split_off(index);
                for (i, snap) in removed.into_iter().enumerate() {
                    // Skip the first one (index 0) - it was already consumed by rollback
                    if i > 0 {
                        let _ = storage.release_group_snapshot(&snap.group_id, &snap.snapshot_name);
                    }
                }

                return Ok(());
            }
        }

        Err(Error::Storage(MdkStorageError::NotFound(
            "No snapshot found for target epoch".to_string(),
        )))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::str::FromStr;

    use mdk_storage_traits::groups::GroupStorage;
    use mdk_storage_traits::groups::types::{Group, GroupState, SelfUpdateState};
    use nostr::PublicKey;

    use super::*;

    /// Helper to create a test group ID
    fn test_group_id(id: u8) -> GroupId {
        GroupId::from_slice(&[id; 32])
    }

    /// Helper to create a test event ID from a hex string
    fn test_event_id(hex: &str) -> EventId {
        EventId::from_str(hex).unwrap()
    }

    // ========================================
    // EpochSnapshotManager::new() Tests
    // ========================================

    #[test]
    fn test_new_creates_manager_with_retention_count() {
        let manager = EpochSnapshotManager::new(5);
        assert_eq!(manager.retention_count, 5);
    }

    #[test]
    fn test_new_with_zero_retention() {
        let manager = EpochSnapshotManager::new(0);
        assert_eq!(manager.retention_count, 0);
    }

    // ========================================
    // is_better_candidate() Tests - MIP-03 Logic
    // ========================================

    #[test]
    fn test_is_better_candidate_earlier_timestamp_wins() {
        let manager = EpochSnapshotManager::new(5);
        let storage = mdk_memory_storage::MdkMemoryStorage::default();
        let group_id = test_group_id(1);
        let applied_id =
            test_event_id("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");

        // Manually insert a snapshot
        {
            let mut inner = manager.inner.lock().unwrap();
            let snapshot = EpochSnapshot {
                group_id: group_id.clone(),
                epoch: 10,
                applied_commit_id: applied_id,
                applied_commit_ts: 1000, // Applied at timestamp 1000
                created_at: Instant::now(),
                snapshot_name: "test_snap".to_string(),
            };
            inner
                .snapshots
                .entry(group_id.clone())
                .or_default()
                .push_back(snapshot);
        }

        // Candidate with earlier timestamp (999 < 1000) should be better
        let candidate_id =
            test_event_id("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        assert!(manager.is_better_candidate(&storage, &group_id, 10, 999, &candidate_id));
    }

    #[test]
    fn test_is_better_candidate_later_timestamp_loses() {
        let manager = EpochSnapshotManager::new(5);
        let storage = mdk_memory_storage::MdkMemoryStorage::default();
        let group_id = test_group_id(1);
        let applied_id =
            test_event_id("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");

        {
            let mut inner = manager.inner.lock().unwrap();
            let snapshot = EpochSnapshot {
                group_id: group_id.clone(),
                epoch: 10,
                applied_commit_id: applied_id,
                applied_commit_ts: 1000,
                created_at: Instant::now(),
                snapshot_name: "test_snap".to_string(),
            };
            inner
                .snapshots
                .entry(group_id.clone())
                .or_default()
                .push_back(snapshot);
        }

        // Candidate with later timestamp (1001 > 1000) should NOT be better
        let candidate_id =
            test_event_id("0000000000000000000000000000000000000000000000000000000000000000");
        assert!(!manager.is_better_candidate(&storage, &group_id, 10, 1001, &candidate_id));
    }

    #[test]
    fn test_is_better_candidate_smaller_id_wins_on_same_timestamp() {
        let manager = EpochSnapshotManager::new(5);
        let storage = mdk_memory_storage::MdkMemoryStorage::default();
        let group_id = test_group_id(1);
        // Applied commit has ID starting with 'b'
        let applied_id =
            test_event_id("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");

        {
            let mut inner = manager.inner.lock().unwrap();
            let snapshot = EpochSnapshot {
                group_id: group_id.clone(),
                epoch: 10,
                applied_commit_id: applied_id,
                applied_commit_ts: 1000,
                created_at: Instant::now(),
                snapshot_name: "test_snap".to_string(),
            };
            inner
                .snapshots
                .entry(group_id.clone())
                .or_default()
                .push_back(snapshot);
        }

        // Candidate with same timestamp but smaller ID ('a' < 'b') should be better
        let candidate_id =
            test_event_id("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        assert!(manager.is_better_candidate(&storage, &group_id, 10, 1000, &candidate_id));
    }

    #[test]
    fn test_is_better_candidate_larger_id_loses_on_same_timestamp() {
        let manager = EpochSnapshotManager::new(5);
        let storage = mdk_memory_storage::MdkMemoryStorage::default();
        let group_id = test_group_id(1);
        // Applied commit has ID starting with 'a'
        let applied_id =
            test_event_id("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");

        {
            let mut inner = manager.inner.lock().unwrap();
            let snapshot = EpochSnapshot {
                group_id: group_id.clone(),
                epoch: 10,
                applied_commit_id: applied_id,
                applied_commit_ts: 1000,
                created_at: Instant::now(),
                snapshot_name: "test_snap".to_string(),
            };
            inner
                .snapshots
                .entry(group_id.clone())
                .or_default()
                .push_back(snapshot);
        }

        // Candidate with same timestamp but larger ID ('c' > 'a') should NOT be better
        let candidate_id =
            test_event_id("cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc");
        assert!(!manager.is_better_candidate(&storage, &group_id, 10, 1000, &candidate_id));
    }

    #[test]
    fn test_is_better_candidate_same_id_returns_false() {
        let manager = EpochSnapshotManager::new(5);
        let storage = mdk_memory_storage::MdkMemoryStorage::default();
        let group_id = test_group_id(1);
        let applied_id =
            test_event_id("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");

        {
            let mut inner = manager.inner.lock().unwrap();
            let snapshot = EpochSnapshot {
                group_id: group_id.clone(),
                epoch: 10,
                applied_commit_id: applied_id,
                applied_commit_ts: 1000,
                created_at: Instant::now(),
                snapshot_name: "test_snap".to_string(),
            };
            inner
                .snapshots
                .entry(group_id.clone())
                .or_default()
                .push_back(snapshot);
        }

        // Same ID and timestamp should NOT be better (it's the same commit)
        assert!(!manager.is_better_candidate(&storage, &group_id, 10, 1000, &applied_id));
    }

    #[test]
    fn test_is_better_candidate_wrong_epoch_returns_false() {
        let manager = EpochSnapshotManager::new(5);
        let storage = mdk_memory_storage::MdkMemoryStorage::default();
        let group_id = test_group_id(1);
        let applied_id =
            test_event_id("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");

        {
            let mut inner = manager.inner.lock().unwrap();
            let snapshot = EpochSnapshot {
                group_id: group_id.clone(),
                epoch: 10,
                applied_commit_id: applied_id,
                applied_commit_ts: 1000,
                created_at: Instant::now(),
                snapshot_name: "test_snap".to_string(),
            };
            inner
                .snapshots
                .entry(group_id.clone())
                .or_default()
                .push_back(snapshot);
        }

        // Even with earlier timestamp, wrong epoch should return false
        let candidate_id =
            test_event_id("0000000000000000000000000000000000000000000000000000000000000000");
        assert!(!manager.is_better_candidate(&storage, &group_id, 11, 999, &candidate_id)); // epoch 11 != 10
    }

    #[test]
    fn test_is_better_candidate_unknown_group_returns_false() {
        let manager = EpochSnapshotManager::new(5);
        let storage = mdk_memory_storage::MdkMemoryStorage::default();
        let unknown_group_id = test_group_id(99);
        let candidate_id =
            test_event_id("0000000000000000000000000000000000000000000000000000000000000000");

        // No snapshots for this group, should return false
        assert!(!manager.is_better_candidate(&storage, &unknown_group_id, 10, 999, &candidate_id));
    }

    #[test]
    fn test_rollback_to_nonexistent_epoch_fails() {
        let manager = EpochSnapshotManager::new(5);
        let group_id = test_group_id(1);
        let storage = mdk_memory_storage::MdkMemoryStorage::default();

        // No snapshots exist, so rollback should fail
        let result = manager.rollback_to_epoch(&storage, &group_id, 10);
        assert!(result.is_err());
        match result.unwrap_err() {
            Error::Storage(MdkStorageError::NotFound(msg)) => {
                assert!(msg.contains("No snapshot found"));
            }
            _ => panic!("Expected NotFound error"),
        }
    }

    #[test]
    fn test_rollback_to_unknown_group_fails() {
        let manager = EpochSnapshotManager::new(5);
        let storage = mdk_memory_storage::MdkMemoryStorage::default();
        let unknown_group_id = test_group_id(99);

        let result = manager.rollback_to_epoch(&storage, &unknown_group_id, 10);
        assert!(result.is_err());
    }

    #[test]
    fn test_snapshots_isolated_per_group() {
        let manager = EpochSnapshotManager::new(5);
        let storage = mdk_memory_storage::MdkMemoryStorage::default();
        let group1 = test_group_id(1);
        let group2 = test_group_id(2);
        let applied_id =
            test_event_id("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");

        // Add snapshot only for group1
        {
            let mut inner = manager.inner.lock().unwrap();
            let snapshot = EpochSnapshot {
                group_id: group1.clone(),
                epoch: 10,
                applied_commit_id: applied_id,
                applied_commit_ts: 1000,
                created_at: Instant::now(),
                snapshot_name: "test_snap".to_string(),
            };
            inner
                .snapshots
                .entry(group1.clone())
                .or_default()
                .push_back(snapshot);
        }

        let candidate_id =
            test_event_id("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");

        // Group1 should find the snapshot
        assert!(manager.is_better_candidate(&storage, &group1, 10, 999, &candidate_id));

        // Group2 should NOT find any snapshot
        assert!(!manager.is_better_candidate(&storage, &group2, 10, 999, &candidate_id));
    }

    #[test]
    fn test_snapshot_retention_pruning() {
        let manager = EpochSnapshotManager::new(2); // Keep only 2 snapshots
        let group_id = test_group_id(1);
        let storage = mdk_memory_storage::MdkMemoryStorage::default();

        // We need to set up the group in storage first
        let admin_pk = PublicKey::from_slice(&[2u8; 32]).unwrap();
        let mut admin_pubkeys = BTreeSet::new();
        admin_pubkeys.insert(admin_pk);
        let group = Group {
            mls_group_id: group_id.clone(),
            nostr_group_id: [0u8; 32],
            name: "Test Group".to_string(),
            description: "Test".to_string(),
            admin_pubkeys,
            epoch: 0,
            last_message_at: None,
            last_message_processed_at: None,
            last_message_id: None,
            image_hash: None,
            image_key: None,
            image_nonce: None,
            state: GroupState::Active,
            self_update_state: SelfUpdateState::Required,
        };
        storage.save_group(group).unwrap();

        // Create 3 snapshots
        for epoch in 0..3 {
            let commit_id = test_event_id(&format!("{:064x}", epoch + 1));
            let _ = manager.create_snapshot(&storage, &group_id, epoch, &commit_id, 1000 + epoch);
        }

        // With retention of 2, only epochs 1 and 2 should remain
        let inner = manager.inner.lock().unwrap();
        let queue = inner.snapshots.get(&group_id).unwrap();
        assert_eq!(queue.len(), 2);
        assert_eq!(queue[0].epoch, 1);
        assert_eq!(queue[1].epoch, 2);
    }

    #[test]
    fn test_rollback_removes_subsequent_snapshots() {
        let manager = EpochSnapshotManager::new(10);
        let group_id = test_group_id(1);
        let storage = mdk_memory_storage::MdkMemoryStorage::default();

        // Set up group in storage
        let admin_pk = PublicKey::from_slice(&[2u8; 32]).unwrap();
        let mut admin_pubkeys = BTreeSet::new();
        admin_pubkeys.insert(admin_pk);
        let group = Group {
            mls_group_id: group_id.clone(),
            nostr_group_id: [0u8; 32],
            name: "Test Group".to_string(),
            description: "Test".to_string(),
            admin_pubkeys,
            epoch: 0,
            last_message_at: None,
            last_message_processed_at: None,
            last_message_id: None,
            image_hash: None,
            image_key: None,
            image_nonce: None,
            state: GroupState::Active,
            self_update_state: SelfUpdateState::Required,
        };
        storage.save_group(group).unwrap();

        // Create snapshots for epochs 0, 1, 2, 3
        for epoch in 0..4 {
            let commit_id = test_event_id(&format!("{:064x}", epoch + 1));
            manager
                .create_snapshot(&storage, &group_id, epoch, &commit_id, 1000 + epoch)
                .unwrap();
        }

        // Verify we have 4 snapshots
        {
            let inner = manager.inner.lock().unwrap();
            assert_eq!(inner.snapshots.get(&group_id).unwrap().len(), 4);
        }

        // Rollback to epoch 1
        manager.rollback_to_epoch(&storage, &group_id, 1).unwrap();

        // After rollback to epoch 1, snapshots for epochs 1, 2, 3 should be removed
        // Only epoch 0 should remain
        {
            let inner = manager.inner.lock().unwrap();
            let queue = inner.snapshots.get(&group_id).unwrap();
            assert_eq!(queue.len(), 1);
            assert_eq!(queue[0].epoch, 0);
        }
    }

    // ========================================
    // Debug Implementation Tests
    // ========================================

    #[test]
    fn test_epoch_snapshot_debug_redacts_sensitive_data() {
        let group_id = test_group_id(1);
        let event_id =
            test_event_id("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");

        let snapshot = EpochSnapshot {
            group_id,
            epoch: 10,
            applied_commit_id: event_id,
            applied_commit_ts: 1000,
            created_at: Instant::now(),
            snapshot_name: "snap_abc123".to_string(),
        };

        let debug_str = format!("{:?}", snapshot);

        // Should contain [REDACTED] for sensitive fields
        assert!(debug_str.contains("[REDACTED]"));
        // Should contain epoch number (not sensitive)
        assert!(debug_str.contains("epoch: 10"));
        // Should NOT contain actual group_id bytes or snapshot name
        assert!(!debug_str.contains("snap_abc123"));
    }

    #[test]
    fn test_epoch_snapshot_manager_debug_redacts_inner() {
        let manager = EpochSnapshotManager::new(5);

        let debug_str = format!("{:?}", manager);

        assert!(debug_str.contains("EpochSnapshotManager"));
        assert!(debug_str.contains("[REDACTED]"));
        assert!(debug_str.contains("retention_count: 5"));
    }

    #[test]
    fn test_epoch_snapshot_manager_inner_debug_shows_counts() {
        let manager = EpochSnapshotManager::new(5);
        let group1 = test_group_id(1);
        let group2 = test_group_id(2);
        let event_id =
            test_event_id("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");

        // Add some snapshots
        {
            let mut inner = manager.inner.lock().unwrap();

            // 2 snapshots for group1
            for epoch in 0..2 {
                let snapshot = EpochSnapshot {
                    group_id: group1.clone(),
                    epoch,
                    applied_commit_id: event_id,
                    applied_commit_ts: 1000,
                    created_at: Instant::now(),
                    snapshot_name: format!("snap_{}", epoch),
                };
                inner
                    .snapshots
                    .entry(group1.clone())
                    .or_default()
                    .push_back(snapshot);
            }

            // 1 snapshot for group2
            let snapshot = EpochSnapshot {
                group_id: group2.clone(),
                epoch: 0,
                applied_commit_id: event_id,
                applied_commit_ts: 1000,
                created_at: Instant::now(),
                snapshot_name: "snap_0".to_string(),
            };
            inner
                .snapshots
                .entry(group2.clone())
                .or_default()
                .push_back(snapshot);

            let debug_str = format!("{:?}", *inner);
            assert!(debug_str.contains("groups: 2"));
            assert!(debug_str.contains("total_snapshots: 3"));
        }
    }

    // ========================================
    // Integration Tests with MdkMemoryStorage
    // ========================================

    #[test]
    fn test_create_snapshot_generates_unique_name() {
        let manager = EpochSnapshotManager::new(5);
        let group_id = test_group_id(1);
        let storage = mdk_memory_storage::MdkMemoryStorage::default();

        // Set up group
        let admin_pk = PublicKey::from_slice(&[2u8; 32]).unwrap();
        let mut admin_pubkeys = BTreeSet::new();
        admin_pubkeys.insert(admin_pk);
        let group = Group {
            mls_group_id: group_id.clone(),
            nostr_group_id: [0u8; 32],
            name: "Test Group".to_string(),
            description: "Test".to_string(),
            admin_pubkeys,
            epoch: 0,
            last_message_at: None,
            last_message_processed_at: None,
            last_message_id: None,
            image_hash: None,
            image_key: None,
            image_nonce: None,
            state: GroupState::Active,
            self_update_state: SelfUpdateState::Required,
        };
        storage.save_group(group).unwrap();

        let commit_id =
            test_event_id("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");

        let name = manager
            .create_snapshot(&storage, &group_id, 5, &commit_id, 1000)
            .unwrap();

        // Name should contain group_id hex, epoch, and commit_id
        assert!(name.starts_with("snap_"));
        assert!(name.contains("_5_")); // epoch
        assert!(name.contains("aaaa")); // part of commit_id
    }

    // ========================================
    // Hydration Tests
    // ========================================

    #[test]
    fn test_hydration_skipped_for_memory_storage() {
        // Memory storage is not persistent, so hydration should be skipped
        // We can verify this by checking that is_better_candidate returns false
        // for a non-existent in-memory snapshot (no hydration occurs)
        let manager = EpochSnapshotManager::new(5);
        let storage = mdk_memory_storage::MdkMemoryStorage::default();
        let group_id = test_group_id(1);

        // Call is_better_candidate - since memory storage is not persistent,
        // no hydration occurs and there are no snapshots
        let candidate_id =
            test_event_id("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        let result = manager.is_better_candidate(&storage, &group_id, 5, 999, &candidate_id);

        // Should return false since there are no snapshots and no hydration
        assert!(
            !result,
            "Should return false for non-persistent storage with no snapshots"
        );

        // Verify the internal state - no snapshots should have been added
        {
            let inner = manager.inner.lock().unwrap();
            assert!(
                !inner.snapshots.contains_key(&group_id)
                    || inner.snapshots.get(&group_id).unwrap().is_empty(),
                "Memory storage should not trigger hydration"
            );
        }
    }

    #[test]
    fn test_memory_storage_not_tracked_for_hydration() {
        // Memory storage groups should not be tracked in hydrated_groups set
        // since they don't need hydration
        let manager = EpochSnapshotManager::new(5);
        let storage = mdk_memory_storage::MdkMemoryStorage::default();
        let group_id = test_group_id(1);

        let candidate_id =
            test_event_id("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let _ = manager.is_better_candidate(&storage, &group_id, 5, 999, &candidate_id);

        // For memory storage, hydrated_groups won't be marked (hydration is skipped entirely)
        {
            let inner = manager.inner.lock().unwrap();
            assert!(
                !inner.hydrated_groups.contains(&group_id),
                "Memory storage groups should not be tracked in hydrated_groups"
            );
        }
    }

    #[test]
    fn test_snapshot_name_format() {
        // Verify that snapshot names follow the expected format:
        // "snap_{group_id_hex}_{epoch}_{commit_id_hex}"
        // This ensures hydration can correctly parse them later
        let manager = EpochSnapshotManager::new(5);
        let storage = mdk_memory_storage::MdkMemoryStorage::default();
        let group_id = test_group_id(42);

        // Set up group in storage
        let admin_pk = PublicKey::from_slice(&[2u8; 32]).unwrap();
        let mut admin_pubkeys = BTreeSet::new();
        admin_pubkeys.insert(admin_pk);
        let group = Group {
            mls_group_id: group_id.clone(),
            nostr_group_id: [0u8; 32],
            name: "Test Group".to_string(),
            description: "Test".to_string(),
            admin_pubkeys,
            epoch: 0,
            last_message_at: None,
            last_message_processed_at: None,
            last_message_id: None,
            image_hash: None,
            image_key: None,
            image_nonce: None,
            state: GroupState::Active,
            self_update_state: SelfUpdateState::Required,
        };
        storage.save_group(group).unwrap();

        let commit_id =
            test_event_id("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        let epoch = 7u64;

        let name = manager
            .create_snapshot(&storage, &group_id, epoch, &commit_id, 1000)
            .unwrap();

        // Verify the format
        let parts: Vec<&str> = name.split('_').collect();
        assert_eq!(parts.len(), 4, "Snapshot name should have 4 parts");
        assert_eq!(parts[0], "snap", "First part should be 'snap'");
        assert_eq!(parts[2], "7", "Third part should be the epoch");
        // parts[1] is the group_id hex, parts[3] is the commit_id hex
        assert_eq!(parts[1].len(), 64, "Group ID hex should be 64 chars");
        assert_eq!(parts[3].len(), 64, "Commit ID hex should be 64 chars");
    }
}
