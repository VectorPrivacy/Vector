//! Unified image encoding utilities to eliminate duplicate PNG/JPEG encoding patterns.
//!
//! This module consolidates the 16+ duplicate image encoding blocks found throughout
//! the codebase (primarily in message.rs) into reusable functions.

use image::{DynamicImage, ExtendedColorType};
use image::codecs::png::{PngEncoder, CompressionType, FilterType};
use image::codecs::jpeg::JpegEncoder;
use image::ImageEncoder;
use std::io::Cursor;

/// Maximum dimension for image compression (1920px on longest side)
pub const MAX_DIMENSION: u32 = 1920;

/// Default JPEG quality for standard compression (0-100)
pub const JPEG_QUALITY_STANDARD: u8 = 85;
/// JPEG quality for higher compression (smaller files)
pub const JPEG_QUALITY_COMPRESSED: u8 = 70;
/// JPEG quality for UI previews (fast encoding, small size)
pub const JPEG_QUALITY_PREVIEW: u8 = 50;

/// Result of image encoding with format metadata
pub struct EncodedImage {
    /// The encoded image bytes
    pub bytes: Vec<u8>,
    /// File extension (e.g., "png" or "jpg")
    pub extension: &'static str,
}

impl EncodedImage {
    /// Convert to a base64 data URI (e.g., "data:image/png;base64,...")
    ///
    /// Pre-allocates exact capacity and encodes directly into the result string,
    /// avoiding an intermediate base64 string allocation.
    #[inline]
    pub fn to_data_uri(&self) -> String {
        use base64::Engine;

        let prefix = if self.extension == "png" {
            "data:image/png;base64,"
        } else {
            "data:image/jpeg;base64,"
        };

        // Base64 output is 4/3 input size, rounded up to nearest 4 (padding)
        let base64_len = (self.bytes.len() + 2) / 3 * 4;
        let mut result = String::with_capacity(prefix.len() + base64_len);

        result.push_str(prefix);
        base64::engine::general_purpose::STANDARD.encode_string(&self.bytes, &mut result);

        result
    }
}

/// Minimum dimension threshold for JPEG encoding.
/// Images smaller than this (in both width AND height) use PNG to avoid artifacts.
/// This preserves quality for pixel art and small icons.
pub const SMALL_IMAGE_THRESHOLD: u32 = 200;

/// Encode RGBA pixel data as PNG with best compression.
///
/// Uses adaptive filtering and best compression for smallest file sizes
/// while preserving alpha transparency.
///
/// # Arguments
/// * `pixels` - RGBA pixel data (4 bytes per pixel)
/// * `width` - Image width in pixels
/// * `height` - Image height in pixels
///
/// # Returns
/// Encoded PNG bytes or an error string
pub fn encode_png(pixels: &[u8], width: u32, height: u32) -> Result<Vec<u8>, String> {
    // Pre-allocate: PNG with best compression is typically 20-40% of raw RGBA size
    let estimated_size = pixels.len() / 3;
    let mut png_data = Vec::with_capacity(estimated_size);
    let encoder = PngEncoder::new_with_quality(
        &mut png_data,
        CompressionType::Best,
        FilterType::Adaptive,
    );
    encoder.write_image(
        pixels,
        width,
        height,
        ExtendedColorType::Rgba8
    ).map_err(|e| format!("Failed to encode PNG: {}", e))?;
    Ok(png_data)
}

/// Convert RGBA pixel data to RGB by dropping the alpha channel.
///
/// This is more efficient than going through DynamicImage when you already
/// have raw RGBA bytes - avoids an extra buffer clone.
#[inline]
fn rgba_to_rgb(rgba: &[u8]) -> Vec<u8> {
    let pixel_count = rgba.len() / 4;
    let mut rgb = Vec::with_capacity(pixel_count * 3);
    for chunk in rgba.chunks_exact(4) {
        rgb.extend_from_slice(&chunk[..3]);
    }
    rgb
}

/// Encode RGB pixel data as JPEG with specified quality.
///
/// # Arguments
/// * `pixels` - RGB pixel data (3 bytes per pixel)
/// * `width` - Image width in pixels
/// * `height` - Image height in pixels
/// * `quality` - JPEG quality (0-100), use JPEG_QUALITY_* constants
///
/// # Returns
/// Encoded JPEG bytes or an error string
pub fn encode_jpeg(pixels: &[u8], width: u32, height: u32, quality: u8) -> Result<Vec<u8>, String> {
    // Pre-allocate: JPEG is typically 5-15% of raw RGB size depending on quality
    // Use ~10% as a reasonable estimate
    let estimated_size = pixels.len() / 10;
    let mut jpeg_data = Vec::with_capacity(estimated_size.max(1024));
    let mut cursor = Cursor::new(&mut jpeg_data);
    let encoder = JpegEncoder::new_with_quality(&mut cursor, quality);
    encoder.write_image(
        pixels,
        width,
        height,
        ExtendedColorType::Rgb8
    ).map_err(|e| format!("Failed to encode JPEG: {}", e))?;
    Ok(jpeg_data)
}

/// Encode a DynamicImage choosing PNG or JPEG based on alpha transparency.
///
/// If the image has transparency (alpha < 255), PNG is used to preserve it.
/// Otherwise, JPEG is used for better compression.
///
/// # Arguments
/// * `img` - The image to encode
/// * `jpeg_quality` - Quality for JPEG encoding if used (0-100)
///
/// # Returns
/// EncodedImage with bytes and format extension, or an error string
pub fn encode_image_auto(img: &DynamicImage, jpeg_quality: u8) -> Result<EncodedImage, String> {
    let width = img.width();
    let height = img.height();

    // Fast path: if source format has no alpha channel, go straight to JPEG
    // This avoids allocating an RGBA buffer just to check for alpha
    match img {
        DynamicImage::ImageRgb8(_) |
        DynamicImage::ImageRgb16(_) |
        DynamicImage::ImageRgb32F(_) |
        DynamicImage::ImageLuma8(_) |
        DynamicImage::ImageLuma16(_) => {
            // No alpha channel possible - encode as JPEG directly
            let rgb = img.to_rgb8();
            let bytes = encode_jpeg(rgb.as_raw(), width, height, jpeg_quality)?;
            return Ok(EncodedImage {
                bytes,
                extension: "jpg",
            });
        }
        _ => {}
    }

    // Source has alpha channel - need to check if it's actually used
    let rgba = img.to_rgba8();
    let pixels = rgba.as_raw();

    if crate::util::has_alpha_transparency(pixels) {
        let bytes = encode_png(pixels, width, height)?;
        Ok(EncodedImage {
            bytes,
            extension: "png",
        })
    } else {
        // Has alpha channel but not used - convert to RGB for JPEG
        let rgb_data = rgba_to_rgb(pixels);
        let bytes = encode_jpeg(&rgb_data, width, height, jpeg_quality)?;
        Ok(EncodedImage {
            bytes,
            extension: "jpg",
        })
    }
}

/// Compress an image by resizing it to fit within max dimensions.
///
/// Maintains aspect ratio while ensuring neither dimension exceeds max_dimension.
/// Then encodes using PNG (with alpha) or JPEG (without alpha).
///
/// # Arguments
/// * `img` - The image to compress
/// * `max_dimension` - Maximum width or height in pixels
/// * `jpeg_quality` - Quality for JPEG encoding if used (0-100)
///
/// # Returns
/// EncodedImage with compressed bytes and format extension, or an error string
pub fn compress_image(img: &DynamicImage, max_dimension: u32, jpeg_quality: u8) -> Result<EncodedImage, String> {
    // Resize if needed, maintaining aspect ratio
    if img.width() > max_dimension || img.height() > max_dimension {
        let resized = img.resize(max_dimension, max_dimension, image::imageops::FilterType::Lanczos3);
        encode_image_auto(&resized, jpeg_quality)
    } else {
        // No resize needed - encode directly without cloning
        encode_image_auto(img, jpeg_quality)
    }
}

/// Encode RGBA image data from raw components, choosing format based on alpha and size.
///
/// Uses PNG for:
/// - Images with alpha transparency
/// - Small images (both dimensions < 200px) to preserve pixel art quality
///
/// Uses JPEG for everything else (better compression for photos).
///
/// # Arguments
/// * `pixels` - RGBA pixel data (4 bytes per pixel)
/// * `width` - Image width in pixels
/// * `height` - Image height in pixels
/// * `jpeg_quality` - Quality for JPEG encoding if used (0-100)
///
/// # Returns
/// EncodedImage with bytes and format extension, or an error string
pub fn encode_rgba_auto(pixels: &[u8], width: u32, height: u32, jpeg_quality: u8) -> Result<EncodedImage, String> {
    let has_alpha = crate::util::has_alpha_transparency(pixels);
    let is_small = width < SMALL_IMAGE_THRESHOLD && height < SMALL_IMAGE_THRESHOLD;

    // Use PNG for alpha transparency OR small images (preserves pixel art)
    if has_alpha || is_small {
        let bytes = encode_png(pixels, width, height)?;
        Ok(EncodedImage {
            bytes,
            extension: "png",
        })
    } else {
        // Convert RGBA to RGB inline (avoids full buffer clone)
        let rgb_data = rgba_to_rgb(pixels);
        let bytes = encode_jpeg(&rgb_data, width, height, jpeg_quality)?;
        Ok(EncodedImage {
            bytes,
            extension: "jpg",
        })
    }
}

/// Calculate target dimensions to fit within max_dimension while preserving aspect ratio.
///
/// Returns the original dimensions if both are already within the limit.
/// Otherwise, scales down proportionally so the longest side equals max_dimension.
///
/// # Arguments
/// * `width` - Original image width
/// * `height` - Original image height
/// * `max_dimension` - Maximum allowed size for either dimension
///
/// # Returns
/// Tuple of (new_width, new_height)
#[inline]
pub fn calculate_resize_dimensions(width: u32, height: u32, max_dimension: u32) -> (u32, u32) {
    if width <= max_dimension && height <= max_dimension {
        (width, height)
    } else if width > height {
        let ratio = max_dimension as f32 / width as f32;
        (max_dimension, (height as f32 * ratio) as u32)
    } else {
        let ratio = max_dimension as f32 / height as f32;
        ((width as f32 * ratio) as u32, max_dimension)
    }
}

/// Calculate preview dimensions based on a quality percentage.
///
/// # Arguments
/// * `width` - Original image width
/// * `height` - Original image height
/// * `quality` - Percentage (1-100) of original size
///
/// # Returns
/// Tuple of (new_width, new_height), both at least 1
#[inline]
pub fn calculate_preview_dimensions(width: u32, height: u32, quality: u32) -> (u32, u32) {
    let quality = quality.clamp(1, 100);
    (
        ((width * quality) / 100).max(1),
        ((height * quality) / 100).max(1),
    )
}

/// Read a file, checking if it's empty via metadata first to avoid reading 0 bytes.
///
/// This is more efficient than reading then checking length, especially for
/// large files that would waste I/O bandwidth on empty file detection.
///
/// # Arguments
/// * `path` - Path to the file
///
/// # Returns
/// File bytes or an error string
pub fn read_file_checked(path: &str) -> Result<Vec<u8>, String> {
    let metadata = std::fs::metadata(path)
        .map_err(|e| format!("Failed to read file metadata: {}", e))?;

    if metadata.len() == 0 {
        return Err(format!("File is empty (0 bytes): {}", path));
    }

    std::fs::read(path)
        .map_err(|e| format!("Failed to read file: {}", e))
}
