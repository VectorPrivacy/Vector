//! Unified image encoding utilities to eliminate duplicate PNG/JPEG encoding patterns.
//!
//! This module consolidates the 16+ duplicate image encoding blocks found throughout
//! the codebase (primarily in message.rs) into reusable functions.

use image::{DynamicImage, RgbaImage, ExtendedColorType};
use image::codecs::png::{PngEncoder, CompressionType, FilterType};
use image::codecs::jpeg::JpegEncoder;
use image::ImageEncoder;
use std::io::Cursor;

/// Default JPEG quality for standard compression (0-100)
pub const JPEG_QUALITY_STANDARD: u8 = 85;
/// JPEG quality for higher compression (smaller files)
pub const JPEG_QUALITY_COMPRESSED: u8 = 70;

/// Result of image encoding with format metadata
pub struct EncodedImage {
    /// The encoded image bytes
    pub bytes: Vec<u8>,
    /// File extension (e.g., "png" or "jpg")
    pub extension: String,
}

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
    let mut png_data = Vec::new();
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
    let mut jpeg_data = Vec::new();
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
    let rgba = img.to_rgba8();
    let pixels = rgba.as_raw();
    let width = img.width();
    let height = img.height();

    // Check if image has alpha transparency
    let has_alpha = crate::util::has_alpha_transparency(pixels);

    if has_alpha {
        let bytes = encode_png(pixels, width, height)?;
        Ok(EncodedImage {
            bytes,
            extension: "png".to_string(),
        })
    } else {
        // Convert to RGB for JPEG
        let rgb = img.to_rgb8();
        let bytes = encode_jpeg(rgb.as_raw(), width, height, jpeg_quality)?;
        Ok(EncodedImage {
            bytes,
            extension: "jpg".to_string(),
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
    let resized = if img.width() > max_dimension || img.height() > max_dimension {
        img.resize(max_dimension, max_dimension, image::imageops::FilterType::Lanczos3)
    } else {
        img.clone()
    };

    encode_image_auto(&resized, jpeg_quality)
}

/// Encode RGBA image data from raw components, choosing format based on alpha.
///
/// This is a convenience function for the common pattern of having separate
/// width, height, and pixel data.
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

    if has_alpha {
        let bytes = encode_png(pixels, width, height)?;
        Ok(EncodedImage {
            bytes,
            extension: "png".to_string(),
        })
    } else {
        // Convert RGBA to RGB for JPEG
        let rgba_img = RgbaImage::from_raw(width, height, pixels.to_vec())
            .ok_or_else(|| "Failed to create RGBA image from pixels".to_string())?;
        let rgb_img = DynamicImage::ImageRgba8(rgba_img).to_rgb8();
        let bytes = encode_jpeg(rgb_img.as_raw(), width, height, jpeg_quality)?;
        Ok(EncodedImage {
            bytes,
            extension: "jpg".to_string(),
        })
    }
}
