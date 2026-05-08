//! Encrypted Media support for the Marmot protocol
//!
//! This module implements the Encrypted Media specification from the Marmot protocol (04.md),
//! providing functionality for secure media sharing within MLS groups on Nostr.
//!
//! The Encrypted Media feature allows group members to share media files (images, videos, etc.)
//! in a secure manner, leveraging the MLS group's encryption keys and Nostr's event system.

// Internal modules
pub mod crypto;
pub mod manager;
pub mod metadata;
pub mod types;

// Re-export public API
pub use types::{
    EncryptedMediaError, EncryptedMediaUpload, MediaMetadata, MediaProcessingOptions,
    MediaReference,
};

pub use manager::EncryptedMediaManager;

// Re-export constants that users might need
pub use types::{MAX_FILE_SIZE, MAX_FILENAME_LENGTH, MAX_IMAGE_DIMENSION};
