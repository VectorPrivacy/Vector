//! Sync mode enumeration for message synchronization state.

/// Represents the current synchronization mode for message fetching.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum SyncMode {
    /// Initial sync from most recent message going backward
    ForwardSync,
    /// Syncing historically old messages
    BackwardSync,
    /// Deep rescan mode - continues until 30 days of no events
    DeepRescan,
    /// Sync complete
    Finished,
}
