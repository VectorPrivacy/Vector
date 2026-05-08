//! Memory-based storage implementation of the MdkStorageProvider trait for MDK groups

use std::collections::BTreeSet;

use mdk_storage_traits::GroupId;
use mdk_storage_traits::groups::error::GroupError;
use mdk_storage_traits::groups::types::*;
use mdk_storage_traits::groups::{GroupStorage, MAX_MESSAGE_LIMIT, MessageSortOrder, Pagination};
use mdk_storage_traits::messages::types::Message;
use nostr::{PublicKey, RelayUrl};

use crate::MdkMemoryStorage;

impl GroupStorage for MdkMemoryStorage {
    fn save_group(&self, group: Group) -> Result<(), GroupError> {
        // Validate group name length
        if group.name.len() > self.limits.max_group_name_length {
            return Err(GroupError::InvalidParameters(format!(
                "Group name exceeds maximum length of {} bytes (got {} bytes)",
                self.limits.max_group_name_length,
                group.name.len()
            )));
        }

        // Validate group description length
        if group.description.len() > self.limits.max_group_description_length {
            return Err(GroupError::InvalidParameters(format!(
                "Group description exceeds maximum length of {} bytes (got {} bytes)",
                self.limits.max_group_description_length,
                group.description.len()
            )));
        }

        // Validate admin pubkeys count
        if group.admin_pubkeys.len() > self.limits.max_admins_per_group {
            return Err(GroupError::InvalidParameters(format!(
                "Group admin count exceeds maximum of {} (got {})",
                self.limits.max_admins_per_group,
                group.admin_pubkeys.len()
            )));
        }

        // Acquire lock on inner storage
        let mut guard = self.inner.write();
        let inner = &mut *guard;
        let groups_cache = &mut inner.groups_cache;
        let nostr_id_cache = &mut inner.groups_by_nostr_id_cache;

        // Check if nostr_group_id is already mapped to a different mls_group_id
        if let Some(existing_group) = nostr_id_cache.peek(&group.nostr_group_id)
            && existing_group.mls_group_id != group.mls_group_id
        {
            return Err(GroupError::InvalidParameters(
                "nostr_group_id already exists for a different group".to_string(),
            ));
        }

        // If updating an existing group and nostr_group_id changed, remove stale entry
        if let Some(existing_group) = groups_cache.peek(&group.mls_group_id)
            && existing_group.nostr_group_id != group.nostr_group_id
        {
            nostr_id_cache.pop(&existing_group.nostr_group_id);
        }

        // Store in both caches
        groups_cache.put(group.mls_group_id.clone(), group.clone());
        nostr_id_cache.put(group.nostr_group_id, group);

        Ok(())
    }

    fn all_groups(&self) -> Result<Vec<Group>, GroupError> {
        let inner = self.inner.read();
        // Convert the values from the cache to a Vec
        let groups: Vec<Group> = inner.groups_cache.iter().map(|(_, v)| v.clone()).collect();
        Ok(groups)
    }

    fn find_group_by_mls_group_id(
        &self,
        mls_group_id: &GroupId,
    ) -> Result<Option<Group>, GroupError> {
        let inner = self.inner.read();
        Ok(inner.groups_cache.peek(mls_group_id).cloned())
    }

    fn find_group_by_nostr_group_id(
        &self,
        nostr_group_id: &[u8; 32],
    ) -> Result<Option<Group>, GroupError> {
        let inner = self.inner.read();
        Ok(inner.groups_by_nostr_id_cache.peek(nostr_group_id).cloned())
    }

    fn messages(
        &self,
        mls_group_id: &GroupId,
        pagination: Option<Pagination>,
    ) -> Result<Vec<Message>, GroupError> {
        let pagination = pagination.unwrap_or_default();
        let limit = pagination.limit();
        let offset = pagination.offset();

        // Validate limit is within allowed range
        if !(1..=MAX_MESSAGE_LIMIT).contains(&limit) {
            return Err(GroupError::InvalidParameters(format!(
                "Limit must be between 1 and {}, got {}",
                MAX_MESSAGE_LIMIT, limit
            )));
        }

        let inner = self.inner.read();

        // Check if the group exists while holding the lock
        if inner.groups_cache.peek(mls_group_id).is_none() {
            return Err(GroupError::InvalidParameters("Group not found".to_string()));
        }

        let sort_order = pagination.sort_order();

        match inner.messages_by_group_cache.peek(mls_group_id) {
            Some(messages_map) => {
                // Collect values from HashMap into a Vec for sorting
                let mut messages: Vec<Message> = messages_map.values().cloned().collect();

                // Sort newest-first using the requested sort order.
                // Both comparators are called with (b, a) to get DESC ordering.
                match sort_order {
                    MessageSortOrder::CreatedAtFirst => {
                        messages.sort_by(|a, b| b.display_order_cmp(a));
                    }
                    MessageSortOrder::ProcessedAtFirst => {
                        messages.sort_by(|a, b| b.processed_at_order_cmp(a));
                    }
                }

                // Apply pagination
                let start = offset.min(messages.len());
                let end = (offset + limit).min(messages.len());

                Ok(messages[start..end].to_vec())
            }
            // If not in cache but group exists, return empty vector
            None => Ok(Vec::new()),
        }
    }

    fn last_message(
        &self,
        mls_group_id: &GroupId,
        sort_order: MessageSortOrder,
    ) -> Result<Option<Message>, GroupError> {
        let inner = self.inner.read();

        if inner.groups_cache.peek(mls_group_id).is_none() {
            return Err(GroupError::InvalidParameters("Group not found".to_string()));
        }

        match inner.messages_by_group_cache.peek(mls_group_id) {
            Some(messages_map) if !messages_map.is_empty() => {
                // Find the maximum element under the requested ordering.
                // Both comparators compare (b, a) to find the DESC-first element via max_by.
                let winner = match sort_order {
                    MessageSortOrder::CreatedAtFirst => {
                        messages_map.values().max_by(|a, b| a.display_order_cmp(b))
                    }
                    MessageSortOrder::ProcessedAtFirst => messages_map
                        .values()
                        .max_by(|a, b| a.processed_at_order_cmp(b)),
                };
                Ok(winner.cloned())
            }
            _ => Ok(None),
        }
    }

    fn admins(&self, mls_group_id: &GroupId) -> Result<BTreeSet<PublicKey>, GroupError> {
        match self.find_group_by_mls_group_id(mls_group_id)? {
            Some(group) => Ok(group.admin_pubkeys.clone()),
            None => Err(GroupError::InvalidParameters("Group not found".to_string())),
        }
    }

    fn group_relays(&self, mls_group_id: &GroupId) -> Result<BTreeSet<GroupRelay>, GroupError> {
        let inner = self.inner.read();

        // Check if the group exists while holding the lock
        if inner.groups_cache.peek(mls_group_id).is_none() {
            return Err(GroupError::InvalidParameters("Group not found".to_string()));
        }

        match inner.group_relays_cache.peek(mls_group_id).cloned() {
            Some(relays) => Ok(relays),
            // If not in cache but group exists, return empty set
            None => Ok(BTreeSet::new()),
        }
    }

    fn replace_group_relays(
        &self,
        group_id: &GroupId,
        relays: BTreeSet<RelayUrl>,
    ) -> Result<(), GroupError> {
        // Validate relay count to prevent memory exhaustion
        if relays.len() > self.limits.max_relays_per_group {
            return Err(GroupError::InvalidParameters(format!(
                "Relay count exceeds maximum of {} (got {})",
                self.limits.max_relays_per_group,
                relays.len()
            )));
        }

        // Validate individual relay URL lengths
        for relay in &relays {
            if relay.as_str().len() > self.limits.max_relay_url_length {
                return Err(GroupError::InvalidParameters(format!(
                    "Relay URL exceeds maximum length of {} bytes",
                    self.limits.max_relay_url_length
                )));
            }
        }

        let mut inner = self.inner.write();

        // Check if the group exists while holding the lock
        if inner.groups_cache.peek(group_id).is_none() {
            return Err(GroupError::InvalidParameters("Group not found".to_string()));
        }

        // Convert RelayUrl set to GroupRelay set
        let group_relays: BTreeSet<GroupRelay> = relays
            .into_iter()
            .map(|relay_url| GroupRelay {
                mls_group_id: group_id.clone(),
                relay_url,
            })
            .collect();

        // Replace the entire relay set for this group
        inner.group_relays_cache.put(group_id.clone(), group_relays);

        Ok(())
    }

    fn get_group_exporter_secret(
        &self,
        mls_group_id: &GroupId,
        epoch: u64,
    ) -> Result<Option<GroupExporterSecret>, GroupError> {
        let inner = self.inner.read();

        // Check if the group exists while holding the lock
        if inner.groups_cache.peek(mls_group_id).is_none() {
            return Err(GroupError::InvalidParameters("Group not found".to_string()));
        }

        // Use tuple (GroupId, epoch) as key
        Ok(inner
            .group_exporter_secrets_cache
            .peek(&(mls_group_id.clone(), epoch))
            .cloned())
    }

    fn save_group_exporter_secret(
        &self,
        group_exporter_secret: GroupExporterSecret,
    ) -> Result<(), GroupError> {
        let mut inner = self.inner.write();

        // Check if the group exists while holding the lock
        if inner
            .groups_cache
            .peek(&group_exporter_secret.mls_group_id)
            .is_none()
        {
            return Err(GroupError::InvalidParameters("Group not found".to_string()));
        }

        // Use tuple (GroupId, epoch) as key
        let key = (
            group_exporter_secret.mls_group_id.clone(),
            group_exporter_secret.epoch,
        );
        inner
            .group_exporter_secrets_cache
            .put(key, group_exporter_secret);

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use mdk_storage_traits::groups::types::GroupState;
    use mdk_storage_traits::messages::MessageStorage;
    use mdk_storage_traits::messages::types::{Message, MessageState};
    use nostr::{EventId, Keys, Kind, Tags, Timestamp, UnsignedEvent};

    use super::*;
    use crate::{
        DEFAULT_MAX_ADMINS_PER_GROUP, DEFAULT_MAX_GROUP_DESCRIPTION_LENGTH,
        DEFAULT_MAX_GROUP_NAME_LENGTH, DEFAULT_MAX_RELAY_URL_LENGTH, DEFAULT_MAX_RELAYS_PER_GROUP,
    };

    fn create_test_group(mls_group_id: GroupId, nostr_group_id: [u8; 32]) -> Group {
        Group {
            mls_group_id,
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
        }
    }

    #[test]
    fn test_save_group_name_length_validation() {
        let storage = MdkMemoryStorage::new();
        let mls_group_id = GroupId::from_slice(&[1, 2, 3, 4]);

        // Test with name at exactly the limit (should succeed)
        let mut group = create_test_group(mls_group_id.clone(), [1u8; 32]);
        group.name = "a".repeat(DEFAULT_MAX_GROUP_NAME_LENGTH);
        assert!(storage.save_group(group).is_ok());

        // Test with name exceeding the limit (should fail)
        let mut group = create_test_group(GroupId::from_slice(&[2, 3, 4, 5]), [2u8; 32]);
        group.name = "a".repeat(DEFAULT_MAX_GROUP_NAME_LENGTH + 1);
        let result = storage.save_group(group);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Group name exceeds maximum length")
        );
    }

    #[test]
    fn test_save_group_description_length_validation() {
        let storage = MdkMemoryStorage::new();
        let mls_group_id = GroupId::from_slice(&[1, 2, 3, 4]);

        // Test with description at exactly the limit (should succeed)
        let mut group = create_test_group(mls_group_id.clone(), [1u8; 32]);
        group.description = "a".repeat(DEFAULT_MAX_GROUP_DESCRIPTION_LENGTH);
        assert!(storage.save_group(group).is_ok());

        // Test with description exceeding the limit (should fail)
        let mut group = create_test_group(GroupId::from_slice(&[2, 3, 4, 5]), [2u8; 32]);
        group.description = "a".repeat(DEFAULT_MAX_GROUP_DESCRIPTION_LENGTH + 1);
        let result = storage.save_group(group);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Group description exceeds maximum length")
        );
    }

    #[test]
    fn test_save_group_admin_count_validation() {
        let storage = MdkMemoryStorage::new();
        let mls_group_id = GroupId::from_slice(&[1, 2, 3, 4]);

        // Test with admin count at exactly the limit (should succeed)
        let mut group = create_test_group(mls_group_id.clone(), [1u8; 32]);
        for _ in 0..DEFAULT_MAX_ADMINS_PER_GROUP {
            group.admin_pubkeys.insert(Keys::generate().public_key());
        }
        assert!(storage.save_group(group).is_ok());

        // Test with admin count exceeding the limit (should fail)
        let mut group = create_test_group(GroupId::from_slice(&[2, 3, 4, 5]), [2u8; 32]);
        for _ in 0..=DEFAULT_MAX_ADMINS_PER_GROUP {
            group.admin_pubkeys.insert(Keys::generate().public_key());
        }
        let result = storage.save_group(group);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Group admin count exceeds maximum")
        );
    }

    #[test]
    fn test_replace_group_relays_count_validation() {
        let storage = MdkMemoryStorage::new();
        let mls_group_id = GroupId::from_slice(&[1, 2, 3, 4]);

        // Create a group first
        let group = create_test_group(mls_group_id.clone(), [1u8; 32]);
        storage.save_group(group).unwrap();

        // Test with relay count at exactly the limit (should succeed)
        let mut relays = BTreeSet::new();
        for i in 0..DEFAULT_MAX_RELAYS_PER_GROUP {
            relays.insert(RelayUrl::parse(&format!("wss://relay{}.example.com", i)).unwrap());
        }
        assert!(storage.replace_group_relays(&mls_group_id, relays).is_ok());

        // Test with relay count exceeding the limit (should fail)
        let mut relays = BTreeSet::new();
        for i in 0..=DEFAULT_MAX_RELAYS_PER_GROUP {
            relays.insert(RelayUrl::parse(&format!("wss://relay{}.example.com", i)).unwrap());
        }
        let result = storage.replace_group_relays(&mls_group_id, relays);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Relay count exceeds maximum")
        );
    }

    #[test]
    fn test_replace_group_relays_url_length_validation() {
        let storage = MdkMemoryStorage::new();
        let mls_group_id = GroupId::from_slice(&[1, 2, 3, 4]);

        // Create a group first
        let group = create_test_group(mls_group_id.clone(), [1u8; 32]);
        storage.save_group(group).unwrap();

        // Test with URL at exactly the limit (should succeed)
        // URL format: wss:// (6) + domain + .com (4) = need domain of DEFAULT_MAX_RELAY_URL_LENGTH - 10
        let domain = "a".repeat(DEFAULT_MAX_RELAY_URL_LENGTH - 10);
        let url = format!("wss://{}.com", domain);
        let relays = BTreeSet::from([RelayUrl::parse(&url).unwrap()]);
        assert!(storage.replace_group_relays(&mls_group_id, relays).is_ok());

        // Test with URL exceeding the limit (should fail)
        let domain = "a".repeat(DEFAULT_MAX_RELAY_URL_LENGTH); // This will exceed the limit
        let url = format!("wss://{}.com", domain);
        let relays = BTreeSet::from([RelayUrl::parse(&url).unwrap()]);
        let result = storage.replace_group_relays(&mls_group_id, relays);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Relay URL exceeds maximum length")
        );
    }

    #[test]
    fn test_messages_pagination_memory() {
        let storage = MdkMemoryStorage::new();

        // Create a test group
        let mls_group_id = GroupId::from_slice(&[1, 2, 3, 4]);
        let nostr_group_id = [1u8; 32];

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

        storage.save_group(group).unwrap();

        // Create 25 test messages
        let pubkey = Keys::generate().public_key();
        for i in 0..25 {
            let event_id = EventId::from_slice(&[i as u8; 32]).unwrap();
            let wrapper_event_id = EventId::from_slice(&[100 + i as u8; 32]).unwrap();

            let ts = Timestamp::from((1000 + i) as u64);
            let message = Message {
                id: event_id,
                pubkey,
                kind: Kind::from(1u16),
                mls_group_id: mls_group_id.clone(),
                created_at: ts,
                processed_at: ts,
                content: format!("Message {}", i),
                tags: Tags::new(),
                event: UnsignedEvent::new(
                    pubkey,
                    ts,
                    Kind::from(9u16),
                    vec![],
                    format!("content {}", i),
                ),
                wrapper_event_id,
                state: MessageState::Created,
                epoch: None,
            };

            storage.save_message(message).unwrap();
        }

        // Test 1: Get all messages with default limit
        let all_messages = storage.messages(&mls_group_id, None).unwrap();
        assert_eq!(all_messages.len(), 25);

        // Test 2: Get first 10 messages
        let page1 = storage
            .messages(&mls_group_id, Some(Pagination::new(Some(10), Some(0))))
            .unwrap();
        assert_eq!(page1.len(), 10);
        // Should be newest first (highest timestamp)
        assert_eq!(page1[0].content, "Message 24");

        // Test 3: Get next 10 messages (offset 10)
        let page2 = storage
            .messages(&mls_group_id, Some(Pagination::new(Some(10), Some(10))))
            .unwrap();
        assert_eq!(page2.len(), 10);
        assert_eq!(page2[0].content, "Message 14");

        // Test 4: Get last 5 messages (offset 20)
        let page3 = storage
            .messages(&mls_group_id, Some(Pagination::new(Some(10), Some(20))))
            .unwrap();
        assert_eq!(page3.len(), 5);
        assert_eq!(page3[0].content, "Message 4");

        // Test 5: Offset beyond available messages returns empty
        let beyond = storage
            .messages(&mls_group_id, Some(Pagination::new(Some(10), Some(30))))
            .unwrap();
        assert_eq!(beyond.len(), 0);

        // Test 6: Verify no overlap between pages
        let first_id = page1[0].id;
        let second_page_ids: Vec<EventId> = page2.iter().map(|m| m.id).collect();
        assert!(
            !second_page_ids.contains(&first_id),
            "Pages should not overlap"
        );

        // Test 7: Verify ordering is descending by created_at
        for i in 0..page1.len() - 1 {
            assert!(
                page1[i].created_at >= page1[i + 1].created_at,
                "Messages should be ordered by created_at descending"
            );
        }

        // Test 8: Limit of 0 should return error
        let result = storage.messages(&mls_group_id, Some(Pagination::new(Some(0), Some(0))));
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("must be between 1 and")
        );

        // Test 9: Limit exceeding MAX should return error
        let result = storage.messages(&mls_group_id, Some(Pagination::new(Some(20000), Some(0))));
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("must be between 1 and")
        );

        // Test 10: Non-existent group returns error
        let fake_group_id = GroupId::from_slice(&[99, 99, 99, 99]);
        let result = storage.messages(&fake_group_id, Some(Pagination::new(Some(10), Some(0))));
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));

        // Test 11: Empty results when group has no messages
        let empty_group_id = GroupId::from_slice(&[5, 6, 7, 8]);
        let empty_group = Group {
            mls_group_id: empty_group_id.clone(),
            nostr_group_id: [2u8; 32],
            name: "Empty Group".to_string(),
            description: "A group with no messages".to_string(),
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
        storage.save_group(empty_group).unwrap();

        let empty = storage
            .messages(&empty_group_id, Some(Pagination::new(Some(10), Some(0))))
            .unwrap();
        assert_eq!(empty.len(), 0);

        // Test 12: Large offset should work (no MAX_OFFSET validation)
        let result = storage.messages(
            &mls_group_id,
            Some(Pagination::new(Some(10), Some(2_000_000))),
        );
        assert!(result.is_ok());
        assert_eq!(result.unwrap().len(), 0); // No results at that offset
    }

    /// Test that custom validation limits work correctly for groups
    #[test]
    fn test_custom_group_limits() {
        use crate::ValidationLimits;

        // Create storage with custom smaller limits
        let limits = ValidationLimits::default()
            .with_max_group_name_length(10)
            .with_max_group_description_length(20)
            .with_max_admins_per_group(2)
            .with_max_relays_per_group(3);

        let storage = MdkMemoryStorage::with_limits(limits);

        // Verify limits are accessible
        assert_eq!(storage.limits().max_group_name_length, 10);
        assert_eq!(storage.limits().max_group_description_length, 20);
        assert_eq!(storage.limits().max_admins_per_group, 2);
        assert_eq!(storage.limits().max_relays_per_group, 3);

        let mls_group_id = GroupId::from_slice(&[1, 2, 3, 4]);

        // Test name length with custom limit (10 chars should succeed)
        let mut group = create_test_group(mls_group_id.clone(), [1u8; 32]);
        group.name = "a".repeat(10);
        assert!(storage.save_group(group).is_ok());

        // Test name length exceeding custom limit (11 chars should fail)
        let mut group = create_test_group(GroupId::from_slice(&[2, 3, 4, 5]), [2u8; 32]);
        group.name = "a".repeat(11);
        let result = storage.save_group(group);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("10 bytes"));

        // Test admin count with custom limit (2 admins should succeed)
        let mut group = create_test_group(GroupId::from_slice(&[3, 4, 5, 6]), [3u8; 32]);
        group.admin_pubkeys.insert(Keys::generate().public_key());
        group.admin_pubkeys.insert(Keys::generate().public_key());
        assert!(storage.save_group(group).is_ok());

        // Test admin count exceeding custom limit (3 admins should fail)
        let mut group = create_test_group(GroupId::from_slice(&[4, 5, 6, 7]), [4u8; 32]);
        for _ in 0..3 {
            group.admin_pubkeys.insert(Keys::generate().public_key());
        }
        let result = storage.save_group(group);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("maximum of 2"));

        // Test relay count with custom limit
        let group = create_test_group(GroupId::from_slice(&[5, 6, 7, 8]), [5u8; 32]);
        storage.save_group(group).unwrap();

        // 3 relays should succeed
        let relays = BTreeSet::from([
            RelayUrl::parse("wss://r1.com").unwrap(),
            RelayUrl::parse("wss://r2.com").unwrap(),
            RelayUrl::parse("wss://r3.com").unwrap(),
        ]);
        assert!(
            storage
                .replace_group_relays(&GroupId::from_slice(&[5, 6, 7, 8]), relays)
                .is_ok()
        );

        // 4 relays should fail
        let relays = BTreeSet::from([
            RelayUrl::parse("wss://r1.com").unwrap(),
            RelayUrl::parse("wss://r2.com").unwrap(),
            RelayUrl::parse("wss://r3.com").unwrap(),
            RelayUrl::parse("wss://r4.com").unwrap(),
        ]);
        let result = storage.replace_group_relays(&GroupId::from_slice(&[5, 6, 7, 8]), relays);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("maximum of 3"));
    }

    #[test]
    fn test_nostr_group_id_collision_rejected() {
        let storage = MdkMemoryStorage::new();

        // Create first group with a specific nostr_group_id
        let mls_group_id_1 = GroupId::from_slice(&[1, 2, 3, 4]);
        let shared_nostr_group_id = [42u8; 32];

        let group1 = Group {
            mls_group_id: mls_group_id_1.clone(),
            nostr_group_id: shared_nostr_group_id,
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

        // Attempt to create a second group with the same nostr_group_id but different mls_group_id
        let mls_group_id_2 = GroupId::from_slice(&[5, 6, 7, 8]);

        let group2 = Group {
            mls_group_id: mls_group_id_2.clone(),
            nostr_group_id: shared_nostr_group_id, // Same nostr_group_id - collision!
            name: "Group 2".to_string(),
            description: "Second group trying to hijack".to_string(),
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

        // This should fail because nostr_group_id is already used by a different group
        let result = storage.save_group(group2);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("nostr_group_id already exists"),
            "Expected collision error, got: {}",
            err
        );

        // Verify the original group is still intact
        let found = storage
            .find_group_by_nostr_group_id(&shared_nostr_group_id)
            .unwrap()
            .unwrap();
        assert_eq!(found.mls_group_id, mls_group_id_1);
        assert_eq!(found.name, "Group 1");
    }

    #[test]
    fn test_nostr_group_id_update_removes_stale_entry() {
        let storage = MdkMemoryStorage::new();

        let mls_group_id = GroupId::from_slice(&[1, 2, 3, 4]);
        let old_nostr_group_id = [1u8; 32];
        let new_nostr_group_id = [2u8; 32];

        // Create group with initial nostr_group_id
        let group = Group {
            mls_group_id: mls_group_id.clone(),
            nostr_group_id: old_nostr_group_id,
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

        storage.save_group(group).unwrap();

        // Verify group is findable by old nostr_group_id
        assert!(
            storage
                .find_group_by_nostr_group_id(&old_nostr_group_id)
                .unwrap()
                .is_some()
        );

        // Update the group with a new nostr_group_id
        let updated_group = Group {
            mls_group_id: mls_group_id.clone(),
            nostr_group_id: new_nostr_group_id,
            name: "Test Group Updated".to_string(),
            description: "A test group".to_string(),
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

        storage.save_group(updated_group).unwrap();

        // Old nostr_group_id should no longer find the group (stale entry removed)
        assert!(
            storage
                .find_group_by_nostr_group_id(&old_nostr_group_id)
                .unwrap()
                .is_none(),
            "Old nostr_group_id should not find the group after update"
        );

        // New nostr_group_id should find the updated group
        let found = storage
            .find_group_by_nostr_group_id(&new_nostr_group_id)
            .unwrap()
            .unwrap();
        assert_eq!(found.mls_group_id, mls_group_id);
        assert_eq!(found.name, "Test Group Updated");
        assert_eq!(found.epoch, 1);
    }

    #[test]
    fn test_same_group_update_allowed() {
        let storage = MdkMemoryStorage::new();

        let mls_group_id = GroupId::from_slice(&[1, 2, 3, 4]);
        let nostr_group_id = [1u8; 32];

        // Create initial group
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

        storage.save_group(group).unwrap();

        // Update the same group (same mls_group_id and nostr_group_id)
        let updated_group = Group {
            mls_group_id: mls_group_id.clone(),
            nostr_group_id, // Same nostr_group_id
            name: "Updated Group Name".to_string(),
            description: "Updated description".to_string(),
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

        // This should succeed - updating the same group
        let result = storage.save_group(updated_group);
        assert!(result.is_ok());

        // Verify the update was applied
        let found = storage
            .find_group_by_mls_group_id(&mls_group_id)
            .unwrap()
            .unwrap();
        assert_eq!(found.name, "Updated Group Name");
        assert_eq!(found.epoch, 1);
    }
}
