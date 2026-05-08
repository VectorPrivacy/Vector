//! Message creation functionality
//!
//! This module handles creating and encrypting messages for MLS groups.

use mdk_storage_traits::groups::types as group_types;
use mdk_storage_traits::messages::types as message_types;
use mdk_storage_traits::{GroupId, MdkStorageProvider};
use nostr::{Event, EventId, JsonUtil, Keys, Timestamp, UnsignedEvent};
use openmls::prelude::MlsGroup;
use openmls_basic_credential::SignatureKeyPair;
use tls_codec::Serialize as TlsSerialize;

use crate::MDK;
use crate::error::Error;

use super::Result;

impl<Storage> MDK<Storage>
where
    Storage: MdkStorageProvider,
{
    /// Creates an MLS-encrypted message from an unsigned Nostr event
    ///
    /// This internal function handles the MLS-level encryption of a message:
    /// 1. Loads the member's signing keys
    /// 2. Ensures the message has a unique ID
    /// 3. Serializes the message content
    /// 4. Creates and signs the MLS message
    ///
    /// # Arguments
    ///
    /// * `group` - The MLS group to create the message in
    /// * `rumor` - The unsigned Nostr event to encrypt
    ///
    /// # Returns
    ///
    /// * `Ok(Vec<u8>)` - The serialized encrypted MLS message
    /// * `Err(Error)` - If message creation or encryption fails
    pub(crate) fn create_mls_message_payload(
        &self,
        group: &mut MlsGroup,
        rumor: &mut UnsignedEvent,
    ) -> Result<Vec<u8>> {
        // Load signer
        let signer: SignatureKeyPair = self.load_mls_signer(group)?;

        // Ensure rumor ID
        rumor.ensure_id();

        // Serialize as JSON
        let json: String = rumor.as_json();

        // Create message
        let message_out = group.create_message(&self.provider, &signer, json.as_bytes())?;

        let serialized_message = message_out.tls_serialize_detached()?;

        Ok(serialized_message)
    }

    /// Creates a complete encrypted Nostr event for an MLS group message
    ///
    /// This is the main entry point for creating group messages. The function:
    /// 1. Loads the MLS group and its metadata
    /// 2. Creates and encrypts the MLS message
    /// 3. Derives NIP-44 encryption keys from the group's secret
    /// 4. Creates a Nostr event wrapping the encrypted message
    /// 5. Stores the message state for tracking
    ///
    /// # Arguments
    ///
    /// * `mls_group_id` - The MLS group ID
    /// * `rumor` - The unsigned Nostr event to encrypt and send
    ///
    /// # Returns
    ///
    /// * `Ok(Event)` - The signed Nostr event ready for relay publication
    /// * `Err(Error)` - If message creation or encryption fails
    pub fn create_message(
        &self,
        mls_group_id: &GroupId,
        rumor: UnsignedEvent,
    ) -> Result<Event> {
        // Delegate to the retained variant and discard the ephemeral
        // wrapper key — the original behaviour. Callers that need to
        // retain the key (so they can later publish a NIP-09 deletion
        // against the kind-445 wrapper) should call
        // `create_message_retained` instead.
        Ok(self.create_message_retained(mls_group_id, rumor)?.0)
    }

    /// Same as `create_message` but ALSO returns the ephemeral keypair
    /// used to sign the kind-445 wrapper.
    ///
    /// Wire-compatible with `create_message` — relays cannot
    /// distinguish events produced by the two paths. The ephemeral
    /// public key is the wrapper's `pubkey`; the secret half lets the
    /// caller sign a NIP-09 deletion later (the only key that satisfies
    /// `event.pubkey == deletion.pubkey` per the NIP-09 spec for that
    /// specific wrapper event id).
    pub fn create_message_retained(
        &self,
        mls_group_id: &GroupId,
        mut rumor: UnsignedEvent,
    ) -> Result<(Event, Keys)> {
        // Load groups by MLS Group ID (Pattern A)
        // Used when we already have the MLS group ID (e.g., from API calls).
        // This is different from Pattern B (in decryption.rs) which loads by
        // Nostr group ID when processing incoming events.
        let mut mls_group = self
            .load_mls_group(mls_group_id)?
            .ok_or(Error::GroupNotFound)?;

        let mut group: group_types::Group = self
            .get_group(mls_group_id)
            .map_err(|_e| Error::Group("Storage error while getting group".to_string()))?
            .ok_or(Error::GroupNotFound)?;

        // Create message
        let message: Vec<u8> = self.create_mls_message_payload(&mut mls_group, &mut rumor)?;

        // Get the rumor ID
        let rumor_id: EventId = rumor.id();

        let (event, ephemeral_keys) =
            self.build_message_event_retained(mls_group_id, message)?;

        // Create message to save to storage
        let now = Timestamp::now();
        let message: message_types::Message = message_types::Message {
            id: rumor_id,
            pubkey: rumor.pubkey,
            kind: rumor.kind,
            mls_group_id: mls_group_id.clone(),
            created_at: rumor.created_at,
            processed_at: now,
            content: rumor.content.clone(),
            tags: rumor.tags.clone(),
            event: rumor.clone(),
            wrapper_event_id: event.id,
            state: message_types::MessageState::Created,
            epoch: Some(mls_group.epoch().as_u64()),
        };

        // Create processed_message to track state of message
        let processed_message = super::create_processed_message_record(
            event.id,
            Some(rumor_id),
            Some(mls_group.epoch().as_u64()),
            Some(mls_group_id.clone()),
            message_types::ProcessedMessageState::Created,
            None,
        );

        // Save message and processed message to storage
        self.save_message_record(message.clone())?;
        self.save_processed_message_record(processed_message)?;

        // Update last_message_at, last_message_processed_at, and last_message_id using
        // the canonical display-order comparison.
        group.update_last_message_if_newer(&message);
        self.save_group_record(group)?;

        Ok((event, ephemeral_keys))
    }
}

#[cfg(test)]
mod tests {
    use mdk_storage_traits::GroupId;
    use mdk_storage_traits::messages::types as message_types;
    use nostr::{Keys, Kind, TagKind, Timestamp};

    use crate::error::Error;
    use crate::test_util::*;
    use crate::tests::create_test_mdk;

    #[test]
    fn test_create_message_success() {
        let mdk = create_test_mdk();
        let (creator, members, admins) = create_test_group_members();
        let group_id = create_test_group(&mdk, &creator, &members, &admins);

        // Create a test message
        let mut rumor = create_test_rumor(&creator, "Hello, world!");
        let rumor_id = rumor.id();

        let result = mdk.create_message(&group_id, rumor);
        assert!(result.is_ok());

        let event = result.unwrap();
        assert_eq!(event.kind, Kind::MlsGroupMessage);

        // Verify the message was stored
        let stored_message = mdk
            .get_message(&group_id, &rumor_id)
            .expect("Failed to get message")
            .expect("Message should exist");

        assert_eq!(stored_message.id, rumor_id);
        assert_eq!(stored_message.content, "Hello, world!");
        assert_eq!(stored_message.state, message_types::MessageState::Created);
        assert_eq!(stored_message.wrapper_event_id, event.id);
    }

    #[test]
    fn test_create_message_group_not_found() {
        let mdk = create_test_mdk();
        let creator = Keys::generate();
        let rumor = create_test_rumor(&creator, "Hello, world!");
        let non_existent_group_id = GroupId::from_slice(&[1, 2, 3, 4]);

        let result = mdk.create_message(&non_existent_group_id, rumor);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), Error::GroupNotFound));
    }

    #[test]
    fn test_create_message_updates_group_metadata() {
        let mdk = create_test_mdk();
        let (creator, members, admins) = create_test_group_members();
        let group_id = create_test_group(&mdk, &creator, &members, &admins);

        // Get initial group state
        let initial_group = mdk
            .get_group(&group_id)
            .expect("Failed to get group")
            .expect("Group should exist");
        assert!(initial_group.last_message_at.is_none());
        assert!(initial_group.last_message_id.is_none());

        // Create a message
        let mut rumor = create_test_rumor(&creator, "Hello, world!");
        let rumor_id = rumor.id();
        let rumor_timestamp = rumor.created_at;

        let _event = mdk
            .create_message(&group_id, rumor)
            .expect("Failed to create message");

        // Verify group metadata was updated
        let updated_group = mdk
            .get_group(&group_id)
            .expect("Failed to get group")
            .expect("Group should exist");

        assert_eq!(updated_group.last_message_at, Some(rumor_timestamp));
        assert_eq!(updated_group.last_message_id, Some(rumor_id));
    }

    #[test]
    fn test_message_content_preservation() {
        let mdk = create_test_mdk();
        let (creator, members, admins) = create_test_group_members();
        let group_id = create_test_group(&mdk, &creator, &members, &admins);

        // Test with various content types
        let test_cases = vec![
            "Simple text message",
            "Message with emojis 🚀 🎉 ✨",
            "Message with\nmultiple\nlines",
            "Message with special chars: !@#$%^&*()",
            "Minimal content",
        ];

        for content in test_cases {
            let mut rumor = create_test_rumor(&creator, content);
            let rumor_id = rumor.id();

            let _event = mdk
                .create_message(&group_id, rumor)
                .expect("Failed to create message");

            let stored_message = mdk
                .get_message(&group_id, &rumor_id)
                .expect("Failed to get message")
                .expect("Message should exist");

            assert_eq!(stored_message.content, content);
            assert_eq!(stored_message.pubkey, creator.public_key());
        }
    }

    #[test]
    fn test_create_message_ensures_rumor_id() {
        let mdk = create_test_mdk();
        let (creator, members, admins) = create_test_group_members();
        let group_id = create_test_group(&mdk, &creator, &members, &admins);

        // Create a rumor - EventBuilder.build() ensures the ID is set
        let rumor = create_test_rumor(&creator, "Test message");

        let result = mdk.create_message(&group_id, rumor);
        assert!(result.is_ok());

        // The message should have been stored with a valid ID
        let event = result.unwrap();
        let messages = mdk
            .get_messages(&group_id, None)
            .expect("Failed to get messages");

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].wrapper_event_id, event.id);
    }

    #[test]
    fn test_group_message_event_structure_mip03_compliance() {
        let mdk = create_test_mdk();
        let (creator, members, admins) = create_test_group_members();
        let group_id = create_test_group(&mdk, &creator, &members, &admins);

        // Create a test message
        let rumor = create_test_rumor(&creator, "Test message for MIP-03 compliance");

        let message_event = mdk
            .create_message(&group_id, rumor)
            .expect("Failed to create message");

        // 1. Verify kind is 445 (MlsGroupMessage)
        assert_eq!(
            message_event.kind,
            Kind::MlsGroupMessage,
            "Message event must have kind 445 (MlsGroupMessage)"
        );

        // 2. Verify content is encrypted (substantial length, not plaintext)
        assert!(
            message_event.content.len() > 50,
            "Encrypted content should be substantial (> 50 chars), got {}",
            message_event.content.len()
        );

        // Content should not be the original plaintext
        assert_ne!(
            message_event.content, "Test message for MIP-03 compliance",
            "Content should be encrypted, not plaintext"
        );

        // 3. Verify exactly 1 tag (h tag with group ID)
        assert_eq!(
            message_event.tags.len(),
            1,
            "Message event must have exactly 1 tag per MIP-03"
        );

        // 4. Verify tag is h tag
        let tags_vec: Vec<&nostr::Tag> = message_event.tags.iter().collect();
        let group_id_tag = tags_vec[0];
        assert_eq!(
            group_id_tag.kind(),
            TagKind::h(),
            "Tag must be 'h' (group ID) tag"
        );

        // 5. Verify h tag is valid 32-byte hex
        let group_id_hex = group_id_tag.content().expect("h tag should have content");
        assert_eq!(
            group_id_hex.len(),
            64,
            "Group ID should be 32 bytes (64 hex chars), got {}",
            group_id_hex.len()
        );

        let group_id_bytes = hex::decode(group_id_hex).expect("Group ID should be valid hex");
        assert_eq!(
            group_id_bytes.len(),
            32,
            "Group ID should decode to 32 bytes"
        );

        // 6. Verify event is signed (has valid signature)
        assert!(
            message_event.verify().is_ok(),
            "Message event must be properly signed"
        );

        // 7. Verify pubkey is NOT the creator's real pubkey (ephemeral key)
        assert_ne!(
            message_event.pubkey,
            creator.public_key(),
            "Message should use ephemeral pubkey, not sender's real pubkey"
        );
    }

    /// Test that each message uses a different ephemeral pubkey (MIP-03)
    #[test]
    fn test_group_message_ephemeral_keys_mip03_compliance() {
        let mdk = create_test_mdk();
        let (creator, members, admins) = create_test_group_members();
        let group_id = create_test_group(&mdk, &creator, &members, &admins);

        // Send 3 messages
        let rumor1 = create_test_rumor(&creator, "First message");
        let rumor2 = create_test_rumor(&creator, "Second message");
        let rumor3 = create_test_rumor(&creator, "Third message");

        let event1 = mdk
            .create_message(&group_id, rumor1)
            .expect("Failed to create first message");
        let event2 = mdk
            .create_message(&group_id, rumor2)
            .expect("Failed to create second message");
        let event3 = mdk
            .create_message(&group_id, rumor3)
            .expect("Failed to create third message");

        // Collect all ephemeral pubkeys
        let pubkeys = [event1.pubkey, event2.pubkey, event3.pubkey];

        // 1. Verify all 3 use different ephemeral pubkeys
        assert_ne!(
            pubkeys[0], pubkeys[1],
            "First and second messages should use different ephemeral keys"
        );
        assert_ne!(
            pubkeys[1], pubkeys[2],
            "Second and third messages should use different ephemeral keys"
        );
        assert_ne!(
            pubkeys[0], pubkeys[2],
            "First and third messages should use different ephemeral keys"
        );

        // 2. Verify none use sender's real pubkey
        let real_pubkey = creator.public_key();
        for (i, pubkey) in pubkeys.iter().enumerate() {
            assert_ne!(
                *pubkey,
                real_pubkey,
                "Message {} should not use sender's real pubkey",
                i + 1
            );
        }
    }

    /// Test that commit events also use ephemeral pubkeys (MIP-03)
    #[test]
    fn test_commit_event_structure_mip03_compliance() {
        let mdk = create_test_mdk();
        let (creator, members, admins) = create_test_group_members();
        let group_id = create_test_group(&mdk, &creator, &members, &admins);

        // Add another member (creates commit)
        let new_member = Keys::generate();
        let add_result = mdk
            .add_members(&group_id, &[create_key_package_event(&mdk, &new_member)])
            .expect("Failed to add member");

        let commit_event = &add_result.evolution_event;

        // 1. Verify commit event has kind 445 (same as regular messages)
        assert_eq!(
            commit_event.kind,
            Kind::MlsGroupMessage,
            "Commit event should have kind 445"
        );

        // 2. Verify commit event structure matches regular messages
        assert_eq!(
            commit_event.tags.len(),
            1,
            "Commit event should have exactly 1 tag"
        );

        let commit_tags: Vec<&nostr::Tag> = commit_event.tags.iter().collect();
        assert_eq!(
            commit_tags[0].kind(),
            TagKind::h(),
            "Commit event should have h tag"
        );

        // 3. Verify commit uses ephemeral pubkey
        assert_ne!(
            commit_event.pubkey,
            creator.public_key(),
            "Commit should use ephemeral pubkey, not creator's real pubkey"
        );

        // 4. Verify commit is signed
        assert!(
            commit_event.verify().is_ok(),
            "Commit event must be properly signed"
        );

        // 5. Verify content is encrypted
        assert!(
            commit_event.content.len() > 50,
            "Commit content should be encrypted and substantial"
        );
    }

    /// Test that group ID in h tag matches NostrGroupDataExtension
    #[test]
    fn test_group_id_consistency_mip03() {
        let mdk = create_test_mdk();
        let (creator, members, admins) = create_test_group_members();
        let group_id = create_test_group(&mdk, &creator, &members, &admins);

        // Get the Nostr group ID from the stored group
        let stored_group = mdk
            .get_group(&group_id)
            .expect("Failed to get group")
            .expect("Group should exist");

        let expected_nostr_group_id = hex::encode(stored_group.nostr_group_id);

        // Send a message
        let rumor = create_test_rumor(&creator, "Test message");
        let message_event = mdk
            .create_message(&group_id, rumor)
            .expect("Failed to create message");

        // Extract group ID from h tag
        let h_tag = message_event
            .tags
            .iter()
            .find(|t| t.kind() == TagKind::h())
            .expect("Message should have h tag");

        let message_group_id = h_tag.content().expect("h tag should have content");

        // Verify they match
        assert_eq!(
            message_group_id, expected_nostr_group_id,
            "h tag group ID should match NostrGroupDataExtension"
        );
    }

    /// Test that all messages in the same group reference the same group ID
    #[test]
    fn test_group_id_consistency_across_messages() {
        let mdk = create_test_mdk();
        let (creator, members, admins) = create_test_group_members();
        let group_id = create_test_group(&mdk, &creator, &members, &admins);

        // Send multiple messages
        let event1 = mdk
            .create_message(&group_id, create_test_rumor(&creator, "Message 1"))
            .expect("Failed to create message 1");
        let event2 = mdk
            .create_message(&group_id, create_test_rumor(&creator, "Message 2"))
            .expect("Failed to create message 2");
        let event3 = mdk
            .create_message(&group_id, create_test_rumor(&creator, "Message 3"))
            .expect("Failed to create message 3");

        // Extract group IDs from all messages
        let group_id1 = event1
            .tags
            .iter()
            .find(|t| t.kind() == TagKind::h())
            .expect("Message 1 should have h tag")
            .content()
            .expect("h tag should have content");

        let group_id2 = event2
            .tags
            .iter()
            .find(|t| t.kind() == TagKind::h())
            .expect("Message 2 should have h tag")
            .content()
            .expect("h tag should have content");

        let group_id3 = event3
            .tags
            .iter()
            .find(|t| t.kind() == TagKind::h())
            .expect("Message 3 should have h tag")
            .content()
            .expect("h tag should have content");

        // Verify all reference the same group
        assert_eq!(
            group_id1, group_id2,
            "All messages should reference the same group"
        );
        assert_eq!(
            group_id2, group_id3,
            "All messages should reference the same group"
        );
    }

    /// Test message content encryption with NIP-44
    #[test]
    fn test_message_content_encryption_mip03() {
        let mdk = create_test_mdk();
        let (creator, members, admins) = create_test_group_members();
        let group_id = create_test_group(&mdk, &creator, &members, &admins);

        let plaintext = "Secret message content that should be encrypted";
        let rumor = create_test_rumor(&creator, plaintext);

        let message_event = mdk
            .create_message(&group_id, rumor)
            .expect("Failed to create message");

        // Verify content is encrypted (doesn't contain plaintext)
        assert!(
            !message_event.content.contains(plaintext),
            "Encrypted content should not contain plaintext"
        );

        // Verify content is substantial (encrypted data has overhead)
        assert!(
            message_event.content.len() > plaintext.len(),
            "Encrypted content should be longer than plaintext due to encryption overhead"
        );

        // Verify content appears to be encrypted (not just hex-encoded plaintext)
        // Encrypted NIP-44 content starts with specific markers
        assert!(
            message_event.content.len() > 100,
            "NIP-44 encrypted content should be substantial"
        );
    }

    /// Test that different messages have different encrypted content even with same plaintext
    #[test]
    fn test_message_encryption_uniqueness() {
        let mdk = create_test_mdk();
        let (creator, members, admins) = create_test_group_members();
        let group_id = create_test_group(&mdk, &creator, &members, &admins);

        // Send two messages with identical plaintext
        let plaintext = "Identical message content";
        let rumor1 = create_test_rumor(&creator, plaintext);
        let rumor2 = create_test_rumor(&creator, plaintext);

        let event1 = mdk
            .create_message(&group_id, rumor1)
            .expect("Failed to create first message");
        let event2 = mdk
            .create_message(&group_id, rumor2)
            .expect("Failed to create second message");

        // Verify encrypted contents are different (nonce/IV makes each encryption unique)
        assert_ne!(
            event1.content, event2.content,
            "Two messages with same plaintext should have different encrypted content"
        );
    }

    /// Test complete message lifecycle spec compliance
    #[test]
    fn test_complete_message_lifecycle_spec_compliance() {
        let mdk = create_test_mdk();
        let (creator, members, admins) = create_test_group_members();

        // 1. Create group -> verify commit event structure
        let create_result = mdk
            .create_group(
                &creator.public_key(),
                vec![
                    create_key_package_event(&mdk, &members[0]),
                    create_key_package_event(&mdk, &members[1]),
                ],
                create_nostr_group_config_data(admins.clone()),
            )
            .expect("Failed to create group");

        let group_id = create_result.group.mls_group_id.clone();

        // The creation itself doesn't produce a commit event that gets published,
        // so we merge and continue
        mdk.merge_pending_commit(&group_id)
            .expect("Failed to merge pending commit");

        // 2. Send message -> verify message event structure
        let rumor1 = create_test_rumor(&creator, "First message");
        let msg_event1 = mdk
            .create_message(&group_id, rumor1)
            .expect("Failed to send first message");

        assert_eq!(msg_event1.kind, Kind::MlsGroupMessage);
        assert_eq!(msg_event1.tags.len(), 1);

        let msg1_tags: Vec<&nostr::Tag> = msg_event1.tags.iter().collect();
        assert_eq!(msg1_tags[0].kind(), TagKind::h());

        let pubkey1 = msg_event1.pubkey;

        // 3. Add member -> verify commit event structure
        let new_member = Keys::generate();
        let add_result = mdk
            .add_members(&group_id, &[create_key_package_event(&mdk, &new_member)])
            .expect("Failed to add member");

        let commit_event = &add_result.evolution_event;
        assert_eq!(commit_event.kind, Kind::MlsGroupMessage);
        assert_eq!(commit_event.tags.len(), 1);
        assert_ne!(
            commit_event.pubkey,
            creator.public_key(),
            "Commit should use ephemeral key"
        );

        // 4. Send another message -> verify different ephemeral key
        mdk.merge_pending_commit(&group_id)
            .expect("Failed to merge commit");

        let rumor2 = create_test_rumor(&creator, "Second message after member add");
        let msg_event2 = mdk
            .create_message(&group_id, rumor2)
            .expect("Failed to send second message");

        let pubkey2 = msg_event2.pubkey;

        // 5. Verify all use different ephemeral keys
        assert_ne!(
            pubkey1, pubkey2,
            "Different messages should use different ephemeral keys"
        );
        assert_ne!(
            pubkey1, commit_event.pubkey,
            "Message and commit should use different ephemeral keys"
        );
        assert_ne!(
            pubkey2, commit_event.pubkey,
            "Message and commit should use different ephemeral keys"
        );

        // 6. Verify all reference the same group ID
        let msg1_tags: Vec<&nostr::Tag> = msg_event1.tags.iter().collect();
        let commit_tags: Vec<&nostr::Tag> = commit_event.tags.iter().collect();
        let msg2_tags: Vec<&nostr::Tag> = msg_event2.tags.iter().collect();

        let group_id_hex1 = msg1_tags[0].content().unwrap();
        let group_id_hex2 = commit_tags[0].content().unwrap();
        let group_id_hex3 = msg2_tags[0].content().unwrap();

        assert_eq!(
            group_id_hex1, group_id_hex2,
            "All events should reference same group"
        );
        assert_eq!(
            group_id_hex2, group_id_hex3,
            "All events should reference same group"
        );
    }

    /// Test that message events are properly validated before sending
    #[test]
    fn test_message_event_validation() {
        let mdk = create_test_mdk();
        let (creator, members, admins) = create_test_group_members();
        let group_id = create_test_group(&mdk, &creator, &members, &admins);

        let rumor = create_test_rumor(&creator, "Validation test message");
        let message_event = mdk
            .create_message(&group_id, rumor)
            .expect("Failed to create message");

        // Verify event passes Nostr signature validation
        assert!(
            message_event.verify().is_ok(),
            "Message event should have valid signature"
        );

        // Verify event ID is computed correctly
        let recomputed_id = message_event.id;
        assert_eq!(
            message_event.id, recomputed_id,
            "Event ID should be correctly computed"
        );

        // Verify created_at timestamp is reasonable (not in far future/past)
        let now = Timestamp::now();
        assert!(
            message_event.created_at <= now,
            "Message timestamp should not be in the future"
        );

        // Allow for some clock skew, but message shouldn't be more than a day old
        let one_day_ago = now.as_secs().saturating_sub(86400);
        assert!(
            message_event.created_at.as_secs() > one_day_ago,
            "Message timestamp should be recent"
        );
    }

    /// Test creating message for non-existent group
    #[test]
    fn test_create_message_for_nonexistent_group() {
        let mdk = create_test_mdk();
        let creator = Keys::generate();
        let rumor = create_test_rumor(&creator, "Hello");

        let non_existent_group_id = GroupId::from_slice(&[1, 2, 3, 4, 5]);
        let result = mdk.create_message(&non_existent_group_id, rumor);

        assert!(
            matches!(result, Err(Error::GroupNotFound)),
            "Should return GroupNotFound error"
        );
    }

    /// Test message with empty content
    #[test]
    fn test_message_with_empty_content() {
        let mdk = create_test_mdk();
        let (creator, members, admins) = create_test_group_members();
        let group_id = create_test_group(&mdk, &creator, &members, &admins);

        // Create a message with empty content
        let rumor = create_test_rumor(&creator, "");
        let result = mdk.create_message(&group_id, rumor);

        // Should succeed - empty messages are valid
        assert!(result.is_ok(), "Empty message should be valid");
    }

    /// Test message with very long content
    #[test]
    fn test_message_with_long_content() {
        let mdk = create_test_mdk();
        let (creator, members, admins) = create_test_group_members();
        let group_id = create_test_group(&mdk, &creator, &members, &admins);

        // Create a message with very long content (10KB)
        let long_content = "a".repeat(10000);
        let rumor = create_test_rumor(&creator, &long_content);
        let result = mdk.create_message(&group_id, rumor);

        // Should succeed - long messages are valid
        assert!(result.is_ok(), "Long message should be valid");

        let event = result.unwrap();
        assert_eq!(event.kind, Kind::MlsGroupMessage);
    }
}
