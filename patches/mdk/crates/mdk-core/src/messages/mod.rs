//! MDK messages
//!
//! This module provides functionality for creating, processing, and managing encrypted
//! messages in MLS groups. It handles:
//! - Message creation and encryption
//! - Message processing and decryption
//! - Message state tracking
//! - Integration with Nostr events
//!
//! Messages in MDK are wrapped in Nostr events (kind:445) for relay transmission.
//! The message content is encrypted using both MLS group keys and NIP-44 encryption.
//! Message state is tracked to handle processing status and failure scenarios.

mod application;
mod commit;
mod create;
mod decryption;
mod error_handling;
mod process;
mod proposal;
mod validation;

use mdk_storage_traits::groups::types as group_types;
use mdk_storage_traits::groups::{MessageSortOrder, Pagination};
use mdk_storage_traits::messages::types as message_types;
use mdk_storage_traits::{GroupId, MdkStorageProvider};
use nostr::{EventId, Timestamp};

use crate::MDK;
use crate::error::Error;
use crate::groups::UpdateGroupResult;

// Internal Result type alias for this module
pub(crate) type Result<T> = std::result::Result<T, Error>;

// =============================================================================
// Helper Functions for ProcessedMessage Creation
// =============================================================================

/// Creates a ProcessedMessage record with common defaults
///
/// This helper reduces boilerplate across the many places that create
/// ProcessedMessage records. The `processed_at` timestamp is automatically
/// set to the current time.
pub(crate) fn create_processed_message_record(
    wrapper_event_id: EventId,
    message_event_id: Option<EventId>,
    epoch: Option<u64>,
    mls_group_id: Option<GroupId>,
    state: message_types::ProcessedMessageState,
    failure_reason: Option<String>,
) -> message_types::ProcessedMessage {
    message_types::ProcessedMessage {
        wrapper_event_id,
        message_event_id,
        processed_at: Timestamp::now(),
        epoch,
        mls_group_id,
        state,
        failure_reason,
    }
}

/// Default number of epochs to look back when trying to decrypt messages with older exporter secrets
pub(crate) const DEFAULT_EPOCH_LOOKBACK: u64 = 5;

/// MessageProcessingResult covers the full spectrum of responses that we can get back from attempting to process a message
pub enum MessageProcessingResult {
    /// An application message (this is usually a message in a chat)
    ApplicationMessage(message_types::Message),
    /// Proposal message that was auto-committed (self-remove proposals when receiver is admin)
    Proposal(UpdateGroupResult),
    /// Pending proposal message stored but not committed
    ///
    /// For add/remove member proposals, these are always stored as pending so that
    /// admins can approve them through a manual commit. For self-remove (leave) proposals,
    /// these are stored as pending when the receiver is not an admin.
    PendingProposal {
        /// The MLS group ID this pending proposal belongs to
        mls_group_id: GroupId,
    },
    /// Proposal was ignored and not stored
    ///
    /// This occurs for proposals that should not be processed, such as:
    /// - Extension/ciphersuite change proposals (admins should create commits directly)
    /// - Other unsupported proposal types
    IgnoredProposal {
        /// The MLS group ID this proposal was for
        mls_group_id: GroupId,
        /// Reason the proposal was ignored
        reason: String,
    },
    /// External Join Proposal
    ExternalJoinProposal {
        /// The MLS group ID this proposal belongs to
        mls_group_id: GroupId,
    },
    /// Commit message
    Commit {
        /// The MLS group ID this commit applies to
        mls_group_id: GroupId,
    },
    /// Unprocessable message
    Unprocessable {
        /// The MLS group ID of the message that could not be processed
        mls_group_id: GroupId,
    },
    /// Message was previously marked as failed and cannot be reprocessed
    ///
    /// This variant is returned when a message that previously failed processing
    /// is received again. Unlike `Unprocessable`, this does not require an MLS group ID
    /// because the group ID may not be extractable from malformed messages.
    PreviouslyFailed,
}

impl std::fmt::Debug for MessageProcessingResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ApplicationMessage(msg) => f
                .debug_struct("ApplicationMessage")
                .field("id", &msg.id)
                .field("pubkey", &msg.pubkey)
                .field("kind", &msg.kind)
                .field("mls_group_id", &"[REDACTED]")
                .field("created_at", &msg.created_at)
                .field("state", &msg.state)
                .finish(),
            Self::Proposal(result) => f
                .debug_struct("Proposal")
                .field("evolution_event_id", &result.evolution_event.id)
                .field("mls_group_id", &"[REDACTED]")
                .finish(),
            Self::PendingProposal { .. } => f
                .debug_struct("PendingProposal")
                .field("mls_group_id", &"[REDACTED]")
                .finish(),
            Self::IgnoredProposal { reason, .. } => f
                .debug_struct("IgnoredProposal")
                .field("mls_group_id", &"[REDACTED]")
                .field("reason", reason)
                .finish(),
            Self::ExternalJoinProposal { .. } => f
                .debug_struct("ExternalJoinProposal")
                .field("mls_group_id", &"[REDACTED]")
                .finish(),
            Self::Commit { .. } => f
                .debug_struct("Commit")
                .field("mls_group_id", &"[REDACTED]")
                .finish(),
            Self::Unprocessable { .. } => f
                .debug_struct("Unprocessable")
                .field("mls_group_id", &"[REDACTED]")
                .finish(),
            Self::PreviouslyFailed => f.debug_struct("PreviouslyFailed").finish(),
        }
    }
}

impl<Storage> MDK<Storage>
where
    Storage: MdkStorageProvider,
{
    /// Retrieves a message by its Nostr event ID within a specific group
    ///
    /// This function looks up a message in storage using its associated Nostr event ID
    /// and MLS group ID. The message must have been previously processed and stored.
    /// Requiring both the event ID and group ID prevents messages from different groups
    /// from overwriting each other.
    ///
    /// # Arguments
    ///
    /// * `mls_group_id` - The MLS group ID the message belongs to
    /// * `event_id` - The Nostr event ID to look up
    ///
    /// # Returns
    ///
    /// * `Ok(Some(Message))` - The message if found
    /// * `Ok(None)` - If no message exists with the given event ID in the specified group
    /// * `Err(Error)` - If there is an error accessing storage
    pub fn get_message(
        &self,
        mls_group_id: &GroupId,
        event_id: &EventId,
    ) -> Result<Option<message_types::Message>> {
        self.storage()
            .find_message_by_event_id(mls_group_id, event_id)
            .map_err(|_e| Error::Message("Storage error while finding message".to_string()))
    }

    /// Retrieves messages for a specific MLS group with optional pagination
    ///
    /// This function returns messages that have been processed and stored for a group,
    /// ordered by creation time (descending). If no pagination is specified, uses default
    /// pagination (1000 messages, offset 0).
    ///
    /// # Arguments
    ///
    /// * `mls_group_id` - The MLS group ID to get messages for
    /// * `pagination` - Optional pagination parameters. If `None`, uses default limit and offset.
    ///
    /// # Returns
    ///
    /// * `Ok(Vec<Message>)` - List of messages for the group (up to limit)
    /// * `Err(Error)` - If there is an error accessing storage
    ///
    /// # Examples
    ///
    /// ```ignore
    /// // Get messages with default pagination (1000 messages, offset 0)
    /// let messages = mdk.get_messages(&group_id, None)?;
    ///
    /// // Get first 100 messages
    /// use mdk_storage_traits::groups::Pagination;
    /// let messages = mdk.get_messages(&group_id, Some(Pagination::new(Some(100), Some(0))))?;
    ///
    /// // Get next 100 messages
    /// let messages = mdk.get_messages(&group_id, Some(Pagination::new(Some(100), Some(100))))?;
    /// ```
    pub fn get_messages(
        &self,
        mls_group_id: &GroupId,
        pagination: Option<Pagination>,
    ) -> Result<Vec<message_types::Message>> {
        self.storage()
            .messages(mls_group_id, pagination)
            .map_err(|_e| Error::Message("Storage error while getting messages".to_string()))
    }

    /// Returns the most recent message in a group according to the given sort order.
    ///
    /// This is useful for clients that use [`MessageSortOrder::ProcessedAtFirst`] and
    /// need a "last message" value that is consistent with their [`get_messages()`](Self::get_messages)
    /// ordering. The cached [`Group::last_message_id`](group_types::Group::last_message_id) always
    /// reflects [`MessageSortOrder::CreatedAtFirst`], so clients using a different sort order
    /// can call this method instead.
    ///
    /// # Arguments
    ///
    /// * `mls_group_id` - The MLS group ID
    /// * `sort_order` - The sort order to use when determining the "last" message
    ///
    /// # Returns
    ///
    /// * `Ok(Some(Message))` - The most recent message under the given ordering
    /// * `Ok(None)` - If the group has no messages
    /// * `Err(Error)` - If the group does not exist or a storage error occurs
    pub fn get_last_message(
        &self,
        mls_group_id: &GroupId,
        sort_order: MessageSortOrder,
    ) -> Result<Option<message_types::Message>> {
        self.storage()
            .last_message(mls_group_id, sort_order)
            .map_err(|_e| Error::Message("Storage error while getting last message".to_string()))
    }

    // =========================================================================
    // Storage Save Helpers
    // =========================================================================

    /// Saves a message record to storage with standardized error handling
    pub(crate) fn save_message_record(&self, message: message_types::Message) -> Result<()> {
        self.storage()
            .save_message(message)
            .map_err(|_e| Error::Message("Storage error while saving message".to_string()))
    }

    /// Saves a processed message record to storage with standardized error handling
    pub(crate) fn save_processed_message_record(
        &self,
        processed_message: message_types::ProcessedMessage,
    ) -> Result<()> {
        self.storage()
            .save_processed_message(processed_message)
            .map_err(|_e| {
                Error::Message("Storage error while saving processed message".to_string())
            })
    }

    /// Saves a group record to storage with standardized error handling
    pub(crate) fn save_group_record(&self, group: group_types::Group) -> Result<()> {
        self.storage()
            .save_group(group)
            .map_err(|_e| Error::Group("Storage error while saving group".to_string()))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use mdk_storage_traits::groups::Pagination;
    use nostr::EventId;

    use crate::test_util::*;
    use crate::tests::create_test_mdk;

    #[test]
    fn test_get_message_not_found() {
        let mdk = create_test_mdk();
        let (creator, members, admins) = create_test_group_members();
        let group_id = create_test_group(&mdk, &creator, &members, &admins);
        let non_existent_event_id = EventId::all_zeros();

        let result = mdk.get_message(&group_id, &non_existent_event_id);
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[test]
    fn test_get_messages_empty_group() {
        let mdk = create_test_mdk();
        let (creator, members, admins) = create_test_group_members();
        let group_id = create_test_group(&mdk, &creator, &members, &admins);

        let messages = mdk
            .get_messages(&group_id, None)
            .expect("Failed to get messages");
        assert!(messages.is_empty());
    }

    #[test]
    fn test_get_messages_with_pagination() {
        let mdk = create_test_mdk();
        let (creator, members, admins) = create_test_group_members();
        let group_id = create_test_group(&mdk, &creator, &members, &admins);

        // Create 15 messages
        for i in 0..15 {
            let rumor = create_test_rumor(&creator, &format!("Message {}", i));
            mdk.create_message(&group_id, rumor)
                .expect("Failed to create message");
        }

        // Test 1: Get first page (10 messages)
        let page1 = mdk
            .get_messages(&group_id, Some(Pagination::new(Some(10), Some(0))))
            .expect("Failed to get first page");
        assert_eq!(page1.len(), 10, "First page should have 10 messages");

        // Test 2: Get second page (5 messages)
        let page2 = mdk
            .get_messages(&group_id, Some(Pagination::new(Some(10), Some(10))))
            .expect("Failed to get second page");
        assert_eq!(page2.len(), 5, "Second page should have 5 messages");

        // Test 3: Verify no duplicates between pages
        let page1_ids: HashSet<_> = page1.iter().map(|m| m.id).collect();
        let page2_ids: HashSet<_> = page2.iter().map(|m| m.id).collect();
        assert!(
            page1_ids.is_disjoint(&page2_ids),
            "Pages should not have duplicate messages"
        );

        // Test 4: Get all messages with default pagination
        let all_messages = mdk
            .get_messages(&group_id, None)
            .expect("Failed to get all messages");
        assert_eq!(
            all_messages.len(),
            15,
            "Should get all 15 messages with default pagination"
        );

        // Test 5: Request beyond available messages
        let page3 = mdk
            .get_messages(&group_id, Some(Pagination::new(Some(10), Some(20))))
            .expect("Failed to get third page");
        assert!(
            page3.is_empty(),
            "Should return empty when offset exceeds message count"
        );

        // Test 6: Small page size
        let small_page = mdk
            .get_messages(&group_id, Some(Pagination::new(Some(3), Some(0))))
            .expect("Failed to get small page");
        assert_eq!(small_page.len(), 3, "Should respect small page size");
    }

    #[test]
    fn test_get_messages_for_group() {
        let mdk = create_test_mdk();
        let (creator, members, admins) = create_test_group_members();
        let group_id = create_test_group(&mdk, &creator, &members, &admins);

        // Create multiple messages
        let rumor1 = create_test_rumor(&creator, "First message");
        let rumor2 = create_test_rumor(&creator, "Second message");

        let _event1 = mdk
            .create_message(&group_id, rumor1)
            .expect("Failed to create first message");
        let _event2 = mdk
            .create_message(&group_id, rumor2)
            .expect("Failed to create second message");

        // Get all messages for the group
        let messages = mdk
            .get_messages(&group_id, None)
            .expect("Failed to get messages");

        assert_eq!(messages.len(), 2);

        // Verify message contents
        let contents: Vec<&str> = messages.iter().map(|m| m.content.as_str()).collect();
        assert!(contents.contains(&"First message"));
        assert!(contents.contains(&"Second message"));

        // Verify all messages belong to the correct group
        for message in &messages {
            assert_eq!(message.mls_group_id, group_id.clone());
        }
    }

    /// Test getting messages for non-existent group
    #[test]
    fn test_get_messages_nonexistent_group() {
        let mdk = create_test_mdk();
        let non_existent_group_id = crate::GroupId::from_slice(&[9, 9, 9, 9]);

        let result = mdk.get_messages(&non_existent_group_id, None);

        // Both storage implementations should return error for non-existent group
        assert!(
            result.is_err(),
            "Should return error for non-existent group"
        );
    }

    /// Test getting single message that doesn't exist
    #[test]
    fn test_get_nonexistent_message() {
        let mdk = create_test_mdk();
        let (creator, members, admins) = create_test_group_members();
        let group_id = create_test_group(&mdk, &creator, &members, &admins);
        let non_existent_id = nostr::EventId::all_zeros();

        let result = mdk.get_message(&group_id, &non_existent_id);

        assert!(result.is_ok(), "Should succeed");
        assert!(
            result.unwrap().is_none(),
            "Should return None for non-existent message"
        );
    }
}
