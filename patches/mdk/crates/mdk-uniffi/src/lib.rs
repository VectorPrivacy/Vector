//! UniFFI bindings for mdk-core with SQLite storage
//!
//! This crate provides foreign language bindings for mdk-core using UniFFI.
//! It wraps the MDK core functionality with SQLite storage backend.

#![warn(missing_docs)]

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Mutex;

use mdk_core::{
    Error as MdkError, MDK, MdkConfig as CoreMdkConfig,
    extension::group_image::{
        decrypt_group_image as core_decrypt_group_image,
        derive_upload_keypair as core_derive_upload_keypair,
        prepare_group_image_for_upload as core_prepare_group_image_for_upload,
    },
    groups::{NostrGroupConfigData, NostrGroupDataUpdate},
    messages::MessageProcessingResult,
};
use mdk_sqlite_storage::{EncryptionConfig, MdkSqliteStorage};
use mdk_storage_traits::{
    GroupId,
    groups::{MessageSortOrder, Pagination as MessagePagination, types as group_types},
    messages::types as message_types,
    welcomes::{Pagination as WelcomePagination, types as welcome_types},
};
use nostr::{Event, EventBuilder, EventId, Kind, PublicKey, RelayUrl, Tag, TagKind, UnsignedEvent};

uniffi::setup_scaffolding!();

/// Main MDK instance with SQLite storage
#[derive(uniffi::Object)]
pub struct Mdk {
    mdk: Mutex<MDK<MdkSqliteStorage>>,
}

/// Configuration for MDK behavior
///
/// This struct allows customization of various MDK parameters including
/// message validation and MLS sender ratchet settings. All fields are optional
/// and default to sensible values when not provided.
#[derive(uniffi::Record)]
pub struct MdkConfig {
    /// Maximum age for accepted events in seconds.
    /// Default: 3888000 (45 days)
    pub max_event_age_secs: Option<u64>,

    /// Maximum future timestamp skew allowed in seconds.
    /// Default: 300 (5 minutes)
    pub max_future_skew_secs: Option<u64>,

    /// Number of past message decryption secrets to retain for out-of-order delivery.
    /// Higher values improve tolerance for reordered messages but reduce forward secrecy.
    /// Default: 100
    pub out_of_order_tolerance: Option<u32>,

    /// Maximum number of messages that can be skipped before decryption fails.
    /// Default: 1000
    pub maximum_forward_distance: Option<u32>,

    /// Number of epoch snapshots to retain for rollback support.
    /// Default: 5
    pub epoch_snapshot_retention: Option<u32>,

    /// Time-to-live for snapshots in seconds.
    /// Snapshots older than this will be pruned on startup.
    /// Default: 604800 (1 week)
    pub snapshot_ttl_seconds: Option<u64>,
}

impl From<MdkConfig> for CoreMdkConfig {
    fn from(config: MdkConfig) -> Self {
        let defaults = CoreMdkConfig::default();
        Self {
            max_event_age_secs: config
                .max_event_age_secs
                .unwrap_or(defaults.max_event_age_secs),
            max_future_skew_secs: config
                .max_future_skew_secs
                .unwrap_or(defaults.max_future_skew_secs),
            out_of_order_tolerance: config
                .out_of_order_tolerance
                .unwrap_or(defaults.out_of_order_tolerance),
            maximum_forward_distance: config
                .maximum_forward_distance
                .unwrap_or(defaults.maximum_forward_distance),
            epoch_snapshot_retention: config
                .epoch_snapshot_retention
                .map(|v| v as usize)
                .unwrap_or(defaults.epoch_snapshot_retention),
            snapshot_ttl_seconds: config
                .snapshot_ttl_seconds
                .unwrap_or(defaults.snapshot_ttl_seconds),
        }
    }
}

/// Error type for MDK UniFFI operations
#[derive(uniffi::Enum, Debug, thiserror::Error)]
pub enum MdkUniffiError {
    /// Storage-related error
    #[error("Storage error: {0}")]
    Storage(String),
    /// MDK core error
    #[error("MDK error: {0}")]
    Mdk(String),
    /// Invalid input parameter error
    #[error("Invalid input: {0}")]
    InvalidInput(String),
}

impl From<mdk_sqlite_storage::error::Error> for MdkUniffiError {
    fn from(err: mdk_sqlite_storage::error::Error) -> Self {
        Self::Storage(err.to_string())
    }
}

impl From<MdkError> for MdkUniffiError {
    fn from(err: MdkError) -> Self {
        Self::Mdk(err.to_string())
    }
}

// Helper functions

fn parse_group_id(hex: &str) -> Result<GroupId, MdkUniffiError> {
    hex::decode(hex)
        .map_err(|e| MdkUniffiError::InvalidInput(format!("Invalid group ID hex: {e}")))
        .map(|bytes| GroupId::from_slice(&bytes))
}

fn parse_event_id(hex: &str) -> Result<EventId, MdkUniffiError> {
    EventId::from_hex(hex)
        .map_err(|e| MdkUniffiError::InvalidInput(format!("Invalid event ID: {e}")))
}

fn parse_public_key(hex: &str) -> Result<PublicKey, MdkUniffiError> {
    PublicKey::from_hex(hex)
        .map_err(|e| MdkUniffiError::InvalidInput(format!("Invalid public key: {e}")))
}

fn parse_relay_urls(relays: &[String]) -> Result<Vec<RelayUrl>, MdkUniffiError> {
    relays
        .iter()
        .map(|r| RelayUrl::parse(r))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| MdkUniffiError::InvalidInput(format!("Invalid relay URL: {e}")))
}

fn parse_json<T>(json: &str, context: &str) -> Result<T, MdkUniffiError>
where
    T: serde::de::DeserializeOwned,
{
    serde_json::from_str(json)
        .map_err(|e| MdkUniffiError::InvalidInput(format!("Invalid {context}: {e}")))
}

fn vec_to_array<const N: usize>(vec: Option<Vec<u8>>) -> Result<Option<[u8; N]>, MdkUniffiError> {
    match vec {
        Some(bytes) if bytes.len() == N => {
            let mut arr = [0u8; N];
            arr.copy_from_slice(&bytes);
            Ok(Some(arr))
        }
        Some(bytes) => Err(MdkUniffiError::InvalidInput(format!(
            "Expected {} bytes, got {} bytes",
            N,
            bytes.len()
        ))),
        None => Ok(None),
    }
}

fn parse_message_sort_order(
    sort_order: Option<&str>,
) -> Result<Option<MessageSortOrder>, MdkUniffiError> {
    match sort_order {
        None => Ok(None),
        Some("created_at_first") => Ok(Some(MessageSortOrder::CreatedAtFirst)),
        Some("processed_at_first") => Ok(Some(MessageSortOrder::ProcessedAtFirst)),
        Some(other) => Err(MdkUniffiError::InvalidInput(format!(
            "Invalid sort order: {other}. Expected \"created_at_first\" or \"processed_at_first\""
        ))),
    }
}

fn parse_tags(tags: Vec<Vec<String>>) -> Result<Vec<Tag>, MdkUniffiError> {
    tags.into_iter()
        .map(|tag_vec| {
            Tag::parse(tag_vec)
                .map_err(|e| MdkUniffiError::InvalidInput(format!("Failed to parse tag: {e}")))
        })
        .collect()
}

fn welcome_from_uniffi(w: Welcome) -> Result<welcome_types::Welcome, MdkUniffiError> {
    let id = parse_event_id(&w.id)?;
    let event: UnsignedEvent = parse_json(&w.event_json, "welcome event JSON")?;
    let mls_group_id = parse_group_id(&w.mls_group_id)?;

    let nostr_group_id_vec = hex::decode(&w.nostr_group_id)
        .map_err(|e| MdkUniffiError::InvalidInput(format!("Invalid nostr group ID hex: {e}")))?;
    let nostr_group_id: [u8; 32] = nostr_group_id_vec
        .try_into()
        .map_err(|_| MdkUniffiError::InvalidInput("Nostr group ID must be 32 bytes".to_string()))?;

    let group_image_hash = vec_to_array::<32>(w.group_image_hash)?;
    let group_image_key =
        vec_to_array::<32>(w.group_image_key)?.map(mdk_storage_traits::Secret::new);
    let group_image_nonce =
        vec_to_array::<12>(w.group_image_nonce)?.map(mdk_storage_traits::Secret::new);

    let group_admin_pubkeys: Result<BTreeSet<PublicKey>, _> = w
        .group_admin_pubkeys
        .into_iter()
        .map(|pk| parse_public_key(&pk))
        .collect();
    let group_admin_pubkeys = group_admin_pubkeys?;

    let group_relays = parse_relay_urls(&w.group_relays)?.into_iter().collect();

    let welcomer = parse_public_key(&w.welcomer)?;
    let wrapper_event_id = parse_event_id(&w.wrapper_event_id)?;

    let state = welcome_types::WelcomeState::from_str(&w.state)
        .map_err(|e| MdkUniffiError::InvalidInput(format!("Invalid welcome state: {e}")))?;

    Ok(welcome_types::Welcome {
        id,
        event,
        mls_group_id,
        nostr_group_id,
        group_name: w.group_name,
        group_description: w.group_description,
        group_image_hash,
        group_image_key,
        group_image_nonce,
        group_admin_pubkeys,
        group_relays,
        welcomer,
        member_count: w.member_count,
        state,
        wrapper_event_id,
    })
}

impl Mdk {
    /// Lock the internal MDK instance for exclusive access.
    /// Returns an error if the mutex is poisoned.
    /// Using MDK correctly (do NOT share memory across threads) should never result in a poisoned mutex.
    fn lock(&self) -> Result<std::sync::MutexGuard<'_, MDK<MdkSqliteStorage>>, MdkUniffiError> {
        self.mdk.lock().map_err(|_| {
            MdkUniffiError::Mdk(
                "MDK mutex poisoned. This indicates a critical internal error. Using MDK correctly (do NOT share memory across threads) should never result in a poisoned mutex.".to_string(),
            )
        })
    }
}

/// Create a new MDK instance with encrypted SQLite storage using automatic key management.
///
/// This is the recommended constructor for production use. The database encryption key
/// is automatically retrieved from (or generated and stored in) the platform's native
/// keyring (Keychain on macOS/iOS, Keystore on Android, etc.).
///
/// # Prerequisites
///
/// The host application must initialize a platform-specific keyring store before calling
/// this function:
///
/// - **macOS/iOS**: `keyring_core::set_default_store(AppleStore::new())`
/// - **Android**: Initialize from Kotlin (see Android documentation)
/// - **Windows**: `keyring_core::set_default_store(WindowsStore::new())`
/// - **Linux**: `keyring_core::set_default_store(KeyutilsStore::new())`
///
/// # Arguments
///
/// * `db_path` - Path to the SQLite database file
/// * `service_id` - A stable, host-defined application identifier (e.g., "com.example.myapp")
/// * `db_key_id` - A stable identifier for this database's key (e.g., "mdk.db.key.default")
/// * `config` - Optional MDK configuration. If None, uses default configuration.
///
/// # Errors
///
/// Returns an error if:
/// - No keyring store has been initialized
/// - The keyring is unavailable or inaccessible
/// - The database cannot be opened or created
#[uniffi::export]
pub fn new_mdk(
    db_path: String,
    service_id: String,
    db_key_id: String,
    config: Option<MdkConfig>,
) -> Result<Mdk, MdkUniffiError> {
    let storage = MdkSqliteStorage::new(PathBuf::from(db_path), &service_id, &db_key_id)?;
    let mdk = match config {
        Some(c) => MDK::builder(storage).with_config(c.into()).build(),
        None => MDK::new(storage),
    };
    Ok(Mdk {
        mdk: Mutex::new(mdk),
    })
}

/// Create a new MDK instance with encrypted SQLite storage using a directly provided key.
///
/// Use this when you want to manage encryption keys yourself rather than using the
/// platform keyring. For most applications, prefer `new_mdk` which handles key
/// management automatically.
///
/// # Arguments
///
/// * `db_path` - Path to the SQLite database file
/// * `encryption_key` - 32-byte encryption key (must be exactly 32 bytes)
/// * `config` - Optional MDK configuration. If None, uses default configuration.
///
/// # Errors
///
/// Returns an error if the key is not 32 bytes or if the database cannot be opened.
#[uniffi::export]
pub fn new_mdk_with_key(
    db_path: String,
    encryption_key: Vec<u8>,
    config: Option<MdkConfig>,
) -> Result<Mdk, MdkUniffiError> {
    let encryption_config = EncryptionConfig::from_slice(&encryption_key)
        .map_err(|e| MdkUniffiError::InvalidInput(format!("Invalid encryption key: {}", e)))?;
    let storage = MdkSqliteStorage::new_with_key(PathBuf::from(db_path), encryption_config)?;
    let mdk = match config {
        Some(c) => MDK::builder(storage).with_config(c.into()).build(),
        None => MDK::new(storage),
    };
    Ok(Mdk {
        mdk: Mutex::new(mdk),
    })
}

/// Create a new MDK instance with unencrypted SQLite storage.
///
/// ⚠️ **WARNING**: This creates an unencrypted database. Sensitive MLS state
/// including exporter secrets will be stored in plaintext.
///
/// Only use this for development or testing. For production use, use `new_mdk`
/// with an encryption key.
///
/// # Arguments
///
/// * `db_path` - Path to the SQLite database file
/// * `config` - Optional MDK configuration. If None, uses default configuration.
#[uniffi::export]
pub fn new_mdk_unencrypted(
    db_path: String,
    config: Option<MdkConfig>,
) -> Result<Mdk, MdkUniffiError> {
    let storage = MdkSqliteStorage::new_unencrypted(PathBuf::from(db_path))?;
    let mdk = match config {
        Some(c) => MDK::builder(storage).with_config(c.into()).build(),
        None => MDK::new(storage),
    };
    Ok(Mdk {
        mdk: Mutex::new(mdk),
    })
}

#[uniffi::export]
impl Mdk {
    /// Create a key package for a Nostr event
    ///
    /// This function does NOT add the NIP-70 protected tag, ensuring maximum relay
    /// compatibility. Many popular relays (Damus, Primal, nos.lol) reject protected events.
    /// If you need the protected tag, use `create_key_package_for_event_with_options` instead.
    pub fn create_key_package_for_event(
        &self,
        public_key: String,
        relays: Vec<String>,
    ) -> Result<KeyPackageResult, MdkUniffiError> {
        let pubkey = parse_public_key(&public_key)?;
        let relay_urls = parse_relay_urls(&relays)?;

        let mdk = self.lock()?;
        let (key_package_hex, tags, hash_ref) =
            mdk.create_key_package_for_event(&pubkey, relay_urls)?;

        let tags: Vec<Vec<String>> = tags.iter().map(|tag| tag.as_slice().to_vec()).collect();

        Ok(KeyPackageResult {
            key_package: key_package_hex,
            tags,
            hash_ref,
        })
    }

    /// Create a key package for a Nostr event with additional options
    ///
    /// # Arguments
    ///
    /// * `public_key` - The Nostr public key (hex) for the credential
    /// * `relays` - Relay URLs where the key package will be published
    /// * `protected` - Whether to add the NIP-70 protected tag. When `true`, relays that
    ///   implement NIP-70 will reject republishing by third parties. However, many popular
    ///   relays reject protected events entirely. Set to `false` for maximum relay
    ///   compatibility.
    pub fn create_key_package_for_event_with_options(
        &self,
        public_key: String,
        relays: Vec<String>,
        protected: bool,
    ) -> Result<KeyPackageResult, MdkUniffiError> {
        let pubkey = parse_public_key(&public_key)?;
        let relay_urls = parse_relay_urls(&relays)?;

        let mdk = self.lock()?;
        let (key_package_hex, tags, hash_ref) =
            mdk.create_key_package_for_event_with_options(&pubkey, relay_urls, protected)?;

        let tags: Vec<Vec<String>> = tags.iter().map(|tag| tag.as_slice().to_vec()).collect();

        Ok(KeyPackageResult {
            key_package: key_package_hex,
            tags,
            hash_ref,
        })
    }

    /// Parse a key package from a Nostr event
    pub fn parse_key_package(&self, event_json: String) -> Result<String, MdkUniffiError> {
        let event: Event = parse_json(&event_json, "event JSON")?;
        self.lock()?.parse_key_package(&event)?;
        Ok(event.content)
    }

    /// Get all groups
    pub fn get_groups(&self) -> Result<Vec<Group>, MdkUniffiError> {
        Ok(self
            .lock()?
            .get_groups()?
            .into_iter()
            .map(Group::from)
            .collect())
    }

    /// Get a group by MLS group ID
    pub fn get_group(&self, mls_group_id: String) -> Result<Option<Group>, MdkUniffiError> {
        let group_id = parse_group_id(&mls_group_id)?;
        Ok(self.lock()?.get_group(&group_id)?.map(Group::from))
    }

    /// Get group IDs that need a self-update (post-join or stale rotation).
    pub fn groups_needing_self_update(
        &self,
        threshold_secs: u64,
    ) -> Result<Vec<String>, MdkUniffiError> {
        Ok(self
            .lock()?
            .groups_needing_self_update(threshold_secs)?
            .into_iter()
            .map(|id| hex::encode(id.as_slice()))
            .collect())
    }

    /// Get members of a group
    pub fn get_members(&self, mls_group_id: String) -> Result<Vec<String>, MdkUniffiError> {
        let group_id = parse_group_id(&mls_group_id)?;
        Ok(self
            .lock()?
            .get_members(&group_id)?
            .into_iter()
            .map(|pk| pk.to_hex())
            .collect())
    }

    /// Get messages for a group with optional pagination
    ///
    /// # Arguments
    ///
    /// * `mls_group_id` - Hex-encoded MLS group ID
    /// * `limit` - Optional maximum number of messages to return (defaults to 1000 if None)
    /// * `offset` - Optional number of messages to skip (defaults to 0 if None)
    /// * `sort_order` - Optional sort order: `"created_at_first"` (default) or `"processed_at_first"`
    ///
    /// # Returns
    ///
    /// Returns a vector of messages in the requested sort order
    pub fn get_messages(
        &self,
        mls_group_id: String,
        limit: Option<u32>,
        offset: Option<u32>,
        sort_order: Option<String>,
    ) -> Result<Vec<Message>, MdkUniffiError> {
        let group_id = parse_group_id(&mls_group_id)?;
        let sort = parse_message_sort_order(sort_order.as_deref())?;
        let pagination = match (limit, offset, sort) {
            (None, None, None) => None,
            _ => {
                let mut p =
                    MessagePagination::new(limit.map(|l| l as usize), offset.map(|o| o as usize));
                p.sort_order = sort;
                Some(p)
            }
        };
        Ok(self
            .lock()?
            .get_messages(&group_id, pagination)?
            .into_iter()
            .map(Message::from)
            .collect())
    }

    /// Get a message by event ID within a specific group
    ///
    /// # Arguments
    ///
    /// * `mls_group_id` - The MLS group ID the message belongs to (hex-encoded)
    /// * `event_id` - The Nostr event ID to look up (hex-encoded)
    ///
    /// # Returns
    ///
    /// Returns the message if found, None otherwise
    pub fn get_message(
        &self,
        mls_group_id: String,
        event_id: String,
    ) -> Result<Option<Message>, MdkUniffiError> {
        let group_id = parse_group_id(&mls_group_id)?;
        let event_id = parse_event_id(&event_id)?;
        Ok(self
            .lock()?
            .get_message(&group_id, &event_id)?
            .map(Message::from))
    }

    /// Get the most recent message in a group according to the given sort order
    ///
    /// This is useful for clients that use `"processed_at_first"` sort order and need
    /// a "last message" value that is consistent with their `get_messages()` ordering.
    /// The cached `group.last_message_id` always reflects `"created_at_first"` ordering.
    ///
    /// # Arguments
    ///
    /// * `mls_group_id` - Hex-encoded MLS group ID
    /// * `sort_order` - Sort order: `"created_at_first"` or `"processed_at_first"`
    ///
    /// # Returns
    ///
    /// Returns the most recent message under the given ordering, or None if the group has no messages
    pub fn get_last_message(
        &self,
        mls_group_id: String,
        sort_order: String,
    ) -> Result<Option<Message>, MdkUniffiError> {
        let group_id = parse_group_id(&mls_group_id)?;
        let sort = parse_message_sort_order(Some(&sort_order))?
            .ok_or_else(|| MdkUniffiError::InvalidInput("sort_order is required".to_string()))?;
        Ok(self
            .lock()?
            .get_last_message(&group_id, sort)?
            .map(Message::from))
    }

    /// Get pending welcomes with optional pagination
    ///
    /// # Arguments
    ///
    /// * `limit` - Optional maximum number of welcomes to return (defaults to 1000 if None)
    /// * `offset` - Optional number of welcomes to skip (defaults to 0 if None)
    ///
    /// # Returns
    ///
    /// Returns a vector of pending welcomes ordered by ID (descending)
    pub fn get_pending_welcomes(
        &self,
        limit: Option<u32>,
        offset: Option<u32>,
    ) -> Result<Vec<Welcome>, MdkUniffiError> {
        let pagination = match (limit, offset) {
            (None, None) => None,
            _ => Some(WelcomePagination::new(
                limit.map(|l| l as usize),
                offset.map(|o| o as usize),
            )),
        };
        Ok(self
            .lock()?
            .get_pending_welcomes(pagination)?
            .into_iter()
            .map(Welcome::from)
            .collect())
    }

    /// Get a welcome by event ID
    pub fn get_welcome(&self, event_id: String) -> Result<Option<Welcome>, MdkUniffiError> {
        let event_id = parse_event_id(&event_id)?;
        Ok(self.lock()?.get_welcome(&event_id)?.map(Welcome::from))
    }

    /// Process a welcome message
    pub fn process_welcome(
        &self,
        wrapper_event_id: String,
        rumor_event_json: String,
    ) -> Result<Welcome, MdkUniffiError> {
        let wrapper_id = parse_event_id(&wrapper_event_id)?;
        let rumor_event: UnsignedEvent = parse_json(&rumor_event_json, "rumor event JSON")?;
        Ok(Welcome::from(
            self.lock()?.process_welcome(&wrapper_id, &rumor_event)?,
        ))
    }

    /// Accept a welcome message
    pub fn accept_welcome(&self, welcome: Welcome) -> Result<(), MdkUniffiError> {
        let welcome = welcome_from_uniffi(welcome)?;
        self.lock()?.accept_welcome(&welcome)?;
        Ok(())
    }

    /// Accept a welcome message from JSON
    pub fn accept_welcome_json(&self, welcome_json: String) -> Result<(), MdkUniffiError> {
        let welcome: welcome_types::Welcome = parse_json(&welcome_json, "welcome JSON")?;
        self.lock()?.accept_welcome(&welcome)?;
        Ok(())
    }

    /// Decline a welcome message
    pub fn decline_welcome(&self, welcome: Welcome) -> Result<(), MdkUniffiError> {
        let welcome = welcome_from_uniffi(welcome)?;
        self.lock()?.decline_welcome(&welcome)?;
        Ok(())
    }

    /// Decline a welcome message from JSON
    pub fn decline_welcome_json(&self, welcome_json: String) -> Result<(), MdkUniffiError> {
        let welcome: welcome_types::Welcome = parse_json(&welcome_json, "welcome JSON")?;
        self.lock()?.decline_welcome(&welcome)?;
        Ok(())
    }

    /// Get relays for a group
    pub fn get_relays(&self, mls_group_id: String) -> Result<Vec<String>, MdkUniffiError> {
        let group_id = parse_group_id(&mls_group_id)?;
        Ok(self
            .lock()?
            .get_relays(&group_id)?
            .into_iter()
            .map(|r| r.to_string())
            .collect())
    }

    /// Create a new group
    pub fn create_group(
        &self,
        creator_public_key: String,
        member_key_package_events_json: Vec<String>,
        name: String,
        description: String,
        relays: Vec<String>,
        admins: Vec<String>,
    ) -> Result<CreateGroupResult, MdkUniffiError> {
        let creator_pubkey = parse_public_key(&creator_public_key)?;
        let relay_urls = parse_relay_urls(&relays)?;
        let admin_pubkeys: Result<Vec<PublicKey>, _> =
            admins.iter().map(|a| parse_public_key(a)).collect();
        let admin_pubkeys = admin_pubkeys?;

        let member_key_package_events: Result<Vec<Event>, _> = member_key_package_events_json
            .iter()
            .map(|json| parse_json(json, "key package event JSON"))
            .collect();
        let member_key_package_events = member_key_package_events?;

        let config = NostrGroupConfigData::new(
            name,
            description,
            None, // image_hash
            None, // image_key
            None, // image_nonce
            relay_urls,
            admin_pubkeys,
        );

        let mdk = self.lock()?;
        let result = mdk.create_group(&creator_pubkey, member_key_package_events, config)?;

        let welcome_rumors_json: Vec<String> = result
            .welcome_rumors
            .iter()
            .map(|rumor| {
                serde_json::to_string(rumor).map_err(|e| {
                    MdkUniffiError::InvalidInput(format!("Failed to serialize welcome rumor: {e}"))
                })
            })
            .collect::<Result<Vec<_>, _>>()?;

        Ok(CreateGroupResult {
            group: Group::from(result.group),
            welcome_rumors_json,
        })
    }

    /// Add members to a group
    pub fn add_members(
        &self,
        mls_group_id: String,
        key_package_events_json: Vec<String>,
    ) -> Result<UpdateGroupResult, MdkUniffiError> {
        let group_id = parse_group_id(&mls_group_id)?;

        let key_package_events: Result<Vec<Event>, _> = key_package_events_json
            .iter()
            .map(|json| parse_json(json, "key package event JSON"))
            .collect();
        let key_package_events = key_package_events?;

        let mdk = self.lock()?;
        let result = mdk.add_members(&group_id, &key_package_events)?;

        let evolution_event_json = serde_json::to_string(&result.evolution_event).map_err(|e| {
            MdkUniffiError::InvalidInput(format!("Failed to serialize evolution event: {e}"))
        })?;

        let welcome_rumors_json: Option<Vec<String>> = result
            .welcome_rumors
            .map(|rumors| {
                rumors
                    .iter()
                    .map(|rumor| {
                        serde_json::to_string(rumor).map_err(|e| {
                            MdkUniffiError::InvalidInput(format!(
                                "Failed to serialize welcome rumor: {e}"
                            ))
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?;

        Ok(UpdateGroupResult {
            evolution_event_json,
            welcome_rumors_json,
            mls_group_id: hex::encode(result.mls_group_id.as_slice()),
        })
    }

    /// Remove members from a group
    pub fn remove_members(
        &self,
        mls_group_id: String,
        member_public_keys: Vec<String>,
    ) -> Result<UpdateGroupResult, MdkUniffiError> {
        let group_id = parse_group_id(&mls_group_id)?;

        let pubkeys: Result<Vec<PublicKey>, _> = member_public_keys
            .iter()
            .map(|pk| parse_public_key(pk))
            .collect();
        let pubkeys = pubkeys?;

        let mdk = self.lock()?;
        let result = mdk.remove_members(&group_id, &pubkeys)?;

        let evolution_event_json = serde_json::to_string(&result.evolution_event).map_err(|e| {
            MdkUniffiError::InvalidInput(format!("Failed to serialize evolution event: {e}"))
        })?;

        let welcome_rumors_json: Option<Vec<String>> = result
            .welcome_rumors
            .map(|rumors| {
                rumors
                    .iter()
                    .map(|rumor| {
                        serde_json::to_string(rumor).map_err(|e| {
                            MdkUniffiError::InvalidInput(format!(
                                "Failed to serialize welcome rumor: {e}"
                            ))
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?;

        Ok(UpdateGroupResult {
            evolution_event_json,
            welcome_rumors_json,
            mls_group_id: hex::encode(result.mls_group_id.as_slice()),
        })
    }

    /// Merge pending commit for a group
    pub fn merge_pending_commit(&self, mls_group_id: String) -> Result<(), MdkUniffiError> {
        let group_id = parse_group_id(&mls_group_id)?;
        self.lock()?.merge_pending_commit(&group_id)?;
        Ok(())
    }

    /// Clear pending commit for a group
    ///
    /// This rolls back the group to its pre-commit state — no epoch advance, no member changes.
    /// Call this when publish exhausts retries to recover from failed relay publishes.
    ///
    /// # Arguments
    /// * `mls_group_id` - The MLS group ID to clear the pending commit for (hex-encoded)
    ///
    /// # Returns
    /// * `Ok(())` - if the pending commit was cleared successfully
    /// * `Err` - if the group doesn't exist or another error occurs
    pub fn clear_pending_commit(&self, mls_group_id: String) -> Result<(), MdkUniffiError> {
        let group_id = parse_group_id(&mls_group_id)?;
        self.lock()?.clear_pending_commit(&group_id)?;
        Ok(())
    }

    /// Sync group metadata from MLS
    pub fn sync_group_metadata_from_mls(&self, mls_group_id: String) -> Result<(), MdkUniffiError> {
        let group_id = parse_group_id(&mls_group_id)?;
        self.lock()?.sync_group_metadata_from_mls(&group_id)?;
        Ok(())
    }

    /// Create a message in a group
    pub fn create_message(
        &self,
        mls_group_id: String,
        sender_public_key: String,
        content: String,
        kind: u16,
        tags: Option<Vec<Vec<String>>>,
    ) -> Result<String, MdkUniffiError> {
        let group_id = parse_group_id(&mls_group_id)?;
        let sender_pubkey = parse_public_key(&sender_public_key)?;
        let mdk = self.lock()?;

        let mut builder = EventBuilder::new(Kind::Custom(kind), content);

        if let Some(tags_vec) = tags {
            let parsed_tags = parse_tags(tags_vec)?;
            builder = builder.tags(parsed_tags);
        }

        let rumor = builder.build(sender_pubkey);

        let event = mdk.create_message(&group_id, rumor)?;

        let event_json = serde_json::to_string(&event)
            .map_err(|e| MdkUniffiError::InvalidInput(format!("Failed to serialize event: {e}")))?;

        Ok(event_json)
    }

    /// Update the current member's leaf node in an MLS group
    pub fn self_update(&self, mls_group_id: String) -> Result<UpdateGroupResult, MdkUniffiError> {
        let group_id = parse_group_id(&mls_group_id)?;
        let mdk = self.lock()?;
        let result = mdk.self_update(&group_id)?;

        let evolution_event_json = serde_json::to_string(&result.evolution_event).map_err(|e| {
            MdkUniffiError::InvalidInput(format!("Failed to serialize evolution event: {e}"))
        })?;

        let welcome_rumors_json: Option<Vec<String>> = result
            .welcome_rumors
            .map(|rumors| {
                rumors
                    .iter()
                    .map(|rumor| {
                        serde_json::to_string(rumor).map_err(|e| {
                            MdkUniffiError::InvalidInput(format!(
                                "Failed to serialize welcome rumor: {e}"
                            ))
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?;

        Ok(UpdateGroupResult {
            evolution_event_json,
            welcome_rumors_json,
            mls_group_id: hex::encode(result.mls_group_id.as_slice()),
        })
    }

    /// Create a proposal to leave the group
    pub fn leave_group(&self, mls_group_id: String) -> Result<UpdateGroupResult, MdkUniffiError> {
        let group_id = parse_group_id(&mls_group_id)?;
        let mdk = self.lock()?;
        let result = mdk.leave_group(&group_id)?;

        let evolution_event_json = serde_json::to_string(&result.evolution_event).map_err(|e| {
            MdkUniffiError::InvalidInput(format!("Failed to serialize evolution event: {e}"))
        })?;

        let welcome_rumors_json: Option<Vec<String>> = result
            .welcome_rumors
            .map(|rumors| {
                rumors
                    .iter()
                    .map(|rumor| {
                        serde_json::to_string(rumor).map_err(|e| {
                            MdkUniffiError::InvalidInput(format!(
                                "Failed to serialize welcome rumor: {e}"
                            ))
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?;

        Ok(UpdateGroupResult {
            evolution_event_json,
            welcome_rumors_json,
            mls_group_id: hex::encode(result.mls_group_id.as_slice()),
        })
    }

    /// Update group data (name, description, image, relays, admins)
    pub fn update_group_data(
        &self,
        mls_group_id: String,
        update: GroupDataUpdate,
    ) -> Result<UpdateGroupResult, MdkUniffiError> {
        let group_id = parse_group_id(&mls_group_id)?;

        let mut group_update = NostrGroupDataUpdate::new();

        if let Some(name) = update.name {
            group_update = group_update.name(name);
        }

        if let Some(description) = update.description {
            group_update = group_update.description(description);
        }

        if let Some(image_hash) = update.image_hash {
            group_update = group_update.image_hash(vec_to_array::<32>(image_hash)?);
        }

        if let Some(image_key) = update.image_key {
            group_update = group_update.image_key(vec_to_array::<32>(image_key)?);
        }

        if let Some(image_nonce) = update.image_nonce {
            group_update = group_update.image_nonce(vec_to_array::<12>(image_nonce)?);
        }

        if let Some(relays) = update.relays {
            let relay_urls = parse_relay_urls(&relays)?;
            group_update = group_update.relays(relay_urls);
        }

        if let Some(admins) = update.admins {
            let admin_pubkeys: Result<Vec<PublicKey>, _> =
                admins.iter().map(|a| parse_public_key(a)).collect();
            let admin_pubkeys = admin_pubkeys?;
            group_update = group_update.admins(admin_pubkeys);
        }

        let mdk = self.lock()?;
        let result = mdk.update_group_data(&group_id, group_update)?;

        let evolution_event_json = serde_json::to_string(&result.evolution_event).map_err(|e| {
            MdkUniffiError::InvalidInput(format!("Failed to serialize evolution event: {e}"))
        })?;

        let welcome_rumors_json: Option<Vec<String>> = result
            .welcome_rumors
            .map(|rumors| {
                rumors
                    .iter()
                    .map(|rumor| {
                        serde_json::to_string(rumor).map_err(|e| {
                            MdkUniffiError::InvalidInput(format!(
                                "Failed to serialize welcome rumor: {e}"
                            ))
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?;

        Ok(UpdateGroupResult {
            evolution_event_json,
            welcome_rumors_json,
            mls_group_id: hex::encode(result.mls_group_id.as_slice()),
        })
    }

    /// Process an incoming MLS message
    pub fn process_message(
        &self,
        event_json: String,
    ) -> Result<ProcessMessageResult, MdkUniffiError> {
        let event: Event = parse_json(&event_json, "event JSON")?;
        let mdk = self.lock()?;
        let result = mdk.process_message(&event)?;

        Ok(match result {
            MessageProcessingResult::ApplicationMessage(message) => {
                ProcessMessageResult::ApplicationMessage {
                    message: Message::from(message),
                }
            }
            MessageProcessingResult::Proposal(update_result) => {
                let evolution_event_json = serde_json::to_string(&update_result.evolution_event)
                    .map_err(|e| {
                        MdkUniffiError::InvalidInput(format!(
                            "Failed to serialize evolution event: {e}"
                        ))
                    })?;

                let welcome_rumors_json: Option<Vec<String>> = update_result
                    .welcome_rumors
                    .map(|rumors| {
                        rumors
                            .iter()
                            .map(|rumor| {
                                serde_json::to_string(rumor).map_err(|e| {
                                    MdkUniffiError::InvalidInput(format!(
                                        "Failed to serialize welcome rumor: {e}"
                                    ))
                                })
                            })
                            .collect::<Result<Vec<_>, _>>()
                    })
                    .transpose()?;

                ProcessMessageResult::Proposal {
                    result: UpdateGroupResult {
                        evolution_event_json,
                        welcome_rumors_json,
                        mls_group_id: hex::encode(update_result.mls_group_id.as_slice()),
                    },
                }
            }
            MessageProcessingResult::PendingProposal { mls_group_id } => {
                ProcessMessageResult::PendingProposal {
                    mls_group_id: hex::encode(mls_group_id.as_slice()),
                }
            }
            MessageProcessingResult::ExternalJoinProposal { mls_group_id } => {
                ProcessMessageResult::ExternalJoinProposal {
                    mls_group_id: hex::encode(mls_group_id.as_slice()),
                }
            }
            MessageProcessingResult::Commit { mls_group_id } => ProcessMessageResult::Commit {
                mls_group_id: hex::encode(mls_group_id.as_slice()),
            },
            MessageProcessingResult::Unprocessable { mls_group_id } => {
                ProcessMessageResult::Unprocessable {
                    mls_group_id: hex::encode(mls_group_id.as_slice()),
                }
            }
            MessageProcessingResult::IgnoredProposal {
                mls_group_id,
                reason,
            } => ProcessMessageResult::IgnoredProposal {
                mls_group_id: hex::encode(mls_group_id.as_slice()),
                reason,
            },
            MessageProcessingResult::PreviouslyFailed => ProcessMessageResult::PreviouslyFailed,
        })
    }
}

/// Result of creating a key package
#[derive(uniffi::Record)]
pub struct KeyPackageResult {
    /// Hex-encoded key package
    pub key_package: String,
    /// JSON-encoded tags for the key package event
    pub tags: Vec<Vec<String>>,
    /// Serialized hash_ref bytes for the key package (for lifecycle tracking)
    pub hash_ref: Vec<u8>,
}

/// Result of creating a group
#[derive(uniffi::Record)]
pub struct CreateGroupResult {
    /// The created group
    pub group: Group,
    /// JSON-encoded welcome rumors to be published
    pub welcome_rumors_json: Vec<String>,
}

/// Result of updating a group
#[derive(uniffi::Record)]
pub struct UpdateGroupResult {
    /// JSON-encoded evolution event to be published
    pub evolution_event_json: String,
    /// Optional JSON-encoded welcome rumors to be published
    pub welcome_rumors_json: Option<Vec<String>>,
    /// Hex-encoded MLS group ID
    pub mls_group_id: String,
}

/// Configuration for updating group data with optional fields
#[derive(uniffi::Record)]
pub struct GroupDataUpdate {
    /// Group name (optional)
    pub name: Option<String>,
    /// Group description (optional)
    pub description: Option<String>,
    /// Group image hash (optional, use Some(None) to clear)
    pub image_hash: Option<Option<Vec<u8>>>,
    /// Group image encryption key (optional, use Some(None) to clear)
    pub image_key: Option<Option<Vec<u8>>>,
    /// Group image encryption nonce (optional, use Some(None) to clear)
    pub image_nonce: Option<Option<Vec<u8>>>,
    /// Relays used by the group (optional)
    pub relays: Option<Vec<String>>,
    /// Group admins (optional)
    pub admins: Option<Vec<String>>,
}

/// Result of processing a message
#[derive(uniffi::Enum)]
pub enum ProcessMessageResult {
    /// An application message (usually a chat message)
    ApplicationMessage {
        /// The processed message
        message: Message,
    },
    /// A proposal message that was auto-committed by an admin receiver
    Proposal {
        /// The proposal result containing evolution event and welcome rumors
        result: UpdateGroupResult,
    },
    /// A pending proposal stored but not committed (receiver is not admin)
    PendingProposal {
        /// Hex-encoded MLS group ID this pending proposal belongs to
        mls_group_id: String,
    },
    /// External join proposal
    ExternalJoinProposal {
        /// Hex-encoded MLS group ID this proposal belongs to
        mls_group_id: String,
    },
    /// Commit message
    Commit {
        /// Hex-encoded MLS group ID this commit applies to
        mls_group_id: String,
    },
    /// Unprocessable message
    Unprocessable {
        /// Hex-encoded MLS group ID of the message that could not be processed
        mls_group_id: String,
    },
    /// Proposal was ignored and not stored
    IgnoredProposal {
        /// Hex-encoded MLS group ID this proposal was for
        mls_group_id: String,
        /// Reason the proposal was ignored
        reason: String,
    },
    /// Message was previously marked as failed and cannot be reprocessed
    ///
    /// This is returned when attempting to process a message that previously
    /// failed. Unlike throwing an error, this allows clients to handle the
    /// case gracefully without crashing.
    PreviouslyFailed,
}

/// Group representation
#[derive(uniffi::Record)]
pub struct Group {
    /// Hex-encoded MLS group ID
    pub mls_group_id: String,
    /// Hex-encoded Nostr group ID
    pub nostr_group_id: String,
    /// Group name
    pub name: String,
    /// Group description
    pub description: String,
    /// Optional group image hash
    pub image_hash: Option<Vec<u8>>,
    /// Optional group image encryption key
    pub image_key: Option<Vec<u8>>,
    /// Optional group image encryption nonce
    pub image_nonce: Option<Vec<u8>>,
    /// List of admin public keys (hex-encoded)
    pub admin_pubkeys: Vec<String>,
    /// Last message event ID (hex-encoded)
    pub last_message_id: Option<String>,
    /// Timestamp of last message (Unix timestamp, sender's `created_at`)
    pub last_message_at: Option<u64>,
    /// Timestamp when the last message was processed/received (Unix timestamp)
    ///
    /// This differs from `last_message_at` which reflects the sender's timestamp.
    /// `last_message_processed_at` reflects when this client received the message,
    /// which may differ due to network delays or clock skew.
    pub last_message_processed_at: Option<u64>,
    /// Current epoch number
    pub epoch: u64,
    /// Group state (e.g., "active", "archived")
    pub state: String,
    /// Self-update tracking state.
    /// - `"required"`: Must perform a post-join self-update (MIP-02).
    /// - `"completed_at:<unix_timestamp>"`: Last self-update merged at this time (MIP-00).
    pub self_update_state: String,
}

impl From<group_types::Group> for Group {
    fn from(g: group_types::Group) -> Self {
        Self {
            mls_group_id: hex::encode(g.mls_group_id.as_slice()),
            nostr_group_id: hex::encode(g.nostr_group_id),
            name: g.name.clone(),
            description: g.description.clone(),
            image_hash: g.image_hash.map(Into::into),
            image_key: g.image_key.map(|k| k.as_ref().to_vec()),
            image_nonce: g.image_nonce.map(|n| n.as_ref().to_vec()),
            admin_pubkeys: g.admin_pubkeys.iter().map(|pk| pk.to_hex()).collect(),
            last_message_id: g.last_message_id.map(|id| id.to_hex()),
            last_message_at: g.last_message_at.map(|ts| ts.as_secs()),
            last_message_processed_at: g.last_message_processed_at.map(|ts| ts.as_secs()),
            epoch: g.epoch,
            state: g.state.as_str().to_string(),
            self_update_state: match g.self_update_state {
                group_types::SelfUpdateState::Required => "required".to_string(),
                group_types::SelfUpdateState::CompletedAt(ts) => {
                    format!("completed_at:{}", ts.as_secs())
                }
            },
        }
    }
}

/// Message representation
#[derive(uniffi::Record)]
pub struct Message {
    /// Message ID (hex-encoded event ID)
    pub id: String,
    /// Hex-encoded MLS group ID
    pub mls_group_id: String,
    /// Hex-encoded Nostr group ID
    pub nostr_group_id: String,
    /// Event ID (hex-encoded)
    pub event_id: String,
    /// Sender public key (hex-encoded)
    pub sender_pubkey: String,
    /// JSON representation of the event
    pub event_json: String,
    /// Timestamp when message was created by the sender (Unix timestamp).
    /// Note: This timestamp comes from the sender's device and may differ
    /// from `processed_at` due to clock skew between devices.
    pub created_at: u64,
    /// Timestamp when this client processed/received the message (Unix timestamp).
    /// This is useful for clients that want to display messages in the order
    /// they were received locally, rather than in the order they were created.
    pub processed_at: u64,
    /// Message kind
    pub kind: u16,
    /// Message state (e.g., "processed", "pending")
    pub state: String,
}

impl From<message_types::Message> for Message {
    fn from(m: message_types::Message) -> Self {
        let nostr_group_id = m
            .event
            .tags
            .iter()
            .find(|t| t.kind() == TagKind::h())
            .and_then(|t| t.content())
            .unwrap_or_default()
            .to_string();

        let event_json = serde_json::to_string(&m.event).unwrap_or_else(|e| {
            tracing::error!(target: "mdk_uniffi::message", "Failed to serialize message event: {}", e);
            "{}".to_string()
        });

        Self {
            id: m.id.to_hex(),
            mls_group_id: hex::encode(m.mls_group_id.as_slice()),
            nostr_group_id,
            event_id: m.wrapper_event_id.to_hex(),
            sender_pubkey: m.pubkey.to_hex(),
            event_json,
            created_at: m.created_at.as_secs(),
            processed_at: m.processed_at.as_secs(),
            kind: m.kind.as_u16(),
            state: m.state.as_str().to_string(),
        }
    }
}

/// Welcome representation
#[derive(uniffi::Record)]
pub struct Welcome {
    /// Welcome ID (hex-encoded event ID)
    pub id: String,
    /// JSON representation of the welcome event
    pub event_json: String,
    /// Hex-encoded MLS group ID
    pub mls_group_id: String,
    /// Hex-encoded Nostr group ID
    pub nostr_group_id: String,
    /// Group name
    pub group_name: String,
    /// Group description
    pub group_description: String,
    /// Optional group image hash
    pub group_image_hash: Option<Vec<u8>>,
    /// Optional group image encryption key
    pub group_image_key: Option<Vec<u8>>,
    /// Optional group image encryption nonce
    pub group_image_nonce: Option<Vec<u8>>,
    /// List of admin public keys (hex-encoded)
    pub group_admin_pubkeys: Vec<String>,
    /// List of relay URLs for the group
    pub group_relays: Vec<String>,
    /// Welcomer public key (hex-encoded)
    pub welcomer: String,
    /// Current member count
    pub member_count: u32,
    /// Welcome state (e.g., "pending", "accepted", "declined")
    pub state: String,
    /// Wrapper event ID (hex-encoded)
    pub wrapper_event_id: String,
}

impl From<welcome_types::Welcome> for Welcome {
    fn from(w: welcome_types::Welcome) -> Self {
        let event_json = serde_json::to_string(&w.event).unwrap_or_else(|e| {
            tracing::error!(target: "mdk_uniffi::welcome", "Failed to serialize welcome event: {}", e);
            "{}".to_string()
        });

        Self {
            id: w.id.to_hex(),
            event_json,
            mls_group_id: hex::encode(w.mls_group_id.as_slice()),
            nostr_group_id: hex::encode(w.nostr_group_id),
            group_name: w.group_name.clone(),
            group_description: w.group_description.clone(),
            group_image_hash: w.group_image_hash.map(Into::into),
            group_image_key: w.group_image_key.map(|k| k.as_ref().to_vec()),
            group_image_nonce: w.group_image_nonce.map(|n| n.as_ref().to_vec()),
            group_admin_pubkeys: w.group_admin_pubkeys.iter().map(|pk| pk.to_hex()).collect(),
            group_relays: w.group_relays.iter().map(|r| r.to_string()).collect(),
            welcomer: w.welcomer.to_hex(),
            member_count: w.member_count,
            state: w.state.as_str().to_string(),
            wrapper_event_id: w.wrapper_event_id.to_hex(),
        }
    }
}

/// Prepared group image data ready for upload to Blossom
#[derive(uniffi::Record)]
pub struct GroupImageUpload {
    /// Encrypted image data (ready to upload to Blossom)
    pub encrypted_data: Vec<u8>,
    /// SHA256 hash of encrypted data (verify against Blossom response)
    pub encrypted_hash: Vec<u8>,
    /// Encryption key (store in extension)
    pub image_key: Vec<u8>,
    /// Encryption nonce (store in extension)
    pub image_nonce: Vec<u8>,
    /// Derived keypair secret for Blossom authentication (hex encoded)
    pub upload_secret_key: String,
    /// Original image size before encryption
    pub original_size: u64,
    /// Size after encryption
    pub encrypted_size: u64,
    /// Validated and canonical MIME type
    pub mime_type: String,
    /// Image dimensions (width, height) if available
    pub dimensions: Option<ImageDimensions>,
    /// Blurhash for preview if generated
    pub blurhash: Option<String>,
}

/// Image dimensions
#[derive(uniffi::Record)]
pub struct ImageDimensions {
    /// Width in pixels
    pub width: u32,
    /// Height in pixels
    pub height: u32,
}

/// Prepare group image for upload
#[uniffi::export]
pub fn prepare_group_image_for_upload(
    image_data: Vec<u8>,
    mime_type: String,
) -> Result<GroupImageUpload, MdkUniffiError> {
    let prepared = core_prepare_group_image_for_upload(&image_data, &mime_type)
        .map_err(|e| MdkUniffiError::Mdk(e.to_string()))?;

    Ok(GroupImageUpload {
        encrypted_data: prepared.encrypted_data.as_ref().clone(),
        encrypted_hash: prepared.encrypted_hash.to_vec(),
        image_key: prepared.image_key.as_ref().to_vec(),
        image_nonce: prepared.image_nonce.as_ref().to_vec(),
        upload_secret_key: prepared.upload_keypair.secret_key().to_secret_hex(),
        original_size: prepared.original_size as u64,
        encrypted_size: prepared.encrypted_size as u64,
        mime_type: prepared.mime_type.clone(),
        dimensions: prepared.dimensions.map(|(w, h)| ImageDimensions {
            width: w,
            height: h,
        }),
        blurhash: prepared.blurhash.clone(),
    })
}

/// Decrypt group image
#[uniffi::export]
pub fn decrypt_group_image(
    encrypted_data: Vec<u8>,
    expected_hash: Option<Vec<u8>>,
    image_key: Vec<u8>,
    image_nonce: Vec<u8>,
) -> Result<Vec<u8>, MdkUniffiError> {
    let hash_arr_opt: Option<[u8; 32]> = expected_hash
        .map(|hash| {
            hash.try_into().map_err(|_| {
                MdkUniffiError::InvalidInput("Expected hash must be 32 bytes".to_string())
            })
        })
        .transpose()?;

    let key_arr: [u8; 32] = image_key
        .try_into()
        .map_err(|_| MdkUniffiError::InvalidInput("Image key must be 32 bytes".to_string()))?;

    let nonce_arr: [u8; 12] = image_nonce
        .try_into()
        .map_err(|_| MdkUniffiError::InvalidInput("Image nonce must be 12 bytes".to_string()))?;

    core_decrypt_group_image(
        &encrypted_data,
        hash_arr_opt.as_ref(),
        &mdk_storage_traits::Secret::new(key_arr),
        &mdk_storage_traits::Secret::new(nonce_arr),
    )
    .map_err(|e| MdkUniffiError::Mdk(e.to_string()))
}

/// Derive upload keypair for group image
#[uniffi::export]
pub fn derive_upload_keypair(image_key: Vec<u8>, version: u16) -> Result<String, MdkUniffiError> {
    let key_arr: [u8; 32] = image_key
        .try_into()
        .map_err(|_| MdkUniffiError::InvalidInput("Image key must be 32 bytes".to_string()))?;

    let keys = core_derive_upload_keypair(&mdk_storage_traits::Secret::new(key_arr), version)
        .map_err(|e| MdkUniffiError::Mdk(e.to_string()))?;

    Ok(keys.secret_key().to_secret_hex())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostr::{EventBuilder, JsonUtil, Keys, Kind, Tag, UnsignedEvent};
    use tempfile::TempDir;

    fn create_test_mdk() -> Mdk {
        new_mdk_unencrypted(":memory:".to_string(), None).unwrap()
    }

    #[test]
    fn test_new_mdk_with_key_creates_instance() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");

        // Test encrypted constructor with direct key
        let key = vec![0u8; 32];
        let result = new_mdk_with_key(db_path.to_string_lossy().to_string(), key, None);
        assert!(result.is_ok());
        let mdk = result.unwrap();
        // Should be able to get groups (empty initially)
        let groups = mdk.get_groups().unwrap();
        assert_eq!(groups.len(), 0);
    }

    #[test]
    fn test_new_mdk_with_key_invalid_key_length() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test_invalid_key.db");

        // Test with wrong key length
        let short_key = vec![0u8; 16];
        let result = new_mdk_with_key(db_path.to_string_lossy().to_string(), short_key, None);
        assert!(result.is_err());

        match result {
            Err(MdkUniffiError::InvalidInput(msg)) => {
                assert!(msg.contains("Invalid encryption key"));
            }
            _ => panic!("Expected InvalidInput error"),
        }
    }

    #[test]
    fn test_new_mdk_unencrypted_creates_instance() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test_unencrypted.db");
        let result = new_mdk_unencrypted(db_path.to_string_lossy().to_string(), None);
        assert!(result.is_ok());
        let mdk = result.unwrap();
        // Should be able to get groups (empty initially)
        let groups = mdk.get_groups().unwrap();
        assert_eq!(groups.len(), 0);
    }

    #[test]
    fn test_new_mdk_with_custom_config() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test_custom_config.db");

        // Test with all fields specified
        let config = MdkConfig {
            max_event_age_secs: Some(86400),     // 1 day
            max_future_skew_secs: Some(60),      // 1 minute
            out_of_order_tolerance: Some(50),    // 50 past messages
            maximum_forward_distance: Some(500), // 500 forward messages
            epoch_snapshot_retention: Some(5),   // 5 snapshots
            snapshot_ttl_seconds: Some(604800),  // 1 week
        };

        let result = new_mdk_unencrypted(db_path.to_string_lossy().to_string(), Some(config));
        assert!(result.is_ok());
        let mdk = result.unwrap();
        let groups = mdk.get_groups().unwrap();
        assert_eq!(groups.len(), 0);
    }

    #[test]
    fn test_new_mdk_with_partial_config() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test_partial_config.db");

        // Test with only some fields specified - others should use defaults
        let config = MdkConfig {
            max_event_age_secs: None,
            max_future_skew_secs: None,
            out_of_order_tolerance: Some(200), // Only override this one
            maximum_forward_distance: None,
            epoch_snapshot_retention: None,
            snapshot_ttl_seconds: None,
        };

        let result = new_mdk_unencrypted(db_path.to_string_lossy().to_string(), Some(config));
        assert!(result.is_ok());
        let mdk = result.unwrap();
        let groups = mdk.get_groups().unwrap();
        assert_eq!(groups.len(), 0);
    }

    #[test]
    fn test_mdk_config_defaults() {
        // Verify that the From implementation uses correct defaults
        let config = MdkConfig {
            max_event_age_secs: None,
            max_future_skew_secs: None,
            out_of_order_tolerance: None,
            maximum_forward_distance: None,
            epoch_snapshot_retention: None,
            snapshot_ttl_seconds: None,
        };

        let core_config: CoreMdkConfig = config.into();
        assert_eq!(core_config.max_event_age_secs, 3888000);
        assert_eq!(core_config.max_future_skew_secs, 300);
        assert_eq!(core_config.out_of_order_tolerance, 100);
        assert_eq!(core_config.maximum_forward_distance, 1000);
        assert_eq!(core_config.epoch_snapshot_retention, 5);
        assert_eq!(core_config.snapshot_ttl_seconds, 604800);
    }

    #[test]
    fn test_create_key_package_for_event() {
        let mdk = create_test_mdk();
        let keys = Keys::generate();
        let pubkey_hex = keys.public_key().to_hex();
        let relays = vec!["wss://relay.example.com".to_string()];

        let result = mdk.create_key_package_for_event(pubkey_hex, relays);
        assert!(result.is_ok());
        let key_package_result = result.unwrap();
        assert!(!key_package_result.key_package.is_empty());
        assert!(!key_package_result.tags.is_empty());
        assert!(
            !key_package_result.hash_ref.is_empty(),
            "hash_ref should be non-empty"
        );
    }

    #[test]
    fn test_create_key_package_invalid_public_key() {
        let mdk = create_test_mdk();
        let invalid_pubkey = "not_a_valid_hex".to_string();
        let relays = vec!["wss://relay.example.com".to_string()];

        let result = mdk.create_key_package_for_event(invalid_pubkey, relays);
        assert!(matches!(result, Err(MdkUniffiError::InvalidInput(_))));
    }

    #[test]
    fn test_create_key_package_invalid_relay() {
        let mdk = create_test_mdk();
        let keys = Keys::generate();
        let pubkey_hex = keys.public_key().to_hex();
        let invalid_relays = vec!["not_a_valid_url".to_string()];

        let result = mdk.create_key_package_for_event(pubkey_hex, invalid_relays);
        assert!(matches!(result, Err(MdkUniffiError::InvalidInput(_))));
    }

    #[test]
    fn test_get_groups_empty_initially() {
        let mdk = create_test_mdk();
        let groups = mdk.get_groups().unwrap();
        assert_eq!(groups.len(), 0);
    }

    #[test]
    fn test_get_group_nonexistent() {
        let mdk = create_test_mdk();
        let fake_group_id = hex::encode([0u8; 32]);
        let result = mdk.get_group(fake_group_id);
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[test]
    fn test_get_group_invalid_hex() {
        let mdk = create_test_mdk();
        let invalid_group_id = "not_valid_hex".to_string();
        let result = mdk.get_group(invalid_group_id);
        assert!(matches!(result, Err(MdkUniffiError::InvalidInput(_))));
    }

    #[test]
    fn test_get_members_nonexistent_group() {
        let mdk = create_test_mdk();
        let fake_group_id = hex::encode([0u8; 32]);
        let result = mdk.get_members(fake_group_id);
        // Should return error for non-existent group
        assert!(result.is_err());
    }

    #[test]
    fn test_get_messages_empty_group() {
        let mdk = create_test_mdk();
        let fake_group_id = hex::encode([0u8; 32]);
        let result = mdk.get_messages(fake_group_id, None, None, None);
        // Should return error for non-existent group
        assert!(result.is_err());
    }

    #[test]
    fn test_get_messages_with_pagination() {
        let mdk = create_test_mdk();
        let creator_keys = Keys::generate();
        let member_keys = Keys::generate();

        let member_pubkey_hex = member_keys.public_key().to_hex();
        let relays = vec!["wss://relay.example.com".to_string()];

        // Create key package for member
        let kp_result = mdk
            .create_key_package_for_event(member_pubkey_hex.clone(), relays.clone())
            .unwrap();

        let kp_event = EventBuilder::new(Kind::Custom(443), kp_result.key_package)
            .tags(
                kp_result
                    .tags
                    .into_iter()
                    .map(|t| Tag::parse(&t).unwrap())
                    .collect::<Vec<_>>(),
            )
            .sign_with_keys(&member_keys)
            .unwrap();

        // Create group
        let create_result = mdk
            .create_group(
                creator_keys.public_key().to_hex(),
                vec![kp_event.as_json()],
                "Test Group".to_string(),
                "Test Description".to_string(),
                relays.clone(),
                vec![creator_keys.public_key().to_hex()],
            )
            .unwrap();

        mdk.merge_pending_commit(create_result.group.mls_group_id.clone())
            .unwrap();

        // Create a message
        mdk.create_message(
            create_result.group.mls_group_id.clone(),
            creator_keys.public_key().to_hex(),
            "Test message".to_string(),
            1,
            None,
        )
        .unwrap();

        // Test 1: Get with default pagination (None, None)
        let default_messages = mdk
            .get_messages(create_result.group.mls_group_id.clone(), None, None, None)
            .unwrap();
        assert_eq!(default_messages.len(), 1, "Should have 1 message");

        // Test 2: Get with explicit limit and offset
        let paginated = mdk
            .get_messages(
                create_result.group.mls_group_id.clone(),
                Some(10),
                Some(0),
                None,
            )
            .unwrap();
        assert_eq!(paginated.len(), 1, "Should have 1 message with pagination");

        // Test 3: Get with offset beyond available messages
        let empty_page = mdk
            .get_messages(
                create_result.group.mls_group_id.clone(),
                Some(10),
                Some(100),
                None,
            )
            .unwrap();
        assert_eq!(
            empty_page.len(),
            0,
            "Should return empty when offset is beyond available"
        );

        // Test 4: Get with limit 1
        let limited = mdk
            .get_messages(
                create_result.group.mls_group_id.clone(),
                Some(1),
                Some(0),
                None,
            )
            .unwrap();
        assert_eq!(
            limited.len(),
            1,
            "Should return exactly 1 message with limit 1"
        );
    }

    #[test]
    fn test_get_message_invalid_event_id() {
        let mdk = create_test_mdk();
        let fake_group_id = hex::encode([0u8; 32]);
        let invalid_event_id = "not_valid_hex".to_string();
        let result = mdk.get_message(fake_group_id, invalid_event_id);
        assert!(matches!(result, Err(MdkUniffiError::InvalidInput(_))));
    }

    #[test]
    fn test_get_pending_welcomes_empty() {
        let mdk = create_test_mdk();
        let welcomes = mdk.get_pending_welcomes(None, None).unwrap();
        assert_eq!(welcomes.len(), 0);
    }

    #[test]
    fn test_get_pending_welcomes_with_pagination() {
        let mdk = create_test_mdk();
        let creator_keys = Keys::generate();
        let member_keys = Keys::generate();

        let member_pubkey_hex = member_keys.public_key().to_hex();
        let relays = vec!["wss://relay.example.com".to_string()];

        // Create key package for member
        let kp_result = mdk
            .create_key_package_for_event(member_pubkey_hex.clone(), relays.clone())
            .unwrap();

        let kp_event = EventBuilder::new(Kind::Custom(443), kp_result.key_package)
            .tags(
                kp_result
                    .tags
                    .into_iter()
                    .map(|t| Tag::parse(&t).unwrap())
                    .collect::<Vec<_>>(),
            )
            .sign_with_keys(&member_keys)
            .unwrap();

        // Create group
        let create_result = mdk
            .create_group(
                creator_keys.public_key().to_hex(),
                vec![kp_event.as_json()],
                "Test Group".to_string(),
                "Test Description".to_string(),
                relays.clone(),
                vec![creator_keys.public_key().to_hex()],
            )
            .unwrap();

        mdk.merge_pending_commit(create_result.group.mls_group_id.clone())
            .unwrap();

        // Process welcome for member
        let welcome_rumor_json = &create_result.welcome_rumors_json[0];
        let wrapper_event_id = EventId::all_zeros().to_hex();
        mdk.process_welcome(wrapper_event_id, welcome_rumor_json.clone())
            .unwrap();

        // Test 1: Get with default pagination (None, None)
        let default_welcomes = mdk.get_pending_welcomes(None, None).unwrap();
        assert_eq!(default_welcomes.len(), 1, "Should have 1 pending welcome");

        // Test 2: Get with explicit limit and offset
        let paginated = mdk.get_pending_welcomes(Some(10), Some(0)).unwrap();
        assert_eq!(paginated.len(), 1, "Should have 1 welcome with pagination");

        // Test 3: Get with offset beyond available welcomes
        let empty_page = mdk.get_pending_welcomes(Some(10), Some(100)).unwrap();
        assert_eq!(
            empty_page.len(),
            0,
            "Should return empty when offset is beyond available"
        );

        // Test 4: Get with limit 1
        let limited = mdk.get_pending_welcomes(Some(1), Some(0)).unwrap();
        assert_eq!(
            limited.len(),
            1,
            "Should return exactly 1 welcome with limit 1"
        );
    }

    #[test]
    fn test_accept_welcome_with_object() {
        let mdk = create_test_mdk();
        let creator_keys = Keys::generate();
        let member_keys = Keys::generate();

        let member_pubkey_hex = member_keys.public_key().to_hex();
        let relays = vec!["wss://relay.example.com".to_string()];
        let key_package_result = mdk
            .create_key_package_for_event(member_pubkey_hex.clone(), relays.clone())
            .unwrap();

        let key_package_event =
            EventBuilder::new(Kind::MlsKeyPackage, key_package_result.key_package)
                .tags(
                    key_package_result
                        .tags
                        .iter()
                        .map(|t| Tag::parse(t.clone()).unwrap())
                        .collect::<Vec<_>>(),
                )
                .sign_with_keys(&member_keys)
                .unwrap();

        let key_package_event_json = serde_json::to_string(&key_package_event).unwrap();

        let creator_pubkey_hex = creator_keys.public_key().to_hex();
        let create_result = mdk
            .create_group(
                creator_pubkey_hex,
                vec![key_package_event_json],
                "Test Group".to_string(),
                "Test Description".to_string(),
                relays.clone(),
                vec![creator_keys.public_key().to_hex()],
            )
            .unwrap();

        // Get the welcome rumor from the create result
        let welcome_rumor_json = create_result.welcome_rumors_json.first().unwrap();

        // Process the welcome to get a Welcome object
        let wrapper_event_id = EventId::all_zeros();
        let welcome = mdk
            .process_welcome(wrapper_event_id.to_hex(), welcome_rumor_json.clone())
            .unwrap();

        // Verify welcome is pending
        assert_eq!(welcome.state, "pending");

        // Accept the welcome using the new method that takes a Welcome object
        let result = mdk.accept_welcome(welcome);
        assert!(result.is_ok());

        // Verify the welcome was accepted by checking pending welcomes
        let pending_welcomes = mdk.get_pending_welcomes(None, None).unwrap();
        assert_eq!(pending_welcomes.len(), 0);
    }

    #[test]
    fn test_decline_welcome_with_object() {
        let mdk = create_test_mdk();
        let creator_keys = Keys::generate();
        let member_keys = Keys::generate();

        let member_pubkey_hex = member_keys.public_key().to_hex();
        let relays = vec!["wss://relay.example.com".to_string()];
        let key_package_result = mdk
            .create_key_package_for_event(member_pubkey_hex.clone(), relays.clone())
            .unwrap();

        let key_package_event =
            EventBuilder::new(Kind::MlsKeyPackage, key_package_result.key_package)
                .tags(
                    key_package_result
                        .tags
                        .iter()
                        .map(|t| Tag::parse(t.clone()).unwrap())
                        .collect::<Vec<_>>(),
                )
                .sign_with_keys(&member_keys)
                .unwrap();

        let key_package_event_json = serde_json::to_string(&key_package_event).unwrap();

        let creator_pubkey_hex = creator_keys.public_key().to_hex();
        let create_result = mdk
            .create_group(
                creator_pubkey_hex,
                vec![key_package_event_json],
                "Test Group".to_string(),
                "Test Description".to_string(),
                relays.clone(),
                vec![creator_keys.public_key().to_hex()],
            )
            .unwrap();

        // Get the welcome rumor from the create result
        let welcome_rumor_json = create_result.welcome_rumors_json.first().unwrap();

        // Process the welcome to get a Welcome object
        let wrapper_event_id = EventId::all_zeros();
        let welcome = mdk
            .process_welcome(wrapper_event_id.to_hex(), welcome_rumor_json.clone())
            .unwrap();

        // Verify welcome is pending
        assert_eq!(welcome.state, "pending");

        // Decline the welcome using the new method that takes a Welcome object
        let result = mdk.decline_welcome(welcome);
        assert!(result.is_ok());

        // Verify the welcome was declined by checking pending welcomes
        let pending_welcomes = mdk.get_pending_welcomes(None, None).unwrap();
        assert_eq!(pending_welcomes.len(), 0);
    }

    #[test]
    fn test_accept_welcome_invalid_event_id() {
        let mdk = create_test_mdk();
        let welcome = Welcome {
            id: "invalid_hex".to_string(),
            event_json: "{}".to_string(),
            mls_group_id: hex::encode([0u8; 32]),
            nostr_group_id: hex::encode([0u8; 32]),
            group_name: "Test".to_string(),
            group_description: "Test".to_string(),
            group_image_hash: None,
            group_image_key: None,
            group_image_nonce: None,
            group_admin_pubkeys: vec![],
            group_relays: vec![],
            welcomer: "invalid_hex".to_string(),
            member_count: 0,
            state: "pending".to_string(),
            wrapper_event_id: hex::encode([0u8; 32]),
        };

        let result = mdk.accept_welcome(welcome);
        assert!(matches!(result, Err(MdkUniffiError::InvalidInput(_))));
    }

    #[test]
    fn test_accept_welcome_invalid_event_json() {
        let mdk = create_test_mdk();
        let keys = Keys::generate();
        let event_id = EventId::all_zeros();
        let welcome = Welcome {
            id: event_id.to_hex(),
            event_json: "invalid_json".to_string(),
            mls_group_id: hex::encode([0u8; 32]),
            nostr_group_id: hex::encode([0u8; 32]),
            group_name: "Test".to_string(),
            group_description: "Test".to_string(),
            group_image_hash: None,
            group_image_key: None,
            group_image_nonce: None,
            group_admin_pubkeys: vec![keys.public_key().to_hex()],
            group_relays: vec!["wss://relay.example.com".to_string()],
            welcomer: keys.public_key().to_hex(),
            member_count: 0,
            state: "pending".to_string(),
            wrapper_event_id: event_id.to_hex(),
        };

        let result = mdk.accept_welcome(welcome);
        assert!(matches!(result, Err(MdkUniffiError::InvalidInput(_))));
    }

    #[test]
    fn test_accept_welcome_invalid_nostr_group_id() {
        let mdk = create_test_mdk();
        let keys = Keys::generate();
        let event_id = EventId::all_zeros();
        let event = UnsignedEvent {
            id: Some(event_id),
            pubkey: keys.public_key(),
            created_at: nostr::Timestamp::now(),
            kind: Kind::Custom(444),
            tags: nostr::Tags::new(),
            content: "test".to_string(),
        };
        let event_json = serde_json::to_string(&event).unwrap();

        let welcome = Welcome {
            id: event_id.to_hex(),
            event_json,
            mls_group_id: hex::encode([0u8; 32]),
            nostr_group_id: "invalid_hex".to_string(),
            group_name: "Test".to_string(),
            group_description: "Test".to_string(),
            group_image_hash: None,
            group_image_key: None,
            group_image_nonce: None,
            group_admin_pubkeys: vec![keys.public_key().to_hex()],
            group_relays: vec!["wss://relay.example.com".to_string()],
            welcomer: keys.public_key().to_hex(),
            member_count: 0,
            state: "pending".to_string(),
            wrapper_event_id: event_id.to_hex(),
        };

        let result = mdk.accept_welcome(welcome);
        assert!(matches!(result, Err(MdkUniffiError::InvalidInput(_))));
    }

    #[test]
    fn test_accept_welcome_invalid_state() {
        let mdk = create_test_mdk();
        let keys = Keys::generate();
        let event_id = EventId::all_zeros();
        let event = UnsignedEvent {
            id: Some(event_id),
            pubkey: keys.public_key(),
            created_at: nostr::Timestamp::now(),
            kind: Kind::Custom(444),
            tags: nostr::Tags::new(),
            content: "test".to_string(),
        };
        let event_json = serde_json::to_string(&event).unwrap();

        let welcome = Welcome {
            id: event_id.to_hex(),
            event_json,
            mls_group_id: hex::encode([0u8; 32]),
            nostr_group_id: hex::encode([0u8; 32]),
            group_name: "Test".to_string(),
            group_description: "Test".to_string(),
            group_image_hash: None,
            group_image_key: None,
            group_image_nonce: None,
            group_admin_pubkeys: vec![keys.public_key().to_hex()],
            group_relays: vec!["wss://relay.example.com".to_string()],
            welcomer: keys.public_key().to_hex(),
            member_count: 0,
            state: "invalid_state".to_string(),
            wrapper_event_id: event_id.to_hex(),
        };

        let result = mdk.accept_welcome(welcome);
        assert!(matches!(result, Err(MdkUniffiError::InvalidInput(_))));
    }

    #[test]
    fn test_accept_welcome_invalid_image_hash_size() {
        let mdk = create_test_mdk();
        let keys = Keys::generate();
        let event_id = EventId::all_zeros();
        let event = UnsignedEvent {
            id: Some(event_id),
            pubkey: keys.public_key(),
            created_at: nostr::Timestamp::now(),
            kind: Kind::Custom(444),
            tags: nostr::Tags::new(),
            content: "test".to_string(),
        };
        let event_json = serde_json::to_string(&event).unwrap();

        let welcome = Welcome {
            id: event_id.to_hex(),
            event_json,
            mls_group_id: hex::encode([0u8; 32]),
            nostr_group_id: hex::encode([0u8; 32]),
            group_name: "Test".to_string(),
            group_description: "Test".to_string(),
            group_image_hash: Some(vec![0u8; 31]), // Wrong size
            group_image_key: None,
            group_image_nonce: None,
            group_admin_pubkeys: vec![keys.public_key().to_hex()],
            group_relays: vec!["wss://relay.example.com".to_string()],
            welcomer: keys.public_key().to_hex(),
            member_count: 0,
            state: "pending".to_string(),
            wrapper_event_id: event_id.to_hex(),
        };

        let result = mdk.accept_welcome(welcome);
        assert!(matches!(result, Err(MdkUniffiError::InvalidInput(_))));
    }

    #[test]
    fn test_decline_welcome_invalid_event_id() {
        let mdk = create_test_mdk();
        let welcome = Welcome {
            id: "invalid_hex".to_string(),
            event_json: "{}".to_string(),
            mls_group_id: hex::encode([0u8; 32]),
            nostr_group_id: hex::encode([0u8; 32]),
            group_name: "Test".to_string(),
            group_description: "Test".to_string(),
            group_image_hash: None,
            group_image_key: None,
            group_image_nonce: None,
            group_admin_pubkeys: vec![],
            group_relays: vec![],
            welcomer: "invalid_hex".to_string(),
            member_count: 0,
            state: "pending".to_string(),
            wrapper_event_id: hex::encode([0u8; 32]),
        };

        let result = mdk.decline_welcome(welcome);
        assert!(matches!(result, Err(MdkUniffiError::InvalidInput(_))));
    }

    #[test]
    fn test_get_relays_nonexistent_group() {
        let mdk = create_test_mdk();
        let fake_group_id = hex::encode([0u8; 32]);
        let result = mdk.get_relays(fake_group_id);
        // Should return error for non-existent group
        assert!(result.is_err());
    }

    #[test]
    fn test_create_group_basic() {
        let mdk = create_test_mdk();
        let creator_keys = Keys::generate();
        let member_keys = Keys::generate();

        let member_pubkey_hex = member_keys.public_key().to_hex();
        let relays = vec!["wss://relay.example.com".to_string()];
        let key_package_result = mdk
            .create_key_package_for_event(member_pubkey_hex.clone(), relays.clone())
            .unwrap();

        let key_package_event =
            EventBuilder::new(Kind::MlsKeyPackage, key_package_result.key_package)
                .tags(
                    key_package_result
                        .tags
                        .iter()
                        .map(|t| Tag::parse(t.clone()).unwrap())
                        .collect::<Vec<_>>(),
                )
                .sign_with_keys(&member_keys)
                .unwrap();

        let key_package_event_json = serde_json::to_string(&key_package_event).unwrap();

        let creator_pubkey_hex = creator_keys.public_key().to_hex();
        let result = mdk.create_group(
            creator_pubkey_hex,
            vec![key_package_event_json],
            "Test Group".to_string(),
            "Test Description".to_string(),
            relays,
            vec![creator_keys.public_key().to_hex()],
        );

        assert!(result.is_ok());
        let create_result = result.unwrap();
        assert_eq!(create_result.group.name, "Test Group");
        assert_eq!(create_result.group.description, "Test Description");
        assert!(!create_result.welcome_rumors_json.is_empty());
    }

    #[test]
    fn test_create_group_invalid_creator_key() {
        let mdk = create_test_mdk();
        let invalid_pubkey = "not_valid_hex".to_string();
        let result = mdk.create_group(
            invalid_pubkey,
            vec![],
            "Test".to_string(),
            "Test".to_string(),
            vec!["wss://relay.example.com".to_string()],
            vec![],
        );
        assert!(matches!(result, Err(MdkUniffiError::InvalidInput(_))));
    }

    #[test]
    fn test_create_group_invalid_admin_key() {
        let mdk = create_test_mdk();
        let creator_keys = Keys::generate();
        let creator_pubkey_hex = creator_keys.public_key().to_hex();
        let result = mdk.create_group(
            creator_pubkey_hex,
            vec![],
            "Test".to_string(),
            "Test".to_string(),
            vec!["wss://relay.example.com".to_string()],
            vec!["not_valid_hex".to_string()],
        );
        assert!(matches!(result, Err(MdkUniffiError::InvalidInput(_))));
    }

    #[test]
    fn test_create_group_invalid_relay() {
        let mdk = create_test_mdk();
        let creator_keys = Keys::generate();
        let creator_pubkey_hex = creator_keys.public_key().to_hex();
        let result = mdk.create_group(
            creator_pubkey_hex,
            vec![],
            "Test".to_string(),
            "Test".to_string(),
            vec!["not_a_valid_url".to_string()],
            vec![],
        );
        assert!(matches!(result, Err(MdkUniffiError::InvalidInput(_))));
    }

    #[test]
    fn test_create_group_invalid_key_package_json() {
        let mdk = create_test_mdk();
        let creator_keys = Keys::generate();
        let creator_pubkey_hex = creator_keys.public_key().to_hex();
        let result = mdk.create_group(
            creator_pubkey_hex,
            vec!["not_valid_json".to_string()],
            "Test".to_string(),
            "Test".to_string(),
            vec!["wss://relay.example.com".to_string()],
            vec![],
        );
        assert!(matches!(result, Err(MdkUniffiError::InvalidInput(_))));
    }

    #[test]
    fn test_add_members_invalid_group_id() {
        let mdk = create_test_mdk();
        let invalid_group_id = "not_valid_hex".to_string();
        let result = mdk.add_members(invalid_group_id, vec![]);
        assert!(matches!(result, Err(MdkUniffiError::InvalidInput(_))));
    }

    #[test]
    fn test_remove_members_invalid_group_id() {
        let mdk = create_test_mdk();
        let invalid_group_id = "not_valid_hex".to_string();
        let result = mdk.remove_members(invalid_group_id, vec![]);
        assert!(matches!(result, Err(MdkUniffiError::InvalidInput(_))));
    }

    #[test]
    fn test_remove_members_invalid_public_key() {
        let mdk = create_test_mdk();
        let fake_group_id = hex::encode([0u8; 32]);
        let result = mdk.remove_members(fake_group_id, vec!["not_valid_hex".to_string()]);
        assert!(matches!(result, Err(MdkUniffiError::InvalidInput(_))));
    }

    #[test]
    fn test_create_message_invalid_group_id() {
        let mdk = create_test_mdk();
        let keys = Keys::generate();
        let invalid_group_id = "not_valid_hex".to_string();
        let result = mdk.create_message(
            invalid_group_id,
            keys.public_key().to_hex(),
            "Hello".to_string(),
            1,
            None,
        );
        assert!(matches!(result, Err(MdkUniffiError::InvalidInput(_))));
    }

    #[test]
    fn test_create_message_invalid_sender_key() {
        let mdk = create_test_mdk();
        let fake_group_id = hex::encode([0u8; 32]);
        let result = mdk.create_message(
            fake_group_id,
            "not_valid_hex".to_string(),
            "Hello".to_string(),
            1,
            None,
        );
        assert!(matches!(result, Err(MdkUniffiError::InvalidInput(_))));
    }

    #[test]
    fn test_process_message_invalid_json() {
        let mdk = create_test_mdk();
        let result = mdk.process_message("not_valid_json".to_string());
        assert!(matches!(result, Err(MdkUniffiError::InvalidInput(_))));
    }

    #[test]
    fn test_update_group_data_invalid_group_id() {
        let mdk = create_test_mdk();
        let invalid_group_id = "not_valid_hex".to_string();
        let update = GroupDataUpdate {
            name: Some("New Name".to_string()),
            description: None,
            image_hash: None,
            image_key: None,
            image_nonce: None,
            relays: None,
            admins: None,
        };
        let result = mdk.update_group_data(invalid_group_id, update);
        assert!(matches!(result, Err(MdkUniffiError::InvalidInput(_))));
    }

    #[test]
    fn test_update_group_data_invalid_relays() {
        let mdk = create_test_mdk();
        let fake_group_id = hex::encode([0u8; 32]);
        let update = GroupDataUpdate {
            name: None,
            description: None,
            image_hash: None,
            image_key: None,
            image_nonce: None,
            relays: Some(vec!["not_a_valid_url".to_string()]),
            admins: None,
        };
        let result = mdk.update_group_data(fake_group_id, update);
        assert!(matches!(result, Err(MdkUniffiError::InvalidInput(_))));
    }

    #[test]
    fn test_update_group_data_invalid_admin() {
        let mdk = create_test_mdk();
        let fake_group_id = hex::encode([0u8; 32]);
        let update = GroupDataUpdate {
            name: None,
            description: None,
            image_hash: None,
            image_key: None,
            image_nonce: None,
            relays: None,
            admins: Some(vec!["not_valid_hex".to_string()]),
        };
        let result = mdk.update_group_data(fake_group_id, update);
        assert!(matches!(result, Err(MdkUniffiError::InvalidInput(_))));
    }

    #[test]
    fn test_vec_to_array_image_key() {
        let vec = Some(vec![0u8; 32]);
        let result = vec_to_array::<32>(vec);
        assert!(result.is_ok());
        assert!(result.unwrap().is_some());
    }

    #[test]
    fn test_vec_to_array_image_nonce() {
        let vec = Some(vec![0u8; 12]);
        let result = vec_to_array::<12>(vec);
        assert!(result.is_ok());
        assert!(result.unwrap().is_some());
    }

    #[test]
    fn test_vec_to_array_wrong_size() {
        let vec = Some(vec![0u8; 31]); // Wrong size for 32-byte array
        let result = vec_to_array::<32>(vec);
        assert!(matches!(result, Err(MdkUniffiError::InvalidInput(_))));
    }

    #[test]
    fn test_vec_to_array_none() {
        let vec: Option<Vec<u8>> = None;
        let result = vec_to_array::<32>(vec);
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[test]
    fn test_error_conversion_storage() {
        use mdk_sqlite_storage::error::Error as StorageError;
        let storage_err = StorageError::Database("test error".to_string());
        let mdk_err: MdkUniffiError = storage_err.into();
        assert!(matches!(mdk_err, MdkUniffiError::Storage(_)));
    }

    #[test]
    fn test_parse_relay_urls_valid() {
        let valid_urls = vec![
            "wss://relay.example.com".to_string(),
            "wss://another.relay.com".to_string(),
        ];
        let result = parse_relay_urls(&valid_urls);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().len(), 2);
    }

    #[test]
    fn test_parse_relay_urls_invalid() {
        let invalid_urls = vec!["not_a_valid_url".to_string()];
        let result = parse_relay_urls(&invalid_urls);
        assert!(matches!(result, Err(MdkUniffiError::InvalidInput(_))));
    }

    #[test]
    fn test_parse_relay_urls_empty() {
        let empty_urls: Vec<String> = vec![];
        let result = parse_relay_urls(&empty_urls);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().len(), 0);
    }

    #[test]
    fn test_parse_tags_valid() {
        let keys = Keys::generate();
        let event_id =
            EventId::from_hex("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")
                .unwrap();
        let tags = vec![
            vec!["p".to_string(), keys.public_key().to_hex()],
            vec!["e".to_string(), event_id.to_hex()],
        ];
        let result = parse_tags(tags);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().len(), 2);
    }

    #[test]
    fn test_parse_tags_empty() {
        let tags: Vec<Vec<String>> = vec![];
        let result = parse_tags(tags);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().len(), 0);
    }
}
