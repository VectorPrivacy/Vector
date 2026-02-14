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
use tauri::{AppHandle, Runtime, Manager, Emitter};
use tokio::sync::Semaphore;
use once_cell::sync::Lazy;
use log::{info, warn, debug};
use serde_json::json;

use crate::net::{ProgressReporter, download_with_reporter};
use crate::util::bytes_to_hex_string;
use std::collections::HashSet;
use tokio::sync::Mutex;

/// Maximum concurrent image downloads
static DOWNLOAD_SEMAPHORE: Lazy<Semaphore> = Lazy::new(|| Semaphore::new(4));

/// Track URLs currently being downloaded to prevent duplicate downloads
/// (e.g., when messages re-render from Pending to Sent)
static DOWNLOADS_IN_PROGRESS: Lazy<Mutex<HashSet<String>>> = Lazy::new(|| Mutex::new(HashSet::new()));

/// Maximum entries in DOWNLOADS_IN_PROGRESS before forced cleanup
const MAX_IN_PROGRESS_ENTRIES: usize = 100;

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
    /// Inline images from URLs posted in chat messages
    InlineImage,
}

impl ImageType {
    /// Get the subdirectory name for this image type
    pub fn subdir(&self) -> &'static str {
        match self {
            ImageType::Avatar => "avatars",
            ImageType::Banner => "banners",
            ImageType::MiniAppIcon => "miniapp_icons",
            ImageType::InlineImage => "inline_images",
        }
    }
}

/// Progress reporter for inline image downloads
/// Emits events to frontend with download progress
pub struct InlineImageProgressReporter<'a, R: Runtime> {
    handle: &'a AppHandle<R>,
    url: String,
}

impl<'a, R: Runtime> InlineImageProgressReporter<'a, R> {
    pub fn new(handle: &'a AppHandle<R>, url: &str) -> Self {
        Self {
            handle,
            url: url.to_string(),
        }
    }
}

impl<R: Runtime> ProgressReporter for InlineImageProgressReporter<'_, R> {
    fn report_progress(&self, percentage: Option<u8>, bytes_downloaded: Option<u64>) -> Result<(), &'static str> {
        let mut payload = json!({
            "url": self.url
        });

        if let Some(p) = percentage {
            payload["progress"] = json!(p);
        } else {
            payload["progress"] = json!(-1); // Indeterminate
        }

        if let Some(bytes) = bytes_downloaded {
            payload["bytesDownloaded"] = json!(bytes);
        }

        self.handle
            .emit("inline_image_progress", payload)
            .map_err(|_| "Failed to emit event")
    }

    fn report_complete(&self) -> Result<(), &'static str> {
        self.handle
            .emit(
                "inline_image_progress",
                json!({
                    "url": self.url,
                    "progress": 100
                }),
            )
            .map_err(|_| "Failed to emit event")
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
    bytes_to_hex_string(&result[..16])
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

    // Also accept SVG (text-based) - search bytes directly to avoid allocation
    if bytes.len() > 5 {
        let check_len = std::cmp::min(512, bytes.len());
        let header = &bytes[..check_len];

        // Helper to find a pattern in bytes
        fn contains_pattern(haystack: &[u8], needle: &[u8]) -> bool {
            haystack.windows(needle.len()).any(|w| w == needle)
        }

        if contains_pattern(header, b"<svg") {
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
            let filename = entry.file_name();
            let filename_str = filename.to_string_lossy();
            if filename_str.starts_with(&cache_key) {
                return Some(entry.path().to_string_lossy().into_owned());
            }
        }
    }

    None
}

/// Pre-cache image bytes we already have (e.g., after uploading)
/// This avoids re-downloading an image we just uploaded
/// Returns the local file path if successful
pub fn precache_image_bytes<R: Runtime>(
    handle: &AppHandle<R>,
    url: &str,
    bytes: &[u8],
    image_type: ImageType,
) -> CacheResult {
    if url.is_empty() {
        return CacheResult::Failed("Empty URL".to_string());
    }

    // Check if already cached
    if let Some(path) = get_cached_path(handle, url, image_type) {
        return CacheResult::AlreadyCached(path);
    }

    // Validate the image bytes
    let extension = match validate_image(bytes) {
        Some(ext) => ext,
        None => {
            warn!("[ImageCache] Invalid image data for precache: {}", url);
            return CacheResult::Failed("Invalid image format".to_string());
        }
    };

    // Get cache directory
    let cache_dir = match get_cache_dir(handle, image_type) {
        Ok(d) => d,
        Err(e) => return CacheResult::Failed(e),
    };

    // Create filename from URL hash
    let cache_key = url_to_cache_key(url);
    let filename = format!("{}.{}", cache_key, extension);
    let file_path = cache_dir.join(&filename);

    // Write the file
    if let Err(e) = std::fs::write(&file_path, bytes) {
        warn!("[ImageCache] Failed to write precache file: {}", e);
        return CacheResult::Failed(format!("Failed to write: {}", e));
    }

    let path_str = file_path.to_string_lossy().to_string();
    info!("[ImageCache] Pre-cached {:?} {} -> {}", image_type, url, path_str);

    CacheResult::Cached(path_str)
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
    total += clear_cache(&handle, ImageType::InlineImage)?;
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
    let mut inline_count = 0;

    if let Ok(dir) = get_cache_dir(&handle, ImageType::Avatar) {
        avatar_count = std::fs::read_dir(dir).map(|e| e.count()).unwrap_or(0);
    }
    if let Ok(dir) = get_cache_dir(&handle, ImageType::Banner) {
        banner_count = std::fs::read_dir(dir).map(|e| e.count()).unwrap_or(0);
    }
    if let Ok(dir) = get_cache_dir(&handle, ImageType::MiniAppIcon) {
        icon_count = std::fs::read_dir(dir).map(|e| e.count()).unwrap_or(0);
    }
    if let Ok(dir) = get_cache_dir(&handle, ImageType::InlineImage) {
        inline_count = std::fs::read_dir(dir).map(|e| e.count()).unwrap_or(0);
    }

    Ok(serde_json::json!({
        "total_size_bytes": size,
        "avatar_count": avatar_count,
        "banner_count": banner_count,
        "miniapp_icon_count": icon_count,
        "inline_image_count": inline_count,
    }))
}

/// Check if a URL uses HTTPS (required for inline images for security)
fn is_https_url(url: &str) -> bool {
    url.starts_with("https://")
}

/// Check if a hostname resolves to a private/internal IP range
/// Blocks SSRF attacks by preventing requests to internal networks
fn is_private_host(host: &str) -> bool {
    // Block obvious private hostnames
    let host_lower = host.to_lowercase();
    if host_lower == "localhost" || host_lower.ends_with(".local") || host_lower.ends_with(".internal") {
        return true;
    }

    // Try to parse as IP address directly
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        return is_private_ip(ip);
    }

    // For hostnames, we can't resolve DNS here synchronously
    // The HTTP client will handle it, but we block obvious patterns
    false
}

/// Check if an IP address is in a private/reserved range
fn is_private_ip(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(ipv4) => {
            ipv4.is_loopback()           // 127.0.0.0/8
                || ipv4.is_private()     // 10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16
                || ipv4.is_link_local()  // 169.254.0.0/16
                || ipv4.is_broadcast()   // 255.255.255.255
                || ipv4.is_unspecified() // 0.0.0.0
        }
        std::net::IpAddr::V6(ipv6) => {
            ipv6.is_loopback()           // ::1
                || ipv6.is_unspecified() // ::
        }
    }
}

/// Cleanup stale entries from DOWNLOADS_IN_PROGRESS
/// Called periodically from maintenance to prevent unbounded growth
pub async fn cleanup_stale_downloads() {
    let mut in_progress = DOWNLOADS_IN_PROGRESS.lock().await;
    if in_progress.len() > MAX_IN_PROGRESS_ENTRIES {
        // If we have too many entries, something is wrong - clear them all
        warn!("[ImageCache] Clearing {} stale in-progress entries", in_progress.len());
        in_progress.clear();
    }
}

/// Tauri command: Cache an image URL from chat messages with progress reporting
/// Downloads, validates, and caches the image, returning the local path
/// Emits "inline_image_progress" events with { id, progress, bytesDownloaded }
///
/// Security: Only HTTPS URLs from non-private hosts are allowed.
/// URL format validation is done in JS to avoid IPC overhead for invalid URLs.
#[tauri::command]
pub async fn cache_url_image<R: Runtime>(
    handle: AppHandle<R>,
    url: String,
) -> Result<Option<String>, String> {
    // Security: Only allow HTTPS URLs
    if !is_https_url(&url) {
        return Err("Only HTTPS URLs are allowed for inline images".to_string());
    }

    // Security: Block private/internal hosts (SSRF protection)
    if let Ok(parsed) = url::Url::parse(&url) {
        if let Some(host) = parsed.host_str() {
            if is_private_host(host) {
                return Err("Private/internal hosts are not allowed".to_string());
            }
        }
    }

    // Check if already cached - skip download if so
    if let Some(path) = get_cached_path(&handle, &url, ImageType::InlineImage) {
        return Ok(Some(path));
    }

    // Check if this URL is already being downloaded (e.g., message re-rendered from Pending to Sent)
    // If so, return None - the frontend will receive the cached path via the inline_image_cached event
    {
        let mut in_progress = DOWNLOADS_IN_PROGRESS.lock().await;
        if in_progress.contains(&url) {
            debug!("[ImageCache] Download already in progress, frontend will get path via event: {}", url);
            return Ok(None);
        }
        // Mark as in-progress (clone here is necessary for the set)
        in_progress.insert(url.clone());
    }

    // Use a scope guard pattern - url is moved into cleanup, avoiding extra clone
    let cleanup_url = url.clone(); // One clone for cleanup
    let cleanup = || async move {
        DOWNLOADS_IN_PROGRESS.lock().await.remove(&cleanup_url);
    };

    // Acquire semaphore permit to limit concurrent downloads
    let _permit = match DOWNLOAD_SEMAPHORE.acquire().await {
        Ok(p) => p,
        Err(e) => {
            cleanup().await;
            return Err(format!("Semaphore error: {}", e));
        }
    };

    // Create progress reporter for this download
    let reporter = InlineImageProgressReporter::new(&handle, &url);

    // Helper to emit failure event so frontend can remove spinner
    let emit_failure = |handle: &AppHandle<R>, url: &str| {
        handle.emit("inline_image_cached", json!({
            "url": url,
            "path": serde_json::Value::Null
        })).ok();
    };

    // Download with progress reporting (10s timeout)
    debug!("[ImageCache] Downloading inline image with progress: {}", url);
    let bytes = match download_with_reporter(&url, &reporter, Some(Duration::from_secs(10))).await {
        Ok(b) => b,
        Err(e) => {
            cleanup().await;
            warn!("[ImageCache] Failed to download inline image {}: {}", url, e);
            emit_failure(&handle, &url);
            return Ok(None);
        }
    };

    // Validate the image
    let extension = match validate_image(&bytes) {
        Some(ext) => ext,
        None => {
            cleanup().await;
            warn!("[ImageCache] Invalid image data from {}", url);
            emit_failure(&handle, &url);
            return Ok(None);
        }
    };

    // Get cache directory and create filename
    let cache_dir = match get_cache_dir(&handle, ImageType::InlineImage) {
        Ok(d) => d,
        Err(e) => {
            cleanup().await;
            return Err(e);
        }
    };
    let cache_key = url_to_cache_key(&url);
    let filename = format!("{}.{}", cache_key, extension);
    let file_path = cache_dir.join(&filename);

    // Write the file
    if let Err(e) = std::fs::write(&file_path, &bytes) {
        cleanup().await;
        warn!("[ImageCache] Failed to write cache file: {}", e);
        return Ok(None);
    }

    // Done - remove from in-progress
    cleanup().await;

    let path_str = file_path.to_string_lossy().to_string();
    info!("[ImageCache] Cached inline image {} -> {}", url, path_str);

    // Emit completion event so ALL loading indicators for this URL get updated
    // (handles cases like message re-rendering or multiple instances of same image)
    handle.emit("inline_image_cached", json!({
        "url": url,
        "path": path_str.clone()
    })).ok();

    Ok(Some(path_str))
}
