//! File handling commands.
//!
//! This module handles:
//! - File caching from JavaScript/WebView
//! - File sending (compressed and uncompressed)
//! - Image preview generation
//! - Android file handling

use std::sync::Arc;
use tokio::sync::Mutex as TokioMutex;
use std::sync::LazyLock;

use crate::util;
use crate::shared::image::read_file_checked;

use super::types::{CachedCompressedImage, AttachmentFile, COMPRESSION_CACHE, ANDROID_FILE_CACHE};
use super::compression::{compress_bytes_internal, compress_image_internal};
use super::sending::{message, MessageSendResult};

#[cfg(target_os = "android")]
use crate::android::filesystem;

/// Cache for bytes received from JavaScript (for Android file handling)
pub(crate) static JS_FILE_CACHE: LazyLock<std::sync::Mutex<Option<(Arc<Vec<u8>>, String, String)>>> =
    LazyLock::new(|| std::sync::Mutex::new(None));

/// Cache for compressed bytes from JavaScript file
pub(crate) static JS_COMPRESSION_CACHE: LazyLock<TokioMutex<Option<CachedCompressedImage>>> =
    LazyLock::new(|| TokioMutex::new(None));

/// Longest side of a composer preview. Enough for a retina overlay, a few hundred KB
/// at most, and the webview never decodes the original's full resolution for it.
const PREVIEW_MAX_DIM: u32 = 1024;
const PREVIEW_JPEG_QUALITY: u8 = 70;
/// A GIF is kept verbatim so the preview animates, but only a small one: past this it
/// is decoded like a photo and the preview is its first frame.
const PREVIEW_GIF_VERBATIM_MAX: usize = 4 * 1024 * 1024;
/// Previews outlive one preview dialog only by accident; anything this old is litter.
const PREVIEW_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);
static PREVIEW_TMP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn is_previewable_extension(extension: &str) -> bool {
    matches!(extension, "png" | "jpg" | "jpeg" | "gif" | "webp" | "tiff" | "tif" | "ico")
}

fn preview_cache_dir(app: &tauri::AppHandle) -> Result<std::path::PathBuf, String> {
    use tauri::Manager;
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("Failed to get app data directory: {}", e))?
        .join("cache")
        .join("previews");
    std::fs::create_dir_all(&dir).map_err(|e| format!("Failed to create preview cache: {}", e))?;
    Ok(dir)
}

/// Write a downscaled copy of an image into the preview cache and return its path.
/// Blocking CPU work: call from `spawn_blocking`, never inline in a command.
pub(crate) fn write_preview_file(app: &tauri::AppHandle, bytes: &[u8]) -> Result<String, String> {
    let dir = preview_cache_dir(app)?;
    // The directory stays a handful of files, so sweeping it on every write is cheaper
    // than letting cancelled previews sit until the next boot.
    prune_dir(&dir);
    write_preview_into(&dir, bytes).map(|p| p.to_string_lossy().into_owned())
}

/// The preview itself: a small GIF is copied verbatim so it animates; everything else is
/// bounded to `PREVIEW_MAX_DIM` and re-encoded (PNG when transparent, JPEG otherwise).
/// Keyed by content hash, so re-previewing the same bytes is a stat, not a decode.
fn write_preview_into(dir: &std::path::Path, bytes: &[u8]) -> Result<std::path::PathBuf, String> {
    use crate::shared::image::{animated_dims, encode_rgba_auto};
    use sha2::{Digest, Sha256};

    let key = crate::util::bytes_to_hex_string(&Sha256::digest(bytes)[..16]);
    let verbatim_gif = crate::util::mime_from_magic_bytes(bytes) == "image/gif"
        && bytes.len() <= PREVIEW_GIF_VERBATIM_MAX
        && animated_dims(bytes).is_some_and(|(w, h)| w.max(h) <= PREVIEW_MAX_DIM);
    // encode_rgba_auto picks PNG or JPEG from the pixels, so the extension is only known
    // after encoding; a hit on either spelling is the same preview.
    for ext in if verbatim_gif { &["gif"][..] } else { &["jpg", "png"][..] } {
        let hit = dir.join(format!("{key}.{ext}"));
        if hit.is_file() {
            // A hit is a use: keep it out of the age sweep while the overlay may show it.
            if let Ok(f) = std::fs::File::open(&hit) {
                let _ = f.set_modified(std::time::SystemTime::now());
            }
            return Ok(hit);
        }
    }

    let (ext, out): (&str, std::borrow::Cow<[u8]>) = if verbatim_gif {
        ("gif", std::borrow::Cow::Borrowed(bytes))
    } else {
        let img = vector_core::crypto::decode_image_bounded(bytes)?;
        let (w, h) = (img.width(), img.height());
        let scale = (PREVIEW_MAX_DIM as f32 / w.max(h) as f32).min(1.0);
        let (nw, nh) = (((w as f32 * scale) as u32).max(1), ((h as f32 * scale) as u32).max(1));
        let (rgba, ow, oh) = crate::simd::image::fast_resize_to_rgba(&img, nw, nh);
        let encoded = encode_rgba_auto(&rgba, ow, oh, PREVIEW_JPEG_QUALITY)?;
        (encoded.extension, std::borrow::Cow::Owned(encoded.bytes))
    };

    let path = dir.join(format!("{key}.{ext}"));
    let seq = PREVIEW_TMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = dir.join(format!("{key}.{ext}.tmp-{}-{seq}", std::process::id()));
    std::fs::write(&tmp, &out).map_err(|e| format!("Failed to write preview: {}", e))?;
    if let Err(e) = std::fs::rename(&tmp, &path) {
        let _ = std::fs::remove_file(&tmp);
        // A concurrent writer of the same key already placed identical content.
        if !path.is_file() {
            return Err(format!("Failed to place preview: {}", e));
        }
    }
    Ok(path)
}

/// Delete stale composer previews. Runs with the other cache sweeps at boot.
pub fn prune_preview_cache(app: &tauri::AppHandle) -> usize {
    let Ok(dir) = preview_cache_dir(app) else { return 0 };
    prune_dir(&dir)
}

fn prune_dir(dir: &std::path::Path) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else { return 0 };
    let now = std::time::SystemTime::now();
    let mut removed = 0;
    for entry in entries.flatten() {
        let stale = entry
            .metadata()
            .and_then(|m| m.modified())
            .map(|t| now.duration_since(t).unwrap_or_default() > PREVIEW_MAX_AGE)
            .unwrap_or(false);
        if stale && std::fs::remove_file(entry.path()).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// The preview for the file the composer just cached: the clipboard-paste bytes when
/// `file_path` is empty, else the Android pick cached under that content URI. Separate
/// from the caching commands so the decode runs off the IPC thread.
#[tauri::command]
pub async fn preview_cached_file(app: tauri::AppHandle, file_path: String) -> Result<Option<String>, String> {
    let cached: Option<(Arc<Vec<u8>>, String)> = if file_path.is_empty() {
        JS_FILE_CACHE.lock().unwrap().as_ref().map(|(b, _, ext)| (b.clone(), ext.clone()))
    } else {
        ANDROID_FILE_CACHE.lock().unwrap().get(&file_path).map(|(b, ext, _, _)| (b.clone(), ext.clone()))
    };
    let Some((bytes, ext)) = cached else { return Ok(None) };
    if !is_previewable_extension(&ext) {
        return Ok(None);
    }
    tokio::task::spawn_blocking(move || write_preview_file(&app, &bytes).map(Some))
        .await
        .map_err(|e| e.to_string())?
}

/// Response from caching file bytes. The preview is a second call
/// (`preview_cached_file`) so the decode never runs on the IPC thread.
#[derive(serde::Serialize)]
pub struct CacheFileBytesResult {
    pub size: u64,
    pub name: String,
    pub extension: String,
}

/// Cache file bytes received from JavaScript (for Android)
/// This is called immediately when a file is selected via the WebView file input
/// Returns file info and a thumbnail preview for images
#[tauri::command]
pub fn cache_file_bytes(request: tauri::ipc::Request<'_>) -> Result<CacheFileBytesResult, String> {
    // Raw-bytes IPC: file in the binary body; name (may be unicode, so base64'd)
    // + extension in headers.
    let bytes = crate::shared::ipc::raw_body(&request)?;
    let file_name = crate::shared::ipc::header_b64(&request, "file-name").unwrap_or_default();
    let extension = crate::shared::ipc::header(&request, "extension").unwrap_or_default();
    let size = bytes.len() as u64;

    let bytes = Arc::new(bytes);

    let mut cache = JS_FILE_CACHE.lock().unwrap();
    *cache = Some((bytes, file_name.clone(), extension.clone()));

    Ok(CacheFileBytesResult {
        size,
        name: file_name,
        extension,
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

/// Generate a thumbhash data-URL from an image: the file at `file_path`, or the
/// JS byte cache (Android / clipboard paste) when the path is empty. The path wins
/// when given: the cache may still hold an earlier paste.
#[tauri::command]
pub fn generate_thumbhash_for_preview(file_path: String) -> Result<String, String> {
    let img = if file_path.is_empty() {
        let cache = JS_FILE_CACHE.lock().unwrap();
        let (bytes, _, _) = cache.as_ref().ok_or("No cached file and no file path provided")?;
        vector_core::crypto::decode_image_bounded(bytes)
            .map_err(|e| format!("Failed to decode cached image: {}", e))?
    } else {
        ::image::open(&file_path)
            .map_err(|e| format!("Failed to open image: {}", e))?
    };

    let thumbhash = util::generate_thumbhash_from_image(&img)
        .ok_or_else(|| "Failed to generate thumbhash".to_string())?;
    Ok(util::decode_thumbhash_to_base64(&thumbhash))
}

/// Whether the previewed image carries strip-worthy EXIF metadata, so the UI can
/// hide the "Keep Metadata" toggle for screenshots/memes that have none. An empty
/// `file_path` checks the JS-cached bytes (clipboard / File-object sends).
#[tauri::command]
pub fn file_has_metadata(file_path: String) -> Result<bool, String> {
    if file_path.is_empty() {
        let cache = JS_FILE_CACHE.lock().unwrap();
        Ok(match cache.as_ref() {
            Some((bytes, _, ext)) => crate::shared::image::image_bytes_have_metadata(bytes.as_slice(), ext),
            None => false,
        })
    } else {
        let ext = file_path.rsplit('.').next().unwrap_or("").to_lowercase();
        match read_file_checked(&file_path) {
            Ok(bytes) => Ok(crate::shared::image::image_bytes_have_metadata(&bytes, &ext)),
            Err(_) => Ok(false),
        }
    }
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
    // spawn-detached: image compression — CPU work on bytes already in hand.
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

/// Send cached file (with optional compression and metadata retention)
#[tauri::command]
pub async fn send_cached_file(receiver: String, replied_to: String, use_compression: bool, keep_metadata: bool, name_override: String) -> Result<MessageSendResult, String> {
    use super::compression::process_image_for_send;

    // Take the background pre-compression result (stripped + resized), if ready.
    let precompressed = JS_COMPRESSION_CACHE.lock().await.take();

    let (original_bytes, original_name, original_extension) = {
        let mut cache = JS_FILE_CACHE.lock().unwrap();
        cache.take().ok_or("No cached file")?
    };

    let is_image = matches!(original_extension.as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp" | "tiff" | "tif" | "ico");

    let mut attachment_file = if is_image {
        let processed = process_image_for_send(
            original_bytes, &original_extension, use_compression, keep_metadata, precompressed,
        )?;
        AttachmentFile {
            bytes: processed.bytes,
            extension: processed.extension,
            img_meta: processed.img_meta,
            name: original_name,
        }
    } else {
        AttachmentFile {
            bytes: original_bytes,
            extension: original_extension,
            img_meta: None,
            name: original_name,
        }
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

#[tauri::command]
pub async fn file_message(receiver: String, replied_to: String, file_path: String, keep_metadata: bool, name_override: String) -> Result<MessageSendResult, String> {
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

    // Images (no compression here): strip metadata (default) or keep the
    // original bytes untouched. Either way orientation is baked and preview
    // metadata is generated.
    if matches!(attachment_file.extension.as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp" | "tiff" | "tif" | "ico") {
        let processed = super::compression::process_image_for_send(
            attachment_file.bytes.clone(), &attachment_file.extension,
            /* use_compression */ false, keep_metadata, None,
        )?;
        attachment_file.bytes = processed.bytes;
        attachment_file.extension = processed.extension;
        attachment_file.img_meta = processed.img_meta;
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

/// Response from caching an Android file. The preview is a second call
/// (`preview_cached_file` with the same URI) so the decode never runs on the IPC thread.
#[derive(serde::Serialize)]
pub struct AndroidFileCacheResult {
    pub size: u64,
    pub name: String,
    pub extension: String,
}

/// Cache an Android content URI's bytes immediately after file selection.
/// This must be called immediately after the file picker returns, before the permission expires.
/// On non-Android platforms, this just returns file info without caching.
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
        })
    }
    #[cfg(target_os = "android")]
    {
        // Read the file using the same method as avatar upload (read_android_uri)
        // This uses getType() instead of query() which may have different permission behavior
        // read_android_uri now carries the real display name + name-derived extension
        // (falling back to MIME); only synthesize a generic name if it had neither.
        let attachment = filesystem::read_android_uri(file_path.clone())?;
        let bytes = attachment.bytes;
        let size = bytes.len() as u64;
        let extension = attachment.extension.clone();
        let name = if attachment.name.is_empty() {
            format!("file.{}", extension)
        } else {
            attachment.name.clone()
        };


        // Cache the bytes - already Arc from read_android_uri
        let mut cache = ANDROID_FILE_CACHE.lock().unwrap();
        cache.insert(file_path, (bytes, extension.clone(), name.clone(), size));
        
        Ok(AndroidFileCacheResult {
            size,
            name,
            extension,
        })
    }
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
    // spawn-detached: same, for a path already resolved.
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
pub(crate) static PENDING_ZIP_PATH: LazyLock<std::sync::Mutex<Option<String>>> =
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

/// Preview for an image OUTSIDE the asset-protocol scope (pasted via a clipboard
/// manager, dragged from an arbitrary folder): the webview's asset:// route refuses
/// those paths by design, so a downscaled copy is written into the app cache, which
/// it does serve. Image-only (sniffed from magic bytes, never the extension) and
/// size-capped, so it can't grow into a general file-read primitive.
#[tauri::command]
pub async fn read_image_preview(app: tauri::AppHandle, path: String) -> Result<String, String> {
    const MAX_PREVIEW_BYTES: u64 = 32 * 1024 * 1024;
    let meta = std::fs::metadata(&path).map_err(|e| e.to_string())?;
    if !meta.is_file() {
        return Err("not a file".to_string());
    }
    if meta.len() > MAX_PREVIEW_BYTES {
        return Err("file too large for an inline preview".to_string());
    }
    tokio::task::spawn_blocking(move || {
        let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
        // An explicit list, not a prefix: the sniffer also names SVG (any XML) an image.
        let mime = vector_core::crypto::mime_from_magic_bytes(&bytes);
        if !matches!(mime, "image/png" | "image/jpeg" | "image/gif" | "image/webp" | "image/tiff" | "image/x-icon" | "image/bmp") {
            return Err("not an image".to_string());
        }
        write_preview_file(&app, &bytes)
    })
    .await
    .map_err(|e| e.to_string())?
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
pub async fn send_cached_compressed_file(receiver: String, replied_to: String, file_path: String, keep_metadata: bool, name_override: String) -> Result<MessageSendResult, String> {
    use super::compression::process_image_for_send;

    let file_name = std::path::Path::new(&file_path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string();

    // Await the background pre-compression if still running, then take the
    // (stripped + resized) result out of the cache. The wait is time-bounded:
    // the notifier fires via notify_waiters() (which stores no permit), so a
    // completion landing in the gap between the status read and the await would
    // otherwise hang forever — on timeout we just re-read the cache below and
    // fall back to a fresh compress if it's genuinely not ready.
    let precompressed = {
        let status = { COMPRESSION_CACHE.lock().await.get(&file_path).cloned() };
        if let Some(None) = status {
            let notify = { super::types::COMPRESSION_NOTIFY.lock().await.get(&file_path).cloned() };
            if let Some(n) = notify {
                let _ = tokio::time::timeout(std::time::Duration::from_secs(30), n.notified()).await;
            }
        }
        COMPRESSION_CACHE.lock().await.remove(&file_path).flatten()
    };

    let extension = file_path.rsplit('.').next().unwrap_or("bin").to_lowercase();

    // Default strip+compress reuses the pre-compressed result. Keep-metadata
    // (needs EXIF re-attach) and cache misses re-derive from the original file.
    let processed = if !keep_metadata {
        match precompressed {
            Some(pc) => pc,
            None => {
                let bytes = read_file_checked(&file_path)?;
                process_image_for_send(Arc::new(bytes), &extension, true, false, None)?
            }
        }
    } else {
        let bytes = read_file_checked(&file_path)?;
        process_image_for_send(Arc::new(bytes), &extension, true, true, None)?
    };

    let mut attachment_file = AttachmentFile {
        bytes: processed.bytes,
        extension: processed.extension,
        img_meta: processed.img_meta,
        name: file_name,
    };
    if !name_override.is_empty() {
        let sanitized = crate::commands::attachments::sanitize_filename(&name_override);
        if !sanitized.is_empty() { attachment_file.name = sanitized; }
    }
    message(receiver, String::new(), replied_to, Some(attachment_file)).await
}

#[cfg(test)]
mod preview_tests {
    use super::*;

    fn scratch_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("vector-preview-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn jpeg(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbImage::from_fn(w, h, |x, y| image::Rgb([(x % 256) as u8, (y % 256) as u8, 90]));
        let mut out = std::io::Cursor::new(Vec::new());
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 80).encode_image(&img).unwrap();
        out.into_inner()
    }

    fn transparent_png() -> Vec<u8> {
        let img = image::RgbaImage::from_fn(40, 30, |x, _| image::Rgba([200, 20, 20, if x < 20 { 0 } else { 255 }]));
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(img).write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    }

    fn gif(w: u32, h: u32) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut enc = image::codecs::gif::GifEncoder::new(&mut out);
            let frame = image::Frame::new(image::RgbaImage::from_pixel(w, h, image::Rgba([1, 2, 3, 255])));
            enc.encode_frame(frame).unwrap();
        }
        out
    }

    #[test]
    fn a_large_photo_is_bounded_and_a_hit_keeps_its_spelling() {
        let dir = scratch_dir("photo");
        let bytes = jpeg(2000, 1500);
        let first = write_preview_into(&dir, &bytes).unwrap();
        assert_eq!(first.extension().unwrap(), "jpg");
        let dims = image::image_dimensions(&first).unwrap();
        assert!(dims.0 <= PREVIEW_MAX_DIM && dims.1 <= PREVIEW_MAX_DIM, "not bounded: {dims:?}");
        assert_eq!(dims.0, 1024, "longest side lands exactly on the cap");
        let again = write_preview_into(&dir, &bytes).unwrap();
        assert_eq!(first, again, "the second call is a hit, not a fresh file");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1, "no tmp litter, no duplicate");
    }

    #[test]
    fn a_transparent_image_stays_png_and_hits_as_png() {
        let dir = scratch_dir("png");
        let bytes = transparent_png();
        let first = write_preview_into(&dir, &bytes).unwrap();
        assert_eq!(first.extension().unwrap(), "png");
        assert_eq!(write_preview_into(&dir, &bytes).unwrap(), first);
    }

    #[test]
    fn a_small_gif_is_kept_verbatim_and_a_huge_one_is_not() {
        let dir = scratch_dir("gif");
        let small = gif(64, 48);
        let path = write_preview_into(&dir, &small).unwrap();
        assert_eq!(path.extension().unwrap(), "gif");
        assert_eq!(std::fs::read(&path).unwrap(), small, "the animation must survive untouched");

        let wide = gif(PREVIEW_MAX_DIM + 1, 8);
        let path = write_preview_into(&dir, &wide).unwrap();
        assert_ne!(path.extension().unwrap(), "gif", "over the cap it is decoded like a photo");
        let dims = image::image_dimensions(&path).unwrap();
        assert!(dims.0 <= PREVIEW_MAX_DIM);
    }

    #[test]
    fn the_sweep_removes_only_stale_files() {
        let dir = scratch_dir("prune");
        let stale = dir.join("old.jpg");
        let fresh = dir.join("new.jpg");
        std::fs::write(&stale, b"x").unwrap();
        std::fs::write(&fresh, b"y").unwrap();
        let long_ago = std::time::SystemTime::now() - PREVIEW_MAX_AGE - std::time::Duration::from_secs(60);
        std::fs::File::open(&stale).unwrap().set_modified(long_ago).unwrap();
        assert_eq!(prune_dir(&dir), 1);
        assert!(!stale.exists());
        assert!(fresh.exists());
    }
}
