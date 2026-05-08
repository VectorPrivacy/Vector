//! Main message processing orchestration
//!
//! This module contains the main entry points for processing MLS messages.

use mdk_storage_traits::MdkStorageProvider;
use mdk_storage_traits::groups::types as group_types;
use mdk_storage_traits::messages::types as message_types;
use nostr::{Event, Timestamp};
use openmls::group::{ProcessMessageError, ValidationError};
use openmls::prelude::{
    ContentType, MlsGroup, MlsMessageIn, ProcessedMessage, ProcessedMessageContent, Proposal,
};
use tls_codec::Deserialize as TlsDeserialize;

use crate::MDK;
use crate::error::Error;

use super::{MessageProcessingResult, Result};

impl<Storage> MDK<Storage>
where
    Storage: MdkStorageProvider,
{
    /// Processes an incoming MLS message
    ///
    /// This internal function handles the MLS protocol-level message processing:
    /// 1. Deserializes the MLS message
    /// 2. Validates the message's group ID
    /// 3. Processes the message according to its type
    /// 4. Handles any resulting group state changes
    ///
    /// # Arguments
    ///
    /// * `group` - The MLS group the message belongs to
    /// * `message_bytes` - The serialized MLS message to process
    ///
    /// # Returns
    ///
    /// * `Ok(ProcessedMessage)` - The processed message including sender and credential info
    /// * `Err(Error)` - If message processing fails
    pub(super) fn process_mls_message(
        &self,
        group: &mut MlsGroup,
        message_bytes: &[u8],
    ) -> Result<ProcessedMessage> {
        let mls_message = MlsMessageIn::tls_deserialize_exact(message_bytes)?;
        let protocol_message = mls_message.try_into_protocol_message()?;

        // Return error if group ID doesn't match
        if protocol_message.group_id() != group.group_id() {
            return Err(Error::ProtocolGroupIdMismatch);
        }

        // Capture epoch in case we need it for error reporting
        let msg_epoch = protocol_message.epoch().as_u64();
        let content_type = protocol_message.content_type();

        tracing::debug!(
            target: "mdk_core::messages::process_mls_message",
            "Received MLS message (epoch={}, content_type={:?})",
            msg_epoch,
            content_type
        );

        let processed_message = match group.process_message(&self.provider, protocol_message) {
            Ok(processed_message) => processed_message,
            Err(ProcessMessageError::ValidationError(ValidationError::WrongEpoch)) => {
                return Err(Error::ProcessMessageWrongEpoch(msg_epoch));
            }
            Err(ProcessMessageError::ValidationError(ValidationError::CannotDecryptOwnMessage)) => {
                // If this is a commit message and we have a pending commit, it might be our own commit
                // that we are trying to process after a rollback. In this case, we should try to
                // merge the pending commit instead of decrypting the message.
                if content_type == ContentType::Commit && group.pending_commit().is_some() {
                    return Err(Error::OwnCommitPending);
                }
                return Err(Error::CannotDecryptOwnMessage);
            }

            Err(e) => {
                tracing::error!(target: "mdk_core::messages::process_mls_message", "Error processing MLS message");
                return Err(e.into());
            }
        };

        tracing::debug!(
            target: "mdk_core::messages::process_mls_message",
            "Processed MLS message (epoch={}, content_type={:?})",
            msg_epoch,
            content_type
        );

        Ok(processed_message)
    }

    /// Processes the decrypted message content based on its type
    ///
    /// This private method processes the decrypted MLS message and handles the
    /// different message types (application messages, proposals, commits, etc.).
    ///
    /// # Arguments
    ///
    /// * `group` - The group metadata from storage
    /// * `mls_group` - The MLS group instance (mutable for potential state changes)
    /// * `message_bytes` - The decrypted message bytes
    /// * `event` - The wrapper Nostr event
    ///
    /// # Returns
    ///
    /// * `Ok(MessageProcessingResult)` - The result based on message type
    /// * `Err(Error)` - If message processing fails
    pub(super) fn dispatch_by_content_type(
        &self,
        group: group_types::Group,
        mls_group: &mut MlsGroup,
        message_bytes: &[u8],
        event: &Event,
    ) -> Result<MessageProcessingResult> {
        match self.process_mls_message(mls_group, message_bytes) {
            Ok(processed_mls_message) => {
                // Clone the sender's credential and sender for validation before consuming
                let sender_credential = processed_mls_message.credential().clone();
                let message_sender = processed_mls_message.sender().clone();

                match processed_mls_message.into_content() {
                    ProcessedMessageContent::ApplicationMessage(application_message) => {
                        Ok(MessageProcessingResult::ApplicationMessage(
                            self.process_application_message(
                                group,
                                mls_group.epoch().as_u64(),
                                event,
                                application_message,
                                sender_credential,
                            )?,
                        ))
                    }
                    ProcessedMessageContent::ProposalMessage(staged_proposal) => {
                        self.process_proposal(mls_group, event, *staged_proposal)
                    }
                    ProcessedMessageContent::StagedCommitMessage(staged_commit) => {
                        self.process_commit(mls_group, event, *staged_commit, &message_sender)?;
                        Ok(MessageProcessingResult::Commit {
                            mls_group_id: group.mls_group_id.clone(),
                        })
                    }
                    ProcessedMessageContent::ExternalJoinProposalMessage(
                        _external_join_proposal,
                    ) => {
                        // Save a processed message so we don't reprocess
                        let processed_message = super::create_processed_message_record(
                            event.id,
                            None,
                            Some(mls_group.epoch().as_u64()),
                            Some(group.mls_group_id.clone()),
                            message_types::ProcessedMessageState::Processed,
                            None,
                        );

                        self.save_processed_message_record(processed_message)?;

                        Ok(MessageProcessingResult::ExternalJoinProposal {
                            mls_group_id: group.mls_group_id.clone(),
                        })
                    }
                }
            }
            Err(Error::OwnCommitPending) => {
                // This is our own commit that we can't decrypt via process_message,
                // but we have a pending commit locally. Merge it.
                tracing::debug!(
                    target: "mdk_core::messages::dispatch_by_content_type",
                    "Merging pending own commit after rollback/reprocess"
                );

                // Snapshot current state before applying commit (for rollback support)
                if self
                    .epoch_snapshots
                    .create_snapshot(
                        self.storage(),
                        &group.mls_group_id,
                        mls_group.epoch().as_u64(),
                        &event.id,
                        event.created_at.as_secs(),
                    )
                    .is_err()
                {
                    tracing::warn!(
                        target: "mdk_core::messages::dispatch_by_content_type",
                        "Failed to create snapshot for pending commit merge"
                    );
                    return Err(Error::Message(
                        "Failed to create epoch snapshot".to_string(),
                    ));
                }

                // Detect self-update before merging (same logic as MDK::merge_pending_commit)
                let is_self_update = mls_group.pending_commit().is_some_and(|staged_commit| {
                    let has_update_signal = staged_commit.update_path_leaf_node().is_some()
                        || staged_commit.update_proposals().next().is_some();
                    let no_non_update_proposals = staged_commit
                        .queued_proposals()
                        .all(|p| matches!(p.proposal(), Proposal::Update(_)));
                    has_update_signal && no_non_update_proposals
                });

                mls_group
                    .merge_pending_commit(&self.provider)
                    .map_err(|_e| Error::Message("Failed to merge pending commit".to_string()))?;

                // Handle post-commit operations

                // Check if the local member was removed by this commit
                if mls_group.own_leaf().is_none() {
                    return match self.handle_local_member_eviction(&group.mls_group_id, event) {
                        Ok(_) => Ok(MessageProcessingResult::Commit {
                            mls_group_id: group.mls_group_id.clone(),
                        }),
                        Err(e) => Err(e),
                    };
                }

                // Save exporter secret for the new epoch
                self.exporter_secret(&group.mls_group_id)?;

                // Sync the stored group metadata with the updated MLS group state
                self.sync_group_metadata_from_mls(&group.mls_group_id)?;

                // Update self-update tracking if this was a self-update commit
                if is_self_update {
                    let mut stored_group = self
                        .get_group(&group.mls_group_id)?
                        .ok_or(Error::GroupNotFound)?;
                    stored_group.self_update_state =
                        group_types::SelfUpdateState::CompletedAt(Timestamp::now());
                    self.storage()
                        .save_group(stored_group)
                        .map_err(|e| Error::Group(e.to_string()))?;
                }

                // Save a processed message so we don't reprocess
                let processed_message = super::create_processed_message_record(
                    event.id,
                    None,
                    Some(mls_group.epoch().as_u64()),
                    Some(group.mls_group_id.clone()),
                    message_types::ProcessedMessageState::ProcessedCommit,
                    None,
                );

                self.save_processed_message_record(processed_message)?;

                Ok(MessageProcessingResult::Commit {
                    mls_group_id: group.mls_group_id.clone(),
                })
            }
            Err(e) => Err(e),
        }
    }

    /// Processes an incoming encrypted Nostr event containing an MLS message
    ///
    /// This is the main entry point for processing received messages. The function orchestrates
    /// the message processing workflow by delegating to specialized private methods:
    /// 0. Checks if the message was already processed (deduplication)
    /// 1. Validates the event and extracts group ID
    /// 2. Loads the group and decrypts the message content
    /// 3. Processes the decrypted message based on its type
    /// 4. Handles errors with specialized recovery logic
    ///
    /// Early validation and decryption failures are persisted to prevent expensive reprocessing
    /// of the same invalid events.
    ///
    /// # Arguments
    ///
    /// * `event` - The received Nostr event containing the encrypted MLS message
    ///
    /// # Returns
    ///
    /// * `Ok(MessageProcessingResult)` - Result indicating the type of message processed
    /// * `Err(Error)` - If message processing fails
    pub fn process_message(&self, event: &Event) -> Result<MessageProcessingResult> {
        // Step 0: Check if already processed (deduplication)
        if let Some(processed) = self
            .storage()
            .find_processed_message_by_event_id(&event.id)
            .map_err(|_e| {
                Error::Message("Storage error while checking for processed message".to_string())
            })?
        {
            tracing::debug!(
                target: "mdk_core::messages::process_message",
                "Message already processed with state: {:?}",
                processed.state
            );

            // Block reprocessing for Failed and EpochInvalidated states
            // Other states (Created, Processed, ProcessedCommit) should continue
            // to allow normal message flow (e.g., processing own messages from relay)
            let is_failed = processed.state == message_types::ProcessedMessageState::Failed;
            let is_epoch_invalidated =
                processed.state == message_types::ProcessedMessageState::EpochInvalidated;

            if is_failed || is_epoch_invalidated {
                match self.extract_mls_group_id_from_event(event) {
                    Some(mls_group_id) => {
                        tracing::debug!(
                            target: "mdk_core::messages::process_message",
                            "Returning Unprocessable for previously failed/invalidated message with extracted group_id"
                        );
                        return Ok(MessageProcessingResult::Unprocessable { mls_group_id });
                    }
                    None => {
                        tracing::debug!(
                            target: "mdk_core::messages::process_message",
                            "Returning PreviouslyFailed for message without extractable group_id"
                        );
                        return Ok(MessageProcessingResult::PreviouslyFailed);
                    }
                }
            }

            // Allow Retryable messages to be reprocessed after rollback
            if processed.state == message_types::ProcessedMessageState::Retryable {
                tracing::info!(
                    target: "mdk_core::messages::process_message",
                    "Retrying previously failed message after rollback (event_id: {})",
                    event.id
                );
                // Continue to processing - don't return early
            }
        }

        // Step 1: Validate event and extract group ID
        let nostr_group_id = match self
            .validate_event(event)
            .and_then(|()| self.extract_nostr_group_id(event))
        {
            Ok(id) => id,
            Err(e) => {
                // Save failed processing record to prevent reprocessing
                // Don't fail if we can't save the failure record - log and continue
                if let Err(_save_err) = self.record_failure(event.id, &e, None, None) {
                    tracing::warn!(
                        target: "mdk_core::messages::process_message",
                        "Failed to persist failure record; error details redacted"
                    );
                }
                return Err(e);
            }
        };

        // Step 2: Load group and decrypt message
        let (group, mut mls_group, message_bytes) =
            match self.decrypt_message(nostr_group_id, event) {
                Ok(result) => result,
                Err(e) => {
                    // Save failed processing record to prevent reprocessing
                    // Don't fail if we can't save the failure record - log and continue
                    //
                    // For decryption failures, we look up the group to get mls_group_id for
                    // retry tracking, but we pass epoch=None because we don't know what
                    // epoch the message was encrypted for. Messages with epoch=None and
                    // state=Failed are candidates for retry after rollback.
                    let mls_group_id = self
                        .storage()
                        .find_group_by_nostr_group_id(&nostr_group_id)
                        .ok()
                        .flatten()
                        .map(|g| g.mls_group_id);
                    if let Err(_save_err) =
                        self.record_failure(event.id, &e, mls_group_id.as_ref(), None)
                    {
                        tracing::warn!(
                            target: "mdk_core::messages::process_message",
                            "Failed to persist failure record; error details redacted"
                        );
                    }
                    return Err(e);
                }
            };

        // Step 3: Process the decrypted message
        match self.dispatch_by_content_type(group.clone(), &mut mls_group, &message_bytes, event) {
            Ok(result) => Ok(result),
            Err(error) => {
                // Step 4: Handle errors with specialized recovery logic
                self.handle_processing_error(error, event, &group)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use mdk_storage_traits::GroupId;
    use mdk_storage_traits::messages::types as message_types;
    use nostr::{EventBuilder, EventId, Keys, Kind, PublicKey, Tags, Timestamp};

    use crate::extension::NostrGroupDataExtension;
    use crate::groups::NostrGroupDataUpdate;
    use crate::test_util::*;
    use crate::tests::create_test_mdk;
    use mdk_storage_traits::groups::GroupStorage;
    use mdk_storage_traits::messages::MessageStorage;

    use super::MessageProcessingResult;

    #[test]
    fn test_message_processing_result_variants() {
        // Test that MessageProcessingResult variants can be created and matched
        let test_group_id = GroupId::from_slice(&[1, 2, 3, 4]);
        let now = Timestamp::now();
        let dummy_message = message_types::Message {
            id: EventId::all_zeros(),
            pubkey: PublicKey::from_hex(
                "8a9de562cbbed225b6ea0118dd3997a02df92c0bffd2224f71081a7450c3e549",
            )
            .unwrap(),
            kind: Kind::TextNote,
            mls_group_id: test_group_id.clone(),
            created_at: now,
            processed_at: now,
            content: "Test".to_string(),
            tags: Tags::new(),
            event: EventBuilder::new(Kind::TextNote, "Test").build(
                PublicKey::from_hex(
                    "8a9de562cbbed225b6ea0118dd3997a02df92c0bffd2224f71081a7450c3e549",
                )
                .unwrap(),
            ),
            wrapper_event_id: EventId::all_zeros(),
            state: message_types::MessageState::Processed,
            epoch: None,
        };

        let app_result = MessageProcessingResult::ApplicationMessage(dummy_message);
        let commit_result = MessageProcessingResult::Commit {
            mls_group_id: test_group_id.clone(),
        };
        let external_join_result = MessageProcessingResult::ExternalJoinProposal {
            mls_group_id: test_group_id.clone(),
        };
        let unprocessable_result = MessageProcessingResult::Unprocessable {
            mls_group_id: test_group_id.clone(),
        };
        // PendingProposal: for when a non-admin receiver stores a proposal without committing
        let pending_proposal_result = MessageProcessingResult::PendingProposal {
            mls_group_id: test_group_id.clone(),
        };
        // PreviouslyFailed: for when a message that previously failed cannot provide a group_id
        let previously_failed_result = MessageProcessingResult::PreviouslyFailed;

        // Test that we can match on variants
        match app_result {
            MessageProcessingResult::ApplicationMessage(_) => {}
            _ => panic!("Expected ApplicationMessage variant"),
        }

        match commit_result {
            MessageProcessingResult::Commit { .. } => {}
            _ => panic!("Expected Commit variant"),
        }

        match external_join_result {
            MessageProcessingResult::ExternalJoinProposal { .. } => {}
            _ => panic!("Expected ExternalJoinProposal variant"),
        }

        match unprocessable_result {
            MessageProcessingResult::Unprocessable { .. } => {}
            _ => panic!("Expected Unprocessable variant"),
        }

        match pending_proposal_result {
            MessageProcessingResult::PendingProposal { .. } => {}
            _ => panic!("Expected PendingProposal variant"),
        }

        match previously_failed_result {
            MessageProcessingResult::PreviouslyFailed => {}
            _ => panic!("Expected PreviouslyFailed variant"),
        }
    }

    #[test]
    fn test_merge_pending_commit_syncs_group_metadata() {
        let mdk = create_test_mdk();

        // Create test group members
        let creator_keys = Keys::generate();
        let member1_keys = Keys::generate();
        let member2_keys = Keys::generate();

        let creator_pk = creator_keys.public_key();
        let member1_pk = member1_keys.public_key();

        let members = vec![member1_keys.clone(), member2_keys.clone()];
        let admins = vec![creator_pk, member1_pk]; // Creator and member1 are admins

        // Create group
        let group_id = create_test_group(&mdk, &creator_keys, &members, &admins);

        // Get initial stored group state
        let initial_group = mdk
            .get_group(&group_id)
            .expect("Failed to get initial group")
            .expect("Initial group should exist");

        let initial_epoch = initial_group.epoch;
        let initial_name = initial_group.name.clone();

        // Create a commit by updating the group name
        let new_name = "Updated Group Name via MLS Commit".to_string();
        let update = NostrGroupDataUpdate::new().name(new_name.clone());
        let _update_result = mdk
            .update_group_data(&group_id, update)
            .expect("Failed to update group name");

        // Before merging commit - verify stored group still has old data
        let pre_merge_group = mdk
            .get_group(&group_id)
            .expect("Failed to get pre-merge group")
            .expect("Pre-merge group should exist");

        assert_eq!(
            pre_merge_group.name, initial_name,
            "Stored group name should still be old before merge"
        );
        assert_eq!(
            pre_merge_group.epoch, initial_epoch,
            "Stored group epoch should still be old before merge"
        );

        // Get MLS group state before merge (epoch shouldn't advance until merge)
        let pre_merge_mls_group = mdk
            .load_mls_group(&group_id)
            .expect("Failed to load pre-merge MLS group")
            .expect("Pre-merge MLS group should exist");

        let pre_merge_mls_epoch = pre_merge_mls_group.epoch().as_u64();
        assert_eq!(
            pre_merge_mls_epoch, initial_epoch,
            "MLS group epoch should not advance until commit is merged"
        );

        // This is the key test: merge_pending_commit should sync the stored group metadata
        mdk.merge_pending_commit(&group_id)
            .expect("Failed to merge pending commit");

        // Verify stored group is now synchronized after merge
        let post_merge_group = mdk
            .get_group(&group_id)
            .expect("Failed to get post-merge group")
            .expect("Post-merge group should exist");

        // Verify epoch is synchronized
        assert!(
            post_merge_group.epoch > initial_epoch,
            "Stored group epoch should advance after merge"
        );

        // Verify extension data is synchronized
        let post_merge_mls_group = mdk
            .load_mls_group(&group_id)
            .expect("Failed to load post-merge MLS group")
            .expect("Post-merge MLS group should exist");

        let group_data = NostrGroupDataExtension::from_group(&post_merge_mls_group)
            .expect("Failed to get group data extension");

        assert_eq!(
            post_merge_group.name, group_data.name,
            "Stored group name should match extension after merge"
        );
        assert_eq!(
            post_merge_group.name, new_name,
            "Stored group name should be updated after merge"
        );
        assert_eq!(
            post_merge_group.description, group_data.description,
            "Stored group description should match extension"
        );
        assert_eq!(
            post_merge_group.admin_pubkeys, group_data.admins,
            "Stored group admins should match extension"
        );

        // Test that the sync function itself works correctly by manually de-syncing and re-syncing
        let mut manually_desync_group = post_merge_group.clone();
        manually_desync_group.name = "Manually Corrupted Name".to_string();
        manually_desync_group.epoch = initial_epoch;
        mdk.storage()
            .save_group(manually_desync_group)
            .expect("Failed to save corrupted group");

        // Verify it's out of sync
        let corrupted_group = mdk
            .get_group(&group_id)
            .expect("Failed to get corrupted group")
            .expect("Corrupted group should exist");

        assert_eq!(
            corrupted_group.name, "Manually Corrupted Name",
            "Group should be manually corrupted"
        );
        assert_eq!(
            corrupted_group.epoch, initial_epoch,
            "Group epoch should be manually corrupted"
        );

        // Call sync function directly
        mdk.sync_group_metadata_from_mls(&group_id)
            .expect("Failed to sync group metadata");

        // Verify it's back in sync
        let re_synced_group = mdk
            .get_group(&group_id)
            .expect("Failed to get re-synced group")
            .expect("Re-synced group should exist");

        assert_eq!(
            re_synced_group.name, new_name,
            "Group name should be re-synced"
        );
        assert!(
            re_synced_group.epoch > initial_epoch,
            "Group epoch should be re-synced"
        );
        assert_eq!(
            re_synced_group.admin_pubkeys, group_data.admins,
            "Group admins should be re-synced"
        );
    }

    #[test]
    fn test_processing_own_commit_syncs_group_metadata() {
        let mdk = create_test_mdk();

        // Create test group
        let creator_keys = Keys::generate();
        let member1_keys = Keys::generate();
        let member2_keys = Keys::generate();

        let creator_pk = creator_keys.public_key();
        let member1_pk = member1_keys.public_key();

        let members = vec![member1_keys.clone(), member2_keys.clone()];
        let admins = vec![creator_pk, member1_pk];

        let group_id = create_test_group(&mdk, &creator_keys, &members, &admins);

        // Get initial state
        let initial_group = mdk
            .get_group(&group_id)
            .expect("Failed to get initial group")
            .expect("Initial group should exist");

        let initial_epoch = initial_group.epoch;

        // Create and merge a commit to update group name
        let new_name = "Updated Name for Own Commit Test".to_string();
        let update = NostrGroupDataUpdate::new().name(new_name.clone());
        let update_result = mdk
            .update_group_data(&group_id, update)
            .expect("Failed to update group name");

        mdk.merge_pending_commit(&group_id)
            .expect("Failed to merge pending commit");

        // Verify the commit event is marked as ProcessedCommit
        let commit_event_id = update_result.evolution_event.id;
        let processed_message = mdk
            .storage()
            .find_processed_message_by_event_id(&commit_event_id)
            .expect("Failed to find processed message")
            .expect("Processed message should exist");

        assert_eq!(
            processed_message.state,
            message_types::ProcessedMessageState::ProcessedCommit
        );

        // Manually corrupt the stored group to simulate desync
        let mut corrupted_group = initial_group.clone();
        corrupted_group.name = "Corrupted Name".to_string();
        corrupted_group.epoch = initial_epoch;
        mdk.storage()
            .save_group(corrupted_group)
            .expect("Failed to save corrupted group");

        // Verify it's out of sync
        let out_of_sync_group = mdk
            .get_group(&group_id)
            .expect("Failed to get out of sync group")
            .expect("Out of sync group should exist");

        assert_eq!(out_of_sync_group.name, "Corrupted Name");
        assert_eq!(out_of_sync_group.epoch, initial_epoch);

        // Process our own commit message - this should trigger sync even though it's marked as ProcessedCommit
        let message_result = mdk
            .process_message(&update_result.evolution_event)
            .expect("Failed to process own commit message");

        // Verify it returns Commit result (our fix should handle epoch mismatch errors)
        assert!(matches!(
            message_result,
            MessageProcessingResult::Commit { .. }
        ));

        // Most importantly: verify that processing our own commit synchronized the stored group metadata
        let synced_group = mdk
            .get_group(&group_id)
            .expect("Failed to get synced group")
            .expect("Synced group should exist");

        assert_eq!(
            synced_group.name, new_name,
            "Processing own commit should sync group name"
        );
        assert!(
            synced_group.epoch > initial_epoch,
            "Processing own commit should sync group epoch"
        );

        // Verify the stored group matches the MLS group state
        let mls_group = mdk
            .load_mls_group(&group_id)
            .expect("Failed to load MLS group")
            .expect("MLS group should exist");

        assert_eq!(
            synced_group.epoch,
            mls_group.epoch().as_u64(),
            "Stored and MLS group epochs should match"
        );

        let group_data = NostrGroupDataExtension::from_group(&mls_group)
            .expect("Failed to get group data extension");

        assert_eq!(
            synced_group.name, group_data.name,
            "Stored group name should match extension"
        );
        assert_eq!(
            synced_group.admin_pubkeys, group_data.admins,
            "Stored group admins should match extension"
        );
    }

    /// Test processing message multiple times (idempotency)
    #[test]
    fn test_process_message_idempotency() {
        let creator_mdk = create_test_mdk();
        let (creator, members, admins) = create_test_group_members();
        let group_id = create_test_group(&creator_mdk, &creator, &members, &admins);

        // Create a message
        let rumor = create_test_rumor(&creator, "Test idempotency");
        let event = creator_mdk
            .create_message(&group_id, rumor)
            .expect("Failed to create message");

        // Process the message once
        let result1 = creator_mdk.process_message(&event);
        assert!(
            result1.is_ok(),
            "First message processing should succeed: {:?}",
            result1.err()
        );

        // Process the same message again - should be idempotent
        let result2 = creator_mdk.process_message(&event);
        assert!(
            result2.is_ok(),
            "Second message processing should also succeed (idempotent): {:?}",
            result2.err()
        );

        // Both results should be consistent - true idempotency means
        // processing the same message multiple times produces consistent results
        assert!(
            result1.is_ok() && result2.is_ok(),
            "Message processing should be idempotent - both calls should succeed"
        );
    }

    /// Test duplicate message handling from multiple relays
    ///
    /// Validates that the same message received from multiple relays is processed
    /// only once and duplicates are handled gracefully.
    #[test]
    fn test_duplicate_message_from_multiple_relays() {
        let mdk = create_test_mdk();
        let (creator, members, admins) = create_test_group_members();
        let group_id = create_test_group(&mdk, &creator, &members, &admins);

        // Create a message
        let rumor = create_test_rumor(&creator, "Test message");
        let message_event = mdk
            .create_message(&group_id, rumor)
            .expect("Failed to create message");

        // Process the message for the first time
        let first_result = mdk.process_message(&message_event);
        assert!(
            first_result.is_ok(),
            "First message processing should succeed"
        );

        // Simulate receiving the same message from a different relay
        // Process the exact same message again
        // OpenMLS is idempotent - processing the same message twice should succeed
        let second_result = mdk.process_message(&message_event);
        assert!(
            second_result.is_ok(),
            "OpenMLS should idempotently handle duplicate message processing: {:?}",
            second_result.err()
        );

        // Verify we still only have one message (no duplication)
        let messages = mdk
            .get_messages(&group_id, None)
            .expect("Failed to get messages");
        assert_eq!(
            messages.len(),
            1,
            "Should still have only 1 message after duplicate processing"
        );

        // Verify group state is consistent
        let group = mdk
            .get_group(&group_id)
            .expect("Failed to get group")
            .expect("Group should exist");
        assert!(
            group.last_message_id.is_some(),
            "Group should have last message ID"
        );
    }

    /// Single-client message idempotency
    ///
    /// Tests that messages can be processed multiple times without duplication
    /// and that message retrieval works correctly.
    #[test]
    fn test_single_client_message_idempotency() {
        let mdk = create_test_mdk();
        let (creator, members, admins) = create_test_group_members();
        let group_id = create_test_group(&mdk, &creator, &members, &admins);

        // Create three messages in order
        let rumor1 = create_test_rumor(&creator, "Message 1");
        let message1 = mdk
            .create_message(&group_id, rumor1)
            .expect("Failed to create message 1");

        let rumor2 = create_test_rumor(&creator, "Message 2");
        let message2 = mdk
            .create_message(&group_id, rumor2)
            .expect("Failed to create message 2");

        let rumor3 = create_test_rumor(&creator, "Message 3");
        let message3 = mdk
            .create_message(&group_id, rumor3)
            .expect("Failed to create message 3");

        // Process messages in different order: 3, 1, 2
        // All three messages are in the same epoch, so they should all process
        let result3 = mdk.process_message(&message3);
        let result1 = mdk.process_message(&message1);
        let result2 = mdk.process_message(&message2);

        // All should succeed
        assert!(result3.is_ok(), "Message 3 should process successfully");
        assert!(result1.is_ok(), "Message 1 should process successfully");
        assert!(result2.is_ok(), "Message 2 should process successfully");

        // Verify all messages are stored
        let messages = mdk
            .get_messages(&group_id, None)
            .expect("Failed to get messages");
        assert_eq!(
            messages.len(),
            3,
            "Should have all 3 messages regardless of processing order"
        );

        // Verify messages can be retrieved by their IDs
        for msg in &messages {
            let retrieved = mdk
                .get_message(&msg.mls_group_id, &msg.id)
                .expect("Failed to get message")
                .expect("Message should exist");
            assert_eq!(retrieved.id, msg.id, "Retrieved message should match");
        }
    }

    /// Test message processing order independence
    ///
    /// Validates that the storage and retrieval of messages works correctly
    /// regardless of the order in which messages are processed.
    #[test]
    fn test_message_processing_order_independence() {
        let mdk = create_test_mdk();
        let (creator, members, admins) = create_test_group_members();
        let group_id = create_test_group(&mdk, &creator, &members, &admins);

        // Create messages with explicit timestamps
        let mut messages_created = Vec::new();
        for i in 1..=5 {
            let rumor = create_test_rumor(&creator, &format!("Message {}", i));
            let message_event = mdk
                .create_message(&group_id, rumor)
                .unwrap_or_else(|_| panic!("Failed to create message {}", i));
            messages_created.push((i, message_event));
        }

        // Process messages in reverse order (simulating network delays)
        for (i, message_event) in messages_created.iter().rev() {
            let result = mdk.process_message(message_event);
            assert!(result.is_ok(), "Processing message {} should succeed", i);
        }

        // Verify all messages are stored
        let stored_messages = mdk
            .get_messages(&group_id, None)
            .expect("Failed to get messages");
        assert_eq!(stored_messages.len(), 5, "Should have all 5 messages");

        // Messages should be retrievable regardless of processing order
        for (i, _) in &messages_created {
            let content = format!("Message {}", i);
            let found = stored_messages.iter().any(|m| m.content == content);
            assert!(found, "Should find message with content '{}'", content);
        }
    }

    #[test]
    fn test_extended_offline_period_sync() {
        let alice_keys = Keys::generate();
        let bob_keys = Keys::generate();

        let alice_mdk = create_test_mdk();
        let bob_mdk = create_test_mdk();

        // Create key packages
        let bob_key_package = create_key_package_event(&bob_mdk, &bob_keys);

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

        // Simulate Bob going offline - Alice sends multiple messages
        let mut alice_messages = Vec::new();
        for i in 0..5 {
            let rumor = create_test_rumor(&alice_keys, &format!("Message {} while Bob offline", i));
            let message_event = alice_mdk
                .create_message(&group_id, rumor)
                .expect("Alice should create message");
            alice_messages.push(message_event);
        }

        // Bob comes back online and processes all messages
        for message_event in &alice_messages {
            let result = bob_mdk.process_message(message_event);
            assert!(
                result.is_ok(),
                "Bob should process offline message: {:?}",
                result.err()
            );
        }

        // Verify Bob received all messages
        let bob_messages = bob_mdk
            .get_messages(&group_id, None)
            .expect("Bob should get messages");

        assert_eq!(
            bob_messages.len(),
            5,
            "Bob should have all 5 messages after sync"
        );

        // Verify all messages are present (order may vary with equal timestamps)
        let bob_contents: Vec<&str> = bob_messages.iter().map(|m| m.content.as_str()).collect();
        for i in 0..5 {
            let expected = format!("Message {} while Bob offline", i);
            assert!(
                bob_contents
                    .iter()
                    .any(|&content| content.contains(&expected)),
                "Should contain: {}",
                expected
            );
        }
    }

    /// Device Synchronization After Member Changes
    ///
    /// Validates that when one device makes member changes (add/remove),
    /// other devices can properly process and synchronize those changes.
    #[test]
    fn test_device_sync_after_member_changes() {
        let alice_keys = Keys::generate();
        let bob_keys = Keys::generate();

        let alice_device1 = create_test_mdk();
        let bob_mdk = create_test_mdk();

        // Create key packages
        let bob_key_package = create_key_package_event(&bob_mdk, &bob_keys);

        // Alice device 1 creates group with Bob
        let admin_pubkeys = vec![alice_keys.public_key()];
        let config = create_nostr_group_config_data(admin_pubkeys);

        let create_result = alice_device1
            .create_group(&alice_keys.public_key(), vec![bob_key_package], config)
            .expect("Alice device 1 should create group");

        let group_id = create_result.group.mls_group_id.clone();

        alice_device1
            .merge_pending_commit(&group_id)
            .expect("Alice device 1 should merge commit");

        // Bob joins
        let bob_welcome_rumor = &create_result.welcome_rumors[0];
        let bob_welcome = bob_mdk
            .process_welcome(&nostr::EventId::all_zeros(), bob_welcome_rumor)
            .expect("Bob should process welcome");

        bob_mdk
            .accept_welcome(&bob_welcome)
            .expect("Bob should accept welcome");

        // Verify initial state - Alice device 1 and Bob both see 2 members
        let alice_d1_members = alice_device1
            .get_members(&group_id)
            .expect("Alice device 1 should get members");
        let bob_members = bob_mdk
            .get_members(&group_id)
            .expect("Bob should get members");

        assert_eq!(
            alice_d1_members.len(),
            2,
            "Alice device 1 should see 2 members"
        );
        assert_eq!(bob_members.len(), 2, "Bob should see 2 members");

        // Alice device 1 sends a message
        let rumor1 = create_test_rumor(&alice_keys, "Message from device 1");
        let message1 = alice_device1
            .create_message(&group_id, rumor1)
            .expect("Alice device 1 should create message");

        // Bob processes the message
        bob_mdk
            .process_message(&message1)
            .expect("Bob should process message");

        // Alice adds a new member (Charlie)
        let charlie_keys = Keys::generate();
        let charlie_mdk = create_test_mdk();
        let charlie_key_package = create_key_package_event(&charlie_mdk, &charlie_keys);

        let add_result = alice_device1
            .add_members(&group_id, &[charlie_key_package])
            .expect("Alice should add Charlie");

        alice_device1
            .merge_pending_commit(&group_id)
            .expect("Alice should merge commit");

        // Bob processes the member addition commit
        bob_mdk
            .process_message(&add_result.evolution_event)
            .expect("Bob should process member addition");

        // Verify Bob's member list is synchronized
        let bob_updated_members = bob_mdk
            .get_members(&group_id)
            .expect("Bob should get updated members");

        assert_eq!(
            bob_updated_members.len(),
            3,
            "Bob should see Charlie was added"
        );
        assert!(
            bob_updated_members.contains(&charlie_keys.public_key()),
            "Bob should see Charlie in member list"
        );

        // Verify Bob received the message
        let bob_messages = bob_mdk
            .get_messages(&group_id, None)
            .expect("Bob should get messages");

        assert_eq!(bob_messages.len(), 1, "Bob should have 1 message");
        assert!(
            bob_messages[0].content.contains("Message from device 1"),
            "Bob should have message from Alice device 1"
        );
    }

    /// Message Processing Across Epoch Transitions
    ///
    /// Validates that devices can process messages from different epochs correctly,
    /// especially when syncing after being offline during epoch transitions.
    #[test]
    fn test_message_processing_across_epochs() {
        let alice_keys = Keys::generate();
        let bob_keys = Keys::generate();
        let charlie_keys = Keys::generate();

        let alice_mdk = create_test_mdk();
        let bob_mdk = create_test_mdk();
        let charlie_mdk = create_test_mdk();

        // Create key packages
        let bob_key_package = create_key_package_event(&bob_mdk, &bob_keys);

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

        // Get initial epoch
        let epoch0 = alice_mdk
            .get_group(&group_id)
            .expect("Should get group")
            .expect("Group should exist")
            .epoch;

        // Alice sends message in epoch 0
        let rumor0 = create_test_rumor(&alice_keys, "Message in epoch 0");
        let message0 = alice_mdk
            .create_message(&group_id, rumor0)
            .expect("Alice should create message in epoch 0");

        // Advance epoch by adding Charlie
        let charlie_key_package = create_key_package_event(&charlie_mdk, &charlie_keys);
        let add_result = alice_mdk
            .add_members(&group_id, &[charlie_key_package])
            .expect("Alice should add Charlie");

        let add_commit_event = add_result.evolution_event.clone();

        alice_mdk
            .merge_pending_commit(&group_id)
            .expect("Alice should merge commit");

        // Verify epoch advanced
        let epoch1 = alice_mdk
            .get_group(&group_id)
            .expect("Should get group")
            .expect("Group should exist")
            .epoch;

        assert!(epoch1 > epoch0, "Epoch should have advanced");

        // Alice sends message in epoch 1
        let rumor1 = create_test_rumor(&alice_keys, "Message in epoch 1");
        let message1 = alice_mdk
            .create_message(&group_id, rumor1)
            .expect("Alice should create message in epoch 1");

        // Bob processes message from epoch 0
        bob_mdk
            .process_message(&message0)
            .expect("Bob should process message from epoch 0");

        // Bob processes the commit to advance to epoch 1

        bob_mdk
            .process_message(&add_commit_event)
            .expect("Bob should process commit to advance epoch");

        // Bob processes message from epoch 1
        bob_mdk
            .process_message(&message1)
            .expect("Bob should process message from epoch 1");

        let bob_messages = bob_mdk
            .get_messages(&group_id, None)
            .expect("Bob should get messages");

        assert!(
            !bob_messages.is_empty(),
            "Bob should have messages from both epochs"
        );
        assert!(
            bob_messages
                .iter()
                .any(|m| m.content.contains("Message in epoch 0")),
            "Bob should have message from epoch 0"
        );
        assert!(
            bob_messages
                .iter()
                .any(|m| m.content.contains("Message in epoch 1")),
            "Bob should have message from epoch 1"
        );
    }
}
