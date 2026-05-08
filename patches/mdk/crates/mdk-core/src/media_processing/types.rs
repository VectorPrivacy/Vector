//! Shared types and constants for image processing

/// Maximum file size for images (100MB)
pub const MAX_FILE_SIZE: usize = 100 * 1024 * 1024;

/// Maximum filename length
pub const MAX_FILENAME_LENGTH: usize = 210;

/// Maximum image dimension (width or height) - supports flagship phone cameras (200MP)
pub const MAX_IMAGE_DIMENSION: u32 = 16384;

/// Maximum total pixels allowed in an image (50 million pixels)
/// This prevents decompression bombs. At 50M pixels with 4 bytes per pixel (RGBA),
/// this allows ~200MB of decoded image data, which is reasonable for high-res images
/// but protects against malicious images that could exhaust memory.
pub const MAX_IMAGE_PIXELS: u64 = 50_000_000;

/// Maximum memory allowed for decoded images in MB (256MB)
/// This is a hard limit on memory allocation to prevent OOM from decompression bombs.
pub const MAX_IMAGE_MEMORY_MB: u64 = 256;

/// Unified options for media processing and validation
///
/// This type serves both MIP-04 encrypted media and MIP-01 group images,
/// providing configuration for validation, sanitization, and metadata extraction.
#[derive(Debug, Clone)]
pub struct MediaProcessingOptions {
    /// Sanitize EXIF and other metadata for privacy (default: true)
    pub sanitize_exif: bool,
    /// Generate blurhash for images (default: true)
    pub generate_blurhash: bool,
    /// Maximum allowed dimension for images (default: uses MAX_IMAGE_DIMENSION)
    pub max_dimension: Option<u32>,
    /// Custom file size limit (default: uses MAX_FILE_SIZE)
    pub max_file_size: Option<usize>,
    /// Maximum allowed filename length (default: uses MAX_FILENAME_LENGTH)
    pub max_filename_length: Option<usize>,
}

impl Default for MediaProcessingOptions {
    fn default() -> Self {
        Self {
            sanitize_exif: true,     // Privacy-first default
            generate_blurhash: true, // Good UX
            max_dimension: Some(MAX_IMAGE_DIMENSION),
            max_file_size: Some(MAX_FILE_SIZE),
            max_filename_length: Some(MAX_FILENAME_LENGTH),
        }
    }
}

impl MediaProcessingOptions {
    /// Create options suitable for validation-only use cases
    /// (no sanitization or blurhash generation)
    pub fn validation_only() -> Self {
        Self {
            sanitize_exif: false,
            generate_blurhash: false,
            ..Default::default()
        }
    }
}

/// Metadata extracted from image files
///
/// This is a simplified version focused on the core metadata that's useful
/// for both MIP-04 and MIP-01 use cases.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageMetadata {
    /// Image dimensions (width, height)
    pub dimensions: Option<(u32, u32)>,
    /// Blurhash for preview
    pub blurhash: Option<String>,
}

impl ImageMetadata {
    /// Create empty metadata
    pub fn new() -> Self {
        Self {
            dimensions: None,
            blurhash: None,
        }
    }
}

impl Default for ImageMetadata {
    fn default() -> Self {
        Self::new()
    }
}

/// Errors that can occur during image validation and processing
#[derive(Debug, thiserror::Error)]
pub enum MediaProcessingError {
    /// File is too large
    #[error("File size {size} exceeds maximum allowed size {max_size}")]
    FileTooLarge {
        /// The actual file size
        size: usize,
        /// The maximum allowed file size
        max_size: usize,
    },

    /// Invalid MIME type format
    #[error("Invalid MIME type format: {mime_type}")]
    InvalidMimeType {
        /// The invalid MIME type
        mime_type: String,
    },

    /// MIME type mismatch between claimed and detected
    #[error("MIME type mismatch: claimed '{claimed}' but detected '{detected}' from file data")]
    MimeTypeMismatch {
        /// The MIME type claimed by the application
        claimed: String,
        /// The MIME type detected from file content
        detected: String,
    },

    /// Filename is too long
    #[error("Filename length {length} exceeds maximum {max_length}")]
    FilenameTooLong {
        /// The actual filename length
        length: usize,
        /// The maximum allowed filename length
        max_length: usize,
    },

    /// Filename is empty or invalid
    #[error("Filename cannot be empty")]
    EmptyFilename,

    /// Filename contains invalid characters
    #[error("Filename contains invalid characters")]
    InvalidFilename,

    /// Image dimensions are too large
    #[error("Image dimensions {width}x{height} exceed maximum {max_dimension}")]
    ImageDimensionsTooLarge {
        /// The image width in pixels
        width: u32,
        /// The image height in pixels
        height: u32,
        /// The maximum allowed dimension
        max_dimension: u32,
    },

    /// Image has too many pixels (decompression bomb protection)
    #[error("Image has {total_pixels} pixels, exceeding maximum {max_pixels}")]
    ImageTooManyPixels {
        /// Total number of pixels
        total_pixels: u64,
        /// Maximum allowed pixels
        max_pixels: u64,
    },

    /// Image would require too much memory to decode (decompression bomb protection)
    #[error("Image would require {estimated_mb}MB to decode, exceeding maximum {max_mb}MB")]
    ImageMemoryTooLarge {
        /// Estimated memory requirement in MB
        estimated_mb: u64,
        /// Maximum allowed memory in MB
        max_mb: u64,
    },

    /// Metadata extraction failed
    #[error("Failed to extract metadata: {reason}")]
    MetadataExtractionFailed {
        /// The reason for metadata extraction failure
        reason: String,
    },
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
        assert_eq!(options.max_filename_length, Some(MAX_FILENAME_LENGTH));
    }

    #[test]
    fn test_media_processing_options_validation_only() {
        let options = MediaProcessingOptions::validation_only();
        assert!(!options.sanitize_exif);
        assert!(!options.generate_blurhash);
        assert_eq!(options.max_dimension, Some(MAX_IMAGE_DIMENSION));
        assert_eq!(options.max_file_size, Some(MAX_FILE_SIZE));
        assert_eq!(options.max_filename_length, Some(MAX_FILENAME_LENGTH));
    }

    #[test]
    fn test_image_metadata() {
        let empty = ImageMetadata::new();
        assert_eq!(empty.dimensions, None);
        assert_eq!(empty.blurhash, None);

        let with_dims = ImageMetadata {
            dimensions: Some((1920, 1080)),
            blurhash: None,
        };
        assert_eq!(with_dims.dimensions, Some((1920, 1080)));
        assert_eq!(with_dims.blurhash, None);
    }
}
