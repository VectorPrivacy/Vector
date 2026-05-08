//! Test utilities for the mdk-core crate
//!
//! This module provides shared test utilities used across multiple test modules
//! to avoid code duplication and ensure consistency in test setup.

use mdk_storage_traits::GroupId;
use mdk_storage_traits::MdkStorageProvider;
use nostr::{Event, EventBuilder, Keys, Kind, PublicKey, RelayUrl};

use crate::MDK;
use crate::groups::NostrGroupConfigData;

/// Creates test group members with standard configuration
///
/// Returns a tuple of (creator_keys, member_keys_vec, admin_pubkeys_vec)
/// where the creator and first member are admins.
pub fn create_test_group_members() -> (Keys, Vec<Keys>, Vec<PublicKey>) {
    let creator = Keys::generate();
    let member1 = Keys::generate();
    let member2 = Keys::generate();

    let creator_pk = creator.public_key();
    let members = vec![member1, member2];
    let admins = vec![creator_pk, members[0].public_key()];

    (creator, members, admins)
}

/// Creates a key package event for a member
///
/// This helper creates a properly signed key package event that can be used
/// in group creation or member addition operations.
pub fn create_key_package_event<Storage>(mdk: &MDK<Storage>, member_keys: &Keys) -> Event
where
    Storage: MdkStorageProvider,
{
    let relays = vec![RelayUrl::parse("wss://test.relay").unwrap()];
    let (key_package_hex, tags, _hash_ref) = mdk
        .create_key_package_for_event(&member_keys.public_key(), relays)
        .expect("Failed to create key package");

    EventBuilder::new(Kind::MlsKeyPackage, key_package_hex)
        .tags(tags)
        .sign_with_keys(member_keys)
        .expect("Failed to sign event")
}

/// Creates a key package event with specified public key and signing keys
///
/// This variant allows creating a key package for one public key but signing
/// it with different keys, useful for testing edge cases.
pub fn create_key_package_event_with_key<Storage>(
    mdk: &MDK<Storage>,
    pubkey: &PublicKey,
    signing_keys: &Keys,
) -> Event
where
    Storage: MdkStorageProvider,
{
    let relays = vec![RelayUrl::parse("wss://test.relay").unwrap()];
    let (key_package_hex, tags, _hash_ref) = mdk
        .create_key_package_for_event(pubkey, relays)
        .expect("Failed to create key package");

    EventBuilder::new(Kind::MlsKeyPackage, key_package_hex)
        .tags(tags)
        .sign_with_keys(signing_keys)
        .expect("Failed to sign event")
}

/// Creates standard test group configuration data
///
/// Returns a NostrGroupConfigData with random test values for creating test groups.
pub fn create_nostr_group_config_data(admins: Vec<PublicKey>) -> NostrGroupConfigData {
    let relays = vec![RelayUrl::parse("wss://test.relay").unwrap()];
    let image_hash = mdk_storage_traits::test_utils::crypto_utils::generate_random_bytes(32)
        .try_into()
        .unwrap();
    let image_key = mdk_storage_traits::test_utils::crypto_utils::generate_random_bytes(32)
        .try_into()
        .unwrap();
    let image_nonce = mdk_storage_traits::test_utils::crypto_utils::generate_random_bytes(12)
        .try_into()
        .unwrap();
    let name = "Test Group".to_owned();
    let description = "A test group for basic testing".to_owned();
    NostrGroupConfigData::new(
        name,
        description,
        Some(image_hash),
        Some(image_key),
        Some(image_nonce),
        relays,
        admins,
    )
}

/// Creates a complete test group and returns the group ID
///
/// This helper function creates a group with the specified creator, members, and admins,
/// then merges the pending commit to complete the group setup.
pub fn create_test_group<Storage>(
    mdk: &MDK<Storage>,
    creator: &Keys,
    members: &[Keys],
    admins: &[PublicKey],
) -> GroupId
where
    Storage: MdkStorageProvider,
{
    let creator_pk = creator.public_key();

    // Create key package events for initial members
    let mut initial_key_package_events = Vec::new();
    for member_keys in members {
        let key_package_event = create_key_package_event(mdk, member_keys);
        initial_key_package_events.push(key_package_event);
    }

    // Create the group
    let create_result = mdk
        .create_group(
            &creator_pk,
            initial_key_package_events,
            create_nostr_group_config_data(admins.to_vec()),
        )
        .expect("Failed to create group");

    let group_id = create_result.group.mls_group_id.clone();

    // Merge the pending commit to apply the member additions
    mdk.merge_pending_commit(&group_id.clone())
        .expect("Failed to merge pending commit");

    group_id
}

/// Creates a test message rumor (unsigned event)
///
/// This helper creates an unsigned event that can be used for testing
/// message creation and processing.
pub fn create_test_rumor(sender_keys: &Keys, content: &str) -> nostr::UnsignedEvent {
    EventBuilder::new(Kind::TextNote, content).build(sender_keys.public_key())
}

/// Helper structure for managing multiple clients in tests
///
/// This structure simplifies testing scenarios involving multiple clients
/// for the same user or multiple users in a group.
pub struct MultiClientTestSetup<Storage>
where
    Storage: MdkStorageProvider,
{
    /// List of clients with their keys and MDK instances
    pub clients: Vec<(Keys, MDK<Storage>)>,
    /// Optional group ID for the test group
    pub group_id: Option<GroupId>,
}

impl<Storage> MultiClientTestSetup<Storage>
where
    Storage: MdkStorageProvider + Default,
{
    /// Create a new multi-client test setup with the specified number of clients
    ///
    /// Each client gets a unique identity (Keys) and MDK instance.
    pub fn new(num_clients: usize) -> Self {
        let mut clients = Vec::new();
        for _ in 0..num_clients {
            let keys = Keys::generate();
            let mdk = MDK::new(Storage::default());
            clients.push((keys, mdk));
        }

        Self {
            clients,
            group_id: None,
        }
    }

    /// Get a reference to a specific client by index
    pub fn get_client(&self, index: usize) -> Option<&(Keys, MDK<Storage>)> {
        self.clients.get(index)
    }

    /// Get a mutable reference to a specific client by index
    pub fn get_client_mut(&mut self, index: usize) -> Option<&mut (Keys, MDK<Storage>)> {
        self.clients.get_mut(index)
    }

    /// Advance the group epoch by creating an update proposal
    ///
    /// This is useful for testing epoch transitions and lookback mechanisms.
    pub fn advance_epoch(&mut self, client_idx: usize) -> Result<(), crate::Error> {
        let group_id = self.group_id.as_ref().ok_or(crate::Error::GroupNotFound)?;

        let client = self
            .get_client(client_idx)
            .ok_or(crate::Error::GroupNotFound)?;
        let mdk = &client.1;

        // Create self-update to advance epoch
        let _update_result = mdk.self_update(group_id)?;
        mdk.merge_pending_commit(group_id)?;

        Ok(())
    }
}

/// Helper for simulating race conditions with controlled timestamps
///
/// This structure helps create deterministic race condition scenarios
/// by allowing control over event timestamps and IDs.
pub struct RaceConditionSimulator {
    /// Base timestamp for generating offset timestamps
    pub base_timestamp: nostr::Timestamp,
}

impl RaceConditionSimulator {
    /// Create a new race condition simulator with the current timestamp
    pub fn new() -> Self {
        Self {
            base_timestamp: nostr::Timestamp::now(),
        }
    }

    /// Create a new simulator with a specific base timestamp
    pub fn with_timestamp(timestamp: nostr::Timestamp) -> Self {
        Self {
            base_timestamp: timestamp,
        }
    }

    /// Get a timestamp offset from the base by the specified number of seconds
    pub fn timestamp_offset(&self, offset_seconds: i64) -> nostr::Timestamp {
        let new_timestamp = (self.base_timestamp.as_secs() as i64 + offset_seconds).max(0) as u64;
        nostr::Timestamp::from(new_timestamp)
    }
}

impl Default for RaceConditionSimulator {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Test Infrastructure (MockRelay, CorruptionSimulator, TimeController)
// ============================================================================

/// Helper to create a group and simulate restart
///
/// This function creates a group, then drops the MDK instance and creates
/// a new one with the same storage to simulate an application restart.
pub fn create_group_and_restart<S>(storage: S) -> (MDK<S>, GroupId, Keys, Vec<Keys>)
where
    S: MdkStorageProvider + Clone,
{
    // Create initial MDK and group
    let mdk = MDK::new(storage.clone());
    let (creator, members, admins) = create_test_group_members();
    let group_id = create_test_group(&mdk, &creator, &members, &admins);

    // Drop the MDK to simulate shutdown
    drop(mdk);

    // Create new MDK with same storage (simulating restart)
    let new_mdk = MDK::new(storage);

    (new_mdk, group_id, creator, members)
}

/// Assert that two group states are equal
///
/// This helper provides detailed error messages when group states don't match,
/// making it easier to debug test failures.
pub fn assert_group_state_equal(
    group1: &mdk_storage_traits::groups::types::Group,
    group2: &mdk_storage_traits::groups::types::Group,
    message: &str,
) {
    assert_eq!(
        group1.mls_group_id, group2.mls_group_id,
        "{}: Group IDs don't match",
        message
    );
    assert_eq!(
        group1.nostr_group_id, group2.nostr_group_id,
        "{}: Nostr Group IDs don't match",
        message
    );
    assert_eq!(
        group1.name, group2.name,
        "{}: Group names don't match",
        message
    );
    assert_eq!(
        group1.description, group2.description,
        "{}: Group descriptions don't match",
        message
    );
    assert_eq!(
        group1.epoch, group2.epoch,
        "{}: Epochs don't match",
        message
    );
    assert_eq!(
        group1.admin_pubkeys, group2.admin_pubkeys,
        "{}: Admin lists don't match",
        message
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_helper_function_randomness() {
        let (_, _, admins) = create_test_group_members();

        // Test that the helper works and generates random data
        let config1 = create_nostr_group_config_data(admins.clone());
        let config2 = create_nostr_group_config_data(admins);

        // Both should have the same basic properties
        assert_eq!(config1.name, "Test Group");
        assert_eq!(config2.name, "Test Group");
        assert_eq!(config1.description, "A test group for basic testing");
        assert_eq!(config2.description, "A test group for basic testing");

        // Random helper should return different values (very unlikely to be the same)
        assert_ne!(config1.image_hash, config2.image_hash);
        assert_ne!(config1.image_key, config2.image_key);
        assert_ne!(config1.image_nonce, config2.image_nonce);

        // All should be Some (not None)
        assert!(config1.image_hash.is_some());
        assert!(config1.image_key.is_some());
        assert!(config1.image_nonce.is_some());
        assert!(config2.image_hash.is_some());
        assert!(config2.image_key.is_some());
        assert!(config2.image_nonce.is_some());
    }
}
