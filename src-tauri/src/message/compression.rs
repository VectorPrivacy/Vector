//! Image compression functions.
//!
//! This module handles:
//! - Image compression with resize to max 1920px
//! - GIF preservation (skip compression to keep animation)
//! - PNG for transparent images, JPEG for opaque
//! - ThumbHash generation for previews

use std::sync::Arc;

use super::types::{CachedCompressedImage, ImageMetadata};

#[cfg(target_os = "android")]
use super::types::ANDROID_FILE_CACHE;
#[cfg(target_os = "android")]
use crate::android::filesystem;

/// Minimum savings percentage required for compression to be worthwhile
pub const MIN_SAVINGS_PERCENT: u64 = 1;

/// Internal function to compress bytes
/// Takes Arc<Vec<u8>> for zero-copy sharing.
/// If `min_savings_percent` is Some and compression doesn't meet threshold,
/// returns original bytes with metadata (no wasted clone).
pub(super) fn compress_bytes_internal(
    bytes: Arc<Vec<u8>>,
    extension: &str,
    min_savings_percent: Option<u64>,
) -> Result<CachedCompressedImage, String> {
    let original_size = bytes.len() as u64;

    // For GIFs, skip compression to preserve animation
    if extension == "gif" {
        let img = ::image::load_from_memory(&bytes)
            .map_err(|e| format!("Failed to decode GIF: {}", e))?;

        let (width, height) = (img.width(), img.height());

        let img_meta = crate::util::generate_thumbhash_from_image(&img)
            .map(|thumbhash| ImageMetadata {
                thumbhash,
                width,
                height,
            });

        return Ok(CachedCompressedImage {
            bytes,
            extension: "gif".to_string(),
            img_meta,
            original_size,
            compressed_size: original_size,
        });
    }

    // Load and decode the image
    let img = ::image::load_from_memory(&bytes)
        .map_err(|e| format!("Failed to decode image: {}", e))?;

    // Determine target dimensions (max 1920px on longest side)
    use crate::shared::image::{calculate_resize_dimensions, MAX_DIMENSION};
    let (width, height) = (img.width(), img.height());
    let (new_width, new_height) = calculate_resize_dimensions(width, height, MAX_DIMENSION);

    // Resize if needed
    let resized_img = if new_width != width || new_height != height {
        img.resize(new_width, new_height, ::image::imageops::FilterType::Lanczos3)
    } else {
        img
    };

    let actual_width = resized_img.width();
    let actual_height = resized_img.height();

    // Generate metadata from final image only (avoid redundant thumbhash generation)
    let final_meta = crate::util::generate_thumbhash_from_image(&resized_img)
        .map(|thumbhash| ImageMetadata {
            thumbhash,
            width: actual_width,
            height: actual_height,
        });

    // Keep reference to original metadata for fallback path
    let img_meta = final_meta.clone();

    let rgba_img = resized_img.to_rgba8();

    // Encode as PNG (alpha/small) or JPEG (standard)
    use crate::shared::image::{encode_rgba_auto, JPEG_QUALITY_STANDARD};
    let encoded = encode_rgba_auto(rgba_img.as_raw(), actual_width, actual_height, JPEG_QUALITY_STANDARD)?;
    let compressed_bytes = encoded.bytes;
    let new_extension = encoded.extension;

    let compressed_size = compressed_bytes.len() as u64;

    // Check if compression meets minimum savings threshold
    if let Some(min_percent) = min_savings_percent {
        let savings_percent = if original_size > 0 && compressed_size < original_size {
            ((original_size - compressed_size) * 100) / original_size
        } else {
            0
        };

        if savings_percent < min_percent {
            // Compression not worth it - return original
            return Ok(CachedCompressedImage {
                bytes,
                extension: extension.to_string(),
                img_meta,
                original_size,
                compressed_size: original_size,
            });
        }
    }

    Ok(CachedCompressedImage {
        bytes: Arc::new(compressed_bytes),
        extension: new_extension.to_string(),
        img_meta: final_meta,
        original_size,
        compressed_size,
    })
}

/// Internal function to compress an image and return cached data
pub(super) fn compress_image_internal(file_path: &str) -> Result<CachedCompressedImage, String> {
    #[cfg(not(target_os = "android"))]
    {
        // Get extension early to check if it's a GIF
        let extension = file_path
            .rsplit('.')
            .next()
            .unwrap_or("")
            .to_lowercase();
        
        // Read file bytes
        let bytes = std::fs::read(file_path)
            .map_err(|e| format!("Failed to read file: {}", e))?;
        
        let original_size = bytes.len() as u64;
        
        // For GIFs, skip compression entirely to preserve animation
        // Just decode first frame for thumbhash, then return original bytes
        if extension == "gif" {
            // Decode just to get dimensions and generate thumbhash from first frame
            let img = ::image::load_from_memory(&bytes)
                .map_err(|e| format!("Failed to decode GIF: {}", e))?;

            let (width, height) = (img.width(), img.height());

            let img_meta = crate::util::generate_thumbhash_from_image(&img)
                .map(|thumbhash| ImageMetadata {
                    thumbhash,
                    width,
                    height,
                });

            // Return original bytes to preserve animation
            return Ok(CachedCompressedImage {
                bytes: Arc::new(bytes),
                extension: "gif".to_string(),
                img_meta,
                original_size,
                compressed_size: original_size, // Same size, no compression
            });
        }
        
        // Try to load and decode the image
        let img = ::image::load_from_memory(&bytes)
            .map_err(|e| format!("Failed to decode image: {}", e))?;

        // Determine target dimensions (max 1920px on longest side)
        use crate::shared::image::{calculate_resize_dimensions, MAX_DIMENSION, encode_rgba_auto, JPEG_QUALITY_STANDARD};
        let (width, height) = (img.width(), img.height());
        let (new_width, new_height) = calculate_resize_dimensions(width, height, MAX_DIMENSION);

        // Resize if needed
        let resized_img = if new_width != width || new_height != height {
            img.resize(new_width, new_height, ::image::imageops::FilterType::Lanczos3)
        } else {
            img
        };

        let actual_width = resized_img.width();
        let actual_height = resized_img.height();

        let img_meta = crate::util::generate_thumbhash_from_image(&resized_img)
            .map(|thumbhash| ImageMetadata {
                thumbhash,
                width: actual_width,
                height: actual_height,
            });

        let rgba_img = resized_img.to_rgba8();
        let encoded = encode_rgba_auto(rgba_img.as_raw(), actual_width, actual_height, JPEG_QUALITY_STANDARD)?;
        let compressed_bytes = encoded.bytes;
        let extension = encoded.extension;

        let compressed_size = compressed_bytes.len() as u64;

        Ok(CachedCompressedImage {
            bytes: Arc::new(compressed_bytes),
            extension: extension.to_string(),
            img_meta,
            original_size,
            compressed_size,
        })
    }
    #[cfg(target_os = "android")]
    {
        // Check if we have cached bytes for this URI
        let (bytes, extension) = {
            let cache = ANDROID_FILE_CACHE.lock().unwrap();
            if let Some((cached_bytes, ext, _, _)) = cache.get(file_path) {
                (cached_bytes.clone(), ext.clone())
            } else {
                drop(cache);
                // Fall back to reading directly (may fail if permission expired)
                let (raw_bytes, ext) = filesystem::read_android_uri_bytes(file_path.to_string())?;
                (Arc::new(raw_bytes), ext)
            }
        };
        let original_size = bytes.len() as u64;

        // For GIFs, skip compression entirely to preserve animation
        if extension == "gif" {
            let img = ::image::load_from_memory(&bytes)
                .map_err(|e| format!("Failed to decode GIF: {}", e))?;

            let (width, height) = (img.width(), img.height());

            let img_meta = crate::util::generate_thumbhash_from_image(&img)
                .map(|thumbhash| ImageMetadata {
                    thumbhash,
                    width,
                    height,
                });

            return Ok(CachedCompressedImage {
                bytes,
                extension: "gif".to_string(),
                img_meta,
                original_size,
                compressed_size: original_size,
            });
        }

        // Try to load and decode the image
        let img = ::image::load_from_memory(&bytes)
            .map_err(|e| format!("Failed to decode image: {}", e))?;

        // Determine target dimensions (max 1920px on longest side)
        use crate::shared::image::{calculate_resize_dimensions, MAX_DIMENSION, encode_rgba_auto, JPEG_QUALITY_STANDARD};
        let (width, height) = (img.width(), img.height());
        let (new_width, new_height) = calculate_resize_dimensions(width, height, MAX_DIMENSION);

        // Resize if needed
        let resized_img = if new_width != width || new_height != height {
            img.resize(new_width, new_height, ::image::imageops::FilterType::Lanczos3)
        } else {
            img
        };

        let actual_width = resized_img.width();
        let actual_height = resized_img.height();

        let img_meta = crate::util::generate_thumbhash_from_image(&resized_img)
            .map(|thumbhash| ImageMetadata {
                thumbhash,
                width: actual_width,
                height: actual_height,
            });

        let rgba_img = resized_img.to_rgba8();
        let encoded = encode_rgba_auto(rgba_img.as_raw(), actual_width, actual_height, JPEG_QUALITY_STANDARD)?;
        let compressed_bytes = encoded.bytes;
        let extension = encoded.extension;

        let compressed_size = compressed_bytes.len() as u64;

        Ok(CachedCompressedImage {
            bytes: Arc::new(compressed_bytes),
            extension: extension.to_string(),
            img_meta,
            original_size,
            compressed_size,
        })
    }
}