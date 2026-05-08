//! Error recovery and failure persistence
//!
//! This module handles error recovery logic and saving failed message records.

use mdk_storage_traits::groups::types as group_types;
use mdk_storage_traits::messages::types as message_types;
use mdk_storage_traits::{GroupId, MdkStorageProvider};
use nostr::{Event, EventId, Timestamp};

use crate::MDK;
use crate::error::Error;

use super::{MessageProcessingResult, Result};

impl<Storage> MDK<Storage>
where
    Storage: MdkStorageProvider,
{
    /// Sanitizes an error into a safe-to-expose failure reason
    ///
    /// This function maps internal errors to generic, safe-to-expose failure categories
    /// that don't leak implementation details or sensitive information.
    ///
    /// # Arguments
    ///
    /// * `error` - The internal error to sanitize
    ///
    /// # Returns
    ///
    /// A sanitized string suitable for external exposure
    pub(super) fn sanitize_error_reason(error: &Error) -> &'static str {
        match error {
            Error::UnexpectedEvent { .. } => "invalid_event_type",
            Error::MissingGroupIdTag => "invalid_event_format",
            Error::InvalidGroupIdFormat(_) => "invalid_event_format",
            Error::MultipleGroupIdTags(_) => "invalid_event_format",
            Error::InvalidTimestamp(_) => "invalid_event_format",
            Error::GroupNotFound => "group_not_found",
            Error::CannotDecryptOwnMessage => "own_message",
            Error::AuthorMismatch => "authentication_failed",
            Error::CommitFromNonAdmin => "authorization_failed",
            _ => "processing_failed",
        }
    }

    /// Records a failed message processing attempt to prevent reprocessing
    ///
    /// Saves a failed processing record with a sanitized error reason.
    /// Falls back to any existing record's context for fields not provided.
    ///
    /// # Arguments
    ///
    /// * `event_id` - The event ID of the failed message
    /// * `error` - The error that caused the failure (will be sanitized for storage)
    /// * `mls_group_id` - Optional group ID for retry tracking
    /// * `epoch` - Optional epoch (pass None for decryption failures where epoch is unknown)
    pub(super) fn record_failure(
        &self,
        event_id: EventId,
        error: &Error,
        mls_group_id: Option<&GroupId>,
        epoch: Option<u64>,
    ) -> Result<()> {
        let sanitized_reason = Self::sanitize_error_reason(error);

        tracing::warn!(
            target: "mdk_core::messages::record_failure",
            "Message processing failed for event {}: {}",
            event_id,
            sanitized_reason
        );

        // Try to fetch existing record to preserve message_event_id
        let existing_record = match self.storage().find_processed_message_by_event_id(&event_id) {
            Ok(record) => record,
            Err(_e) => {
                tracing::warn!(
                    target: "mdk_core::messages::record_failure",
                    "Failed to fetch existing record for context preservation"
                );
                None
            }
        };

        // Preserve message_event_id from existing record
        // Use provided epoch/mls_group_id, falling back to existing record
        let message_event_id = existing_record.as_ref().and_then(|r| r.message_event_id);
        let epoch = epoch.or_else(|| existing_record.as_ref().and_then(|r| r.epoch));
        let mls_group_id = mls_group_id
            .cloned()
            .or_else(|| existing_record.and_then(|r| r.mls_group_id));

        let processed_message = super::create_processed_message_record(
            event_id,
            message_event_id,
            epoch,
            mls_group_id,
            message_types::ProcessedMessageState::Failed,
            Some(sanitized_reason.to_string()),
        );

        self.save_processed_message_record(processed_message)?;

        Ok(())
    }

    /// Records a failure and returns an Unprocessable result
    ///
    /// Convenience method combining failure recording with returning Unprocessable.
    pub(super) fn fail_unprocessable(
        &self,
        event_id: EventId,
        error: &Error,
        group: &group_types::Group,
    ) -> Result<MessageProcessingResult> {
        self.record_failure(
            event_id,
            error,
            Some(&group.mls_group_id),
            Some(group.epoch),
        )?;

        Ok(MessageProcessingResult::Unprocessable {
            mls_group_id: group.mls_group_id.clone(),
        })
    }

    /// Returns a Commit result for our own already-processed commit
    ///
    /// Syncs group metadata and returns a Commit result. Used when we encounter
    /// our own commit that we've already processed.
    pub(super) fn return_own_commit(
        &self,
        group: &group_types::Group,
    ) -> Result<MessageProcessingResult> {
        if let Err(_e) = self.sync_group_metadata_from_mls(&group.mls_group_id) {
            tracing::warn!(
                target: "mdk_core::messages::return_own_commit",
                "Failed to sync group metadata"
            );
            return Err(Error::Message("Failed to sync group metadata".to_string()));
        }

        Ok(MessageProcessingResult::Commit {
            mls_group_id: group.mls_group_id.clone(),
        })
    }

    /// Handles processing errors with specific error recovery logic
    ///
    /// This method handles complex error scenarios when message processing fails,
    /// including special cases like processing own messages, epoch mismatches, and
    /// other MLS-specific validation errors.
    ///
    /// # Arguments
    ///
    /// * `error` - The error that occurred during processing
    /// * `event` - The wrapper Nostr event that caused the error
    /// * `group` - The group metadata from storage
    ///
    /// # Returns
    ///
    /// * `Ok(MessageProcessingResult)` - Recovery result or unprocessable status
    /// * `Err(Error)` - If error handling itself fails
    pub(super) fn handle_processing_error(
        &self,
        error: Error,
        event: &Event,
        group: &group_types::Group,
    ) -> Result<MessageProcessingResult> {
        match error {
            Error::CannotDecryptOwnMessage => {
                tracing::debug!(target: "mdk_core::messages::process_message", "Cannot decrypt own message, checking for cached message");

                let mut processed_message = self
                    .storage()
                    .find_processed_message_by_event_id(&event.id)
                    .map_err(|_e| {
                        Error::Message("Storage error while finding processed message".to_string())
                    })?
                    .ok_or(Error::Message("Processed message not found".to_string()))?;

                // If the message is created, we need to update the state of the message and processed message
                // If it's already processed, we don't need to do anything
                match processed_message.state {
                    message_types::ProcessedMessageState::Created => {
                        let message_event_id: EventId = processed_message
                            .message_event_id
                            .ok_or(Error::Message("Message event ID not found".to_string()))?;

                        let mut message = self
                            .get_message(&group.mls_group_id, &message_event_id)?
                            .ok_or(Error::Message("Message not found".to_string()))?;

                        message.state = message_types::MessageState::Processed;
                        self.storage().save_message(message).map_err(|_e| {
                            Error::Message("Storage error while saving message".to_string())
                        })?;

                        processed_message.state = message_types::ProcessedMessageState::Processed;
                        self.storage()
                            .save_processed_message(processed_message.clone())
                            .map_err(|_e| {
                                Error::Message(
                                    "Storage error while saving processed message".to_string(),
                                )
                            })?;

                        tracing::debug!(target: "mdk_core::messages::process_message", "Updated state of own cached message");
                        let message = self
                            .get_message(&group.mls_group_id, &message_event_id)?
                            .ok_or(Error::MessageNotFound)?;
                        Ok(MessageProcessingResult::ApplicationMessage(message))
                    }
                    message_types::ProcessedMessageState::Retryable => {
                        // Retryable messages are ones that previously failed due to wrong epoch keys
                        // but have been marked for retry after a rollback. For our own messages,
                        // we should have cached content - try to retrieve and return it.
                        tracing::debug!(target: "mdk_core::messages::process_message", "Retrying own message after rollback");

                        if let Some(message_event_id) = processed_message.message_event_id {
                            match self
                                .get_message(&group.mls_group_id, &message_event_id)
                                .map_err(|_e| {
                                    Error::Message(
                                        "Storage error while getting message".to_string(),
                                    )
                                })? {
                                Some(mut message) => {
                                    // Update states to mark as successfully processed
                                    message.state = message_types::MessageState::Processed;
                                    self.storage().save_message(message).map_err(|_e| {
                                        Error::Message(
                                            "Storage error while saving message".to_string(),
                                        )
                                    })?;

                                    processed_message.state =
                                        message_types::ProcessedMessageState::Processed;
                                    processed_message.failure_reason = None;
                                    processed_message.processed_at = Timestamp::now();
                                    self.storage()
                                        .save_processed_message(processed_message.clone())
                                        .map_err(|_e| {
                                            Error::Message(
                                                "Storage error while saving processed message"
                                                    .to_string(),
                                            )
                                        })?;

                                    tracing::info!(
                                        target: "mdk_core::messages::process_message",
                                        "Successfully retried own cached message after rollback"
                                    );
                                    let message = self
                                        .get_message(&group.mls_group_id, &message_event_id)
                                        .map_err(|_e| {
                                            Error::Message(
                                                "Storage error while getting message".to_string(),
                                            )
                                        })?
                                        .ok_or(Error::MessageNotFound)?;
                                    return Ok(MessageProcessingResult::ApplicationMessage(
                                        message,
                                    ));
                                }
                                None => {
                                    // No cached content available - fall through to Unprocessable
                                }
                            }
                        }

                        // No cached content available - this shouldn't happen for our own messages,
                        // but if it does, we can't recover
                        tracing::warn!(
                            target: "mdk_core::messages::process_message",
                            "Retryable own message has no cached content - cannot recover"
                        );
                        Ok(MessageProcessingResult::Unprocessable {
                            mls_group_id: group.mls_group_id.clone(),
                        })
                    }
                    message_types::ProcessedMessageState::ProcessedCommit => {
                        tracing::debug!(target: "mdk_core::messages::process_message", "Message already processed as a commit");
                        self.return_own_commit(group)
                    }
                    message_types::ProcessedMessageState::Processed
                    | message_types::ProcessedMessageState::Failed
                    | message_types::ProcessedMessageState::EpochInvalidated => {
                        tracing::debug!(target: "mdk_core::messages::process_message", "Message cannot be processed (already processed, failed, or epoch invalidated)");
                        Ok(MessageProcessingResult::Unprocessable {
                            mls_group_id: group.mls_group_id.clone(),
                        })
                    }
                }
            }
            Error::ProcessMessageWrongEpoch(msg_epoch) => {
                // Check if this commit is "better" than what we have for this epoch
                let is_better = self.epoch_snapshots.is_better_candidate(
                    self.storage(),
                    &group.mls_group_id,
                    msg_epoch,
                    event.created_at.as_secs(),
                    &event.id,
                );

                if is_better {
                    tracing::info!("Found better commit for epoch {}. Rolling back.", msg_epoch);

                    match self.epoch_snapshots.rollback_to_epoch(
                        self.storage(),
                        &group.mls_group_id,
                        msg_epoch,
                    ) {
                        Ok(_) => {
                            tracing::info!("Rollback successful. Re-processing better commit.");

                            // Invalidate messages from epochs after the rollback target
                            // These are messages processed with the wrong commit's keys - they
                            // can never be decrypted again and should be marked as invalidated
                            let invalidated_messages = self
                                .storage()
                                .invalidate_messages_after_epoch(&group.mls_group_id, msg_epoch)
                                .unwrap_or_default();

                            // Also invalidate processed_messages from wrong epochs
                            let _ = self.storage().invalidate_processed_messages_after_epoch(
                                &group.mls_group_id,
                                msg_epoch,
                            );

                            // Find messages that failed to decrypt because we had the wrong
                            // commit's keys. Now that we've rolled back and will apply the
                            // correct commit, these can potentially be decrypted.
                            let messages_needing_refetch = self
                                .storage()
                                .find_failed_messages_for_retry(&group.mls_group_id)
                                .unwrap_or_default();

                            // Mark these messages as Retryable so they can pass through
                            // deduplication when the application re-fetches and reprocesses them
                            for event_id in &messages_needing_refetch {
                                if self
                                    .storage()
                                    .mark_processed_message_retryable(event_id)
                                    .is_err()
                                {
                                    tracing::warn!(
                                        target: "mdk_core::messages::process_message",
                                        "Failed to mark message {} as retryable",
                                        event_id
                                    );
                                }
                            }

                            if let Some(cb) = &self.callback {
                                cb.on_rollback(&crate::RollbackInfo {
                                    group_id: group.mls_group_id.clone(),
                                    target_epoch: msg_epoch,
                                    new_head_event: event.id,
                                    invalidated_messages,
                                    messages_needing_refetch,
                                });
                            }

                            // Recursively call process_message now that state is rolled back.
                            // This will reload the group and apply the new commit.
                            return self.process_message(event);
                        }
                        Err(_) => {
                            tracing::error!("Rollback failed");
                            // Fall through to standard error handling
                        }
                    }
                }

                // Epoch mismatch - check if this is our own commit that we've already processed
                if let Ok(Some(processed_message)) =
                    self.storage().find_processed_message_by_event_id(&event.id)
                    && processed_message.state
                        == message_types::ProcessedMessageState::ProcessedCommit
                {
                    tracing::debug!(target: "mdk_core::messages::process_message", "Found own commit with epoch mismatch, syncing group metadata");
                    return self.return_own_commit(group);
                }

                // Not our own commit - this is a genuine error
                tracing::error!(target: "mdk_core::messages::process_message", "Epoch mismatch for message that is not our own commit");
                self.fail_unprocessable(event.id, &error, group)
            }
            Error::ProcessMessageWrongGroupId => {
                tracing::error!(target: "mdk_core::messages::process_message", "Group ID mismatch");
                self.fail_unprocessable(event.id, &error, group)
            }
            Error::ProcessMessageUseAfterEviction => {
                tracing::error!(target: "mdk_core::messages::process_message", "Attempted to use group after eviction");
                self.fail_unprocessable(event.id, &error, group)
            }
            Error::CommitFromNonAdmin => {
                // Authorization errors should propagate as errors, not be silently swallowed
                // Save a failed processing record to prevent reprocessing (best-effort)
                if let Err(_save_err) = self.record_failure(
                    event.id,
                    &error,
                    Some(&group.mls_group_id),
                    Some(group.epoch),
                ) {
                    tracing::warn!(
                        target: "mdk_core::messages::handle_processing_error",
                        "Failed to persist failure record for commit from non-admin"
                    );
                }
                Err(error)
            }
            _ => {
                tracing::error!(target: "mdk_core::messages::process_message", "Unexpected error processing message");
                self.fail_unprocessable(event.id, &error, group)
            }
        }
    }

    /// Extracts the MLS group ID from an event's h-tag
    ///
    /// This helper extracts the Nostr group ID from the event's h-tag and looks up
    /// the corresponding MLS group ID in storage.
    ///
    /// # Arguments
    ///
    /// * `event` - The event to extract the group ID from
    ///
    /// # Returns
    ///
    /// `Some(GroupId)` if the group is found in storage,
    /// `None` if the h-tag is missing/malformed or the group isn't in storage.
    pub(super) fn extract_mls_group_id_from_event(&self, event: &Event) -> Option<GroupId> {
        let nostr_group_id = self.extract_nostr_group_id(event).ok()?;

        self.storage()
            .find_group_by_nostr_group_id(&nostr_group_id)
            .ok()
            .flatten()
            .map(|group| group.mls_group_id)
    }
}

#[cfg(test)]
mod tests {
    use mdk_storage_traits::GroupId;
    use mdk_storage_traits::messages::MessageStorage;
    use mdk_storage_traits::messages::types as message_types;
    use nostr::{EventBuilder, EventId, Keys, Kind, Tag, TagKind, Timestamp};

    use crate::error::Error;
    use crate::test_util::*;
    use crate::tests::create_test_mdk;

    use super::super::MessageProcessingResult;

    /// Test that validation failures persist failed processing state
    ///
    /// This test verifies that when message validation fails (e.g., wrong event kind),
    /// a failed processing record is saved to prevent expensive reprocessing.
    #[test]
    fn test_validation_failure_persists_failed_state() {
        let mdk = create_test_mdk();
        let keys = Keys::generate();

        // Create an event with wrong kind (should be 445, but we use 1)
        let event = EventBuilder::new(Kind::Metadata, "")
            .sign_with_keys(&keys)
            .unwrap();

        // First attempt should fail validation
        let result = mdk.process_message(&event);
        assert!(result.is_err(), "Expected validation error");

        // Check that a failed processing record was saved
        let processed = mdk
            .storage()
            .find_processed_message_by_event_id(&event.id)
            .unwrap();
        assert!(processed.is_some(), "Failed record should be saved");
        let processed = processed.unwrap();
        assert_eq!(
            processed.state,
            message_types::ProcessedMessageState::Failed,
            "State should be Failed"
        );
        assert!(
            processed.failure_reason.is_some(),
            "Failure reason should be set"
        );
        // Check for sanitized failure reason (not internal error details)
        assert_eq!(
            processed.failure_reason.unwrap(),
            "invalid_event_type",
            "Failure reason should be sanitized classification"
        );
    }

    /// Test that repeated validation failures are rejected immediately
    ///
    /// This test verifies the deduplication mechanism prevents reprocessing
    /// of previously failed events, mitigating DoS attacks.
    ///
    /// When a previously failed message cannot provide a valid group_id (missing or
    /// malformed h-tag), we return PreviouslyFailed to avoid crashing client apps.
    #[test]
    fn test_repeated_validation_failure_rejected_immediately() {
        let mdk = create_test_mdk();
        let keys = Keys::generate();

        // Create an event with wrong kind (no group_id tag)
        let event = EventBuilder::new(Kind::Metadata, "")
            .sign_with_keys(&keys)
            .unwrap();

        // First attempt - full validation
        let result1 = mdk.process_message(&event);
        assert!(result1.is_err(), "First attempt should fail validation");

        // Second attempt - should be rejected immediately via deduplication
        // Returns PreviouslyFailed because group_id cannot be extracted from malformed event
        let result2 = mdk.process_message(&event);
        assert!(
            result2.is_ok(),
            "Second attempt should return Ok(PreviouslyFailed), not error"
        );
        assert!(
            matches!(result2.unwrap(), MessageProcessingResult::PreviouslyFailed),
            "Should return PreviouslyFailed variant"
        );
    }

    /// Test that decryption failures persist failed processing state
    ///
    /// This test verifies that when message decryption fails (e.g., group not found),
    /// a failed processing record is saved to prevent expensive reprocessing.
    #[test]
    fn test_decryption_failure_persists_failed_state() {
        let mdk = create_test_mdk();
        let keys = Keys::generate();

        // Create a valid-looking event but for a non-existent group
        let fake_group_id = hex::encode([42u8; 32]);
        let tag = Tag::custom(TagKind::h(), [fake_group_id]);
        let event = EventBuilder::new(Kind::MlsGroupMessage, "encrypted_content")
            .tag(tag)
            .sign_with_keys(&keys)
            .unwrap();

        // First attempt should fail decryption (group not found)
        let result = mdk.process_message(&event);
        assert!(result.is_err(), "Expected decryption error");

        // Check that a failed processing record was saved
        let processed = mdk
            .storage()
            .find_processed_message_by_event_id(&event.id)
            .unwrap();
        assert!(processed.is_some(), "Failed record should be saved");
        let processed = processed.unwrap();
        assert_eq!(
            processed.state,
            message_types::ProcessedMessageState::Failed,
            "State should be Failed"
        );
        assert!(
            processed.failure_reason.is_some(),
            "Failure reason should be set"
        );
        // Check for sanitized failure reason (not internal error details)
        assert_eq!(
            processed.failure_reason.unwrap(),
            "group_not_found",
            "Failure reason should be sanitized classification"
        );
    }

    /// Test that repeated decryption failures are rejected immediately
    ///
    /// This test verifies the deduplication mechanism works for decryption failures,
    /// preventing expensive repeated decryption attempts. When the group doesn't exist
    /// in storage, we return PreviouslyFailed to avoid crashing client apps.
    #[test]
    fn test_repeated_decryption_failure_rejected_immediately() {
        let mdk = create_test_mdk();
        let keys = Keys::generate();

        // Create a valid-looking event but for a non-existent group
        let fake_group_id = hex::encode([42u8; 32]);
        let tag = Tag::custom(TagKind::h(), [fake_group_id]);
        let event = EventBuilder::new(Kind::MlsGroupMessage, "encrypted_content")
            .tag(tag)
            .sign_with_keys(&keys)
            .unwrap();

        // First attempt - full decryption attempt
        let result1 = mdk.process_message(&event);
        assert!(result1.is_err(), "First attempt should fail decryption");

        // Second attempt - should return PreviouslyFailed because group isn't in storage
        // (we can't determine the MLS group ID from just the Nostr group ID)
        let result2 = mdk.process_message(&event);
        assert!(
            result2.is_ok(),
            "Second attempt should return Ok(PreviouslyFailed), not error"
        );
        assert!(
            matches!(result2.unwrap(), MessageProcessingResult::PreviouslyFailed),
            "Should return PreviouslyFailed variant"
        );
    }

    /// Test that previously failed message without group in storage returns PreviouslyFailed
    ///
    /// This test verifies that when a previously failed message has a valid h-tag
    /// but the group doesn't exist in storage, we return PreviouslyFailed since we can't
    /// determine the MLS group ID (Nostr group ID != MLS group ID).
    #[test]
    fn test_previously_failed_message_without_group_in_storage() {
        let mdk = create_test_mdk();
        let keys = Keys::generate();

        // Create a valid nostr_group_id but don't create the group in storage
        let nostr_group_id_bytes = [42u8; 32];
        let nostr_group_id_hex = hex::encode(nostr_group_id_bytes);

        // Create an event with valid h-tag but group doesn't exist
        let tag = Tag::custom(TagKind::h(), [nostr_group_id_hex.clone()]);
        let event = EventBuilder::new(Kind::MlsGroupMessage, "invalid_encrypted_content")
            .tag(tag)
            .sign_with_keys(&keys)
            .unwrap();

        // First attempt - will fail (group doesn't exist)
        let result1 = mdk.process_message(&event);
        assert!(result1.is_err(), "First attempt should fail");

        // Verify failed state was persisted
        let processed = mdk
            .storage()
            .find_processed_message_by_event_id(&event.id)
            .unwrap()
            .expect("Failed record should exist");
        assert_eq!(
            processed.state,
            message_types::ProcessedMessageState::Failed,
            "State should be Failed"
        );

        // Second attempt - should return PreviouslyFailed because we can't determine MLS group ID
        let result2 = mdk.process_message(&event);
        assert!(
            result2.is_ok(),
            "Second attempt should return Ok(PreviouslyFailed), not error"
        );
        assert!(
            matches!(result2.unwrap(), MessageProcessingResult::PreviouslyFailed),
            "Should return PreviouslyFailed variant"
        );
    }

    /// Test that previously failed message with oversized hex in h-tag returns PreviouslyFailed
    ///
    /// This test verifies that when a previously failed message has an oversized hex string
    /// in the h-tag (potential DoS vector), the size check prevents decoding and returns
    /// PreviouslyFailed to avoid crashing client apps.
    #[test]
    fn test_previously_failed_message_with_oversized_hex() {
        let mdk = create_test_mdk();
        let keys = Keys::generate();

        // Create an oversized hex string (128 chars instead of 64)
        let oversized_hex = "a".repeat(128);
        let tag = Tag::custom(TagKind::h(), [oversized_hex]);
        let event = EventBuilder::new(Kind::MlsGroupMessage, "invalid_content")
            .tag(tag)
            .sign_with_keys(&keys)
            .unwrap();

        // First attempt - will fail
        let result1 = mdk.process_message(&event);
        assert!(result1.is_err(), "First attempt should fail");

        // Verify failed state was persisted
        let processed = mdk
            .storage()
            .find_processed_message_by_event_id(&event.id)
            .unwrap()
            .expect("Failed record should exist");
        assert_eq!(
            processed.state,
            message_types::ProcessedMessageState::Failed
        );

        // Second attempt - should return PreviouslyFailed due to malformed h-tag
        let result2 = mdk.process_message(&event);
        assert!(
            result2.is_ok(),
            "Second attempt should return Ok(PreviouslyFailed), not error"
        );
        assert!(
            matches!(result2.unwrap(), MessageProcessingResult::PreviouslyFailed),
            "Should return PreviouslyFailed variant"
        );
    }

    /// Test that previously failed message with undersized hex in h-tag returns PreviouslyFailed
    ///
    /// This test verifies that when a previously failed message has an undersized hex string
    /// in the h-tag, the size check prevents decoding and returns PreviouslyFailed to avoid
    /// crashing client apps.
    #[test]
    fn test_previously_failed_message_with_undersized_hex() {
        let mdk = create_test_mdk();
        let keys = Keys::generate();

        // Create an undersized hex string (32 chars instead of 64)
        let undersized_hex = "a".repeat(32);
        let tag = Tag::custom(TagKind::h(), [undersized_hex]);
        let event = EventBuilder::new(Kind::MlsGroupMessage, "invalid_content")
            .tag(tag)
            .sign_with_keys(&keys)
            .unwrap();

        // First attempt - will fail
        let result1 = mdk.process_message(&event);
        assert!(result1.is_err(), "First attempt should fail");

        // Verify failed state was persisted
        let processed = mdk
            .storage()
            .find_processed_message_by_event_id(&event.id)
            .unwrap()
            .expect("Failed record should exist");
        assert_eq!(
            processed.state,
            message_types::ProcessedMessageState::Failed
        );

        // Second attempt - should return PreviouslyFailed due to malformed h-tag
        let result2 = mdk.process_message(&event);
        assert!(
            result2.is_ok(),
            "Second attempt should return Ok(PreviouslyFailed), not error"
        );
        assert!(
            matches!(result2.unwrap(), MessageProcessingResult::PreviouslyFailed),
            "Should return PreviouslyFailed variant"
        );
    }

    /// Test that previously failed message with group in storage returns correct MLS group ID
    ///
    /// This test verifies that when a group exists in storage, the code looks up and returns
    /// the actual MLS group ID (not just the Nostr group ID).
    #[test]
    fn test_previously_failed_message_with_group_in_storage() {
        let mdk = create_test_mdk();
        let (creator, members, admins) = create_test_group_members();

        // Create a real group in storage
        let group_id = create_test_group(&mdk, &creator, &members, &admins);

        // Get the group to extract its nostr_group_id
        let group = mdk
            .get_group(&group_id)
            .expect("Failed to get group")
            .expect("Group should exist");
        let nostr_group_id_hex = hex::encode(group.nostr_group_id);

        // Create an event with the group's nostr_group_id but invalid content
        let keys = Keys::generate();
        let tag = Tag::custom(TagKind::h(), [nostr_group_id_hex]);
        let event = EventBuilder::new(Kind::MlsGroupMessage, "invalid_encrypted_content")
            .tag(tag)
            .sign_with_keys(&keys)
            .unwrap();

        // First attempt - will fail (invalid content)
        let result1 = mdk.process_message(&event);
        assert!(result1.is_err(), "First attempt should fail");

        // Verify failed state was persisted
        let processed = mdk
            .storage()
            .find_processed_message_by_event_id(&event.id)
            .unwrap()
            .expect("Failed record should exist");
        assert_eq!(
            processed.state,
            message_types::ProcessedMessageState::Failed
        );

        // Second attempt - should return Unprocessable with the MLS group ID from storage
        let result2 = mdk.process_message(&event);
        assert!(
            result2.is_ok(),
            "Second attempt should return Ok(Unprocessable)"
        );

        match result2.unwrap() {
            MessageProcessingResult::Unprocessable { mls_group_id } => {
                // Verify it returned the actual MLS group ID from storage
                assert_eq!(
                    mls_group_id, group_id,
                    "Should return MLS group ID from storage, not Nostr group ID"
                );
            }
            _ => panic!("Expected Unprocessable variant"),
        }
    }

    /// Test that previously failed message with invalid hex characters returns PreviouslyFailed
    ///
    /// This test verifies that when hex::decode fails due to invalid characters,
    /// the code returns PreviouslyFailed to avoid crashing client apps.
    #[test]
    fn test_previously_failed_message_with_invalid_hex_chars() {
        let mdk = create_test_mdk();
        let keys = Keys::generate();

        // Create invalid hex string (64 chars but contains non-hex characters like 'z')
        let invalid_hex = "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz";
        assert_eq!(
            invalid_hex.len(),
            64,
            "Should be 64 chars to pass length check"
        );

        let tag = Tag::custom(TagKind::h(), [invalid_hex]);
        let event = EventBuilder::new(Kind::MlsGroupMessage, "invalid_content")
            .tag(tag)
            .sign_with_keys(&keys)
            .unwrap();

        // First attempt - will fail
        let result1 = mdk.process_message(&event);
        assert!(result1.is_err(), "First attempt should fail");

        // Verify failed state was persisted
        let processed = mdk
            .storage()
            .find_processed_message_by_event_id(&event.id)
            .unwrap()
            .expect("Failed record should exist");
        assert_eq!(
            processed.state,
            message_types::ProcessedMessageState::Failed
        );

        // Second attempt - should return PreviouslyFailed due to invalid hex
        let result2 = mdk.process_message(&event);
        assert!(
            result2.is_ok(),
            "Second attempt should return Ok(PreviouslyFailed), not error"
        );
        assert!(
            matches!(result2.unwrap(), MessageProcessingResult::PreviouslyFailed),
            "Should return PreviouslyFailed variant"
        );
    }

    /// Test that missing group ID tag persists failed state
    ///
    /// This test verifies that validation failures for missing required tags
    /// are properly persisted to prevent reprocessing.
    #[test]
    fn test_missing_group_id_tag_persists_failed_state() {
        let mdk = create_test_mdk();
        let keys = Keys::generate();

        // Create an event with correct kind but missing group ID tag
        let event = EventBuilder::new(Kind::MlsGroupMessage, "encrypted_content")
            .sign_with_keys(&keys)
            .unwrap();

        // First attempt should fail validation
        let result = mdk.process_message(&event);
        assert!(result.is_err(), "Expected validation error");

        // Check that a failed processing record was saved
        let processed = mdk
            .storage()
            .find_processed_message_by_event_id(&event.id)
            .unwrap();
        assert!(processed.is_some(), "Failed record should be saved");
        let processed = processed.unwrap();
        assert_eq!(
            processed.state,
            message_types::ProcessedMessageState::Failed,
            "State should be Failed"
        );
    }

    /// Test that deduplication only blocks Failed state
    ///
    /// This test verifies that the deduplication check only prevents reprocessing
    /// of Failed messages, allowing normal message flow for other states.
    #[test]
    fn test_deduplication_only_blocks_failed_state() {
        let mdk = create_test_mdk();
        let keys = Keys::generate();

        // Create a test event
        let event = EventBuilder::new(Kind::Metadata, "")
            .sign_with_keys(&keys)
            .unwrap();

        // Manually save a Processed state (simulating a successfully processed message)
        let processed_message = message_types::ProcessedMessage {
            wrapper_event_id: event.id,
            message_event_id: None,
            processed_at: Timestamp::now(),
            epoch: None,
            mls_group_id: None,
            state: message_types::ProcessedMessageState::Processed,
            failure_reason: None,
        };
        mdk.storage()
            .save_processed_message(processed_message)
            .unwrap();

        // Attempting to process again should not be blocked by deduplication
        // (it will fail for other reasons like wrong kind, but not due to deduplication)
        let result = mdk.process_message(&event);
        assert!(result.is_err());
        // The error should NOT be about "previously failed"
        assert!(
            !result
                .unwrap_err()
                .to_string()
                .contains("Message processing previously failed"),
            "Should not be blocked by deduplication for non-Failed state"
        );
    }

    #[test]
    fn test_previously_failed_message_returns_unprocessable_not_error() {
        // Setup: Create MDK and a test group
        let mdk = create_test_mdk();
        let (creator, members, admins) = create_test_group_members();
        let group_id = create_test_group(&mdk, &creator, &members, &admins);

        // Create a test message event
        let rumor = create_test_rumor(&creator, "Test message");
        let event = mdk
            .create_message(&group_id, rumor)
            .expect("Failed to create message");

        // Manually mark the message as failed in storage
        // This simulates a message that previously failed processing
        let processed_message = message_types::ProcessedMessage {
            wrapper_event_id: event.id,
            message_event_id: None,
            processed_at: Timestamp::now(),
            epoch: None,
            mls_group_id: None,
            state: message_types::ProcessedMessageState::Failed,
            failure_reason: Some("Simulated failure for test".to_string()),
        };

        mdk.storage()
            .save_processed_message(processed_message)
            .expect("Failed to save processed message");

        // Try to process the message again
        // Before the fix: This would return Err() and crash apps
        // After the fix: This should return Ok(Unprocessable)
        let result = mdk.process_message(&event);

        // Assert: Should return Ok with Unprocessable, not Err
        assert!(
            result.is_ok(),
            "Should not throw error for previously failed message, got error: {:?}",
            result.as_ref().err()
        );

        // Verify it returns Unprocessable variant
        match result.unwrap() {
            MessageProcessingResult::Unprocessable { mls_group_id } => {
                // Just verify we got a valid group_id (not empty)
                assert!(
                    !mls_group_id.as_slice().is_empty(),
                    "Should return a non-empty group ID"
                );
            }
            _ => panic!("Expected Unprocessable variant"),
        }
    }

    /// Test that sanitize_error_reason covers all explicitly mapped error variants
    /// and falls back to "processing_failed" for unmapped variants
    #[test]
    fn test_sanitize_error_reason_all_variants() {
        use crate::MDK;
        use mdk_memory_storage::MdkMemoryStorage;
        use nostr::Kind;

        // Test explicitly mapped error variants
        assert_eq!(
            MDK::<MdkMemoryStorage>::sanitize_error_reason(&Error::UnexpectedEvent {
                expected: Kind::MlsGroupMessage,
                received: Kind::TextNote,
            }),
            "invalid_event_type"
        );

        assert_eq!(
            MDK::<MdkMemoryStorage>::sanitize_error_reason(&Error::MissingGroupIdTag),
            "invalid_event_format"
        );

        assert_eq!(
            MDK::<MdkMemoryStorage>::sanitize_error_reason(&Error::InvalidGroupIdFormat(
                "bad format".to_string()
            )),
            "invalid_event_format"
        );

        assert_eq!(
            MDK::<MdkMemoryStorage>::sanitize_error_reason(&Error::MultipleGroupIdTags(3)),
            "invalid_event_format"
        );

        assert_eq!(
            MDK::<MdkMemoryStorage>::sanitize_error_reason(&Error::InvalidTimestamp(
                "future timestamp".to_string()
            )),
            "invalid_event_format"
        );

        assert_eq!(
            MDK::<MdkMemoryStorage>::sanitize_error_reason(&Error::GroupNotFound),
            "group_not_found"
        );

        assert_eq!(
            MDK::<MdkMemoryStorage>::sanitize_error_reason(&Error::CannotDecryptOwnMessage),
            "own_message"
        );

        assert_eq!(
            MDK::<MdkMemoryStorage>::sanitize_error_reason(&Error::AuthorMismatch),
            "authentication_failed"
        );

        assert_eq!(
            MDK::<MdkMemoryStorage>::sanitize_error_reason(&Error::CommitFromNonAdmin),
            "authorization_failed"
        );

        // Test catch-all for unmapped variants (should return "processing_failed")
        assert_eq!(
            MDK::<MdkMemoryStorage>::sanitize_error_reason(&Error::MessageNotFound),
            "processing_failed"
        );

        assert_eq!(
            MDK::<MdkMemoryStorage>::sanitize_error_reason(&Error::OwnLeafNotFound),
            "processing_failed"
        );

        assert_eq!(
            MDK::<MdkMemoryStorage>::sanitize_error_reason(&Error::ProcessMessageWrongEpoch(5)),
            "processing_failed"
        );
    }

    #[test]
    fn test_record_failure_preserves_message_event_id() {
        let mdk = create_test_mdk();
        let keys = Keys::generate();

        // Create a test event
        let event = EventBuilder::new(Kind::Metadata, "")
            .sign_with_keys(&keys)
            .unwrap();

        // Create a fake message event ID
        let message_event_id =
            EventId::from_hex("0000000000000000000000000000000000000000000000000000000000000001")
                .unwrap();

        // Manually save a Created state with message_event_id (simulating a message we created/sent)
        let processed_message = message_types::ProcessedMessage {
            wrapper_event_id: event.id,
            message_event_id: Some(message_event_id),
            processed_at: Timestamp::now(),
            epoch: Some(123),
            mls_group_id: Some(GroupId::from_slice(&[1, 2, 3, 4])),
            state: message_types::ProcessedMessageState::Created,
            failure_reason: None,
        };
        mdk.storage()
            .save_processed_message(processed_message)
            .unwrap();

        // Now simulate a failure (e.g. decryption failed for own message)
        let error = Error::CannotDecryptOwnMessage;
        mdk.record_failure(event.id, &error, None, None).unwrap();

        // Verify the message_event_id is preserved
        let updated_record = mdk
            .storage()
            .find_processed_message_by_event_id(&event.id)
            .unwrap()
            .expect("Record should exist");

        assert_eq!(
            updated_record.state,
            message_types::ProcessedMessageState::Failed
        );
        assert_eq!(
            updated_record.message_event_id,
            Some(message_event_id),
            "message_event_id should be preserved"
        );
        assert_eq!(updated_record.epoch, Some(123), "epoch should be preserved");
        assert_eq!(
            updated_record.mls_group_id,
            Some(GroupId::from_slice(&[1, 2, 3, 4])),
            "mls_group_id should be preserved"
        );
    }

    /// Test: Commit race recovery - rollback should enable retry of failed messages
    ///
    /// This test reproduces a realistic commit race scenario and verifies that
    /// the rollback mechanism correctly recovers failed messages:
    ///
    /// ```text
    /// Timeline:
    ///   1. Alice, Bob, and Charlie are in a group at epoch 1
    ///   2. Both Alice and Bob create commits simultaneously (race condition)
    ///   3. Charlie receives and applies Alice's commit first → epoch 2 (Alice's keys)
    ///   4. A message encrypted with Bob's epoch 2 keys arrives at Charlie
    ///   5. Charlie cannot decrypt it (has Alice's keys, not Bob's) → FAILS
    ///   6. Bob's commit arrives - it's "better" (earlier timestamp per MIP-03) → ROLLBACK
    ///   7. Charlie retries the failed message → SUCCESS
    /// ```
    #[test]
    fn test_commit_race_rollback_enables_message_retry() {
        // =========================================================================
        // SETUP: Create a 3-member group (Alice admin, Bob admin, Charlie member)
        //
        // We need 3 members because:
        // - Alice and Bob will race to create commits
        // - Charlie is the observer who processes messages in a specific order
        // =========================================================================
        let alice_keys = Keys::generate();
        let bob_keys = Keys::generate();
        let charlie_keys = Keys::generate();

        let alice_mdk = create_test_mdk();
        let bob_mdk = create_test_mdk();
        let charlie_mdk = create_test_mdk();

        // Create key packages for Bob and Charlie
        let bob_key_package = create_key_package_event(&bob_mdk, &bob_keys);
        let charlie_key_package = create_key_package_event(&charlie_mdk, &charlie_keys);

        // Alice creates group with Bob and Charlie as members
        // Both Alice and Bob are admins (so both can create commits)
        let admin_pubkeys = vec![alice_keys.public_key(), bob_keys.public_key()];
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
            .expect("Alice should merge create commit");

        // Bob joins via welcome
        let bob_welcome = bob_mdk
            .process_welcome(&EventId::all_zeros(), &create_result.welcome_rumors[0])
            .expect("Bob should process welcome");
        bob_mdk
            .accept_welcome(&bob_welcome)
            .expect("Bob should accept welcome");

        // Charlie joins via welcome
        let charlie_welcome = charlie_mdk
            .process_welcome(&EventId::all_zeros(), &create_result.welcome_rumors[1])
            .expect("Charlie should process welcome");
        charlie_mdk
            .accept_welcome(&charlie_welcome)
            .expect("Charlie should accept welcome");

        // Verify all are at epoch 1
        let initial_epoch = alice_mdk.get_group(&group_id).unwrap().unwrap().epoch;
        assert_eq!(initial_epoch, 1, "All should start at epoch 1");

        // =========================================================================
        // STEP 1: COMMIT RACE - Alice and Bob both create commits
        //
        // In the real world, this happens when both admins try to update the
        // group simultaneously (e.g., both adding a member, both doing self-update).
        // Each commit produces different epoch keys.
        //
        // Per MIP-03, the commit with the EARLIEST timestamp wins. So we create
        // Bob's commit first (he will be the "winner"), then Alice's commit second.
        // Charlie will apply Alice's commit first (simulating out-of-order delivery),
        // then when Bob's commit arrives, it should trigger a rollback.
        // =========================================================================

        // Bob creates a self-update commit FIRST (earlier timestamp = winner)
        let bob_commit_result = bob_mdk
            .self_update(&group_id)
            .expect("Bob should create self-update");
        let bob_commit_event = bob_commit_result.evolution_event.clone();

        // Wait to ensure distinct timestamps (MIP-03 uses created_at for ordering)
        std::thread::sleep(std::time::Duration::from_secs(2));

        // Alice creates a self-update commit SECOND (later timestamp = loser)
        // Both commits are based on epoch 1, but Alice's has a later timestamp
        let alice_commit_result = alice_mdk
            .self_update(&group_id)
            .expect("Alice should create self-update");
        let alice_commit_event = alice_commit_result.evolution_event.clone();

        // Bob merges his own commit and sends a message with his new epoch 2 keys
        bob_mdk
            .merge_pending_commit(&group_id)
            .expect("Bob should merge his commit");

        let bob_message_rumor = create_test_rumor(&bob_keys, "Message from Bob after his commit");
        let bob_message_event = bob_mdk
            .create_message(&group_id, bob_message_rumor)
            .expect("Bob should create message with his epoch 2 keys");

        // =========================================================================
        // STEP 2: Charlie applies ALICE's commit first (wrong order)
        //
        // Charlie receives Alice's commit first and applies it.
        // Now Charlie has epoch 2 with ALICE's keys.
        // =========================================================================
        charlie_mdk
            .process_message(&alice_commit_event)
            .expect("Charlie should process Alice's commit");

        let charlie_epoch = charlie_mdk.get_group(&group_id).unwrap().unwrap().epoch;
        assert_eq!(charlie_epoch, 2, "Charlie should be at epoch 2 (Alice's)");

        // =========================================================================
        // STEP 3: Bob's message arrives - Charlie CANNOT decrypt it
        //
        // Bob's message was encrypted with BOB's epoch 2 keys.
        // Charlie has ALICE's epoch 2 keys.
        // Decryption fails and the message is recorded as Failed.
        // =========================================================================
        let decrypt_result = charlie_mdk.process_message(&bob_message_event);
        assert!(
            decrypt_result.is_err(),
            "Charlie should fail to decrypt Bob's message (wrong epoch keys)"
        );

        // Verify the message is in Failed state
        let failed_record = charlie_mdk
            .storage()
            .find_processed_message_by_event_id(&bob_message_event.id)
            .expect("Storage should not error")
            .expect("Failed record should exist");

        assert_eq!(
            failed_record.state,
            message_types::ProcessedMessageState::Failed,
            "Bob's message should be in Failed state"
        );

        // =========================================================================
        // STEP 4: Bob's commit arrives - it's "better" so Charlie should ROLLBACK
        //
        // Bob's commit has an earlier timestamp (MIP-03 ordering), so when Charlie
        // processes it, the rollback mechanism should:
        // 1. Detect Bob's commit is "better" than Alice's
        // 2. Restore group state to epoch 1
        // 3. Find failed messages via find_failed_messages_for_retry (internal)
        // 4. Mark them as Retryable
        // 5. Apply Bob's commit
        // =========================================================================
        let rollback_result = charlie_mdk.process_message(&bob_commit_event);
        assert!(
            rollback_result.is_ok(),
            "Charlie should successfully process Bob's commit (triggering rollback): {:?}",
            rollback_result.err()
        );

        // =========================================================================
        // STEP 5: Retry the message - should now succeed
        //
        // After rollback, Charlie has Bob's epoch 2 keys. The rollback mechanism
        // should have marked the failed message as Retryable, allowing it to pass
        // through deduplication and be decrypted successfully.
        // =========================================================================

        // Check the message state before retry
        let pre_retry_record = charlie_mdk
            .storage()
            .find_processed_message_by_event_id(&bob_message_event.id)
            .expect("Storage should not error")
            .expect("Record should exist");

        assert_eq!(
            pre_retry_record.state,
            message_types::ProcessedMessageState::Retryable,
            "After rollback, Bob's message should be marked as Retryable so it can be reprocessed"
        );

        let retry_result = charlie_mdk.process_message(&bob_message_event);
        assert!(
            retry_result.is_ok(),
            "After rollback, Charlie should successfully decrypt Bob's message: {:?}",
            retry_result.err()
        );

        // Verify Charlie received Bob's message
        let charlie_messages = charlie_mdk
            .get_messages(&group_id, None)
            .expect("Should get messages");

        assert!(
            charlie_messages
                .iter()
                .any(|m| m.content.contains("Message from Bob")),
            "Charlie should have Bob's message after retry"
        );
    }
}
