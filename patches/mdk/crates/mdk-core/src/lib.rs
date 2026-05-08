//! A Rust implementation of the Nostr Message Layer Security (MLS) protocol
//!
//! This crate provides functionality for implementing secure group messaging in Nostr using the MLS protocol.
//! It handles group creation, member management, message encryption/decryption, key management, and storage of groups and messages.
//! The implementation follows the MLS specification while integrating with Nostr's event system.

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![warn(rustdoc::bare_urls)]
#![cfg_attr(docsrs, feature(doc_auto_cfg))]
#![doc = include_str!("../README.md")]

use std::sync::Arc;

use mdk_storage_traits::MdkStorageProvider;
use openmls::prelude::*;
use openmls_rust_crypto::RustCrypto;

pub mod callback;
mod constant;
#[cfg(feature = "mip04")]
#[cfg_attr(docsrs, doc(cfg(feature = "mip04")))]
pub mod encrypted_media;
pub mod epoch_snapshots;
pub mod error;
pub mod extension;
pub mod groups;
pub mod key_packages;
pub mod media_processing;
pub mod messages;
pub mod prelude;
#[cfg(test)]
pub mod test_util;
mod util;
pub mod welcomes;

use self::callback::{MdkCallback, RollbackInfo};
use self::constant::{
    DEFAULT_CIPHERSUITE, GROUP_CONTEXT_REQUIRED_EXTENSIONS, SUPPORTED_EXTENSIONS,
};
use self::epoch_snapshots::EpochSnapshotManager;
pub use self::error::Error;
use self::util::NostrTagFormat;

// Re-export GroupId for convenience
pub use mdk_storage_traits::GroupId;

/// Configuration for MDK behavior
///
/// This struct allows customization of various MDK parameters including
/// message validation settings. All fields have secure defaults.
///
/// # Examples
///
/// ```rust
/// use mdk_core::MdkConfig;
///
/// // Use defaults (recommended for most cases)
/// let config = MdkConfig::default();
///
/// // Custom configuration
/// let config = MdkConfig {
///     max_event_age_secs: 86400,  // 1 day instead of 45
///     out_of_order_tolerance: 50, // Stricter forward secrecy
///     ..Default::default()
/// };
/// ```
#[derive(Debug, Clone)]
pub struct MdkConfig {
    /// Maximum age for accepted events in seconds.
    ///
    /// Events older than this will be rejected during validation to prevent:
    /// - Replay attacks with old messages
    /// - Resource exhaustion from processing large message backlogs
    /// - Synchronization issues with stale group state
    ///
    /// Default: 3888000 (45 days)
    ///
    /// # Security Note
    /// This value balances security with usability for offline scenarios.
    /// The 45-day window accommodates extended offline periods while still
    /// providing protection against replay attacks. Applications with stricter
    /// security requirements may reduce this value.
    pub max_event_age_secs: u64,

    /// Maximum future timestamp skew allowed in seconds.
    ///
    /// Events with timestamps too far in the future will be rejected
    /// to prevent timestamp manipulation attacks. The default 5-minute
    /// window accounts for reasonable clock skew between clients.
    ///
    /// Default: 300 (5 minutes)
    pub max_future_skew_secs: u64,

    /// Number of past message decryption secrets to retain for out-of-order delivery.
    ///
    /// This controls how many past decryption secrets are kept to handle messages
    /// that arrive out of order. Nostr relays do not guarantee message ordering,
    /// so a higher value improves reliability when messages are reordered.
    ///
    /// Default: 100
    ///
    /// # Security Note
    /// Higher values reduce forward secrecy within an epoch, as more past secrets
    /// are retained in memory. The default of 100 balances reliability with security
    /// for typical Nostr relay behavior. Applications with stricter forward secrecy
    /// requirements may reduce this value.
    pub out_of_order_tolerance: u32,

    /// Maximum number of messages that can be skipped before decryption fails.
    ///
    /// This controls how far ahead the sender ratchet can advance when messages
    /// are dropped or lost. If more than this many messages are skipped,
    /// decryption will fail.
    ///
    /// Default: 1000
    ///
    /// # Security Note
    /// Higher values improve tolerance for dropped messages but require more
    /// computation to advance the ratchet when catching up. The default of 1000
    /// handles most message loss scenarios while keeping catch-up costs reasonable.
    pub maximum_forward_distance: u32,

    /// Number of epoch snapshots to retain for rollback support.
    ///
    /// Enables recovery when a better commit arrives late by allowing the
    /// client to rollback to a previous epoch state and re-apply commits.
    ///
    /// Default: 5
    pub epoch_snapshot_retention: usize,

    /// Time-to-live for snapshots in seconds.
    ///
    /// Snapshots older than this will be pruned on startup to prevent
    /// indefinite storage growth. This ensures that cryptographic key
    /// material in snapshots doesn't persist longer than necessary.
    ///
    /// Default: 604800 (1 week)
    pub snapshot_ttl_seconds: u64,
}

impl Default for MdkConfig {
    fn default() -> Self {
        Self {
            max_event_age_secs: 3888000,    // 45 days
            max_future_skew_secs: 300,      // 5 minutes
            out_of_order_tolerance: 100,    // 100 past messages
            maximum_forward_distance: 1000, // 1000 forward messages
            epoch_snapshot_retention: 5,
            snapshot_ttl_seconds: 604800, // 1 week
        }
    }
}

impl MdkConfig {
    /// Create a new configuration with default settings
    pub fn new() -> Self {
        Self::default()
    }
}

/// Builder for constructing MDK instances
///
/// This builder provides a fluent API for configuring and creating MDK instances.
/// It follows the builder pattern commonly used in Rust libraries.
///
/// # Examples
///
/// ```no_run
/// use mdk_core::{MDK, MdkConfig};
/// use mdk_memory_storage::MdkMemoryStorage;
///
/// // Simple usage with defaults
/// let mdk = MDK::new(MdkMemoryStorage::default());
///
/// // With custom configuration
/// let mdk = MDK::builder(MdkMemoryStorage::default())
///     .with_config(MdkConfig::new())
///     .build();
/// ```
#[derive(Debug)]
pub struct MdkBuilder<Storage> {
    storage: Storage,
    config: MdkConfig,
    callback: Option<Arc<dyn MdkCallback>>,
}

impl<Storage> MdkBuilder<Storage>
where
    Storage: MdkStorageProvider,
{
    /// Create a new MDK builder with the given storage
    pub fn new(storage: Storage) -> Self {
        Self {
            storage,
            config: MdkConfig::default(),
            callback: None,
        }
    }

    /// Set a custom configuration
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use mdk_core::{MDK, MdkConfig};
    /// # use mdk_memory_storage::MdkMemoryStorage;
    /// let config = MdkConfig::new();
    /// let mdk = MDK::builder(MdkMemoryStorage::default())
    ///     .with_config(config)
    ///     .build();
    /// ```
    pub fn with_config(mut self, config: MdkConfig) -> Self {
        self.config = config;
        self
    }

    /// Set a callback for MDK events
    pub fn with_callback(mut self, callback: Arc<dyn MdkCallback>) -> Self {
        self.callback = Some(callback);
        self
    }

    /// Build the MDK instance with the configured settings
    pub fn build(self) -> MDK<Storage> {
        // Prune expired snapshots on startup for persistent backends
        if self.storage.backend().is_persistent() {
            let current_time = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("System time before Unix epoch")
                .as_secs();
            let min_timestamp = current_time.saturating_sub(self.config.snapshot_ttl_seconds);
            if let Ok(pruned_count) = self.storage.prune_expired_snapshots(min_timestamp)
                && pruned_count > 0
            {
                tracing::info!(
                    pruned = pruned_count,
                    ttl_seconds = self.config.snapshot_ttl_seconds,
                    "Pruned expired snapshots on startup"
                );
            }
        }

        let epoch_snapshots = Arc::new(EpochSnapshotManager::new(
            self.config.epoch_snapshot_retention,
        ));

        MDK {
            ciphersuite: DEFAULT_CIPHERSUITE,
            extensions: SUPPORTED_EXTENSIONS.to_vec(),
            provider: MdkProvider {
                crypto: RustCrypto::default(),
                storage: self.storage,
            },
            config: self.config,
            epoch_snapshots,
            callback: self.callback,
        }
    }
}

/// The main struct for the MDK implementation.
///
/// This struct provides the core functionality for MLS operations in the Marmot protocol:
/// - Group management (creation, updates, member management)
/// - Message handling (encryption, decryption, processing)
/// - Key management (key packages, welcome messages)
///
/// It uses a generic storage provider that implements the `MdkStorageProvider` trait,
/// allowing for flexible storage backends.
#[derive(Debug)]
pub struct MDK<Storage>
where
    Storage: MdkStorageProvider,
{
    /// The MLS ciphersuite used for cryptographic operations
    pub ciphersuite: Ciphersuite,
    /// Required MLS extensions for Nostr functionality
    pub extensions: Vec<ExtensionType>,
    /// The OpenMLS provider implementation for cryptographic and storage operations
    pub provider: MdkProvider<Storage>,
    /// Configuration for encoding behavior
    pub config: MdkConfig,
    /// Snapshot manager for rollback support
    epoch_snapshots: Arc<EpochSnapshotManager>,
    /// Optional callback for events
    callback: Option<Arc<dyn MdkCallback>>,
}

/// Provider implementation for OpenMLS that integrates with Nostr.
///
/// This struct implements the OpenMLS Provider trait, providing:
/// - Cryptographic operations through RustCrypto
/// - Storage operations through the generic Storage type
/// - Random number generation through RustCrypto
#[derive(Debug)]
pub struct MdkProvider<Storage>
where
    Storage: MdkStorageProvider,
{
    crypto: RustCrypto,
    storage: Storage,
}

impl<Storage> OpenMlsProvider for MdkProvider<Storage>
where
    Storage: MdkStorageProvider,
{
    type CryptoProvider = RustCrypto;
    type RandProvider = RustCrypto;
    type StorageProvider = Storage;

    fn storage(&self) -> &Self::StorageProvider {
        &self.storage
    }

    fn crypto(&self) -> &Self::CryptoProvider {
        &self.crypto
    }

    fn rand(&self) -> &Self::RandProvider {
        &self.crypto
    }
}

impl<Storage> MDK<Storage>
where
    Storage: MdkStorageProvider,
{
    /// Create a builder for constructing an MDK instance
    ///
    /// This is the recommended way to create MDK instances when you need
    /// custom configuration.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use mdk_core::MDK;
    /// # use mdk_memory_storage::MdkMemoryStorage;
    /// let mdk = MDK::builder(MdkMemoryStorage::default()).build();
    /// ```
    pub fn builder(storage: Storage) -> MdkBuilder<Storage> {
        MdkBuilder::new(storage)
    }

    /// Construct a new MDK instance with default configuration
    ///
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use mdk_core::MDK;
    /// # use mdk_memory_storage::MdkMemoryStorage;
    /// # use mdk_core::MdkConfig;
    /// let mdk = MDK::new(MdkMemoryStorage::default());
    ///
    /// let mdk = MDK::builder(MdkMemoryStorage::default())
    ///     .with_config(MdkConfig::new())
    ///     .build();
    /// ```
    pub fn new(storage: Storage) -> Self {
        Self::builder(storage).build()
    }

    /// Get nostr MLS capabilities with GREASE values for extensibility testing.
    ///
    /// GREASE (Generate Random Extensions And Sustain Extensibility) values are
    /// automatically injected into capabilities as per RFC 9420 Section 13.5.
    /// This ensures implementations correctly handle unknown values and maintains
    /// protocol extensibility.
    #[inline]
    pub(crate) fn capabilities(&self) -> Capabilities {
        Capabilities::new(
            None,
            Some(&[self.ciphersuite]),
            Some(&self.extensions),
            None,
            None,
        )
        .with_grease(&self.provider.crypto)
    }

    /// Get the group's required capabilities extension
    #[inline]
    pub(crate) fn required_capabilities_extension(&self) -> Extension {
        Extension::RequiredCapabilities(RequiredCapabilitiesExtension::new(
            &GROUP_CONTEXT_REQUIRED_EXTENSIONS,
            &[],
            &[],
        ))
    }

    /// Get the ciphersuite value formatted for Nostr tags (hex with 0x prefix)
    pub(crate) fn ciphersuite_value(&self) -> String {
        self.ciphersuite.to_nostr_tag()
    }

    /// Get the extensions value formatted for Nostr tags (array of hex values)
    pub(crate) fn extensions_value(&self) -> Vec<String> {
        self.extensions.iter().map(|e| e.to_nostr_tag()).collect()
    }

    /// Get the storage provider
    pub(crate) fn storage(&self) -> &Storage {
        &self.provider.storage
    }
}

/// Tests module for mdk-core
#[cfg(test)]
pub mod tests {
    use mdk_memory_storage::MdkMemoryStorage;

    use super::*;

    /// Create a test MDK instance with an in-memory storage provider
    pub fn create_test_mdk() -> MDK<MdkMemoryStorage> {
        MDK::new(MdkMemoryStorage::default())
    }

    /// Create a test MDK instance with custom configuration
    pub fn create_test_mdk_with_config(config: MdkConfig) -> MDK<MdkMemoryStorage> {
        MDK::builder(MdkMemoryStorage::default())
            .with_config(config)
            .build()
    }

    /// Tests for GREASE (Generate Random Extensions And Sustain Extensibility) support.
    /// GREASE values ensure implementations correctly handle unknown values per RFC 9420 Section 13.5.
    mod grease_tests {
        use openmls_traits::types::VerifiableCiphersuite;

        use super::*;

        #[test]
        fn test_capabilities_include_grease_ciphersuites() {
            let mdk = create_test_mdk();
            let caps = mdk.capabilities();

            // Verify at least one GREASE value is present in ciphersuites
            let has_grease_ciphersuite = caps.ciphersuites().iter().any(|cs| cs.is_grease());

            assert!(
                has_grease_ciphersuite,
                "Capabilities should include at least one GREASE ciphersuite"
            );
        }

        #[test]
        fn test_capabilities_include_grease_extensions() {
            let mdk = create_test_mdk();
            let caps = mdk.capabilities();

            // Verify at least one GREASE value is present in extensions
            let has_grease_extension = caps.extensions().iter().any(|ext| ext.is_grease());

            assert!(
                has_grease_extension,
                "Capabilities should include at least one GREASE extension"
            );
        }

        #[test]
        fn test_capabilities_include_grease_proposals() {
            let mdk = create_test_mdk();
            let caps = mdk.capabilities();

            // Verify at least one GREASE value is present in proposals
            let has_grease_proposal = caps.proposals().iter().any(|prop| prop.is_grease());

            assert!(
                has_grease_proposal,
                "Capabilities should include at least one GREASE proposal type"
            );
        }

        #[test]
        fn test_capabilities_include_grease_credentials() {
            let mdk = create_test_mdk();
            let caps = mdk.capabilities();

            // Verify at least one GREASE value is present in credentials
            let has_grease_credential = caps.credentials().iter().any(|cred| cred.is_grease());

            assert!(
                has_grease_credential,
                "Capabilities should include at least one GREASE credential type"
            );
        }

        #[test]
        fn test_capabilities_still_include_real_values() {
            let mdk = create_test_mdk();
            let caps = mdk.capabilities();

            // Verify the real ciphersuite is still present
            let expected_cs: VerifiableCiphersuite = DEFAULT_CIPHERSUITE.into();
            let has_real_ciphersuite = caps.ciphersuites().contains(&expected_cs);

            assert!(
                has_real_ciphersuite,
                "Capabilities should still include the real ciphersuite"
            );

            // Verify real extensions are still present
            let has_last_resort = caps.extensions().contains(&ExtensionType::LastResort);

            assert!(
                has_last_resort,
                "Capabilities should still include LastResort extension"
            );
        }

        #[test]
        fn test_different_mdk_instances_get_different_grease_values() {
            // Create two MDK instances and verify they get different GREASE values
            // (GREASE values should be randomly selected)
            let mdk1 = create_test_mdk();
            let mdk2 = create_test_mdk();

            let caps1 = mdk1.capabilities();
            let caps2 = mdk2.capabilities();

            // Extract GREASE ciphersuites
            let grease_cs1: Vec<_> = caps1
                .ciphersuites()
                .iter()
                .filter(|cs| cs.is_grease())
                .collect();

            let grease_cs2: Vec<_> = caps2
                .ciphersuites()
                .iter()
                .filter(|cs| cs.is_grease())
                .collect();

            // Both should have GREASE values
            assert!(
                !grease_cs1.is_empty(),
                "MDK1 should have GREASE ciphersuites"
            );
            assert!(
                !grease_cs2.is_empty(),
                "MDK2 should have GREASE ciphersuites"
            );

            // Note: It's possible (but unlikely) that two random selections could be the same,
            // so we don't assert inequality. The test mainly verifies GREASE is being injected.
        }
    }

    /// Tests for sender ratchet configuration (out_of_order_tolerance and maximum_forward_distance).
    ///
    /// These settings control the MLS sender ratchet which handles message ordering:
    /// - `out_of_order_tolerance`: Number of past decryption secrets to keep for out-of-order messages
    /// - `maximum_forward_distance`: Maximum number of skipped messages before decryption fails
    ///
    /// When messages arrive out of order beyond the tolerance, decryption fails.
    /// The default tolerance of 100 handles typical Nostr relay reordering scenarios.
    mod sender_ratchet_tests {
        use nostr::Keys;

        use super::*;
        use crate::messages::MessageProcessingResult;
        use crate::test_util::{
            create_key_package_event, create_nostr_group_config_data, create_test_rumor,
        };

        /// Test that custom MdkConfig is properly applied to groups.
        ///
        /// This test verifies that the configuration is correctly passed through the
        /// group creation and joining process.
        #[test]
        fn test_custom_config_is_applied() {
            let config = MdkConfig {
                out_of_order_tolerance: 50,
                maximum_forward_distance: 500,
                max_event_age_secs: 86400,
                max_future_skew_secs: 120,
                epoch_snapshot_retention: 5,
                snapshot_ttl_seconds: 604800,
            };

            let alice_keys = Keys::generate();
            let bob_keys = Keys::generate();

            let alice_mdk = create_test_mdk_with_config(config.clone());
            let bob_mdk = create_test_mdk_with_config(config.clone());

            // Verify configs are set correctly
            assert_eq!(alice_mdk.config.out_of_order_tolerance, 50);
            assert_eq!(alice_mdk.config.maximum_forward_distance, 500);
            assert_eq!(bob_mdk.config.out_of_order_tolerance, 50);
            assert_eq!(bob_mdk.config.maximum_forward_distance, 500);

            let admins = vec![alice_keys.public_key(), bob_keys.public_key()];

            // Bob creates key package in his MDK
            let bob_key_package = create_key_package_event(&bob_mdk, &bob_keys);

            // Alice creates group and adds Bob
            let create_result = alice_mdk
                .create_group(
                    &alice_keys.public_key(),
                    vec![bob_key_package],
                    create_nostr_group_config_data(admins),
                )
                .expect("Alice should create group");

            let group_id = create_result.group.mls_group_id.clone();

            alice_mdk
                .merge_pending_commit(&group_id)
                .expect("Alice should merge commit");

            // Bob processes welcome and joins with his config
            let bob_welcome_rumor = &create_result.welcome_rumors[0];
            let bob_welcome = bob_mdk
                .process_welcome(&nostr::EventId::all_zeros(), bob_welcome_rumor)
                .expect("Bob should process welcome");

            bob_mdk
                .accept_welcome(&bob_welcome)
                .expect("Bob should accept welcome");

            // Verify both clients have the same group
            assert_eq!(group_id, bob_welcome.mls_group_id);
        }

        /// Test that high out_of_order_tolerance allows heavily reordered messages.
        ///
        /// With tolerance of 100, receiving messages in reverse order (100 messages apart)
        /// should all decrypt successfully.
        #[test]
        fn test_high_tolerance_allows_reordered_messages() {
            // Default tolerance of 100
            let alice_keys = Keys::generate();
            let bob_keys = Keys::generate();

            let alice_mdk = create_test_mdk();
            let bob_mdk = create_test_mdk();

            let admins = vec![alice_keys.public_key(), bob_keys.public_key()];

            let bob_key_package = create_key_package_event(&bob_mdk, &bob_keys);

            let create_result = alice_mdk
                .create_group(
                    &alice_keys.public_key(),
                    vec![bob_key_package],
                    create_nostr_group_config_data(admins),
                )
                .expect("Alice should create group");

            let group_id = create_result.group.mls_group_id.clone();

            alice_mdk
                .merge_pending_commit(&group_id)
                .expect("Alice should merge commit");

            let bob_welcome_rumor = &create_result.welcome_rumors[0];
            let bob_welcome = bob_mdk
                .process_welcome(&nostr::EventId::all_zeros(), bob_welcome_rumor)
                .expect("Bob should process welcome");

            bob_mdk
                .accept_welcome(&bob_welcome)
                .expect("Bob should accept welcome");

            // Alice sends 50 messages (within default tolerance of 100)
            let num_messages = 50;
            let mut message_events = Vec::new();

            for i in 0..num_messages {
                let rumor = create_test_rumor(&alice_keys, &format!("Message {}", i));
                let msg_event = alice_mdk
                    .create_message(&group_id, rumor)
                    .expect("Alice should send message");
                message_events.push(msg_event);
            }

            // Bob receives messages in extreme out-of-order pattern:
            // last, first, second-to-last, second, etc.
            let mut receive_order: Vec<usize> = Vec::new();
            for i in 0..num_messages / 2 {
                receive_order.push(num_messages - 1 - i); // from end
                receive_order.push(i); // from start
            }

            for &idx in &receive_order {
                let msg_event = &message_events[idx];
                let result = bob_mdk
                    .process_message(msg_event)
                    .unwrap_or_else(|e| panic!("Bob should decrypt message {idx}: {e}"));

                match result {
                    MessageProcessingResult::ApplicationMessage(msg) => {
                        assert_eq!(msg.content, format!("Message {}", idx));
                    }
                    other => panic!("Expected ApplicationMessage for message {idx}, got {other:?}"),
                }
            }
        }

        /// Test that low out_of_order_tolerance causes decryption failures for distant messages.
        ///
        /// With tolerance of 5, receiving message 19 first then trying to decrypt
        /// message 0 (which is 19 generations behind) should fail.
        #[test]
        fn test_low_tolerance_rejects_distant_messages() {
            // Very low tolerance
            let config = MdkConfig {
                out_of_order_tolerance: 5,
                ..Default::default()
            };

            let alice_keys = Keys::generate();
            let bob_keys = Keys::generate();

            let alice_mdk = create_test_mdk_with_config(config.clone());
            let bob_mdk = create_test_mdk_with_config(config);

            let admins = vec![alice_keys.public_key(), bob_keys.public_key()];

            let bob_key_package = create_key_package_event(&bob_mdk, &bob_keys);

            let create_result = alice_mdk
                .create_group(
                    &alice_keys.public_key(),
                    vec![bob_key_package],
                    create_nostr_group_config_data(admins),
                )
                .expect("Alice should create group");

            let group_id = create_result.group.mls_group_id.clone();

            alice_mdk
                .merge_pending_commit(&group_id)
                .expect("Alice should merge commit");

            let bob_welcome_rumor = &create_result.welcome_rumors[0];
            let bob_welcome = bob_mdk
                .process_welcome(&nostr::EventId::all_zeros(), bob_welcome_rumor)
                .expect("Bob should process welcome");

            bob_mdk
                .accept_welcome(&bob_welcome)
                .expect("Bob should accept welcome");

            // Alice sends 20 messages
            let num_messages = 20;
            let mut message_events = Vec::new();

            for i in 0..num_messages {
                let rumor = create_test_rumor(&alice_keys, &format!("Message {}", i));
                let msg_event = alice_mdk
                    .create_message(&group_id, rumor)
                    .expect("Alice should send message");
                message_events.push(msg_event);
            }

            // Bob receives the LAST message first (message 19)
            // This advances his ratchet state to generation 19
            let last_msg = &message_events[num_messages - 1];
            let result = bob_mdk
                .process_message(last_msg)
                .expect("Bob should decrypt the latest message");

            match result {
                MessageProcessingResult::ApplicationMessage(msg) => {
                    assert_eq!(msg.content, format!("Message {}", num_messages - 1));
                }
                _ => panic!("Expected ApplicationMessage"),
            }

            // Now Bob tries to decrypt message 0 (which is 19 generations behind)
            // With tolerance of 5, this should return Unprocessable because the
            // ratchet secret for generation 0 was not retained
            let first_msg = &message_events[0];
            let result = bob_mdk.process_message(first_msg);

            match result {
                Ok(MessageProcessingResult::Unprocessable { .. }) => {
                    // Expected - the message is too far in the past
                }
                Ok(MessageProcessingResult::ApplicationMessage(_)) => {
                    panic!(
                        "Message 0 should NOT decrypt after receiving message 19 with tolerance 5"
                    );
                }
                Err(_) => {
                    // Also acceptable - the processing failed entirely
                }
                other => {
                    panic!("Unexpected result: {:?}", other);
                }
            }

            // But messages within tolerance should still work
            // Messages 15, 16, 17, 18 are within 5 generations of message 19
            for (i, msg_event) in message_events
                .iter()
                .enumerate()
                .take(num_messages - 1)
                .skip(num_messages - 5)
            {
                let result = bob_mdk.process_message(msg_event).unwrap_or_else(|e| {
                    panic!("Message {i} should decrypt (within tolerance): {e}")
                });

                match result {
                    MessageProcessingResult::ApplicationMessage(msg) => {
                        assert_eq!(msg.content, format!("Message {}", i));
                    }
                    other => panic!("Expected ApplicationMessage for message {i}, got {other:?}"),
                }
            }
        }
    }
}
