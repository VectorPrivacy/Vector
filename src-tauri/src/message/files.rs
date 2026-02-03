//! File handling commands.
//!
//! This module handles:
//! - File caching from JavaScript/WebView
//! - File sending (compressed and uncompressed)
//! - Image preview generation
//! - Android file handling

use std::sync::Arc;
use nostr_sdk::prelude::*;
use tokio::sync::Mutex as TokioMutex;
use once_cell::sync::Lazy;

use crate::util;
use crate::shared::image::read_file_checked;

use super::types::{CachedCompressedImage, AttachmentFile, ImageMetadata, COMPRESSION_CACHE, ANDROID_FILE_CACHE};
use super::compression::{compress_bytes_internal, compress_image_internal};
use super::sending::{message, MessageSendResult};

#[cfg(target_os = "android")]
use crate::android::filesystem;

/// Cache for bytes received from JavaScript (for Android file handling)
static JS_FILE_CACHE: Lazy<std::sync::Mutex<Option<(Arc<Vec<u8>>, String, String)>>> =
    Lazy::new(|| std::sync::Mutex::new(None));

/// Cache for compressed bytes from JavaScript file
static JS_COMPRESSION_CACHE: Lazy<TokioMutex<Option<CachedCompressedImage>>> =
    Lazy::new(|| TokioMutex::new(None));

/// Response from caching file bytes, includes preview for images
#[derive(serde::Serialize)]
pub struct CacheFileBytesResult {
    pub size: u64,
    pub name: String,
    pub extension: String,
    /// Base64 data URL for image preview (only for supported image types)
    pub preview: Option<String>,
}

/// Cache file bytes received from JavaScript (for Android)
/// This is called immediately when a file is selected via the WebView file input
/// Returns file info and a thumbnail preview for images
#[tauri::command]
pub fn cache_file_bytes(bytes: Vec<u8>, file_name: String, extension: String) -> Result<CacheFileBytesResult, String> {
    let size = bytes.len() as u64;

    // Generate preview for supported image types
    let preview = if matches!(extension.as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp" | "tiff" | "tif" | "ico") {
        generate_image_preview_from_bytes(&bytes).ok()
    } else {
        None
    };

    let bytes = Arc::new(bytes);

    let mut cache = JS_FILE_CACHE.lock().unwrap();
    *cache = Some((bytes, file_name.clone(), extension.clone()));

    Ok(CacheFileBytesResult {
        size,
        name: file_name,
        extension,
        preview,
    })
}

/// Get cached file info (for preview display)
#[tauri::command]
pub fn get_cached_file_info() -> Result<Option<FileInfo>, String> {
    let cache = JS_FILE_CACHE.lock().unwrap();
    match &*cache {
        Some((bytes, name, ext)) => Ok(Some(FileInfo {
            size: bytes.len() as u64,
            name: name.clone(),
            extension: ext.clone(),
        })),
        None => Ok(None),
    }
}

/// Get base64 preview of cached image bytes
#[tauri::command]
pub fn get_cached_image_preview(quality: u32) -> Result<String, String> {
    use crate::shared::image::{calculate_preview_dimensions, encode_rgba_auto, JPEG_QUALITY_PREVIEW};

    let cache = JS_FILE_CACHE.lock().unwrap();
    let (bytes, _, _) = cache.as_ref().ok_or("No cached file")?;
    let bytes = bytes.clone();
    drop(cache);

    let img = ::image::load_from_memory(&bytes)
        .map_err(|e| format!("Failed to decode image: {}", e))?;

    let (width, height) = (img.width(), img.height());
    let (new_width, new_height) = calculate_preview_dimensions(width, height, quality);

    // Use SIMD-accelerated resize (10-15x faster for large JPEGs)
    let (rgba_pixels, out_w, out_h) = crate::simd::image::fast_resize_to_rgba(&img, new_width, new_height);
    let encoded = encode_rgba_auto(&rgba_pixels, out_w, out_h, JPEG_QUALITY_PREVIEW)?;

    Ok(encoded.to_data_uri())
}

/// Start compression of cached bytes
#[tauri::command]
pub async fn start_cached_bytes_compression() -> Result<(), String> {
    let (bytes, _, extension) = {
        let cache = JS_FILE_CACHE.lock().unwrap();
        let (b, _, e) = cache.as_ref().ok_or("No cached file")?;
        (b.clone(), String::new(), e.clone())
    };

    // Clear any previous compression result
    {
        let mut comp_cache = JS_COMPRESSION_CACHE.lock().await;
        *comp_cache = None;
    }

    // Spawn compression task (no min_savings - checked later by caller)
    tokio::spawn(async move {
        let result = compress_bytes_internal(bytes, &extension, None);
        let mut comp_cache = JS_COMPRESSION_CACHE.lock().await;
        *comp_cache = result.ok();
    });

    Ok(())
}

/// Get compression status for cached bytes
#[tauri::command]
pub async fn get_cached_bytes_compression_status() -> Result<Option<CompressionEstimate>, String> {
    let comp_cache = JS_COMPRESSION_CACHE.lock().await;
    
    match &*comp_cache {
        Some(cached) => {
            let savings_percent = if cached.original_size > 0 && cached.compressed_size < cached.original_size {
                ((cached.original_size - cached.compressed_size) * 100 / cached.original_size) as u32
            } else {
                0
            };
            
            Ok(Some(CompressionEstimate {
                original_size: cached.original_size,
                estimated_size: cached.compressed_size,
                savings_percent,
            }))
        }
        None => Ok(None),
    }
}

/// Send cached file (with optional compression)
#[tauri::command]
pub async fn send_cached_file(receiver: String, replied_to: String, use_compression: bool) -> Result<MessageSendResult, String> {
    use super::compression::MIN_SAVINGS_PERCENT;

    if use_compression {
        // Check if compression is complete - take ownership to avoid clone
        let mut comp_cache = JS_COMPRESSION_CACHE.lock().await;
        if let Some(compressed) = comp_cache.take() {
            // Check if compression provides significant savings
            let savings_percent = if compressed.original_size > 0 && compressed.compressed_size < compressed.original_size {
                ((compressed.original_size - compressed.compressed_size) * 100) / compressed.original_size
            } else {
                0
            };

            if savings_percent >= MIN_SAVINGS_PERCENT {
                // Use compressed version - no clone needed, we own it
                drop(comp_cache);
                *JS_FILE_CACHE.lock().unwrap() = None;

                let attachment_file = AttachmentFile {
                    bytes: compressed.bytes,
                    extension: compressed.extension,
                    img_meta: compressed.img_meta,
                };
                return message(receiver, String::new(), replied_to, Some(attachment_file)).await;
            }
        }
        drop(comp_cache);
    }
    
    // Use original bytes - compress on-the-fly if use_compression is true
    let (original_bytes, _, original_extension) = {
        let mut cache = JS_FILE_CACHE.lock().unwrap();
        cache.take().ok_or("No cached file")?
    };

    // Clear compression cache
    *JS_COMPRESSION_CACHE.lock().await = None;

    // Check if this is an image type
    let is_image = matches!(original_extension.as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp" | "tiff" | "tif" | "ico");

    // Process images: generate metadata and optionally compress
    let (bytes, extension, img_meta) = if is_image {
        if let Ok(img) = ::image::load_from_memory(&original_bytes) {
            let blurhash_meta = crate::util::generate_blurhash_from_image(&img)
                .map(|blurhash| ImageMetadata {
                    blurhash,
                    width: img.width(),
                    height: img.height(),
                });

            // GIFs: never compress, preserve animation
            // Other images: compress if requested
            if original_extension == "gif" || !use_compression {
                // No compression - just use original bytes with metadata
                (original_bytes, original_extension, blurhash_meta)
            } else {
                // Compress on-the-fly since pre-compression wasn't ready
                use crate::shared::image::{encode_rgba_auto, JPEG_QUALITY_STANDARD};
                let rgba_img = img.to_rgba8();
                match encode_rgba_auto(rgba_img.as_raw(), img.width(), img.height(), JPEG_QUALITY_STANDARD) {
                    Ok(encoded) => (Arc::new(encoded.bytes), encoded.extension.to_string(), blurhash_meta),
                    Err(_) => (original_bytes, original_extension, blurhash_meta),
                }
            }
        } else {
            (original_bytes, original_extension, None)
        }
    } else {
        // Non-image file
        (original_bytes, original_extension, None)
    };

    let attachment_file = AttachmentFile {
        bytes,
        extension,
        img_meta,
    };

    message(receiver, String::new(), replied_to, Some(attachment_file)).await
}

/// Clear cached file bytes
#[tauri::command]
pub async fn clear_cached_file() -> Result<(), String> {
    *JS_FILE_CACHE.lock().unwrap() = None;
    *JS_COMPRESSION_CACHE.lock().await = None;
    Ok(())
}

/// Clear Android file cache for a specific file path
/// This should be called when the user cancels file selection or after sending
#[tauri::command]
pub fn clear_android_file_cache(file_path: String) -> Result<(), String> {
    let mut cache = ANDROID_FILE_CACHE.lock().unwrap();
    cache.remove(&file_path);
    Ok(())
}

/// Clear all Android file cache entries
/// This is a cleanup function to ensure no stale data remains
#[tauri::command]
pub fn clear_all_android_file_cache() -> Result<(), String> {
    let mut cache = ANDROID_FILE_CACHE.lock().unwrap();
    cache.clear();
    Ok(())
}

/// Send file bytes directly from the frontend (used for Android optimized flow)
/// This receives the file bytes from JavaScript and sends them as an attachment
#[tauri::command]
pub async fn send_file_bytes(
    receiver: String,
    replied_to: String,
    file_bytes: Vec<u8>,
    file_name: String,
    use_compression: bool
) -> Result<MessageSendResult, String> {
    use super::compression::MIN_SAVINGS_PERCENT;

    // Extract extension from filename
    let extension = file_name
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_lowercase();

    let is_image = matches!(extension.as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp" | "tiff" | "tif" | "ico");

    // For images: compress if requested, otherwise just generate metadata
    let attachment_file = if is_image {
        let min_savings = if use_compression && extension != "gif" {
            Some(MIN_SAVINGS_PERCENT)
        } else {
            None // GIFs or no compression - just get metadata
        };

        match compress_bytes_internal(Arc::new(file_bytes), &extension, min_savings) {
            Ok(result) => AttachmentFile {
                bytes: result.bytes,
                extension: result.extension,
                img_meta: result.img_meta,
            },
            Err(e) => {
                eprintln!("Image processing failed: {}", e);
                return Err(e);
            }
        }
    } else {
        // Non-image file - send as-is
        AttachmentFile {
            bytes: Arc::new(file_bytes),
            extension,
            img_meta: None,
        }
    };

    message(receiver, String::new(), replied_to, Some(attachment_file)).await
}

#[tauri::command]
pub async fn file_message(receiver: String, replied_to: String, file_path: String) -> Result<MessageSendResult, String> {
    // Load the file as AttachmentFile
    let mut attachment_file = {
        #[cfg(not(target_os = "android"))]
        {
            let bytes = read_file_checked(&file_path)?;

            let extension = file_path
                .rsplit('.')
                .next()
                .unwrap_or("bin")
                .to_lowercase();

            AttachmentFile {
                bytes: Arc::new(bytes),
                img_meta: None,
                extension,
            }
        }
        #[cfg(target_os = "android")]
        {
            // First check if we have cached bytes for this URI
            // Take ownership from cache to avoid clone - bytes already Arc
            let mut cache = ANDROID_FILE_CACHE.lock().unwrap();
            if let Some((bytes, extension, _, _)) = cache.remove(&file_path) {
                drop(cache);
                AttachmentFile {
                    bytes,
                    img_meta: None,
                    extension,
                }
            } else {
                drop(cache);
                // Check if this is a content:// URI or a regular file path
                if file_path.starts_with("content://") {
                    // Content URI - use Android ContentResolver
                    filesystem::read_android_uri(file_path)?
                } else {
                    // Regular file path (e.g., marketplace apps) - use standard file I/O
                    let bytes = read_file_checked(&file_path)?;

                    let extension = file_path
                        .rsplit('.')
                        .next()
                        .unwrap_or("bin")
                        .to_lowercase();

                    AttachmentFile {
                        bytes: Arc::new(bytes),
                        img_meta: None,
                        extension,
                    }
                }
            }
        }
    };

    // Generate image metadata if the file is an image
    if matches!(attachment_file.extension.as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp" | "tiff" | "tif" | "ico") {
        if let Ok(img) = ::image::load_from_memory(&attachment_file.bytes) {
            attachment_file.img_meta = util::generate_blurhash_from_image(&img)
                .map(|blurhash| ImageMetadata {
                    blurhash,
                    width: img.width(),
                    height: img.height(),
                });
        }
    }

    // Message the file to the intended user
    message(receiver, String::new(), replied_to, Some(attachment_file)).await
}

/// File info structure for the frontend
#[derive(serde::Serialize)]
pub struct FileInfo {
    pub size: u64,
    pub name: String,
    pub extension: String,
}

/// Response from caching an Android file, includes preview for images
#[derive(serde::Serialize)]
pub struct AndroidFileCacheResult {
    pub size: u64,
    pub name: String,
    pub extension: String,
    /// Base64 data URL for image preview (only for supported image types)
    pub preview: Option<String>,
}

/// Cache an Android content URI's bytes immediately after file selection.
/// This must be called immediately after the file picker returns, before the permission expires.
/// On non-Android platforms, this just returns file info without caching.
/// For Android, also generates a compressed base64 preview for images.
#[tauri::command]
pub fn cache_android_file(file_path: String) -> Result<AndroidFileCacheResult, String> {
    #[cfg(not(target_os = "android"))]
    {
        // On non-Android platforms, just return file info without caching
        let path = std::path::Path::new(&file_path);

        let metadata = std::fs::metadata(&file_path)
            .map_err(|e| format!("Failed to get file metadata: {}", e))?;

        let name = path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string();

        let extension = path.extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();

        Ok(AndroidFileCacheResult {
            size: metadata.len(),
            name,
            extension,
            preview: None, // Desktop doesn't need preview from this function
        })
    }
    #[cfg(target_os = "android")]
    {
        // Read the file using the same method as avatar upload (read_android_uri)
        // This uses getType() instead of query() which may have different permission behavior
        let attachment = filesystem::read_android_uri(file_path.clone())?;
        let bytes = attachment.bytes;
        let extension = attachment.extension.clone();
        let size = bytes.len() as u64;
        
        // For Android content URIs, we can't easily get the display name without query()
        // which may fail due to permissions. Use a generic name with the extension.
        let name = format!("file.{}", extension);
        
        // Generate preview for supported image types
        let preview = if matches!(extension.as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp" | "tiff" | "tif" | "ico") {
            generate_image_preview_from_bytes(&bytes).ok()
        } else {
            None
        };

        // Cache the bytes - already Arc from read_android_uri
        let mut cache = ANDROID_FILE_CACHE.lock().unwrap();
        cache.insert(file_path, (bytes, extension.clone(), name.clone(), size));
        
        Ok(AndroidFileCacheResult {
            size,
            name,
            extension,
            preview,
        })
    }
}

/// Generate a compressed base64 preview from image bytes
/// Preview is capped to UI display size (300x400 mobile, 512x512 desktop)
/// For files smaller than 5MB or GIFs, returns the original image as base64
fn generate_image_preview_from_bytes(bytes: &[u8]) -> Result<String, String> {
    use base64::Engine;
    use crate::shared::image::{calculate_capped_preview_dimensions, encode_rgba_auto, JPEG_QUALITY_PREVIEW};

    const SKIP_RESIZE_THRESHOLD: usize = 5 * 1024 * 1024; // 5MB

    // Detect if this is a GIF (we never resize GIFs to preserve animation)
    let is_gif = bytes.starts_with(b"GIF");

    // For small files or GIFs, just return the original as base64 (skip resizing)
    if bytes.len() < SKIP_RESIZE_THRESHOLD || is_gif {
        let base64_str = base64::engine::general_purpose::STANDARD.encode(bytes);

        // Detect image type from magic bytes for correct MIME type
        let mime_type = if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
            "image/jpeg"
        } else if bytes.starts_with(&[0x89, 0x50, 0x4E, 0x47]) {
            "image/png"
        } else if is_gif {
            "image/gif"
        } else if bytes.starts_with(b"RIFF") && bytes.len() > 12 && &bytes[8..12] == b"WEBP" {
            "image/webp"
        } else if bytes.len() >= 4 && ((bytes[0..2] == [0x49, 0x49] && bytes[2..4] == [0x2A, 0x00]) ||
                                        (bytes[0..2] == [0x4D, 0x4D] && bytes[2..4] == [0x00, 0x2A])) {
            // TIFF: II (little-endian) or MM (big-endian) followed by 42
            "image/tiff"
        } else if bytes.starts_with(&[0x00, 0x00, 0x01, 0x00]) {
            // ICO format
            "image/x-icon"
        } else {
            "image/jpeg" // Default fallback
        };

        return Ok(format!("data:{};base64,{}", mime_type, base64_str));
    }

    let img = ::image::load_from_memory(bytes)
        .map_err(|e| format!("Failed to decode image: {}", e))?;

    let (width, height) = (img.width(), img.height());
    let (new_width, new_height) = calculate_capped_preview_dimensions(width, height);

    // Use SIMD-accelerated resize (10-15x faster for large JPEGs)
    let (rgba_pixels, out_w, out_h) = crate::simd::image::fast_resize_to_rgba(&img, new_width, new_height);
    let encoded = encode_rgba_auto(&rgba_pixels, out_w, out_h, JPEG_QUALITY_PREVIEW)?;

    Ok(encoded.to_data_uri())
}

/// Get file information (size, name, extension)
#[tauri::command]
pub fn get_file_info(file_path: String) -> Result<FileInfo, String> {
    #[cfg(not(target_os = "android"))]
    {
        let path = std::path::Path::new(&file_path);

        let metadata = std::fs::metadata(&file_path)
            .map_err(|e| format!("Failed to get file metadata: {}", e))?;

        let name = path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string();

        let extension = path.extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();

        Ok(FileInfo {
            size: metadata.len(),
            name,
            extension,
        })
    }
    #[cfg(target_os = "android")]
    {
        // First check if we have cached bytes for this URI
        let cache = ANDROID_FILE_CACHE.lock().unwrap();
        if let Some((bytes, extension, name, _)) = cache.get(&file_path) {
            return Ok(FileInfo {
                size: bytes.len() as u64,
                name: name.clone(),
                extension: extension.clone(),
            });
        }
        drop(cache);
        
        // Fall back to querying the URI directly (may fail if permission expired)
        filesystem::get_android_uri_info(file_path)
    }
}

/// Get a base64 preview of an image (for Android where convertFileSrc doesn't work)
/// The quality parameter (1-100) determines the resize percentage
#[tauri::command]
pub fn get_image_preview_base64(file_path: String, quality: u32) -> Result<String, String> {
    use crate::shared::image::{calculate_preview_dimensions, encode_rgba_auto, JPEG_QUALITY_PREVIEW};

    #[cfg(not(target_os = "android"))]
    {
        let bytes = std::fs::read(&file_path)
            .map_err(|e| format!("Failed to read file: {}", e))?;

        let img = ::image::load_from_memory(&bytes)
            .map_err(|e| format!("Failed to decode image: {}", e))?;

        let (width, height) = (img.width(), img.height());
        let (new_width, new_height) = calculate_preview_dimensions(width, height, quality);

        // Use SIMD-accelerated resize (10-15x faster for large JPEGs)
        let (rgba_pixels, out_w, out_h) = crate::simd::image::fast_resize_to_rgba(&img, new_width, new_height);
        let encoded = encode_rgba_auto(&rgba_pixels, out_w, out_h, JPEG_QUALITY_PREVIEW)?;

        Ok(encoded.to_data_uri())
    }

    #[cfg(target_os = "android")]
    {
        // First check if we have cached bytes for this URI
        let bytes = {
            let cache = ANDROID_FILE_CACHE.lock().unwrap();
            if let Some((cached_bytes, _, _, _)) = cache.get(&file_path) {
                cached_bytes.clone()
            } else {
                drop(cache);
                // Fall back to reading directly (may fail if permission expired)
                Arc::new(filesystem::read_android_uri_bytes(file_path)?.0)
            }
        };

        let img = ::image::load_from_memory(&bytes)
            .map_err(|e| format!("Failed to decode image: {}", e))?;

        let (width, height) = (img.width(), img.height());
        let (new_width, new_height) = calculate_preview_dimensions(width, height, quality);

        // Use SIMD-accelerated resize (10-15x faster for large JPEGs)
        let (rgba_pixels, out_w, out_h) = crate::simd::image::fast_resize_to_rgba(&img, new_width, new_height);
        let encoded = encode_rgba_auto(&rgba_pixels, out_w, out_h, JPEG_QUALITY_PREVIEW)?;

        Ok(encoded.to_data_uri())
    }
}

/// Send a file with compression (for images)
#[tauri::command]
pub async fn file_message_compressed(receiver: String, replied_to: String, file_path: String) -> Result<MessageSendResult, String> {
    // Load the file as AttachmentFile
    let mut attachment_file = {
        #[cfg(not(target_os = "android"))]
        {
            let bytes = read_file_checked(&file_path)?;

            let extension = file_path
                .rsplit('.')
                .next()
                .unwrap_or("bin")
                .to_lowercase();

            AttachmentFile {
                bytes: Arc::new(bytes),
                img_meta: None,
                extension,
            }
        }
        #[cfg(target_os = "android")]
        {
            // First check if we have cached bytes for this URI
            // Take ownership from cache to avoid clone - bytes already Arc
            let mut cache = ANDROID_FILE_CACHE.lock().unwrap();
            if let Some((bytes, extension, _, _)) = cache.remove(&file_path) {
                drop(cache);
                AttachmentFile {
                    bytes,
                    img_meta: None,
                    extension,
                }
            } else {
                drop(cache);
                // Check if this is a content:// URI or a regular file path
                if file_path.starts_with("content://") {
                    // Content URI - use Android ContentResolver
                    filesystem::read_android_uri(file_path)?
                } else {
                    // Regular file path (e.g., marketplace apps) - use standard file I/O
                    let bytes = read_file_checked(&file_path)?;

                    let extension = file_path
                        .rsplit('.')
                        .next()
                        .unwrap_or("bin")
                        .to_lowercase();

                    AttachmentFile {
                        bytes: Arc::new(bytes),
                        img_meta: None,
                        extension,
                    }
                }
            }
        }
    };

    // Compress the image if it's a supported format
    if matches!(attachment_file.extension.as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp" | "tiff" | "tif" | "ico") {
        if let Ok(img) = ::image::load_from_memory(&attachment_file.bytes) {
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

            attachment_file.img_meta = crate::util::generate_blurhash_from_image(&resized_img)
                .map(|blurhash| ImageMetadata {
                    blurhash,
                    width: resized_img.width(),
                    height: resized_img.height(),
                });

            let rgba_img = resized_img.to_rgba8();

            let mut compressed_bytes = Vec::new();

            // Encode based on format (GIF stays GIF, others use smart PNG/JPEG selection)
            if attachment_file.extension == "gif" {
                // For GIFs, just resize but keep format
                let mut cursor = std::io::Cursor::new(&mut compressed_bytes);
                let mut encoder = ::image::codecs::gif::GifEncoder::new(&mut cursor);
                encoder.encode(
                    rgba_img.as_raw(),
                    rgba_img.width(),
                    rgba_img.height(),
                    ::image::ExtendedColorType::Rgba8.into()
                ).map_err(|e| format!("Failed to encode GIF: {}", e))?;
            } else {
                let encoded = encode_rgba_auto(rgba_img.as_raw(), rgba_img.width(), rgba_img.height(), JPEG_QUALITY_STANDARD)?;
                compressed_bytes = encoded.bytes;
                attachment_file.extension = encoded.extension.to_string();
            }

            attachment_file.bytes = Arc::new(compressed_bytes);
        }
    }

    // Message the file to the intended user
    message(receiver, String::new(), replied_to, Some(attachment_file)).await
}

/// Compression estimate result
#[derive(serde::Serialize, Clone)]
pub struct CompressionEstimate {
    pub original_size: u64,
    pub estimated_size: u64,
    pub savings_percent: u32,
}

/// Start pre-compressing an image and cache the result
/// This is called when the file preview opens
#[tauri::command]
pub async fn start_image_precompression(file_path: String) -> Result<(), String> {
    // Mark as "in progress" by inserting None
    {
        let mut cache = COMPRESSION_CACHE.lock().await;
        cache.insert(file_path.clone(), None);
    }
    
    // Spawn the compression task
    let path_clone = file_path.clone();
    tokio::spawn(async move {
        let result = compress_image_internal(&path_clone);
        let mut cache = COMPRESSION_CACHE.lock().await;
        
        // Only store if still in cache (not cancelled)
        if cache.contains_key(&path_clone) {
            cache.insert(path_clone, result.ok());
        }
    });
    
    Ok(())
}

/// Get the compression status/result for a file
#[tauri::command]
pub async fn get_compression_status(file_path: String) -> Result<Option<CompressionEstimate>, String> {
    let cache = COMPRESSION_CACHE.lock().await;
    
    match cache.get(&file_path) {
        Some(Some(cached)) => {
            // Compression complete
            let savings_percent = if cached.original_size > 0 && cached.compressed_size < cached.original_size {
                ((cached.original_size - cached.compressed_size) * 100 / cached.original_size) as u32
            } else {
                0
            };
            
            Ok(Some(CompressionEstimate {
                original_size: cached.original_size,
                estimated_size: cached.compressed_size,
                savings_percent,
            }))
        }
        Some(None) => {
            // Still compressing
            Ok(None)
        }
        None => {
            // Not in cache
            Err("File not in compression cache".to_string())
        }
    }
}

/// Clear the compression cache for a file (called on cancel)
#[tauri::command]
pub async fn clear_compression_cache(file_path: String) -> Result<(), String> {
    // Clear compression cache
    let mut cache = COMPRESSION_CACHE.lock().await;
    cache.remove(&file_path);
    drop(cache);
    
    // Also clear Android file cache
    let mut android_cache = ANDROID_FILE_CACHE.lock().unwrap();
    android_cache.remove(&file_path);
    
    Ok(())
}

/// Send a file using the cached compressed version if available
#[tauri::command]
pub async fn send_cached_compressed_file(receiver: String, replied_to: String, file_path: String) -> Result<MessageSendResult, String> {
    use super::compression::MIN_SAVINGS_PERCENT;
    
    // First check if compression is complete or still in progress
    let status = {
        let cache = COMPRESSION_CACHE.lock().await;
        cache.get(&file_path).cloned()
    };
    
    match status {
        Some(Some(compressed)) => {
            // Compression complete - remove from cache
            {
                let mut cache = COMPRESSION_CACHE.lock().await;
                cache.remove(&file_path);
            }
            
            // Check if compression provides significant savings
            let savings_percent = if compressed.original_size > 0 && compressed.compressed_size < compressed.original_size {
                ((compressed.original_size - compressed.compressed_size) * 100) / compressed.original_size
            } else {
                0 // No savings or compression made it bigger
            };
            
            if savings_percent >= MIN_SAVINGS_PERCENT {
                // Compression provides significant savings - send compressed
                let attachment_file = AttachmentFile {
                    bytes: compressed.bytes,
                    extension: compressed.extension,
                    img_meta: compressed.img_meta,
                };
                message(receiver, String::new(), replied_to, Some(attachment_file)).await
            } else {
                // No significant savings - send original file
                file_message(receiver, replied_to, file_path).await
            }
        }
        Some(None) => {
            // Still compressing - wait for it
            loop {
                tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                let cache = COMPRESSION_CACHE.lock().await;
                match cache.get(&file_path) {
                    Some(Some(_)) => break,
                    Some(None) => continue,
                    None => {
                        // Cache was cleared - fall back to compressing now
                        drop(cache);
                        return file_message_compressed(receiver, replied_to, file_path).await;
                    }
                }
            }
            
            // Now get the result
            let cached = {
                let mut cache = COMPRESSION_CACHE.lock().await;
                cache.remove(&file_path)
            };
            
            if let Some(Some(compressed)) = cached {
                // Check if compression provides significant savings
                let savings_percent = if compressed.original_size > 0 && compressed.compressed_size < compressed.original_size {
                    ((compressed.original_size - compressed.compressed_size) * 100) / compressed.original_size
                } else {
                    0 // No savings or compression made it bigger
                };
                
                if savings_percent >= MIN_SAVINGS_PERCENT {
                    // Compression provides significant savings - send compressed
                    let attachment_file = AttachmentFile {
                        bytes: compressed.bytes,
                        extension: compressed.extension,
                        img_meta: compressed.img_meta,
                    };
                    message(receiver, String::new(), replied_to, Some(attachment_file)).await
                } else {
                    // No significant savings - send original file
                    file_message(receiver, replied_to, file_path).await
                }
            } else {
                Err("Failed to get compressed image".to_string())
            }
        }
        None => {
            // Not in cache, compress now
            file_message_compressed(receiver, replied_to, file_path).await
        }
    }
}