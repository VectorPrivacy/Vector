//! Image metadata extraction and EXIF sanitization
//!
//! This module handles extraction of metadata from image files, including
//! dimensions and blurhash generation for previews. It also provides EXIF
//! sanitization for privacy protection.

use std::io::Cursor;

use blurhash::encode;
use exif::{In, Reader, Tag};
use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngEncoder;
use image::{ImageEncoder, ImageReader};

use crate::media_processing::types::{ImageMetadata, MediaProcessingError, MediaProcessingOptions};
use crate::media_processing::validation::validate_image_dimensions;

/// Extract metadata from an encoded image (decodes the image data first)
///
/// This function decodes the image and extracts dimensions and optionally generates
/// a blurhash for preview purposes.
///
/// # Arguments
/// * `data` - The encoded image data
/// * `options` - Validation options for dimension checking
/// * `generate_blurhash` - Whether to generate a blurhash (requires full decode)
///
/// # Returns
/// * `ImageMetadata` with dimensions and optional blurhash
///
/// # Errors
/// * `MetadataExtractionFailed` - If the image cannot be decoded
/// * `ImageDimensionsTooLarge` / `ImageTooManyPixels` / `ImageMemoryTooLarge` - If dimensions exceed limits
pub(crate) fn extract_metadata_from_encoded_image(
    data: &[u8],
    options: &MediaProcessingOptions,
    generate_blurhash_flag: bool,
) -> Result<ImageMetadata, MediaProcessingError> {
    // First, get dimensions without full decode for performance
    let img_reader = ImageReader::new(Cursor::new(data))
        .with_guessed_format()
        .map_err(|e| MediaProcessingError::MetadataExtractionFailed {
            reason: format!("Failed to read image: {}", e),
        })?;

    let (width, height) = img_reader.into_dimensions().map_err(|e| {
        MediaProcessingError::MetadataExtractionFailed {
            reason: format!("Failed to get image dimensions: {}", e),
        }
    })?;

    // Validate dimensions early - fail fast if image is too large
    validate_image_dimensions(width, height, options)?;

    let mut metadata = ImageMetadata {
        dimensions: Some((width, height)),
        blurhash: None,
    };

    // Only decode the full image if we need to generate blurhash
    if generate_blurhash_flag {
        let img_reader = ImageReader::new(Cursor::new(data))
            .with_guessed_format()
            .map_err(|e| MediaProcessingError::MetadataExtractionFailed {
                reason: format!("Failed to read image for blurhash: {}", e),
            })?;

        let img =
            img_reader
                .decode()
                .map_err(|e| MediaProcessingError::MetadataExtractionFailed {
                    reason: format!("Failed to decode image for blurhash: {}", e),
                })?;

        metadata.blurhash = generate_blurhash(&img);
    }

    Ok(metadata)
}

/// Extract metadata from an already-decoded image
///
/// This function extracts dimensions and blurhash from a decoded DynamicImage,
/// avoiding the need to decode the image again.
///
/// # Arguments
/// * `img` - The decoded image
/// * `options` - Validation options for dimension checking
/// * `generate_blurhash_flag` - Whether to generate a blurhash
///
/// # Returns
/// * `ImageMetadata` with dimensions and optional blurhash
///
/// # Errors
/// * `ImageDimensionsTooLarge` / `ImageTooManyPixels` / `ImageMemoryTooLarge` - If dimensions exceed limits
pub(crate) fn extract_metadata_from_decoded_image(
    img: &image::DynamicImage,
    options: &MediaProcessingOptions,
    generate_blurhash_flag: bool,
) -> Result<ImageMetadata, MediaProcessingError> {
    let width = img.width();
    let height = img.height();

    // Validate dimensions
    validate_image_dimensions(width, height, options)?;

    let mut metadata = ImageMetadata {
        dimensions: Some((width, height)),
        blurhash: None,
    };

    // Generate blurhash if requested
    if generate_blurhash_flag {
        metadata.blurhash = generate_blurhash(img);
    }

    Ok(metadata)
}

/// Generate blurhash for an image
///
/// Creates a compact string representation of the image that can be used
/// to generate a blurred placeholder while the full image loads.
///
/// # Arguments
/// * `img` - The decoded image
///
/// # Returns
/// * `Some(String)` with the blurhash, or `None` if generation fails
pub(crate) fn generate_blurhash(img: &image::DynamicImage) -> Option<String> {
    // Resize image for blurhash (max 32x32 for performance)
    let small_img = img.resize(32, 32, image::imageops::FilterType::Lanczos3);
    // Convert to RGBA8 because blurhash expects 4 bytes per pixel (RGBA format)
    let rgba_img = small_img.to_rgba8();

    encode(4, 3, rgba_img.width(), rgba_img.height(), rgba_img.as_raw()).ok()
}

/// Check if a MIME type is a known safe raster format that supports EXIF sanitization
///
/// This function returns true only for raster image formats that:
/// 1. Can be safely decoded by the `image` crate
/// 2. Can be re-encoded without loss of format features (e.g., not animated)
/// 3. Are commonly used formats where EXIF stripping is valuable
///
/// Formats which are excluded:
/// - image/gif: May be animated, would be flattened
/// - image/webp: May be animated, would be flattened
/// - image/svg+xml: Vector format, cannot be decoded as raster
/// - Other vector or specialized formats
pub(crate) fn is_safe_raster_format(mime_type: &str) -> bool {
    matches!(mime_type, "image/jpeg" | "image/png")
}

/// Perform a lightweight preflight check on image dimensions without full decode
///
/// This function reads only the image header to get dimensions and validates them
/// against size limits. This protects against decompression bombs by rejecting
/// oversized images before we attempt to fully decode them for sanitization.
///
/// This is much faster and safer than decoding the entire image, as it only
/// reads the image header (typically the first few KB of data).
pub(crate) fn preflight_dimension_check(
    data: &[u8],
    options: &MediaProcessingOptions,
) -> Result<(), MediaProcessingError> {
    // Read just the image header to get dimensions
    let img_reader = ImageReader::new(Cursor::new(data))
        .with_guessed_format()
        .map_err(|e| MediaProcessingError::MetadataExtractionFailed {
            reason: format!("Failed to read image header during preflight: {}", e),
        })?;

    // Get dimensions without decoding the full image
    // This is very fast and doesn't allocate memory for pixel data
    let (width, height) = img_reader.into_dimensions().map_err(|e| {
        MediaProcessingError::MetadataExtractionFailed {
            reason: format!("Failed to get image dimensions during preflight: {}", e),
        }
    })?;

    // Validate dimensions to catch decompression bombs early
    validate_image_dimensions(width, height, options)?;

    Ok(())
}

/// Strip ALL EXIF data from an image and return both the encoded data and decoded image
///
/// This function re-encodes the image to remove all EXIF metadata for privacy.
/// It returns both the cleaned encoded bytes and the decoded image object to avoid
/// needing to decode again for metadata extraction.
///
/// IMPORTANT: This function should only be called for safe raster formats (JPEG, PNG).
/// The caller is responsible for checking format compatibility via `is_safe_raster_format()`.
///
/// Returns: (cleaned_data, decoded_image)
pub(crate) fn strip_exif_and_return_image(
    data: &[u8],
    mime_type: &str,
) -> Result<(Vec<u8>, image::DynamicImage), MediaProcessingError> {
    // Decode the image once
    let img_reader = ImageReader::new(Cursor::new(data))
        .with_guessed_format()
        .map_err(|e| MediaProcessingError::MetadataExtractionFailed {
            reason: format!("Failed to read image for EXIF stripping: {}", e),
        })?;

    let mut img =
        img_reader
            .decode()
            .map_err(|e| MediaProcessingError::MetadataExtractionFailed {
                reason: format!("Failed to decode image for EXIF stripping: {}", e),
            })?;

    // Apply EXIF orientation transform before re-encoding
    // This "bakes in" the correct orientation so the image displays correctly
    // even without EXIF metadata
    img = apply_exif_orientation(data, img)?;

    // Re-encode the image without metadata
    let mut output = Cursor::new(Vec::new());

    match mime_type {
        "image/jpeg" => {
            // Use high quality (100) to minimize quality loss during re-encoding
            // This is important for preserving image fidelity while still stripping metadata
            let mut encoder = JpegEncoder::new_with_quality(&mut output, 100);
            encoder
                .encode(
                    img.as_bytes(),
                    img.width(),
                    img.height(),
                    img.color().into(),
                )
                .map_err(|e| MediaProcessingError::MetadataExtractionFailed {
                    reason: format!("Failed to re-encode JPEG: {}", e),
                })?;
        }
        "image/png" => {
            let encoder = PngEncoder::new(&mut output);
            encoder
                .write_image(
                    img.as_bytes(),
                    img.width(),
                    img.height(),
                    img.color().into(),
                )
                .map_err(|e| MediaProcessingError::MetadataExtractionFailed {
                    reason: format!("Failed to re-encode PNG: {}", e),
                })?;
        }
        _ => {
            // For unknown formats, return error
            return Err(MediaProcessingError::MetadataExtractionFailed {
                reason: format!("Unsupported image format for EXIF stripping: {}", mime_type),
            });
        }
    }

    Ok((output.into_inner(), img))
}

/// Apply EXIF orientation transform to an image
///
/// Reads the EXIF orientation tag from the original image data and applies
/// the appropriate rotation and/or flip operations to the decoded image.
/// This ensures images display correctly even after EXIF metadata is stripped.
///
/// EXIF Orientation values:
/// 1 = Normal
/// 2 = Flip horizontal
/// 3 = Rotate 180°
/// 4 = Flip vertical
/// 5 = Flip horizontal + Rotate 270° CW
/// 6 = Rotate 90° CW
/// 7 = Flip horizontal + Rotate 90° CW
/// 8 = Rotate 270° CW
fn apply_exif_orientation(
    data: &[u8],
    img: image::DynamicImage,
) -> Result<image::DynamicImage, MediaProcessingError> {
    // Try to read EXIF data - if it fails or doesn't exist, just return the original image
    let exif_reader = match Reader::new().read_from_container(&mut Cursor::new(data)) {
        Ok(exif) => exif,
        Err(_) => return Ok(img), // No EXIF data or couldn't read it - return as-is
    };

    // Get the orientation tag
    let orientation = match exif_reader.get_field(Tag::Orientation, In::PRIMARY) {
        Some(field) => match field.value.get_uint(0) {
            Some(val) => val,
            None => return Ok(img), // Couldn't parse orientation - return as-is
        },
        None => return Ok(img), // No orientation tag - return as-is
    };

    // Apply the appropriate transform based on orientation value
    let transformed = match orientation {
        1 => img,                     // Normal - no transformation needed
        2 => img.fliph(),             // Flip horizontal
        3 => img.rotate180(),         // Rotate 180°
        4 => img.flipv(),             // Flip vertical
        5 => img.rotate270().fliph(), // Flip horizontal + Rotate 270° CW
        6 => img.rotate90(),          // Rotate 90° CW
        7 => img.rotate90().fliph(),  // Flip horizontal + Rotate 90° CW
        8 => img.rotate270(),         // Rotate 270° CW (or 90° CCW)
        _ => img,                     // Unknown orientation value - return as-is
    };

    Ok(transformed)
}

#[cfg(test)]
mod tests {
    use image::{DynamicImage, ImageBuffer, Rgb, RgbImage};

    use super::*;

    /// Create a test PNG image with specified dimensions
    fn create_test_png(width: u32, height: u32) -> Vec<u8> {
        let img = ImageBuffer::from_fn(width, height, |_, _| Rgb([255u8, 0u8, 0u8]));
        let mut png_data = Vec::new();
        img.write_to(
            &mut std::io::Cursor::new(&mut png_data),
            image::ImageFormat::Png,
        )
        .unwrap();
        png_data
    }

    /// Create a test JPEG image with specified dimensions
    fn create_test_jpeg(width: u32, height: u32) -> Vec<u8> {
        let img = ImageBuffer::from_fn(width, height, |_, _| Rgb([255u8, 0u8, 0u8]));
        let mut jpeg_data = Vec::new();
        img.write_to(
            &mut std::io::Cursor::new(&mut jpeg_data),
            image::ImageFormat::Jpeg,
        )
        .unwrap();
        jpeg_data
    }

    #[test]
    fn test_extract_metadata_from_encoded_image() {
        // Create a valid 10x10 red PNG image (1x1 causes issues with blurhash library)
        let img = ImageBuffer::from_fn(10, 10, |_, _| Rgb([255u8, 0u8, 0u8]));
        let mut png_data = Vec::new();
        img.write_to(
            &mut std::io::Cursor::new(&mut png_data),
            image::ImageFormat::Png,
        )
        .unwrap();

        let options = MediaProcessingOptions::validation_only();

        // Test without blurhash
        let result = extract_metadata_from_encoded_image(&png_data, &options, false);
        assert!(result.is_ok(), "Failed to extract metadata: {:?}", result);
        let metadata = result.unwrap();
        assert_eq!(metadata.dimensions, Some((10, 10)));
        assert!(metadata.blurhash.is_none());

        // Note: Skipping blurhash test due to known issues with the blurhash library
        // The blurhash functionality is tested in the encrypted_media module tests
    }

    #[test]
    fn test_extract_metadata_dimension_validation() {
        let png_data = vec![
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, // PNG signature
            0x00, 0x00, 0x00, 0x0D, // IHDR chunk length
            0x49, 0x48, 0x44, 0x52, // IHDR
            0x00, 0x00, 0x00, 0x01, // Width: 1
            0x00, 0x00, 0x00, 0x01, // Height: 1
            0x08, 0x02, 0x00, 0x00, 0x00, // Bit depth, color type, etc
            0x90, 0x77, 0x53, 0xDE, // CRC
            0x00, 0x00, 0x00, 0x0C, // IDAT chunk length
            0x49, 0x44, 0x41, 0x54, // IDAT
            0x08, 0x99, 0x01, 0x01, 0x00, 0x00, 0x00, 0xFF, 0xFF, 0x00, 0x00, 0x00, 0x02, 0x00,
            0x01, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
        ];

        // Test with dimension validation failure
        let strict_options = MediaProcessingOptions {
            sanitize_exif: false,
            generate_blurhash: false,
            max_dimension: Some(0), // This should cause validation to fail
            ..Default::default()
        };

        let result = extract_metadata_from_encoded_image(&png_data, &strict_options, false);
        assert!(result.is_err());
        if let Err(MediaProcessingError::ImageDimensionsTooLarge {
            width,
            height,
            max_dimension,
        }) = result
        {
            assert_eq!(width, 1);
            assert_eq!(height, 1);
            assert_eq!(max_dimension, 0);
        } else {
            panic!("Expected DimensionsTooLarge error");
        }
    }

    #[test]
    fn test_safe_raster_format_detection() {
        // Safe formats
        assert!(is_safe_raster_format("image/jpeg"));
        assert!(is_safe_raster_format("image/png"));

        // Unsafe/unsupported formats
        assert!(!is_safe_raster_format("image/gif"));
        assert!(!is_safe_raster_format("image/webp"));
        assert!(!is_safe_raster_format("image/svg+xml"));
        assert!(!is_safe_raster_format("image/bmp"));
        assert!(!is_safe_raster_format("application/pdf"));
        assert!(!is_safe_raster_format(""));
    }

    #[test]
    fn test_preflight_rejects_oversized_image() {
        let png_data = create_test_png(100, 100);

        let strict_options = MediaProcessingOptions {
            max_dimension: Some(50), // Smaller than the image
            ..Default::default()
        };

        let result = preflight_dimension_check(&png_data, &strict_options);
        assert!(result.is_err());
        assert!(matches!(
            result,
            Err(MediaProcessingError::ImageDimensionsTooLarge { .. })
        ));
    }

    #[test]
    fn test_preflight_accepts_valid_image() {
        let png_data = create_test_png(50, 50);

        let options = MediaProcessingOptions::default();
        let result = preflight_dimension_check(&png_data, &options);
        assert!(result.is_ok());
    }

    #[test]
    fn test_preflight_invalid_data() {
        let invalid_data = vec![0x00, 0x01, 0x02, 0x03];
        let options = MediaProcessingOptions::default();

        let result = preflight_dimension_check(&invalid_data, &options);
        assert!(result.is_err());
        assert!(matches!(
            result,
            Err(MediaProcessingError::MetadataExtractionFailed { .. })
        ));
    }

    #[test]
    fn test_extract_metadata_from_decoded_image() {
        // Create a decoded image directly
        let img: RgbImage = ImageBuffer::from_fn(100, 50, |_, _| Rgb([255u8, 0u8, 0u8]));
        let dynamic_img = DynamicImage::ImageRgb8(img);

        let options = MediaProcessingOptions::default();

        // Test without blurhash
        let result = extract_metadata_from_decoded_image(&dynamic_img, &options, false);
        assert!(result.is_ok());
        let metadata = result.unwrap();
        assert_eq!(metadata.dimensions, Some((100, 50)));
        assert!(metadata.blurhash.is_none());

        // Test with blurhash
        let result = extract_metadata_from_decoded_image(&dynamic_img, &options, true);
        assert!(result.is_ok());
        let metadata = result.unwrap();
        assert_eq!(metadata.dimensions, Some((100, 50)));
        assert!(metadata.blurhash.is_some());
    }

    #[test]
    fn test_extract_metadata_from_decoded_image_dimension_validation() {
        let img: RgbImage = ImageBuffer::from_fn(100, 100, |_, _| Rgb([255u8, 0u8, 0u8]));
        let dynamic_img = DynamicImage::ImageRgb8(img);

        let strict_options = MediaProcessingOptions {
            max_dimension: Some(50),
            ..Default::default()
        };

        let result = extract_metadata_from_decoded_image(&dynamic_img, &strict_options, false);
        assert!(result.is_err());
        assert!(matches!(
            result,
            Err(MediaProcessingError::ImageDimensionsTooLarge { .. })
        ));
    }

    #[test]
    fn test_generate_blurhash_produces_valid_hash() {
        let img: RgbImage = ImageBuffer::from_fn(32, 32, |x, y| {
            Rgb([((x * 8) % 256) as u8, ((y * 8) % 256) as u8, 128u8])
        });
        let dynamic_img = DynamicImage::ImageRgb8(img);

        let result = generate_blurhash(&dynamic_img);
        assert!(result.is_some());

        let hash = result.unwrap();
        // Blurhash should be a non-empty string
        assert!(!hash.is_empty());
        // Blurhash typically starts with a component count indicator
        assert!(hash.len() > 4);
    }

    #[test]
    fn test_strip_exif_jpeg() {
        let jpeg_data = create_test_jpeg(50, 50);

        let result = strip_exif_and_return_image(&jpeg_data, "image/jpeg");
        assert!(result.is_ok());

        let (cleaned_data, img) = result.unwrap();
        assert!(!cleaned_data.is_empty());
        assert_eq!(img.width(), 50);
        assert_eq!(img.height(), 50);
    }

    #[test]
    fn test_strip_exif_png() {
        let png_data = create_test_png(50, 50);

        let result = strip_exif_and_return_image(&png_data, "image/png");
        assert!(result.is_ok());

        let (cleaned_data, img) = result.unwrap();
        assert!(!cleaned_data.is_empty());
        assert_eq!(img.width(), 50);
        assert_eq!(img.height(), 50);
    }

    #[test]
    fn test_strip_exif_unsupported_format() {
        let png_data = create_test_png(50, 50);

        // Try to strip with an unsupported mime type
        let result = strip_exif_and_return_image(&png_data, "image/webp");
        assert!(result.is_err());
        assert!(matches!(
            result,
            Err(MediaProcessingError::MetadataExtractionFailed { .. })
        ));
    }

    #[test]
    fn test_strip_exif_invalid_data() {
        let invalid_data = vec![0x00, 0x01, 0x02, 0x03];

        let result = strip_exif_and_return_image(&invalid_data, "image/jpeg");
        assert!(result.is_err());
    }

    #[test]
    fn test_animated_format_fallback() {
        // GIF and WebP are not safe raster formats because they might be animated
        assert!(!is_safe_raster_format("image/gif"));
        assert!(!is_safe_raster_format("image/webp"));
    }

    #[test]
    fn test_animated_format_without_sanitize() {
        // Even with sanitize_exif = false, animated formats should be passthrough
        // This test just verifies that we correctly identify safe formats
        let options = MediaProcessingOptions {
            sanitize_exif: false,
            ..Default::default()
        };

        // PNG is safe
        assert!(is_safe_raster_format("image/png"));
        // JPEG is safe
        assert!(is_safe_raster_format("image/jpeg"));

        // Verify options don't affect format detection
        assert!(!options.sanitize_exif);
    }

    #[test]
    fn test_svg_passthrough_with_sanitize_requested() {
        // SVG is a vector format, not a safe raster format
        assert!(!is_safe_raster_format("image/svg+xml"));
    }

    #[test]
    fn test_extract_metadata_with_blurhash_generation() {
        let png_data = create_test_png(32, 32);
        let options = MediaProcessingOptions::default();

        let result = extract_metadata_from_encoded_image(&png_data, &options, true);
        assert!(result.is_ok());

        let metadata = result.unwrap();
        assert_eq!(metadata.dimensions, Some((32, 32)));
        assert!(metadata.blurhash.is_some());
    }
}
