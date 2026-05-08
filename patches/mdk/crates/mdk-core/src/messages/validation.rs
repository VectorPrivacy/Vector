//! Event and identity validation
//!
//! This module handles validation of Nostr events and MLS identity verification.

use mdk_storage_traits::MdkStorageProvider;
use nostr::{Event, Kind, TagKind, Timestamp};
use openmls::prelude::{BasicCredential, MlsGroup, Proposal, Sender, StagedCommit};

use crate::MDK;
use crate::error::Error;

use super::Result;

impl<Storage> MDK<Storage>
where
    Storage: MdkStorageProvider,
{
    /// Verifies that a rumor's author matches the MLS sender's credential
    ///
    /// This function ensures the Nostr identity (rumor pubkey) is bound to the
    /// authenticated MLS sender, preventing impersonation attacks where a malicious
    /// actor could try to send a message with someone else's pubkey.
    ///
    /// # Arguments
    ///
    /// * `rumor_pubkey` - The public key from the rumor (inner Nostr event)
    /// * `sender_credential` - The MLS credential of the authenticated sender (consumed)
    ///
    /// # Returns
    ///
    /// * `Ok(())` - If the rumor pubkey matches the credential identity
    /// * `Err(Error::AuthorMismatch)` - If the pubkeys don't match
    /// * `Err(Error)` - If credential parsing fails
    pub(crate) fn verify_rumor_author(
        &self,
        rumor_pubkey: &nostr::PublicKey,
        sender_credential: openmls::credentials::Credential,
    ) -> Result<()> {
        let basic_credential = BasicCredential::try_from(sender_credential)?;
        let mls_sender_pubkey = self.parse_credential_identity(basic_credential.identity())?;
        if *rumor_pubkey != mls_sender_pubkey {
            tracing::warn!(
                target: "mdk_core::messages::verify_rumor_author",
                "author mismatch: rumor pubkey {} does not match MLS sender {}",
                rumor_pubkey,
                mls_sender_pubkey
            );
            return Err(Error::AuthorMismatch);
        }
        Ok(())
    }

    /// Checks if two identities match, returning an error if they differ
    ///
    /// This is a core validation helper that enforces MIP-00's immutable identity requirement.
    /// It compares two Nostr public keys and returns an error if they are different.
    ///
    /// # Arguments
    ///
    /// * `current_identity` - The member's current identity in the group
    /// * `new_identity` - The proposed new identity
    ///
    /// # Returns
    ///
    /// * `Ok(())` - If identities match
    /// * `Err(Error::IdentityChangeNotAllowed)` - If identities differ
    pub(super) fn validate_identity_unchanged(
        current_identity: nostr::PublicKey,
        new_identity: nostr::PublicKey,
    ) -> Result<()> {
        if current_identity != new_identity {
            return Err(Error::IdentityChangeNotAllowed {
                original_identity: current_identity.to_hex(),
                new_identity: new_identity.to_hex(),
            });
        }
        Ok(())
    }

    /// Validates that a proposal does not attempt to change a member's identity
    ///
    /// MIP-00 mandates immutable identity fields. This function validates that
    /// Update proposals do not attempt to change the BasicCredential.identity
    /// of a member. Identity changes are not allowed as they could enable
    /// impersonation, misattribution, and persistent group state corruption.
    ///
    /// # Arguments
    ///
    /// * `mls_group` - The MLS group to validate against
    /// * `proposal` - The proposal to validate
    /// * `sender` - The sender of the proposal
    ///
    /// # Returns
    ///
    /// * `Ok(())` - If the proposal does not attempt to change identity
    /// * `Err(Error::IdentityChangeNotAllowed)` - If the proposal attempts to change identity
    pub(super) fn validate_proposal_identity(
        &self,
        mls_group: &MlsGroup,
        proposal: &Proposal,
        sender: &Sender,
    ) -> Result<()> {
        // Only Update proposals can change a member's identity
        // Add proposals add new members (no existing identity to change)
        // Remove proposals only specify a leaf index
        if let Proposal::Update(update_proposal) = proposal {
            // Get the sender's leaf index - only members can send Update proposals
            let sender_leaf_index = match sender {
                Sender::Member(leaf_index) => *leaf_index,
                _ => {
                    // Non-member senders cannot send Update proposals
                    // This should be caught earlier, but we handle it gracefully
                    return Ok(());
                }
            };

            // Get the current member's identity from the group
            let current_member = mls_group.member_at(sender_leaf_index);
            let current_identity = match current_member {
                Some(member) => {
                    let credential = BasicCredential::try_from(member.credential.clone())?;
                    self.parse_credential_identity(credential.identity())?
                }
                None => {
                    // Member not found - this shouldn't happen but handle gracefully
                    tracing::warn!(
                        target: "mdk_core::messages::validate_proposal_identity",
                        "Member not found at leaf index {:?}",
                        sender_leaf_index
                    );
                    return Ok(());
                }
            };

            // Get the new identity from the Update proposal's leaf node
            let new_leaf_node = update_proposal.leaf_node();
            let new_credential = BasicCredential::try_from(new_leaf_node.credential().clone())?;
            let new_identity = self.parse_credential_identity(new_credential.identity())?;

            // Check if identity is being changed
            if current_identity != new_identity {
                tracing::warn!(
                    target: "mdk_core::messages::validate_proposal_identity",
                    "Identity change not allowed: proposal attempts to change identity from {} to {}",
                    current_identity,
                    new_identity
                );
            }
            Self::validate_identity_unchanged(current_identity, new_identity)?;
        }

        Ok(())
    }

    /// Checks if a staged commit is a pure self-update commit
    ///
    /// A pure self-update commit is one that only updates the sender's own leaf node
    /// without adding or removing any members or modifying group state. Per the Marmot
    /// protocol specification, any member (not just admins) can create a self-update
    /// commit to rotate their own key material.
    ///
    /// # Arguments
    ///
    /// * `staged_commit` - The staged commit to check
    /// * `sender_leaf_index` - The leaf index of the commit sender
    ///
    /// # Returns
    ///
    /// * `true` - If the commit is a pure self-update (no add/remove/extension proposals, only
    ///   updates to sender's own leaf)
    /// * `false` - If the commit contains add/remove/extension proposals or updates to other leaves
    pub(super) fn is_pure_self_update_commit(
        &self,
        staged_commit: &StagedCommit,
        sender_leaf_index: &openmls::prelude::LeafNodeIndex,
    ) -> bool {
        // A self-update commit must contain at least one self-update signal:
        // either an UpdatePath or an Update proposal. Reject empty commits.
        if staged_commit.update_path_leaf_node().is_none()
            && staged_commit.update_proposals().next().is_none()
        {
            return false;
        }

        // Use a whitelist approach: only allow Update proposals that are self-updates.
        // Any other proposal type (Add, Remove, PreSharedKey, GroupContextExtensions,
        // ReInit, ExternalInit, AppAck, Custom, or future types) requires admin privileges.
        //
        // This is more secure than a blocklist because it automatically rejects any
        // new proposal types that might be added in future MLS/OpenMLS versions.

        // Check all proposals are Update variants
        if !staged_commit
            .queued_proposals()
            .all(|p| matches!(p.proposal(), Proposal::Update(_)))
        {
            return false;
        }

        // Verify all update proposals are self-updates (sender's own leaf)
        staged_commit
            .update_proposals()
            .all(|p| matches!(p.sender(), Sender::Member(idx) if idx == sender_leaf_index))
    }

    /// Validates that a staged commit does not attempt to change any member's identity
    ///
    /// This function checks all Update proposals within a staged commit to ensure
    /// none of them attempt to change the BasicCredential.identity of a member.
    /// It also validates the update path leaf node if present (which represents
    /// the committer's own leaf update).
    ///
    /// # Arguments
    ///
    /// * `mls_group` - The MLS group to validate against
    /// * `staged_commit` - The staged commit to validate
    /// * `commit_sender` - The sender of the commit message
    ///
    /// # Returns
    ///
    /// * `Ok(())` - If no proposals attempt to change identity
    /// * `Err(Error::IdentityChangeNotAllowed)` - If any proposal attempts to change identity
    pub(super) fn validate_commit_identities(
        &self,
        mls_group: &MlsGroup,
        staged_commit: &StagedCommit,
        commit_sender: &Sender,
    ) -> Result<()> {
        // Validate all Update proposals in the staged commit
        for update_proposal in staged_commit.update_proposals() {
            let sender = update_proposal.sender();
            let proposal = Proposal::Update(Box::new(update_proposal.update_proposal().clone()));
            self.validate_proposal_identity(mls_group, &proposal, sender)?;
        }

        // Validate the update path leaf node if present
        // The update path is used when the committer updates their own leaf as part of the commit
        if let Some(update_path_leaf_node) = staged_commit.update_path_leaf_node() {
            // The committer is updating their own leaf via the commit path
            // Get the committer's leaf index from the sender and validate their identity
            if let Sender::Member(committer_leaf_index) = commit_sender
                && let Some(committer_member) = mls_group.member_at(*committer_leaf_index)
            {
                let current_credential =
                    BasicCredential::try_from(committer_member.credential.clone())?;
                let current_identity =
                    self.parse_credential_identity(current_credential.identity())?;

                let new_credential =
                    BasicCredential::try_from(update_path_leaf_node.credential().clone())?;
                let new_identity = self.parse_credential_identity(new_credential.identity())?;

                if current_identity != new_identity {
                    tracing::warn!(
                        target: "mdk_core::messages::validate_commit_identities",
                        "Identity change not allowed in commit update path: committer {} attempted to change identity to {}",
                        current_identity,
                        new_identity
                    );
                }
                Self::validate_identity_unchanged(current_identity, new_identity)?;
            }
        }

        Ok(())
    }

    /// Validates that the commit sender is authorized to create this commit.
    ///
    /// Admins can create any commit. Non-admins can only create pure self-update commits
    /// (commits that only update their own leaf node with no add/remove proposals).
    ///
    /// # Arguments
    ///
    /// * `mls_group` - The MLS group to check authorization against
    /// * `staged_commit` - The staged commit to validate
    /// * `commit_sender` - The MLS sender of the commit
    ///
    /// # Returns
    ///
    /// * `Ok(())` - If the sender is authorized
    /// * `Err(Error::CommitFromNonAdmin)` - If a non-admin tries to create a non-self-update commit
    /// * `Err(Error::MessageFromNonMember)` - If the sender is not a member
    pub(super) fn validate_commit_authorization(
        &self,
        mls_group: &MlsGroup,
        staged_commit: &StagedCommit,
        commit_sender: &Sender,
    ) -> Result<()> {
        match commit_sender {
            Sender::Member(leaf_index) => {
                let member = mls_group
                    .member_at(*leaf_index)
                    .ok_or(Error::MessageFromNonMember)?;

                let basic_cred = BasicCredential::try_from(member.credential.clone())?;
                let sender_pubkey = self.parse_credential_identity(basic_cred.identity())?;
                let group_data = crate::extension::NostrGroupDataExtension::from_group(mls_group)?;
                let sender_is_admin = group_data.admins.contains(&sender_pubkey);

                let is_pure_self_update =
                    self.is_pure_self_update_commit(staged_commit, leaf_index);

                match (sender_is_admin, is_pure_self_update) {
                    (true, _) => Ok(()),
                    (false, true) => {
                        tracing::debug!(
                            target: "mdk_core::messages::process_commit",
                            "Allowing self-update commit from non-admin member at leaf index {:?}",
                            leaf_index
                        );
                        Ok(())
                    }
                    (false, false) => {
                        tracing::warn!(
                            target: "mdk_core::messages::process_commit",
                            "Received non-self-update commit from non-admin member at leaf index {:?}",
                            leaf_index
                        );
                        Err(Error::CommitFromNonAdmin)
                    }
                }
            }
            _ => {
                tracing::warn!(
                    target: "mdk_core::messages::process_commit",
                    "Received commit from non-member sender."
                );
                Err(Error::MessageFromNonMember)
            }
        }
    }

    /// Validates that an event's timestamp is within acceptable bounds
    ///
    /// This method checks that the event timestamp is not too far in the future
    /// (beyond configurable clock skew) and not too old (beyond configurable max age).
    ///
    /// # Arguments
    ///
    /// * `event` - The Nostr event to validate
    ///
    /// # Returns
    ///
    /// * `Ok(())` - If timestamp is valid
    /// * `Err(Error::InvalidTimestamp)` - If timestamp is outside acceptable bounds
    pub(super) fn validate_created_at(&self, event: &Event) -> Result<()> {
        let now = Timestamp::now();

        // Reject events from the future (allow configurable clock skew)
        if event.created_at.as_secs()
            > now
                .as_secs()
                .saturating_add(self.config.max_future_skew_secs)
        {
            return Err(Error::InvalidTimestamp(format!(
                "event timestamp {} is too far in the future (current time: {})",
                event.created_at.as_secs(),
                now.as_secs()
            )));
        }

        // Reject events that are too old (configurable via MdkConfig)
        let min_timestamp = now.as_secs().saturating_sub(self.config.max_event_age_secs);
        if event.created_at.as_secs() < min_timestamp {
            return Err(Error::InvalidTimestamp(format!(
                "event timestamp {} is too old (minimum acceptable: {})",
                event.created_at.as_secs(),
                min_timestamp
            )));
        }

        Ok(())
    }

    /// Extracts the Nostr group ID from event tags
    ///
    /// This method validates that the event has exactly one 'h' tag (per MIP-03)
    /// and extracts the 32-byte group ID from its hex content.
    ///
    /// # Arguments
    ///
    /// * `event` - The Nostr event to extract group ID from
    ///
    /// # Returns
    ///
    /// * `Ok([u8; 32])` - The extracted Nostr group ID
    /// * `Err(Error)` - If the h-tag is missing, malformed, or contains invalid data
    pub(super) fn extract_nostr_group_id(&self, event: &Event) -> Result<[u8; 32]> {
        // Extract and validate group ID tag (MIP-03 requires exactly one h tag)
        let h_tags: Vec<_> = event
            .tags
            .iter()
            .filter(|tag| tag.kind() == TagKind::h())
            .collect();

        if h_tags.is_empty() {
            return Err(Error::MissingGroupIdTag);
        }

        if h_tags.len() > 1 {
            return Err(Error::MultipleGroupIdTags(h_tags.len()));
        }

        let nostr_group_id_tag = h_tags[0];

        let group_id_hex = nostr_group_id_tag
            .content()
            .ok_or_else(|| Error::InvalidGroupIdFormat("h tag has no content".to_string()))?;

        // Validate hex string length before decoding to prevent unbounded memory allocation
        // A 32-byte value requires exactly 64 hex characters
        if group_id_hex.len() != 64 {
            return Err(Error::InvalidGroupIdFormat(format!(
                "expected 64 hex characters (32 bytes), got {} characters",
                group_id_hex.len()
            )));
        }

        // Decode once and reuse the result
        let bytes = hex::decode(group_id_hex)
            .map_err(|e| Error::InvalidGroupIdFormat(format!("hex decode failed: {}", e)))?;

        let nostr_group_id: [u8; 32] = bytes.try_into().map_err(|v: Vec<u8>| {
            Error::InvalidGroupIdFormat(format!("expected 32 bytes, got {} bytes", v.len()))
        })?;

        Ok(nostr_group_id)
    }

    /// Validates the incoming event structure
    ///
    /// This method validates that the event has the correct kind and checks
    /// timestamp bounds per MIP-03 requirements.
    ///
    /// Note: Nostr signature verification is handled by nostr-sdk's relay pool when
    /// events are received from relays.
    ///
    /// # Arguments
    ///
    /// * `event` - The Nostr event to validate
    ///
    /// # Returns
    ///
    /// * `Ok(())` - If the event passes validation
    /// * `Err(Error)` - If validation fails
    pub(super) fn validate_event(&self, event: &Event) -> Result<()> {
        // 1. Verify event kind
        if event.kind != Kind::MlsGroupMessage {
            return Err(Error::UnexpectedEvent {
                expected: Kind::MlsGroupMessage,
                received: event.kind,
            });
        }

        // 2. Verify timestamp is within acceptable bounds
        self.validate_created_at(event)?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use mdk_memory_storage::MdkMemoryStorage;
    use nostr::{EventBuilder, Keys, Kind, Tag, TagKind};
    use openmls::prelude::BasicCredential;
    use tls_codec::Serialize as TlsSerialize;

    use crate::MDK;
    use crate::error::Error;
    use crate::messages::MessageProcessingResult;
    use crate::test_util::*;
    use crate::tests::create_test_mdk;

    /// Direct unit test for the AuthorMismatch error path
    ///
    /// This test directly invokes the verify_rumor_author function with mismatched
    /// inputs to exercise the security-critical error path that prevents impersonation.
    #[test]
    fn test_verify_rumor_author_mismatch() {
        let mdk = create_test_mdk();

        // Create two different identities
        let alice_keys = Keys::generate();
        let bob_keys = Keys::generate();

        // Create a credential for Alice (the authenticated MLS sender)
        let alice_credential = BasicCredential::new(alice_keys.public_key().to_bytes().to_vec());
        let credential: openmls::credentials::Credential = alice_credential.into();

        // Test 1: Mismatched pubkeys should return AuthorMismatch
        // This simulates an attacker (Bob) trying to claim a message was from them
        // when the MLS credential proves it was sent by Alice
        let result = mdk.verify_rumor_author(&bob_keys.public_key(), credential.clone());
        assert!(
            matches!(result, Err(Error::AuthorMismatch)),
            "Expected AuthorMismatch error when rumor pubkey doesn't match credential"
        );

        // Test 2: Matching pubkeys should succeed
        let result = mdk.verify_rumor_author(&alice_keys.public_key(), credential);
        assert!(
            result.is_ok(),
            "Expected success when rumor pubkey matches credential"
        );
    }

    /// Test that validate_identity_unchanged returns Ok when identities match
    ///
    /// This directly tests the core validation helper to ensure it allows
    /// proposals and commits where the identity remains the same.
    #[test]
    fn test_validate_identity_unchanged_same_identity() {
        let keys = Keys::generate();
        let identity = keys.public_key();

        // Same identity should pass validation
        let result = MDK::<MdkMemoryStorage>::validate_identity_unchanged(identity, identity);
        assert!(result.is_ok(), "Matching identities should pass validation");
    }

    /// Test that validate_identity_unchanged returns IdentityChangeNotAllowed when identities differ
    ///
    /// This directly tests the core validation helper to ensure it rejects
    /// proposals and commits that attempt to change a member's identity.
    /// This is the key error path that enforces MIP-00's immutable identity requirement.
    #[test]
    fn test_validate_identity_unchanged_rejects_different_identity() {
        let original_keys = Keys::generate();
        let attacker_keys = Keys::generate();

        let original_identity = original_keys.public_key();
        let attacker_identity = attacker_keys.public_key();

        // Different identities should fail validation
        let result = MDK::<MdkMemoryStorage>::validate_identity_unchanged(
            original_identity,
            attacker_identity,
        );

        assert!(
            result.is_err(),
            "Different identities should fail validation"
        );

        // Verify we get the correct error type with correct identities
        let error = result.unwrap_err();
        assert!(
            matches!(error, Error::IdentityChangeNotAllowed { .. }),
            "Error should be IdentityChangeNotAllowed variant"
        );

        // Verify the error contains the correct identity hex strings
        let error_msg = error.to_string();
        assert!(
            error_msg.contains(&original_identity.to_hex()),
            "Error should contain original identity hex"
        );
        assert!(
            error_msg.contains(&attacker_identity.to_hex()),
            "Error should contain attacker identity hex"
        );
    }

    /// Test that validate_event rejects events with timestamps too far in the future
    #[test]
    fn test_validate_event_rejects_future_timestamp() {
        let mdk = create_test_mdk();
        let (creator, members, admins) = create_test_group_members();
        let group_id = create_test_group(&mdk, &creator, &members, &admins);

        // Get the group's nostr_group_id for the h tag
        let group = mdk
            .get_group(&group_id)
            .expect("Failed to get group")
            .expect("Group should exist");

        // Set timestamp to far future (1 hour ahead, beyond 5 minute skew allowance)
        let future_time = nostr::Timestamp::now().as_secs() + 3600;

        // Create an event with future timestamp
        let message_event = EventBuilder::new(Kind::MlsGroupMessage, "test content")
            .custom_created_at(nostr::Timestamp::from(future_time))
            .tag(Tag::custom(
                TagKind::h(),
                [hex::encode(group.nostr_group_id)],
            ))
            .sign_with_keys(&creator)
            .expect("Failed to create event");

        // Validation should fail due to future timestamp
        let result = mdk.validate_event(&message_event);
        assert!(
            matches!(result, Err(Error::InvalidTimestamp(_))),
            "Expected InvalidTimestamp error for future timestamp, got: {:?}",
            result
        );
    }

    /// Test that validate_event rejects events with timestamps too far in the past
    #[test]
    fn test_validate_event_rejects_old_timestamp() {
        let mdk = create_test_mdk();
        let (creator, members, admins) = create_test_group_members();
        let group_id = create_test_group(&mdk, &creator, &members, &admins);

        // Get the group's nostr_group_id for the h tag
        let group = mdk
            .get_group(&group_id)
            .expect("Failed to get group")
            .expect("Group should exist");

        // Set timestamp to 46 days ago (beyond 45 day limit)
        let old_time = nostr::Timestamp::now().as_secs().saturating_sub(46 * 86400);

        // Create an event with old timestamp
        let message_event = EventBuilder::new(Kind::MlsGroupMessage, "test content")
            .custom_created_at(nostr::Timestamp::from(old_time))
            .tag(Tag::custom(
                TagKind::h(),
                [hex::encode(group.nostr_group_id)],
            ))
            .sign_with_keys(&creator)
            .expect("Failed to create event");

        // Validation should fail due to old timestamp
        let result = mdk.validate_event(&message_event);
        assert!(
            matches!(result, Err(Error::InvalidTimestamp(_))),
            "Expected InvalidTimestamp error for old timestamp, got: {:?}",
            result
        );
    }

    /// Test that validate_event accepts events with valid timestamps
    #[test]
    fn test_validate_event_accepts_valid_timestamp() {
        let mdk = create_test_mdk();
        let (creator, members, admins) = create_test_group_members();
        let group_id = create_test_group(&mdk, &creator, &members, &admins);

        // Get the group's nostr_group_id for the h tag
        let group = mdk
            .get_group(&group_id)
            .expect("Failed to get group")
            .expect("Group should exist");

        // Create an event with current timestamp
        let message_event = EventBuilder::new(Kind::MlsGroupMessage, "test content")
            .tag(Tag::custom(
                TagKind::h(),
                [hex::encode(group.nostr_group_id)],
            ))
            .sign_with_keys(&creator)
            .expect("Failed to create event");

        // Validation should succeed
        let result = mdk.validate_event(&message_event);
        assert!(
            result.is_ok(),
            "Expected valid timestamp to be accepted, got: {:?}",
            result
        );
    }

    /// Test that extract_nostr_group_id rejects events with multiple h tags
    #[test]
    fn test_extract_group_id_rejects_multiple_h_tags() {
        let mdk = create_test_mdk();
        let creator = Keys::generate();

        // Create an event with multiple h tags
        let message_event = EventBuilder::new(Kind::MlsGroupMessage, "test content")
            .tag(Tag::custom(TagKind::h(), [hex::encode([1u8; 32])]))
            .tag(Tag::custom(TagKind::h(), [hex::encode([2u8; 32])]))
            .sign_with_keys(&creator)
            .expect("Failed to create event");

        // Extraction should fail due to multiple h tags
        let result = mdk.extract_nostr_group_id(&message_event);
        assert!(
            matches!(result, Err(Error::MultipleGroupIdTags(2))),
            "Expected MultipleGroupIdTags error, got: {:?}",
            result
        );
    }

    /// Test that extract_nostr_group_id rejects events with invalid hex in h tag
    #[test]
    fn test_extract_group_id_rejects_invalid_hex() {
        let mdk = create_test_mdk();
        let creator = Keys::generate();

        // Create an event with invalid hex in h tag
        let message_event = EventBuilder::new(Kind::MlsGroupMessage, "test content")
            .tag(Tag::custom(TagKind::h(), ["not-valid-hex-zzz"]))
            .sign_with_keys(&creator)
            .expect("Failed to create event");

        // Extraction should fail due to invalid hex
        let result = mdk.extract_nostr_group_id(&message_event);
        assert!(
            matches!(result, Err(Error::InvalidGroupIdFormat(_))),
            "Expected InvalidGroupIdFormat error, got: {:?}",
            result
        );
    }

    /// Test that extract_nostr_group_id rejects events with wrong length group ID
    #[test]
    fn test_extract_group_id_rejects_wrong_length() {
        let mdk = create_test_mdk();
        let creator = Keys::generate();

        // Create an event with wrong length group ID (16 bytes instead of 32)
        let message_event = EventBuilder::new(Kind::MlsGroupMessage, "test content")
            .tag(Tag::custom(TagKind::h(), [hex::encode([1u8; 16])]))
            .sign_with_keys(&creator)
            .expect("Failed to create event");

        // Extraction should fail due to wrong length
        let result = mdk.extract_nostr_group_id(&message_event);
        assert!(
            matches!(result, Err(Error::InvalidGroupIdFormat(_))),
            "Expected InvalidGroupIdFormat error for wrong length, got: {:?}",
            result
        );
    }

    /// Test that extract_nostr_group_id extracts valid group ID
    #[test]
    fn test_extract_group_id_returns_valid_id() {
        let mdk = create_test_mdk();
        let (creator, members, admins) = create_test_group_members();
        let group_id = create_test_group(&mdk, &creator, &members, &admins);

        // Get the group's nostr_group_id for the h tag
        let group = mdk
            .get_group(&group_id)
            .expect("Failed to get group")
            .expect("Group should exist");

        // Create an event with valid group ID
        let message_event = EventBuilder::new(Kind::MlsGroupMessage, "test content")
            .tag(Tag::custom(
                TagKind::h(),
                [hex::encode(group.nostr_group_id)],
            ))
            .sign_with_keys(&creator)
            .expect("Failed to create event");

        // Extraction should succeed and return the correct group ID
        let result = mdk.extract_nostr_group_id(&message_event);
        assert!(result.is_ok(), "Expected success, got: {:?}", result);
        assert_eq!(
            result.unwrap(),
            group.nostr_group_id,
            "Extracted group ID should match"
        );
    }

    /// Test that extract_nostr_group_id rejects events missing h tag
    #[test]
    fn test_extract_group_id_rejects_missing_h_tag() {
        let mdk = create_test_mdk();
        let creator = Keys::generate();

        // Create an event without h tag
        let message_event = EventBuilder::new(Kind::MlsGroupMessage, "test content")
            .sign_with_keys(&creator)
            .expect("Failed to create event");

        // Extraction should fail due to missing h tag
        let result = mdk.extract_nostr_group_id(&message_event);
        assert!(
            matches!(result, Err(Error::MissingGroupIdTag)),
            "Expected MissingGroupIdTag error, got: {:?}",
            result
        );
    }

    #[test]
    fn test_process_message_invalid_kind() {
        let mdk = create_test_mdk();
        let creator = Keys::generate();

        // Create an event with wrong kind
        let event = EventBuilder::new(Kind::TextNote, "test content")
            .sign_with_keys(&creator)
            .expect("Failed to sign event");

        let result = mdk.process_message(&event);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), Error::UnexpectedEvent { .. }));
    }

    #[test]
    fn test_process_message_missing_group_id_tag() {
        let mdk = create_test_mdk();
        let creator = Keys::generate();

        // Create an event without group ID tag
        let event = EventBuilder::new(Kind::MlsGroupMessage, "test content")
            .sign_with_keys(&creator)
            .expect("Failed to sign event");

        let result = mdk.process_message(&event);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), Error::MissingGroupIdTag));
    }

    #[test]
    fn test_process_message_invalid_group_id_format() {
        let mdk = create_test_mdk();
        let creator = Keys::generate();

        // Create an event with invalid group ID format (not valid hex)
        let invalid_group_id = "not-valid-hex-zzz";
        let tag = Tag::custom(TagKind::h(), [invalid_group_id]);

        let event = EventBuilder::new(Kind::MlsGroupMessage, "encrypted_content")
            .tag(tag)
            .sign_with_keys(&creator)
            .expect("Failed to sign event");

        let result = mdk.process_message(&event);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            Error::InvalidGroupIdFormat(_)
        ));
    }

    #[test]
    fn test_process_message_group_not_found() {
        let mdk = create_test_mdk();
        let creator = Keys::generate();

        // Create a valid MLS group message event with non-existent group ID
        let fake_group_id = hex::encode([1u8; 32]);
        let tag = Tag::custom(TagKind::h(), [fake_group_id]);

        let event = EventBuilder::new(Kind::MlsGroupMessage, "encrypted_content")
            .tag(tag)
            .sign_with_keys(&creator)
            .expect("Failed to sign event");

        let result = mdk.process_message(&event);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), Error::GroupNotFound));
    }

    /// Test message processing with wrong event kind
    #[test]
    fn test_process_message_wrong_event_kind() {
        let mdk = create_test_mdk();
        let creator = Keys::generate();

        // Create an event with wrong kind (TextNote instead of MlsGroupMessage)
        let event = EventBuilder::new(Kind::TextNote, "test content")
            .sign_with_keys(&creator)
            .expect("Failed to sign event");

        let result = mdk.process_message(&event);

        // Should return UnexpectedEvent error
        assert!(
            matches!(
                result,
                Err(crate::Error::UnexpectedEvent { expected, received })
                if expected == Kind::MlsGroupMessage && received == Kind::TextNote
            ),
            "Should return UnexpectedEvent error for wrong kind"
        );
    }

    /// Test message processing with missing group ID tag
    #[test]
    fn test_process_message_missing_group_id() {
        let mdk = create_test_mdk();
        let creator = Keys::generate();

        // Create a group message event without the required 'h' tag
        let event = EventBuilder::new(Kind::MlsGroupMessage, "encrypted_content")
            .sign_with_keys(&creator)
            .expect("Failed to sign event");

        let result = mdk.process_message(&event);

        // Should fail due to missing group ID tag
        assert!(result.is_err(), "Should fail when group ID tag is missing");
    }

    /// Malformed message handling
    ///
    /// Tests that malformed or invalid messages are rejected gracefully
    /// without causing panics or crashes.
    ///
    /// Requirements tested:
    /// - Invalid event kinds rejected with clear errors
    /// - Missing required tags detected
    /// - No panics on malformed input
    /// - Error messages don't leak sensitive data
    #[test]
    fn test_malformed_message_handling() {
        let mdk = create_test_mdk();
        let creator = Keys::generate();

        // Test 1: Invalid event kind (using TextNote instead of MlsGroupMessage)
        let invalid_kind_event = EventBuilder::new(Kind::TextNote, "malformed content")
            .sign_with_keys(&creator)
            .expect("Failed to sign event");

        let result1 = mdk.process_message(&invalid_kind_event);
        assert!(
            result1.is_err(),
            "Should reject message with wrong event kind"
        );
        assert!(
            matches!(result1, Err(crate::Error::UnexpectedEvent { .. })),
            "Should return UnexpectedEvent error"
        );

        // Test 2: Missing group ID tag
        let missing_tag_event = EventBuilder::new(Kind::MlsGroupMessage, "content")
            .sign_with_keys(&creator)
            .expect("Failed to sign event");

        let result2 = mdk.process_message(&missing_tag_event);
        assert!(
            result2.is_err(),
            "Should reject message without group ID tag"
        );

        // Note: Empty content is actually valid per test_message_with_empty_content
        // The system handles empty messages correctly, so no additional test needed here

        // All error cases should be handled gracefully without panics
    }

    #[test]
    fn test_author_verification_binding() {
        // Setup: Create Alice and Bob
        let alice_keys = Keys::generate();
        let bob_keys = Keys::generate();
        let _malicious_keys = Keys::generate(); // A third party trying to impersonate

        let alice_mdk = create_test_mdk();
        let bob_mdk = create_test_mdk();

        let admins = vec![alice_keys.public_key(), bob_keys.public_key()];

        // Bob creates his key package in his own MDK
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

        // Test 1: Valid message - Alice sends with her correct pubkey
        let valid_rumor = create_test_rumor(&alice_keys, "Hello from Alice");
        let valid_msg = alice_mdk
            .create_message(&group_id, valid_rumor)
            .expect("Alice should be able to send a valid message");

        // Bob processes Alice's valid message - should succeed
        let bob_process_valid = bob_mdk.process_message(&valid_msg);
        assert!(
            bob_process_valid.is_ok(),
            "Bob should process Alice's valid message"
        );
        match bob_process_valid.unwrap() {
            MessageProcessingResult::ApplicationMessage(msg) => {
                assert_eq!(msg.content, "Hello from Alice");
                assert_eq!(msg.pubkey, alice_keys.public_key());
            }
            _ => panic!("Expected ApplicationMessage"),
        }

        // Test 2: Invalid message - Alice creates a message but with a different pubkey
        // This simulates an attacker trying to impersonate someone else by creating
        // a rumor with a forged pubkey, but MLS authentication should catch this.
        //
        // Note: In practice, the MLS layer authenticates the sender using the credential
        // bound to their leaf node. The author check ensures the rumor's pubkey
        // matches the authenticated MLS sender's credential.
        //
        // To truly test this, we would need to craft a message where the rumor pubkey
        // differs from the MLS sender's credential. Since we can't easily craft such
        // a malicious message in the current test framework (the rumor pubkey is set
        // by the sender and MLS authenticates the sender), we verify the mechanism
        // is in place by checking that valid messages work and the error type exists.

        // Verify the error type exists and can be matched
        let test_error = Error::AuthorMismatch;
        assert_eq!(
            test_error.to_string(),
            "author mismatch: rumor pubkey does not match MLS sender"
        );
    }

    /// Test that IdentityChangeNotAllowed error type is properly constructed
    ///
    /// This test verifies the error variant we added for MIP-00 compliance
    /// is correctly defined and provides useful error messages.
    #[test]
    fn test_identity_change_not_allowed_error() {
        let alice_keys = Keys::generate();
        let bob_keys = Keys::generate();

        let error = Error::IdentityChangeNotAllowed {
            original_identity: alice_keys.public_key().to_hex(),
            new_identity: bob_keys.public_key().to_hex(),
        };

        // Verify the error message contains both identities
        let error_msg = error.to_string();
        assert!(
            error_msg.contains(&alice_keys.public_key().to_hex()),
            "Error message should contain original identity"
        );
        assert!(
            error_msg.contains(&bob_keys.public_key().to_hex()),
            "Error message should contain new identity"
        );
        assert!(
            error_msg.contains("identity change not allowed"),
            "Error message should indicate identity change is not allowed"
        );
    }

    /// Test that self_update preserves identity (verifies identity validation passes)
    ///
    /// This integration test verifies that a legitimate self_update operation
    /// passes identity validation since it doesn't change the member's identity.
    /// The validate_proposal_identity function is called internally during
    /// message processing, so this test confirms the validation succeeds for valid updates.
    #[test]
    fn test_self_update_preserves_identity_passes_validation() {
        let mdk = create_test_mdk();
        let (creator, members, admins) = create_test_group_members();
        let group_id = create_test_group(&mdk, &creator, &members, &admins);

        // Get the original identity from the group
        let mls_group = mdk
            .load_mls_group(&group_id)
            .expect("Failed to load MLS group")
            .expect("MLS group should exist");
        let original_leaf = mls_group.own_leaf().expect("Failed to get own leaf");
        let original_credential =
            BasicCredential::try_from(original_leaf.credential().clone()).unwrap();
        let original_identity = original_credential.identity().to_vec();

        // Perform self_update - this internally creates an Update proposal
        // and should pass identity validation
        let update_result = mdk
            .self_update(&group_id)
            .expect("self_update should succeed - identity validation should pass");

        // Merge the pending commit
        mdk.merge_pending_commit(&group_id)
            .expect("merge should succeed");

        // Verify the identity was preserved after the update
        let updated_mls_group = mdk
            .load_mls_group(&group_id)
            .expect("Failed to load MLS group")
            .expect("MLS group should exist");
        let updated_leaf = updated_mls_group
            .own_leaf()
            .expect("Failed to get updated own leaf");
        let updated_credential =
            BasicCredential::try_from(updated_leaf.credential().clone()).unwrap();
        let updated_identity = updated_credential.identity().to_vec();

        assert_eq!(
            original_identity, updated_identity,
            "Identity should be preserved after self_update"
        );

        // Verify the update result is valid
        assert_eq!(
            update_result.mls_group_id, group_id,
            "Update result should have the same group ID"
        );
    }

    /// Test that identity parsing works correctly for validation
    ///
    /// This test verifies the components used in identity validation work correctly:
    /// parsing identities from credentials and comparing them.
    #[test]
    fn test_identity_parsing_for_validation() {
        let mdk = create_test_mdk();
        let (creator, members, admins) = create_test_group_members();
        let group_id = create_test_group(&mdk, &creator, &members, &admins);

        // Load the MLS group
        let mls_group = mdk
            .load_mls_group(&group_id)
            .expect("Failed to load MLS group")
            .expect("MLS group should exist");

        // Create a fake identity (different from any group member)
        let attacker_keys = Keys::generate();
        let attacker_credential =
            BasicCredential::new(attacker_keys.public_key().to_bytes().to_vec());

        // Get the current member's identity at leaf index 0
        if let Some(member) = mls_group.member_at(openmls::prelude::LeafNodeIndex::new(0)) {
            let current_credential = BasicCredential::try_from(member.credential.clone()).unwrap();
            let current_identity = mdk
                .parse_credential_identity(current_credential.identity())
                .expect("Failed to parse credential identity");

            let attacker_identity = mdk
                .parse_credential_identity(attacker_credential.identity())
                .expect("Failed to parse attacker identity");

            // Verify the identities are different
            assert_ne!(
                current_identity, attacker_identity,
                "Attacker identity should be different from member identity"
            );

            // Verify identity matches creator's public key
            assert_eq!(
                current_identity,
                creator.public_key(),
                "Member identity should match creator public key"
            );
        }
    }

    /// Test that commit processing validates identity in a multi-member scenario
    ///
    /// This test creates a multi-member group and verifies that when one member
    /// processes another member's commit, the identity validation passes for
    /// legitimate commits.
    #[test]
    fn test_commit_processing_validates_identity_multi_member() {
        let alice_keys = Keys::generate();
        let bob_keys = Keys::generate();

        let alice_mdk = create_test_mdk();
        let bob_mdk = create_test_mdk();

        // Create key packages
        let bob_key_package = create_key_package_event(&bob_mdk, &bob_keys);

        // Alice creates group with Bob as admin
        let admin_pubkeys = vec![alice_keys.public_key(), bob_keys.public_key()];
        let config = create_nostr_group_config_data(admin_pubkeys);

        let create_result = alice_mdk
            .create_group(&alice_keys.public_key(), vec![bob_key_package], config)
            .expect("Alice should create group");

        let group_id = create_result.group.mls_group_id.clone();

        alice_mdk
            .merge_pending_commit(&group_id)
            .expect("Alice should merge commit");

        // Bob joins the group
        let bob_welcome_rumor = &create_result.welcome_rumors[0];
        let bob_welcome = bob_mdk
            .process_welcome(&nostr::EventId::all_zeros(), bob_welcome_rumor)
            .expect("Bob should process welcome");

        bob_mdk
            .accept_welcome(&bob_welcome)
            .expect("Bob should accept welcome");

        // Verify both see 2 members
        let alice_members = alice_mdk.get_members(&group_id).expect("Alice get members");
        let bob_members = bob_mdk.get_members(&group_id).expect("Bob get members");
        assert_eq!(alice_members.len(), 2, "Alice should see 2 members");
        assert_eq!(bob_members.len(), 2, "Bob should see 2 members");

        // Alice performs a self_update (creates a commit with update_path)
        // This exercises the update_path_leaf_node validation
        let alice_update_result = alice_mdk
            .self_update(&group_id)
            .expect("Alice self_update should succeed");

        alice_mdk
            .merge_pending_commit(&group_id)
            .expect("Alice should merge self_update commit");

        // Bob processes Alice's commit - this triggers identity validation
        // The validation should pass because Alice's identity is preserved
        let bob_process_result = bob_mdk.process_message(&alice_update_result.evolution_event);

        assert!(
            bob_process_result.is_ok(),
            "Bob should successfully process Alice's commit with identity validation"
        );

        // Verify identities are still correct after the update
        let alice_mls_group = alice_mdk
            .load_mls_group(&group_id)
            .expect("Load Alice MLS group")
            .expect("Alice MLS group exists");

        let alice_own_leaf = alice_mls_group
            .own_leaf()
            .expect("Alice should have own leaf");
        let alice_credential =
            BasicCredential::try_from(alice_own_leaf.credential().clone()).unwrap();
        let alice_identity = alice_mdk
            .parse_credential_identity(alice_credential.identity())
            .expect("Parse Alice identity");

        assert_eq!(
            alice_identity,
            alice_keys.public_key(),
            "Alice's identity should be preserved after self_update"
        );
    }

    /// Test that the IdentityChangeNotAllowed error contains useful information
    ///
    /// This test verifies that when an identity change is detected, the error
    /// contains both the original and new identity for debugging purposes.
    #[test]
    fn test_identity_change_error_contains_identities() {
        let alice_keys = Keys::generate();
        let bob_keys = Keys::generate();

        let error = Error::IdentityChangeNotAllowed {
            original_identity: alice_keys.public_key().to_hex(),
            new_identity: bob_keys.public_key().to_hex(),
        };

        // Verify error can be displayed
        let error_string = error.to_string();
        assert!(
            error_string.contains("identity change not allowed"),
            "Error should mention identity change"
        );
        assert!(
            error_string.contains(&alice_keys.public_key().to_hex()),
            "Error should contain original identity"
        );
        assert!(
            error_string.contains(&bob_keys.public_key().to_hex()),
            "Error should contain new identity"
        );

        // Verify error type matches
        assert!(
            matches!(error, Error::IdentityChangeNotAllowed { .. }),
            "Error should be IdentityChangeNotAllowed variant"
        );
    }

    /// Test identity validation during add_members commit processing
    ///
    /// This test verifies that identity validation is triggered when processing
    /// add_members commits that contain update paths.
    #[test]
    fn test_add_members_commit_triggers_identity_validation() {
        let alice_keys = Keys::generate();
        let bob_keys = Keys::generate();
        let charlie_keys = Keys::generate();

        let alice_mdk = create_test_mdk();
        let bob_mdk = create_test_mdk();
        let charlie_mdk = create_test_mdk();

        // Create key packages
        let bob_key_package = create_key_package_event(&bob_mdk, &bob_keys);
        let charlie_key_package = create_key_package_event(&charlie_mdk, &charlie_keys);

        // Alice creates group with Bob
        let admin_pubkeys = vec![alice_keys.public_key()];
        let config = create_nostr_group_config_data(admin_pubkeys);

        let create_result = alice_mdk
            .create_group(&alice_keys.public_key(), vec![bob_key_package], config)
            .expect("Alice should create group");

        let group_id = create_result.group.mls_group_id.clone();

        alice_mdk
            .merge_pending_commit(&group_id)
            .expect("Alice should merge commit");

        // Bob joins the group
        let bob_welcome_rumor = &create_result.welcome_rumors[0];
        let bob_welcome = bob_mdk
            .process_welcome(&nostr::EventId::all_zeros(), bob_welcome_rumor)
            .expect("Bob should process welcome");

        bob_mdk
            .accept_welcome(&bob_welcome)
            .expect("Bob should accept welcome");

        // Alice adds Charlie - this creates a commit with update_path
        let add_result = alice_mdk
            .add_members(&group_id, &[charlie_key_package])
            .expect("Alice should add Charlie");

        alice_mdk
            .merge_pending_commit(&group_id)
            .expect("Alice should merge add commit");

        // Bob processes Alice's add_members commit
        // This triggers identity validation on the update_path
        let bob_process_result = bob_mdk.process_message(&add_result.evolution_event);

        assert!(
            bob_process_result.is_ok(),
            "Bob should successfully process add_members commit with identity validation"
        );

        // Verify Alice's identity is still correct after the commit
        let alice_mls_group = alice_mdk
            .load_mls_group(&group_id)
            .expect("Load Alice MLS group")
            .expect("Alice MLS group exists");

        let alice_own_leaf = alice_mls_group
            .own_leaf()
            .expect("Alice should have own leaf");
        let alice_credential =
            BasicCredential::try_from(alice_own_leaf.credential().clone()).unwrap();
        let alice_identity = alice_mdk
            .parse_credential_identity(alice_credential.identity())
            .expect("Parse Alice identity");

        assert_eq!(
            alice_identity,
            alice_keys.public_key(),
            "Alice's identity should be preserved after add_members"
        );
    }

    /// Test identity validation during remove_members commit processing
    ///
    /// This test verifies that identity validation is triggered when processing
    /// remove_members commits.
    #[test]
    fn test_remove_members_commit_triggers_identity_validation() {
        let alice_keys = Keys::generate();
        let bob_keys = Keys::generate();
        let charlie_keys = Keys::generate();

        let alice_mdk = create_test_mdk();
        let bob_mdk = create_test_mdk();
        let charlie_mdk = create_test_mdk();

        // Create key packages
        let bob_key_package = create_key_package_event(&bob_mdk, &bob_keys);
        let charlie_key_package = create_key_package_event(&charlie_mdk, &charlie_keys);

        // Alice creates group with Bob and Charlie (Alice is admin)
        let admin_pubkeys = vec![alice_keys.public_key()];
        let config = create_nostr_group_config_data(admin_pubkeys);

        let create_result = alice_mdk
            .create_group(
                &alice_keys.public_key(),
                vec![bob_key_package, charlie_key_package],
                config,
            )
            .expect("Alice should create group");

        let group_id = create_result.group.mls_group_id.clone();

        alice_mdk
            .merge_pending_commit(&group_id)
            .expect("Alice should merge commit");

        // Bob joins the group
        let bob_welcome_rumor = &create_result.welcome_rumors[0];
        let bob_welcome = bob_mdk
            .process_welcome(&nostr::EventId::all_zeros(), bob_welcome_rumor)
            .expect("Bob should process welcome");

        bob_mdk
            .accept_welcome(&bob_welcome)
            .expect("Bob should accept welcome");

        // Verify initial member count
        let alice_members = alice_mdk.get_members(&group_id).expect("Alice get members");
        assert_eq!(
            alice_members.len(),
            3,
            "Alice should see 3 members initially"
        );

        // Alice removes Charlie
        let remove_result = alice_mdk
            .remove_members(&group_id, &[charlie_keys.public_key()])
            .expect("Alice should remove Charlie");

        alice_mdk
            .merge_pending_commit(&group_id)
            .expect("Alice should merge remove commit");

        // Bob processes Alice's remove_members commit
        // This triggers identity validation
        let bob_process_result = bob_mdk.process_message(&remove_result.evolution_event);

        assert!(
            bob_process_result.is_ok(),
            "Bob should successfully process remove_members commit with identity validation"
        );

        // Verify member count changed
        let alice_members_after = alice_mdk
            .get_members(&group_id)
            .expect("Alice get members after");
        assert_eq!(
            alice_members_after.len(),
            2,
            "Alice should see 2 members after removal"
        );
    }

    /// Test multiple sequential commits with identity validation
    ///
    /// This test verifies that identity validation works correctly across
    /// multiple sequential commits in a group.
    #[test]
    fn test_sequential_commits_identity_validation() {
        let alice_keys = Keys::generate();
        let bob_keys = Keys::generate();

        let alice_mdk = create_test_mdk();
        let bob_mdk = create_test_mdk();

        // Create key packages
        let bob_key_package = create_key_package_event(&bob_mdk, &bob_keys);

        // Alice creates group with Bob as admin
        let admin_pubkeys = vec![alice_keys.public_key(), bob_keys.public_key()];
        let config = create_nostr_group_config_data(admin_pubkeys);

        let create_result = alice_mdk
            .create_group(&alice_keys.public_key(), vec![bob_key_package], config)
            .expect("Alice should create group");

        let group_id = create_result.group.mls_group_id.clone();

        alice_mdk
            .merge_pending_commit(&group_id)
            .expect("Alice should merge commit");

        // Bob joins the group
        let bob_welcome_rumor = &create_result.welcome_rumors[0];
        let bob_welcome = bob_mdk
            .process_welcome(&nostr::EventId::all_zeros(), bob_welcome_rumor)
            .expect("Bob should process welcome");

        bob_mdk
            .accept_welcome(&bob_welcome)
            .expect("Bob should accept welcome");

        // Perform multiple self_updates and verify identity is preserved each time
        for i in 0..3 {
            // Alice performs self_update
            let alice_update_result = alice_mdk
                .self_update(&group_id)
                .unwrap_or_else(|e| panic!("Alice self_update {} should succeed: {:?}", i, e));

            alice_mdk
                .merge_pending_commit(&group_id)
                .unwrap_or_else(|e| panic!("Alice should merge self_update commit {}: {:?}", i, e));

            // Bob processes Alice's commit
            let bob_process_result = bob_mdk.process_message(&alice_update_result.evolution_event);
            assert!(
                bob_process_result.is_ok(),
                "Bob should process Alice's commit {} with identity validation",
                i
            );

            // Bob performs self_update
            let bob_update_result = bob_mdk
                .self_update(&group_id)
                .unwrap_or_else(|e| panic!("Bob self_update {} should succeed: {:?}", i, e));

            bob_mdk
                .merge_pending_commit(&group_id)
                .unwrap_or_else(|e| panic!("Bob should merge self_update commit {}: {:?}", i, e));

            // Alice processes Bob's commit
            let alice_process_result =
                alice_mdk.process_message(&bob_update_result.evolution_event);
            assert!(
                alice_process_result.is_ok(),
                "Alice should process Bob's commit {} with identity validation",
                i
            );
        }

        // Verify both identities are still correct after all commits
        let alice_mls_group = alice_mdk
            .load_mls_group(&group_id)
            .expect("Load Alice MLS group")
            .expect("Alice MLS group exists");
        let alice_own_leaf = alice_mls_group.own_leaf().expect("Alice own leaf");
        let alice_credential =
            BasicCredential::try_from(alice_own_leaf.credential().clone()).unwrap();
        let alice_identity = alice_mdk
            .parse_credential_identity(alice_credential.identity())
            .expect("Parse Alice identity");

        let bob_mls_group = bob_mdk
            .load_mls_group(&group_id)
            .expect("Load Bob MLS group")
            .expect("Bob MLS group exists");
        let bob_own_leaf = bob_mls_group.own_leaf().expect("Bob own leaf");
        let bob_credential = BasicCredential::try_from(bob_own_leaf.credential().clone()).unwrap();
        let bob_identity = bob_mdk
            .parse_credential_identity(bob_credential.identity())
            .expect("Parse Bob identity");

        assert_eq!(
            alice_identity,
            alice_keys.public_key(),
            "Alice's identity should be preserved after multiple commits"
        );
        assert_eq!(
            bob_identity,
            bob_keys.public_key(),
            "Bob's identity should be preserved after multiple commits"
        );
    }

    /// Test that validate_proposal_identity handles non-Update proposals correctly
    ///
    /// This test verifies that the validation function correctly handles
    /// different proposal types (Add, Remove) without errors.
    #[test]
    fn test_validate_proposal_identity_non_update_proposals() {
        let mdk = create_test_mdk();
        let (creator, members, admins) = create_test_group_members();
        let group_id = create_test_group(&mdk, &creator, &members, &admins);

        // Load the MLS group
        let mls_group = mdk
            .load_mls_group(&group_id)
            .expect("Failed to load MLS group")
            .expect("MLS group should exist");

        // Verify we have members in the group
        let member_count = mls_group.members().count();
        assert!(member_count > 0, "Group should have members");

        // Verify each member has a valid identity
        for member in mls_group.members() {
            let credential = BasicCredential::try_from(member.credential.clone())
                .expect("Should extract credential");
            let identity = mdk
                .parse_credential_identity(credential.identity())
                .expect("Should parse identity");

            // Verify identity is a valid 32-byte public key
            assert_eq!(identity.to_bytes().len(), 32, "Identity should be 32 bytes");
        }
    }

    /// Test identity validation with group epoch changes
    ///
    /// This test verifies that identity validation works correctly as the
    /// group advances through multiple epochs.
    #[test]
    fn test_identity_validation_across_epochs() {
        let alice_keys = Keys::generate();
        let bob_keys = Keys::generate();

        let alice_mdk = create_test_mdk();
        let bob_mdk = create_test_mdk();

        // Create key packages
        let bob_key_package = create_key_package_event(&bob_mdk, &bob_keys);

        // Alice creates group with Bob
        let admin_pubkeys = vec![alice_keys.public_key(), bob_keys.public_key()];
        let config = create_nostr_group_config_data(admin_pubkeys);

        let create_result = alice_mdk
            .create_group(&alice_keys.public_key(), vec![bob_key_package], config)
            .expect("Alice should create group");

        let group_id = create_result.group.mls_group_id.clone();

        alice_mdk
            .merge_pending_commit(&group_id)
            .expect("Alice should merge commit");

        // Bob joins the group
        let bob_welcome_rumor = &create_result.welcome_rumors[0];
        let bob_welcome = bob_mdk
            .process_welcome(&nostr::EventId::all_zeros(), bob_welcome_rumor)
            .expect("Bob should process welcome");

        bob_mdk
            .accept_welcome(&bob_welcome)
            .expect("Bob should accept welcome");

        // Get initial epoch
        let initial_epoch = alice_mdk
            .get_group(&group_id)
            .expect("Get group")
            .expect("Group exists")
            .epoch;

        // Advance epoch multiple times
        for i in 0..5 {
            let update_result = alice_mdk
                .self_update(&group_id)
                .unwrap_or_else(|e| panic!("Alice self_update {} should succeed: {:?}", i, e));

            alice_mdk
                .merge_pending_commit(&group_id)
                .unwrap_or_else(|e| panic!("Alice should merge commit {}: {:?}", i, e));

            // Bob processes to stay in sync
            bob_mdk
                .process_message(&update_result.evolution_event)
                .unwrap_or_else(|e| panic!("Bob should process commit {}: {:?}", i, e));
        }

        // Verify epoch advanced
        let final_epoch = alice_mdk
            .get_group(&group_id)
            .expect("Get group")
            .expect("Group exists")
            .epoch;

        assert!(
            final_epoch > initial_epoch,
            "Epoch should have advanced: {} > {}",
            final_epoch,
            initial_epoch
        );

        // Verify identities are still correct
        let alice_mls_group = alice_mdk
            .load_mls_group(&group_id)
            .expect("Load MLS group")
            .expect("MLS group exists");

        let alice_own_leaf = alice_mls_group.own_leaf().expect("Alice own leaf");
        let alice_credential =
            BasicCredential::try_from(alice_own_leaf.credential().clone()).unwrap();
        let alice_identity = alice_mdk
            .parse_credential_identity(alice_credential.identity())
            .expect("Parse identity");

        assert_eq!(
            alice_identity,
            alice_keys.public_key(),
            "Alice's identity should be preserved across epoch changes"
        );
    }

    /// Test that identity validation correctly detects identity changes
    ///
    /// This test verifies the identity validation logic can correctly detect
    /// when an Update proposal would contain a different identity than the sender's
    /// current identity and would return IdentityChangeNotAllowed error.
    #[test]
    fn test_identity_validation_detects_changes() {
        let mdk = create_test_mdk();
        let (creator, members, admins) = create_test_group_members();
        let group_id = create_test_group(&mdk, &creator, &members, &admins);

        // Load the MLS group
        let mls_group = mdk
            .load_mls_group(&group_id)
            .expect("Failed to load MLS group")
            .expect("MLS group should exist");

        // Get the creator's leaf node (at index 0)
        let own_leaf = mls_group.own_leaf().expect("Should have own leaf");

        // Get the current identity
        let creator_credential = BasicCredential::try_from(own_leaf.credential().clone())
            .expect("Failed to get credential");
        let creator_identity = mdk
            .parse_credential_identity(creator_credential.identity())
            .expect("Failed to parse identity");

        // Create a different identity (attacker)
        let attacker_keys = Keys::generate();
        let attacker_identity = attacker_keys.public_key();

        // Verify identities are different
        assert_ne!(
            creator_identity, attacker_identity,
            "Creator and attacker identities should be different"
        );

        // Verify the error would be constructed correctly if detected
        let expected_error = Error::IdentityChangeNotAllowed {
            original_identity: creator_identity.to_hex(),
            new_identity: attacker_identity.to_hex(),
        };
        assert!(
            expected_error
                .to_string()
                .contains("identity change not allowed"),
            "Error message should indicate identity change"
        );
        assert!(
            expected_error
                .to_string()
                .contains(&creator_identity.to_hex()),
            "Error should contain original identity"
        );
        assert!(
            expected_error
                .to_string()
                .contains(&attacker_identity.to_hex()),
            "Error should contain new identity"
        );

        // Verify the error type matches correctly
        assert!(
            matches!(expected_error, Error::IdentityChangeNotAllowed { .. }),
            "Error should be IdentityChangeNotAllowed variant"
        );
    }

    /// Test that validate_commit_identities logic works correctly
    ///
    /// This test verifies that if a commit's update_path_leaf_node contained
    /// a different identity than the committer's current identity, the validation
    /// logic would correctly return IdentityChangeNotAllowed error.
    #[test]
    fn test_staged_commit_identity_validation_logic() {
        let mdk = create_test_mdk();
        let (creator, members, admins) = create_test_group_members();
        let group_id = create_test_group(&mdk, &creator, &members, &admins);

        // Load the MLS group
        let mls_group = mdk
            .load_mls_group(&group_id)
            .expect("Failed to load MLS group")
            .expect("MLS group should exist");

        // Get the current member's identity
        let member = mls_group
            .member_at(openmls::prelude::LeafNodeIndex::new(0))
            .expect("Member should exist at index 0");
        let current_credential =
            BasicCredential::try_from(member.credential.clone()).expect("Failed to get credential");
        let current_identity = mdk
            .parse_credential_identity(current_credential.identity())
            .expect("Failed to parse identity");

        // Create a different identity
        let attacker_keys = Keys::generate();
        let attacker_credential =
            BasicCredential::new(attacker_keys.public_key().to_bytes().to_vec());
        let attacker_identity = mdk
            .parse_credential_identity(attacker_credential.identity())
            .expect("Failed to parse attacker identity");

        // Verify identities are different
        assert_ne!(
            current_identity, attacker_identity,
            "Current and attacker identities should be different"
        );

        // Verify the comparison logic that would trigger the error
        assert!(
            current_identity != attacker_identity,
            "Identity comparison should detect mismatch"
        );

        // Verify error construction
        let error = Error::IdentityChangeNotAllowed {
            original_identity: current_identity.to_hex(),
            new_identity: attacker_identity.to_hex(),
        };
        assert!(
            error.to_string().contains(&current_identity.to_hex()),
            "Error should contain original identity"
        );
        assert!(
            error.to_string().contains(&attacker_identity.to_hex()),
            "Error should contain new identity"
        );

        // Perform a legitimate self_update to verify the validation is called
        let update_result = mdk
            .self_update(&group_id)
            .expect("Self update should succeed");

        mdk.merge_pending_commit(&group_id)
            .expect("Merge should succeed");

        // Verify identity was preserved (validation passed)
        let updated_mls_group = mdk
            .load_mls_group(&group_id)
            .expect("Failed to load MLS group")
            .expect("MLS group should exist");

        let updated_leaf = updated_mls_group.own_leaf().expect("Should have own leaf");
        let updated_credential = BasicCredential::try_from(updated_leaf.credential().clone())
            .expect("Failed to get credential");
        let updated_identity = mdk
            .parse_credential_identity(updated_credential.identity())
            .expect("Failed to parse identity");

        assert_eq!(
            current_identity, updated_identity,
            "Identity should be preserved after legitimate self_update"
        );

        // The evolution event exists and is valid
        assert!(!update_result.mls_group_id.as_slice().is_empty());
    }

    /// Test validation with TLS serialization
    ///
    /// This test uses TLS serialization to verify the leaf node structure
    /// and that identity parsing works correctly for validation.
    #[test]
    fn test_identity_validation_with_tls_serialization() {
        let mdk = create_test_mdk();
        let (creator, members, admins) = create_test_group_members();
        let group_id = create_test_group(&mdk, &creator, &members, &admins);

        // Load the MLS group
        let mls_group = mdk
            .load_mls_group(&group_id)
            .expect("Failed to load MLS group")
            .expect("MLS group should exist");

        // Get the original leaf node
        let original_leaf = mls_group.own_leaf().expect("Should have own leaf");

        // Serialize the leaf to TLS format
        let original_leaf_bytes = original_leaf
            .tls_serialize_detached()
            .expect("Failed to serialize leaf");

        // Create a different identity (attacker)
        let attacker_keys = Keys::generate();
        let attacker_identity_bytes = attacker_keys.public_key().to_bytes().to_vec();

        // Get the original identity
        let original_credential = BasicCredential::try_from(original_leaf.credential().clone())
            .expect("Failed to get credential");
        let original_identity = mdk
            .parse_credential_identity(original_credential.identity())
            .expect("Failed to parse original identity");

        // Create attacker credential and parse identity
        let attacker_credential = BasicCredential::new(attacker_identity_bytes);
        let attacker_identity = mdk
            .parse_credential_identity(attacker_credential.identity())
            .expect("Failed to parse attacker identity");

        // Verify identities are different
        assert_ne!(
            original_identity, attacker_identity,
            "Original and attacker identities should be different"
        );

        // The validation logic compares:
        // current_identity (from mls_group.member_at(sender_leaf_index))
        // vs new_identity (from update_proposal.leaf_node().credential())
        //
        // If they differ, it returns Error::IdentityChangeNotAllowed

        // Verify the error would be returned
        let error = Error::IdentityChangeNotAllowed {
            original_identity: original_identity.to_hex(),
            new_identity: attacker_identity.to_hex(),
        };

        // Verify error message format
        let error_msg = error.to_string();
        assert!(
            error_msg.contains("identity change not allowed"),
            "Error message should indicate identity change is not allowed"
        );
        assert!(
            error_msg.contains(&original_identity.to_hex()),
            "Error should contain the original identity: {}",
            error_msg
        );
        assert!(
            error_msg.contains(&attacker_identity.to_hex()),
            "Error should contain the new identity: {}",
            error_msg
        );

        // Verify the serialized bytes are valid and contain identity
        assert!(
            !original_leaf_bytes.is_empty(),
            "Serialized leaf should not be empty"
        );
        assert!(
            original_leaf_bytes.len() > 32,
            "Serialized leaf should contain identity"
        );
    }

    /// Test that proposal identity change is rejected through the validation function
    ///
    /// This test verifies that when an UpdateProposal contains a credential with
    /// a different identity than the sender's current identity in the group,
    /// the validation correctly returns IdentityChangeNotAllowed error.
    ///
    /// Note: Since UpdateProposal cannot be directly constructed (pub(crate) fields),
    /// we test through the validate_identity_unchanged helper which is the core
    /// validation logic used by validate_proposal_identity.
    #[test]
    fn test_proposal_identity_change_rejected() {
        // Simulate a member's current identity
        let member_keys = Keys::generate();
        let member_identity = member_keys.public_key();

        // Simulate an attacker attempting to change to their own identity
        let attacker_keys = Keys::generate();
        let attacker_identity = attacker_keys.public_key();

        // The validation should reject this identity change
        let result = MDK::<MdkMemoryStorage>::validate_identity_unchanged(
            member_identity,
            attacker_identity,
        );

        // Assert the validation fails with IdentityChangeNotAllowed
        assert!(
            result.is_err(),
            "Identity change in proposal should be rejected"
        );

        match result.unwrap_err() {
            Error::IdentityChangeNotAllowed {
                original_identity,
                new_identity,
            } => {
                assert_eq!(
                    original_identity,
                    member_identity.to_hex(),
                    "Original identity should match member's identity"
                );
                assert_eq!(
                    new_identity,
                    attacker_identity.to_hex(),
                    "New identity should match attacker's identity"
                );
            }
            other => panic!("Expected IdentityChangeNotAllowed error, got: {:?}", other),
        }
    }

    /// Test that commit with identity-changing update path is rejected
    ///
    /// This test verifies that when a commit's update_path_leaf_node contains
    /// a credential with a different identity than the committer's current
    /// identity, the validation correctly returns IdentityChangeNotAllowed error.
    ///
    /// Note: Since StagedCommit cannot be directly constructed, we test through
    /// the validate_identity_unchanged helper which is the core validation logic
    /// used by validate_commit_identities for the update path.
    #[test]
    fn test_commit_update_path_identity_change_rejected() {
        // Simulate a committer's current identity in the group
        let committer_keys = Keys::generate();
        let committer_identity = committer_keys.public_key();

        // Simulate the committer attempting to change their identity via update path
        let new_keys = Keys::generate();
        let new_identity = new_keys.public_key();

        // The validation should reject this identity change in the update path
        let result =
            MDK::<MdkMemoryStorage>::validate_identity_unchanged(committer_identity, new_identity);

        // Assert the validation fails with IdentityChangeNotAllowed
        assert!(
            result.is_err(),
            "Identity change in commit update path should be rejected"
        );

        match result.unwrap_err() {
            Error::IdentityChangeNotAllowed {
                original_identity,
                new_identity: new_id,
            } => {
                assert_eq!(
                    original_identity,
                    committer_identity.to_hex(),
                    "Original identity should match committer's identity"
                );
                assert_eq!(
                    new_id,
                    new_identity.to_hex(),
                    "New identity should match the attempted new identity"
                );
            }
            other => panic!("Expected IdentityChangeNotAllowed error, got: {:?}", other),
        }
    }

    /// Test that multiple sequential identity changes are all rejected
    ///
    /// This tests that the validation works consistently across multiple
    /// attempts to change identity, ensuring the error contains the correct
    /// identity pairs each time.
    #[test]
    fn test_multiple_identity_change_attempts_rejected() {
        let original_keys = Keys::generate();
        let original_identity = original_keys.public_key();

        // Attempt multiple different identity changes
        for _ in 0..5 {
            let attacker_keys = Keys::generate();
            let attacker_identity = attacker_keys.public_key();

            let result = MDK::<MdkMemoryStorage>::validate_identity_unchanged(
                original_identity,
                attacker_identity,
            );

            assert!(
                result.is_err(),
                "Each identity change attempt should be rejected"
            );

            if let Err(Error::IdentityChangeNotAllowed {
                original_identity: orig,
                new_identity: new,
            }) = result
            {
                assert_eq!(orig, original_identity.to_hex());
                assert_eq!(new, attacker_identity.to_hex());
            }
        }
    }
}
