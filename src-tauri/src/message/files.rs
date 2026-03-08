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
use std::sync::LazyLock;

use crate::util;
use crate::shared::image::read_file_checked;

use super::types::{CachedCompressedImage, AttachmentFile, ImageMetadata, COMPRESSION_CACHE, ANDROID_FILE_CACHE};
use super::compression::{compress_bytes_internal, compress_image_internal};
use super::sending::{message, MessageSendResult};

#[cfg(target_os = "android")]
use crate::android::filesystem;

/// Cache for bytes received from JavaScript (for Android file handling)
static JS_FILE_CACHE: LazyLock<std::sync::Mutex<Option<(Arc<Vec<u8>>, String, String)>>> =
    LazyLock::new(|| std::sync::Mutex::new(None));

/// Cache for compressed bytes from JavaScript file
static JS_COMPRESSION_CACHE: LazyLock<TokioMutex<Option<CachedCompressedImage>>> =
    LazyLock::new(|| TokioMutex::new(None));

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

/// Generate a thumbhash data-URL from an image.
/// Tries the JS byte cache first (Android / clipboard paste), then falls back to
/// reading `file_path` from disk (desktop).
#[tauri::command]
pub fn generate_thumbhash_for_preview(file_path: String) -> Result<String, String> {
    // 1. Try the JS byte cache
    let img = {
        let cache = JS_FILE_CACHE.lock().unwrap();
        if let Some((bytes, _, _)) = cache.as_ref() {
            ::image::load_from_memory(bytes).ok()
        } else {
            None
        }
    };

    // 2. Fall back to reading from file path
    let img = match img {
        Some(i) => i,
        None => {
            if file_path.is_empty() {
                return Err("No cached file and no file path provided".into());
            }
            ::image::open(&file_path)
                .map_err(|e| format!("Failed to open image: {}", e))?
        }
    };

    let thumbhash = util::generate_thumbhash_from_image(&img)
        .ok_or_else(|| "Failed to generate thumbhash".to_string())?;
    Ok(util::decode_thumbhash_to_base64(&thumbhash))
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
pub async fn send_cached_file(receiver: String, replied_to: String, use_compression: bool, name_override: String) -> Result<MessageSendResult, String> {
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
                // Extract name and clear the cache in one lock
                let name = {
                    let mut cache = JS_FILE_CACHE.lock().unwrap();
                    let n = cache.as_ref().map(|(_, n, _)| n.clone()).unwrap_or_default();
                    *cache = None;
                    n
                };

                let mut attachment_file = AttachmentFile {
                    bytes: compressed.bytes,
                    extension: compressed.extension,
                    img_meta: compressed.img_meta,
                    name,
                };
                if !name_override.is_empty() {
                    let sanitized = crate::commands::attachments::sanitize_filename(&name_override);
                    if !sanitized.is_empty() { attachment_file.name = sanitized; }
                }
                return message(receiver, String::new(), replied_to, Some(attachment_file)).await;
            }
        }
        drop(comp_cache);
    }
    
    // Use original bytes - compress on-the-fly if use_compression is true
    let (original_bytes, original_name, original_extension) = {
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
            let thumbhash_meta = crate::util::generate_thumbhash_from_image(&img)
                .map(|thumbhash| ImageMetadata {
                    thumbhash,
                    width: img.width(),
                    height: img.height(),
                });

            // GIFs: never compress, preserve animation
            // Other images: compress if requested
            if original_extension == "gif" || !use_compression {
                // No compression - just use original bytes with metadata
                (original_bytes, original_extension, thumbhash_meta)
            } else {
                // Compress on-the-fly since pre-compression wasn't ready
                use crate::shared::image::{encode_rgba_auto, JPEG_QUALITY_STANDARD};
                let rgba_img = img.to_rgba8();
                match encode_rgba_auto(rgba_img.as_raw(), img.width(), img.height(), JPEG_QUALITY_STANDARD) {
                    Ok(encoded) => (Arc::new(encoded.bytes), encoded.extension.to_string(), thumbhash_meta),
                    Err(_) => (original_bytes, original_extension, thumbhash_meta),
                }
            }
        } else {
            (original_bytes, original_extension, None)
        }
    } else {
        // Non-image file
        (original_bytes, original_extension, None)
    };

    let mut attachment_file = AttachmentFile {
        bytes,
        extension,
        img_meta,
        name: original_name,
    };
    if !name_override.is_empty() {
        let sanitized = crate::commands::attachments::sanitize_filename(&name_override);
        if !sanitized.is_empty() { attachment_file.name = sanitized; }
    }

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
    use_compression: bool,
    name_override: String
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
    let mut attachment_file = if is_image {
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
                name: file_name.clone(),
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
            name: file_name,
        }
    };
    if !name_override.is_empty() {
        let sanitized = crate::commands::attachments::sanitize_filename(&name_override);
        if !sanitized.is_empty() { attachment_file.name = sanitized; }
    }

    message(receiver, String::new(), replied_to, Some(attachment_file)).await
}

#[tauri::command]
pub async fn file_message(receiver: String, replied_to: String, file_path: String, name_override: String) -> Result<MessageSendResult, String> {
    // Extract filename from the path
    let file_name = std::path::Path::new(&file_path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string();

    // Load the file as AttachmentFile
    let mut attachment_file = {
        #[cfg(not(target_os = "android"))]
        {
            let file_bytes = read_file_checked(&file_path)?;

            let extension = file_path
                .rsplit('.')
                .next()
                .unwrap_or("bin")
                .to_lowercase();

            AttachmentFile {
                bytes: Arc::new(file_bytes),
                img_meta: None,
                extension,
                name: file_name.clone(),
            }
        }
        #[cfg(target_os = "android")]
        {
            // First check if we have cached bytes for this URI
            // Take ownership from cache to avoid clone - bytes already Arc
            let mut cache = ANDROID_FILE_CACHE.lock().unwrap();
            if let Some((bytes, extension, cached_name, _)) = cache.remove(&file_path) {
                drop(cache);
                AttachmentFile {
                    bytes,
                    img_meta: None,
                    extension,
                    name: cached_name,
                }
            } else {
                drop(cache);
                // Check if this is a content:// URI or a regular file path
                if file_path.starts_with("content://") {
                    // Content URI - use Android ContentResolver
                    filesystem::read_android_uri(file_path)?
                } else {
                    // Regular file path (e.g., marketplace apps) - use standard file I/O
                    let file_bytes = read_file_checked(&file_path)?;

                    let extension = file_path
                        .rsplit('.')
                        .next()
                        .unwrap_or("bin")
                        .to_lowercase();

                    AttachmentFile {
                        bytes: Arc::new(file_bytes),
                        img_meta: None,
                        extension,
                        name: file_name.clone(),
                    }
                }
            }
        }
    };

    // Generate image metadata if the file is an image
    if matches!(attachment_file.extension.as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp" | "tiff" | "tif" | "ico") {
        if let Ok(img) = ::image::load_from_memory(&attachment_file.bytes) {
            attachment_file.img_meta = util::generate_thumbhash_from_image(&img)
                .map(|thumbhash| ImageMetadata {
                    thumbhash,
                    width: img.width(),
                    height: img.height(),
                });
        }
    }

    // Apply user-edited name override (if any)
    if !name_override.is_empty() {
        let sanitized = crate::commands::attachments::sanitize_filename(&name_override);
        if !sanitized.is_empty() { attachment_file.name = sanitized; }
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
    use crate::shared::image::{calculate_capped_preview_dimensions, encode_rgba_auto, JPEG_QUALITY_PREVIEW};

    const SKIP_RESIZE_THRESHOLD: usize = 5 * 1024 * 1024; // 5MB

    let detected = crate::util::mime_from_magic_bytes(bytes);
    let is_gif = detected == "image/gif";

    // For small files or GIFs, just return the original as base64 (skip resizing)
    if bytes.len() < SKIP_RESIZE_THRESHOLD || is_gif {
        // Fall back to image/jpeg if unrecognized (we know it's an image at this point)
        let mime_type = if detected == "application/octet-stream" { "image/jpeg" } else { detected };

        return Ok(crate::util::data_uri(mime_type, bytes));
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
        let file_data = std::fs::read(&file_path)
            .map_err(|e| format!("Failed to read file: {}", e))?;

        let img = ::image::load_from_memory(&file_data)
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
pub async fn file_message_compressed(receiver: String, replied_to: String, file_path: String, name_override: String) -> Result<MessageSendResult, String> {
    // Extract filename from the path
    let file_name = std::path::Path::new(&file_path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string();

    // Load the file as AttachmentFile
    let mut attachment_file = {
        #[cfg(not(target_os = "android"))]
        {
            let file_bytes = read_file_checked(&file_path)?;

            let extension = file_path
                .rsplit('.')
                .next()
                .unwrap_or("bin")
                .to_lowercase();

            AttachmentFile {
                bytes: Arc::new(file_bytes),
                img_meta: None,
                extension,
                name: file_name.clone(),
            }
        }
        #[cfg(target_os = "android")]
        {
            // First check if we have cached bytes for this URI
            // Take ownership from cache to avoid clone - bytes already Arc
            let mut cache = ANDROID_FILE_CACHE.lock().unwrap();
            if let Some((bytes, extension, cached_name, _)) = cache.remove(&file_path) {
                drop(cache);
                AttachmentFile {
                    bytes,
                    img_meta: None,
                    extension,
                    name: cached_name,
                }
            } else {
                drop(cache);
                // Check if this is a content:// URI or a regular file path
                if file_path.starts_with("content://") {
                    // Content URI - use Android ContentResolver
                    filesystem::read_android_uri(file_path)?
                } else {
                    // Regular file path (e.g., marketplace apps) - use standard file I/O
                    let file_bytes = read_file_checked(&file_path)?;

                    let extension = file_path
                        .rsplit('.')
                        .next()
                        .unwrap_or("bin")
                        .to_lowercase();

                    AttachmentFile {
                        bytes: Arc::new(file_bytes),
                        img_meta: None,
                        extension,
                        name: file_name.clone(),
                    }
                }
            }
        }
    };

    // Compress the image if it's a supported format
    if matches!(attachment_file.extension.as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp" | "tiff" | "tif" | "ico") {
        if let Ok(compressed) = compress_bytes_internal(attachment_file.bytes.clone(), &attachment_file.extension, None) {
            attachment_file.bytes = compressed.bytes;
            attachment_file.extension = compressed.extension;
            attachment_file.img_meta = compressed.img_meta;
        }
    }

    // Apply user-edited name override (if any)
    if !name_override.is_empty() {
        let sanitized = crate::commands::attachments::sanitize_filename(&name_override);
        if !sanitized.is_empty() { attachment_file.name = sanitized; }
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
    // Mark as "in progress" by inserting None, and create a notify for waiters
    {
        let mut cache = COMPRESSION_CACHE.lock().await;
        cache.insert(file_path.clone(), None);
    }
    {
        let mut notifiers = super::types::COMPRESSION_NOTIFY.lock().await;
        notifiers.insert(file_path.clone(), Arc::new(tokio::sync::Notify::new()));
    }

    // Spawn the compression task
    let path_clone = file_path.clone();
    tokio::spawn(async move {
        let result = compress_image_internal(&path_clone);
        let mut cache = COMPRESSION_CACHE.lock().await;

        // Only store if still in cache (not cancelled)
        if cache.contains_key(&path_clone) {
            cache.insert(path_clone.clone(), result.ok());
        }
        drop(cache);

        // Wake any waiters
        let notify = {
            let mut notifiers = super::types::COMPRESSION_NOTIFY.lock().await;
            notifiers.remove(&path_clone)
        };
        if let Some(n) = notify { n.notify_waiters(); }
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

// ─── Directory Zip & Send ───────────────────────────────────────────────────

/// Pending zip path for cleanup
static PENDING_ZIP_PATH: LazyLock<std::sync::Mutex<Option<String>>> =
    LazyLock::new(|| std::sync::Mutex::new(None));

/// Generation counter for zip_directory — each new zip increments this.
/// An in-progress zip aborts if its generation no longer matches the current one.
static ZIP_GENERATION: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// Result returned by zip_directory
#[derive(serde::Serialize)]
pub struct ZipDirectoryResult {
    pub zip_path: String,
    pub zip_name: String,
    pub compressed_size: u64,
    pub uncompressed_size: u64,
    pub file_count: u32,
    pub dir_count: u32,
    pub file_list: Vec<ZipEntry>,
}

/// A single entry in the zip file list
#[derive(serde::Serialize)]
pub struct ZipEntry {
    pub path: String,
    pub size: u64,
    pub is_dir: bool,
}

/// Check if a path is a directory (used by JS drag-drop)
#[tauri::command]
pub fn is_directory(path: String) -> bool {
    std::path::Path::new(&path).is_dir()
}

/// Zip a directory and return metadata about the result
#[tauri::command]
pub async fn zip_directory(dir_path: String) -> Result<ZipDirectoryResult, String> {
    // Claim a new generation — any previous zip will see a mismatch and abort
    let my_generation = ZIP_GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;

    // Run all sync I/O on a blocking thread to avoid tying up the async runtime
    tokio::task::spawn_blocking(move || {
        zip_directory_blocking(&dir_path, my_generation)
    }).await.map_err(|e| format!("Zip task failed: {}", e))?
}

fn zip_directory_blocking(dir_path: &str, my_generation: u64) -> Result<ZipDirectoryResult, String> {
    use std::io::{BufWriter, Write};
    use zip::write::SimpleFileOptions;
    use tauri::Emitter;
    use zip::CompressionMethod;

    let dir = std::path::Path::new(dir_path);
    if !dir.is_dir() {
        return Err("Path is not a directory".to_string());
    }

    let dir_name = dir.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("folder")
        .to_string();

    // Walk phase: collect all entries, sum total size
    const MAX_UNCOMPRESSED: u64 = 1_073_741_824; // 1GB
    // Entries: (path, is_dir, file_size)
    let mut entries: Vec<(std::path::PathBuf, bool, u64)> = Vec::new();
    let mut total_size: u64 = 0;
    const MAX_DEPTH: u32 = 128;

    fn walk_dir(
        base: &std::path::Path,
        current: &std::path::Path,
        entries: &mut Vec<(std::path::PathBuf, bool, u64)>,
        total_size: &mut u64,
        max: u64,
        depth: u32,
    ) -> Result<(), String> {
        if depth > MAX_DEPTH {
            return Err("Directory nesting too deep (>128 levels)".to_string());
        }

        let read_dir = std::fs::read_dir(current)
            .map_err(|e| format!("Failed to read directory {}: {}", current.display(), e))?;

        for entry in read_dir {
            let entry = entry.map_err(|e| format!("Failed to read entry: {}", e))?;
            let path = entry.path();

            // Skip symlinks silently (security — prevents traversal and cycles)
            let meta = match std::fs::symlink_metadata(&path) {
                Ok(m) => m,
                Err(_) => continue,
            };
            if meta.file_type().is_symlink() {
                continue;
            }

            if meta.is_dir() {
                entries.push((path.clone(), true, 0));
                walk_dir(base, &path, entries, total_size, max, depth + 1)?;
            } else if meta.is_file() {
                let size = meta.len();
                *total_size += size;
                if *total_size >= max {
                    return Err("Directory exceeds 1GB limit".to_string());
                }
                entries.push((path, false, size));
            }
        }
        Ok(())
    }

    walk_dir(dir, dir, &mut entries, &mut total_size, MAX_UNCOMPRESSED, 0)?;

    if entries.is_empty() {
        return Err("Directory is empty".to_string());
    }

    // Zip phase — byte-based progress for smooth updates
    // Use generation in filename to avoid collisions with previous cleanup_zip calls
    let zip_name = format!("{}.zip", dir_name);
    let temp_dir = std::env::temp_dir();
    let zip_path = temp_dir.join(format!("vector_zip_{}_{}", my_generation, &zip_name));

    // Run the zip phase, cleaning up the partial file on any error
    let result = (|| -> Result<(u32, u32, Vec<ZipEntry>), String> {
    let mut bytes_written: u64 = 0;
    let mut last_emitted_percent: u64 = 0;

    let file = std::fs::File::create(&zip_path)
        .map_err(|e| format!("Failed to create zip file: {}", e))?;
    let buf_writer = BufWriter::new(file);
    let mut zip_writer = zip::ZipWriter::new(buf_writer);

    // Level 1 (fastest deflate) — ~2-3x faster than default (6) with ~10-15% larger output
    let options: SimpleFileOptions = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .compression_level(Some(1));

    let mut file_list: Vec<ZipEntry> = Vec::new();
    let mut file_count: u32 = 0;
    let mut dir_count: u32 = 0;

    // Chunk size for intra-file progress (512KB)
    const CHUNK_SIZE: usize = 512 * 1024;

    for (path, is_dir, walked_size) in &entries {
        let rel_path = path.strip_prefix(dir)
            .map_err(|_| "Failed to compute relative path".to_string())?;
        let rel_str = rel_path.to_string_lossy().replace('\\', "/");

        if *is_dir {
            dir_count += 1;
            let dir_path_str = format!("{}/", rel_str);
            zip_writer.add_directory(&dir_path_str, options)
                .map_err(|e| format!("Failed to add directory: {}", e))?;

            if file_list.len() < 200 {
                file_list.push(ZipEntry {
                    path: dir_path_str,
                    size: 0,
                    is_dir: true,
                });
            }
        } else {
            // Check cancellation between files (covers small-file-heavy directories)
            if ZIP_GENERATION.load(std::sync::atomic::Ordering::Relaxed) != my_generation {
                drop(zip_writer);
                let _ = std::fs::remove_file(&zip_path);
                return Err("Cancelled".to_string());
            }

            file_count += 1;
            let file_size = *walked_size;

            // Re-verify not a symlink at zip time (TOCTOU mitigation)
            match std::fs::symlink_metadata(path) {
                Ok(m) if m.file_type().is_symlink() => continue,
                Err(_) => continue,
                _ => {}
            }

            zip_writer.start_file(&rel_str, options)
                .map_err(|e| format!("Failed to start file in zip: {}", e))?;

            // Handle empty files (nothing to write)
            if file_size == 0 {
                if file_list.len() < 200 {
                    file_list.push(ZipEntry {
                        path: rel_str.to_string(),
                        size: 0,
                        is_dir: false,
                    });
                }
                continue;
            }

            // Read file into memory, write in chunks for progress
            let file_data = std::fs::read(path)
                .map_err(|e| format!("Failed to read file {}: {}", path.display(), e))?;

            let data = &file_data[..];
            let mut offset = 0;
            while offset < data.len() {
                // Check if this zip has been superseded (cancelled or new zip started)
                if ZIP_GENERATION.load(std::sync::atomic::Ordering::Relaxed) != my_generation {
                    drop(zip_writer);
                    let _ = std::fs::remove_file(&zip_path);
                    return Err("Cancelled".to_string());
                }

                let end = (offset + CHUNK_SIZE).min(data.len());
                zip_writer.write_all(&data[offset..end])
                    .map_err(|e| format!("Failed to write to zip: {}", e))?;
                bytes_written += (end - offset) as u64;
                offset = end;

                // Emit progress (only when percent changes, only if still current generation)
                if total_size > 0 {
                    let percent = ((bytes_written * 100) / total_size).min(100);
                    if percent != last_emitted_percent {
                        last_emitted_percent = percent;
                        if ZIP_GENERATION.load(std::sync::atomic::Ordering::Relaxed) == my_generation {
                            if let Some(handle) = crate::TAURI_APP.get() {
                                let _ = handle.emit("zip_progress", serde_json::json!({
                                    "percent": percent,
                                }));
                            }
                        }
                    }
                }
            }

            if file_list.len() < 200 {
                file_list.push(ZipEntry {
                    path: rel_str.to_string(),
                    size: file_size,
                    is_dir: false,
                });
            }
        }
    }

    zip_writer.finish()
        .map_err(|e| format!("Failed to finalize zip: {}", e))?;

    Ok((file_count, dir_count, file_list))
    })(); // end of zip phase closure

    // On error, clean up partial zip file
    let (file_count, dir_count, file_list) = match result {
        Ok(v) => v,
        Err(e) => {
            let _ = std::fs::remove_file(&zip_path);
            return Err(e);
        }
    };

    let compressed_size = std::fs::metadata(&zip_path)
        .map(|m| m.len())
        .unwrap_or(0);

    let zip_path_str = zip_path.to_string_lossy().to_string();

    // Store path for cleanup
    *PENDING_ZIP_PATH.lock().unwrap_or_else(|e| e.into_inner()) = Some(zip_path_str.clone());

    Ok(ZipDirectoryResult {
        zip_path: zip_path_str,
        zip_name,
        compressed_size,
        uncompressed_size: total_size,
        file_count,
        dir_count,
        file_list,
    })
}

/// Cancel an in-progress zip and/or clean up the pending zip file
#[tauri::command]
pub fn cleanup_zip() -> Result<(), String> {
    // Bump generation to invalidate any running zip_directory
    ZIP_GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    // Also clean up the file if compression already finished
    let path = PENDING_ZIP_PATH.lock().unwrap_or_else(|e| e.into_inner()).take();
    if let Some(p) = path {
        let _ = std::fs::remove_file(&p);
    }
    Ok(())
}

/// Send a file using the cached compressed version if available
#[tauri::command]
pub async fn send_cached_compressed_file(receiver: String, replied_to: String, file_path: String, name_override: String) -> Result<MessageSendResult, String> {
    use super::compression::MIN_SAVINGS_PERCENT;

    // Extract filename from the path
    let file_name = std::path::Path::new(&file_path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string();

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
                let mut attachment_file = AttachmentFile {
                    bytes: compressed.bytes,
                    extension: compressed.extension,
                    img_meta: compressed.img_meta,
                    name: file_name,
                };
                if !name_override.is_empty() {
                    let sanitized = crate::commands::attachments::sanitize_filename(&name_override);
                    if !sanitized.is_empty() { attachment_file.name = sanitized; }
                }
                message(receiver, String::new(), replied_to, Some(attachment_file)).await
            } else {
                // No significant savings - send original file
                file_message(receiver, replied_to, file_path, name_override).await
            }
        }
        Some(None) => {
            // Still compressing — await the notify instead of polling
            let notify = {
                let notifiers = super::types::COMPRESSION_NOTIFY.lock().await;
                notifiers.get(&file_path).cloned()
            };
            if let Some(n) = notify {
                n.notified().await;
            }

            // Get the result
            let cached = {
                let mut cache = COMPRESSION_CACHE.lock().await;
                cache.remove(&file_path)
            };

            match cached {
                Some(Some(compressed)) => {
                    let savings_percent = if compressed.original_size > 0 && compressed.compressed_size < compressed.original_size {
                        ((compressed.original_size - compressed.compressed_size) * 100) / compressed.original_size
                    } else {
                        0
                    };

                    if savings_percent >= MIN_SAVINGS_PERCENT {
                        let mut attachment_file = AttachmentFile {
                            bytes: compressed.bytes,
                            extension: compressed.extension,
                            img_meta: compressed.img_meta,
                            name: file_name,
                        };
                        if !name_override.is_empty() {
                            let sanitized = crate::commands::attachments::sanitize_filename(&name_override);
                            if !sanitized.is_empty() { attachment_file.name = sanitized; }
                        }
                        message(receiver, String::new(), replied_to, Some(attachment_file)).await
                    } else {
                        file_message(receiver, replied_to, file_path, name_override).await
                    }
                }
                _ => {
                    // Cache was cleared or missing — fall back to compressing now
                    file_message_compressed(receiver, replied_to, file_path, name_override).await
                }
            }
        }
        None => {
            // Not in cache, compress now
            file_message_compressed(receiver, replied_to, file_path, name_override).await
        }
    }
}
