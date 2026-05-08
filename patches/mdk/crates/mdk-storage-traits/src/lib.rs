//! MDK storage - A set of storage provider traits and types for implementing MLS storage
//! It is designed to be used in conjunction with the `openmls` crate.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![warn(rustdoc::bare_urls)]

use openmls_traits::storage::StorageProvider;

pub mod error;
pub mod group_id;
pub mod groups;
pub mod messages;
pub mod mls_codec;
/// Secret wrapper for zeroization
pub mod secret;
#[cfg(feature = "test-utils")]
pub mod test_utils;

pub mod welcomes;

// Re-export GroupId for convenience
pub use error::MdkStorageError;
pub use group_id::GroupId;
pub use secret::{Secret, Zeroize};

use self::groups::GroupStorage;
use self::messages::MessageStorage;
use self::welcomes::WelcomeStorage;

const CURRENT_VERSION: u16 = 1;

/// Backend
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Backend {
    /// Memory
    Memory,
    /// SQLite
    SQLite,
}

impl Backend {
    /// Check if it's a persistent backend
    ///
    /// All values different from [`Backend::Memory`] are considered persistent
    pub fn is_persistent(&self) -> bool {
        !matches!(self, Self::Memory)
    }
}

/// Storage provider for MDK.
///
/// This trait combines all MDK storage requirements with the OpenMLS
/// `StorageProvider` trait, enabling unified storage implementations
/// that can atomically manage both MLS state and MDK-specific data.
///
/// Implementors must provide:
/// - Group storage for MLS group metadata and relays
/// - Message storage for encrypted messages
/// - Welcome storage for pending welcome messages
/// - Full OpenMLS `StorageProvider<1>` implementation for MLS cryptographic state
pub trait MdkStorageProvider:
    GroupStorage + MessageStorage + WelcomeStorage + StorageProvider<CURRENT_VERSION>
{
    /// Returns the backend type.
    ///
    /// # Returns
    ///
    /// The storage backend type (e.g., [`Backend::Memory`] or [`Backend::SQLite`]).
    fn backend(&self) -> Backend;

    /// Create a snapshot of a group's state before applying a commit.
    ///
    /// This captures all MLS and MDK state for the specified group,
    /// enabling rollback if a better commit arrives later (MIP-03).
    ///
    /// The snapshot is stored persistently (in SQLite) or in memory,
    /// keyed by both the group ID and snapshot name.
    fn create_group_snapshot(&self, group_id: &GroupId, name: &str) -> Result<(), MdkStorageError>;

    /// Rollback a group's state to a previously created snapshot.
    ///
    /// This restores all MLS and MDK state for the group to what it was
    /// when the snapshot was created. The snapshot is consumed (deleted) after use.
    fn rollback_group_to_snapshot(
        &self,
        group_id: &GroupId,
        name: &str,
    ) -> Result<(), MdkStorageError>;

    /// Release a snapshot that is no longer needed.
    ///
    /// Call this to free resources when a snapshot won't be used for rollback.
    fn release_group_snapshot(&self, group_id: &GroupId, name: &str)
    -> Result<(), MdkStorageError>;

    /// List all snapshots for a specific group with their creation timestamps.
    ///
    /// Returns a list of (snapshot_name, created_at_unix_timestamp) tuples
    /// ordered by creation time (oldest first). This is used for:
    /// - Hydrating the EpochSnapshotManager after restart
    /// - Auditing existing snapshots
    ///
    /// # Arguments
    ///
    /// * `group_id` - The group to list snapshots for
    ///
    /// # Returns
    ///
    /// A vector of (snapshot_name, created_at) tuples, or an error.
    fn list_group_snapshots(
        &self,
        group_id: &GroupId,
    ) -> Result<Vec<(String, u64)>, MdkStorageError>;

    /// Prune all snapshots created before the given Unix timestamp.
    ///
    /// This is used for TTL-based cleanup of old snapshots to prevent
    /// indefinite storage growth and ensure cryptographic key material
    /// doesn't persist longer than necessary.
    ///
    /// # Arguments
    ///
    /// * `min_timestamp` - Unix timestamp cutoff; snapshots with `created_at < min_timestamp` are deleted
    ///
    /// # Returns
    ///
    /// The number of snapshots deleted, or an error.
    fn prune_expired_snapshots(&self, min_timestamp: u64) -> Result<usize, MdkStorageError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_backend_is_persistent() {
        assert!(!Backend::Memory.is_persistent());
        assert!(Backend::SQLite.is_persistent());
    }
}
