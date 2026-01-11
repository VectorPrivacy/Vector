//! Image Cache Module
//!
//! Handles caching of user avatars, banners, and Mini App icons for offline
//! support and graceful fallback when images fail to load.
//!
//! ## Storage Structure
//! ```
//! AppData/cache/
//!   avatars/{hash}.{ext}
//!   banners/{hash}.{ext}
//!   miniapp_icons/{hash}.{ext}
//! ```
//!
//! Images are stored globally (not per-account) to enable deduplication across
//! accounts - if multiple accounts have the same contact, they share the cached image.
//! The original URL is hashed with SHA-256 (truncated) to create the filename.

use std::path::PathBuf;
use std::time::Duration;
use sha2::{Sha256, Digest};
use tauri::{AppHandle, Runtime, Manager};
use tokio::sync::Semaphore;
use once_cell::sync::Lazy;
use log::{info, warn, debug};

/// Maximum concurrent image downloads
static DOWNLOAD_SEMAPHORE: Lazy<Semaphore> = Lazy::new(|| Semaphore::new(4));

/// HTTP client for downloading images
static HTTP_CLIENT: Lazy<reqwest::Client> = Lazy::new(|| {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .connect_timeout(Duration::from_secs(10))
        .user_agent("Vector/1.0")
        .build()
        .expect("Failed to create HTTP client")
});

/// Supported image types for validation
const VALID_IMAGE_SIGNATURES: &[(&[u8], &str)] = &[
    // PNG: 89 50 4E 47 0D 0A 1A 0A
    (&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A], "png"),
    // JPEG: FF D8 FF
    (&[0xFF, 0xD8, 0xFF], "jpg"),
    // GIF: 47 49 46 38
    (&[0x47, 0x49, 0x46, 0x38], "gif"),
    // WebP: 52 49 46 46 ... 57 45 42 50
    (&[0x52, 0x49, 0x46, 0x46], "webp"),
    // BMP: 42 4D
    (&[0x42, 0x4D], "bmp"),
    // ICO: 00 00 01 00
    (&[0x00, 0x00, 0x01, 0x00], "ico"),
];

/// Type of cached image
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageType {
    Avatar,
    Banner,
    MiniAppIcon,
}

impl ImageType {
    /// Get the subdirectory name for this image type
    pub fn subdir(&self) -> &'static str {
        match self {
            ImageType::Avatar => "avatars",
            ImageType::Banner => "banners",
            ImageType::MiniAppIcon => "miniapp_icons",
        }
    }
}

/// Result of a cache operation
#[derive(Debug, Clone)]
pub enum CacheResult {
    /// Image was cached successfully, returns local path
    Cached(String),
    /// Image already exists in cache, returns local path
    AlreadyCached(String),
    /// Failed to cache (invalid image, network error, etc.)
    Failed(String),
}

/// Get the cache directory for a specific image type
/// Cache is stored globally (not per-account) for deduplication
pub fn get_cache_dir<R: Runtime>(
    handle: &AppHandle<R>,
    image_type: ImageType,
) -> Result<PathBuf, String> {
    let app_data = handle.path().app_data_dir()
        .map_err(|e| format!("Failed to get app data directory: {}", e))?;
    let cache_dir = app_data.join("cache").join(image_type.subdir());

    if !cache_dir.exists() {
        std::fs::create_dir_all(&cache_dir)
            .map_err(|e| format!("Failed to create cache directory: {}", e))?;
    }

    Ok(cache_dir)
}

/// Generate a cache filename from a URL
/// Uses first 16 bytes of SHA-256 hash (32 hex chars) for uniqueness
fn url_to_cache_key(url: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(url.as_bytes());
    let result = hasher.finalize();
    // Use first 16 bytes for a shorter but still unique filename
    hex::encode(&result[..16])
}

/// Validate image bytes and detect format
/// Returns the detected extension if valid, None if invalid/corrupted
fn validate_image(bytes: &[u8]) -> Option<&'static str> {
    if bytes.len() < 8 {
        return None;
    }

    for (signature, ext) in VALID_IMAGE_SIGNATURES {
        if bytes.starts_with(signature) {
            // Special case for WebP: need to check for WEBP at offset 8
            if *ext == "webp" {
                if bytes.len() >= 12 && &bytes[8..12] == b"WEBP" {
                    return Some(ext);
                }
                continue;
            }
            return Some(ext);
        }
    }

    // Also accept SVG (text-based)
    if bytes.len() > 5 {
        let start = String::from_utf8_lossy(&bytes[..std::cmp::min(256, bytes.len())]);
        if start.contains("<svg") || (start.contains("<?xml") && start.contains("<svg")) {
            return Some("svg");
        }
    }

    None
}

/// Check if a cached image exists and return its path
pub fn get_cached_path<R: Runtime>(
    handle: &AppHandle<R>,
    url: &str,
    image_type: ImageType,
) -> Option<String> {
    if url.is_empty() {
        return None;
    }

    let cache_dir = get_cache_dir(handle, image_type).ok()?;
    let cache_key = url_to_cache_key(url);

    // Check for any file with this cache key (any extension)
    if let Ok(entries) = std::fs::read_dir(&cache_dir) {
        for entry in entries.flatten() {
            let filename = entry.file_name().to_string_lossy().to_string();
            if filename.starts_with(&cache_key) {
                return Some(entry.path().to_string_lossy().to_string());
            }
        }
    }

    None
}

/// Download and cache an image from a URL
/// Returns the local file path if successful
pub async fn cache_image<R: Runtime>(
    handle: &AppHandle<R>,
    url: &str,
    image_type: ImageType,
) -> CacheResult {
    if url.is_empty() {
        return CacheResult::Failed("Empty URL".to_string());
    }

    // Check if already cached
    if let Some(path) = get_cached_path(handle, url, image_type) {
        return CacheResult::AlreadyCached(path);
    }

    // Acquire semaphore permit to limit concurrent downloads
    let _permit = DOWNLOAD_SEMAPHORE.acquire().await
        .map_err(|e| format!("Semaphore error: {}", e));

    if _permit.is_err() {
        return CacheResult::Failed("Failed to acquire download permit".to_string());
    }

    // Download the image
    debug!("[ImageCache] Downloading {} for {:?}", url, image_type);

    let response = match HTTP_CLIENT.get(url).send().await {
        Ok(resp) => resp,
        Err(e) => {
            warn!("[ImageCache] Failed to download {}: {}", url, e);
            return CacheResult::Failed(format!("Download failed: {}", e));
        }
    };

    if !response.status().is_success() {
        return CacheResult::Failed(format!("HTTP {}", response.status()));
    }

    // Check content length to avoid downloading huge files
    if let Some(len) = response.content_length() {
        // Max 10MB for images
        if len > 10 * 1024 * 1024 {
            return CacheResult::Failed("Image too large (>10MB)".to_string());
        }
    }

    let bytes = match response.bytes().await {
        Ok(b) => b,
        Err(e) => {
            return CacheResult::Failed(format!("Failed to read response: {}", e));
        }
    };

    // Validate the image
    let extension = match validate_image(&bytes) {
        Some(ext) => ext,
        None => {
            warn!("[ImageCache] Invalid image data from {}", url);
            return CacheResult::Failed("Invalid or corrupted image".to_string());
        }
    };

    // Get cache directory and create filename
    let cache_dir = match get_cache_dir(handle, image_type) {
        Ok(dir) => dir,
        Err(e) => return CacheResult::Failed(e),
    };

    let cache_key = url_to_cache_key(url);
    let filename = format!("{}.{}", cache_key, extension);
    let file_path = cache_dir.join(&filename);

    // Write the file
    if let Err(e) = std::fs::write(&file_path, &bytes) {
        return CacheResult::Failed(format!("Failed to write cache file: {}", e));
    }

    let path_str = file_path.to_string_lossy().to_string();
    info!("[ImageCache] Cached {} -> {}", url, path_str);

    CacheResult::Cached(path_str)
}

/// Cache an avatar for a user profile
pub async fn cache_avatar<R: Runtime>(
    handle: &AppHandle<R>,
    avatar_url: &str,
) -> CacheResult {
    cache_image(handle, avatar_url, ImageType::Avatar).await
}

/// Cache a banner for a user profile
pub async fn cache_banner<R: Runtime>(
    handle: &AppHandle<R>,
    banner_url: &str,
) -> CacheResult {
    cache_image(handle, banner_url, ImageType::Banner).await
}

/// Cache a Mini App icon
#[allow(dead_code)] // Available for future Mini App icon caching
pub async fn cache_miniapp_icon<R: Runtime>(
    handle: &AppHandle<R>,
    icon_url: &str,
) -> CacheResult {
    cache_image(handle, icon_url, ImageType::MiniAppIcon).await
}

/// Remove a cached image (e.g., when user changes their avatar)
#[allow(dead_code)] // Available for cache invalidation when avatars change
pub fn remove_cached_image<R: Runtime>(
    handle: &AppHandle<R>,
    url: &str,
    image_type: ImageType,
) -> Result<(), String> {
    if let Some(path) = get_cached_path(handle, url, image_type) {
        std::fs::remove_file(&path)
            .map_err(|e| format!("Failed to remove cached image: {}", e))?;
        info!("[ImageCache] Removed cached image: {}", path);
    }
    Ok(())
}

/// Clear all cached images of a specific type
pub fn clear_cache<R: Runtime>(
    handle: &AppHandle<R>,
    image_type: ImageType,
) -> Result<u64, String> {
    let cache_dir = get_cache_dir(handle, image_type)?;
    let mut count = 0;

    if let Ok(entries) = std::fs::read_dir(&cache_dir) {
        for entry in entries.flatten() {
            if entry.path().is_file() {
                if std::fs::remove_file(entry.path()).is_ok() {
                    count += 1;
                }
            }
        }
    }

    info!("[ImageCache] Cleared {} {:?} images", count, image_type);
    Ok(count)
}

/// Get total cache size in bytes
pub fn get_cache_size<R: Runtime>(
    handle: &AppHandle<R>,
) -> Result<u64, String> {
    let app_data = handle.path().app_data_dir()
        .map_err(|e| format!("Failed to get app data directory: {}", e))?;
    let cache_dir = app_data.join("cache");

    if !cache_dir.exists() {
        return Ok(0);
    }

    fn dir_size(path: &PathBuf) -> u64 {
        let mut size = 0;
        if let Ok(entries) = std::fs::read_dir(path) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() {
                    size += entry.metadata().map(|m| m.len()).unwrap_or(0);
                } else if path.is_dir() {
                    size += dir_size(&path);
                }
            }
        }
        size
    }

    Ok(dir_size(&cache_dir))
}

/// Tauri command: Get the cached path for an image, or download and cache it
#[tauri::command]
pub async fn get_or_cache_image<R: Runtime>(
    handle: AppHandle<R>,
    url: String,
    image_type: String,
) -> Result<Option<String>, String> {
    let img_type = match image_type.as_str() {
        "avatar" => ImageType::Avatar,
        "banner" => ImageType::Banner,
        "miniapp_icon" => ImageType::MiniAppIcon,
        _ => return Err("Invalid image type".to_string()),
    };

    match cache_image(&handle, &url, img_type).await {
        CacheResult::Cached(path) | CacheResult::AlreadyCached(path) => Ok(Some(path)),
        CacheResult::Failed(e) => {
            warn!("[ImageCache] Failed to cache {}: {}", url, e);
            Ok(None)
        }
    }
}

/// Tauri command: Clear all image caches
#[tauri::command]
pub async fn clear_image_cache<R: Runtime>(
    handle: AppHandle<R>,
) -> Result<u64, String> {
    let mut total = 0;
    total += clear_cache(&handle, ImageType::Avatar)?;
    total += clear_cache(&handle, ImageType::Banner)?;
    total += clear_cache(&handle, ImageType::MiniAppIcon)?;
    Ok(total)
}

/// Tauri command: Get cache statistics
#[tauri::command]
pub async fn get_image_cache_stats<R: Runtime>(
    handle: AppHandle<R>,
) -> Result<serde_json::Value, String> {
    let size = get_cache_size(&handle)?;

    // Count files per type
    let mut avatar_count = 0;
    let mut banner_count = 0;
    let mut icon_count = 0;

    if let Ok(dir) = get_cache_dir(&handle, ImageType::Avatar) {
        avatar_count = std::fs::read_dir(dir).map(|e| e.count()).unwrap_or(0);
    }
    if let Ok(dir) = get_cache_dir(&handle, ImageType::Banner) {
        banner_count = std::fs::read_dir(dir).map(|e| e.count()).unwrap_or(0);
    }
    if let Ok(dir) = get_cache_dir(&handle, ImageType::MiniAppIcon) {
        icon_count = std::fs::read_dir(dir).map(|e| e.count()).unwrap_or(0);
    }

    Ok(serde_json::json!({
        "total_size_bytes": size,
        "avatar_count": avatar_count,
        "banner_count": banner_count,
        "miniapp_icon_count": icon_count,
    }))
}
