//! Groups module
//!
//! This module is responsible for storing and retrieving groups
//! It also handles the parsing of group content
//!
//! The groups are stored in the database and can be retrieved by MLS group ID or Nostr group ID
//!
//! Here we also define the storage traits that are used to store and retrieve groups

use std::collections::BTreeSet;

use crate::GroupId;
use nostr::{PublicKey, RelayUrl, Timestamp};

pub mod error;
pub mod types;

use self::error::GroupError;
use self::types::*;
use crate::messages::types::Message;

/// Default limit for messages queries to prevent unbounded memory usage
pub const DEFAULT_MESSAGE_LIMIT: usize = 1000;

/// Maximum allowed limit for messages queries to prevent resource exhaustion
pub const MAX_MESSAGE_LIMIT: usize = 10000;

/// Sort order for message queries
///
/// Controls the column priority used when ordering messages.
/// Both orderings are descending (newest first) and use three columns
/// as a compound sort key to guarantee stable, deterministic results.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MessageSortOrder {
    /// Sort by `created_at DESC, processed_at DESC, id DESC` (default).
    ///
    /// Best for showing messages in sender-timestamp order.
    /// This is the natural ordering when the sender's clock is trusted.
    #[default]
    CreatedAtFirst,

    /// Sort by `processed_at DESC, created_at DESC, id DESC`.
    ///
    /// Best for showing messages in local reception order.
    /// This avoids visual reordering caused by clock skew between senders
    /// and ensures that the most recently received messages always appear first.
    ProcessedAtFirst,
}

/// Pagination parameters for querying messages
#[derive(Debug, Clone, Copy)]
pub struct Pagination {
    /// Maximum number of messages to return
    pub limit: Option<usize>,
    /// Number of messages to skip
    pub offset: Option<usize>,
    /// Sort order for the query results. Defaults to [`MessageSortOrder::CreatedAtFirst`].
    pub sort_order: Option<MessageSortOrder>,
}

impl Pagination {
    /// Create a new Pagination with specified limit and offset
    pub fn new(limit: Option<usize>, offset: Option<usize>) -> Self {
        Self {
            limit,
            offset,
            sort_order: None,
        }
    }

    /// Create a new Pagination with specified limit, offset, and sort order
    pub fn with_sort_order(
        limit: Option<usize>,
        offset: Option<usize>,
        sort_order: MessageSortOrder,
    ) -> Self {
        Self {
            limit,
            offset,
            sort_order: Some(sort_order),
        }
    }

    /// Get the limit value, using default if not specified
    pub fn limit(&self) -> usize {
        self.limit.unwrap_or(DEFAULT_MESSAGE_LIMIT)
    }

    /// Get the offset value, using 0 if not specified
    pub fn offset(&self) -> usize {
        self.offset.unwrap_or(0)
    }

    /// Get the sort order, using default if not specified
    pub fn sort_order(&self) -> MessageSortOrder {
        self.sort_order.unwrap_or_default()
    }
}

impl Default for Pagination {
    fn default() -> Self {
        Self {
            limit: Some(DEFAULT_MESSAGE_LIMIT),
            offset: Some(0),
            sort_order: None,
        }
    }
}

/// Storage traits for the groups module
pub trait GroupStorage {
    /// Get all groups
    fn all_groups(&self) -> Result<Vec<Group>, GroupError>;

    /// Find a group by MLS group ID
    fn find_group_by_mls_group_id(&self, group_id: &GroupId) -> Result<Option<Group>, GroupError>;

    /// Find a group by Nostr group ID
    fn find_group_by_nostr_group_id(
        &self,
        nostr_group_id: &[u8; 32],
    ) -> Result<Option<Group>, GroupError>;

    /// Save a group
    fn save_group(&self, group: Group) -> Result<(), GroupError>;

    /// Get messages for a group with optional pagination and sort order
    ///
    /// Returns messages ordered according to the sort order specified in
    /// [`Pagination::sort_order`] (defaults to [`MessageSortOrder::CreatedAtFirst`]).
    ///
    /// ## Sort orders
    ///
    /// **[`MessageSortOrder::CreatedAtFirst`]** (default):
    /// `created_at DESC, processed_at DESC, id DESC`
    /// - Primary sort by the sender's timestamp
    /// - `processed_at` tiebreaker keeps reception order when `created_at` matches
    /// - `id` ensures deterministic ordering when both timestamps are equal
    ///
    /// **[`MessageSortOrder::ProcessedAtFirst`]**:
    /// `processed_at DESC, created_at DESC, id DESC`
    /// - Primary sort by when this client received the message
    /// - Best for local reception ordering; avoids visual reordering from clock skew
    /// - `created_at` and `id` provide secondary/tertiary tiebreakers
    ///
    /// # Arguments
    /// * `group_id` - The group ID to fetch messages for
    /// * `pagination` - Optional pagination parameters. If `None`, uses default limit, offset,
    ///   and sort order.
    ///
    /// # Returns
    ///
    /// Returns a vector of messages in the requested sort order
    ///
    /// # Errors
    ///
    /// Returns [`GroupError::InvalidParameters`] if:
    /// - `limit` is 0
    /// - `limit` exceeds [`MAX_MESSAGE_LIMIT`]
    /// - Group with the specified ID does not exist
    ///
    /// # Examples
    /// ```ignore
    /// // Get messages with default pagination (created_at first)
    /// let messages = storage.messages(&group_id, None)?;
    ///
    /// // Get first 100 messages sorted by created_at
    /// let messages = storage.messages(&group_id, Some(Pagination::new(Some(100), Some(0))))?;
    ///
    /// // Get first 100 messages sorted by processed_at
    /// let messages = storage.messages(
    ///     &group_id,
    ///     Some(Pagination::with_sort_order(Some(100), Some(0), MessageSortOrder::ProcessedAtFirst)),
    /// )?;
    /// ```
    fn messages(
        &self,
        group_id: &GroupId,
        pagination: Option<Pagination>,
    ) -> Result<Vec<Message>, GroupError>;

    /// Get the most recent message in a group according to the given sort order.
    ///
    /// This is equivalent to calling [`messages()`](GroupStorage::messages) with `limit=1, offset=0`
    /// and the specified sort order, but may be implemented more efficiently.
    ///
    /// Clients can use this to obtain the "last message" that is consistent with the
    /// sort order they pass to [`messages()`](GroupStorage::messages), which may differ
    /// from the cached [`Group::last_message_id`] (which always reflects
    /// [`MessageSortOrder::CreatedAtFirst`]).
    ///
    /// # Arguments
    /// * `group_id` - The group ID to fetch the last message for
    /// * `sort_order` - The sort order to use when determining the "last" message
    ///
    /// # Returns
    ///
    /// Returns the first message in the given sort order, or `None` if the group has no messages.
    ///
    /// # Errors
    ///
    /// Returns [`GroupError::InvalidParameters`] if the group does not exist.
    fn last_message(
        &self,
        group_id: &GroupId,
        sort_order: MessageSortOrder,
    ) -> Result<Option<Message>, GroupError>;

    /// Get all admins for a group
    fn admins(&self, group_id: &GroupId) -> Result<BTreeSet<PublicKey>, GroupError>;

    /// Get all relays for a group
    fn group_relays(&self, group_id: &GroupId) -> Result<BTreeSet<GroupRelay>, GroupError>;

    /// Replace all relays for a group with the provided set
    /// This operation is atomic - either all relays are replaced or none are changed
    fn replace_group_relays(
        &self,
        group_id: &GroupId,
        relays: BTreeSet<RelayUrl>,
    ) -> Result<(), GroupError>;

    /// Get an exporter secret for a group and epoch
    fn get_group_exporter_secret(
        &self,
        group_id: &GroupId,
        epoch: u64,
    ) -> Result<Option<GroupExporterSecret>, GroupError>;

    /// Save an exporter secret for a group and epoch
    fn save_group_exporter_secret(
        &self,
        group_exporter_secret: GroupExporterSecret,
    ) -> Result<(), GroupError>;

    /// Returns active groups that need a self-update: either because
    /// `self_update_state` is [`SelfUpdateState::Required`] (post-join
    /// requirement per MIP-02) or because the last self-update is older than
    /// `threshold_secs` seconds ago (periodic rotation per MIP-00).
    fn groups_needing_self_update(&self, threshold_secs: u64) -> Result<Vec<GroupId>, GroupError> {
        let now = Timestamp::now().as_secs();
        let groups = self.all_groups()?;
        Ok(groups
            .into_iter()
            .filter(|g| {
                if g.state != types::GroupState::Active {
                    return false;
                }
                match g.self_update_state {
                    types::SelfUpdateState::Required => true,
                    types::SelfUpdateState::CompletedAt(ts) => {
                        now.saturating_sub(ts.as_secs()) >= threshold_secs
                    }
                }
            })
            .map(|g| g.mls_group_id)
            .collect())
    }
}
