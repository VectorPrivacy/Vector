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

/// (bytes, file name, extension)
type JsCachedFile = (Arc<Vec<u8>>, String, String);

/// Cache for bytes received from JavaScript (for Android file handling)
pub(crate) static JS_FILE_CACHE: LazyLock<std::sync::Mutex<Option<JsCachedFile>>> =
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


/// A path's extension, lower-cased, or `bin` when it has none: never the whole path, as
/// splitting on the last dot gives for `README` or `/a.dir/readme`.
fn path_extension(file_path: &str) -> String {
    std::path::Path::new(file_path)
        .extension()
        .and_then(|e| e.to_str())
        .filter(|e| !e.is_empty())
        .map(|e| e.to_lowercase())
        .unwrap_or_else(|| "bin".to_string())
}

fn is_previewable_extension(extension: &str) -> bool {
    matches!(extension, "png" | "jpg" | "jpeg" | "gif" | "webp" | "tiff" | "tif" | "ico" | "svg")
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

/// The preview itself: a small GIF is copied verbatim so it animates; an SVG is rendered to
/// pixels (its markup never reaches the webview); everything else is bounded to
/// `PREVIEW_MAX_DIM` and re-encoded (PNG when transparent, JPEG otherwise).
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
    } else if vector_core::svg::looks_like_svg(bytes) {
        // Always PNG: JPEG would soften the hard edges and text an SVG is drawn with.
        ("png", std::borrow::Cow::Owned(vector_core::svg::rasterize_png(bytes, PREVIEW_MAX_DIM)?))
    } else {
        let img = crate::shared::image::decode_image(bytes, PREVIEW_MAX_DIM)?;
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
pub async fn generate_thumbhash_for_preview(file_path: String) -> Result<String, String> {
    // A synchronous command runs on the main thread, where a full decode freezes the UI.
    tokio::task::spawn_blocking(move || thumbhash_for_preview(&file_path))
        .await
        .map_err(|e| e.to_string())?
}

fn thumbhash_for_preview(file_path: &str) -> Result<String, String> {
    let img = if file_path.is_empty() {
        let cache = JS_FILE_CACHE.lock().unwrap();
        let (bytes, _, _) = cache.as_ref().ok_or("No cached file and no file path provided")?;
        crate::shared::image::decode_image(bytes, 100)
            .map_err(|e| format!("Failed to decode cached image: {}", e))?
    } else {
        let bytes = std::fs::read(file_path).map_err(|e| format!("Failed to open image: {}", e))?;
        crate::shared::image::decode_image(&bytes, 100)
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
pub async fn file_has_metadata(file_path: String) -> Result<bool, String> {
    tokio::task::spawn_blocking(move || has_metadata(file_path))
        .await
        .map_err(|e| e.to_string())?
}

fn has_metadata(file_path: String) -> Result<bool, String> {
    if file_path.is_empty() {
        let cache = JS_FILE_CACHE.lock().unwrap();
        Ok(match cache.as_ref() {
            Some((bytes, _, ext)) => crate::shared::image::image_bytes_have_metadata(bytes.as_slice(), ext),
            None => false,
        })
    } else {
        let ext = path_extension(&file_path);
        // A JPEG or PNG is answered from its segment headers, never read whole.
        if matches!(ext.as_str(), "jpg" | "jpeg" | "png") {
            return Ok(std::fs::File::open(&file_path)
                .ok()
                .and_then(|mut f| crate::shared::image::header_has_metadata(&mut f, &ext))
                .unwrap_or(false));
        }
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
        let result = tokio::task::spawn_blocking(move || compress_bytes_internal(bytes, &extension, None))
            .await
            .unwrap_or_else(|e| Err(e.to_string()));
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
        let ext = original_extension.clone();
        let processed = tokio::task::spawn_blocking(move || process_image_for_send(
            original_bytes, &ext, use_compression, keep_metadata, precompressed,
        ))
        .await
        .map_err(|e| e.to_string())??;
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
    ANDROID_FILE_CACHE.lock().unwrap().remove(&file_path);
    super::types::discard_picked_file(&file_path);
    Ok(())
}

/// Clear all Android file cache entries
/// This is a cleanup function to ensure no stale data remains
#[tauri::command]
pub fn clear_all_android_file_cache() -> Result<(), String> {
    ANDROID_FILE_CACHE.lock().unwrap().clear();
    if let Ok(mut picked) = super::types::ANDROID_PICKED_FILES.lock() {
        for (_, (path, ..)) in picked.drain() {
            let _ = std::fs::remove_file(path);
        }
    }
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

    // Anything that isn't processed first streams from disk, whatever its size.
    #[cfg(not(target_os = "android"))]
    {
        let extension = path_extension(&file_path);
        if !matches!(extension.as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp" | "tiff" | "tif" | "ico") {
            match std::fs::metadata(&file_path) {
                Ok(m) if m.len() > 0 => {}
                Ok(_) => return Err(format!("File is empty (0 bytes): {}", file_path)),
                Err(e) => return Err(format!("Failed to read file metadata: {}", e)),
            }
            take_precompressed(&file_path, false).await;
            let mut name = file_name.clone();
            if !name_override.is_empty() {
                let sanitized = crate::commands::attachments::sanitize_filename(&name_override);
                if !sanitized.is_empty() { name = sanitized; }
            }
            return super::sending::send_file_from_path(receiver, replied_to, std::path::PathBuf::from(&file_path), name, extension).await;
        }
    }

    // Load the file as AttachmentFile
    let mut attachment_file = {
        #[cfg(not(target_os = "android"))]
        {
            let path = file_path.clone();
            let file_bytes = tokio::task::spawn_blocking(move || read_file_checked(&path))
                .await
                .map_err(|e| e.to_string())??;

            let extension = path_extension(&file_path);

            AttachmentFile {
                bytes: Arc::new(file_bytes),
                img_meta: None,
                extension,
                name: file_name.clone(),
            }
        }
        #[cfg(target_os = "android")]
        {
            // A pick copied to disk at selection streams from that copy, which goes once sent.
            let picked = super::types::ANDROID_PICKED_FILES.lock().unwrap().get(&file_path).cloned();
            if let Some((copy, extension, cached_name, _)) = picked {
                let mut name = cached_name;
                if !name_override.is_empty() {
                    let sanitized = crate::commands::attachments::sanitize_filename(&name_override);
                    if !sanitized.is_empty() { name = sanitized; }
                }
                let sent = super::sending::send_file_from_path(receiver, replied_to, copy, name, extension).await;
                super::types::discard_picked_file(&file_path);
                return sent;
            }
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
                    filesystem::read_android_uri(file_path.clone())?
                } else {
                    // Regular file path (e.g., marketplace apps) - use standard file I/O
                    let file_bytes = read_file_checked(&file_path)?;

                    let extension = path_extension(&file_path);

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

    // The preview's pre-compression is no use to a full-resolution send beyond its
    // thumbhash. Taking the entry also drops a result still being made.
    let precompressed = take_precompressed(&file_path, false).await;

    // Images (no compression here): strip metadata (default) or keep the
    // original bytes untouched. Either way orientation is kept and preview
    // metadata is generated.
    if matches!(attachment_file.extension.as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp" | "tiff" | "tif" | "ico") {
        let (bytes, extension) = (attachment_file.bytes.clone(), attachment_file.extension.clone());
        let processed = tokio::task::spawn_blocking(move || super::compression::process_image_for_send(
            bytes, &extension, /* use_compression */ false, keep_metadata, precompressed,
        ))
        .await
        .map_err(|e| e.to_string())??;
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

/// Where Android picks wait to be sent. Copies older than a day are leftovers of a
/// preview that was never sent or closed, and are swept on the way in.
#[cfg(target_os = "android")]
fn picked_files_dir() -> Result<std::path::PathBuf, String> {
    let dir = vector_core::db::get_app_data_dir()?.join("picked");
    std::fs::create_dir_all(&dir).map_err(|e| format!("Failed to prepare the file: {e}"))?;
    if let Ok(entries) = std::fs::read_dir(&dir) {
        let now = std::time::SystemTime::now();
        for entry in entries.flatten() {
            let old = entry.metadata().and_then(|m| m.modified())
                .map(|t| now.duration_since(t).unwrap_or_default() > std::time::Duration::from_secs(24 * 3600))
                .unwrap_or(false);
            if old {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    Ok(dir)
}

/// Cache an Android content URI's bytes immediately after file selection.
/// This must be called immediately after the file picker returns, before the permission expires.
/// On non-Android platforms, this just returns file info without caching.
#[tauri::command]
pub async fn cache_android_file(file_path: String) -> Result<AndroidFileCacheResult, String> {
    // A synchronous command runs on the main thread, where reading a large pick stalls the UI.
    tokio::task::spawn_blocking(move || cache_picked_file(file_path))
        .await
        .map_err(|e| e.to_string())?
}

fn cache_picked_file(file_path: String) -> Result<AndroidFileCacheResult, String> {
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
        // Anything that isn't processed before sending is copied to disk rather than
        // held in memory, so its size is bounded by storage alone.
        if let Ok(info) = filesystem::get_android_uri_info(file_path.clone()) {
            let processed = matches!(info.extension.as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp" | "tiff" | "tif" | "ico");
            if !processed && !info.extension.is_empty() {
                let dir = picked_files_dir()?;
                let copy = dir.join(format!("{}.{}", vector_core::crypto::sha256_hex(file_path.as_bytes()), info.extension));
                let size = filesystem::copy_android_uri_to_file(&file_path, &copy)?;
                let name = if info.name.is_empty() || info.name == "unknown" {
                    format!("file.{}", info.extension)
                } else {
                    info.name.clone()
                };
                super::types::ANDROID_PICKED_FILES.lock().unwrap()
                    .insert(file_path, (copy, info.extension.clone(), name.clone(), size));
                return Ok(AndroidFileCacheResult { size, name, extension: info.extension });
            }
        }

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
        if let Some((_, extension, name, size)) = super::types::ANDROID_PICKED_FILES.lock().unwrap().get(&file_path) {
            return Ok(FileInfo { size: *size, name: name.clone(), extension: extension.clone() });
        }
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
    // Mark as in progress, and create a notify for waiters
    let cancel = {
        let mut cache = COMPRESSION_CACHE.lock().await;
        if let Some(old) = cache.get(&file_path) {
            old.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        let slot = super::types::CompressionSlot::running();
        let cancel = slot.cancel.clone();
        cache.insert(file_path.clone(), slot);
        cancel
    };
    {
        let mut notifiers = super::types::COMPRESSION_NOTIFY.lock().await;
        notifiers.insert(file_path.clone(), Arc::new(tokio::sync::Notify::new()));
    }

    // Spawn the compression task
    let path_clone = file_path.clone();
    // spawn-detached: same, for a path already resolved.
    tokio::spawn(async move {
        let path_for_work = path_clone.clone();
        let flag = cancel.clone();
        let result = tokio::task::spawn_blocking(move || crate::shared::cancel::scoped(flag, || compress_image_internal(&path_for_work)))
            .await
            .unwrap_or_else(|e| Err(e.to_string()));
        let mut cache = COMPRESSION_CACHE.lock().await;

        // Only into the slot this run was started for: a cancel removed it, a reopen replaced it.
        if let Some(slot) = cache.get_mut(&path_clone).filter(|s| Arc::ptr_eq(&s.cancel, &cancel)) {
            slot.result = result.ok();
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
    
    match cache.get(&file_path).map(|slot| &slot.result) {
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

/// Fraction done of a video still compressing for the preview; None for anything else.
#[tauri::command]
pub async fn get_compression_progress(file_path: String) -> Option<f32> {
    super::video_compression::progress(&file_path)
}

/// Clear the compression cache for a file (called on cancel)
#[tauri::command]
pub async fn clear_compression_cache(file_path: String) -> Result<(), String> {
    let removed = COMPRESSION_CACHE.lock().await.remove(&file_path);
    if let Some(slot) = removed {
        slot.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    super::video_compression::cancel(&file_path);
    
    // Also clear Android file cache
    ANDROID_FILE_CACHE.lock().unwrap().remove(&file_path);
    super::types::discard_picked_file(&file_path);
    
    Ok(())
}

// ─── Directory Zip & Send ───────────────────────────────────────────────────

/// Pending zip path for cleanup
pub(crate) static PENDING_ZIP_PATH: LazyLock<std::sync::Mutex<Option<String>>> =
    LazyLock::new(|| std::sync::Mutex::new(None));

/// Generation counter for zip_paths — each new zip increments this.
/// An in-progress zip aborts if its generation no longer matches the current one.
static ZIP_GENERATION: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// Result returned by zip_paths
#[derive(serde::Serialize)]
pub struct ZipResult {
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
        // An explicit list, not a prefix: the sniffer also names SVG (any XML) an image. An SVG
        // passes only as the pixels the preview renders; any other XML fails to render.
        let mime = vector_core::crypto::mime_from_magic_bytes(&bytes);
        if !matches!(mime, "image/png" | "image/jpeg" | "image/gif" | "image/webp" | "image/tiff" | "image/x-icon" | "image/bmp")
            && !vector_core::svg::looks_like_svg(&bytes)
        {
            return Err("not an image".to_string());
        }
        write_preview_file(&app, &bytes)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// The picked image at `path` as it will upload as an avatar with `crop`: cropped, shrunk and
/// re-encoded the same way, so the preview is the upload, and a moving picture keeps moving.
#[tauri::command]
pub async fn crop_image_preview(
    app: tauri::AppHandle,
    path: String,
    crop: vector_core::image_crop::CropRect,
) -> Result<String, String> {
    #[cfg(not(target_os = "android"))]
    let bytes = tokio::fs::read(&path).await.map_err(|e| e.to_string())?;
    #[cfg(target_os = "android")]
    let bytes = (*crate::android::filesystem::read_android_uri(path)?.bytes).clone();
    tokio::task::spawn_blocking(move || {
        if vector_core::svg::looks_like_svg(&bytes) {
            return Err(vector_core::community::SVG_REFUSED.to_string());
        }
        let cropped = crate::commands::emoji_packs::crop_drawn(bytes, crop)?;
        let prepared = crate::shared::image::prepare_upload_image(&cropped, crate::shared::image::UploadImageKind::Avatar)?;
        let dir = preview_cache_dir(&app)?;
        prune_dir(&dir);
        write_preview_as_is(&dir, &prepared.bytes, prepared.extension).map(|p| p.to_string_lossy().into_owned())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// `bytes` into the preview cache unchanged, keyed by content.
fn write_preview_as_is(dir: &std::path::Path, bytes: &[u8], ext: &str) -> Result<std::path::PathBuf, String> {
    use sha2::{Digest, Sha256};
    let key = crate::util::bytes_to_hex_string(&Sha256::digest(bytes)[..16]);
    let path = dir.join(format!("{key}.{ext}"));
    if path.is_file() {
        return Ok(path);
    }
    let seq = PREVIEW_TMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = dir.join(format!("{key}.{ext}.tmp-{}-{seq}", std::process::id()));
    std::fs::write(&tmp, bytes).map_err(|e| format!("Failed to write preview: {}", e))?;
    if let Err(e) = std::fs::rename(&tmp, &path) {
        let _ = std::fs::remove_file(&tmp);
        if !path.is_file() {
            return Err(format!("Failed to place preview: {}", e));
        }
    }
    Ok(path)
}

/// An SVG attachment drawn for display: the file itself stays as received, and the webview only
/// ever shows this render. Cached by content and size (sizes rounded up to 256 px steps, so a
/// viewer's many window sizes share renders); `None` when the SVG refuses to render.
#[tauri::command]
pub async fn render_svg(app: tauri::AppHandle, path: String, max_dim: u32) -> Result<Option<String>, String> {
    let meta = std::fs::metadata(&path).map_err(|e| e.to_string())?;
    if !meta.is_file() || meta.len() > vector_core::svg::MAX_SVG_BYTES as u64 {
        return Ok(None);
    }
    tokio::task::spawn_blocking(move || {
        use sha2::{Digest, Sha256};
        let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
        if !vector_core::svg::looks_like_svg(&bytes) {
            return Ok(None);
        }
        let dim = max_dim.clamp(256, vector_core::svg::MAX_RENDER_DIM).div_ceil(256) * 256;
        let dir = svg_render_dir(&app)?;
        let key = crate::util::bytes_to_hex_string(&Sha256::digest(&bytes)[..16]);
        let out = dir.join(format!("{key}-{dim}.png"));
        if out.is_file() {
            // A hit is a use: keep it out of the age sweep.
            if let Ok(f) = std::fs::File::open(&out) {
                let _ = f.set_modified(std::time::SystemTime::now());
            }
            return Ok(Some(out.to_string_lossy().into_owned()));
        }
        let Ok(png) = vector_core::svg::rasterize_png(&bytes, dim) else { return Ok(None) };
        let seq = PREVIEW_TMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let tmp = dir.join(format!("{key}-{dim}.png.tmp-{}-{seq}", std::process::id()));
        std::fs::write(&tmp, &png).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &out).map_err(|e| e.to_string())?;
        Ok(Some(out.to_string_lossy().into_owned()))
    })
    .await
    .map_err(|e| e.to_string())?
}

fn svg_render_dir(app: &tauri::AppHandle) -> Result<std::path::PathBuf, String> {
    use tauri::Manager;
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?.join("cache").join("svg_renders");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

/// Delete SVG renders nothing has shown for a while; any is re-drawn on demand in milliseconds.
pub fn prune_svg_renders(app: &tauri::AppHandle) -> usize {
    const MAX_AGE: std::time::Duration = std::time::Duration::from_secs(14 * 24 * 60 * 60);
    let Ok(dir) = svg_render_dir(app) else { return 0 };
    let Ok(entries) = std::fs::read_dir(&dir) else { return 0 };
    let now = std::time::SystemTime::now();
    entries
        .flatten()
        .filter(|e| {
            e.metadata().and_then(|m| m.modified()).is_ok_and(|t| now.duration_since(t).unwrap_or_default() > MAX_AGE)
        })
        .filter(|e| std::fs::remove_file(e.path()).is_ok())
        .count()
}

/// Let the webview play one video the user picked from outside the asset scope.
///
/// The static scope is deliberately narrow, and a picked video can live anywhere.
/// Images are copied into the preview dir instead, but copying a video that may run
/// to gigabytes just to show its first frame is the wrong trade: this admits that
/// single file, and only when its bytes say it is a video.
#[tauri::command]
pub fn allow_video_preview(app: tauri::AppHandle, path: String) -> Result<(), String> {
    use std::io::Read;
    use tauri::Manager;
    let meta = std::fs::metadata(&path).map_err(|e| e.to_string())?;
    if !meta.is_file() {
        return Err("not a file".to_string());
    }
    let mut head = [0u8; 64];
    let n = std::fs::File::open(&path)
        .and_then(|mut f| f.read(&mut head))
        .map_err(|e| e.to_string())?;
    if !vector_core::crypto::mime_from_magic_bytes(&head[..n]).starts_with("video/") {
        return Err("not a video".to_string());
    }
    app.asset_protocol_scope().allow_file(&path).map_err(|e| e.to_string())
}

/// Zip a dropped or pasted selection for sending as one attachment. A single folder zips its
/// contents under the folder's name; several items (files, folders, or both) sit side by side
/// at the archive's root.
#[tauri::command]
pub async fn zip_paths(paths: Vec<String>) -> Result<ZipResult, String> {
    // Claim a new generation — any previous zip will see a mismatch and abort
    let my_generation = ZIP_GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;

    // Run all sync I/O on a blocking thread to avoid tying up the async runtime
    tokio::task::spawn_blocking(move || {
        zip_paths_blocking(&paths, my_generation)
    }).await.map_err(|e| format!("Zip task failed: {}", e))?
}

/// One archive entry: where its bytes come from and its path inside the zip.
struct ZipItem {
    source: std::path::PathBuf,
    name: String,
    is_dir: bool,
    size: u64,
}

const ZIP_MAX_DEPTH: u32 = 128;

fn walk_zip_dir(
    current: &std::path::Path,
    prefix: &str,
    items: &mut Vec<ZipItem>,
    total_size: &mut u64,
    depth: u32,
) -> Result<(), String> {
    if depth > ZIP_MAX_DEPTH {
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

        let name = format!("{prefix}{}", entry.file_name().to_string_lossy());
        if meta.is_dir() {
            items.push(ZipItem { source: path.clone(), name: name.clone(), is_dir: true, size: 0 });
            walk_zip_dir(&path, &format!("{name}/"), items, total_size, depth + 1)?;
        } else if meta.is_file() {
            *total_size += meta.len();
            items.push(ZipItem { source: path, name, is_dir: false, size: meta.len() });
        }
    }
    Ok(())
}

/// `name`, or `name-1`, `name-2`… before the extension when the root already holds it. Case-blind,
/// since the archive mostly lands on case-insensitive filesystems.
fn unique_root_name(name: &str, is_dir: bool, taken: &mut std::collections::HashSet<String>) -> String {
    if taken.insert(name.to_lowercase()) {
        return name.to_string();
    }
    let path = std::path::Path::new(name);
    let (stem, ext) = match (is_dir, path.file_stem(), path.extension()) {
        (false, Some(stem), Some(ext)) => (stem.to_string_lossy().into_owned(), Some(ext.to_string_lossy().into_owned())),
        _ => (name.to_string(), None),
    };
    (1u32..)
        .map(|n| match &ext {
            Some(ext) => format!("{stem}-{n}.{ext}"),
            None => format!("{stem}-{n}"),
        })
        .find(|candidate| taken.insert(candidate.to_lowercase()))
        .expect("an unused suffix exists")
}

/// The archive's name and its entries, folders before their contents.
fn collect_zip_items(paths: &[String]) -> Result<(String, Vec<ZipItem>, u64), String> {
    let mut items = Vec::new();
    let mut total_size = 0u64;

    if let [only] = paths {
        let dir = std::path::Path::new(only);
        if dir.is_dir() {
            let dir_name = dir.file_name().and_then(|n| n.to_str()).unwrap_or("folder").to_string();
            walk_zip_dir(dir, "", &mut items, &mut total_size, 0)?;
            if items.is_empty() {
                return Err("Directory is empty".to_string());
            }
            return Ok((dir_name, items, total_size));
        }
    }

    let mut taken = std::collections::HashSet::new();
    for raw in paths {
        let picked = std::path::Path::new(raw);
        let Some(file_name) = picked.file_name() else { continue };
        // The user chose these roots, so an alias is followed to what it names; the walk below
        // still skips symlinks inside folders.
        let Ok(source) = std::fs::canonicalize(picked) else { continue };
        let Ok(meta) = std::fs::metadata(&source) else { continue };
        if !meta.is_dir() && !meta.is_file() {
            continue;
        }
        let name = unique_root_name(&file_name.to_string_lossy(), meta.is_dir(), &mut taken);
        if meta.is_dir() {
            items.push(ZipItem { source: source.clone(), name: name.clone(), is_dir: true, size: 0 });
            walk_zip_dir(&source, &format!("{name}/"), &mut items, &mut total_size, 1)?;
        } else {
            total_size += meta.len();
            items.push(ZipItem { source, name, is_dir: false, size: meta.len() });
        }
    }
    if items.is_empty() {
        return Err("Nothing to zip".to_string());
    }
    Ok(("Archive".to_string(), items, total_size))
}

fn zip_paths_blocking(paths: &[String], my_generation: u64) -> Result<ZipResult, String> {
    use std::io::{BufWriter, Write};
    use zip::write::SimpleFileOptions;
    use tauri::Emitter;
    use zip::CompressionMethod;

    let (archive_name, items, total_size) = collect_zip_items(paths)?;

    // Zip phase — byte-based progress for smooth updates
    // Use generation in filename to avoid collisions with previous cleanup_zip calls
    let zip_name = format!("{}.zip", archive_name);
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

    // Level 2: zlib-rs's level 1 is deflate_quick (up to ~20% larger); 2 still beats C zlib's 1 on speed and size
    let options: SimpleFileOptions = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .compression_level(Some(2));

    let mut file_list: Vec<ZipEntry> = Vec::new();
    let mut file_count: u32 = 0;
    let mut dir_count: u32 = 0;

    // Chunk size for intra-file progress (512KB)
    const CHUNK_SIZE: usize = 512 * 1024;

    for item in &items {
        let path = &item.source;
        let rel_str = item.name.replace('\\', "/");

        if item.is_dir {
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
            let file_size = item.size;

            // Re-verify not a symlink at zip time (TOCTOU mitigation)
            match std::fs::symlink_metadata(path) {
                Ok(m) if m.file_type().is_symlink() => continue,
                Err(_) => continue,
                _ => {}
            }

            // Past 4 GB an entry needs ZIP64 sizes.
            zip_writer.start_file(&rel_str, options.large_file(file_size >= u32::MAX as u64))
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
                if let Some(percent) = (bytes_written * 100).checked_div(total_size) {
                    let percent = percent.min(100);
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

    Ok(ZipResult {
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
    // Bump generation to invalidate any running zip_paths
    ZIP_GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    // Also clean up the file if compression already finished
    let path = PENDING_ZIP_PATH.lock().unwrap_or_else(|e| e.into_inner()).take();
    if let Some(p) = path {
        let _ = std::fs::remove_file(&p);
    }
    Ok(())
}

/// Take the preview's pre-compression of `file_path` out of the cache. `wait` awaits a
/// run still in progress, for a send that will use the compressed bytes; otherwise a
/// run in progress is dropped on arrival.
///
/// The notifier fires via notify_waiters(), which stores no permit, so the waiter is
/// registered before the status is read: a completion in between still wakes it. The
/// timeout is only a backstop.
pub(crate) async fn take_precompressed(file_path: &str, wait: bool) -> Option<CachedCompressedImage> {
    let video = super::video_compression::is_video_extension(&path_extension(file_path));
    if video && !wait {
        super::video_compression::cancel(file_path);
    }
    if wait {
        let notify = { super::types::COMPRESSION_NOTIFY.lock().await.get(file_path).cloned() };
        if let Some(n) = notify {
            let notified = n.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let running = COMPRESSION_CACHE.lock().await.get(file_path).is_some_and(|s| s.result.is_none());
            if running {
                // A video encode takes as long as the clip needs, and shows its progress.
                if video {
                    notified.await;
                } else {
                    let _ = tokio::time::timeout(std::time::Duration::from_secs(30), notified).await;
                }
            }
        }
    }
    let slot = COMPRESSION_CACHE.lock().await.remove(file_path)?;
    if slot.result.is_none() {
        slot.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    slot.result
}

/// Backstop for previews that never closed through the frontend (a reload, a crash):
/// finished pre-compressions nobody collected go after `PRECOMPRESSION_TTL`.
const PRECOMPRESSION_TTL: std::time::Duration = std::time::Duration::from_secs(15 * 60);

pub(crate) async fn sweep_compression_cache() -> usize {
    let running: std::collections::HashSet<String> =
        super::types::COMPRESSION_NOTIFY.lock().await.keys().cloned().collect();
    let mut cache = COMPRESSION_CACHE.lock().await;
    let before = cache.len();
    cache.retain(|path, slot| running.contains(path) || slot.started.elapsed() < PRECOMPRESSION_TTL);
    before - cache.len()
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

    // Keep-metadata re-derives from the original, so only the default path waits.
    let precompressed = take_precompressed(&file_path, !keep_metadata).await;

    let extension = path_extension(&file_path);

    // Default strip+compress reuses the pre-compressed result. Keep-metadata
    // (needs EXIF re-attach) and cache misses re-derive from the original file.
    let processed = match precompressed {
        Some(pc) if !keep_metadata => pc,
        _ => {
            let (path, extension) = (file_path.clone(), extension.clone());
            tokio::task::spawn_blocking(move || {
                if super::video_compression::is_video_extension(&extension) {
                    return super::video_compression::compress_file(&path);
                }
                let bytes = read_file_checked(&path)?;
                process_image_for_send(Arc::new(bytes), &extension, true, keep_metadata, None)
            })
            .await
            .map_err(|e| e.to_string())??
        }
    };
    // The picked file's bytes were only needed to compress it.
    #[cfg(target_os = "android")]
    ANDROID_FILE_CACHE.lock().unwrap().remove(&file_path);

    let mut name = file_name;
    if !name_override.is_empty() {
        let sanitized = crate::commands::attachments::sanitize_filename(&name_override);
        if !sanitized.is_empty() { name = sanitized; }
    }
    if processed.is_original_on_disk() {
        return super::sending::send_file_from_path(receiver, replied_to, std::path::PathBuf::from(&file_path), name, processed.extension).await;
    }
    let mut attachment_file = AttachmentFile {
        bytes: processed.bytes,
        extension: processed.extension,
        img_meta: processed.img_meta,
        name,
    };
    // A compressed video's name follows its new container, so the receiver opens it as one.
    if super::video_compression::is_video_extension(&extension) && attachment_file.extension != extension {
        attachment_file.name = super::video_compression::renamed(&attachment_file.name, &attachment_file.extension);
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

#[cfg(test)]
mod zip_tests {
    use super::*;

    fn scratch(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("vector-zip-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn zip(paths: &[&std::path::Path]) -> (ZipResult, Vec<(String, Vec<u8>)>) {
        use std::io::Read;
        let paths: Vec<String> = paths.iter().map(|p| p.to_string_lossy().into_owned()).collect();
        let generation = ZIP_GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        let result = zip_paths_blocking(&paths, generation).unwrap();
        let mut archive = zip::ZipArchive::new(std::fs::File::open(&result.zip_path).unwrap()).unwrap();
        let mut entries: Vec<(String, Vec<u8>)> = (0..archive.len())
            .map(|i| {
                let mut entry = archive.by_index(i).unwrap();
                let mut bytes = Vec::new();
                entry.read_to_end(&mut bytes).unwrap();
                (entry.name().to_string(), bytes)
            })
            .collect();
        entries.sort();
        std::fs::remove_file(&result.zip_path).unwrap();
        (result, entries)
    }

    fn names(entries: &[(String, Vec<u8>)]) -> Vec<&str> {
        entries.iter().map(|(name, _)| name.as_str()).collect()
    }

    // One test: every zip claims the shared generation, so parallel zips would cancel each other.
    #[test]
    fn a_selection_zips_side_by_side_and_a_lone_folder_zips_its_contents() {
        let root = scratch("selection");
        let photos = root.join("Photos");
        std::fs::create_dir_all(photos.join("raw")).unwrap();
        std::fs::write(photos.join("cat.jpg"), b"meow").unwrap();
        std::fs::write(photos.join("raw/cat.dng"), b"raw meow").unwrap();
        let other = root.join("other");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(root.join("notes.txt"), b"first").unwrap();
        std::fs::write(other.join("Notes.txt"), b"second").unwrap();
        std::fs::write(root.join("empty.bin"), b"").unwrap();

        // A lone folder: its contents at the root, the archive named after it.
        let (result, entries) = zip(&[&photos]);
        assert_eq!(result.zip_name, "Photos.zip");
        assert_eq!(names(&entries), ["cat.jpg", "raw/", "raw/cat.dng"]);
        assert_eq!((result.file_count, result.dir_count), (2, 1));

        // A selection: each item at the root under its own name, a clash suffixed case-blind.
        let (result, entries) = zip(&[&root.join("notes.txt"), &other.join("Notes.txt"), &photos, &root.join("empty.bin")]);
        assert_eq!(result.zip_name, "Archive.zip");
        assert_eq!(
            names(&entries),
            ["Notes-1.txt", "Photos/", "Photos/cat.jpg", "Photos/raw/", "Photos/raw/cat.dng", "empty.bin", "notes.txt"]
        );
        assert_eq!(entries.iter().find(|(n, _)| n == "Notes-1.txt").unwrap().1, b"second");
        assert_eq!(entries.iter().find(|(n, _)| n == "notes.txt").unwrap().1, b"first");
        assert_eq!((result.file_count, result.dir_count), (5, 2));
        assert_eq!(result.uncompressed_size, 4 + 8 + 5 + 6);

        // A picked alias is followed; one inside a picked folder is not.
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(root.join("notes.txt"), root.join("link.txt")).unwrap();
            std::os::unix::fs::symlink(root.join("notes.txt"), photos.join("inner-link.txt")).unwrap();
            let (_, entries) = zip(&[&root.join("link.txt"), &photos]);
            assert_eq!(names(&entries), ["Photos/", "Photos/cat.jpg", "Photos/raw/", "Photos/raw/cat.dng", "link.txt"]);
            assert_eq!(entries.iter().find(|(n, _)| n == "link.txt").unwrap().1, b"first");
        }

        // Nothing readable is an error, never an empty archive.
        let generation = ZIP_GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        assert!(zip_paths_blocking(&[root.join("missing").to_string_lossy().into_owned()], generation).is_err());

        let _ = std::fs::remove_dir_all(&root);
    }
}
