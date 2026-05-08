//! Type definitions for encrypted media functionality
//!
//! This module contains all the core types, constants, and error definitions
//! used throughout the encrypted media system.

// Re-export shared types and constants from media_processing module
pub use crate::media_processing::{
    MAX_FILE_SIZE, MAX_FILENAME_LENGTH, MAX_IMAGE_DIMENSION, MediaProcessingOptions,
};

/// Metadata extracted from media files
#[derive(Debug, Clone)]
pub struct MediaMetadata {
    /// MIME type of the media
    pub mime_type: String,
    /// Dimensions for images/videos (width, height)
    pub dimensions: Option<(u32, u32)>,
    /// Blurhash for images
    pub blurhash: Option<String>,
    /// Original file size in bytes
    pub original_size: u64,
}

/// Encrypted media ready for upload
#[derive(Debug, Clone)]
pub struct EncryptedMediaUpload {
    /// The encrypted media data
    pub encrypted_data: Vec<u8>,
    /// Hash of the original (unencrypted) data
    pub original_hash: [u8; 32],
    /// Hash of the encrypted data (for verification)
    pub encrypted_hash: [u8; 32],
    /// MIME type of the original media
    pub mime_type: String,
    /// Original filename
    pub filename: String,
    /// Size of original data before encryption
    pub original_size: u64,
    /// Size of encrypted data
    pub encrypted_size: u64,
    /// Image/video dimensions if applicable
    pub dimensions: Option<(u32, u32)>,
    /// Blurhash for images
    pub blurhash: Option<String>,
    /// Encryption nonce (96 bits, 12 bytes) - randomly generated per encryption
    pub nonce: [u8; 12],
}

/// Reference to encrypted media
#[derive(Debug, Clone)]
pub struct MediaReference {
    /// URL where the encrypted media is stored
    pub url: String,
    /// Hash of original data for verification
    pub original_hash: [u8; 32],
    /// MIME type
    pub mime_type: String,
    /// Original filename
    pub filename: String,
    /// Dimensions if applicable
    pub dimensions: Option<(u32, u32)>,
    /// Encryption scheme version (e.g., "mip04-v2")
    pub scheme_version: String,
    /// Encryption nonce (96 bits, 12 bytes) - required for decryption
    pub nonce: [u8; 12],
}

/// Errors that can occur during encrypted media operations
#[derive(Debug, thiserror::Error)]
pub enum EncryptedMediaError {
    /// Media processing error (validation, metadata extraction, etc.)
    #[error(transparent)]
    MediaProcessing(#[from] crate::media_processing::types::MediaProcessingError),

    /// Unsupported MIME type
    #[error("MIME type '{mime_type}' is not supported")]
    UnsupportedMimeType {
        /// The unsupported MIME type
        mime_type: String,
    },

    /// Encryption failed
    #[error("Encryption failed: {reason}")]
    EncryptionFailed {
        /// The reason for encryption failure
        reason: String,
    },

    /// Decryption failed
    #[error("Decryption failed: {reason}")]
    DecryptionFailed {
        /// The reason for decryption failure
        reason: String,
    },

    /// Hash verification failed
    #[error("Hash verification failed")]
    HashVerificationFailed,

    /// Group not found
    #[error("MLS group not found")]
    GroupNotFound,

    /// Invalid nonce
    #[error("Invalid encryption nonce")]
    InvalidNonce,

    /// Invalid IMETA tag format
    #[error("Invalid IMETA tag format: {reason}")]
    InvalidImetaTag {
        /// The reason for the invalid IMETA tag
        reason: String,
    },

    /// No exporter secret found for the hinted epoch
    #[error("No exporter secret found for epoch {0}")]
    NoExporterSecretForEpoch(u64),

    /// Unknown encryption scheme version
    #[error("Unknown encryption scheme version: {0}")]
    UnknownSchemeVersion(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_media_processing_options_default() {
        let options = MediaProcessingOptions::default();
        assert!(options.sanitize_exif);
        assert!(options.generate_blurhash);
        assert_eq!(options.max_dimension, Some(MAX_IMAGE_DIMENSION));
        assert_eq!(options.max_file_size, Some(MAX_FILE_SIZE));
    }

    #[test]
    fn test_media_processing_options() {
        let default_options = MediaProcessingOptions::default();
        assert!(default_options.sanitize_exif);
        assert!(default_options.generate_blurhash);
        assert_eq!(default_options.max_dimension, Some(MAX_IMAGE_DIMENSION));
        assert_eq!(default_options.max_file_size, Some(MAX_FILE_SIZE));

        // Test custom options
        let custom_options = MediaProcessingOptions {
            sanitize_exif: false,
            generate_blurhash: false,
            max_dimension: Some(1024),
            max_file_size: Some(1024 * 1024),
            max_filename_length: Some(100),
        };

        assert!(!custom_options.sanitize_exif);
        assert!(!custom_options.generate_blurhash);
        assert_eq!(custom_options.max_dimension, Some(1024));
        assert_eq!(custom_options.max_file_size, Some(1024 * 1024));
    }

    #[test]
    fn test_encrypted_media_upload_structure() {
        let upload = EncryptedMediaUpload {
            encrypted_data: vec![1, 2, 3, 4, 5],
            original_hash: [0x01; 32],
            encrypted_hash: [0x02; 32],
            mime_type: "image/webp".to_string(),
            filename: "test.webp".to_string(),
            original_size: 5000,
            encrypted_size: 5016, // Original + ChaCha20-Poly1305 overhead
            dimensions: Some((1024, 768)),
            blurhash: Some("L6PZfSi_.AyE_3t7t7R**0o#DgR4".to_string()),
            nonce: [0x03; 12],
        };

        // Verify all fields are accessible
        assert_eq!(upload.encrypted_data.len(), 5);
        assert_eq!(upload.original_hash, [0x01; 32]);
        assert_eq!(upload.encrypted_hash, [0x02; 32]);
        assert_eq!(upload.mime_type, "image/webp");
        assert_eq!(upload.filename, "test.webp");
        assert_eq!(upload.original_size, 5000);
        assert_eq!(upload.encrypted_size, 5016);
        assert_eq!(upload.dimensions, Some((1024, 768)));
        assert!(upload.blurhash.is_some());
        assert_eq!(upload.nonce, [0x03; 12]);
    }

    #[test]
    fn test_media_reference_structure() {
        let media_ref = MediaReference {
            url: "https://storage.example.com/abc123.enc".to_string(),
            original_hash: [0xFF; 32],
            mime_type: "video/mp4".to_string(),
            filename: "test.mp4".to_string(),
            dimensions: Some((1920, 1080)),
            scheme_version: "mip04-v2".to_string(),
            nonce: [0xAA; 12],
        };

        // Verify all fields are accessible
        assert_eq!(media_ref.url, "https://storage.example.com/abc123.enc");
        assert_eq!(media_ref.original_hash, [0xFF; 32]);
        assert_eq!(media_ref.mime_type, "video/mp4");
        assert_eq!(media_ref.filename, "test.mp4");
        assert_eq!(media_ref.dimensions, Some((1920, 1080)));
        assert_eq!(media_ref.scheme_version, "mip04-v2");
        assert_eq!(media_ref.nonce, [0xAA; 12]);
    }

    #[test]
    fn test_error_types() {
        use crate::media_processing::types::MediaProcessingError;

        // Test that all error types can be created and formatted properly
        let errors = vec![
            EncryptedMediaError::MediaProcessing(MediaProcessingError::FileTooLarge {
                size: 1000,
                max_size: 500,
            }),
            EncryptedMediaError::MediaProcessing(MediaProcessingError::InvalidMimeType {
                mime_type: "invalid".to_string(),
            }),
            EncryptedMediaError::MediaProcessing(MediaProcessingError::FilenameTooLong {
                length: 300,
                max_length: 210,
            }),
            EncryptedMediaError::MediaProcessing(MediaProcessingError::EmptyFilename),
            EncryptedMediaError::MediaProcessing(MediaProcessingError::ImageDimensionsTooLarge {
                width: 20000,
                height: 15000,
                max_dimension: 16384,
            }),
            EncryptedMediaError::MediaProcessing(MediaProcessingError::ImageTooManyPixels {
                total_pixels: 100_000_000,
                max_pixels: 50_000_000,
            }),
            EncryptedMediaError::MediaProcessing(MediaProcessingError::ImageMemoryTooLarge {
                estimated_mb: 1024,
                max_mb: 256,
            }),
            EncryptedMediaError::MediaProcessing(MediaProcessingError::MetadataExtractionFailed {
                reason: "Test metadata failure".to_string(),
            }),
            EncryptedMediaError::EncryptionFailed {
                reason: "Test encryption failure".to_string(),
            },
            EncryptedMediaError::DecryptionFailed {
                reason: "Test decryption failure".to_string(),
            },
            EncryptedMediaError::HashVerificationFailed,
            EncryptedMediaError::GroupNotFound,
            EncryptedMediaError::InvalidNonce,
            EncryptedMediaError::InvalidImetaTag {
                reason: "Test invalid tag".to_string(),
            },
            EncryptedMediaError::NoExporterSecretForEpoch(42),
            EncryptedMediaError::UnknownSchemeVersion("v99".to_string()),
        ];

        // Verify all errors can be formatted (tests Display implementation)
        for error in errors {
            let error_string = format!("{}", error);
            assert!(!error_string.is_empty());
        }
    }
}
