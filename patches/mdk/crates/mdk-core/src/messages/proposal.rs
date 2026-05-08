//! Proposal message processing
//!
//! This module handles processing of MLS proposal messages.

use mdk_storage_traits::messages::types as message_types;
use mdk_storage_traits::{GroupId, MdkStorageProvider};
use nostr::Event;
use openmls::prelude::{MlsGroup, Proposal, QueuedProposal, Sender};
use openmls_traits::OpenMlsProvider;
use tls_codec::Serialize as TlsSerialize;

use crate::MDK;
use crate::error::Error;
use crate::groups::UpdateGroupResult;

use super::{MessageProcessingResult, Result};

impl<Storage> MDK<Storage>
where
    Storage: MdkStorageProvider,
{
    /// Processes a proposal message from a group member
    ///
    /// This internal function handles MLS proposal messages according to the Marmot protocol:
    ///
    /// - **Add/Remove member proposals**: Always stored as pending for admin approval via manual commit
    /// - **Self-remove (leave) proposals**: Auto-committed if receiver is admin, otherwise pending
    /// - **Extension/ciphersuite proposals**: Ignored with warning (admins should create commits directly)
    /// - **Update proposals**: Out of scope (see issue #59)
    ///
    /// # Arguments
    ///
    /// * `mls_group` - The MLS group to process the proposal for
    /// * `event` - The wrapper Nostr event containing the encrypted proposal
    /// * `staged_proposal` - The validated MLS proposal to process
    ///
    /// # Returns
    ///
    /// * `Ok(MessageProcessingResult::Proposal)` - Self-remove auto-committed by admin
    /// * `Ok(MessageProcessingResult::PendingProposal)` - Proposal stored for admin approval
    /// * `Ok(MessageProcessingResult::IgnoredProposal)` - Proposal ignored (extensions, etc.)
    /// * `Err(Error)` - If proposal processing fails or sender is not a member
    pub(super) fn process_proposal(
        &self,
        mls_group: &mut MlsGroup,
        event: &Event,
        staged_proposal: QueuedProposal,
    ) -> Result<MessageProcessingResult> {
        match staged_proposal.sender() {
            Sender::Member(sender_leaf_index) => {
                let member = mls_group.member_at(*sender_leaf_index);

                match member {
                    Some(_member) => {
                        let group_id: GroupId = mls_group.group_id().into();
                        let own_leaf = mls_group.own_leaf().ok_or(Error::OwnLeafNotFound)?;
                        let receiver_is_admin = self.is_leaf_node_admin(&group_id, own_leaf)?;

                        // Determine proposal type and how to handle it
                        match staged_proposal.proposal() {
                            Proposal::Add(_) => {
                                // Add proposals: always store as pending for admin approval
                                self.store_pending_proposal(
                                    mls_group,
                                    event,
                                    staged_proposal,
                                    &group_id,
                                )?;

                                tracing::debug!(
                                    target: "mdk_core::messages::process_proposal",
                                    "Stored Add proposal as pending for admin approval"
                                );

                                Ok(MessageProcessingResult::PendingProposal {
                                    mls_group_id: group_id,
                                })
                            }
                            Proposal::Remove(remove_proposal) => {
                                // Check if this is a self-remove (leave) proposal
                                let removed_leaf_index = remove_proposal.removed();
                                let is_self_remove = *sender_leaf_index == removed_leaf_index;

                                if is_self_remove && receiver_is_admin {
                                    // Self-remove proposal + admin receiver: auto-commit
                                    self.auto_commit_proposal(
                                        mls_group,
                                        event,
                                        staged_proposal,
                                        &group_id,
                                    )
                                } else {
                                    // Either not self-remove, or receiver is not admin
                                    // Store as pending for admin approval
                                    self.store_pending_proposal(
                                        mls_group,
                                        event,
                                        staged_proposal,
                                        &group_id,
                                    )?;

                                    if is_self_remove {
                                        tracing::debug!(
                                            target: "mdk_core::messages::process_proposal",
                                            "Non-admin receiver stored self-remove proposal as pending"
                                        );
                                    } else {
                                        tracing::debug!(
                                            target: "mdk_core::messages::process_proposal",
                                            "Stored Remove proposal as pending for admin approval"
                                        );
                                    }

                                    Ok(MessageProcessingResult::PendingProposal {
                                        mls_group_id: group_id,
                                    })
                                }
                            }
                            Proposal::Update(_) => {
                                // Update proposals (self key rotation) - out of scope for this issue
                                // See: https://github.com/marmot-protocol/mdk/issues/59
                                tracing::warn!(
                                    target: "mdk_core::messages::process_proposal",
                                    "Ignoring Update proposal - self-update handling not yet implemented (see issue #59)"
                                );

                                self.mark_processed(event, &group_id, mls_group.epoch().as_u64())?;

                                Ok(MessageProcessingResult::IgnoredProposal {
                                    mls_group_id: group_id,
                                    reason: "Update proposals not yet supported (see issue #59)"
                                        .to_string(),
                                })
                            }
                            Proposal::GroupContextExtensions(_) => {
                                // Extension proposals should be ignored - admins create commits directly
                                tracing::warn!(
                                    target: "mdk_core::messages::process_proposal",
                                    "Ignoring GroupContextExtensions proposal - admins should create commits directly"
                                );

                                self.mark_processed(event, &group_id, mls_group.epoch().as_u64())?;

                                Ok(MessageProcessingResult::IgnoredProposal {
                                    mls_group_id: group_id,
                                    reason: "Extension proposals not allowed - admins should create commits directly".to_string(),
                                })
                            }
                            _ => {
                                // Other proposal types (PreSharedKey, ReInit, ExternalInit, etc.)
                                tracing::warn!(
                                    target: "mdk_core::messages::process_proposal",
                                    "Ignoring unsupported proposal type"
                                );

                                self.mark_processed(event, &group_id, mls_group.epoch().as_u64())?;

                                Ok(MessageProcessingResult::IgnoredProposal {
                                    mls_group_id: group_id,
                                    reason: "Unsupported proposal type".to_string(),
                                })
                            }
                        }
                    }
                    None => {
                        tracing::warn!(target: "mdk_core::messages::process_mls_message", "Received proposal from non-member.");
                        Err(Error::MessageFromNonMember)
                    }
                }
            }
            Sender::External(_) => {
                // TODO: FUTURE Handle external proposals from external proposal extensions
                Err(Error::NotImplemented("Processing external proposals from external proposal extensions is not supported".to_string()))
            }
            Sender::NewMemberCommit => {
                // TODO: FUTURE Handle new member from external member commits.
                Err(Error::NotImplemented(
                    "Processing external proposals for new member commits is not supported"
                        .to_string(),
                ))
            }
            Sender::NewMemberProposal => {
                // TODO: FUTURE Handle new member from external member proposals.
                Err(Error::NotImplemented(
                    "Processing external proposals for new member proposals is not supported"
                        .to_string(),
                ))
            }
        }
    }

    /// Stores a proposal as pending and marks the event as processed
    ///
    /// This stores the proposal in the MLS group's pending proposal queue
    /// for later commit by an admin, and marks the wrapper event as processed
    /// to prevent reprocessing.
    pub(super) fn store_pending_proposal(
        &self,
        mls_group: &mut MlsGroup,
        event: &Event,
        staged_proposal: QueuedProposal,
        group_id: &GroupId,
    ) -> Result<()> {
        mls_group
            .store_pending_proposal(self.provider.storage(), staged_proposal)
            .map_err(|_e| Error::Message("Failed to store pending proposal".to_string()))?;

        self.mark_processed(event, group_id, mls_group.epoch().as_u64())
    }

    /// Marks an event as processed to prevent reprocessing
    ///
    /// # Arguments
    ///
    /// * `event` - The wrapper Nostr event to mark as processed
    /// * `mls_group_id` - The MLS group ID for context
    /// * `epoch` - The current epoch from the MLS group
    pub(super) fn mark_processed(
        &self,
        event: &Event,
        mls_group_id: &GroupId,
        epoch: u64,
    ) -> Result<()> {
        let processed_message = super::create_processed_message_record(
            event.id,
            None,
            Some(epoch),
            Some(mls_group_id.clone()),
            message_types::ProcessedMessageState::Processed,
            None,
        );

        self.save_processed_message_record(processed_message)
    }

    /// Stores a proposal and immediately auto-commits it (for self-remove by admin)
    pub(super) fn auto_commit_proposal(
        &self,
        mls_group: &mut MlsGroup,
        event: &Event,
        staged_proposal: QueuedProposal,
        group_id: &GroupId,
    ) -> Result<MessageProcessingResult> {
        mls_group
            .store_pending_proposal(self.provider.storage(), staged_proposal)
            .map_err(|_e| Error::Message("Failed to store pending proposal".to_string()))?;

        let mls_signer = self.load_mls_signer(mls_group)?;

        // Self-remove proposals never generate welcomes (only Add proposals do),
        // so we can safely ignore the welcome output here
        let (commit_message, _welcomes, _group_info) =
            mls_group.commit_to_pending_proposals(&self.provider, &mls_signer)?;

        let serialized_commit_message = commit_message
            .tls_serialize_detached()
            .map_err(|_e| Error::Group("Failed to serialize commit message".to_string()))?;

        let commit_event = self.build_message_event(group_id, serialized_commit_message)?;

        self.mark_processed(event, group_id, mls_group.epoch().as_u64())?;

        tracing::debug!(
            target: "mdk_core::messages::process_proposal",
            "Admin auto-committed self-remove proposal"
        );

        Ok(MessageProcessingResult::Proposal(UpdateGroupResult {
            evolution_event: commit_event,
            welcome_rumors: None,
            mls_group_id: group_id.clone(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use nostr::Keys;

    use crate::messages::MessageProcessingResult;
    use crate::test_util::{create_key_package_event, create_nostr_group_config_data};
    use crate::tests::create_test_mdk;

    /// Tests that self-leave proposals are auto-committed when processed by an admin.
    /// Per the Marmot protocol, admins should auto-commit self-leave proposals.
    #[test]
    fn test_self_leave_proposal_auto_committed_by_admin() {
        // Setup: Alice (admin), Bob (non-admin member)
        let alice_keys = Keys::generate();
        let bob_keys = Keys::generate();

        let alice_mdk = create_test_mdk();
        let bob_mdk = create_test_mdk();

        // Only Alice is admin
        let admins = vec![alice_keys.public_key()];

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

        // Bob joins the group
        let bob_welcome = &create_result.welcome_rumors[0];
        let bob_welcome_preview = bob_mdk
            .process_welcome(&nostr::EventId::all_zeros(), bob_welcome)
            .expect("Bob should process welcome");
        bob_mdk
            .accept_welcome(&bob_welcome_preview)
            .expect("Bob should accept welcome");

        // Bob leaves the group (creates a leave proposal)
        let bob_leave_result = bob_mdk
            .leave_group(&group_id)
            .expect("Bob should be able to leave");

        // Alice (admin) processes Bob's leave proposal
        // This should auto-commit and return Proposal variant
        let process_result = alice_mdk
            .process_message(&bob_leave_result.evolution_event)
            .expect("Alice should process Bob's leave");

        // Verify it returns Proposal (indicating auto-commit happened)
        assert!(
            matches!(process_result, MessageProcessingResult::Proposal(_)),
            "Admin processing self-leave should return Proposal (auto-committed), got: {:?}",
            process_result
        );

        // Extract the commit event from the result
        let _commit_event = match process_result {
            MessageProcessingResult::Proposal(update_result) => update_result.evolution_event,
            _ => panic!("Expected Proposal variant"),
        };

        // The pending proposal is cleared after merge_pending_commit is called
        // (which happens after the commit is published to relays)
        alice_mdk
            .merge_pending_commit(&group_id)
            .expect("Should merge pending commit");

        // Verify no pending proposals remain after merge
        let pending = alice_mdk
            .pending_removed_members_pubkeys(&group_id)
            .expect("Should get pending");
        assert!(pending.is_empty(), "No pending removals after merge");
    }

    /// Tests that self-leave proposals are stored as pending when processed by a non-admin.
    /// Non-admin members cannot commit, so they store the proposal for later admin approval.
    #[test]
    fn test_self_leave_proposal_stored_pending_by_non_admin() {
        // Setup: Alice (admin), Bob (non-admin), Charlie (non-admin)
        let alice_keys = Keys::generate();
        let bob_keys = Keys::generate();
        let charlie_keys = Keys::generate();

        let alice_mdk = create_test_mdk();
        let bob_mdk = create_test_mdk();
        let charlie_mdk = create_test_mdk();

        // Only Alice is admin
        let admins = vec![alice_keys.public_key()];

        let bob_key_package = create_key_package_event(&bob_mdk, &bob_keys);
        let charlie_key_package = create_key_package_event(&charlie_mdk, &charlie_keys);

        let create_result = alice_mdk
            .create_group(
                &alice_keys.public_key(),
                vec![bob_key_package, charlie_key_package],
                create_nostr_group_config_data(admins),
            )
            .expect("Alice should create group");

        let group_id = create_result.group.mls_group_id.clone();

        alice_mdk
            .merge_pending_commit(&group_id)
            .expect("Alice should merge commit");

        // Bob and Charlie join
        let bob_welcome = &create_result.welcome_rumors[0];
        let charlie_welcome = &create_result.welcome_rumors[1];

        let bob_welcome_preview = bob_mdk
            .process_welcome(&nostr::EventId::all_zeros(), bob_welcome)
            .expect("Bob should process welcome");
        bob_mdk
            .accept_welcome(&bob_welcome_preview)
            .expect("Bob should accept welcome");

        let charlie_welcome_preview = charlie_mdk
            .process_welcome(&nostr::EventId::all_zeros(), charlie_welcome)
            .expect("Charlie should process welcome");
        charlie_mdk
            .accept_welcome(&charlie_welcome_preview)
            .expect("Charlie should accept welcome");

        // Bob leaves (creates proposal)
        let bob_leave_result = bob_mdk.leave_group(&group_id).expect("Bob should leave");

        // Charlie (non-admin) processes the leave proposal
        // This should store as pending and return PendingProposal variant
        let process_result = charlie_mdk
            .process_message(&bob_leave_result.evolution_event)
            .expect("Charlie should process leave");

        // Verify it returns PendingProposal (indicating it was stored, not committed)
        assert!(
            matches!(
                process_result,
                MessageProcessingResult::PendingProposal { .. }
            ),
            "Non-admin processing self-leave should return PendingProposal, got: {:?}",
            process_result
        );

        // Verify the proposal is now pending
        let pending = charlie_mdk
            .pending_removed_members_pubkeys(&group_id)
            .expect("Should get pending");
        assert_eq!(pending.len(), 1, "Bob should be in pending removals");
        assert_eq!(
            pending[0],
            bob_keys.public_key(),
            "Pending removal should be Bob"
        );
    }

    /// Test that self-update commits from non-admin members are ALLOWED (Issue #44, #59)
    ///
    /// Per the Marmot protocol specification, any member can create a self-update
    /// commit to rotate their own key material. This is different from add/remove
    /// commits which require admin privileges.
    ///
    /// Scenario:
    /// 1. Alice (admin) creates a group with Charlie (non-admin member)
    /// 2. Charlie creates a self-update commit
    /// 3. Alice processes Charlie's commit successfully
    #[test]
    fn test_self_update_commit_from_non_admin_is_allowed() {
        // Setup: Alice (admin) and Charlie (non-admin member)
        let alice_keys = Keys::generate();
        let charlie_keys = Keys::generate();

        let alice_mdk = create_test_mdk();
        let charlie_mdk = create_test_mdk();

        // Only Alice is admin
        let admins = vec![alice_keys.public_key()];

        // Create key package for Charlie
        let charlie_key_package = create_key_package_event(&charlie_mdk, &charlie_keys);

        // Alice creates the group with Charlie as a non-admin member
        let create_result = alice_mdk
            .create_group(
                &alice_keys.public_key(),
                vec![charlie_key_package],
                create_nostr_group_config_data(admins.clone()),
            )
            .expect("Failed to create group");

        let group_id = create_result.group.mls_group_id.clone();

        // Alice merges her commit
        alice_mdk
            .merge_pending_commit(&group_id)
            .expect("Failed to merge pending commit");

        // Charlie joins the group via welcome message
        let charlie_welcome_rumor = &create_result.welcome_rumors[0];
        let charlie_welcome = charlie_mdk
            .process_welcome(&nostr::EventId::all_zeros(), charlie_welcome_rumor)
            .expect("Charlie should process welcome");
        charlie_mdk
            .accept_welcome(&charlie_welcome)
            .expect("Charlie should accept welcome");

        // Verify: Charlie is NOT an admin
        let group_state = charlie_mdk
            .get_group(&group_id)
            .expect("Failed to get group")
            .expect("Group should exist");
        assert!(
            !group_state
                .admin_pubkeys
                .contains(&charlie_keys.public_key()),
            "Charlie should NOT be an admin"
        );

        // Charlie creates a self-update commit (allowed for any member)
        let charlie_update_result = charlie_mdk
            .self_update(&group_id)
            .expect("Charlie can create self-update commit");

        // Get the commit event that Charlie would broadcast
        let charlie_commit_event = charlie_update_result.evolution_event;

        // Alice tries to process Charlie's self-update commit
        // This should SUCCEED because self-update commits are allowed from any member
        let result = alice_mdk.process_message(&charlie_commit_event);

        assert!(
            result.is_ok(),
            "Self-update commit from non-admin should succeed, got error: {:?}",
            result.err()
        );

        // Verify the result is a Commit
        assert!(
            matches!(result.unwrap(), MessageProcessingResult::Commit { .. }),
            "Result should be a Commit"
        );
    }

    /// Test that non-admin trying to update group extensions fails at client level
    ///
    /// This verifies the client-side check prevents non-admins from creating
    /// extension update commits. The server-side check in `is_pure_self_update_commit`
    /// provides defense-in-depth for malformed messages.
    #[test]
    fn test_non_admin_extension_update_rejected_at_client() {
        let alice_keys = Keys::generate();
        let bob_keys = Keys::generate();

        let alice_mdk = create_test_mdk();
        let bob_mdk = create_test_mdk();

        // Only Alice is admin
        let admins = vec![alice_keys.public_key()];

        let bob_key_package = create_key_package_event(&bob_mdk, &bob_keys);

        // Alice creates the group with Bob
        let create_result = alice_mdk
            .create_group(
                &alice_keys.public_key(),
                vec![bob_key_package],
                create_nostr_group_config_data(admins),
            )
            .expect("Failed to create group");

        let group_id = create_result.group.mls_group_id.clone();

        // Alice merges and Bob joins
        alice_mdk
            .merge_pending_commit(&group_id)
            .expect("Failed to merge pending commit");

        let bob_welcome_rumor = &create_result.welcome_rumors[0];
        let bob_welcome = bob_mdk
            .process_welcome(&nostr::EventId::all_zeros(), bob_welcome_rumor)
            .expect("Bob should process welcome");
        bob_mdk
            .accept_welcome(&bob_welcome)
            .expect("Bob should accept welcome");

        // Bob (non-admin) tries to update group extensions
        let update =
            crate::groups::NostrGroupDataUpdate::new().name("Hacked Group Name".to_string());
        let result = bob_mdk.update_group_data(&group_id, update);

        // This should fail at the client level with a permission error
        assert!(
            result.is_err(),
            "Non-admin should not be able to update group data"
        );
        // The error is Error::Group with a message about admin permissions
        assert!(
            matches!(result.as_ref().unwrap_err(), crate::Error::Group(msg) if msg.contains("Only group admins")),
            "Error should indicate admin permission required, got: {:?}",
            result
        );
    }

    /// Test that a commit with only the update path (no explicit proposals) from non-admin succeeds
    ///
    /// In MLS, a commit can update the sender's leaf via the "update path" without
    /// including explicit Update proposals. This tests that such commits from
    /// non-admins are correctly identified as self-updates and allowed.
    #[test]
    fn test_non_admin_empty_self_update_commit_succeeds() {
        let alice_keys = Keys::generate();
        let bob_keys = Keys::generate();

        let alice_mdk = create_test_mdk();
        let bob_mdk = create_test_mdk();

        // Only Alice is admin
        let admins = vec![alice_keys.public_key()];

        let bob_key_package = create_key_package_event(&bob_mdk, &bob_keys);

        // Alice creates the group with Bob
        let create_result = alice_mdk
            .create_group(
                &alice_keys.public_key(),
                vec![bob_key_package],
                create_nostr_group_config_data(admins),
            )
            .expect("Failed to create group");

        let group_id = create_result.group.mls_group_id.clone();

        alice_mdk
            .merge_pending_commit(&group_id)
            .expect("Failed to merge pending commit");

        let bob_welcome_rumor = &create_result.welcome_rumors[0];
        let bob_welcome = bob_mdk
            .process_welcome(&nostr::EventId::all_zeros(), bob_welcome_rumor)
            .expect("Bob should process welcome");
        bob_mdk
            .accept_welcome(&bob_welcome)
            .expect("Bob should accept welcome");

        // Verify Bob is not admin
        let group_state = bob_mdk
            .get_group(&group_id)
            .expect("Failed to get group")
            .expect("Group should exist");
        assert!(
            !group_state.admin_pubkeys.contains(&bob_keys.public_key()),
            "Bob should NOT be an admin"
        );

        // Bob performs multiple self-updates to verify the pattern is consistently allowed
        for i in 0..3 {
            let bob_update_result = bob_mdk
                .self_update(&group_id)
                .unwrap_or_else(|e| panic!("Bob self-update {} should succeed: {:?}", i + 1, e));

            // Alice processes Bob's self-update
            let result = alice_mdk.process_message(&bob_update_result.evolution_event);
            assert!(
                result.is_ok(),
                "Non-admin self-update {} should succeed, got: {:?}",
                i + 1,
                result.err()
            );

            // Bob merges his own commit
            bob_mdk
                .merge_pending_commit(&group_id)
                .unwrap_or_else(|e| panic!("Bob should merge self-update {}: {:?}", i + 1, e));
        }
    }
}
