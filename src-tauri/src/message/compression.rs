//! Image compression functions.
//!
//! This module handles:
//! - Image compression with resize to max 1920px
//! - GIF preservation (skip compression to keep animation)
//! - PNG for transparent images, JPEG for opaque
//! - Blurhash generation for previews

use super::types::{CachedCompressedImage, ImageMetadata};

#[cfg(target_os = "android")]
use super::types::ANDROID_FILE_CACHE;
#[cfg(target_os = "android")]
use crate::android::filesystem;

/// Minimum savings percentage required for compression to be worthwhile
pub const MIN_SAVINGS_PERCENT: u64 = 1;

/// Internal function to compress bytes
/// Takes ownership of bytes to avoid cloning.
/// If `min_savings_percent` is Some and compression doesn't meet threshold,
/// returns original bytes with metadata (no wasted clone).
pub(super) fn compress_bytes_internal(
    bytes: Vec<u8>,
    extension: &str,
    min_savings_percent: Option<u64>,
) -> Result<CachedCompressedImage, String> {
    let original_size = bytes.len() as u64;

    // For GIFs, skip compression to preserve animation
    if extension == "gif" {
        let img = ::image::load_from_memory(&bytes)
            .map_err(|e| format!("Failed to decode GIF: {}", e))?;

        let (width, height) = (img.width(), img.height());
        let rgba_img = img.to_rgba8();

        let img_meta = crate::util::generate_blurhash_from_rgba(
            rgba_img.as_raw(),
            width,
            height
        ).map(|blurhash| ImageMetadata {
            blurhash,
            width,
            height,
        });

        // Return owned bytes directly - no clone needed
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

    // Generate metadata from original image (needed for both paths)
    let rgba_for_meta = img.to_rgba8();
    let img_meta = crate::util::generate_blurhash_from_rgba(
        rgba_for_meta.as_raw(),
        img.width(),
        img.height()
    ).map(|blurhash| ImageMetadata {
        blurhash,
        width: img.width(),
        height: img.height(),
    });

    // Determine target dimensions (max 1920px on longest side)
    let (width, height) = (img.width(), img.height());
    let max_dimension = 1920u32;

    let (new_width, new_height) = if width > max_dimension || height > max_dimension {
        if width > height {
            let ratio = max_dimension as f32 / width as f32;
            (max_dimension, (height as f32 * ratio) as u32)
        } else {
            let ratio = max_dimension as f32 / height as f32;
            ((width as f32 * ratio) as u32, max_dimension)
        }
    } else {
        (width, height)
    };

    // Resize if needed
    let resized_img = if new_width != width || new_height != height {
        img.resize(new_width, new_height, ::image::imageops::FilterType::Lanczos3)
    } else {
        img
    };

    let rgba_img = resized_img.to_rgba8();
    let actual_width = rgba_img.width();
    let actual_height = rgba_img.height();

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
            // Compression not worth it - return original bytes with metadata
            return Ok(CachedCompressedImage {
                bytes,
                extension: extension.to_string(),
                img_meta,
                original_size,
                compressed_size: original_size,
            });
        }
    }

    // Update metadata with resized dimensions if image was resized
    let final_meta = if actual_width != width || actual_height != height {
        crate::util::generate_blurhash_from_rgba(
            rgba_img.as_raw(),
            actual_width,
            actual_height
        ).map(|blurhash| ImageMetadata {
            blurhash,
            width: actual_width,
            height: actual_height,
        })
    } else {
        img_meta
    };

    Ok(CachedCompressedImage {
        bytes: compressed_bytes,
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
        let path = std::path::Path::new(file_path);
        
        if !path.exists() {
            return Err(format!("File does not exist: {}", file_path));
        }
        
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
        // Just decode first frame for blurhash, then return original bytes
        if extension == "gif" {
            // Decode just to get dimensions and generate blurhash from first frame
            let img = ::image::load_from_memory(&bytes)
                .map_err(|e| format!("Failed to decode GIF: {}", e))?;
            
            let (width, height) = (img.width(), img.height());
            let rgba_img = img.to_rgba8();
            
            let img_meta = crate::util::generate_blurhash_from_rgba(
                rgba_img.as_raw(),
                width,
                height
            ).map(|blurhash| ImageMetadata {
                blurhash,
                width,
                height,
            });
            
            // Return original bytes unchanged to preserve animation
            return Ok(CachedCompressedImage {
                bytes,
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
        let (width, height) = (img.width(), img.height());
        let max_dimension = 1920u32;
        
        let (new_width, new_height) = if width > max_dimension || height > max_dimension {
            if width > height {
                let ratio = max_dimension as f32 / width as f32;
                (max_dimension, (height as f32 * ratio) as u32)
            } else {
                let ratio = max_dimension as f32 / height as f32;
                ((width as f32 * ratio) as u32, max_dimension)
            }
        } else {
            (width, height)
        };
        
        // Resize if needed
        let resized_img = if new_width != width || new_height != height {
            img.resize(new_width, new_height, ::image::imageops::FilterType::Lanczos3)
        } else {
            img
        };
        
        let rgba_img = resized_img.to_rgba8();
        let actual_width = rgba_img.width();
        let actual_height = rgba_img.height();
        
        // Encode as PNG (alpha/small) or JPEG (standard)
        use crate::shared::image::{encode_rgba_auto, JPEG_QUALITY_STANDARD};
        let encoded = encode_rgba_auto(rgba_img.as_raw(), actual_width, actual_height, JPEG_QUALITY_STANDARD)?;
        let compressed_bytes = encoded.bytes;
        let extension = encoded.extension;

        let img_meta = crate::util::generate_blurhash_from_rgba(
            rgba_img.as_raw(),
            actual_width,
            actual_height
        ).map(|blurhash| ImageMetadata {
            blurhash,
            width: actual_width,
            height: actual_height,
        });
        
        let compressed_size = compressed_bytes.len() as u64;

        Ok(CachedCompressedImage {
            bytes: compressed_bytes,
            extension: extension.to_string(),
            img_meta,
            original_size,
            compressed_size,
        })
    }
    #[cfg(target_os = "android")]
    {
        // First check if we have cached bytes for this URI
        let (bytes, extension) = {
            let cache = ANDROID_FILE_CACHE.lock().unwrap();
            if let Some((cached_bytes, ext, _, _)) = cache.get(file_path) {
                (cached_bytes.clone(), ext.clone())
            } else {
                drop(cache);
                // Fall back to reading directly (may fail if permission expired)
                filesystem::read_android_uri_bytes(file_path.to_string())?
            }
        };
        let original_size = bytes.len() as u64;
        
        // For GIFs, skip compression entirely to preserve animation
        if extension == "gif" {
            let img = ::image::load_from_memory(&bytes)
                .map_err(|e| format!("Failed to decode GIF: {}", e))?;
            
            let (width, height) = (img.width(), img.height());
            let rgba_img = img.to_rgba8();
            
            let img_meta = crate::util::generate_blurhash_from_rgba(
                rgba_img.as_raw(),
                width,
                height
            ).map(|blurhash| ImageMetadata {
                blurhash,
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
        let (width, height) = (img.width(), img.height());
        let max_dimension = 1920u32;
        
        let (new_width, new_height) = if width > max_dimension || height > max_dimension {
            if width > height {
                let ratio = max_dimension as f32 / width as f32;
                (max_dimension, (height as f32 * ratio) as u32)
            } else {
                let ratio = max_dimension as f32 / height as f32;
                ((width as f32 * ratio) as u32, max_dimension)
            }
        } else {
            (width, height)
        };
        
        // Resize if needed
        let resized_img = if new_width != width || new_height != height {
            img.resize(new_width, new_height, ::image::imageops::FilterType::Lanczos3)
        } else {
            img
        };
        
        let rgba_img = resized_img.to_rgba8();
        let actual_width = rgba_img.width();
        let actual_height = rgba_img.height();
        
        // Encode as PNG (alpha/small) or JPEG (standard)
        use crate::shared::image::{encode_rgba_auto, JPEG_QUALITY_STANDARD};
        let encoded = encode_rgba_auto(rgba_img.as_raw(), actual_width, actual_height, JPEG_QUALITY_STANDARD)?;
        let compressed_bytes = encoded.bytes;
        let extension = encoded.extension;

        let img_meta = crate::util::generate_blurhash_from_rgba(
            rgba_img.as_raw(),
            actual_width,
            actual_height
        ).map(|blurhash| ImageMetadata {
            blurhash,
            width: actual_width,
            height: actual_height,
        });
        
        let compressed_size = compressed_bytes.len() as u64;
        
        Ok(CachedCompressedImage {
            bytes: compressed_bytes,
            extension,
            img_meta,
            original_size,
            compressed_size,
        })
    }
}