//! Welcomes module
//!
//! This module is responsible for storing and retrieving welcomes
//! It also handles the parsing of welcome content
//!
//! The welcomes are stored in the database and can be retrieved by event ID
//!
//! Here we also define the storage traits that are used to store and retrieve welcomes

use nostr::EventId;

pub mod error;
pub mod types;

use self::error::WelcomeError;
use self::types::*;

/// Default limit for pending welcomes queries to prevent unbounded memory usage
pub const DEFAULT_PENDING_WELCOMES_LIMIT: usize = 1000;

/// Maximum allowed limit for pending welcomes queries to prevent resource exhaustion
pub const MAX_PENDING_WELCOMES_LIMIT: usize = 10000;

/// Pagination parameters for querying pending welcomes
#[derive(Debug, Clone, Copy)]
pub struct Pagination {
    /// Maximum number of welcomes to return
    pub limit: Option<usize>,
    /// Number of welcomes to skip
    pub offset: Option<usize>,
}

impl Pagination {
    /// Create a new Pagination with specified limit and offset
    pub fn new(limit: Option<usize>, offset: Option<usize>) -> Self {
        Self { limit, offset }
    }

    /// Get the limit value, using default if not specified
    pub fn limit(&self) -> usize {
        self.limit.unwrap_or(DEFAULT_PENDING_WELCOMES_LIMIT)
    }

    /// Get the offset value, using 0 if not specified
    pub fn offset(&self) -> usize {
        self.offset.unwrap_or(0)
    }
}

impl Default for Pagination {
    fn default() -> Self {
        Self {
            limit: Some(DEFAULT_PENDING_WELCOMES_LIMIT),
            offset: Some(0),
        }
    }
}

/// Storage traits for the welcomes module
pub trait WelcomeStorage {
    /// Save a welcome
    fn save_welcome(&self, welcome: Welcome) -> Result<(), WelcomeError>;

    /// Find a welcome by event ID
    fn find_welcome_by_event_id(&self, event_id: &EventId)
    -> Result<Option<Welcome>, WelcomeError>;

    /// Get pending welcomes with optional pagination
    ///
    /// # Arguments
    ///
    /// * `pagination` - Optional pagination parameters. If `None`, uses default limit and offset.
    ///
    /// # Returns
    ///
    /// Returns a vector of pending welcomes ordered by ID (descending)
    ///
    /// # Errors
    ///
    /// Returns [`WelcomeError::InvalidParameters`] if:
    /// - `limit` is 0
    /// - `limit` exceeds [`MAX_PENDING_WELCOMES_LIMIT`]
    ///
    /// # Examples
    ///
    /// ```ignore
    /// // Get pending welcomes with default pagination
    /// let welcomes = storage.pending_welcomes(None)?;
    ///
    /// // Get first 10 pending welcomes
    /// let welcomes = storage.pending_welcomes(Some(Pagination::new(Some(10), Some(0))))?;
    ///
    /// // Get next 10 pending welcomes
    /// let welcomes = storage.pending_welcomes(Some(Pagination::new(Some(10), Some(10))))?;
    /// ```
    fn pending_welcomes(
        &self,
        pagination: Option<Pagination>,
    ) -> Result<Vec<Welcome>, WelcomeError>;

    /// Save a processed welcome
    fn save_processed_welcome(
        &self,
        processed_welcome: ProcessedWelcome,
    ) -> Result<(), WelcomeError>;

    /// Find a processed welcome by event ID
    fn find_processed_welcome_by_event_id(
        &self,
        event_id: &EventId,
    ) -> Result<Option<ProcessedWelcome>, WelcomeError>;
}
