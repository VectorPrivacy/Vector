//! Shared image processing utilities for validation and metadata extraction
//!
//! This module provides common image processing functionality used by both
//! MIP-04 encrypted media and MIP-01 group images. It includes:
//! - Image validation (dimensions, file size, format)
//! - Metadata extraction (dimensions, blurhash)
//! - EXIF sanitization for privacy

pub mod metadata;
pub mod types;
pub mod validation;

// Re-export commonly used types and functions
pub use types::{
    ImageMetadata, MAX_FILE_SIZE, MAX_FILENAME_LENGTH, MAX_IMAGE_DIMENSION, MediaProcessingError,
    MediaProcessingOptions,
};
