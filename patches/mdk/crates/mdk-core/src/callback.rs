//! Callback interface for MDK events.
//!
//! This module provides the [`MdkCallback`] trait that applications can implement
//! to receive notifications about important MDK events, such as rollbacks due to
//! commit race resolution.

use std::fmt::Debug;

use mdk_storage_traits::GroupId;
use nostr::EventId;

/// Information about a rollback that occurred due to commit race resolution.
#[derive(Debug, Clone)]
pub struct RollbackInfo {
    /// The group that was rolled back
    pub group_id: GroupId,
    /// The epoch the group was rolled back to
    pub target_epoch: u64,
    /// The new head event after rollback
    pub new_head_event: EventId,
    /// Message event IDs that were marked as EpochInvalidated
    pub invalidated_messages: Vec<EventId>,
    /// ProcessedMessage wrapper event IDs that were marked as EpochInvalidated
    pub messages_needing_refetch: Vec<EventId>,
}

/// Callback interface for MDK events.
pub trait MdkCallback: Send + Sync + Debug {
    /// Notifies that a rollback occurred due to race resolution.
    ///
    /// This happens when a commit with an earlier timestamp or smaller ID arrives
    /// after we have already applied a commit for the same epoch. MDK rolls back
    /// to the previous state and applies the winner.
    ///
    /// The [`RollbackInfo`] contains:
    /// - `group_id`: The group that was rolled back
    /// - `target_epoch`: The epoch the group was rolled back to
    /// - `new_head_event`: The new head event after rollback
    /// - `invalidated_messages`: Message event IDs that were marked as EpochInvalidated
    /// - `messages_needing_refetch`: ProcessedMessage wrapper event IDs that need refetching
    ///
    /// Applications can use this information to:
    /// - Show messages with uncertainty indicators ("this message may have changed")
    /// - Re-fetch and reprocess invalidated messages when the correct epoch state is restored
    /// - Update UI to reflect the rollback state
    fn on_rollback(&self, info: &RollbackInfo);
}
