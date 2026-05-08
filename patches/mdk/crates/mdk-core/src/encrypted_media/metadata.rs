//! Metadata extraction and processing for encrypted media
//!
//! This module handles extraction and processing of metadata from media files,
//! with a focus on privacy and security. It strips EXIF data from images
//! by default and includes blurhash generation for previews.

use crate::encrypted_media::types::{EncryptedMediaError, MediaMetadata, MediaProcessingOptions};

/// Extract and process metadata from media file, optionally sanitizing the file
///
/// The mime_type parameter should be the canonical (normalized) MIME type
/// to ensure consistency in cryptographic operations.
///
/// Returns a tuple of (processed_data, metadata) where processed_data is either:
/// - The original data if sanitize_exif is false
/// - The sanitized data with EXIF stripped if sanitize_exif is true and it's an image
/// - The original data if sanitization is not supported (e.g., animated GIF/WebP)
///
/// SECURITY NOTE: For images with sanitize_exif=true, this function sanitizes FIRST
/// before extracting metadata. This ensures that any malicious metadata or exploits
/// in the original image cannot affect the metadata extraction process.
///
/// NOTE: For animated formats (GIF/WebP), sanitization will be skipped to avoid
/// flattening animations. The original file will be used instead, with a warning logged.
pub fn extract_and_process_metadata(
    data: &[u8],
    mime_type: &str,
    options: &MediaProcessingOptions,
) -> Result<(Vec<u8>, MediaMetadata), EncryptedMediaError> {
    use crate::media_processing::metadata::{
        is_safe_raster_format, preflight_dimension_check, strip_exif_and_return_image,
    };

    let mut metadata = MediaMetadata {
        mime_type: mime_type.to_string(),
        dimensions: None,
        blurhash: None,
        original_size: data.len() as u64,
    };

    let processed_data: Vec<u8>;

    // Process image metadata if it's an image
    if mime_type.starts_with("image/") {
        // SECURITY: Sanitize first if requested, then extract metadata from clean image
        // This prevents malicious metadata from being processed during extraction
        if options.sanitize_exif {
            // Only attempt sanitization for known safe raster formats
            // This prevents DoS attacks from:
            // 1. SVG and other vector formats that can't be decoded by image crate
            // 2. Decompression bombs with huge dimensions
            // 3. Animated formats that would be flattened
            if is_safe_raster_format(mime_type) {
                // PREFLIGHT CHECK: Validate dimensions without full decode to prevent OOM
                // This lightweight check protects against decompression bombs before
                // we fully decode the image for sanitization
                match preflight_dimension_check(data, options) {
                    Ok(_) => {
                        // Dimensions are safe, proceed with sanitization
                        match strip_exif_and_return_image(data, mime_type) {
                            Ok((cleaned_data, decoded_img)) => {
                                // Extract metadata from decoded image
                                let image_metadata = crate::media_processing::metadata::extract_metadata_from_decoded_image(
                                    &decoded_img,
                                    options,
                                    options.generate_blurhash,
                                )?;

                                metadata.dimensions = image_metadata.dimensions;
                                metadata.blurhash = image_metadata.blurhash;
                                processed_data = cleaned_data;
                            }
                            Err(e) => {
                                // Sanitization failed (shouldn't happen for safe formats)
                                tracing::warn!(
                                    "Failed to sanitize {} despite preflight passing: {} - using original data",
                                    mime_type,
                                    e
                                );
                                // Extract metadata from encoded image
                                let image_metadata = crate::media_processing::metadata::extract_metadata_from_encoded_image(
                                    data,
                                    options,
                                    options.generate_blurhash,
                                )?;

                                metadata.dimensions = image_metadata.dimensions;
                                metadata.blurhash = image_metadata.blurhash;
                                processed_data = data.to_vec();
                            }
                        }
                    }
                    Err(e) => {
                        // Preflight check failed (image too large or invalid)
                        // Return error to reject the image rather than processing it
                        tracing::warn!(
                            "Preflight dimension check failed for {}: {} - rejecting image",
                            mime_type,
                            e
                        );
                        return Err(e.into());
                    }
                }
            } else {
                // Not a safe raster format (e.g., SVG, unknown format, animated format)
                // Pass through original data without sanitization
                tracing::info!(
                    "Skipping EXIF sanitization for {} - not a safe raster format, using original data",
                    mime_type
                );
                // Extract metadata from encoded image
                let image_metadata =
                    crate::media_processing::metadata::extract_metadata_from_encoded_image(
                        data,
                        options,
                        options.generate_blurhash,
                    )?;

                metadata.dimensions = image_metadata.dimensions;
                metadata.blurhash = image_metadata.blurhash;
                processed_data = data.to_vec();
            }
        } else {
            // If not sanitizing, process original data
            let image_metadata =
                crate::media_processing::metadata::extract_metadata_from_encoded_image(
                    data,
                    options,
                    options.generate_blurhash,
                )?;

            metadata.dimensions = image_metadata.dimensions;
            metadata.blurhash = image_metadata.blurhash;
            processed_data = data.to_vec();
        }
    } else {
        // For non-images, just use the original data
        // TODO: add support for sanitizing other media types
        processed_data = data.to_vec();
    }

    Ok((processed_data, metadata))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media_processing::metadata::{is_safe_raster_format, preflight_dimension_check};

    #[test]
    fn test_animated_format_fallback() {
        use crate::encrypted_media::types::MediaProcessingOptions;

        // Create a minimal valid GIF (not actually animated, but format is GIF)
        let gif_data = vec![
            0x47, 0x49, 0x46, 0x38, 0x39, 0x61, // GIF89a header
            0x01, 0x00, 0x01, 0x00, // Width: 1, Height: 1
            0x80, 0x00, 0x00, // Global color table
            0x00, 0x00, 0x00, 0xFF, 0xFF, 0xFF, // Black and white
            0x2C, 0x00, 0x00, 0x00, 0x00, // Image descriptor
            0x01, 0x00, 0x01, 0x00, 0x00, // Image dimensions
            0x02, 0x02, 0x44, 0x01, 0x00, // Image data
            0x3B, // Trailer
        ];

        let options = MediaProcessingOptions {
            generate_blurhash: false,
            sanitize_exif: true, // Request sanitization
            max_dimension: Some(100),
            max_file_size: None,
            max_filename_length: None,
        };

        // Test that GIF with sanitize_exif=true falls back to original data
        let result = extract_and_process_metadata(&gif_data, "image/gif", &options);
        assert!(
            result.is_ok(),
            "GIF processing should succeed with fallback"
        );

        let (processed_data, metadata) = result.unwrap();
        // Should return original data since sanitization isn't supported
        assert_eq!(processed_data, gif_data, "Should return original GIF data");
        assert_eq!(metadata.mime_type, "image/gif");
        assert_eq!(metadata.original_size, gif_data.len() as u64);

        // Test WebP fallback behavior
        let result = extract_and_process_metadata(&gif_data, "image/webp", &options);
        assert!(
            result.is_ok(),
            "WebP processing should succeed with fallback"
        );

        let (processed_data, _) = result.unwrap();
        // Should return original data since sanitization isn't supported
        assert_eq!(processed_data, gif_data, "Should return original WebP data");
    }

    #[test]
    fn test_animated_format_without_sanitize() {
        use crate::encrypted_media::types::MediaProcessingOptions;

        // Create a minimal valid GIF
        let gif_data = vec![
            0x47, 0x49, 0x46, 0x38, 0x39, 0x61, // GIF89a header
            0x01, 0x00, 0x01, 0x00, // Width: 1, Height: 1
            0x80, 0x00, 0x00, // Global color table
            0x00, 0x00, 0x00, 0xFF, 0xFF, 0xFF, // Black and white
            0x2C, 0x00, 0x00, 0x00, 0x00, // Image descriptor
            0x01, 0x00, 0x01, 0x00, 0x00, // Image dimensions
            0x02, 0x02, 0x44, 0x01, 0x00, // Image data
            0x3B, // Trailer
        ];

        let options = MediaProcessingOptions {
            generate_blurhash: false,
            sanitize_exif: false, // Don't sanitize
            max_dimension: Some(100),
            max_file_size: None,
            max_filename_length: None,
        };

        // Test that GIF without sanitization works normally
        let result = extract_and_process_metadata(&gif_data, "image/gif", &options);
        assert!(
            result.is_ok(),
            "GIF processing without sanitization should succeed"
        );

        let (processed_data, metadata) = result.unwrap();
        assert_eq!(processed_data, gif_data, "Should return original GIF data");
        assert_eq!(metadata.mime_type, "image/gif");
    }

    #[test]
    fn test_safe_raster_format_detection() {
        // Safe formats that support sanitization
        assert!(is_safe_raster_format("image/jpeg"));
        assert!(is_safe_raster_format("image/png"));

        // Unsafe formats (animated or vector)
        assert!(!is_safe_raster_format("image/gif"));
        assert!(!is_safe_raster_format("image/webp"));
        assert!(!is_safe_raster_format("image/svg+xml"));
        assert!(!is_safe_raster_format("image/bmp"));
        assert!(!is_safe_raster_format("image/tiff"));
    }

    #[test]
    fn test_svg_passthrough_with_sanitize_requested() {
        use crate::encrypted_media::types::MediaProcessingOptions;

        // Minimal SVG data
        let svg_data =
            b"<svg xmlns=\"http://www.w3.org/2000/svg\"><rect width=\"10\" height=\"10\"/></svg>";

        let options = MediaProcessingOptions {
            generate_blurhash: false,
            sanitize_exif: true, // Request sanitization (should be skipped for SVG)
            max_dimension: Some(100),
            max_file_size: None,
            max_filename_length: None,
        };

        // SVG should pass through as-is since it's not a safe raster format
        let result = extract_and_process_metadata(svg_data, "image/svg+xml", &options);

        // Note: This may fail metadata extraction since SVG isn't a raster format
        // The important thing is that it doesn't try to decode/sanitize
        match result {
            Ok((processed_data, _)) => {
                // Should return original data without modification
                assert_eq!(
                    processed_data, svg_data,
                    "SVG should pass through unchanged"
                );
            }
            Err(_) => {
                // It's OK if metadata extraction fails for SVG, as long as it doesn't panic
                // or try to fully decode it
            }
        }
    }

    #[test]
    fn test_preflight_rejects_oversized_image() {
        use crate::encrypted_media::types::MediaProcessingOptions;

        // Create a valid PNG header with huge dimensions
        // This simulates a decompression bomb
        let huge_png_header = vec![
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, // PNG signature
            0x00, 0x00, 0x00, 0x0D, // IHDR chunk length
            0x49, 0x48, 0x44, 0x52, // IHDR
            0x00, 0x01, 0x00, 0x00, // Width: 65536 (too large)
            0x00, 0x01, 0x00, 0x00, // Height: 65536 (too large)
            0x08, 0x02, 0x00, 0x00, 0x00, // Bit depth, color type, etc
            0x00, 0x00, 0x00, 0x00, // Placeholder CRC
        ];

        let options = MediaProcessingOptions {
            generate_blurhash: false,
            sanitize_exif: true,
            max_dimension: Some(16384), // Standard max dimension
            max_file_size: None,
            max_filename_length: None,
        };

        // Should reject during preflight check, not during decode
        let result = preflight_dimension_check(&huge_png_header, &options);
        assert!(
            result.is_err(),
            "Preflight should reject oversized image dimensions"
        );

        // Now test via full extract_and_process_metadata flow
        let result = extract_and_process_metadata(&huge_png_header, "image/png", &options);
        assert!(
            result.is_err(),
            "Should reject oversized PNG during preflight, before attempting decode"
        );
    }
}
