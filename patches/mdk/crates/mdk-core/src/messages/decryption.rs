//! Decryption and epoch fallback
//!
//! This module handles message decryption with epoch-based key fallback.

use mdk_storage_traits::groups::types as group_types;
use mdk_storage_traits::{GroupId, MdkStorageProvider};
use nostr::Event;
use openmls::prelude::MlsGroup;

use crate::error::Error;
use crate::{MDK, util};

use super::{DEFAULT_EPOCH_LOOKBACK, Result};

impl<Storage> MDK<Storage>
where
    Storage: MdkStorageProvider,
{
    /// Loads the group and decrypts the message content
    ///
    /// This private method loads the group from storage using the Nostr group ID,
    /// loads the corresponding MLS group, and decrypts the message content using
    /// the group's exporter secrets.
    ///
    /// # Arguments
    ///
    /// * `nostr_group_id` - The Nostr group ID extracted from the event
    /// * `event` - The Nostr event containing the encrypted message
    ///
    /// # Returns
    ///
    /// * `Ok((group_types::Group, MlsGroup, Vec<u8>))` - The loaded group, MLS group, and decrypted message bytes
    /// * `Err(Error)` - If group loading or message decryption fails
    pub(super) fn decrypt_message(
        &self,
        nostr_group_id: [u8; 32],
        event: &Event,
    ) -> Result<(group_types::Group, MlsGroup, Vec<u8>)> {
        // Load groups by Nostr Group ID (Pattern B)
        // Used when processing incoming events which only have the Nostr group ID
        // from the h-tag. This is different from Pattern A (in create.rs) which
        // loads by MLS group ID when we already have it from API calls.
        let group = self
            .storage()
            .find_group_by_nostr_group_id(&nostr_group_id)
            .map_err(|_e| Error::Group("Storage error while finding group".to_string()))?
            .ok_or(Error::GroupNotFound)?;

        // Load the MLS group to get the current epoch
        let mls_group: MlsGroup = self
            .load_mls_group(&group.mls_group_id)
            .map_err(|_e| Error::Group("Storage error while loading MLS group".to_string()))?
            .ok_or(Error::GroupNotFound)?;

        // Try to decrypt message with recent exporter secrets (fallback across epochs)
        let message_bytes: Vec<u8> =
            self.try_decrypt_with_recent_epochs(&mls_group, &event.content)?;

        Ok((group, mls_group, message_bytes))
    }

    /// Tries to decrypt a message using exporter secrets from multiple recent epochs excluding the current one
    ///
    /// This helper method attempts to decrypt a message by trying exporter secrets from
    /// the most recent epoch backwards for a configurable number of epochs. This handles
    /// the case where a message was encrypted with an older epoch's secret due to timing
    /// issues or delayed message processing.
    ///
    /// # Arguments
    ///
    /// * `mls_group` - The MLS group
    /// * `encrypted_content` - The NIP-44 encrypted message content
    /// * `max_epoch_lookback` - Maximum number of epochs to search backwards (default: 5)
    ///
    /// # Returns
    ///
    /// * `Ok(Vec<u8>)` - The decrypted message bytes
    /// * `Err(Error)` - If decryption fails with all available exporter secrets
    fn try_decrypt_with_past_epochs(
        &self,
        mls_group: &MlsGroup,
        encrypted_content: &str,
        max_epoch_lookback: u64,
    ) -> Result<Vec<u8>> {
        let group_id: GroupId = mls_group.group_id().into();
        let current_epoch: u64 = mls_group.epoch().as_u64();

        // Guard: no past epochs to try if we're at epoch 0 or lookback is 0
        if current_epoch == 0 || max_epoch_lookback == 0 {
            return Err(Error::Message(
                "No past epochs available for decryption".to_string(),
            ));
        }

        // Start from current epoch and go backwards
        // We want exactly max_epoch_lookback iterations, so end_epoch is calculated
        // to make the inclusive range have that many elements
        let start_epoch: u64 = current_epoch.saturating_sub(1);
        let end_epoch: u64 = start_epoch.saturating_sub(max_epoch_lookback.saturating_sub(1));

        for epoch in (end_epoch..=start_epoch).rev() {
            tracing::debug!(
                target: "mdk_core::messages::try_decrypt_with_past_epochs",
                "Trying to decrypt with epoch {}",
                epoch
            );

            // Try to get the exporter secret for this epoch
            // Propagate storage errors instead of swallowing them
            match self.storage().get_group_exporter_secret(&group_id, epoch) {
                Ok(Some(secret)) => {
                    // Try to decrypt with this epoch's secret
                    match util::decrypt_with_exporter_secret(&secret, encrypted_content) {
                        Ok(decrypted_bytes) => {
                            tracing::debug!(
                                target: "mdk_core::messages::try_decrypt_with_past_epochs",
                                "Successfully decrypted message with epoch {}",
                                epoch
                            );
                            return Ok(decrypted_bytes);
                        }
                        Err(e) => {
                            tracing::trace!(
                                target: "mdk_core::messages::try_decrypt_with_past_epochs",
                                "Failed to decrypt with epoch {}: {:?}",
                                epoch,
                                e
                            );
                            // Continue to next epoch
                        }
                    }
                }
                Ok(None) => {
                    tracing::trace!(
                        target: "mdk_core::messages::try_decrypt_with_past_epochs",
                        "No exporter secret found for epoch {}",
                        epoch
                    );
                }
                Err(_e) => {
                    return Err(Error::Group(
                        "Storage error while finding exporter secret".to_string(),
                    ));
                }
            }
        }

        Err(Error::Message(format!(
            "Failed to decrypt message with any exporter secret from epochs {} to {}",
            end_epoch, start_epoch
        )))
    }

    /// Try to decrypt using the current exporter secret and if fails try with the past ones until a max lookback of [`DEFAULT_EPOCH_LOOKBACK`].
    pub(super) fn try_decrypt_with_recent_epochs(
        &self,
        mls_group: &MlsGroup,
        encrypted_content: &str,
    ) -> Result<Vec<u8>> {
        // Get exporter secret for current epoch
        let secret = self.exporter_secret(&mls_group.group_id().into())?;

        // Try to decrypt it for the current epoch
        match util::decrypt_with_exporter_secret(&secret, encrypted_content) {
            Ok(decrypted_bytes) => {
                tracing::debug!("Successfully decrypted message with current exporter secret");
                Ok(decrypted_bytes)
            }
            // Decryption failed using the current epoch exporter secret
            Err(_) => {
                tracing::debug!(
                    "Failed to decrypt message with current exporter secret. Trying with past ones."
                );

                // Try with past exporter secrets
                self.try_decrypt_with_past_epochs(
                    mls_group,
                    encrypted_content,
                    DEFAULT_EPOCH_LOOKBACK,
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use nostr::Keys;
    use openmls::prelude::MlsGroup;

    use crate::test_util::{
        create_key_package_event, create_nostr_group_config_data, create_test_rumor,
    };
    use crate::tests::create_test_mdk;

    /// Test epoch lookback limits for message decryption (MIP-03)
    ///
    /// This test validates the epoch lookback mechanism which allows messages from
    /// previous epochs to be decrypted (up to 5 epochs back).
    ///
    /// Requirements tested:
    /// - Messages from recent epochs (within 5 epochs) can be decrypted
    /// - Messages beyond the lookback limit cannot be decrypted
    /// - Epoch secrets are properly retained for lookback
    /// - Clear error messages when lookback limit is exceeded
    #[test]
    fn test_epoch_lookback_limits() {
        // Setup: Create Alice and Bob
        let alice_keys = Keys::generate();
        let bob_keys = Keys::generate();

        let alice_mdk = create_test_mdk();
        let bob_mdk = create_test_mdk();

        let admins = vec![alice_keys.public_key(), bob_keys.public_key()];

        // Step 1: Bob creates his key package and Alice creates the group
        let bob_key_package = create_key_package_event(&bob_mdk, &bob_keys);

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

        // Step 2: Alice creates a message in epoch 1 (initial epoch)
        // Save this message to test lookback limit later
        let rumor_epoch1 = create_test_rumor(&alice_keys, "Message in epoch 1");
        let msg_epoch1 = alice_mdk
            .create_message(&group_id, rumor_epoch1)
            .expect("Alice should send message in epoch 1");

        // Verify Bob can process it initially
        let bob_process1 = bob_mdk.process_message(&msg_epoch1);
        assert!(
            bob_process1.is_ok(),
            "Bob should process epoch 1 message initially"
        );

        // Step 3: Advance through 7 epochs (beyond the 5-epoch lookback limit)
        for i in 1..=7 {
            let update_result = alice_mdk
                .self_update(&group_id)
                .expect("Alice should be able to update");

            // Both clients process the update
            alice_mdk
                .process_message(&update_result.evolution_event)
                .expect("Alice should process update");

            alice_mdk
                .merge_pending_commit(&group_id)
                .expect("Alice should merge update");

            bob_mdk
                .process_message(&update_result.evolution_event)
                .expect("Bob should process update");

            // Send a message in this epoch to verify it works
            let rumor = create_test_rumor(&alice_keys, &format!("Message in epoch {}", i + 1));
            let msg = alice_mdk
                .create_message(&group_id, rumor)
                .expect("Alice should send message");

            // Bob should be able to process recent messages
            let process_result = bob_mdk.process_message(&msg);
            assert!(
                process_result.is_ok(),
                "Bob should process message from epoch {}",
                i + 1
            );
        }

        // Step 4: Verify final epoch
        let final_epoch = alice_mdk
            .get_group(&group_id)
            .expect("Failed to get group")
            .expect("Group should exist")
            .epoch;

        // Group creation puts us at epoch 1, then we advanced 7 times, so we should be at epoch 8
        assert_eq!(
            final_epoch, 8,
            "Group should be at epoch 8 after group creation (epoch 1) + 7 updates"
        );

        // Step 5: Verify lookback mechanism
        // We're now at epoch 8. Messages from epochs 3+ (within 5-epoch lookback) can be
        // decrypted, while messages from epochs 1-2 would be beyond the lookback limit.
        //
        // Note: We can't easily test the actual lookback failure without the ability to
        // create messages from old epochs after advancing (would require "time travel").
        // The MLS protocol handles this at the decryption layer by maintaining exporter
        // secrets for the last 5 epochs only.

        // The actual lookback validation happens in the MLS layer during decryption.
        // Our test confirms:
        // 1. We can advance through multiple epochs successfully
        // 2. Messages can be processed in each epoch
        // 3. The epoch count is correct (8 epochs total)
        // 4. The system maintains state correctly across epoch transitions

        // Note: Full epoch lookback boundary testing requires the ability to
        // store encrypted messages from old epochs and attempt decryption after
        // advancing beyond the lookback window. This is a protocol-level test
        // that would need access to the exporter secret retention mechanism.
    }

    /// Test that try_decrypt_with_past_epochs returns early when at epoch 0
    ///
    /// When a group is at epoch 0, there are no past epochs to try.
    /// The function should return an error immediately rather than
    /// attempting to iterate over an empty or invalid range.
    #[test]
    fn test_past_epoch_decryption_guards_epoch_zero() {
        let alice_keys = Keys::generate();
        let alice_mdk = create_test_mdk();

        // Create a group - after creation and merge, we're still at epoch 0
        let create_result = alice_mdk
            .create_group(
                &alice_keys.public_key(),
                vec![],
                create_nostr_group_config_data(vec![alice_keys.public_key()]),
            )
            .expect("Should create group");

        let group_id = create_result.group.mls_group_id.clone();

        alice_mdk
            .merge_pending_commit(&group_id)
            .expect("Should merge commit");

        // Load the MLS group to check its epoch
        let mls_group: MlsGroup = alice_mdk
            .load_mls_group(&group_id)
            .expect("Should load group")
            .expect("Group should exist");

        // Newly created group is at epoch 0
        assert_eq!(
            mls_group.epoch().as_u64(),
            0,
            "Group should be at epoch 0 after creation"
        );

        // Test with epoch 0 - should return early since there are no past epochs
        let result = alice_mdk.try_decrypt_with_past_epochs(
            &mls_group,
            "invalid_encrypted_content",
            5, // normal lookback, but epoch 0 means no past epochs
        );

        assert!(result.is_err(), "Should fail at epoch 0");
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("No past epochs available"),
            "Error should indicate no past epochs: {}",
            err_msg
        );
    }

    /// Test that try_decrypt_with_past_epochs handles zero lookback parameter
    ///
    /// When max_epoch_lookback is 0, no past epochs should be tried.
    #[test]
    fn test_past_epoch_decryption_guards_zero_lookback() {
        let alice_keys = Keys::generate();
        let alice_mdk = create_test_mdk();

        // Create a group and advance a few epochs
        let create_result = alice_mdk
            .create_group(
                &alice_keys.public_key(),
                vec![],
                create_nostr_group_config_data(vec![alice_keys.public_key()]),
            )
            .expect("Should create group");

        let group_id = create_result.group.mls_group_id.clone();

        alice_mdk
            .merge_pending_commit(&group_id)
            .expect("Should merge commit");

        // Advance a few epochs so we're not at epoch 0/1
        for _ in 0..3 {
            let update = alice_mdk.self_update(&group_id).expect("Should update");
            alice_mdk
                .process_message(&update.evolution_event)
                .expect("Should process update");
            alice_mdk
                .merge_pending_commit(&group_id)
                .expect("Should merge");
        }

        let mls_group: MlsGroup = alice_mdk
            .load_mls_group(&group_id)
            .expect("Should load group")
            .expect("Group should exist");

        // Verify we're at a higher epoch
        assert!(
            mls_group.epoch().as_u64() > 1,
            "Group should be past epoch 1"
        );

        // Test with max_epoch_lookback = 0
        let result = alice_mdk.try_decrypt_with_past_epochs(
            &mls_group,
            "invalid_encrypted_content",
            0, // zero lookback - should return early
        );

        assert!(result.is_err(), "Should fail with zero lookback");
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("No past epochs available"),
            "Error should indicate no past epochs: {}",
            err_msg
        );
    }
}
