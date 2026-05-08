//! Application message processing
//!
//! This module handles processing of application messages (chat messages) from group members.

use mdk_storage_traits::MdkStorageProvider;
use mdk_storage_traits::groups::types as group_types;
use mdk_storage_traits::messages::types as message_types;
use nostr::{Event, EventId, JsonUtil, Timestamp, UnsignedEvent};
use openmls::prelude::ApplicationMessage;

use crate::MDK;

use super::Result;

impl<Storage> MDK<Storage>
where
    Storage: MdkStorageProvider,
{
    /// Processes an application message from a group member
    ///
    /// This internal function handles application messages (chat messages) that have been
    /// successfully decrypted. It:
    /// 1. Deserializes the message content as a Nostr event
    /// 2. Verifies the rumor pubkey matches the MLS sender credential (author binding)
    /// 3. Creates tracking records for the message and processing state
    /// 4. Updates the group's last message metadata
    /// 5. Stores all data in the storage provider
    ///
    /// # Arguments
    ///
    /// * `group` - The group metadata from storage
    /// * `mls_epoch` - The current epoch from the MLS group (authoritative source)
    /// * `event` - The wrapper Nostr event containing the encrypted message
    /// * `application_message` - The decrypted MLS application message
    /// * `sender_credential` - The MLS credential of the sender for author verification
    ///
    /// # Returns
    ///
    /// * `Ok(Message)` - The processed and stored message
    /// * `Err(Error)` - If message processing, author verification, or storage fails
    pub(super) fn process_application_message(
        &self,
        mut group: group_types::Group,
        mls_epoch: u64,
        event: &Event,
        application_message: ApplicationMessage,
        sender_credential: openmls::credentials::Credential,
    ) -> Result<message_types::Message> {
        // This is a message from a group member
        let bytes = application_message.into_bytes();
        let mut rumor: UnsignedEvent = UnsignedEvent::from_json(bytes)?;

        self.verify_rumor_author(&rumor.pubkey, sender_credential)?;

        let rumor_id: EventId = rumor.id();

        let processed_message = super::create_processed_message_record(
            event.id,
            Some(rumor_id),
            Some(mls_epoch),
            Some(group.mls_group_id.clone()),
            message_types::ProcessedMessageState::Processed,
            None,
        );

        let now = Timestamp::now();
        let message = message_types::Message {
            id: rumor_id,
            pubkey: rumor.pubkey,
            kind: rumor.kind,
            mls_group_id: group.mls_group_id.clone(),
            created_at: rumor.created_at,
            processed_at: now,
            content: rumor.content.clone(),
            tags: rumor.tags.clone(),
            event: rumor.clone(),
            wrapper_event_id: event.id,
            state: message_types::MessageState::Processed,
            epoch: Some(mls_epoch),
        };

        self.save_message_record(message.clone())?;
        self.save_processed_message_record(processed_message.clone())?;

        // Update last_message_at, last_message_processed_at, and last_message_id only if this
        // message should appear first in get_messages(). Delegates to the centralized
        // Group::update_last_message_if_newer which uses the canonical display ordering
        // (`created_at DESC, processed_at DESC, id DESC`).
        if group.update_last_message_if_newer(&message) {
            self.save_group_record(group)?;
        }

        tracing::debug!(
            target: "mdk_core::messages::process_message",
            "Processed application message"
        );
        Ok(message)
    }
}

#[cfg(test)]
mod tests {
    use mdk_storage_traits::messages::MessageStorage;
    use mdk_storage_traits::messages::types as message_types;
    use nostr::{Keys, Kind};

    use crate::messages::MessageProcessingResult;
    use crate::test_util::*;
    use crate::tests::create_test_mdk;

    #[test]
    fn test_message_state_tracking() {
        let mdk = create_test_mdk();
        let (creator, members, admins) = create_test_group_members();
        let group_id = create_test_group(&mdk, &creator, &members, &admins);

        // Create a message
        let mut rumor = create_test_rumor(&creator, "Test message state");
        let rumor_id = rumor.id();

        let event = mdk
            .create_message(&group_id, rumor)
            .expect("Failed to create message");

        // Verify initial state
        let message = mdk
            .get_message(&group_id, &rumor_id)
            .expect("Failed to get message")
            .expect("Message should exist");

        assert_eq!(message.state, message_types::MessageState::Created);

        // Verify processed message state
        let processed_message = mdk
            .storage()
            .find_processed_message_by_event_id(&event.id)
            .expect("Failed to get processed message")
            .expect("Processed message should exist");

        assert_eq!(
            processed_message.state,
            message_types::ProcessedMessageState::Created
        );
        assert_eq!(processed_message.message_event_id, Some(rumor_id));
        assert_eq!(processed_message.wrapper_event_id, event.id);
    }

    /// Test message state transitions
    #[test]
    fn test_message_state_transitions() {
        let mdk = create_test_mdk();
        let (creator, members, admins) = create_test_group_members();
        let group_id = create_test_group(&mdk, &creator, &members, &admins);

        // Create a message
        let mut rumor = create_test_rumor(&creator, "Test message");
        let rumor_id = rumor.id();
        let _event = mdk
            .create_message(&group_id, rumor)
            .expect("Failed to create message");

        // Check initial state
        let message = mdk
            .get_message(&group_id, &rumor_id)
            .expect("Failed to get message")
            .expect("Message should exist");
        assert_eq!(
            message.state,
            message_types::MessageState::Created,
            "Initial state should be Created"
        );

        // Process the message (simulating receiving it)
        // In a real scenario, another client would process this
        // For this test, we verify the state tracking works
        assert_eq!(message.content, "Test message");
        assert_eq!(message.pubkey, creator.public_key());
    }

    /// Test message from non-member
    #[test]
    fn test_message_from_non_member() {
        let creator_mdk = create_test_mdk();
        let (creator, members, admins) = create_test_group_members();

        // Create group
        let group_id = create_test_group(&creator_mdk, &creator, &members, &admins);

        // Create a message from someone not in the group
        let non_member = Keys::generate();
        let rumor = create_test_rumor(&non_member, "I'm not in this group");

        // Try to create a message (this would fail at the MLS level)
        // In practice, a non-member wouldn't have the group loaded
        let non_member_mdk = create_test_mdk();
        let result = non_member_mdk.create_message(&group_id, rumor);

        // Should fail because the group doesn't exist for this user
        assert!(
            result.is_err(),
            "Non-member should not be able to create messages"
        );
    }

    /// Message from non-member handling
    ///
    /// Tests that messages from non-members are properly rejected.
    ///
    /// Requirements tested:
    /// - Messages from non-members rejected
    /// - Clear error indicating sender not in group
    /// - No state corruption from unauthorized messages
    #[test]
    fn test_message_from_non_member_rejected() {
        // Create Alice (admin) and Bob (member)
        let alice_keys = Keys::generate();
        let bob_keys = Keys::generate();
        let charlie_keys = Keys::generate(); // Not a member

        let alice_mdk = create_test_mdk();
        let bob_mdk = create_test_mdk();
        let charlie_mdk = create_test_mdk();

        let admins = vec![alice_keys.public_key()];

        // Bob creates his key package
        let bob_key_package = create_key_package_event(&bob_mdk, &bob_keys);

        // Alice creates group with only Bob (Charlie is excluded)
        let create_result = alice_mdk
            .create_group(
                &alice_keys.public_key(),
                vec![bob_key_package],
                create_nostr_group_config_data(admins),
            )
            .expect("Alice should be able to create group");

        let group_id = create_result.group.mls_group_id.clone();

        alice_mdk
            .merge_pending_commit(&group_id)
            .expect("Failed to merge Alice's create commit");

        // Bob processes and accepts welcome
        let bob_welcome_rumor = &create_result.welcome_rumors[0];

        let bob_welcome = bob_mdk
            .process_welcome(&nostr::EventId::all_zeros(), bob_welcome_rumor)
            .expect("Bob should be able to process welcome");

        bob_mdk
            .accept_welcome(&bob_welcome)
            .expect("Bob should be able to accept welcome");

        // Verify initial member list (should be Alice and Bob only)
        let members = alice_mdk
            .get_members(&group_id)
            .expect("Failed to get members");
        assert_eq!(members.len(), 2, "Group should have 2 members");
        assert!(
            !members.contains(&charlie_keys.public_key()),
            "Charlie should not be a member"
        );

        // Charlie (non-member) attempts to send a message to the group
        // This should fail because Charlie doesn't have the group loaded
        let charlie_rumor = create_test_rumor(&charlie_keys, "Unauthorized message");
        let charlie_message_result = charlie_mdk.create_message(&group_id, charlie_rumor);

        assert!(
            charlie_message_result.is_err(),
            "Non-member should not be able to create message for group"
        );

        // Verify the error is GroupNotFound (Charlie doesn't have access)
        assert!(
            matches!(charlie_message_result, Err(crate::Error::GroupNotFound)),
            "Should return GroupNotFound error for non-member"
        );

        // Verify group state is unchanged
        let final_members = alice_mdk
            .get_members(&group_id)
            .expect("Failed to get members");
        assert_eq!(
            final_members.len(),
            2,
            "Member count should remain unchanged"
        );
    }

    /// Test multi-client message synchronization (MIP-03)
    ///
    /// This test validates that messages can be properly synchronized across multiple
    /// clients and that epoch lookback mechanisms work correctly.
    ///
    /// Requirements tested:
    /// - Messages decrypt across all clients
    /// - Epoch lookback mechanism works
    /// - Historical message processing across epochs
    /// - State convergence across clients
    #[test]
    fn test_multi_client_message_synchronization() {
        // Setup: Create Alice and Bob as admins
        let alice_keys = Keys::generate();
        let bob_keys = Keys::generate();

        let alice_mdk = create_test_mdk();
        let bob_mdk = create_test_mdk();

        let admins = vec![alice_keys.public_key(), bob_keys.public_key()];

        // Step 1: Bob creates his key package in his own MDK
        let bob_key_package = create_key_package_event(&bob_mdk, &bob_keys);

        // Alice creates the group and adds Bob
        let create_result = alice_mdk
            .create_group(
                &alice_keys.public_key(),
                vec![bob_key_package],
                create_nostr_group_config_data(admins),
            )
            .expect("Alice should be able to create group");

        let group_id = create_result.group.mls_group_id.clone();

        alice_mdk
            .merge_pending_commit(&group_id)
            .expect("Failed to merge Alice's create commit");

        // Bob processes and accepts welcome to join the group
        let bob_welcome_rumor = &create_result.welcome_rumors[0];
        let bob_welcome = bob_mdk
            .process_welcome(&nostr::EventId::all_zeros(), bob_welcome_rumor)
            .expect("Bob should be able to process welcome");

        bob_mdk
            .accept_welcome(&bob_welcome)
            .expect("Bob should be able to accept welcome");

        // Verify both clients have the same group ID
        assert_eq!(
            group_id, bob_welcome.mls_group_id,
            "Alice and Bob should have the same group ID"
        );

        // Step 2: Alice sends a message in epoch 0
        let rumor1 = create_test_rumor(&alice_keys, "Hello from Alice");
        let msg_event1 = alice_mdk
            .create_message(&group_id, rumor1)
            .expect("Alice should be able to send message");

        assert_eq!(msg_event1.kind, Kind::MlsGroupMessage);

        // Bob processes Alice's message
        let bob_process1 = bob_mdk
            .process_message(&msg_event1)
            .expect("Bob should be able to process Alice's message");

        // Verify Bob decrypted the message
        match bob_process1 {
            MessageProcessingResult::ApplicationMessage(msg) => {
                assert_eq!(msg.content, "Hello from Alice");
            }
            _ => panic!("Expected ApplicationMessage but got different result type"),
        }

        // Step 3: Advance epoch with Alice's update
        let update_result = alice_mdk
            .self_update(&group_id)
            .expect("Alice should be able to create update");

        // Both clients process the update
        let _alice_process_update = alice_mdk
            .process_message(&update_result.evolution_event)
            .expect("Alice should process her update");

        alice_mdk
            .merge_pending_commit(&group_id)
            .expect("Alice should merge update");

        let _bob_process_update = bob_mdk
            .process_message(&update_result.evolution_event)
            .expect("Bob should process Alice's update");

        // Step 4: Alice sends message in new epoch
        let rumor2 = create_test_rumor(&alice_keys, "Message in epoch 1");
        let msg_event2 = alice_mdk
            .create_message(&group_id, rumor2)
            .expect("Alice should send message in new epoch");

        // Bob processes message from new epoch
        let bob_process2 = bob_mdk
            .process_message(&msg_event2)
            .expect("Bob should process message from epoch 1");

        match bob_process2 {
            MessageProcessingResult::ApplicationMessage(msg) => {
                assert_eq!(msg.content, "Message in epoch 1");
            }
            _ => panic!("Expected ApplicationMessage but got different result type"),
        }

        // Step 5: Bob sends a message
        let rumor3 = create_test_rumor(&bob_keys, "Hello from Bob");
        let msg_event3 = bob_mdk
            .create_message(&group_id, rumor3)
            .expect("Bob should be able to send message");

        // Alice processes Bob's message
        let alice_process3 = alice_mdk
            .process_message(&msg_event3)
            .expect("Alice should process Bob's message");

        match alice_process3 {
            MessageProcessingResult::ApplicationMessage(msg) => {
                assert_eq!(msg.content, "Hello from Bob");
            }
            _ => panic!("Expected ApplicationMessage but got different result type"),
        }

        // Step 6: Verify state convergence - both clients should be in same epoch
        let alice_final_epoch = alice_mdk
            .get_group(&group_id)
            .expect("Failed to get Alice's group")
            .expect("Alice's group should exist")
            .epoch;

        let bob_final_epoch = bob_mdk
            .get_group(&group_id)
            .expect("Failed to get Bob's group")
            .expect("Bob's group should exist")
            .epoch;

        assert_eq!(
            alice_final_epoch, bob_final_epoch,
            "Both clients should be in the same epoch"
        );

        // Step 7: Verify all messages are stored on both clients
        let alice_messages = alice_mdk
            .get_messages(&group_id, None)
            .expect("Failed to get Alice's messages");

        let bob_messages = bob_mdk
            .get_messages(&group_id, None)
            .expect("Failed to get Bob's messages");

        assert_eq!(alice_messages.len(), 3, "Alice should have 3 messages");
        assert_eq!(bob_messages.len(), 3, "Bob should have 3 messages");

        // Note: When timestamps are equal (as in fast tests), sort order by ID is deterministic
        // but not chronological. We verify all messages are present.
        let alice_contents: Vec<&str> = alice_messages.iter().map(|m| m.content.as_str()).collect();
        let bob_contents: Vec<&str> = bob_messages.iter().map(|m| m.content.as_str()).collect();

        assert!(alice_contents.contains(&"Hello from Alice"));
        assert!(alice_contents.contains(&"Message in epoch 1"));
        assert!(alice_contents.contains(&"Hello from Bob"));

        assert!(bob_contents.contains(&"Hello from Alice"));
        assert!(bob_contents.contains(&"Message in epoch 1"));
        assert!(bob_contents.contains(&"Hello from Bob"));

        // The test confirms that:
        // - Messages are properly encrypted and decrypted across clients
        // - Messages can be processed across epoch transitions
        // - Both clients maintain synchronized state
        // - Message history is consistent across all clients
    }
}
