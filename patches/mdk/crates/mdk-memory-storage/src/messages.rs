//! Memory-based storage implementation of the MdkStorageProvider trait for MDK messages

use std::collections::HashMap;

use mdk_storage_traits::GroupId;
use nostr::EventId;
#[cfg(test)]
use nostr::{Kind, Tags, Timestamp, UnsignedEvent};

use mdk_storage_traits::groups::GroupStorage;
use mdk_storage_traits::messages::MessageStorage;
use mdk_storage_traits::messages::error::MessageError;
use mdk_storage_traits::messages::types::*;

use crate::MdkMemoryStorage;

impl MessageStorage for MdkMemoryStorage {
    fn save_message(&self, message: Message) -> Result<(), MessageError> {
        // Verify that the group exists before saving the message
        match self.find_group_by_mls_group_id(&message.mls_group_id) {
            Ok(Some(_)) => {
                // Group exists, proceed with saving
            }
            Ok(None) => {
                return Err(MessageError::InvalidParameters(
                    "Group not found".to_string(),
                ));
            }
            Err(e) => {
                return Err(MessageError::InvalidParameters(format!(
                    "Failed to verify group existence: {}",
                    e
                )));
            }
        }

        // Acquire lock on inner storage
        let mut guard = self.inner.write();
        let inner = &mut *guard;
        let cache = &mut inner.messages_cache;
        let group_cache = &mut inner.messages_by_group_cache;

        match group_cache.get_mut(&message.mls_group_id) {
            Some(group_messages) => {
                // Check if this is an update (message already exists) or a new message
                let is_update = group_messages.contains_key(&message.id);

                if !is_update && group_messages.len() >= self.limits.max_messages_per_group {
                    // Evict the oldest message to make room for the new one
                    // Find the message with the oldest created_at timestamp
                    if let Some(oldest_id) = group_messages
                        .iter()
                        .min_by_key(|(_, msg)| msg.created_at)
                        .map(|(id, _)| *id)
                    {
                        // Remove from both caches to prevent orphaned entries
                        group_messages.remove(&oldest_id);
                        cache.pop(&oldest_id);
                    }
                }

                // O(1) insert or update using HashMap
                group_messages.insert(message.id, message.clone());
            }
            None => {
                // Create new HashMap for this group
                let mut messages = HashMap::new();
                let group_id = message.mls_group_id.clone();
                messages.insert(message.id, message.clone());
                group_cache.put(group_id, messages);
            }
        }

        // Save in the messages cache
        cache.put(message.id, message);

        Ok(())
    }

    fn find_message_by_event_id(
        &self,
        mls_group_id: &GroupId,
        event_id: &EventId,
    ) -> Result<Option<Message>, MessageError> {
        let inner = self.inner.read();
        match inner.messages_by_group_cache.peek(mls_group_id) {
            Some(group_messages) => Ok(group_messages.get(event_id).cloned()),
            None => Ok(None),
        }
    }

    fn find_processed_message_by_event_id(
        &self,
        event_id: &EventId,
    ) -> Result<Option<ProcessedMessage>, MessageError> {
        let inner = self.inner.read();
        Ok(inner.processed_messages_cache.peek(event_id).cloned())
    }

    fn save_processed_message(
        &self,
        processed_message: ProcessedMessage,
    ) -> Result<(), MessageError> {
        let mut inner = self.inner.write();
        inner
            .processed_messages_cache
            .put(processed_message.wrapper_event_id, processed_message);

        Ok(())
    }

    fn invalidate_messages_after_epoch(
        &self,
        group_id: &GroupId,
        epoch: u64,
    ) -> Result<Vec<EventId>, MessageError> {
        let mut inner = self.inner.write();
        let mut invalidated_ids = Vec::new();

        // Get the group messages
        if let Some(group_messages) = inner.messages_by_group_cache.get_mut(group_id) {
            for (event_id, message) in group_messages.iter_mut() {
                // Only invalidate messages with epoch > target
                if let Some(msg_epoch) = message.epoch
                    && msg_epoch > epoch
                {
                    message.state = MessageState::EpochInvalidated;
                    invalidated_ids.push(*event_id);
                }
            }
        }

        // Also update in the messages_cache
        for event_id in &invalidated_ids {
            if let Some(message) = inner.messages_cache.get_mut(event_id) {
                message.state = MessageState::EpochInvalidated;
            }
        }

        Ok(invalidated_ids)
    }

    fn invalidate_processed_messages_after_epoch(
        &self,
        group_id: &GroupId,
        epoch: u64,
    ) -> Result<Vec<EventId>, MessageError> {
        let mut inner = self.inner.write();
        let mut invalidated_ids = Vec::new();

        // Iterate through all processed messages and invalidate those matching the group and epoch
        let cache = &mut inner.processed_messages_cache;
        for (wrapper_event_id, processed_message) in cache.iter_mut() {
            // Check if this message belongs to the specified group
            if let Some(ref msg_group_id) = processed_message.mls_group_id
                && msg_group_id == group_id
                && let Some(msg_epoch) = processed_message.epoch
                && msg_epoch > epoch
            {
                processed_message.state = ProcessedMessageState::EpochInvalidated;
                invalidated_ids.push(*wrapper_event_id);
            }
        }

        Ok(invalidated_ids)
    }

    fn find_invalidated_messages(&self, group_id: &GroupId) -> Result<Vec<Message>, MessageError> {
        let inner = self.inner.read();

        if let Some(group_messages) = inner.messages_by_group_cache.peek(group_id) {
            let invalidated: Vec<Message> = group_messages
                .values()
                .filter(|msg| msg.state == MessageState::EpochInvalidated)
                .cloned()
                .collect();
            Ok(invalidated)
        } else {
            Ok(Vec::new())
        }
    }

    fn find_invalidated_processed_messages(
        &self,
        group_id: &GroupId,
    ) -> Result<Vec<ProcessedMessage>, MessageError> {
        let inner = self.inner.read();

        let invalidated: Vec<ProcessedMessage> = inner
            .processed_messages_cache
            .iter()
            .filter_map(|(_, pm)| {
                if let Some(ref msg_group_id) = pm.mls_group_id
                    && msg_group_id == group_id
                    && pm.state == ProcessedMessageState::EpochInvalidated
                {
                    return Some(pm.clone());
                }
                None
            })
            .collect();

        Ok(invalidated)
    }

    fn find_failed_messages_for_retry(
        &self,
        group_id: &GroupId,
    ) -> Result<Vec<EventId>, MessageError> {
        let inner = self.inner.read();

        // Find processed messages that:
        // - Are for this group
        // - Have state = Failed
        // - Have epoch = None (decryption failed before epoch could be determined)
        let event_ids: Vec<EventId> = inner
            .processed_messages_cache
            .iter()
            .filter_map(|(wrapper_event_id, pm)| {
                if let Some(ref msg_group_id) = pm.mls_group_id
                    && msg_group_id == group_id
                    && pm.state == ProcessedMessageState::Failed
                    && pm.epoch.is_none()
                {
                    return Some(*wrapper_event_id);
                }
                None
            })
            .collect();

        Ok(event_ids)
    }

    fn mark_processed_message_retryable(&self, event_id: &EventId) -> Result<(), MessageError> {
        let mut inner = self.inner.write();

        // Only update messages that are currently in Failed state
        if let Some(pm) = inner.processed_messages_cache.get_mut(event_id)
            && pm.state == ProcessedMessageState::Failed
        {
            pm.state = ProcessedMessageState::Retryable;
            return Ok(());
        }

        Err(MessageError::NotFound)
    }

    fn find_message_epoch_by_tag_content(
        &self,
        group_id: &GroupId,
        content_substring: &str,
    ) -> Result<Option<u64>, MessageError> {
        let inner = self.inner.read();

        let Some(group_messages) = inner.messages_by_group_cache.peek(group_id) else {
            return Ok(None);
        };

        for (epoch, message) in group_messages
            .values()
            .filter_map(|message| message.epoch.map(|epoch| (epoch, message)))
        {
            let tags_json = serde_json::to_string(&message.tags).map_err(|e| {
                MessageError::DatabaseError(format!("Failed to serialize tags: {e}"))
            })?;

            if tags_json.contains(content_substring) {
                return Ok(Some(epoch));
            }
        }

        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use mdk_storage_traits::groups::GroupStorage;
    use mdk_storage_traits::groups::types::{Group, GroupState, SelfUpdateState};
    use nostr::Keys;

    use super::*;

    fn create_test_group(group_id: GroupId) -> Group {
        // Use the group_id bytes to derive a unique nostr_group_id
        let mut nostr_group_id = [0u8; 32];
        let group_id_bytes = group_id.as_slice();
        nostr_group_id[..group_id_bytes.len().min(32)]
            .copy_from_slice(&group_id_bytes[..group_id_bytes.len().min(32)]);

        Group {
            mls_group_id: group_id.clone(),
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

    fn create_test_message(
        event_id: EventId,
        group_id: GroupId,
        content: &str,
        timestamp: u64,
    ) -> Message {
        create_test_message_with_epoch(event_id, group_id, content, timestamp, None)
    }

    fn create_test_message_with_epoch(
        event_id: EventId,
        group_id: GroupId,
        content: &str,
        timestamp: u64,
        epoch: Option<u64>,
    ) -> Message {
        let pubkey = Keys::generate().public_key();
        let wrapper_event_id = EventId::from_slice(&[200u8; 32]).unwrap();
        let ts = Timestamp::from(timestamp);

        Message {
            id: event_id,
            pubkey,
            kind: Kind::from(1u16),
            mls_group_id: group_id,
            created_at: ts,
            processed_at: ts,
            content: content.to_string(),
            tags: Tags::new(),
            event: UnsignedEvent::new(pubkey, ts, Kind::from(9u16), vec![], content.to_string()),
            wrapper_event_id,
            epoch,
            state: MessageState::Created,
        }
    }

    /// Test that saving a message with the same EventId updates the existing message
    /// rather than creating a duplicate. This verifies the O(1) update behavior
    /// of the HashMap-based implementation.
    #[test]
    fn test_save_message_update_existing() {
        let storage = MdkMemoryStorage::new();

        let group_id = GroupId::from_slice(&[1, 2, 3, 4]);
        let event_id = EventId::from_slice(&[10u8; 32]).unwrap();

        // Create the group first
        let group = create_test_group(group_id.clone());
        storage.save_group(group).unwrap();

        // Save initial message
        let message1 = create_test_message(event_id, group_id.clone(), "Original content", 1000);
        storage.save_message(message1).unwrap();

        // Verify initial message is saved
        let found = storage
            .find_message_by_event_id(&group_id, &event_id)
            .unwrap()
            .unwrap();
        assert_eq!(found.content, "Original content");

        // Verify the group cache has exactly 1 message
        {
            let inner = storage.inner.read();
            let cache = &inner.messages_by_group_cache;
            let group_messages = cache.peek(&group_id).unwrap();
            assert_eq!(group_messages.len(), 1);
        }

        // Save updated message with same EventId but different content
        let message2 = create_test_message(event_id, group_id.clone(), "Updated content", 1001);
        storage.save_message(message2).unwrap();

        // Verify the message was updated, not duplicated
        let found = storage
            .find_message_by_event_id(&group_id, &event_id)
            .unwrap()
            .unwrap();
        assert_eq!(found.content, "Updated content");
        assert_eq!(found.created_at, Timestamp::from(1001u64));

        // Verify the group cache still has exactly 1 message (no duplicates)
        {
            let inner = storage.inner.read();
            let cache = &inner.messages_by_group_cache;
            let group_messages = cache.peek(&group_id).unwrap();
            assert_eq!(
                group_messages.len(),
                1,
                "Should have exactly 1 message after update, not 2"
            );
            assert_eq!(
                group_messages.get(&event_id).unwrap().content,
                "Updated content"
            );
        }
    }

    /// Test that messages are properly isolated between different groups
    #[test]
    fn test_save_message_multiple_groups() {
        let storage = MdkMemoryStorage::new();

        let group1_id = GroupId::from_slice(&[1, 1, 1, 1]);
        let group2_id = GroupId::from_slice(&[2, 2, 2, 2]);

        // Create the groups first
        let group1 = create_test_group(group1_id.clone());
        storage.save_group(group1).unwrap();
        let group2 = create_test_group(group2_id.clone());
        storage.save_group(group2).unwrap();

        // Save messages to group 1
        for i in 0..3 {
            let event_id = EventId::from_slice(&[i as u8; 32]).unwrap();
            let message = create_test_message(
                event_id,
                group1_id.clone(),
                &format!("Group1 Message {}", i),
                1000 + i as u64,
            );
            storage.save_message(message).unwrap();
        }

        // Save messages to group 2
        for i in 0..5 {
            let event_id = EventId::from_slice(&[100 + i as u8; 32]).unwrap();
            let message = create_test_message(
                event_id,
                group2_id.clone(),
                &format!("Group2 Message {}", i),
                2000 + i as u64,
            );
            storage.save_message(message).unwrap();
        }

        // Verify group 1 has 3 messages
        {
            let inner = storage.inner.read();
            let cache = &inner.messages_by_group_cache;
            let group1_messages = cache.peek(&group1_id).unwrap();
            assert_eq!(group1_messages.len(), 3);
        }

        // Verify group 2 has 5 messages
        {
            let inner = storage.inner.read();
            let cache = &inner.messages_by_group_cache;
            let group2_messages = cache.peek(&group2_id).unwrap();
            assert_eq!(group2_messages.len(), 5);
        }

        // Verify messages are correctly associated with their groups
        let event_id_group1 = EventId::from_slice(&[0u8; 32]).unwrap();
        let found = storage
            .find_message_by_event_id(&group1_id, &event_id_group1)
            .unwrap()
            .unwrap();
        assert_eq!(found.mls_group_id, group1_id);
        assert!(found.content.contains("Group1"));

        let event_id_group2 = EventId::from_slice(&[100u8; 32]).unwrap();
        let found = storage
            .find_message_by_event_id(&group2_id, &event_id_group2)
            .unwrap()
            .unwrap();
        assert_eq!(found.mls_group_id, group2_id);
        assert!(found.content.contains("Group2"));
    }

    /// Test that multiple updates to the same message work correctly
    #[test]
    fn test_save_message_multiple_updates() {
        let storage = MdkMemoryStorage::new();

        let group_id = GroupId::from_slice(&[1, 2, 3, 4]);
        let event_id = EventId::from_slice(&[50u8; 32]).unwrap();

        // Create the group first
        let group = create_test_group(group_id.clone());
        storage.save_group(group).unwrap();

        // Perform multiple updates to the same message
        for i in 0..10 {
            let message = create_test_message(
                event_id,
                group_id.clone(),
                &format!("Version {}", i),
                1000 + i as u64,
            );
            storage.save_message(message).unwrap();
        }

        // Verify only the final version exists
        let found = storage
            .find_message_by_event_id(&group_id, &event_id)
            .unwrap()
            .unwrap();
        assert_eq!(found.content, "Version 9");

        // Verify the group cache has exactly 1 message
        {
            let inner = storage.inner.read();
            let cache = &inner.messages_by_group_cache;
            let group_messages = cache.peek(&group_id).unwrap();
            assert_eq!(
                group_messages.len(),
                1,
                "Should have exactly 1 message after 10 updates"
            );
        }
    }

    /// Test that updating message state works correctly
    #[test]
    fn test_save_message_state_update() {
        let storage = MdkMemoryStorage::new();

        let group_id = GroupId::from_slice(&[1, 2, 3, 4]);
        let event_id = EventId::from_slice(&[75u8; 32]).unwrap();

        // Create the group first
        let group = create_test_group(group_id.clone());
        storage.save_group(group).unwrap();

        // Save message with Created state
        let mut message = create_test_message(event_id, group_id.clone(), "Test content", 1000);
        message.state = MessageState::Created;
        storage.save_message(message).unwrap();

        // Verify initial state
        let found = storage
            .find_message_by_event_id(&group_id, &event_id)
            .unwrap()
            .unwrap();
        assert_eq!(found.state, MessageState::Created);

        // Update message with Processed state
        let mut message = create_test_message(event_id, group_id.clone(), "Test content", 1000);
        message.state = MessageState::Processed;
        storage.save_message(message).unwrap();

        // Verify state was updated
        let found = storage
            .find_message_by_event_id(&group_id, &event_id)
            .unwrap()
            .unwrap();
        assert_eq!(found.state, MessageState::Processed);

        // Verify still only 1 message in the group
        {
            let inner = storage.inner.read();
            let cache = &inner.messages_by_group_cache;
            let group_messages = cache.peek(&group_id).unwrap();
            assert_eq!(group_messages.len(), 1);
        }
    }

    /// Test that the messages per group limit is enforced and oldest messages are evicted.
    /// This test verifies that:
    /// 1. When max_messages_per_group is reached, the oldest message is evicted
    /// 2. Evicted messages are removed from BOTH caches (messages_cache and messages_by_group_cache)
    /// 3. Updates to existing messages don't trigger eviction
    #[test]
    fn test_save_message_per_group_limit_eviction() {
        use crate::{DEFAULT_MAX_MESSAGES_PER_GROUP, ValidationLimits};

        // Create storage with a large cache size for testing
        let limits = ValidationLimits::default().with_cache_size(20000);
        let storage = MdkMemoryStorage::with_limits(limits);

        let group_id = GroupId::from_slice(&[1, 2, 3, 4]);

        // Create the group first
        let group = create_test_group(group_id.clone());
        storage.save_group(group).unwrap();

        // Save exactly DEFAULT_MAX_MESSAGES_PER_GROUP messages
        // Use timestamps 1000..1000+MAX to establish age ordering
        for i in 0..DEFAULT_MAX_MESSAGES_PER_GROUP {
            let mut event_bytes = [0u8; 32];
            event_bytes[0] = (i % 256) as u8;
            event_bytes[1] = ((i / 256) % 256) as u8;
            event_bytes[2] = ((i / 65536) % 256) as u8;
            let event_id = EventId::from_slice(&event_bytes).unwrap();
            let message = create_test_message(
                event_id,
                group_id.clone(),
                &format!("Message {}", i),
                1000 + i as u64, // Oldest message has timestamp 1000
            );
            storage.save_message(message).unwrap();
        }

        // Verify all messages are stored
        {
            let inner = storage.inner.read();
            let cache = &inner.messages_by_group_cache;
            let group_messages = cache.peek(&group_id).unwrap();
            assert_eq!(group_messages.len(), DEFAULT_MAX_MESSAGES_PER_GROUP);
        }

        // The oldest message (index 0, timestamp 1000) should exist
        let oldest_event_id = EventId::from_slice(&[0u8; 32]).unwrap();
        {
            let found = storage
                .find_message_by_event_id(&group_id, &oldest_event_id)
                .unwrap();
            assert!(
                found.is_some(),
                "Oldest message should exist before eviction"
            );
        }

        // Now add one more message to trigger eviction
        let new_event_bytes = [255u8; 32]; // Unique event ID
        let new_event_id = EventId::from_slice(&new_event_bytes).unwrap();
        let new_message = create_test_message(
            new_event_id,
            group_id.clone(),
            "New message triggering eviction",
            999999, // Much newer timestamp
        );
        storage.save_message(new_message).unwrap();

        // Verify the count is still at MAX (eviction occurred)
        {
            let inner = storage.inner.read();
            let cache = &inner.messages_by_group_cache;
            let group_messages = cache.peek(&group_id).unwrap();
            assert_eq!(
                group_messages.len(),
                DEFAULT_MAX_MESSAGES_PER_GROUP,
                "Should still have DEFAULT_MAX_MESSAGES_PER_GROUP after eviction"
            );
        }

        // The oldest message should have been evicted from messages_by_group_cache
        {
            let found = storage
                .find_message_by_event_id(&group_id, &oldest_event_id)
                .unwrap();
            assert!(
                found.is_none(),
                "Oldest message should be evicted from messages_by_group_cache"
            );
        }

        // CRITICAL: The oldest message should ALSO be evicted from messages_cache
        // This verifies the coordinated eviction fix
        {
            let inner = storage.inner.read();
            let cache = &inner.messages_cache;
            assert!(
                !cache.contains(&oldest_event_id),
                "Oldest message should be evicted from messages_cache too (no orphaned entries)"
            );
        }

        // The new message should exist
        {
            let found = storage
                .find_message_by_event_id(&group_id, &new_event_id)
                .unwrap();
            assert!(found.is_some(), "New message should exist after eviction");
            assert_eq!(found.unwrap().content, "New message triggering eviction");
        }

        // Verify updating an existing message doesn't trigger eviction
        // Update the second oldest message (index 1)
        let mut update_event_bytes = [0u8; 32];
        update_event_bytes[0] = 1;
        let update_event_id = EventId::from_slice(&update_event_bytes).unwrap();
        let update_message =
            create_test_message(update_event_id, group_id.clone(), "Updated Message 1", 2000);
        storage.save_message(update_message).unwrap();

        // Should still have the same count (no eviction for updates)
        {
            let inner = storage.inner.read();
            let cache = &inner.messages_by_group_cache;
            let group_messages = cache.peek(&group_id).unwrap();
            assert_eq!(
                group_messages.len(),
                DEFAULT_MAX_MESSAGES_PER_GROUP,
                "Update should not change message count"
            );
        }

        // Verify the message was updated
        let found = storage
            .find_message_by_event_id(&group_id, &update_event_id)
            .unwrap()
            .unwrap();
        assert_eq!(found.content, "Updated Message 1");
    }

    /// Test that custom validation limits work correctly
    #[test]
    fn test_custom_message_limit() {
        use crate::ValidationLimits;

        // Create storage with a custom small message limit for testing
        let custom_limit = 5;
        let limits = ValidationLimits::default()
            .with_cache_size(100)
            .with_max_messages_per_group(custom_limit);
        let storage = MdkMemoryStorage::with_limits(limits);

        let group_id = GroupId::from_slice(&[1, 2, 3, 4]);

        // Create the group first
        let group = create_test_group(group_id.clone());
        storage.save_group(group).unwrap();

        // Save exactly custom_limit messages
        for i in 0..custom_limit {
            let mut event_bytes = [0u8; 32];
            event_bytes[0] = i as u8;
            let event_id = EventId::from_slice(&event_bytes).unwrap();
            let message = create_test_message(
                event_id,
                group_id.clone(),
                &format!("Message {}", i),
                1000 + i as u64,
            );
            storage.save_message(message).unwrap();
        }

        // Verify all messages are stored
        {
            let inner = storage.inner.read();
            let cache = &inner.messages_by_group_cache;
            let group_messages = cache.peek(&group_id).unwrap();
            assert_eq!(group_messages.len(), custom_limit);
        }

        // Add one more message to trigger eviction
        let new_event_id = EventId::from_slice(&[255u8; 32]).unwrap();
        let new_message = create_test_message(
            new_event_id,
            group_id.clone(),
            "New message triggering eviction",
            999999,
        );
        storage.save_message(new_message).unwrap();

        // Verify the count is still at custom_limit (eviction occurred)
        {
            let inner = storage.inner.read();
            let cache = &inner.messages_by_group_cache;
            let group_messages = cache.peek(&group_id).unwrap();
            assert_eq!(group_messages.len(), custom_limit);
        }

        // The oldest message (index 0) should have been evicted
        let oldest_event_id = EventId::from_slice(&[0u8; 32]).unwrap();
        {
            let found = storage
                .find_message_by_event_id(&group_id, &oldest_event_id)
                .unwrap();
            assert!(found.is_none(), "Oldest message should be evicted");
        }
    }

    #[test]
    fn test_mark_processed_message_retryable() {
        use mdk_storage_traits::messages::types::ProcessedMessage;

        let storage = MdkMemoryStorage::new();

        // Create a failed processed message
        let wrapper_event_id = EventId::from_slice(&[100u8; 32]).unwrap();

        let processed_message = ProcessedMessage {
            wrapper_event_id,
            message_event_id: None,
            processed_at: Timestamp::from(1_000_000_000u64),
            epoch: None,
            mls_group_id: Some(GroupId::from_slice(&[1, 2, 3, 4])),
            state: ProcessedMessageState::Failed,
            failure_reason: Some("Decryption failed".to_string()),
        };

        // Save the failed processed message
        storage
            .save_processed_message(processed_message)
            .expect("Failed to save processed message");

        // Verify it's in Failed state
        let found = storage
            .find_processed_message_by_event_id(&wrapper_event_id)
            .unwrap()
            .unwrap();
        assert_eq!(found.state, ProcessedMessageState::Failed);

        // Mark as retryable
        storage
            .mark_processed_message_retryable(&wrapper_event_id)
            .expect("Failed to mark message as retryable");

        // Verify state changed to Retryable
        let found = storage
            .find_processed_message_by_event_id(&wrapper_event_id)
            .unwrap()
            .unwrap();
        assert_eq!(found.state, ProcessedMessageState::Retryable);

        // Verify failure_reason is preserved
        assert_eq!(found.failure_reason, Some("Decryption failed".to_string()));
    }

    #[test]
    fn test_mark_nonexistent_message_retryable_fails() {
        use mdk_storage_traits::messages::error::MessageError;

        let storage = MdkMemoryStorage::new();

        let wrapper_event_id = EventId::from_slice(&[100u8; 32]).unwrap();

        // Attempt to mark a non-existent message as retryable
        let result = storage.mark_processed_message_retryable(&wrapper_event_id);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), MessageError::NotFound));
    }

    #[test]
    fn test_mark_non_failed_message_retryable_fails() {
        use mdk_storage_traits::messages::error::MessageError;
        use mdk_storage_traits::messages::types::ProcessedMessage;

        let storage = MdkMemoryStorage::new();

        // Create a processed message in Processed state (not Failed)
        let wrapper_event_id = EventId::from_slice(&[100u8; 32]).unwrap();

        let processed_message = ProcessedMessage {
            wrapper_event_id,
            message_event_id: None,
            processed_at: Timestamp::from(1_000_000_000u64),
            epoch: Some(1),
            mls_group_id: Some(GroupId::from_slice(&[1, 2, 3, 4])),
            state: ProcessedMessageState::Processed,
            failure_reason: None,
        };

        storage
            .save_processed_message(processed_message)
            .expect("Failed to save processed message");

        // Attempt to mark a Processed message as retryable should fail
        let result = storage.mark_processed_message_retryable(&wrapper_event_id);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), MessageError::NotFound));

        // Verify state is unchanged
        let found = storage
            .find_processed_message_by_event_id(&wrapper_event_id)
            .unwrap()
            .unwrap();
        assert_eq!(found.state, ProcessedMessageState::Processed);
    }

    /// Verifies that querying a group with no stored messages returns Ok(None).
    #[test]
    fn test_find_message_epoch_by_tag_content_unknown_group() {
        let storage = MdkMemoryStorage::new();
        let unknown_group_id = GroupId::from_slice(&[99, 99, 99, 99]);

        let result = storage
            .find_message_epoch_by_tag_content(&unknown_group_id, "x abcdef")
            .unwrap();

        assert_eq!(result, None);
    }

    /// Verifies that when messages exist but none contain the searched
    /// substring, Ok(None) is returned.
    #[test]
    fn test_find_message_epoch_by_tag_content_no_matching_tag() {
        let storage = MdkMemoryStorage::new();
        let group_id = GroupId::from_slice(&[1, 2, 3, 4]);

        let group = create_test_group(group_id.clone());
        storage.save_group(group).unwrap();

        let event_id = EventId::from_slice(&[10u8; 32]).unwrap();
        let message = create_test_message_with_epoch(
            event_id,
            group_id.clone(),
            "some content",
            1000,
            Some(5),
        );
        storage.save_message(message).unwrap();

        let result = storage
            .find_message_epoch_by_tag_content(&group_id, "x deadbeef_not_present")
            .unwrap();

        assert_eq!(result, None);
    }

    /// Verifies the happy path: a message with matching tag content and a
    /// non-null epoch returns Ok(Some(epoch)).
    #[test]
    fn test_find_message_epoch_by_tag_content_happy_path() {
        let storage = MdkMemoryStorage::new();
        let group_id = GroupId::from_slice(&[1, 2, 3, 4]);

        let group = create_test_group(group_id.clone());
        storage.save_group(group).unwrap();

        let event_id = EventId::from_slice(&[10u8; 32]).unwrap();
        let pubkey = Keys::generate().public_key();
        let wrapper_event_id = EventId::from_slice(&[200u8; 32]).unwrap();

        let tags = Tags::parse(vec![vec!["imeta", "x abcdef123456"]]).unwrap();
        let message = Message {
            id: event_id,
            pubkey,
            kind: Kind::from(445u16),
            mls_group_id: group_id.clone(),
            created_at: Timestamp::from(1000u64),
            processed_at: Timestamp::from(1000u64),
            content: "".to_string(),
            tags: tags.clone(),
            event: UnsignedEvent::new(
                pubkey,
                Timestamp::from(1000u64),
                Kind::from(445u16),
                tags,
                "".to_string(),
            ),
            wrapper_event_id,
            epoch: Some(7),
            state: MessageState::Processed,
        };
        storage.save_message(message).unwrap();

        let result = storage
            .find_message_epoch_by_tag_content(&group_id, "x abcdef123456")
            .unwrap();

        assert_eq!(result, Some(7));
    }

    /// Verifies that messages with epoch: None are skipped even when their
    /// tags match the search substring.
    #[test]
    fn test_find_message_epoch_by_tag_content_skips_null_epoch() {
        let storage = MdkMemoryStorage::new();
        let group_id = GroupId::from_slice(&[1, 2, 3, 4]);

        let group = create_test_group(group_id.clone());
        storage.save_group(group).unwrap();

        let event_id = EventId::from_slice(&[10u8; 32]).unwrap();
        let pubkey = Keys::generate().public_key();
        let wrapper_event_id = EventId::from_slice(&[200u8; 32]).unwrap();

        // Store a message with matching tags but epoch: None
        let tags = Tags::parse(vec![vec!["imeta", "x abcdef123456"]]).unwrap();
        let message = Message {
            id: event_id,
            pubkey,
            kind: Kind::from(445u16),
            mls_group_id: group_id.clone(),
            created_at: Timestamp::from(1000u64),
            processed_at: Timestamp::from(1000u64),
            content: "".to_string(),
            tags: tags.clone(),
            event: UnsignedEvent::new(
                pubkey,
                Timestamp::from(1000u64),
                Kind::from(445u16),
                tags,
                "".to_string(),
            ),
            wrapper_event_id,
            epoch: None,
            state: MessageState::Processed,
        };
        storage.save_message(message).unwrap();

        let result = storage
            .find_message_epoch_by_tag_content(&group_id, "x abcdef123456")
            .unwrap();

        assert_eq!(result, None);
    }
}
