//! Memory-based storage implementation for MDK.
//!
//! This module provides a memory-based storage implementation for MDK (Marmot Development Kit).
//! It implements the `MdkStorageProvider` trait, allowing it to be used as an in-memory storage backend.
//!
//! Memory-based storage is non-persistent and will be cleared when the application terminates.
//! It's useful for testing or ephemeral applications where persistence isn't required.
//!
//! # Unified Storage Architecture
//!
//! This implementation stores all MLS and MDK state in-memory. It supports
//! snapshot and restore operations for rollback scenarios, analogous to SQLite
//! savepoints.
//!
//! **Note:** Snapshot and restore operations are **atomic**. `create_snapshot()`
//! acquires a global read lock and `restore_snapshot()` acquires a global write
//! lock on the storage state, ensuring consistency in multi-threaded environments.

//! ## Memory Exhaustion Protection
//!
//! This implementation includes input validation to prevent memory exhaustion attacks.
//! The following limits are enforced (with configurable defaults via [`ValidationLimits`]):
//!
//! - [`DEFAULT_MAX_RELAYS_PER_GROUP`]: Maximum number of relays per group
//! - [`DEFAULT_MAX_MESSAGES_PER_GROUP`]: Maximum messages stored per group in the cache
//! - [`DEFAULT_MAX_GROUP_NAME_LENGTH`]: Maximum length of group name in bytes
//! - [`DEFAULT_MAX_GROUP_DESCRIPTION_LENGTH`]: Maximum length of group description in bytes
//! - [`DEFAULT_MAX_ADMINS_PER_GROUP`]: Maximum number of admin pubkeys per group
//! - [`DEFAULT_MAX_RELAYS_PER_WELCOME`]: Maximum number of relays in a welcome message
//! - [`DEFAULT_MAX_ADMINS_PER_WELCOME`]: Maximum number of admin pubkeys in a welcome message
//! - [`DEFAULT_MAX_RELAY_URL_LENGTH`]: Maximum length of a relay URL in bytes
//!
//! ## Customizing Limits
//!
//! You can customize these limits using [`ValidationLimits`] and the builder pattern:
//!
//! ```rust
//! use mdk_memory_storage::{MdkMemoryStorage, ValidationLimits};
//!
//! let limits = ValidationLimits::default()
//!     .with_cache_size(2000)
//!     .with_max_messages_per_group(5000)
//!     .with_max_relays_per_group(50);
//!
//! let storage = MdkMemoryStorage::with_limits(limits);
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![warn(rustdoc::bare_urls)]

use std::collections::{BTreeSet, HashMap};
use std::fmt;
use std::num::NonZeroUsize;

use lru::LruCache;
use mdk_storage_traits::GroupId;
use mdk_storage_traits::groups::types::{Group, GroupExporterSecret, GroupRelay};
use mdk_storage_traits::messages::types::{Message, ProcessedMessage};
use mdk_storage_traits::welcomes::types::{ProcessedWelcome, Welcome};
use mdk_storage_traits::{Backend, MdkStorageError, MdkStorageProvider};
use nostr::EventId;
use openmls_traits::storage::{StorageProvider, traits};
use parking_lot::RwLock;

mod groups;
mod messages;
mod mls_storage;
mod snapshot;
mod welcomes;

use self::mls_storage::{
    GroupDataType, MlsEncryptionKeys, MlsEpochKeyPairs, MlsGroupData, MlsKeyPackages,
    MlsOwnLeafNodes, MlsProposals, MlsPsks, MlsSignatureKeys, STORAGE_PROVIDER_VERSION,
};
pub use self::snapshot::{GroupScopedSnapshot, MemoryStorageSnapshot};
use self::snapshot::{HashMapToLruExt, LruCacheExt};

/// Default cache size for each LRU cache
const DEFAULT_CACHE_SIZE: NonZeroUsize = match NonZeroUsize::new(1000) {
    Some(v) => v,
    None => panic!("cache size must be non-zero"),
};

/// Default maximum number of relays allowed per group to prevent memory exhaustion.
/// This limit prevents attackers from growing a single cache entry unboundedly.
pub const DEFAULT_MAX_RELAYS_PER_GROUP: usize = 100;

/// Default maximum number of messages stored per group in the messages_by_group_cache.
/// When this limit is reached, the oldest messages are evicted from the per-group cache.
/// This prevents a single hot group from consuming excessive memory.
pub const DEFAULT_MAX_MESSAGES_PER_GROUP: usize = 10000;

/// Default maximum length of a group name in bytes (not characters).
/// Multi-byte UTF-8 characters count as multiple bytes toward this limit.
/// This prevents oversized group metadata from consuming excessive memory.
pub const DEFAULT_MAX_GROUP_NAME_LENGTH: usize = 256;

/// Default maximum length of a group description in bytes (not characters).
/// Multi-byte UTF-8 characters count as multiple bytes toward this limit.
/// This prevents oversized group metadata from consuming excessive memory.
pub const DEFAULT_MAX_GROUP_DESCRIPTION_LENGTH: usize = 4096;

/// Default maximum number of admin pubkeys allowed per group.
/// This prevents unbounded growth of the admin set.
pub const DEFAULT_MAX_ADMINS_PER_GROUP: usize = 100;

/// Default maximum number of relays allowed in a welcome message.
/// This prevents oversized welcome messages from consuming excessive memory.
pub const DEFAULT_MAX_RELAYS_PER_WELCOME: usize = 100;

/// Default maximum number of admin pubkeys allowed in a welcome message.
/// This prevents oversized welcome messages from consuming excessive memory.
pub const DEFAULT_MAX_ADMINS_PER_WELCOME: usize = 100;

/// Default maximum length of a relay URL in bytes.
/// This prevents oversized relay URLs from consuming excessive memory.
pub const DEFAULT_MAX_RELAY_URL_LENGTH: usize = 512;

/// Configurable validation limits for memory storage.
///
/// This struct allows customization of the various limits used to prevent
/// memory exhaustion attacks. All limits have sensible defaults that can
/// be overridden using the builder pattern.
///
/// # Example
///
/// ```rust
/// use mdk_memory_storage::ValidationLimits;
///
/// let limits = ValidationLimits::default()
///     .with_cache_size(2000)
///     .with_max_messages_per_group(5000)
///     .with_max_relays_per_group(50);
/// ```
#[derive(Debug, Clone, Copy)]
pub struct ValidationLimits {
    /// Maximum number of items in each LRU cache
    pub cache_size: usize,
    /// Maximum number of relays allowed per group
    pub max_relays_per_group: usize,
    /// Maximum number of messages stored per group
    pub max_messages_per_group: usize,
    /// Maximum length of a group name in bytes
    pub max_group_name_length: usize,
    /// Maximum length of a group description in bytes
    pub max_group_description_length: usize,
    /// Maximum number of admin pubkeys per group
    pub max_admins_per_group: usize,
    /// Maximum number of relays in a welcome message
    pub max_relays_per_welcome: usize,
    /// Maximum number of admin pubkeys in a welcome message
    pub max_admins_per_welcome: usize,
    /// Maximum length of a relay URL in bytes
    pub max_relay_url_length: usize,
}

impl Default for ValidationLimits {
    fn default() -> Self {
        Self {
            cache_size: DEFAULT_CACHE_SIZE.get(),
            max_relays_per_group: DEFAULT_MAX_RELAYS_PER_GROUP,
            max_messages_per_group: DEFAULT_MAX_MESSAGES_PER_GROUP,
            max_group_name_length: DEFAULT_MAX_GROUP_NAME_LENGTH,
            max_group_description_length: DEFAULT_MAX_GROUP_DESCRIPTION_LENGTH,
            max_admins_per_group: DEFAULT_MAX_ADMINS_PER_GROUP,
            max_relays_per_welcome: DEFAULT_MAX_RELAYS_PER_WELCOME,
            max_admins_per_welcome: DEFAULT_MAX_ADMINS_PER_WELCOME,
            max_relay_url_length: DEFAULT_MAX_RELAY_URL_LENGTH,
        }
    }
}

impl ValidationLimits {
    /// Creates a new `ValidationLimits` with default values.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the maximum number of items in each LRU cache.
    ///
    /// # Panics
    ///
    /// Panics if `size` is 0.
    pub fn with_cache_size(mut self, size: usize) -> Self {
        assert!(size > 0, "cache_size must be greater than 0");
        self.cache_size = size;
        self
    }

    /// Sets the maximum number of relays allowed per group.
    ///
    /// # Panics
    ///
    /// Panics if `limit` is 0.
    pub fn with_max_relays_per_group(mut self, limit: usize) -> Self {
        assert!(limit > 0, "max_relays_per_group must be greater than 0");
        self.max_relays_per_group = limit;
        self
    }

    /// Sets the maximum number of messages stored per group.
    ///
    /// # Panics
    ///
    /// Panics if `limit` is 0.
    pub fn with_max_messages_per_group(mut self, limit: usize) -> Self {
        assert!(limit > 0, "max_messages_per_group must be greater than 0");
        self.max_messages_per_group = limit;
        self
    }

    /// Sets the maximum length of a group name in bytes.
    ///
    /// # Panics
    ///
    /// Panics if `limit` is 0.
    pub fn with_max_group_name_length(mut self, limit: usize) -> Self {
        assert!(limit > 0, "max_group_name_length must be greater than 0");
        self.max_group_name_length = limit;
        self
    }

    /// Sets the maximum length of a group description in bytes.
    ///
    /// # Panics
    ///
    /// Panics if `limit` is 0.
    pub fn with_max_group_description_length(mut self, limit: usize) -> Self {
        assert!(
            limit > 0,
            "max_group_description_length must be greater than 0"
        );
        self.max_group_description_length = limit;
        self
    }

    /// Sets the maximum number of admin pubkeys per group.
    ///
    /// # Panics
    ///
    /// Panics if `limit` is 0.
    pub fn with_max_admins_per_group(mut self, limit: usize) -> Self {
        assert!(limit > 0, "max_admins_per_group must be greater than 0");
        self.max_admins_per_group = limit;
        self
    }

    /// Sets the maximum number of relays in a welcome message.
    ///
    /// # Panics
    ///
    /// Panics if `limit` is 0.
    pub fn with_max_relays_per_welcome(mut self, limit: usize) -> Self {
        assert!(limit > 0, "max_relays_per_welcome must be greater than 0");
        self.max_relays_per_welcome = limit;
        self
    }

    /// Sets the maximum number of admin pubkeys in a welcome message.
    ///
    /// # Panics
    ///
    /// Panics if `limit` is 0.
    pub fn with_max_admins_per_welcome(mut self, limit: usize) -> Self {
        assert!(limit > 0, "max_admins_per_welcome must be greater than 0");
        self.max_admins_per_welcome = limit;
        self
    }

    /// Sets the maximum length of a relay URL in bytes.
    ///
    /// # Panics
    ///
    /// Panics if `limit` is 0.
    pub fn with_max_relay_url_length(mut self, limit: usize) -> Self {
        assert!(limit > 0, "max_relay_url_length must be greater than 0");
        self.max_relay_url_length = limit;
        self
    }
}

/// A memory-based storage implementation for MDK.
///
/// This struct implements both the OpenMLS `StorageProvider<1>` trait and MDK storage
/// traits directly, providing unified storage for MLS cryptographic state and
/// MDK-specific data (groups, messages, welcomes).
///
/// ## Unified Storage Architecture
///
/// This implementation stores all MLS and MDK state in-memory, providing:
/// - Snapshot/restore operations for rollback scenarios
/// - Thread-safe access through `RwLock` protected data structures
/// - LRU caching for frequently accessed MDK objects
///
/// **Concurrency:** Snapshot and restore operations are **atomic**. `create_snapshot()`
/// acquires a global read lock and `restore_snapshot()` acquires a global write lock
/// on the storage state, ensuring consistency in multi-threaded environments.
///
/// ## Caching Strategy
///
/// This implementation uses an LRU (Least Recently Used) caching mechanism to store
/// frequently accessed objects in memory for faster retrieval. The caches are protected
/// by `RwLock`s to ensure thread safety while allowing concurrent reads.
///
/// - Each cache has a configurable size limit (default: 1000 items)
/// - When a cache reaches its size limit, the least recently used items will be evicted
///
/// ## Thread Safety
///
/// All caches are protected by `RwLock`s, which allow:
/// - Multiple concurrent readers (for find/get operations)
/// - Exclusive writers (for create/save/delete operations)
///
/// This approach optimizes for read-heavy workloads while still ensuring data consistency.
///
/// ## Configurable Validation Limits
///
/// You can customize validation limits using [`ValidationLimits`]:
///
/// ```rust
/// use mdk_memory_storage::{MdkMemoryStorage, ValidationLimits};
///
/// let limits = ValidationLimits::default()
///     .with_cache_size(2000)
///     .with_max_messages_per_group(5000);
///
/// let storage = MdkMemoryStorage::with_limits(limits);
/// ```
pub struct MdkMemoryStorage {
    /// Configurable validation limits
    limits: ValidationLimits,
    /// Thread-safe inner storage
    inner: RwLock<MdkMemoryStorageInner>,
    /// Group-scoped snapshots for rollback support (MIP-03)
    /// Key is (group_id, snapshot_name) for group-specific rollback
    /// Uses GroupScopedSnapshot to ensure rollback only affects the target group
    group_snapshots: RwLock<HashMap<(GroupId, String), GroupScopedSnapshot>>,
}

/// Unified storage architecture container
struct MdkMemoryStorageInner {
    // ========================================================================
    // MLS Storage
    // ========================================================================
    mls_group_data: MlsGroupData,
    mls_own_leaf_nodes: MlsOwnLeafNodes,
    mls_proposals: MlsProposals,
    mls_key_packages: MlsKeyPackages,
    mls_psks: MlsPsks,
    mls_signature_keys: MlsSignatureKeys,
    mls_encryption_keys: MlsEncryptionKeys,
    mls_epoch_key_pairs: MlsEpochKeyPairs,

    // ========================================================================
    // MDK Storage
    // ========================================================================
    groups_cache: LruCache<GroupId, Group>,
    groups_by_nostr_id_cache: LruCache<[u8; 32], Group>,
    group_relays_cache: LruCache<GroupId, BTreeSet<GroupRelay>>,
    welcomes_cache: LruCache<EventId, Welcome>,
    processed_welcomes_cache: LruCache<EventId, ProcessedWelcome>,
    messages_cache: LruCache<EventId, Message>,
    messages_by_group_cache: LruCache<GroupId, HashMap<EventId, Message>>,
    processed_messages_cache: LruCache<EventId, ProcessedMessage>,
    group_exporter_secrets_cache: LruCache<(GroupId, u64), GroupExporterSecret>,
}

impl fmt::Debug for MdkMemoryStorage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MdkMemoryStorage")
            .field("limits", &self.limits)
            .field("inner", &"RwLock<MdkMemoryStorageInner>")
            .finish()
    }
}

impl Default for MdkMemoryStorage {
    /// Creates a new `MdkMemoryStorage` with default configuration.
    ///
    /// # Returns
    ///
    /// A new instance of `MdkMemoryStorage` with the default cache size.
    fn default() -> Self {
        Self::new()
    }
}

impl MdkMemoryStorage {
    /// Creates a new `MdkMemoryStorage` with the default configuration.
    ///
    /// # Returns
    ///
    /// A new instance of `MdkMemoryStorage` with the default cache size.
    pub fn new() -> Self {
        Self::with_cache_size(DEFAULT_CACHE_SIZE)
    }

    /// Creates a new `MdkMemoryStorage` with the specified cache size.
    ///
    /// # Arguments
    ///
    /// * `cache_size` - The maximum number of items to store in each LRU cache.
    ///
    /// # Returns
    ///
    /// A new instance of `MdkMemoryStorage` with the specified cache size.
    pub fn with_cache_size(cache_size: NonZeroUsize) -> Self {
        Self::with_limits(ValidationLimits::default().with_cache_size(cache_size.get()))
    }

    /// Creates a new `MdkMemoryStorage` with the provided validation limits.
    ///
    /// # Arguments
    ///
    /// * `limits` - Custom validation limits for memory exhaustion protection.
    ///
    /// # Returns
    ///
    /// A new instance of `MdkMemoryStorage`.
    pub fn with_limits(limits: ValidationLimits) -> Self {
        let cache_size =
            NonZeroUsize::new(limits.cache_size).expect("cache_size must be greater than 0");

        let inner = MdkMemoryStorageInner {
            // MLS storage
            mls_group_data: MlsGroupData::new(),
            mls_own_leaf_nodes: MlsOwnLeafNodes::new(),
            mls_proposals: MlsProposals::new(),
            mls_key_packages: MlsKeyPackages::new(),
            mls_psks: MlsPsks::new(),
            mls_signature_keys: MlsSignatureKeys::new(),
            mls_encryption_keys: MlsEncryptionKeys::new(),
            mls_epoch_key_pairs: MlsEpochKeyPairs::new(),
            // MDK storage
            groups_cache: LruCache::new(cache_size),
            groups_by_nostr_id_cache: LruCache::new(cache_size),
            group_relays_cache: LruCache::new(cache_size),
            welcomes_cache: LruCache::new(cache_size),
            processed_welcomes_cache: LruCache::new(cache_size),
            messages_cache: LruCache::new(cache_size),
            messages_by_group_cache: LruCache::new(cache_size),
            processed_messages_cache: LruCache::new(cache_size),
            group_exporter_secrets_cache: LruCache::new(cache_size),
        };

        MdkMemoryStorage {
            limits,
            inner: RwLock::new(inner),
            group_snapshots: RwLock::new(HashMap::new()),
        }
    }

    // ========================================================================
    // Snapshot and Restore Support
    // ========================================================================

    /// Creates a snapshot of all in-memory state.
    ///
    /// This enables rollback functionality similar to SQLite savepoints.
    ///
    /// # Concurrency
    ///
    /// This operation is **atomic**. It acquires a global read lock on the storage
    /// state, ensuring a consistent snapshot even in multi-threaded environments.
    ///
    /// # Returns
    ///
    /// A `MemoryStorageSnapshot` containing cloned copies of all state.
    pub fn create_snapshot(&self) -> MemoryStorageSnapshot {
        let inner = self.inner.read();
        MemoryStorageSnapshot {
            // MLS data
            mls_group_data: inner.mls_group_data.clone_data(),
            mls_own_leaf_nodes: inner.mls_own_leaf_nodes.clone_data(),
            mls_proposals: inner.mls_proposals.clone_data(),
            mls_key_packages: inner.mls_key_packages.clone_data(),
            mls_psks: inner.mls_psks.clone_data(),
            mls_signature_keys: inner.mls_signature_keys.clone_data(),
            mls_encryption_keys: inner.mls_encryption_keys.clone_data(),
            mls_epoch_key_pairs: inner.mls_epoch_key_pairs.clone_data(),
            // MDK data
            groups: inner.groups_cache.clone_to_hashmap(),
            groups_by_nostr_id: inner.groups_by_nostr_id_cache.clone_to_hashmap(),
            group_relays: inner.group_relays_cache.clone_to_hashmap(),
            group_exporter_secrets: inner.group_exporter_secrets_cache.clone_to_hashmap(),
            welcomes: inner.welcomes_cache.clone_to_hashmap(),
            processed_welcomes: inner.processed_welcomes_cache.clone_to_hashmap(),
            messages: inner.messages_cache.clone_to_hashmap(),
            messages_by_group: inner.messages_by_group_cache.clone_to_hashmap(),
            processed_messages: inner.processed_messages_cache.clone_to_hashmap(),
        }
    }

    /// Restores state from a previously created snapshot.
    ///
    /// This replaces all current in-memory state with the state from the snapshot.
    ///
    /// # Concurrency
    ///
    /// This operation is **atomic**. It acquires a global write lock on the storage
    /// state, ensuring that the restore is consistent even in multi-threaded environments.
    ///
    /// # Arguments
    ///
    /// * `snapshot` - The snapshot to restore from.
    pub fn restore_snapshot(&self, snapshot: MemoryStorageSnapshot) {
        let mut inner = self.inner.write();

        // Restore MLS data
        inner.mls_group_data.restore_data(snapshot.mls_group_data);
        inner
            .mls_own_leaf_nodes
            .restore_data(snapshot.mls_own_leaf_nodes);
        inner.mls_proposals.restore_data(snapshot.mls_proposals);
        inner
            .mls_key_packages
            .restore_data(snapshot.mls_key_packages);
        inner.mls_psks.restore_data(snapshot.mls_psks);
        inner
            .mls_signature_keys
            .restore_data(snapshot.mls_signature_keys);
        inner
            .mls_encryption_keys
            .restore_data(snapshot.mls_encryption_keys);
        inner
            .mls_epoch_key_pairs
            .restore_data(snapshot.mls_epoch_key_pairs);

        // Restore MDK data
        snapshot.groups.restore_to_lru(&mut inner.groups_cache);
        snapshot
            .groups_by_nostr_id
            .restore_to_lru(&mut inner.groups_by_nostr_id_cache);
        snapshot
            .group_relays
            .restore_to_lru(&mut inner.group_relays_cache);
        snapshot
            .group_exporter_secrets
            .restore_to_lru(&mut inner.group_exporter_secrets_cache);
        snapshot.welcomes.restore_to_lru(&mut inner.welcomes_cache);
        snapshot
            .processed_welcomes
            .restore_to_lru(&mut inner.processed_welcomes_cache);
        snapshot.messages.restore_to_lru(&mut inner.messages_cache);
        snapshot
            .messages_by_group
            .restore_to_lru(&mut inner.messages_by_group_cache);
        snapshot
            .processed_messages
            .restore_to_lru(&mut inner.processed_messages_cache);
    }

    // ========================================================================
    // Group-Scoped Snapshot Support
    // ========================================================================

    /// Creates a snapshot containing only data for a specific group.
    ///
    /// This is used by the `MdkStorageProvider::create_group_snapshot` trait method
    /// to create rollback points that don't affect other groups. Unlike `create_snapshot()`
    /// which captures ALL data, this only captures data belonging to the specified group.
    ///
    /// # Concurrency
    ///
    /// This operation is **atomic**. It acquires a global read lock on the storage
    /// state, ensuring a consistent snapshot even in multi-threaded environments.
    ///
    /// # Arguments
    ///
    /// * `group_id` - The group to create a snapshot for.
    ///
    /// # Returns
    ///
    /// A `GroupScopedSnapshot` containing cloned copies of all state for that group.
    pub fn create_group_scoped_snapshot(&self, group_id: &GroupId) -> GroupScopedSnapshot {
        let inner = self.inner.read();

        // MLS storage uses MlsCodec serialization for group_id keys.
        // We need to use the same serialization to match the stored keys.
        let mls_group_id_bytes = mls_storage::MlsCodec::serialize(group_id.inner())
            .expect("Failed to serialize group_id for MLS lookup");

        // Filter MLS group data by group_id
        let mls_group_data: HashMap<(Vec<u8>, GroupDataType), Vec<u8>> = inner
            .mls_group_data
            .data
            .iter()
            .filter(|((gid, _), _)| *gid == mls_group_id_bytes)
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        // Get own leaf nodes for this group
        let mls_own_leaf_nodes = inner
            .mls_own_leaf_nodes
            .data
            .get(&mls_group_id_bytes)
            .cloned()
            .unwrap_or_default();

        // Filter proposals by group_id, keeping only the proposal_ref as the key
        let mls_proposals: HashMap<Vec<u8>, Vec<u8>> = inner
            .mls_proposals
            .data
            .iter()
            .filter(|((gid, _), _)| *gid == mls_group_id_bytes)
            .map(|((_, prop_ref), prop)| (prop_ref.clone(), prop.clone()))
            .collect();

        // Filter epoch key pairs by group_id
        let mls_epoch_key_pairs: HashMap<(Vec<u8>, u32), Vec<u8>> = inner
            .mls_epoch_key_pairs
            .data
            .iter()
            .filter(|((gid, _, _), _)| *gid == mls_group_id_bytes)
            .map(|((_, epoch_id, leaf_idx), kp)| ((epoch_id.clone(), *leaf_idx), kp.clone()))
            .collect();

        // Get MDK group data
        let group = inner.groups_cache.peek(group_id).cloned();

        let group_relays = inner
            .group_relays_cache
            .peek(group_id)
            .cloned()
            .unwrap_or_default();

        let group_exporter_secrets: HashMap<u64, GroupExporterSecret> = inner
            .group_exporter_secrets_cache
            .iter()
            .filter(|((gid, _), _)| gid == group_id)
            .map(|((_, epoch), secret)| (*epoch, secret.clone()))
            .collect();

        // Get current Unix timestamp
        let created_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("System time before Unix epoch")
            .as_secs();

        GroupScopedSnapshot {
            group_id: group_id.clone(),
            created_at,
            mls_group_data,
            mls_own_leaf_nodes,
            mls_proposals,
            mls_epoch_key_pairs,
            group,
            group_relays,
            group_exporter_secrets,
        }
    }

    /// Restores state for a specific group from a previously created group-scoped snapshot.
    ///
    /// This replaces all current in-memory state for the specified group with the state
    /// from the snapshot, leaving all other groups unaffected.
    ///
    /// # Concurrency
    ///
    /// This operation is **atomic**. It acquires a global write lock on the storage
    /// state, ensuring that the restore is consistent even in multi-threaded environments.
    ///
    /// # Arguments
    ///
    /// * `snapshot` - The group-scoped snapshot to restore from.
    pub fn restore_group_scoped_snapshot(&self, snapshot: GroupScopedSnapshot) {
        let mut inner = self.inner.write();
        let group_id = &snapshot.group_id;

        // MLS storage uses MlsCodec serialization for group_id keys.
        // We need to use the same serialization to match the stored keys.
        let mls_group_id_bytes = mls_storage::MlsCodec::serialize(group_id.inner())
            .expect("Failed to serialize group_id for MLS lookup");

        // 1. Remove existing data for this group

        // Remove MLS group data for this group
        inner
            .mls_group_data
            .data
            .retain(|(gid, _), _| *gid != mls_group_id_bytes);

        // Remove own leaf nodes for this group
        inner.mls_own_leaf_nodes.data.remove(&mls_group_id_bytes);

        // Remove proposals for this group
        inner
            .mls_proposals
            .data
            .retain(|(gid, _), _| *gid != mls_group_id_bytes);

        // Remove epoch key pairs for this group
        inner
            .mls_epoch_key_pairs
            .data
            .retain(|(gid, _, _), _| *gid != mls_group_id_bytes);

        // Remove from MDK caches
        // First, get the nostr_group_id if the group exists (for cache cleanup)
        let nostr_group_id = inner.groups_cache.peek(group_id).map(|g| g.nostr_group_id);
        inner.groups_cache.pop(group_id);
        if let Some(nostr_id) = nostr_group_id {
            inner.groups_by_nostr_id_cache.pop(&nostr_id);
        }

        inner.group_relays_cache.pop(group_id);

        // Remove all exporter secrets for this group
        let keys_to_remove: Vec<_> = inner
            .group_exporter_secrets_cache
            .iter()
            .filter(|((gid, _), _)| gid == group_id)
            .map(|(k, _)| k.clone())
            .collect();
        for key in keys_to_remove {
            inner.group_exporter_secrets_cache.pop(&key);
        }

        // 2. Restore from snapshot

        // Restore MLS group data
        for (key, value) in snapshot.mls_group_data {
            inner.mls_group_data.data.insert(key, value);
        }

        // Restore own leaf nodes
        if !snapshot.mls_own_leaf_nodes.is_empty() {
            inner
                .mls_own_leaf_nodes
                .data
                .insert(mls_group_id_bytes.clone(), snapshot.mls_own_leaf_nodes);
        }

        // Restore proposals (re-add group_id to the key)
        for (prop_ref, prop) in snapshot.mls_proposals {
            inner
                .mls_proposals
                .data
                .insert((mls_group_id_bytes.clone(), prop_ref), prop);
        }

        // Restore epoch key pairs (re-add group_id to the key)
        for ((epoch_id, leaf_idx), kp) in snapshot.mls_epoch_key_pairs {
            inner
                .mls_epoch_key_pairs
                .data
                .insert((mls_group_id_bytes.clone(), epoch_id, leaf_idx), kp);
        }

        // Restore MDK data
        if let Some(group) = snapshot.group {
            let nostr_id = group.nostr_group_id;
            inner.groups_cache.put(group_id.clone(), group.clone());
            inner.groups_by_nostr_id_cache.put(nostr_id, group);
        }

        if !snapshot.group_relays.is_empty() {
            inner
                .group_relays_cache
                .put(group_id.clone(), snapshot.group_relays);
        }

        for (epoch, secret) in snapshot.group_exporter_secrets {
            inner
                .group_exporter_secrets_cache
                .put((group_id.clone(), epoch), secret);
        }
    }

    /// Returns the current validation limits.
    pub fn limits(&self) -> &ValidationLimits {
        &self.limits
    }
}

/// Implementation of `MdkStorageProvider` for memory-based storage.
impl MdkStorageProvider for MdkMemoryStorage {
    /// Returns the backend type.
    ///
    /// # Returns
    ///
    /// [`Backend::Memory`] indicating this is a memory-based storage implementation.
    fn backend(&self) -> Backend {
        Backend::Memory
    }

    fn create_group_snapshot(&self, group_id: &GroupId, name: &str) -> Result<(), MdkStorageError> {
        // Create a group-scoped snapshot that only captures data for this group.
        // This ensures that rolling back this snapshot won't affect other groups.
        let snapshot = self.create_group_scoped_snapshot(group_id);
        self.group_snapshots
            .write()
            .insert((group_id.clone(), name.to_string()), snapshot);
        Ok(())
    }

    fn rollback_group_to_snapshot(
        &self,
        group_id: &GroupId,
        name: &str,
    ) -> Result<(), MdkStorageError> {
        let key = (group_id.clone(), name.to_string());
        // Remove and restore the snapshot (consume it)
        let snapshot = self
            .group_snapshots
            .write()
            .remove(&key)
            .ok_or_else(|| MdkStorageError::NotFound("Snapshot not found".to_string()))?;
        self.restore_group_scoped_snapshot(snapshot);
        Ok(())
    }

    fn release_group_snapshot(
        &self,
        group_id: &GroupId,
        name: &str,
    ) -> Result<(), MdkStorageError> {
        let key = (group_id.clone(), name.to_string());
        self.group_snapshots.write().remove(&key);
        Ok(())
    }

    fn list_group_snapshots(
        &self,
        group_id: &GroupId,
    ) -> Result<Vec<(String, u64)>, MdkStorageError> {
        let snapshots = self.group_snapshots.read();
        let mut result: Vec<(String, u64)> = snapshots
            .iter()
            .filter(|((gid, _), _)| gid == group_id)
            .map(|((_, name), snap)| (name.clone(), snap.created_at))
            .collect();
        // Sort by created_at ascending (oldest first)
        result.sort_by_key(|(_, created_at)| *created_at);
        Ok(result)
    }

    fn prune_expired_snapshots(&self, min_timestamp: u64) -> Result<usize, MdkStorageError> {
        let mut snapshots = self.group_snapshots.write();
        let initial_count = snapshots.len();
        snapshots.retain(|_, snap| snap.created_at >= min_timestamp);
        let pruned_count = initial_count - snapshots.len();
        Ok(pruned_count)
    }
}

// ============================================================================
// OpenMLS StorageProvider<1> Implementation
// ============================================================================

impl StorageProvider<STORAGE_PROVIDER_VERSION> for MdkMemoryStorage {
    type Error = MdkStorageError;

    // ========================================================================
    // Write Methods
    // ========================================================================

    fn write_mls_join_config<GroupId, MlsGroupJoinConfig>(
        &self,
        group_id: &GroupId,
        config: &MlsGroupJoinConfig,
    ) -> Result<(), Self::Error>
    where
        GroupId: traits::GroupId<STORAGE_PROVIDER_VERSION>,
        MlsGroupJoinConfig: traits::MlsGroupJoinConfig<STORAGE_PROVIDER_VERSION>,
    {
        self.inner
            .write()
            .mls_group_data
            .write(group_id, GroupDataType::JoinGroupConfig, config)
    }

    fn append_own_leaf_node<GroupId, LeafNode>(
        &self,
        group_id: &GroupId,
        leaf_node: &LeafNode,
    ) -> Result<(), Self::Error>
    where
        GroupId: traits::GroupId<STORAGE_PROVIDER_VERSION>,
        LeafNode: traits::LeafNode<STORAGE_PROVIDER_VERSION>,
    {
        self.inner
            .write()
            .mls_own_leaf_nodes
            .append(group_id, leaf_node)
    }

    fn queue_proposal<GroupId, ProposalRef, QueuedProposal>(
        &self,
        group_id: &GroupId,
        proposal_ref: &ProposalRef,
        proposal: &QueuedProposal,
    ) -> Result<(), Self::Error>
    where
        GroupId: traits::GroupId<STORAGE_PROVIDER_VERSION>,
        ProposalRef: traits::ProposalRef<STORAGE_PROVIDER_VERSION>,
        QueuedProposal: traits::QueuedProposal<STORAGE_PROVIDER_VERSION>,
    {
        self.inner
            .write()
            .mls_proposals
            .queue(group_id, proposal_ref, proposal)
    }

    fn write_tree<GroupId, TreeSync>(
        &self,
        group_id: &GroupId,
        tree: &TreeSync,
    ) -> Result<(), Self::Error>
    where
        GroupId: traits::GroupId<STORAGE_PROVIDER_VERSION>,
        TreeSync: traits::TreeSync<STORAGE_PROVIDER_VERSION>,
    {
        self.inner
            .write()
            .mls_group_data
            .write(group_id, GroupDataType::Tree, tree)
    }

    fn write_interim_transcript_hash<GroupId, InterimTranscriptHash>(
        &self,
        group_id: &GroupId,
        interim_transcript_hash: &InterimTranscriptHash,
    ) -> Result<(), Self::Error>
    where
        GroupId: traits::GroupId<STORAGE_PROVIDER_VERSION>,
        InterimTranscriptHash: traits::InterimTranscriptHash<STORAGE_PROVIDER_VERSION>,
    {
        self.inner.write().mls_group_data.write(
            group_id,
            GroupDataType::InterimTranscriptHash,
            interim_transcript_hash,
        )
    }

    fn write_context<GroupId, GroupContext>(
        &self,
        group_id: &GroupId,
        group_context: &GroupContext,
    ) -> Result<(), Self::Error>
    where
        GroupId: traits::GroupId<STORAGE_PROVIDER_VERSION>,
        GroupContext: traits::GroupContext<STORAGE_PROVIDER_VERSION>,
    {
        self.inner
            .write()
            .mls_group_data
            .write(group_id, GroupDataType::Context, group_context)
    }

    fn write_confirmation_tag<GroupId, ConfirmationTag>(
        &self,
        group_id: &GroupId,
        confirmation_tag: &ConfirmationTag,
    ) -> Result<(), Self::Error>
    where
        GroupId: traits::GroupId<STORAGE_PROVIDER_VERSION>,
        ConfirmationTag: traits::ConfirmationTag<STORAGE_PROVIDER_VERSION>,
    {
        self.inner.write().mls_group_data.write(
            group_id,
            GroupDataType::ConfirmationTag,
            confirmation_tag,
        )
    }

    fn write_group_state<GroupState, GroupId>(
        &self,
        group_id: &GroupId,
        group_state: &GroupState,
    ) -> Result<(), Self::Error>
    where
        GroupState: traits::GroupState<STORAGE_PROVIDER_VERSION>,
        GroupId: traits::GroupId<STORAGE_PROVIDER_VERSION>,
    {
        self.inner
            .write()
            .mls_group_data
            .write(group_id, GroupDataType::GroupState, group_state)
    }

    fn write_message_secrets<GroupId, MessageSecrets>(
        &self,
        group_id: &GroupId,
        message_secrets: &MessageSecrets,
    ) -> Result<(), Self::Error>
    where
        GroupId: traits::GroupId<STORAGE_PROVIDER_VERSION>,
        MessageSecrets: traits::MessageSecrets<STORAGE_PROVIDER_VERSION>,
    {
        self.inner.write().mls_group_data.write(
            group_id,
            GroupDataType::MessageSecrets,
            message_secrets,
        )
    }

    fn write_resumption_psk_store<GroupId, ResumptionPskStore>(
        &self,
        group_id: &GroupId,
        resumption_psk_store: &ResumptionPskStore,
    ) -> Result<(), Self::Error>
    where
        GroupId: traits::GroupId<STORAGE_PROVIDER_VERSION>,
        ResumptionPskStore: traits::ResumptionPskStore<STORAGE_PROVIDER_VERSION>,
    {
        self.inner.write().mls_group_data.write(
            group_id,
            GroupDataType::ResumptionPskStore,
            resumption_psk_store,
        )
    }

    fn write_own_leaf_index<GroupId, LeafNodeIndex>(
        &self,
        group_id: &GroupId,
        own_leaf_index: &LeafNodeIndex,
    ) -> Result<(), Self::Error>
    where
        GroupId: traits::GroupId<STORAGE_PROVIDER_VERSION>,
        LeafNodeIndex: traits::LeafNodeIndex<STORAGE_PROVIDER_VERSION>,
    {
        self.inner.write().mls_group_data.write(
            group_id,
            GroupDataType::OwnLeafIndex,
            own_leaf_index,
        )
    }

    fn write_group_epoch_secrets<GroupId, GroupEpochSecrets>(
        &self,
        group_id: &GroupId,
        group_epoch_secrets: &GroupEpochSecrets,
    ) -> Result<(), Self::Error>
    where
        GroupId: traits::GroupId<STORAGE_PROVIDER_VERSION>,
        GroupEpochSecrets: traits::GroupEpochSecrets<STORAGE_PROVIDER_VERSION>,
    {
        self.inner.write().mls_group_data.write(
            group_id,
            GroupDataType::GroupEpochSecrets,
            group_epoch_secrets,
        )
    }

    fn write_signature_key_pair<SignaturePublicKey, SignatureKeyPair>(
        &self,
        public_key: &SignaturePublicKey,
        signature_key_pair: &SignatureKeyPair,
    ) -> Result<(), Self::Error>
    where
        SignaturePublicKey: traits::SignaturePublicKey<STORAGE_PROVIDER_VERSION>,
        SignatureKeyPair: traits::SignatureKeyPair<STORAGE_PROVIDER_VERSION>,
    {
        self.inner
            .write()
            .mls_signature_keys
            .write(public_key, signature_key_pair)
    }

    fn write_encryption_key_pair<EncryptionKey, HpkeKeyPair>(
        &self,
        public_key: &EncryptionKey,
        key_pair: &HpkeKeyPair,
    ) -> Result<(), Self::Error>
    where
        EncryptionKey: traits::EncryptionKey<STORAGE_PROVIDER_VERSION>,
        HpkeKeyPair: traits::HpkeKeyPair<STORAGE_PROVIDER_VERSION>,
    {
        self.inner
            .write()
            .mls_encryption_keys
            .write(public_key, key_pair)
    }

    fn write_encryption_epoch_key_pairs<GroupId, EpochKey, HpkeKeyPair>(
        &self,
        group_id: &GroupId,
        epoch: &EpochKey,
        leaf_index: u32,
        key_pairs: &[HpkeKeyPair],
    ) -> Result<(), Self::Error>
    where
        GroupId: traits::GroupId<STORAGE_PROVIDER_VERSION>,
        EpochKey: traits::EpochKey<STORAGE_PROVIDER_VERSION>,
        HpkeKeyPair: traits::HpkeKeyPair<STORAGE_PROVIDER_VERSION>,
    {
        self.inner
            .write()
            .mls_epoch_key_pairs
            .write(group_id, epoch, leaf_index, key_pairs)
    }

    fn write_key_package<HashReference, KeyPackage>(
        &self,
        hash_ref: &HashReference,
        key_package: &KeyPackage,
    ) -> Result<(), Self::Error>
    where
        HashReference: traits::HashReference<STORAGE_PROVIDER_VERSION>,
        KeyPackage: traits::KeyPackage<STORAGE_PROVIDER_VERSION>,
    {
        self.inner
            .write()
            .mls_key_packages
            .write(hash_ref, key_package)
    }

    fn write_psk<PskId, PskBundle>(
        &self,
        psk_id: &PskId,
        psk: &PskBundle,
    ) -> Result<(), Self::Error>
    where
        PskId: traits::PskId<STORAGE_PROVIDER_VERSION>,
        PskBundle: traits::PskBundle<STORAGE_PROVIDER_VERSION>,
    {
        self.inner.write().mls_psks.write(psk_id, psk)
    }

    // ========================================================================
    // Read Methods
    // ========================================================================

    fn mls_group_join_config<GroupId, MlsGroupJoinConfig>(
        &self,
        group_id: &GroupId,
    ) -> Result<Option<MlsGroupJoinConfig>, Self::Error>
    where
        GroupId: traits::GroupId<STORAGE_PROVIDER_VERSION>,
        MlsGroupJoinConfig: traits::MlsGroupJoinConfig<STORAGE_PROVIDER_VERSION>,
    {
        self.inner
            .read()
            .mls_group_data
            .read(group_id, GroupDataType::JoinGroupConfig)
    }

    fn own_leaf_nodes<GroupId, LeafNode>(
        &self,
        group_id: &GroupId,
    ) -> Result<Vec<LeafNode>, Self::Error>
    where
        GroupId: traits::GroupId<STORAGE_PROVIDER_VERSION>,
        LeafNode: traits::LeafNode<STORAGE_PROVIDER_VERSION>,
    {
        self.inner.read().mls_own_leaf_nodes.read(group_id)
    }

    fn queued_proposal_refs<GroupId, ProposalRef>(
        &self,
        group_id: &GroupId,
    ) -> Result<Vec<ProposalRef>, Self::Error>
    where
        GroupId: traits::GroupId<STORAGE_PROVIDER_VERSION>,
        ProposalRef: traits::ProposalRef<STORAGE_PROVIDER_VERSION>,
    {
        self.inner.read().mls_proposals.read_refs(group_id)
    }

    fn queued_proposals<GroupId, ProposalRef, QueuedProposal>(
        &self,
        group_id: &GroupId,
    ) -> Result<Vec<(ProposalRef, QueuedProposal)>, Self::Error>
    where
        GroupId: traits::GroupId<STORAGE_PROVIDER_VERSION>,
        ProposalRef: traits::ProposalRef<STORAGE_PROVIDER_VERSION>,
        QueuedProposal: traits::QueuedProposal<STORAGE_PROVIDER_VERSION>,
    {
        self.inner.read().mls_proposals.read_proposals(group_id)
    }

    fn tree<GroupId, TreeSync>(&self, group_id: &GroupId) -> Result<Option<TreeSync>, Self::Error>
    where
        GroupId: traits::GroupId<STORAGE_PROVIDER_VERSION>,
        TreeSync: traits::TreeSync<STORAGE_PROVIDER_VERSION>,
    {
        self.inner
            .read()
            .mls_group_data
            .read(group_id, GroupDataType::Tree)
    }

    fn group_context<GroupId, GroupContext>(
        &self,
        group_id: &GroupId,
    ) -> Result<Option<GroupContext>, Self::Error>
    where
        GroupId: traits::GroupId<STORAGE_PROVIDER_VERSION>,
        GroupContext: traits::GroupContext<STORAGE_PROVIDER_VERSION>,
    {
        self.inner
            .read()
            .mls_group_data
            .read(group_id, GroupDataType::Context)
    }

    fn interim_transcript_hash<GroupId, InterimTranscriptHash>(
        &self,
        group_id: &GroupId,
    ) -> Result<Option<InterimTranscriptHash>, Self::Error>
    where
        GroupId: traits::GroupId<STORAGE_PROVIDER_VERSION>,
        InterimTranscriptHash: traits::InterimTranscriptHash<STORAGE_PROVIDER_VERSION>,
    {
        self.inner
            .read()
            .mls_group_data
            .read(group_id, GroupDataType::InterimTranscriptHash)
    }

    fn confirmation_tag<GroupId, ConfirmationTag>(
        &self,
        group_id: &GroupId,
    ) -> Result<Option<ConfirmationTag>, Self::Error>
    where
        GroupId: traits::GroupId<STORAGE_PROVIDER_VERSION>,
        ConfirmationTag: traits::ConfirmationTag<STORAGE_PROVIDER_VERSION>,
    {
        self.inner
            .read()
            .mls_group_data
            .read(group_id, GroupDataType::ConfirmationTag)
    }

    fn group_state<GroupState, GroupId>(
        &self,
        group_id: &GroupId,
    ) -> Result<Option<GroupState>, Self::Error>
    where
        GroupState: traits::GroupState<STORAGE_PROVIDER_VERSION>,
        GroupId: traits::GroupId<STORAGE_PROVIDER_VERSION>,
    {
        self.inner
            .read()
            .mls_group_data
            .read(group_id, GroupDataType::GroupState)
    }

    fn message_secrets<GroupId, MessageSecrets>(
        &self,
        group_id: &GroupId,
    ) -> Result<Option<MessageSecrets>, Self::Error>
    where
        GroupId: traits::GroupId<STORAGE_PROVIDER_VERSION>,
        MessageSecrets: traits::MessageSecrets<STORAGE_PROVIDER_VERSION>,
    {
        self.inner
            .read()
            .mls_group_data
            .read(group_id, GroupDataType::MessageSecrets)
    }

    fn resumption_psk_store<GroupId, ResumptionPskStore>(
        &self,
        group_id: &GroupId,
    ) -> Result<Option<ResumptionPskStore>, Self::Error>
    where
        GroupId: traits::GroupId<STORAGE_PROVIDER_VERSION>,
        ResumptionPskStore: traits::ResumptionPskStore<STORAGE_PROVIDER_VERSION>,
    {
        self.inner
            .read()
            .mls_group_data
            .read(group_id, GroupDataType::ResumptionPskStore)
    }

    fn own_leaf_index<GroupId, LeafNodeIndex>(
        &self,
        group_id: &GroupId,
    ) -> Result<Option<LeafNodeIndex>, Self::Error>
    where
        GroupId: traits::GroupId<STORAGE_PROVIDER_VERSION>,
        LeafNodeIndex: traits::LeafNodeIndex<STORAGE_PROVIDER_VERSION>,
    {
        self.inner
            .read()
            .mls_group_data
            .read(group_id, GroupDataType::OwnLeafIndex)
    }

    fn group_epoch_secrets<GroupId, GroupEpochSecrets>(
        &self,
        group_id: &GroupId,
    ) -> Result<Option<GroupEpochSecrets>, Self::Error>
    where
        GroupId: traits::GroupId<STORAGE_PROVIDER_VERSION>,
        GroupEpochSecrets: traits::GroupEpochSecrets<STORAGE_PROVIDER_VERSION>,
    {
        self.inner
            .read()
            .mls_group_data
            .read(group_id, GroupDataType::GroupEpochSecrets)
    }

    fn signature_key_pair<SignaturePublicKey, SignatureKeyPair>(
        &self,
        public_key: &SignaturePublicKey,
    ) -> Result<Option<SignatureKeyPair>, Self::Error>
    where
        SignaturePublicKey: traits::SignaturePublicKey<STORAGE_PROVIDER_VERSION>,
        SignatureKeyPair: traits::SignatureKeyPair<STORAGE_PROVIDER_VERSION>,
    {
        self.inner.read().mls_signature_keys.read(public_key)
    }

    fn encryption_key_pair<HpkeKeyPair, EncryptionKey>(
        &self,
        public_key: &EncryptionKey,
    ) -> Result<Option<HpkeKeyPair>, Self::Error>
    where
        HpkeKeyPair: traits::HpkeKeyPair<STORAGE_PROVIDER_VERSION>,
        EncryptionKey: traits::EncryptionKey<STORAGE_PROVIDER_VERSION>,
    {
        self.inner.read().mls_encryption_keys.read(public_key)
    }

    fn encryption_epoch_key_pairs<GroupId, EpochKey, HpkeKeyPair>(
        &self,
        group_id: &GroupId,
        epoch: &EpochKey,
        leaf_index: u32,
    ) -> Result<Vec<HpkeKeyPair>, Self::Error>
    where
        GroupId: traits::GroupId<STORAGE_PROVIDER_VERSION>,
        EpochKey: traits::EpochKey<STORAGE_PROVIDER_VERSION>,
        HpkeKeyPair: traits::HpkeKeyPair<STORAGE_PROVIDER_VERSION>,
    {
        self.inner
            .read()
            .mls_epoch_key_pairs
            .read(group_id, epoch, leaf_index)
    }

    fn key_package<HashReference, KeyPackage>(
        &self,
        hash_ref: &HashReference,
    ) -> Result<Option<KeyPackage>, Self::Error>
    where
        HashReference: traits::HashReference<STORAGE_PROVIDER_VERSION>,
        KeyPackage: traits::KeyPackage<STORAGE_PROVIDER_VERSION>,
    {
        self.inner.read().mls_key_packages.read(hash_ref)
    }

    fn psk<PskBundle, PskId>(&self, psk_id: &PskId) -> Result<Option<PskBundle>, Self::Error>
    where
        PskBundle: traits::PskBundle<STORAGE_PROVIDER_VERSION>,
        PskId: traits::PskId<STORAGE_PROVIDER_VERSION>,
    {
        self.inner.read().mls_psks.read(psk_id)
    }

    // ========================================================================
    // Delete Methods
    // ========================================================================

    fn remove_proposal<GroupId, ProposalRef>(
        &self,
        group_id: &GroupId,
        proposal_ref: &ProposalRef,
    ) -> Result<(), Self::Error>
    where
        GroupId: traits::GroupId<STORAGE_PROVIDER_VERSION>,
        ProposalRef: traits::ProposalRef<STORAGE_PROVIDER_VERSION>,
    {
        self.inner
            .write()
            .mls_proposals
            .remove(group_id, proposal_ref)
    }

    fn delete_own_leaf_nodes<GroupId>(&self, group_id: &GroupId) -> Result<(), Self::Error>
    where
        GroupId: traits::GroupId<STORAGE_PROVIDER_VERSION>,
    {
        self.inner.write().mls_own_leaf_nodes.delete(group_id)
    }

    fn delete_group_config<GroupId>(&self, group_id: &GroupId) -> Result<(), Self::Error>
    where
        GroupId: traits::GroupId<STORAGE_PROVIDER_VERSION>,
    {
        self.inner
            .write()
            .mls_group_data
            .delete(group_id, GroupDataType::JoinGroupConfig)
    }

    fn delete_tree<GroupId>(&self, group_id: &GroupId) -> Result<(), Self::Error>
    where
        GroupId: traits::GroupId<STORAGE_PROVIDER_VERSION>,
    {
        self.inner
            .write()
            .mls_group_data
            .delete(group_id, GroupDataType::Tree)
    }

    fn delete_confirmation_tag<GroupId>(&self, group_id: &GroupId) -> Result<(), Self::Error>
    where
        GroupId: traits::GroupId<STORAGE_PROVIDER_VERSION>,
    {
        self.inner
            .write()
            .mls_group_data
            .delete(group_id, GroupDataType::ConfirmationTag)
    }

    fn delete_group_state<GroupId>(&self, group_id: &GroupId) -> Result<(), Self::Error>
    where
        GroupId: traits::GroupId<STORAGE_PROVIDER_VERSION>,
    {
        self.inner
            .write()
            .mls_group_data
            .delete(group_id, GroupDataType::GroupState)
    }

    fn delete_context<GroupId>(&self, group_id: &GroupId) -> Result<(), Self::Error>
    where
        GroupId: traits::GroupId<STORAGE_PROVIDER_VERSION>,
    {
        self.inner
            .write()
            .mls_group_data
            .delete(group_id, GroupDataType::Context)
    }

    fn delete_interim_transcript_hash<GroupId>(&self, group_id: &GroupId) -> Result<(), Self::Error>
    where
        GroupId: traits::GroupId<STORAGE_PROVIDER_VERSION>,
    {
        self.inner
            .write()
            .mls_group_data
            .delete(group_id, GroupDataType::InterimTranscriptHash)
    }

    fn delete_message_secrets<GroupId>(&self, group_id: &GroupId) -> Result<(), Self::Error>
    where
        GroupId: traits::GroupId<STORAGE_PROVIDER_VERSION>,
    {
        self.inner
            .write()
            .mls_group_data
            .delete(group_id, GroupDataType::MessageSecrets)
    }

    fn delete_all_resumption_psk_secrets<GroupId>(
        &self,
        group_id: &GroupId,
    ) -> Result<(), Self::Error>
    where
        GroupId: traits::GroupId<STORAGE_PROVIDER_VERSION>,
    {
        self.inner
            .write()
            .mls_group_data
            .delete(group_id, GroupDataType::ResumptionPskStore)
    }

    fn delete_own_leaf_index<GroupId>(&self, group_id: &GroupId) -> Result<(), Self::Error>
    where
        GroupId: traits::GroupId<STORAGE_PROVIDER_VERSION>,
    {
        self.inner
            .write()
            .mls_group_data
            .delete(group_id, GroupDataType::OwnLeafIndex)
    }

    fn delete_group_epoch_secrets<GroupId>(&self, group_id: &GroupId) -> Result<(), Self::Error>
    where
        GroupId: traits::GroupId<STORAGE_PROVIDER_VERSION>,
    {
        self.inner
            .write()
            .mls_group_data
            .delete(group_id, GroupDataType::GroupEpochSecrets)
    }

    fn clear_proposal_queue<GroupId, ProposalRef>(
        &self,
        group_id: &GroupId,
    ) -> Result<(), Self::Error>
    where
        GroupId: traits::GroupId<STORAGE_PROVIDER_VERSION>,
        ProposalRef: traits::ProposalRef<STORAGE_PROVIDER_VERSION>,
    {
        self.inner.write().mls_proposals.clear(group_id)
    }

    fn delete_signature_key_pair<SignaturePublicKey>(
        &self,
        public_key: &SignaturePublicKey,
    ) -> Result<(), Self::Error>
    where
        SignaturePublicKey: traits::SignaturePublicKey<STORAGE_PROVIDER_VERSION>,
    {
        self.inner.write().mls_signature_keys.delete(public_key)
    }

    fn delete_encryption_key_pair<EncryptionKey>(
        &self,
        public_key: &EncryptionKey,
    ) -> Result<(), Self::Error>
    where
        EncryptionKey: traits::EncryptionKey<STORAGE_PROVIDER_VERSION>,
    {
        self.inner.write().mls_encryption_keys.delete(public_key)
    }

    fn delete_encryption_epoch_key_pairs<GroupId, EpochKey>(
        &self,
        group_id: &GroupId,
        epoch: &EpochKey,
        leaf_index: u32,
    ) -> Result<(), Self::Error>
    where
        GroupId: traits::GroupId<STORAGE_PROVIDER_VERSION>,
        EpochKey: traits::EpochKey<STORAGE_PROVIDER_VERSION>,
    {
        self.inner
            .write()
            .mls_epoch_key_pairs
            .delete(group_id, epoch, leaf_index)
    }

    fn delete_key_package<HashReference>(&self, hash_ref: &HashReference) -> Result<(), Self::Error>
    where
        HashReference: traits::HashReference<STORAGE_PROVIDER_VERSION>,
    {
        self.inner.write().mls_key_packages.delete(hash_ref)
    }

    fn delete_psk<PskId>(&self, psk_id: &PskId) -> Result<(), Self::Error>
    where
        PskId: traits::PskId<STORAGE_PROVIDER_VERSION>,
    {
        self.inner.write().mls_psks.delete(psk_id)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use mdk_storage_traits::groups::GroupStorage;
    use mdk_storage_traits::groups::types::{
        Group, GroupExporterSecret, GroupState, SelfUpdateState,
    };
    use mdk_storage_traits::messages::MessageStorage;
    use mdk_storage_traits::messages::error::MessageError;
    use mdk_storage_traits::messages::types::{Message, MessageState, ProcessedMessageState};
    use mdk_storage_traits::test_utils::crypto_utils::generate_random_bytes;
    use mdk_storage_traits::welcomes::WelcomeStorage;
    use mdk_storage_traits::welcomes::types::{ProcessedWelcomeState, Welcome, WelcomeState};
    use mdk_storage_traits::{GroupId, MdkStorageProvider, Secret};
    use nostr::{EventId, Kind, PublicKey, RelayUrl, Tags, Timestamp, UnsignedEvent};

    use super::*;

    fn create_test_group_id() -> GroupId {
        GroupId::from_slice(&[1, 2, 3, 4])
    }

    #[test]
    fn test_new() {
        let nostr_storage = MdkMemoryStorage::new();
        assert_eq!(nostr_storage.backend(), Backend::Memory);
    }

    #[test]
    fn test_default() {
        let nostr_storage = MdkMemoryStorage::default();
        assert_eq!(nostr_storage.backend(), Backend::Memory);
    }

    #[test]
    fn test_backend_type() {
        let nostr_storage = MdkMemoryStorage::default();
        assert_eq!(nostr_storage.backend(), Backend::Memory);
        assert!(!nostr_storage.backend().is_persistent());
    }

    #[test]
    fn test_storage_is_memory_based() {
        let nostr_storage = MdkMemoryStorage::default();
        assert!(!nostr_storage.backend().is_persistent());
    }

    #[test]
    fn test_compare_backend_types() {
        let nostr_storage = MdkMemoryStorage::default();
        let memory_backend = nostr_storage.backend();
        assert_eq!(memory_backend, Backend::Memory);
        assert_ne!(memory_backend, Backend::SQLite);
    }

    #[test]
    fn test_create_multiple_instances() {
        let nostr_storage1 = MdkMemoryStorage::new();
        let nostr_storage2 = MdkMemoryStorage::new();

        assert_eq!(nostr_storage1.backend(), nostr_storage2.backend());
        assert_eq!(nostr_storage1.backend(), Backend::Memory);
        assert_eq!(nostr_storage2.backend(), Backend::Memory);
    }

    #[test]
    fn test_group_cache() {
        let nostr_storage = MdkMemoryStorage::default();
        let mls_group_id = create_test_group_id();
        let nostr_group_id = generate_random_bytes(32).try_into().unwrap();
        let image_hash = Some(generate_random_bytes(32).try_into().unwrap());
        let image_key = Some(Secret::new(generate_random_bytes(32).try_into().unwrap()));
        let image_nonce = Some(Secret::new(generate_random_bytes(12).try_into().unwrap()));
        let group = Group {
            mls_group_id: mls_group_id.clone(),
            nostr_group_id,
            name: "Test Group".to_string(),
            description: "A test group".to_string(),
            admin_pubkeys: BTreeSet::new(),
            last_message_id: None,
            last_message_at: None,
            last_message_processed_at: None,
            epoch: 0,
            state: GroupState::Active,
            image_hash,
            image_key,
            image_nonce,
            self_update_state: SelfUpdateState::Required,
        };
        nostr_storage.save_group(group.clone()).unwrap();
        let found_group = nostr_storage
            .find_group_by_mls_group_id(&mls_group_id)
            .unwrap()
            .unwrap();
        assert_eq!(found_group.mls_group_id, mls_group_id);
        assert_eq!(found_group.nostr_group_id, nostr_group_id);

        // Verify the group is in the cache
        {
            let inner = nostr_storage.inner.read();
            let cache = &inner.groups_cache;
            assert!(cache.contains(&mls_group_id));
        }
        {
            let inner = nostr_storage.inner.read();
            let cache = &inner.groups_by_nostr_id_cache;
            assert!(cache.contains(&nostr_group_id));
        }
    }

    #[test]
    fn test_group_relays() {
        let nostr_storage = MdkMemoryStorage::default();
        let mls_group_id = create_test_group_id();
        let nostr_group_id = generate_random_bytes(32).try_into().unwrap();
        let image_hash = Some(generate_random_bytes(32).try_into().unwrap());
        let image_key = Some(Secret::new(generate_random_bytes(32).try_into().unwrap()));
        let image_nonce = Some(Secret::new(generate_random_bytes(12).try_into().unwrap()));
        let group = Group {
            mls_group_id: mls_group_id.clone(),
            nostr_group_id,
            name: "Another Test Group".to_string(),
            description: "Another test group".to_string(),
            admin_pubkeys: BTreeSet::new(),
            last_message_id: None,
            last_message_at: None,
            last_message_processed_at: None,
            epoch: 0,
            state: GroupState::Active,
            image_hash,
            image_key,
            image_nonce,
            self_update_state: SelfUpdateState::Required,
        };
        nostr_storage.save_group(group.clone()).unwrap();
        let relay_url1 = RelayUrl::parse("wss://relay1.example.com").unwrap();
        let relay_url2 = RelayUrl::parse("wss://relay2.example.com").unwrap();
        let relays = BTreeSet::from([relay_url1, relay_url2]);
        nostr_storage
            .replace_group_relays(&mls_group_id, relays)
            .unwrap();
        let found_relays = nostr_storage.group_relays(&mls_group_id).unwrap();
        assert_eq!(found_relays.len(), 2);

        // Check that they're in the cache
        {
            let inner = nostr_storage.inner.read();
            let cache = &inner.group_relays_cache;
            assert!(cache.contains(&mls_group_id));
            if let Some(relays) = cache.peek(&mls_group_id) {
                assert_eq!(relays.len(), 2);
            } else {
                panic!("Group relays not found in cache");
            }
        }
    }

    #[test]
    fn test_group_exporter_secret_cache() {
        let nostr_storage = MdkMemoryStorage::default();
        let mls_group_id = create_test_group_id();
        let nostr_group_id = generate_random_bytes(32).try_into().unwrap();
        let image_hash = Some(generate_random_bytes(32).try_into().unwrap());
        let image_key = Some(Secret::new(generate_random_bytes(32).try_into().unwrap()));
        let image_nonce = Some(Secret::new(generate_random_bytes(12).try_into().unwrap()));
        let group = Group {
            mls_group_id: mls_group_id.clone(),
            nostr_group_id,
            name: "Test Group".to_string(),
            description: "A test group".to_string(),
            admin_pubkeys: BTreeSet::new(),
            last_message_id: None,
            last_message_at: None,
            last_message_processed_at: None,
            epoch: 0,
            state: GroupState::Active,
            image_hash,
            image_key,
            image_nonce,
            self_update_state: SelfUpdateState::Required,
        };
        nostr_storage.save_group(group.clone()).unwrap();
        let group_exporter_secret_0 = GroupExporterSecret {
            mls_group_id: mls_group_id.clone(),
            epoch: 0,
            secret: Secret::new([0u8; 32]),
        };
        let group_exporter_secret_1 = GroupExporterSecret {
            mls_group_id: mls_group_id.clone(),
            epoch: 1,
            secret: Secret::new([0u8; 32]),
        };
        nostr_storage
            .save_group_exporter_secret(group_exporter_secret_0.clone())
            .unwrap();
        nostr_storage
            .save_group_exporter_secret(group_exporter_secret_1.clone())
            .unwrap();
        let found_secret_0 = nostr_storage
            .get_group_exporter_secret(&mls_group_id, 0)
            .unwrap()
            .unwrap();
        assert_eq!(found_secret_0, group_exporter_secret_0);
        let found_secret_1 = nostr_storage
            .get_group_exporter_secret(&mls_group_id, 1)
            .unwrap()
            .unwrap();
        assert_eq!(found_secret_1, group_exporter_secret_1);
        let non_existent_secret = nostr_storage
            .get_group_exporter_secret(&mls_group_id, 999)
            .unwrap();
        assert!(non_existent_secret.is_none());

        // Check cache
        {
            let inner = nostr_storage.inner.read();
            let cache = &inner.group_exporter_secrets_cache;
            assert!(cache.contains(&(mls_group_id.clone(), 0)));
            assert!(cache.contains(&(mls_group_id.clone(), 1)));
            assert!(!cache.contains(&(mls_group_id.clone(), 999)));
        }
    }

    #[test]
    fn test_welcome_cache() {
        let nostr_storage = MdkMemoryStorage::default();

        // Create a test event ID
        let event_id = EventId::all_zeros();
        let wrapper_id = EventId::all_zeros();

        // Create a test pubkey
        let pubkey =
            PublicKey::from_hex("aabbccddeeffaabbccddeeffaabbccddeeffaabbccddeeffaabbccddeeffaabb")
                .unwrap();

        // Create a test welcome
        let mls_group_id = create_test_group_id();
        let nostr_group_id = generate_random_bytes(32).try_into().unwrap();
        let welcome = Welcome {
            id: event_id,
            event: UnsignedEvent::new(
                pubkey,
                Timestamp::now(),
                Kind::MlsWelcome,
                Tags::new(),
                "test".to_string(),
            ),
            mls_group_id: mls_group_id.clone(),
            nostr_group_id,
            group_name: "Test Welcome Group".to_string(),
            group_description: "A test welcome group".to_string(),
            group_image_key: None,
            group_image_hash: None,
            group_image_nonce: None,
            group_admin_pubkeys: BTreeSet::from([pubkey]),
            group_relays: BTreeSet::from([RelayUrl::parse("wss://relay.example.com").unwrap()]),
            welcomer: pubkey,
            member_count: 2,
            state: WelcomeState::Pending,
            wrapper_event_id: wrapper_id,
        };

        // Save the welcome
        let result = nostr_storage.save_welcome(welcome.clone());
        assert!(result.is_ok());

        // Find the welcome by event ID
        let found_welcome = nostr_storage.find_welcome_by_event_id(&event_id);
        assert!(found_welcome.is_ok());
        let found_welcome = found_welcome.unwrap().unwrap();
        assert_eq!(found_welcome.id, event_id);
        assert_eq!(found_welcome.mls_group_id, mls_group_id);

        // Check that it's in the cache
        {
            let inner = nostr_storage.inner.read();
            let cache = &inner.welcomes_cache;
            assert!(cache.contains(&event_id));
        }

        // Create a test processed welcome
        let processed_welcome = ProcessedWelcome {
            wrapper_event_id: wrapper_id,
            welcome_event_id: Some(event_id),
            processed_at: Timestamp::now(),
            state: ProcessedWelcomeState::Processed,
            failure_reason: None,
        };

        // Save the processed welcome
        let result = nostr_storage.save_processed_welcome(processed_welcome.clone());
        assert!(result.is_ok());

        // Find the processed welcome by event ID
        let found_processed_welcome = nostr_storage.find_processed_welcome_by_event_id(&wrapper_id);
        assert!(found_processed_welcome.is_ok());
        let found_processed_welcome = found_processed_welcome.unwrap().unwrap();
        assert_eq!(found_processed_welcome.wrapper_event_id, wrapper_id);
        assert_eq!(found_processed_welcome.welcome_event_id, Some(event_id));

        // Check that it's in the cache
        {
            let inner = nostr_storage.inner.read();
            let cache = &inner.processed_welcomes_cache;
            assert!(cache.contains(&wrapper_id));
        }
    }

    #[test]
    fn test_message_cache() {
        let nostr_storage = MdkMemoryStorage::default();
        let mls_group_id = create_test_group_id();
        let nostr_group_id = generate_random_bytes(32).try_into().unwrap();
        let image_hash = Some(generate_random_bytes(32).try_into().unwrap());
        let image_key = Some(Secret::new(generate_random_bytes(32).try_into().unwrap()));
        let image_nonce = Some(Secret::new(generate_random_bytes(12).try_into().unwrap()));
        let group = Group {
            mls_group_id: mls_group_id.clone(),
            nostr_group_id,
            name: "Message Test Group".to_string(),
            description: "A group for testing messages".to_string(),
            admin_pubkeys: BTreeSet::new(),
            last_message_id: None,
            last_message_at: None,
            last_message_processed_at: None,
            epoch: 0,
            state: GroupState::Active,
            image_hash,
            image_key,
            image_nonce,
            self_update_state: SelfUpdateState::Required,
        };
        nostr_storage.save_group(group.clone()).unwrap();
        let event_id = EventId::all_zeros();
        let wrapper_id = EventId::all_zeros();
        let pubkey =
            PublicKey::from_hex("aabbccddeeffaabbccddeeffaabbccddeeffaabbccddeeffaabbccddeeffaabb")
                .unwrap();
        let now = Timestamp::now();
        let message = Message {
            id: event_id,
            pubkey,
            kind: Kind::MlsGroupMessage,
            mls_group_id: mls_group_id.clone(),
            created_at: now,
            processed_at: now,
            content: "Hello, world!".to_string(),
            tags: Tags::new(),
            event: UnsignedEvent::new(
                pubkey,
                now,
                Kind::MlsGroupMessage,
                Tags::new(),
                "Hello, world!".to_string(),
            ),
            wrapper_event_id: wrapper_id,
            state: MessageState::Created,
            epoch: None,
        };
        nostr_storage.save_message(message.clone()).unwrap();
        let found_message = nostr_storage
            .find_message_by_event_id(&mls_group_id, &event_id)
            .unwrap()
            .unwrap();
        assert_eq!(found_message.id, event_id);
        assert_eq!(found_message.mls_group_id, mls_group_id);

        // Check caches
        {
            let inner = nostr_storage.inner.read();
            let cache = &inner.messages_cache;
            assert!(cache.contains(&event_id));
        }
        {
            // Verify save_message populated the messages_by_group_cache correctly
            let inner = nostr_storage.inner.read();
            let cache = &inner.messages_by_group_cache;
            assert!(cache.contains(&mls_group_id));
            if let Some(msgs) = cache.peek(&mls_group_id) {
                assert_eq!(msgs.len(), 1);
                assert!(msgs.contains_key(&event_id));
                assert_eq!(msgs.get(&event_id).unwrap().id, event_id);
            } else {
                panic!("Messages not found in group cache");
            }
        }
        let processed_message = ProcessedMessage {
            wrapper_event_id: wrapper_id,
            message_event_id: Some(event_id),
            processed_at: Timestamp::now(),
            epoch: None,
            mls_group_id: None,
            state: ProcessedMessageState::Processed,
            failure_reason: None,
        };
        nostr_storage
            .save_processed_message(processed_message.clone())
            .unwrap();
        let found_processed = nostr_storage
            .find_processed_message_by_event_id(&wrapper_id)
            .unwrap()
            .unwrap();
        assert_eq!(found_processed.wrapper_event_id, wrapper_id);
        {
            let inner = nostr_storage.inner.read();
            let cache = &inner.processed_messages_cache;
            assert!(cache.contains(&wrapper_id));
        }
    }

    #[test]
    fn test_save_message_for_nonexistent_group() {
        let nostr_storage = MdkMemoryStorage::default();
        let nonexistent_group_id = create_test_group_id();
        let event_id = EventId::all_zeros();
        let wrapper_id = EventId::all_zeros();
        let pubkey =
            PublicKey::from_hex("aabbccddeeffaabbccddeeffaabbccddeeffaabbccddeeffaabbccddeeffaabb")
                .unwrap();
        let now = Timestamp::now();
        let message = Message {
            id: event_id,
            pubkey,
            kind: Kind::MlsGroupMessage,
            mls_group_id: nonexistent_group_id.clone(),
            created_at: now,
            processed_at: now,
            content: "Hello, world!".to_string(),
            tags: Tags::new(),
            event: UnsignedEvent::new(
                pubkey,
                now,
                Kind::MlsGroupMessage,
                Tags::new(),
                "Hello, world!".to_string(),
            ),
            wrapper_event_id: wrapper_id,
            state: MessageState::Created,
            epoch: None,
        };

        // Attempting to save a message for a non-existent group should return an error
        let result = nostr_storage.save_message(message);
        assert!(result.is_err());
        match result.unwrap_err() {
            MessageError::InvalidParameters(msg) => {
                assert!(msg.contains("not found"));
            }
            _ => panic!("Expected InvalidParameters error"),
        }

        // Verify the message was not added to the cache
        {
            let inner = nostr_storage.inner.read();
            let cache = &inner.messages_by_group_cache;
            assert!(!cache.contains(&nonexistent_group_id));
        }
    }

    #[test]
    fn test_update_existing_message() {
        let nostr_storage = MdkMemoryStorage::default();
        let mls_group_id = create_test_group_id();
        let nostr_group_id = generate_random_bytes(32).try_into().unwrap();
        let group = Group {
            mls_group_id: mls_group_id.clone(),
            nostr_group_id,
            name: "Update Test Group".to_string(),
            description: "A group for testing message updates".to_string(),
            admin_pubkeys: BTreeSet::new(),
            last_message_id: None,
            last_message_at: None,
            last_message_processed_at: None,
            epoch: 0,
            state: GroupState::Active,
            image_hash: None,
            image_key: None,
            image_nonce: None,
            self_update_state: SelfUpdateState::Required,
        };
        nostr_storage.save_group(group).unwrap();

        let event_id = EventId::all_zeros();
        let wrapper_id = EventId::all_zeros();
        let pubkey =
            PublicKey::from_hex("aabbccddeeffaabbccddeeffaabbccddeeffaabbccddeeffaabbccddeeffaabb")
                .unwrap();
        let now = Timestamp::now();
        let original_message = Message {
            id: event_id,
            pubkey,
            kind: Kind::MlsGroupMessage,
            mls_group_id: mls_group_id.clone(),
            created_at: now,
            processed_at: now,
            content: "Original message".to_string(),
            tags: Tags::new(),
            event: UnsignedEvent::new(
                pubkey,
                now,
                Kind::MlsGroupMessage,
                Tags::new(),
                "Original message".to_string(),
            ),
            wrapper_event_id: wrapper_id,
            state: MessageState::Created,
            epoch: None,
        };

        // Save the original message
        nostr_storage
            .save_message(original_message.clone())
            .unwrap();

        // Verify the original message is stored
        let found_message = nostr_storage
            .find_message_by_event_id(&mls_group_id, &event_id)
            .unwrap()
            .unwrap();
        assert_eq!(found_message.content, "Original message");

        // Update the message with new content
        let updated_message = Message {
            content: "Updated message".to_string(),
            event: UnsignedEvent::new(
                pubkey,
                Timestamp::now(),
                Kind::MlsGroupMessage,
                Tags::new(),
                "Updated message".to_string(),
            ),
            ..original_message.clone()
        };

        // Save the updated message
        nostr_storage.save_message(updated_message.clone()).unwrap();

        // Verify the message was updated in the messages cache
        let found_message = nostr_storage
            .find_message_by_event_id(&mls_group_id, &event_id)
            .unwrap()
            .unwrap();
        assert_eq!(found_message.content, "Updated message");

        // Verify the message was updated in the group cache
        {
            let inner = nostr_storage.inner.read();
            let cache = &inner.messages_by_group_cache;
            let group_messages = cache.peek(&mls_group_id).unwrap();
            assert_eq!(group_messages.len(), 1);
            let msg = group_messages.get(&event_id).unwrap();
            assert_eq!(msg.content, "Updated message");
            assert_eq!(msg.id, event_id);
        }
    }

    #[test]
    fn test_save_multiple_messages_for_same_group() {
        let nostr_storage = MdkMemoryStorage::default();
        let mls_group_id = create_test_group_id();
        let nostr_group_id = generate_random_bytes(32).try_into().unwrap();
        let group = Group {
            mls_group_id: mls_group_id.clone(),
            nostr_group_id,
            name: "Multiple Messages Group".to_string(),
            description: "A group for testing multiple messages".to_string(),
            admin_pubkeys: BTreeSet::new(),
            last_message_id: None,
            last_message_at: None,
            last_message_processed_at: None,
            epoch: 0,
            state: GroupState::Active,
            image_hash: None,
            image_key: None,
            image_nonce: None,
            self_update_state: SelfUpdateState::Required,
        };
        nostr_storage.save_group(group).unwrap();

        let pubkey =
            PublicKey::from_hex("aabbccddeeffaabbccddeeffaabbccddeeffaabbccddeeffaabbccddeeffaabb")
                .unwrap();

        // Create and save first message
        let now = Timestamp::now();
        let event_id_1 =
            EventId::from_hex("0000000000000000000000000000000000000000000000000000000000000001")
                .unwrap();
        let wrapper_id_1 = EventId::all_zeros();
        let message_1 = Message {
            id: event_id_1,
            pubkey,
            kind: Kind::MlsGroupMessage,
            mls_group_id: mls_group_id.clone(),
            created_at: now,
            processed_at: now,
            content: "First message".to_string(),
            tags: Tags::new(),
            event: UnsignedEvent::new(
                pubkey,
                now,
                Kind::MlsGroupMessage,
                Tags::new(),
                "First message".to_string(),
            ),
            wrapper_event_id: wrapper_id_1,
            state: MessageState::Created,
            epoch: None,
        };
        nostr_storage.save_message(message_1.clone()).unwrap();

        // Create and save second message
        let event_id_2 =
            EventId::from_hex("0000000000000000000000000000000000000000000000000000000000000002")
                .unwrap();
        let wrapper_id_2 = EventId::all_zeros();
        let message_2 = Message {
            id: event_id_2,
            pubkey,
            kind: Kind::MlsGroupMessage,
            mls_group_id: mls_group_id.clone(),
            created_at: now,
            processed_at: now,
            content: "Second message".to_string(),
            tags: Tags::new(),
            event: UnsignedEvent::new(
                pubkey,
                now,
                Kind::MlsGroupMessage,
                Tags::new(),
                "Second message".to_string(),
            ),
            wrapper_event_id: wrapper_id_2,
            state: MessageState::Created,
            epoch: None,
        };
        nostr_storage.save_message(message_2.clone()).unwrap();

        // Verify both messages are in the messages cache
        let found_message_1 = nostr_storage
            .find_message_by_event_id(&mls_group_id, &event_id_1)
            .unwrap()
            .unwrap();
        assert_eq!(found_message_1.content, "First message");

        let found_message_2 = nostr_storage
            .find_message_by_event_id(&mls_group_id, &event_id_2)
            .unwrap()
            .unwrap();
        assert_eq!(found_message_2.content, "Second message");

        // Verify both messages are in the group cache
        {
            let inner = nostr_storage.inner.read();
            let cache = &inner.messages_by_group_cache;
            let group_messages = cache.peek(&mls_group_id).unwrap();
            assert_eq!(group_messages.len(), 2);
            assert_eq!(
                group_messages.get(&event_id_1).unwrap().content,
                "First message"
            );
            assert_eq!(
                group_messages.get(&event_id_2).unwrap().content,
                "Second message"
            );
        }
    }

    #[test]
    fn test_save_message_verifies_group_existence_before_cache_insertion() {
        let nostr_storage = MdkMemoryStorage::default();
        let mls_group_id = create_test_group_id();
        let nonexistent_group_id = GroupId::from_slice(&[9, 9, 9, 9]);
        let event_id = EventId::all_zeros();
        let wrapper_id = EventId::all_zeros();
        let pubkey =
            PublicKey::from_hex("aabbccddeeffaabbccddeeffaabbccddeeffaabbccddeeffaabbccddeeffaabb")
                .unwrap();

        // Create a group
        let nostr_group_id = generate_random_bytes(32).try_into().unwrap();
        let group = Group {
            mls_group_id: mls_group_id.clone(),
            nostr_group_id,
            name: "Test Group".to_string(),
            description: "A test group".to_string(),
            admin_pubkeys: BTreeSet::new(),
            last_message_id: None,
            last_message_at: None,
            last_message_processed_at: None,
            epoch: 0,
            state: GroupState::Active,
            image_hash: None,
            image_key: None,
            image_nonce: None,
            self_update_state: SelfUpdateState::Required,
        };
        nostr_storage.save_group(group).unwrap();

        // Try to save a message for a non-existent group
        let now = Timestamp::now();
        let message = Message {
            id: event_id,
            pubkey,
            kind: Kind::MlsGroupMessage,
            mls_group_id: nonexistent_group_id.clone(),
            created_at: now,
            processed_at: now,
            content: "Hello, world!".to_string(),
            tags: Tags::new(),
            event: UnsignedEvent::new(
                pubkey,
                now,
                Kind::MlsGroupMessage,
                Tags::new(),
                "Hello, world!".to_string(),
            ),
            wrapper_event_id: wrapper_id,
            state: MessageState::Created,
            epoch: None,
        };

        let result = nostr_storage.save_message(message);
        assert!(result.is_err());

        // Verify the message was not added to either cache
        {
            let inner = nostr_storage.inner.read();
            let cache = &inner.messages_cache;
            assert!(!cache.contains(&event_id));
        }
        {
            let inner = nostr_storage.inner.read();
            let cache = &inner.messages_by_group_cache;
            assert!(!cache.contains(&nonexistent_group_id));
        }

        // Verify the existing group's cache is still empty (no messages were added)
        {
            let inner = nostr_storage.inner.read();
            let cache = &inner.messages_by_group_cache;
            if let Some(messages) = cache.peek(&mls_group_id) {
                assert!(messages.is_empty());
            }
        }
    }

    #[test]
    fn test_with_custom_cache_size() {
        let custom_size = NonZeroUsize::new(50).unwrap();
        let nostr_storage = MdkMemoryStorage::with_cache_size(custom_size);

        // Verify the cache size is set correctly
        assert_eq!(nostr_storage.limits().cache_size, 50);

        // Create a test group to verify the cache works
        let mls_group_id = create_test_group_id();
        let nostr_group_id = generate_random_bytes(32).try_into().unwrap();
        let image_hash = Some(generate_random_bytes(32).try_into().unwrap());
        let image_key = Some(Secret::new(generate_random_bytes(32).try_into().unwrap()));
        let image_nonce = Some(Secret::new(generate_random_bytes(12).try_into().unwrap()));
        let group = Group {
            mls_group_id: mls_group_id.clone(),
            nostr_group_id,
            name: "Custom Cache Group".to_string(),
            description: "A group for testing custom cache size".to_string(),
            admin_pubkeys: BTreeSet::new(),
            last_message_id: None,
            last_message_at: None,
            last_message_processed_at: None,
            epoch: 0,
            state: GroupState::Active,
            image_hash,
            image_key,
            image_nonce,
            self_update_state: SelfUpdateState::Required,
        };

        // Save the group
        nostr_storage.save_group(group.clone()).unwrap();

        // Find the group by MLS group ID
        let found_group = nostr_storage.find_group_by_mls_group_id(&mls_group_id);
        assert!(found_group.is_ok());
        let found_group = found_group.unwrap().unwrap();
        assert_eq!(found_group.mls_group_id, mls_group_id);
    }

    #[test]
    fn test_default_implementation() {
        let nostr_storage = MdkMemoryStorage::default();

        // Create a test group to verify the default implementation works
        let mls_group_id = create_test_group_id();
        let nostr_group_id = generate_random_bytes(32).try_into().unwrap();
        let image_hash = Some(generate_random_bytes(32).try_into().unwrap());
        let image_key = Some(Secret::new(generate_random_bytes(32).try_into().unwrap()));
        let image_nonce = Some(Secret::new(generate_random_bytes(12).try_into().unwrap()));

        let group = Group {
            mls_group_id: mls_group_id.clone(),
            nostr_group_id,
            name: "Default Implementation Group".to_string(),
            description: "A group for testing default implementation".to_string(),
            admin_pubkeys: BTreeSet::new(),
            last_message_id: None,
            last_message_at: None,
            last_message_processed_at: None,
            epoch: 0,
            state: GroupState::Active,
            image_hash,
            image_key,
            image_nonce,
            self_update_state: SelfUpdateState::Required,
        };

        // Save the group
        nostr_storage.save_group(group.clone()).unwrap();

        // Find the group by MLS group ID
        let found_group = nostr_storage.find_group_by_mls_group_id(&mls_group_id);
        assert!(found_group.is_ok());
        let found_group = found_group.unwrap().unwrap();
        assert_eq!(found_group.mls_group_id, mls_group_id);
    }

    #[test]
    fn test_snapshot_and_restore() {
        let storage = MdkMemoryStorage::default();

        // Create and save a group
        let mls_group_id = create_test_group_id();
        let nostr_group_id = generate_random_bytes(32).try_into().unwrap();
        let group = Group {
            mls_group_id: mls_group_id.clone(),
            nostr_group_id,
            name: "Snapshot Test Group".to_string(),
            description: "A group for testing snapshots".to_string(),
            admin_pubkeys: BTreeSet::new(),
            last_message_id: None,
            last_message_at: None,
            last_message_processed_at: None,
            epoch: 0,
            state: GroupState::Active,
            image_hash: None,
            image_key: None,
            image_nonce: None,
            self_update_state: SelfUpdateState::Required,
        };
        storage.save_group(group.clone()).unwrap();

        // Create a snapshot
        let snapshot = storage.create_snapshot();

        // Modify the group
        let modified_group = Group {
            name: "Modified Group Name".to_string(),
            epoch: 5,
            ..group.clone()
        };
        storage.save_group(modified_group.clone()).unwrap();

        // Verify the modification
        let found_group = storage
            .find_group_by_mls_group_id(&mls_group_id)
            .unwrap()
            .unwrap();
        assert_eq!(found_group.name, "Modified Group Name");
        assert_eq!(found_group.epoch, 5);

        // Restore the snapshot
        storage.restore_snapshot(snapshot);

        // Verify the original state is restored
        let restored_group = storage
            .find_group_by_mls_group_id(&mls_group_id)
            .unwrap()
            .unwrap();
        assert_eq!(restored_group.name, "Snapshot Test Group");
        assert_eq!(restored_group.epoch, 0);
    }

    #[test]
    fn test_snapshot_with_messages() {
        let storage = MdkMemoryStorage::default();

        // Create and save a group
        let mls_group_id = create_test_group_id();
        let nostr_group_id = generate_random_bytes(32).try_into().unwrap();
        let group = Group {
            mls_group_id: mls_group_id.clone(),
            nostr_group_id,
            name: "Message Snapshot Group".to_string(),
            description: "A group for testing message snapshots".to_string(),
            admin_pubkeys: BTreeSet::new(),
            last_message_id: None,
            last_message_at: None,
            last_message_processed_at: None,
            epoch: 0,
            state: GroupState::Active,
            image_hash: None,
            image_key: None,
            image_nonce: None,
            self_update_state: SelfUpdateState::Required,
        };
        storage.save_group(group).unwrap();

        // Save a message
        let pubkey =
            PublicKey::from_hex("aabbccddeeffaabbccddeeffaabbccddeeffaabbccddeeffaabbccddeeffaabb")
                .unwrap();
        let event_id =
            EventId::from_hex("0000000000000000000000000000000000000000000000000000000000000001")
                .unwrap();
        let now = Timestamp::now();
        let message = Message {
            id: event_id,
            pubkey,
            kind: Kind::MlsGroupMessage,
            mls_group_id: mls_group_id.clone(),
            created_at: now,
            processed_at: now,
            content: "Original message".to_string(),
            tags: Tags::new(),
            event: UnsignedEvent::new(
                pubkey,
                now,
                Kind::MlsGroupMessage,
                Tags::new(),
                "Original message".to_string(),
            ),
            wrapper_event_id: EventId::all_zeros(),
            state: MessageState::Created,
            epoch: None,
        };
        storage.save_message(message).unwrap();

        // Create a snapshot with the message
        let snapshot = storage.create_snapshot();

        // Add another message
        let event_id_2 =
            EventId::from_hex("0000000000000000000000000000000000000000000000000000000000000002")
                .unwrap();
        let message_2 = Message {
            id: event_id_2,
            pubkey,
            kind: Kind::MlsGroupMessage,
            mls_group_id: mls_group_id.clone(),
            created_at: now,
            processed_at: now,
            content: "Second message".to_string(),
            tags: Tags::new(),
            event: UnsignedEvent::new(
                pubkey,
                now,
                Kind::MlsGroupMessage,
                Tags::new(),
                "Second message".to_string(),
            ),
            wrapper_event_id: EventId::all_zeros(),
            state: MessageState::Created,
            epoch: None,
        };
        storage.save_message(message_2).unwrap();

        // Verify we have two messages
        let messages = storage.messages(&mls_group_id, None).unwrap();
        assert_eq!(messages.len(), 2);

        // Restore the snapshot
        storage.restore_snapshot(snapshot);

        // Verify we're back to one message
        let messages_after = storage.messages(&mls_group_id, None).unwrap();
        assert_eq!(messages_after.len(), 1);
        assert_eq!(messages_after[0].content, "Original message");
    }

    // ========================================
    // Additional Snapshot/Rollback Tests (Phase 5)
    // ========================================

    #[test]
    fn test_snapshot_with_new_group_rollback() {
        let storage = MdkMemoryStorage::default();

        // Verify group doesn't exist
        let mls_group_id = GroupId::from_slice(&[13, 14, 15, 16]);
        let before = storage.find_group_by_mls_group_id(&mls_group_id).unwrap();
        assert!(before.is_none());

        // Create a snapshot
        let snapshot = storage.create_snapshot();

        // Insert a new group
        let nostr_group_id = generate_random_bytes(32).try_into().unwrap();
        let group = Group {
            mls_group_id: mls_group_id.clone(),
            nostr_group_id,
            name: "New Group".to_string(),
            description: "A new group".to_string(),
            admin_pubkeys: BTreeSet::new(),
            last_message_id: None,
            last_message_at: None,
            last_message_processed_at: None,
            epoch: 0,
            state: GroupState::Active,
            image_hash: None,
            image_key: None,
            image_nonce: None,
            self_update_state: SelfUpdateState::Required,
        };
        storage.save_group(group).unwrap();

        // Verify group exists
        let after_insert = storage.find_group_by_mls_group_id(&mls_group_id).unwrap();
        assert!(after_insert.is_some());

        // Restore snapshot (rollback)
        storage.restore_snapshot(snapshot);

        // Verify group no longer exists
        let after_rollback = storage.find_group_by_mls_group_id(&mls_group_id).unwrap();
        assert!(after_rollback.is_none());
    }

    #[test]
    fn test_snapshot_with_multiple_modifications_rollback() {
        let storage = MdkMemoryStorage::default();

        // Create and save a group
        let mls_group_id = GroupId::from_slice(&[17, 18, 19, 20]);
        let nostr_group_id = generate_random_bytes(32).try_into().unwrap();
        let group = Group {
            mls_group_id: mls_group_id.clone(),
            nostr_group_id,
            name: "Original Name".to_string(),
            description: "A group for testing modification rollback".to_string(),
            admin_pubkeys: BTreeSet::new(),
            last_message_id: None,
            last_message_at: None,
            last_message_processed_at: None,
            epoch: 1,
            state: GroupState::Active,
            image_hash: None,
            image_key: None,
            image_nonce: None,
            self_update_state: SelfUpdateState::Required,
        };
        storage.save_group(group.clone()).unwrap();

        // Verify group exists with original values
        let exists = storage
            .find_group_by_mls_group_id(&mls_group_id)
            .unwrap()
            .unwrap();
        assert_eq!(exists.name, "Original Name");
        assert_eq!(exists.epoch, 1);

        // Create snapshot
        let snapshot = storage.create_snapshot();

        // Make multiple modifications
        let modified1 = Group {
            name: "Modified Once".to_string(),
            epoch: 10,
            ..group.clone()
        };
        storage.save_group(modified1).unwrap();

        let modified2 = Group {
            name: "Modified Twice".to_string(),
            epoch: 20,
            ..group.clone()
        };
        storage.save_group(modified2).unwrap();

        // Verify final modification
        let after_mods = storage
            .find_group_by_mls_group_id(&mls_group_id)
            .unwrap()
            .unwrap();
        assert_eq!(after_mods.name, "Modified Twice");
        assert_eq!(after_mods.epoch, 20);

        // Restore snapshot (rollback)
        storage.restore_snapshot(snapshot);

        // Verify original values are restored
        let after_rollback = storage
            .find_group_by_mls_group_id(&mls_group_id)
            .unwrap()
            .unwrap();
        assert_eq!(after_rollback.name, "Original Name");
        assert_eq!(after_rollback.epoch, 1);
    }

    #[test]
    fn test_snapshot_with_relays_rollback() {
        let storage = MdkMemoryStorage::default();

        // Create a group
        let mls_group_id = GroupId::from_slice(&[21, 22, 23, 24]);
        let nostr_group_id = generate_random_bytes(32).try_into().unwrap();
        let group = Group {
            mls_group_id: mls_group_id.clone(),
            nostr_group_id,
            name: "Relay Test Group".to_string(),
            description: "A group for testing relay rollback".to_string(),
            admin_pubkeys: BTreeSet::new(),
            last_message_id: None,
            last_message_at: None,
            last_message_processed_at: None,
            epoch: 0,
            state: GroupState::Active,
            image_hash: None,
            image_key: None,
            image_nonce: None,
            self_update_state: SelfUpdateState::Required,
        };
        storage.save_group(group).unwrap();

        // Add initial relay
        let relay1 = RelayUrl::parse("wss://relay1.example.com").unwrap();
        storage
            .replace_group_relays(&mls_group_id, BTreeSet::from([relay1.clone()]))
            .unwrap();

        // Create snapshot
        let snapshot = storage.create_snapshot();

        // Add more relays
        let relay2 = RelayUrl::parse("wss://relay2.example.com").unwrap();
        storage
            .replace_group_relays(&mls_group_id, BTreeSet::from([relay1.clone(), relay2]))
            .unwrap();

        // Verify two relays
        let relays_before = storage.group_relays(&mls_group_id).unwrap();
        assert_eq!(relays_before.len(), 2);

        // Restore snapshot
        storage.restore_snapshot(snapshot);

        // Verify back to one relay
        let relays_after = storage.group_relays(&mls_group_id).unwrap();
        assert_eq!(relays_after.len(), 1);
    }

    #[test]
    fn test_snapshot_with_exporter_secrets_rollback() {
        let storage = MdkMemoryStorage::default();

        // Create a group
        let mls_group_id = GroupId::from_slice(&[25, 26, 27, 28]);
        let nostr_group_id = generate_random_bytes(32).try_into().unwrap();
        let group = Group {
            mls_group_id: mls_group_id.clone(),
            nostr_group_id,
            name: "Secret Test Group".to_string(),
            description: "A group for testing secret rollback".to_string(),
            admin_pubkeys: BTreeSet::new(),
            last_message_id: None,
            last_message_at: None,
            last_message_processed_at: None,
            epoch: 0,
            state: GroupState::Active,
            image_hash: None,
            image_key: None,
            image_nonce: None,
            self_update_state: SelfUpdateState::Required,
        };
        storage.save_group(group).unwrap();

        // Add epoch 0 secret
        let secret_0 = GroupExporterSecret {
            mls_group_id: mls_group_id.clone(),
            epoch: 0,
            secret: Secret::new([1u8; 32]),
        };
        storage
            .save_group_exporter_secret(secret_0.clone())
            .unwrap();

        // Create snapshot
        let snapshot = storage.create_snapshot();

        // Add epoch 1 secret
        let secret_1 = GroupExporterSecret {
            mls_group_id: mls_group_id.clone(),
            epoch: 1,
            secret: Secret::new([2u8; 32]),
        };
        storage
            .save_group_exporter_secret(secret_1.clone())
            .unwrap();

        // Verify epoch 1 secret exists
        let found_1 = storage.get_group_exporter_secret(&mls_group_id, 1).unwrap();
        assert!(found_1.is_some());

        // Restore snapshot
        storage.restore_snapshot(snapshot);

        // Verify epoch 1 secret is gone
        let after_rollback = storage.get_group_exporter_secret(&mls_group_id, 1).unwrap();
        assert!(after_rollback.is_none());

        // Verify epoch 0 secret still exists
        let epoch_0_exists = storage.get_group_exporter_secret(&mls_group_id, 0).unwrap();
        assert!(epoch_0_exists.is_some());
    }

    #[test]
    fn test_snapshot_with_welcomes_rollback() {
        let storage = MdkMemoryStorage::default();

        // Create snapshot before any welcomes
        let snapshot = storage.create_snapshot();

        // Create a welcome
        let event_id = EventId::all_zeros();
        let wrapper_id = EventId::all_zeros();
        let mls_group_id = GroupId::from_slice(&[29, 30, 31, 32]);
        let nostr_group_id = generate_random_bytes(32).try_into().unwrap();
        let pubkey =
            PublicKey::from_hex("aabbccddeeffaabbccddeeffaabbccddeeffaabbccddeeffaabbccddeeffaabb")
                .unwrap();

        let welcome = Welcome {
            id: event_id,
            event: UnsignedEvent::new(
                pubkey,
                Timestamp::now(),
                Kind::MlsWelcome,
                Tags::new(),
                "test".to_string(),
            ),
            mls_group_id: mls_group_id.clone(),
            nostr_group_id,
            group_name: "Welcome Test Group".to_string(),
            group_description: "A test welcome group".to_string(),
            group_image_key: None,
            group_image_hash: None,
            group_image_nonce: None,
            group_admin_pubkeys: BTreeSet::from([pubkey]),
            group_relays: BTreeSet::from([RelayUrl::parse("wss://relay.example.com").unwrap()]),
            welcomer: pubkey,
            member_count: 2,
            state: WelcomeState::Pending,
            wrapper_event_id: wrapper_id,
        };
        storage.save_welcome(welcome).unwrap();

        // Verify welcome exists
        let found = storage.find_welcome_by_event_id(&event_id).unwrap();
        assert!(found.is_some());

        // Restore snapshot
        storage.restore_snapshot(snapshot);

        // Verify welcome is gone
        let after_rollback = storage.find_welcome_by_event_id(&event_id).unwrap();
        assert!(after_rollback.is_none());
    }

    #[test]
    fn test_snapshot_multiple_operations_rollback() {
        let storage = MdkMemoryStorage::default();

        // Create initial state with one group
        let mls_group_id_1 = GroupId::from_slice(&[33, 34, 35, 36]);
        let nostr_group_id_1 = generate_random_bytes(32).try_into().unwrap();
        let group1 = Group {
            mls_group_id: mls_group_id_1.clone(),
            nostr_group_id: nostr_group_id_1,
            name: "Group 1".to_string(),
            description: "First group".to_string(),
            admin_pubkeys: BTreeSet::new(),
            last_message_id: None,
            last_message_at: None,
            last_message_processed_at: None,
            epoch: 0,
            state: GroupState::Active,
            image_hash: None,
            image_key: None,
            image_nonce: None,
            self_update_state: SelfUpdateState::Required,
        };
        storage.save_group(group1).unwrap();

        // Create snapshot
        let snapshot = storage.create_snapshot();

        // Perform multiple operations:
        // 1. Create second group
        let mls_group_id_2 = GroupId::from_slice(&[37, 38, 39, 40]);
        let nostr_group_id_2 = generate_random_bytes(32).try_into().unwrap();
        let group2 = Group {
            mls_group_id: mls_group_id_2.clone(),
            nostr_group_id: nostr_group_id_2,
            name: "Group 2".to_string(),
            description: "Second group".to_string(),
            admin_pubkeys: BTreeSet::new(),
            last_message_id: None,
            last_message_at: None,
            last_message_processed_at: None,
            epoch: 0,
            state: GroupState::Active,
            image_hash: None,
            image_key: None,
            image_nonce: None,
            self_update_state: SelfUpdateState::Required,
        };
        storage.save_group(group2).unwrap();

        // 2. Add message to first group
        let pubkey =
            PublicKey::from_hex("aabbccddeeffaabbccddeeffaabbccddeeffaabbccddeeffaabbccddeeffaabb")
                .unwrap();
        let event_id =
            EventId::from_hex("0000000000000000000000000000000000000000000000000000000000000099")
                .unwrap();
        let now = Timestamp::now();
        let message = Message {
            id: event_id,
            pubkey,
            kind: Kind::MlsGroupMessage,
            mls_group_id: mls_group_id_1.clone(),
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
            state: MessageState::Created,
            epoch: None,
        };
        storage.save_message(message).unwrap();

        // 3. Modify first group
        let modified_group1 = Group {
            mls_group_id: mls_group_id_1.clone(),
            nostr_group_id: nostr_group_id_1,
            name: "Modified Group 1".to_string(),
            description: "First group".to_string(),
            admin_pubkeys: BTreeSet::new(),
            last_message_id: None,
            last_message_at: None,
            last_message_processed_at: None,
            epoch: 5,
            state: GroupState::Active,
            image_hash: None,
            image_key: None,
            image_nonce: None,
            self_update_state: SelfUpdateState::Required,
        };
        storage.save_group(modified_group1).unwrap();

        // Verify all changes
        let groups = storage.all_groups().unwrap();
        assert_eq!(groups.len(), 2);
        let messages = storage.messages(&mls_group_id_1, None).unwrap();
        assert_eq!(messages.len(), 1);
        let g1 = storage
            .find_group_by_mls_group_id(&mls_group_id_1)
            .unwrap()
            .unwrap();
        assert_eq!(g1.name, "Modified Group 1");
        assert_eq!(g1.epoch, 5);

        // Restore snapshot
        storage.restore_snapshot(snapshot);

        // Verify all changes are rolled back
        let groups_after = storage.all_groups().unwrap();
        assert_eq!(groups_after.len(), 1);
        let g2_gone = storage.find_group_by_mls_group_id(&mls_group_id_2).unwrap();
        assert!(g2_gone.is_none());
        let messages_after = storage.messages(&mls_group_id_1, None).unwrap();
        assert_eq!(messages_after.len(), 0);
        let g1_restored = storage
            .find_group_by_mls_group_id(&mls_group_id_1)
            .unwrap()
            .unwrap();
        assert_eq!(g1_restored.name, "Group 1");
        assert_eq!(g1_restored.epoch, 0);
    }

    #[test]
    fn test_snapshot_preserves_snapshot_independence() {
        let storage = MdkMemoryStorage::default();

        // Create a group
        let mls_group_id = GroupId::from_slice(&[41, 42, 43, 44]);
        let nostr_group_id = generate_random_bytes(32).try_into().unwrap();
        let group = Group {
            mls_group_id: mls_group_id.clone(),
            nostr_group_id,
            name: "State A".to_string(),
            description: "Initial state".to_string(),
            admin_pubkeys: BTreeSet::new(),
            last_message_id: None,
            last_message_at: None,
            last_message_processed_at: None,
            epoch: 0,
            state: GroupState::Active,
            image_hash: None,
            image_key: None,
            image_nonce: None,
            self_update_state: SelfUpdateState::Required,
        };
        storage.save_group(group.clone()).unwrap();

        // Take snapshot A
        let snapshot_a = storage.create_snapshot();

        // Modify to state B
        let group_b = Group {
            name: "State B".to_string(),
            epoch: 1,
            ..group.clone()
        };
        storage.save_group(group_b.clone()).unwrap();

        // Take snapshot B
        let snapshot_b = storage.create_snapshot();

        // Modify to state C
        let group_c = Group {
            name: "State C".to_string(),
            epoch: 2,
            ..group.clone()
        };
        storage.save_group(group_c).unwrap();

        // Current state is C
        let current = storage
            .find_group_by_mls_group_id(&mls_group_id)
            .unwrap()
            .unwrap();
        assert_eq!(current.name, "State C");

        // Restore to A
        storage.restore_snapshot(snapshot_a.clone());
        let after_a = storage
            .find_group_by_mls_group_id(&mls_group_id)
            .unwrap()
            .unwrap();
        assert_eq!(after_a.name, "State A");

        // Restore to B (from A state)
        storage.restore_snapshot(snapshot_b);
        let after_b = storage
            .find_group_by_mls_group_id(&mls_group_id)
            .unwrap()
            .unwrap();
        assert_eq!(after_b.name, "State B");

        // Can still restore to A again
        storage.restore_snapshot(snapshot_a);
        let final_state = storage
            .find_group_by_mls_group_id(&mls_group_id)
            .unwrap()
            .unwrap();
        assert_eq!(final_state.name, "State A");
    }

    /// Test that group-scoped snapshots provide proper isolation between groups.
    ///
    /// This test verifies the fix for Issue 1: Memory Storage Rollback Affects All Groups.
    /// When rolling back Group A's snapshot, Group B should be completely unaffected.
    #[test]
    fn test_snapshot_isolation_between_groups() {
        let storage = MdkMemoryStorage::default();

        // Create two independent groups
        let group1_id = GroupId::from_slice(&[1; 32]);
        let group2_id = GroupId::from_slice(&[2; 32]);
        let nostr_group_id_1: [u8; 32] = generate_random_bytes(32).try_into().unwrap();
        let nostr_group_id_2: [u8; 32] = generate_random_bytes(32).try_into().unwrap();

        let group1 = Group {
            mls_group_id: group1_id.clone(),
            nostr_group_id: nostr_group_id_1,
            name: "Group 1 Original".to_string(),
            description: "First group".to_string(),
            admin_pubkeys: BTreeSet::new(),
            last_message_id: None,
            last_message_at: None,
            last_message_processed_at: None,
            epoch: 5,
            state: GroupState::Active,
            image_hash: None,
            image_key: None,
            image_nonce: None,
            self_update_state: SelfUpdateState::Required,
        };

        let group2 = Group {
            mls_group_id: group2_id.clone(),
            nostr_group_id: nostr_group_id_2,
            name: "Group 2 Original".to_string(),
            description: "Second group".to_string(),
            admin_pubkeys: BTreeSet::new(),
            last_message_id: None,
            last_message_at: None,
            last_message_processed_at: None,
            epoch: 10,
            state: GroupState::Active,
            image_hash: None,
            image_key: None,
            image_nonce: None,
            self_update_state: SelfUpdateState::Required,
        };

        storage.save_group(group1.clone()).unwrap();
        storage.save_group(group2.clone()).unwrap();

        // Create a snapshot for Group 1 only (at epoch 5)
        storage
            .create_group_snapshot(&group1_id, "group1_snap")
            .unwrap();

        // Modify BOTH groups
        let modified_group1 = Group {
            name: "Group 1 Modified".to_string(),
            epoch: 6,
            ..group1.clone()
        };
        let modified_group2 = Group {
            name: "Group 2 Modified".to_string(),
            epoch: 11,
            ..group2.clone()
        };

        storage.save_group(modified_group1).unwrap();
        storage.save_group(modified_group2).unwrap();

        // Verify both groups are modified
        let found1 = storage
            .find_group_by_mls_group_id(&group1_id)
            .unwrap()
            .unwrap();
        let found2 = storage
            .find_group_by_mls_group_id(&group2_id)
            .unwrap()
            .unwrap();
        assert_eq!(found1.name, "Group 1 Modified");
        assert_eq!(found1.epoch, 6);
        assert_eq!(found2.name, "Group 2 Modified");
        assert_eq!(found2.epoch, 11);

        // Rollback Group 1 to its snapshot
        storage
            .rollback_group_to_snapshot(&group1_id, "group1_snap")
            .unwrap();

        // Verify Group 1 is rolled back
        let final1 = storage
            .find_group_by_mls_group_id(&group1_id)
            .unwrap()
            .unwrap();
        assert_eq!(final1.name, "Group 1 Original");
        assert_eq!(final1.epoch, 5);

        // CRITICAL: Verify Group 2 is STILL at its modified state (not rolled back)
        let final2 = storage
            .find_group_by_mls_group_id(&group2_id)
            .unwrap()
            .unwrap();
        assert_eq!(
            final2.name, "Group 2 Modified",
            "Group 2 should NOT be affected by Group 1's rollback"
        );
        assert_eq!(
            final2.epoch, 11,
            "Group 2's epoch should NOT be affected by Group 1's rollback"
        );
    }

    /// Test that group-scoped snapshots also isolate exporter secrets correctly.
    #[test]
    fn test_snapshot_isolation_with_exporter_secrets() {
        let storage = MdkMemoryStorage::default();

        // Create two groups
        let group1_id = GroupId::from_slice(&[11; 32]);
        let group2_id = GroupId::from_slice(&[22; 32]);
        let nostr_group_id_1: [u8; 32] = generate_random_bytes(32).try_into().unwrap();
        let nostr_group_id_2: [u8; 32] = generate_random_bytes(32).try_into().unwrap();

        let group1 = Group {
            mls_group_id: group1_id.clone(),
            nostr_group_id: nostr_group_id_1,
            name: "Group 1".to_string(),
            description: "".to_string(),
            admin_pubkeys: BTreeSet::new(),
            last_message_id: None,
            last_message_at: None,
            last_message_processed_at: None,
            epoch: 1,
            state: GroupState::Active,
            image_hash: None,
            image_key: None,
            image_nonce: None,
            self_update_state: SelfUpdateState::Required,
        };

        let group2 = Group {
            mls_group_id: group2_id.clone(),
            nostr_group_id: nostr_group_id_2,
            name: "Group 2".to_string(),
            description: "".to_string(),
            admin_pubkeys: BTreeSet::new(),
            last_message_id: None,
            last_message_at: None,
            last_message_processed_at: None,
            epoch: 1,
            state: GroupState::Active,
            image_hash: None,
            image_key: None,
            image_nonce: None,
            self_update_state: SelfUpdateState::Required,
        };

        storage.save_group(group1).unwrap();
        storage.save_group(group2).unwrap();

        // Add exporter secrets for both groups
        let secret1_epoch0 = GroupExporterSecret {
            mls_group_id: group1_id.clone(),
            epoch: 0,
            secret: Secret::new([1u8; 32]),
        };
        let secret2_epoch0 = GroupExporterSecret {
            mls_group_id: group2_id.clone(),
            epoch: 0,
            secret: Secret::new([2u8; 32]),
        };

        storage
            .save_group_exporter_secret(secret1_epoch0.clone())
            .unwrap();
        storage
            .save_group_exporter_secret(secret2_epoch0.clone())
            .unwrap();

        // Snapshot Group 1
        storage
            .create_group_snapshot(&group1_id, "group1_secrets_snap")
            .unwrap();

        // Add new epoch secrets to BOTH groups
        let secret1_epoch1 = GroupExporterSecret {
            mls_group_id: group1_id.clone(),
            epoch: 1,
            secret: Secret::new([11u8; 32]),
        };
        let secret2_epoch1 = GroupExporterSecret {
            mls_group_id: group2_id.clone(),
            epoch: 1,
            secret: Secret::new([22u8; 32]),
        };

        storage.save_group_exporter_secret(secret1_epoch1).unwrap();
        storage.save_group_exporter_secret(secret2_epoch1).unwrap();

        // Verify both groups have epoch 1 secrets
        assert!(
            storage
                .get_group_exporter_secret(&group1_id, 1)
                .unwrap()
                .is_some()
        );
        assert!(
            storage
                .get_group_exporter_secret(&group2_id, 1)
                .unwrap()
                .is_some()
        );

        // Rollback Group 1
        storage
            .rollback_group_to_snapshot(&group1_id, "group1_secrets_snap")
            .unwrap();

        // Group 1's epoch 1 secret should be gone
        assert!(
            storage
                .get_group_exporter_secret(&group1_id, 1)
                .unwrap()
                .is_none(),
            "Group 1's epoch 1 secret should be rolled back"
        );

        // Group 2's epoch 1 secret should STILL exist
        assert!(
            storage
                .get_group_exporter_secret(&group2_id, 1)
                .unwrap()
                .is_some(),
            "Group 2's epoch 1 secret should NOT be affected by Group 1's rollback"
        );
    }

    /// Test that rolling back to a nonexistent snapshot returns an error.
    #[test]
    fn test_rollback_nonexistent_snapshot_returns_error() {
        let storage = MdkMemoryStorage::default();

        let group_id = GroupId::from_slice(&[99; 32]);
        let nostr_group_id: [u8; 32] = generate_random_bytes(32).try_into().unwrap();

        let group = Group {
            mls_group_id: group_id.clone(),
            nostr_group_id,
            name: "Test Group".to_string(),
            description: "".to_string(),
            admin_pubkeys: BTreeSet::new(),
            last_message_id: None,
            last_message_at: None,
            last_message_processed_at: None,
            epoch: 1,
            state: GroupState::Active,
            image_hash: None,
            image_key: None,
            image_nonce: None,
            self_update_state: SelfUpdateState::Required,
        };

        storage.save_group(group).unwrap();

        // Try to rollback to a snapshot that was never created
        let result = storage.rollback_group_to_snapshot(&group_id, "nonexistent_snapshot");

        assert!(
            result.is_err(),
            "Should return error for nonexistent snapshot"
        );
        match result {
            Err(MdkStorageError::NotFound(msg)) => {
                assert!(
                    msg.contains("Snapshot not found"),
                    "Error should indicate snapshot not found"
                );
            }
            _ => panic!("Expected NotFound error"),
        }
    }

    #[test]
    fn test_list_group_snapshots_empty() {
        let storage = MdkMemoryStorage::default();
        let group_id = GroupId::from_slice(&[1, 2, 3, 4]);

        let snapshots = storage.list_group_snapshots(&group_id).unwrap();
        assert!(
            snapshots.is_empty(),
            "Should return empty list for no snapshots"
        );
    }

    #[test]
    fn test_list_group_snapshots_returns_snapshots_sorted_by_created_at() {
        let storage = MdkMemoryStorage::default();
        let group_id = GroupId::from_slice(&[1, 2, 3, 4]);
        let nostr_group_id: [u8; 32] = generate_random_bytes(32).try_into().unwrap();

        // Create a group first
        let group = Group {
            mls_group_id: group_id.clone(),
            nostr_group_id,
            name: "Test Group".to_string(),
            description: "".to_string(),
            admin_pubkeys: BTreeSet::new(),
            last_message_id: None,
            last_message_at: None,
            last_message_processed_at: None,
            epoch: 1,
            state: GroupState::Active,
            image_hash: None,
            image_key: None,
            image_nonce: None,
            self_update_state: SelfUpdateState::Required,
        };
        storage.save_group(group).unwrap();

        // Create snapshots with different timestamps
        // We need to manipulate the created_at directly since create_group_scoped_snapshot uses system time
        {
            let mut snapshots = storage.group_snapshots.write();

            // Insert snapshots with known created_at values (out of order)
            let snap1 = crate::snapshot::GroupScopedSnapshot {
                group_id: group_id.clone(),
                created_at: 1000,
                mls_group_data: std::collections::HashMap::new(),
                mls_own_leaf_nodes: vec![],
                mls_proposals: std::collections::HashMap::new(),
                mls_epoch_key_pairs: std::collections::HashMap::new(),
                group: None,
                group_relays: std::collections::BTreeSet::new(),
                group_exporter_secrets: std::collections::HashMap::new(),
            };
            let snap2 = crate::snapshot::GroupScopedSnapshot {
                group_id: group_id.clone(),
                created_at: 3000, // Newest
                ..snap1.clone()
            };
            let snap3 = crate::snapshot::GroupScopedSnapshot {
                group_id: group_id.clone(),
                created_at: 2000, // Middle
                ..snap1.clone()
            };

            snapshots.insert((group_id.clone(), "snap_oldest".to_string()), snap1);
            snapshots.insert((group_id.clone(), "snap_newest".to_string()), snap2);
            snapshots.insert((group_id.clone(), "snap_middle".to_string()), snap3);
        }

        let result = storage.list_group_snapshots(&group_id).unwrap();

        assert_eq!(result.len(), 3);
        // Should be sorted by created_at ascending
        assert_eq!(result[0].0, "snap_oldest");
        assert_eq!(result[0].1, 1000);
        assert_eq!(result[1].0, "snap_middle");
        assert_eq!(result[1].1, 2000);
        assert_eq!(result[2].0, "snap_newest");
        assert_eq!(result[2].1, 3000);
    }

    #[test]
    fn test_list_group_snapshots_only_returns_matching_group() {
        let storage = MdkMemoryStorage::default();
        let group1 = GroupId::from_slice(&[1, 1, 1, 1]);
        let group2 = GroupId::from_slice(&[2, 2, 2, 2]);

        {
            let mut snapshots = storage.group_snapshots.write();

            let snap1 = crate::snapshot::GroupScopedSnapshot {
                group_id: group1.clone(),
                created_at: 1000,
                mls_group_data: std::collections::HashMap::new(),
                mls_own_leaf_nodes: vec![],
                mls_proposals: std::collections::HashMap::new(),
                mls_epoch_key_pairs: std::collections::HashMap::new(),
                group: None,
                group_relays: std::collections::BTreeSet::new(),
                group_exporter_secrets: std::collections::HashMap::new(),
            };
            let snap2 = crate::snapshot::GroupScopedSnapshot {
                group_id: group2.clone(),
                created_at: 2000,
                ..snap1.clone()
            };

            snapshots.insert((group1.clone(), "snap_group1".to_string()), snap1);
            snapshots.insert((group2.clone(), "snap_group2".to_string()), snap2);
        }

        let result1 = storage.list_group_snapshots(&group1).unwrap();
        let result2 = storage.list_group_snapshots(&group2).unwrap();

        assert_eq!(result1.len(), 1);
        assert_eq!(result1[0].0, "snap_group1");

        assert_eq!(result2.len(), 1);
        assert_eq!(result2[0].0, "snap_group2");
    }

    #[test]
    fn test_prune_expired_snapshots_removes_old_snapshots() {
        let storage = MdkMemoryStorage::default();
        let group_id = GroupId::from_slice(&[1, 2, 3, 4]);

        {
            let mut snapshots = storage.group_snapshots.write();

            let base_snap = crate::snapshot::GroupScopedSnapshot {
                group_id: group_id.clone(),
                created_at: 0,
                mls_group_data: std::collections::HashMap::new(),
                mls_own_leaf_nodes: vec![],
                mls_proposals: std::collections::HashMap::new(),
                mls_epoch_key_pairs: std::collections::HashMap::new(),
                group: None,
                group_relays: std::collections::BTreeSet::new(),
                group_exporter_secrets: std::collections::HashMap::new(),
            };

            // Old snapshot (should be pruned)
            let old_snap = crate::snapshot::GroupScopedSnapshot {
                created_at: 1000,
                ..base_snap.clone()
            };
            // New snapshot (should be kept)
            let new_snap = crate::snapshot::GroupScopedSnapshot {
                created_at: 5000,
                ..base_snap.clone()
            };

            snapshots.insert((group_id.clone(), "old_snap".to_string()), old_snap);
            snapshots.insert((group_id.clone(), "new_snap".to_string()), new_snap);
        }

        // Prune snapshots older than 3000
        let pruned = storage.prune_expired_snapshots(3000).unwrap();

        assert_eq!(pruned, 1, "Should have pruned 1 snapshot");

        let remaining = storage.list_group_snapshots(&group_id).unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].0, "new_snap");
        assert_eq!(remaining[0].1, 5000);
    }

    #[test]
    fn test_prune_expired_snapshots_returns_zero_when_nothing_to_prune() {
        let storage = MdkMemoryStorage::default();
        let group_id = GroupId::from_slice(&[1, 2, 3, 4]);

        {
            let mut snapshots = storage.group_snapshots.write();

            let snap = crate::snapshot::GroupScopedSnapshot {
                group_id: group_id.clone(),
                created_at: 5000, // Newer than threshold
                mls_group_data: std::collections::HashMap::new(),
                mls_own_leaf_nodes: vec![],
                mls_proposals: std::collections::HashMap::new(),
                mls_epoch_key_pairs: std::collections::HashMap::new(),
                group: None,
                group_relays: std::collections::BTreeSet::new(),
                group_exporter_secrets: std::collections::HashMap::new(),
            };

            snapshots.insert((group_id.clone(), "recent_snap".to_string()), snap);
        }

        // Prune snapshots older than 1000 (none qualify)
        let pruned = storage.prune_expired_snapshots(1000).unwrap();

        assert_eq!(pruned, 0, "Should have pruned 0 snapshots");

        let remaining = storage.list_group_snapshots(&group_id).unwrap();
        assert_eq!(remaining.len(), 1);
    }

    #[test]
    fn test_prune_expired_snapshots_across_multiple_groups() {
        let storage = MdkMemoryStorage::default();
        let group1 = GroupId::from_slice(&[1, 1, 1, 1]);
        let group2 = GroupId::from_slice(&[2, 2, 2, 2]);

        {
            let mut snapshots = storage.group_snapshots.write();

            let base_snap1 = crate::snapshot::GroupScopedSnapshot {
                group_id: group1.clone(),
                created_at: 1000, // Old, should be pruned
                mls_group_data: std::collections::HashMap::new(),
                mls_own_leaf_nodes: vec![],
                mls_proposals: std::collections::HashMap::new(),
                mls_epoch_key_pairs: std::collections::HashMap::new(),
                group: None,
                group_relays: std::collections::BTreeSet::new(),
                group_exporter_secrets: std::collections::HashMap::new(),
            };
            let base_snap2 = crate::snapshot::GroupScopedSnapshot {
                group_id: group2.clone(),
                created_at: 2000, // Old, should be pruned
                ..base_snap1.clone()
            };
            let new_snap1 = crate::snapshot::GroupScopedSnapshot {
                group_id: group1.clone(),
                created_at: 5000, // New, keep
                ..base_snap1.clone()
            };

            snapshots.insert((group1.clone(), "old_snap_g1".to_string()), base_snap1);
            snapshots.insert((group2.clone(), "old_snap_g2".to_string()), base_snap2);
            snapshots.insert((group1.clone(), "new_snap_g1".to_string()), new_snap1);
        }

        // Prune snapshots older than 3000
        let pruned = storage.prune_expired_snapshots(3000).unwrap();

        assert_eq!(pruned, 2, "Should have pruned 2 snapshots across groups");

        let remaining1 = storage.list_group_snapshots(&group1).unwrap();
        let remaining2 = storage.list_group_snapshots(&group2).unwrap();

        assert_eq!(remaining1.len(), 1);
        assert_eq!(remaining1[0].0, "new_snap_g1");
        assert!(remaining2.is_empty());
    }

    // ========================================
    // Snapshot tests for OpenMLS data
    // ========================================
    // These tests verify that group-scoped snapshots correctly capture and
    // restore OpenMLS cryptographic state written through the MLS storage layer
    // (which uses MlsCodec::serialize for group_id keys). This ensures the
    // memory backend cannot regress to the same bug that affected the SQLite
    // backend, where snapshots silently missed all OpenMLS rows due to a
    // group_id encoding mismatch.

    /// Snapshot must capture MLS group_data written through the storage layer.
    #[test]
    fn test_snapshot_captures_mls_group_data() {
        let storage = MdkMemoryStorage::default();

        let group_id = GroupId::from_slice(&[1; 32]);
        let nostr_group_id: [u8; 32] = generate_random_bytes(32).try_into().unwrap();
        let group = Group {
            mls_group_id: group_id.clone(),
            nostr_group_id,
            name: "MLS Data Test".to_string(),
            description: "".to_string(),
            admin_pubkeys: BTreeSet::new(),
            last_message_id: None,
            last_message_at: None,
            last_message_processed_at: None,
            epoch: 5,
            state: GroupState::Active,
            image_hash: None,
            image_key: None,
            image_nonce: None,
            self_update_state: SelfUpdateState::Required,
        };
        storage.save_group(group).unwrap();

        // Write MLS group data via the internal MLS storage (uses MlsCodec)
        {
            let mut inner = storage.inner.write();
            inner
                .mls_group_data
                .write(
                    &group_id,
                    mls_storage::GroupDataType::GroupState,
                    &"epoch5_state".to_string(),
                )
                .unwrap();
            inner
                .mls_group_data
                .write(
                    &group_id,
                    mls_storage::GroupDataType::Tree,
                    &"epoch5_tree".to_string(),
                )
                .unwrap();
        }

        // Take a group-scoped snapshot
        storage
            .create_group_snapshot(&group_id, "snap_mls")
            .unwrap();

        // Modify MLS data (simulate advancing to epoch 6)
        {
            let mut inner = storage.inner.write();
            inner
                .mls_group_data
                .write(
                    &group_id,
                    mls_storage::GroupDataType::GroupState,
                    &"epoch6_state".to_string(),
                )
                .unwrap();
            inner
                .mls_group_data
                .write(
                    &group_id,
                    mls_storage::GroupDataType::Tree,
                    &"epoch6_tree".to_string(),
                )
                .unwrap();
        }

        // Verify we're at epoch 6 data
        {
            let inner = storage.inner.read();
            let state: Option<String> = inner
                .mls_group_data
                .read(&group_id, mls_storage::GroupDataType::GroupState)
                .unwrap();
            assert_eq!(state.as_deref(), Some("epoch6_state"));
        }

        // Rollback
        storage
            .rollback_group_to_snapshot(&group_id, "snap_mls")
            .unwrap();

        // Verify MLS data was restored to epoch 5 state
        {
            let inner = storage.inner.read();
            let state: Option<String> = inner
                .mls_group_data
                .read(&group_id, mls_storage::GroupDataType::GroupState)
                .unwrap();
            assert_eq!(
                state.as_deref(),
                Some("epoch5_state"),
                "MLS group_data (GroupState) must be restored to snapshot state"
            );

            let tree: Option<String> = inner
                .mls_group_data
                .read(&group_id, mls_storage::GroupDataType::Tree)
                .unwrap();
            assert_eq!(
                tree.as_deref(),
                Some("epoch5_tree"),
                "MLS group_data (Tree) must be restored to snapshot state"
            );
        }
    }

    /// Snapshot must capture MLS own_leaf_nodes written through the storage layer.
    #[test]
    fn test_snapshot_captures_mls_own_leaf_nodes() {
        let storage = MdkMemoryStorage::default();

        let group_id = GroupId::from_slice(&[2; 32]);
        let nostr_group_id: [u8; 32] = generate_random_bytes(32).try_into().unwrap();
        let group = Group {
            mls_group_id: group_id.clone(),
            nostr_group_id,
            name: "Leaf Node Test".to_string(),
            description: "".to_string(),
            admin_pubkeys: BTreeSet::new(),
            last_message_id: None,
            last_message_at: None,
            last_message_processed_at: None,
            epoch: 0,
            state: GroupState::Active,
            image_hash: None,
            image_key: None,
            image_nonce: None,
            self_update_state: SelfUpdateState::Required,
        };
        storage.save_group(group).unwrap();

        // Write MLS leaf node
        {
            let mut inner = storage.inner.write();
            inner
                .mls_own_leaf_nodes
                .append(&group_id, &"original_leaf".to_string())
                .unwrap();
        }

        // Snapshot
        storage
            .create_group_snapshot(&group_id, "snap_leaf")
            .unwrap();

        // Add another leaf node after snapshot
        {
            let mut inner = storage.inner.write();
            inner
                .mls_own_leaf_nodes
                .append(&group_id, &"added_after_snapshot".to_string())
                .unwrap();
        }

        // Verify 2 leaf nodes exist
        {
            let inner = storage.inner.read();
            let leaves: Vec<String> = inner.mls_own_leaf_nodes.read(&group_id).unwrap();
            assert_eq!(leaves.len(), 2);
        }

        // Rollback
        storage
            .rollback_group_to_snapshot(&group_id, "snap_leaf")
            .unwrap();

        // Verify only original leaf node remains
        {
            let inner = storage.inner.read();
            let leaves: Vec<String> = inner.mls_own_leaf_nodes.read(&group_id).unwrap();
            assert_eq!(
                leaves.len(),
                1,
                "Rollback must restore own_leaf_nodes to snapshot state (1 leaf, not 2)"
            );
            assert_eq!(leaves[0], "original_leaf");
        }
    }

    /// Snapshot must capture MLS epoch_key_pairs written through the storage layer.
    #[test]
    fn test_snapshot_captures_mls_epoch_key_pairs() {
        let storage = MdkMemoryStorage::default();

        let group_id = GroupId::from_slice(&[3; 32]);
        let nostr_group_id: [u8; 32] = generate_random_bytes(32).try_into().unwrap();
        let group = Group {
            mls_group_id: group_id.clone(),
            nostr_group_id,
            name: "Epoch Keys Test".to_string(),
            description: "".to_string(),
            admin_pubkeys: BTreeSet::new(),
            last_message_id: None,
            last_message_at: None,
            last_message_processed_at: None,
            epoch: 5,
            state: GroupState::Active,
            image_hash: None,
            image_key: None,
            image_nonce: None,
            self_update_state: SelfUpdateState::Required,
        };
        storage.save_group(group).unwrap();

        // Write epoch key pairs
        let epoch_id = 5u64;
        let leaf_index = 0u32;
        {
            let mut inner = storage.inner.write();
            inner
                .mls_epoch_key_pairs
                .write(
                    &group_id,
                    &epoch_id,
                    leaf_index,
                    &["epoch5_key_pair".to_string()],
                )
                .unwrap();
        }

        // Snapshot
        storage
            .create_group_snapshot(&group_id, "snap_keys")
            .unwrap();

        // Modify key pairs (simulate epoch advance)
        {
            let mut inner = storage.inner.write();
            inner
                .mls_epoch_key_pairs
                .write(
                    &group_id,
                    &epoch_id,
                    leaf_index,
                    &["epoch6_key_pair".to_string()],
                )
                .unwrap();
        }

        // Rollback
        storage
            .rollback_group_to_snapshot(&group_id, "snap_keys")
            .unwrap();

        // Verify epoch key pairs restored
        {
            let inner = storage.inner.read();
            let key_pairs: Vec<String> = inner
                .mls_epoch_key_pairs
                .read(&group_id, &epoch_id, leaf_index)
                .unwrap();
            assert_eq!(
                key_pairs,
                vec!["epoch5_key_pair"],
                "Rollback must restore epoch_key_pairs to snapshot state"
            );
        }
    }

    /// Full MIP-03 rollback simulation: metadata and crypto state must be
    /// consistent after rollback.
    ///
    /// This is the regression guard — if the memory storage's MlsCodec
    /// serialization for group_id filtering were ever removed or broken,
    /// this test would catch the metadata/crypto split-brain condition.
    #[test]
    fn test_rollback_metadata_crypto_consistency() {
        let storage = MdkMemoryStorage::default();

        let group_id = GroupId::from_slice(&[4; 32]);
        let nostr_group_id: [u8; 32] = generate_random_bytes(32).try_into().unwrap();
        let group = Group {
            mls_group_id: group_id.clone(),
            nostr_group_id,
            name: "Consistency Test".to_string(),
            description: "".to_string(),
            admin_pubkeys: BTreeSet::new(),
            last_message_id: None,
            last_message_at: None,
            last_message_processed_at: None,
            epoch: 5,
            state: GroupState::Active,
            image_hash: None,
            image_key: None,
            image_nonce: None,
            self_update_state: SelfUpdateState::Required,
        };
        storage.save_group(group).unwrap();

        // Write epoch 5 MLS state
        {
            let mut inner = storage.inner.write();
            inner
                .mls_group_data
                .write(
                    &group_id,
                    mls_storage::GroupDataType::GroupState,
                    &"epoch5_state".to_string(),
                )
                .unwrap();
        }

        // Snapshot at epoch 5
        storage
            .create_group_snapshot(&group_id, "snap_epoch5")
            .unwrap();

        // Advance to epoch 6: both MDK metadata and MLS crypto
        {
            let mut g = storage
                .find_group_by_mls_group_id(&group_id)
                .unwrap()
                .unwrap();
            g.epoch = 6;
            storage.save_group(g).unwrap();
        }
        {
            let mut inner = storage.inner.write();
            inner
                .mls_group_data
                .write(
                    &group_id,
                    mls_storage::GroupDataType::GroupState,
                    &"epoch6_state".to_string(),
                )
                .unwrap();
        }

        // MIP-03: rollback to epoch 5
        storage
            .rollback_group_to_snapshot(&group_id, "snap_epoch5")
            .unwrap();

        // Check MDK metadata
        let group_after = storage
            .find_group_by_mls_group_id(&group_id)
            .unwrap()
            .unwrap();
        assert_eq!(group_after.epoch, 5, "MDK epoch should be 5 after rollback");

        // Check MLS crypto state
        let crypto_after: Option<String> = {
            let inner = storage.inner.read();
            inner
                .mls_group_data
                .read(&group_id, mls_storage::GroupDataType::GroupState)
                .unwrap()
        };
        assert_eq!(
            crypto_after.as_deref(),
            Some("epoch5_state"),
            "MLS crypto state must match MDK metadata epoch after rollback. \
             groups.epoch=5 but crypto state is epoch6 means split-brain: \
             MDK thinks epoch 5, MLS engine has epoch 6 keys."
        );
    }
}
