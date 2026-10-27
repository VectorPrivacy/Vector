//! System and platform Tauri commands.
//!
//! This module handles system-level operations:
//! - Platform feature detection
//! - Storage management (info and cleanup)
//! - Periodic maintenance tasks

use tauri::{AppHandle, Emitter, Manager, Runtime};

use crate::{STATE, TAURI_APP};
use crate::{db, image_cache, util::format_bytes};

#[cfg(desktop)]
use crate::audio;

#[cfg(feature = "whisper")]
use crate::whisper;

// ============================================================================
// Types
// ============================================================================

/// Platform feature list structure
#[derive(serde::Serialize, Clone)]
pub struct PlatformFeatures {
    pub transcription: bool,
    pub notification_sounds: bool,
    pub os: String,
    pub is_mobile: bool,
    pub debug_mode: bool,
    /// Localhost media server URL prefix (Android only).
    /// When set, the frontend uses this instead of `asset://` for media elements
    /// to get proper HTTP Range request support (seeking, streaming).
    pub media_url: Option<String>,
    /// Where media this account holds in memory is served: `<vmedia>/<hash>`.
    pub vmedia: String,
    /// Where Vector Cache files are served: `<vcache>/<path under app_data>`.
    pub vcache: String,
    /// The app's folder, with a trailing separator: a cache file's path starts with it.
    pub app_data: String,
    /// The folders under `<app_data>/cache` served sealed at `vcache`.
    pub vcache_kinds: Vec<String>,
    /// Whether this build looks for and installs its own updates. Off in the
    /// F-Droid flavour, where the store owns updates.
    pub self_update: bool,
    /// Whether videos can be compressed before sending (the `video` build and an encoder).
    pub video_compression: bool,
    /// Whether this build can host an embedded video player.
    pub players: bool,
    /// Where Save and Keep Unencrypted put files, as a file manager shows the path.
    pub public_folder: String,
}

// ============================================================================
// Tauri Commands
// ============================================================================

/// Returns a list of platform-specific features available
#[tauri::command]
pub async fn get_platform_features() -> PlatformFeatures {
    let os = if cfg!(target_os = "android") {
        "android"
    } else if cfg!(target_os = "ios") {
        "ios"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else {
        "unknown"
    };

    let is_mobile = cfg!(target_os = "android") || cfg!(target_os = "ios");

    // On Android, read the media server URL prefix from managed state
    #[cfg(target_os = "android")]
    let media_url = TAURI_APP.get().and_then(|handle| {
        handle.try_state::<crate::MediaServerState>()
            .map(|state| state.url_prefix.clone())
    });

    #[cfg(not(target_os = "android"))]
    let media_url: Option<String> = None;

    // Android's webview can't seek through a custom scheme, so its media server serves these too.
    #[cfg(target_os = "android")]
    let vmedia = media_url.as_ref().map(|prefix| format!("{prefix}/m")).unwrap_or_default();
    #[cfg(not(target_os = "android"))]
    let vmedia = crate::media_scheme::base_url();
    #[cfg(target_os = "android")]
    let vcache = media_url.as_ref().map(|prefix| format!("{prefix}/c")).unwrap_or_default();
    #[cfg(not(target_os = "android"))]
    let vcache = format!("{vmedia}/c");
    let app_data = vector_core::db::get_app_data_dir()
        .map(|dir| format!("{}{}", dir.to_string_lossy(), std::path::MAIN_SEPARATOR))
        .unwrap_or_default();

    PlatformFeatures {
        transcription: cfg!(feature = "whisper"),
        notification_sounds: cfg!(desktop),
        os: os.to_string(),
        is_mobile,
        debug_mode: cfg!(debug_assertions),
        media_url,
        vmedia,
        vcache,
        app_data,
        vcache_kinds: vector_core::vcache::KINDS.iter().map(|kind| kind.to_string()).collect(),
        self_update: !cfg!(feature = "fdroid"),
        video_compression: crate::message::video_compression::available(),
        // F-Droid counts a built-in YouTube player as promoting a non-free network service.
        players: !cfg!(feature = "fdroid"),
        public_folder: public_folder(),
    }
}

/// The public dir relative to shared storage (Android) or home: Android 8-10 keeps it under
/// `Android/media`, not `Download`.
fn public_folder() -> String {
    let dir = vector_core::db::get_public_dir();
    #[cfg(target_os = "android")]
    let root = vector_core::db::get_download_dir()
        .ancestors()
        .find(|a| a.ends_with("Android"))
        .and_then(std::path::Path::parent)
        .map(std::path::Path::to_path_buf);
    #[cfg(not(target_os = "android"))]
    let root = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(std::path::PathBuf::from);
    root.and_then(|root| dir.strip_prefix(root).ok().map(std::path::Path::to_path_buf))
        .unwrap_or(dir)
        .to_string_lossy()
        .into_owned()
}

/// Periodic memory upkeep, called every ~45s from the JS profile sync loop: idle caches and
/// decoded media are dropped once nothing has used them for a while.
#[tauri::command]
pub async fn run_maintenance() {
    #[cfg(desktop)]
    audio::check_cache_ttl();

    let mut freed = false;
    if let Ok(engine) = crate::audio_engine::AudioEngine::get() {
        freed |= engine.evict_idle() > 0;
    }

    #[cfg(feature = "whisper")]
    {
        freed |= crate::whisper::check_ctx_ttl();
    }

    freed |= crate::message::files::sweep_compression_cache().await > 0;
    freed |= STATE.lock().await.trim_unheld_chats() > 0;

    image_cache::cleanup_stale_downloads().await;

    if freed {
        release_free_heap();
    }
}

/// glibc keeps freed pages in its arenas until told otherwise; the other platforms'
/// allocators return them on their own.
fn release_free_heap() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    // SAFETY: malloc_trim only releases free pages back to the kernel; no allocation moves.
    unsafe {
        libc::malloc_trim(0);
    }
}

/// Get storage information for the Vector directory
#[tauri::command]
pub async fn get_storage_info() -> Result<serde_json::Value, String> {
    let handle = TAURI_APP.get().ok_or("App handle not initialized")?;

    // Canonical media dir: Downloads/vector on desktop, the external media dir on
    // Android (where DOWNLOAD_DIR_OVERRIDE points). Resolving BaseDirectory::Download
    // here instead missed Android's real media location, so attachments never counted.
    let vector_dir = vector_core::db::get_download_dir();

    // Calculate total size and file count
    let mut total_bytes = 0;
    let mut file_count = 0;

    // Track file type distribution by size
    let mut type_distribution = std::collections::HashMap::new();

    // Dotfiles are neither counted nor swept: they're OS/app markers
    // (.nomedia is the Hide-from-Gallery switch, .{hash}.tmp is a staged
    // send), not user content
    let saved: std::collections::HashSet<String> = vector_core::db::saved::all().unwrap_or_default().into_iter().collect();
    let mut walk_dir = |dir: &std::path::Path, only_ext: Option<&str>| {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.starts_with('.') {
                    continue;
                }
                let extension = match name.split('.').next_back() {
                    Some(ext) => ext.to_lowercase(),
                    None => continue,
                };
                if only_ext.is_some_and(|want| extension != want) {
                    continue;
                }
                if std::fs::canonicalize(entry.path()).is_ok_and(|real| saved.contains(&*real.to_string_lossy())) {
                    continue;
                }
                if let Ok(metadata) = entry.metadata() {
                    if metadata.is_file() {
                        let file_size = metadata.len();
                        total_bytes += file_size;
                        file_count += 1;
                        *type_distribution.entry(extension).or_insert(0) += file_size;
                    }
                }
            }
        }
    };
    walk_dir(&vector_dir, None);
    // Marketplace-installed Mini Apps live in app data rather than the download
    // dir, but they're user-visible storage all the same (the Apps slice).
    // Packages only: a crashed update's .xdc.tmp isn't reachable by any sweep
    if let Ok(app_data) = handle.path().app_data_dir() {
        walk_dir(&app_data.join("miniapps").join("marketplace"), Some("xdc"));
    }
    // Kept unencrypted in the public folder: Vector's while unsaved, and nothing else there is.
    let public = vector_core::db::get_public_dir();
    if public != vector_dir {
        let root = public.canonicalize().ok();
        for item in vector_core::media::import::on_disk().unwrap_or_default() {
            let Ok(real) = std::path::Path::new(&item.path).canonicalize() else { continue };
            if !root.as_ref().is_some_and(|r| real.starts_with(r)) || vector_core::db::saved::is_saved(&item.path).unwrap_or(true) {
                continue;
            }
            if let Ok(meta) = std::fs::metadata(&real) {
                total_bytes += meta.len();
                file_count += 1;
                *type_distribution.entry(item.extension.to_lowercase()).or_insert(0) += meta.len();
            }
        }
    }
    // Files kept in the media store, by the type each was kept as.
    for (mime, files, bytes) in vector_core::db::media_store::totals().unwrap_or_default() {
        let ext = vector_core::crypto::extension_from_mime(&mime);
        total_bytes += bytes;
        file_count += files as usize;
        *type_distribution.entry(if ext.is_empty() { "bin".to_string() } else { ext }).or_insert(0) += bytes;
    }

    // Calculate Whisper models size if whisper feature is enabled
    #[cfg(feature = "whisper")]
    {
        // Calculate total size of downloaded Whisper models
        let mut ai_models_size = 0;
        for model in whisper::MODELS {
            if whisper::is_model_downloaded(handle, model.name) {
                // Convert MB to bytes (model sizes are in MB)
                ai_models_size += (model.size as u64) * 1024 * 1024;
            }
        }

        if ai_models_size > 0 {
            // Synthetic bucket key; the '/' can't occur in a filename-derived
            // extension, so a real "foo.ai_models" file can never collide
            *type_distribution.entry("/ai_models".to_string()).or_insert(0) += ai_models_size;
            total_bytes += ai_models_size;
        }
    }

    // Calculate image cache size (avatars, banners, miniapp icons)
    // Cache is global (not per-account) for deduplication across accounts
    if let Ok(cache_size) = image_cache::get_cache_size(handle) {
        if cache_size > 0 {
            *type_distribution.entry("/cache".to_string()).or_insert(0) += cache_size;
            total_bytes += cache_size;
        }
    }

    // Return storage information with type distribution
    Ok(serde_json::json!({
        "path": vector_dir.to_string_lossy().to_string(),
        "total_bytes": total_bytes,
        "file_count": file_count,
        "total_formatted": format_bytes(total_bytes),
        "type_distribution": type_distribution
    }))
}

/// Delete downloaded attachment files and reset their metadata, optionally
/// restricted to a set of lowercase file extensions (None = every attachment).
/// Returns the number of chats that had messages updated.
async fn clear_attachment_files<R: Runtime>(
    handle: &AppHandle<R>,
    exts: Option<&std::collections::HashSet<String>>,
    kept_only: bool,
) -> Result<usize, String> {
    vector_core::db::scoped(async move {
        // Deletion is confined to the download dir: a stale or symlinked
        // attachment path must not be able to reach files elsewhere on disk
        let roots: Vec<std::path::PathBuf> = vector_core::db::download_roots().iter().filter_map(|r| r.canonicalize().ok()).collect();

        // Lock the state to access all chats and messages
        let mut state = STATE.lock().await;

        // Track which chats have been updated to avoid duplicate saves
        let mut updated_chats = std::collections::HashSet::new();

        // Process each chat to clear attachment metadata in messages
        // Use index-based iteration to allow interner access for Message conversion
        for chat_idx in 0..state.chats.len() {
            let mut updated_msg_ids = Vec::new();

            // Iterate through all messages in this chat
            for message in state.chats[chat_idx].messages.iter_mut() {
                let mut attachment_updated = false;

                // Iterate through matching attachments and reset their properties
                for attachment in &mut message.attachments {
                    if !attachment.downloaded() && !attachment.has_path() {
                        continue;
                    }
                    let path = attachment.path();
                    // Downloaded with no path of its own: a copy the media store keeps.
                    let kept = path.is_empty();
                    if kept_only && !kept {
                        continue;
                    }

                    // Category clears match on the on-disk filename extension — the
                    // same derivation the storage chart buckets by
                    if let Some(set) = exts {
                        let ext = if kept {
                            Some(vector_core::crypto::extension_from_mime(vector_core::crypto::mime_from_extension(attachment.extension())))
                        } else {
                            std::path::Path::new(&*path)
                                .file_name()
                                .and_then(|n| n.to_str())
                                .and_then(|n| n.split('.').next_back())
                                .map(|e| e.to_lowercase())
                        };
                        if !ext.is_some_and(|e| set.contains(&e)) {
                            continue;
                        }
                    }

                    // A path that resolves outside the download dir (user-picked
                    // send, symlink) is left fully intact: it isn't Vector's
                    // storage. A path that no longer resolves is a dangling ref:
                    // nothing to delete, but the metadata still needs the reset
                    if kept {
                        vector_core::media::store::forget(attachment.id());
                    } else if let Ok(real) = std::path::Path::new(&*path).canonicalize() {
                        if !roots.iter().any(|root| real.starts_with(root)) {
                            continue;
                        }
                        // A file the user saved stays theirs; Vector only stops showing it.
                        if !vector_core::db::saved::is_saved(&real.to_string_lossy()).unwrap_or(true) {
                            let _ = std::fs::remove_file(&real);
                        }
                    }
                    // Reset attachment properties
                    attachment.set_downloaded(false);
                    attachment.set_downloading(false);
                    attachment.set_path(String::new());
                    attachment_updated = true;
                }

                // If any attachment was updated, track this message for save/emit
                if attachment_updated {
                    updated_msg_ids.push(*message.id());
                }
            }

            // If we have messages to update, save them to the database
            if updated_msg_ids.is_empty() {
                continue;
            }
            let chat_id = state.chats[chat_idx].id().to_string();

            // Convert updated messages to Message format for save and emit
            let messages_to_update: Vec<crate::Message> = updated_msg_ids.iter()
                .filter_map(|msg_id| {
                    let hex_id = crate::util::bytes_to_hex_32(msg_id);
                    state.chats[chat_idx].messages.find_by_hex_id(&hex_id)
                        .map(|m| m.to_message(&state.interner))
                })
                .collect();


            // Save updated messages to database
            db::save_chat_messages(&chat_id, &messages_to_update).await
                .map_err(|e| format!("Failed to save updated messages for chat {}: {}", chat_id, e))?;

            // Emit message_update events for each updated message
            for message in &messages_to_update {
                handle.emit("message_update", serde_json::json!({
                    "old_id": &message.id,
                    "message": message,
                    "chat_id": &chat_id
                })).map_err(|e| format!("Failed to emit message_update for chat {}: {}", chat_id, e))?;
            }

            updated_chats.insert(chat_id);
        }

        Ok(updated_chats.len())
    })
    .await
}

/// Nuke the global image/sound caches and clear cached image paths from profiles
async fn clear_media_caches<R: Runtime>(
    handle: &AppHandle<R>,
) -> Result<(), String> {
    vector_core::db::scoped(async move {
        // Every Vector cache and the resampled sounds, nothing else in the folder: on Android it
        // is the system's own cache directory, where the webview and this launch's exports live.
        let cache_dir = handle.path().app_data_dir()
            .map_err(|e| format!("Failed to get app data dir: {}", e))?
            .join("cache");
        for kind in vector_core::vcache::KINDS.iter().chain(&["sounds"]) {
            let _ = std::fs::remove_dir_all(cache_dir.join(kind));
        }
        crate::commands::emoji_packs::forget_sheets();

        // Clear in-memory notification sound cache (desktop only)
        #[cfg(desktop)]
        audio::purge_sound_cache();

        // Forget the cached paths everywhere, tell the screen, fetch again:
        // the same steps as the image-cache button, or on-screen profiles
        // keep pointing at deleted files and show a broken picture.
        crate::image_cache::forget_profile_images_and_refetch().await;
        Ok(())
    })
    .await
}

/// Delete files with matching extensions from a directory (top level only,
/// mirroring the storage chart's walk). Category clears must sweep untracked
/// files too: the chart counts every file in the folder, not just attachments
/// referenced by the current account.
fn sweep_dir_by_ext(dir: &std::path::Path, exts: &std::collections::HashSet<String>) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let is_file = entry.metadata().map(|m| m.is_file()).unwrap_or(false);
            if !is_file {
                continue;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            // Dotfiles are protected: mirrors the chart walk (.nomedia is the
            // Hide-from-Gallery switch, .{hash}.tmp is a staged send)
            if name.starts_with('.') {
                continue;
            }
            let matches = name
                .split('.')
                .next_back()
                .is_some_and(|e| exts.contains(&e.to_lowercase()));
            if matches && !vector_core::db::saved::is_saved(&entry.path().to_string_lossy()).unwrap_or(true) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}

/// Clear all downloaded attachments from messages and return freed storage space
#[tauri::command]
pub async fn clear_storage<R: Runtime>(handle: AppHandle<R>) -> Result<serde_json::Value, String> {

    // First, get the total storage size before clearing
    let storage_info_before = get_storage_info().await.map_err(|e| format!("Failed to get storage info before clearing: {}", e))?;
    let total_bytes_before = storage_info_before["total_bytes"].as_u64().unwrap_or(0);

    let updated_chats = clear_attachment_files(&handle, None, false).await?;
    vector_core::media::store::forget_where(|_| true);
    clear_media_caches(&handle).await?;
    // Attachment deletion can strand Mini App history rows pointing at nothing
    let _ = crate::db::prune_dangling_miniapp_history();

    // Get storage info after clearing to calculate freed space
    let storage_info_after = get_storage_info().await.map_err(|e| format!("Failed to get storage info after clearing: {}", e))?;
    let total_bytes_after = storage_info_after["total_bytes"].as_u64().unwrap_or(0);

    // Calculate freed space
    let freed_bytes = total_bytes_before.saturating_sub(total_bytes_after);

    // Return the freed storage information
    Ok(serde_json::json!({
        "freed_bytes": freed_bytes,
        "freed_formatted": format_bytes(freed_bytes),
        "updated_chats": updated_chats
    }))
}

/// Clear a single storage category: "cache" (image/sound caches), "ai"
/// (downloaded Whisper models), or any other value = attachment/file sweep
/// restricted to the given extension set.
#[tauri::command]
pub async fn clear_storage_category<R: Runtime>(
    handle: AppHandle<R>,
    category: String,
    exts: Vec<String>,
) -> Result<serde_json::Value, String> {

    let storage_info_before = get_storage_info().await.map_err(|e| format!("Failed to get storage info before clearing: {}", e))?;
    let total_bytes_before = storage_info_before["total_bytes"].as_u64().unwrap_or(0);

    match category.as_str() {
        "cache" => clear_media_caches(&handle).await?,
        "ai" => {
            #[cfg(feature = "whisper")]
            for model in whisper::MODELS {
                if whisper::is_model_downloaded(&handle, model.name) {
                    whisper::delete_whisper_model(handle.clone(), model.name.to_string()).await;
                }
            }
        }
        _ => {
            let ext_set: std::collections::HashSet<String> =
                exts.iter().map(|e| e.to_lowercase()).collect();
            if ext_set.is_empty() {
                return Err("No file types to clear".to_string());
            }
            clear_attachment_files(&handle, Some(&ext_set), false).await?;
            sweep_dir_by_ext(&vector_core::db::get_download_dir(), &ext_set);
            // Kept files of these types that no loaded message names.
            vector_core::media::store::forget_where(|mime| ext_set.contains(&vector_core::crypto::extension_from_mime(mime)));
            // Marketplace installs live in app data, not the download dir;
            // wiping the Apps slice must uninstall them too, or the library
            // keeps offering "Play" on packages that are gone
            if ext_set.contains("xdc") {
                if let Ok(app_data) = handle.path().app_data_dir() {
                    sweep_dir_by_ext(&app_data.join("miniapps").join("marketplace"), &ext_set);
                }
                crate::miniapps::marketplace::sync_install_status_from_disk(&handle).await;
            }
            // File deletion can strand Mini App history rows pointing at nothing
            let _ = crate::db::prune_dangling_miniapp_history();
        }
    }

    let storage_info_after = get_storage_info().await.map_err(|e| format!("Failed to get storage info after clearing: {}", e))?;
    let total_bytes_after = storage_info_after["total_bytes"].as_u64().unwrap_or(0);
    let freed_bytes = total_bytes_before.saturating_sub(total_bytes_after);

    Ok(serde_json::json!({
        "freed_bytes": freed_bytes,
        "freed_formatted": format_bytes(freed_bytes)
    }))
}

/// The link media Keep Files seals into the cache: what a chat shows from a link.
const KEPT_LINK_MEDIA: [&str; 2] = ["inline_images", "embed_videos"];

const KEEP_MEDIA_BUSY: &str = "Turn this off once your downloads finish.";

/// Whether an attachment is downloading: one that read the old setting could still keep a file.
async fn downloads_running() -> bool {
    !crate::commands::attachments::ACTIVE_DOWNLOADS.lock().await.is_empty()
}

/// Old downloads still in the download folder: how many, and their size.
async fn old_downloads() -> (u64, u64) {
    let Ok((Ok(files), Some(shown))) = vector_core::db::blocking_bound(find_old_downloads).await else { return (0, 0) };
    let goes: Vec<_> = files.iter().filter(|f| !shown.shows(f)).collect();
    let bytes = goes.iter().filter_map(|f| std::fs::metadata(&f.path).ok()).map(|m| m.len()).sum();
    (goes.len() as u64, bytes)
}

fn link_media_dirs<R: Runtime>(handle: &AppHandle<R>) -> Vec<std::path::PathBuf> {
    handle.path().app_data_dir().map(|d| KEPT_LINK_MEDIA.iter().map(|k| d.join("cache").join(k)).collect()).unwrap_or_default()
}

/// What turning Keep Files off would delete: the files it kept and the link media sealed with them.
#[tauri::command]
pub async fn kept_media_summary<R: Runtime>(handle: AppHandle<R>) -> Result<serde_json::Value, String> {
    let (mut files, mut bytes) = vector_core::db::blocking_bound(vector_core::media::store::kept_files).await?;
    let (old, old_bytes) = old_downloads().await;
    (files, bytes) = (files + old, bytes + old_bytes);
    for dir in link_media_dirs(&handle) {
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            if let Some(meta) = entry.metadata().ok().filter(|m| m.is_file()) {
                files += 1;
                bytes += meta.len();
            }
        }
    }
    Ok(serde_json::json!({ "files": files, "bytes": bytes, "busy": downloads_running().await }))
}

/// Keep Files on or off. Off deletes what it kept, so a file is kept or held, never both; refused
/// while a download that read the old setting could still keep something.
#[tauri::command]
pub async fn set_keep_media<R: Runtime>(handle: AppHandle<R>, on: bool) -> Result<(), String> {
    vector_core::db::scoped(async move {
        let set = |on: bool| vector_core::db::set_sql_setting("keep_media".into(), on.to_string());
        if on {
            set(true)?;
            start_media_port(std::time::Duration::ZERO);
            return Ok(());
        }
        if downloads_running().await || !stop_media_port().await {
            return Err(KEEP_MEDIA_BUSY.into());
        }
        set(false)?;
        if downloads_running().await {
            let _ = set(true);
            return Err(KEEP_MEDIA_BUSY.into());
        }
        vector_core::db::set_sql_setting("keep_unencrypted".into(), "false".into())?;
        clear_attachment_files(&handle, None, true).await?;
        vector_core::db::blocking_bound(|| vector_core::media::store::forget_where(|_| true)).await?;
        for dir in link_media_dirs(&handle) {
            let _ = std::fs::remove_dir_all(dir);
        }
        release_old_downloads().await;
        Ok(())
    })
    .await
}

/// Keep Unencrypted on or off: what is kept moves, paced, to plain files in the public folder or
/// back into the store. Refused while a download could still keep a file the old way.
#[tauri::command]
pub async fn set_keep_unencrypted(on: bool) -> Result<(), String> {
    vector_core::db::scoped(async move {
        if on && !vector_core::media::keep_media() {
            return Err("Turn on Keep Files on This Device first.".into());
        }
        if downloads_running().await || !stop_media_port().await {
            return Err("Change this once your downloads finish.".into());
        }
        vector_core::db::set_sql_setting("keep_unencrypted".into(), on.to_string())?;
        start_media_port(std::time::Duration::ZERO);
        Ok(())
    })
    .await
}

static MOVING_DOWNLOADS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static PORT_STOP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// How fast old downloads move in, as source bytes a second: slow enough to go unnoticed.
#[cfg(target_os = "android")]
const PORT_RATE: f64 = 4.0 * 1024.0 * 1024.0;
#[cfg(not(target_os = "android"))]
const PORT_RATE: f64 = 12.0 * 1024.0 * 1024.0;

/// Move this account's old downloads into Vector in the background, a file at a time and
/// paced, while it keeps media. Each file is whole in Vector or still a download, so a launch
/// that cuts it short simply picks it up on the next. Reports `media_import_progress`.
pub fn start_media_port(delay: std::time::Duration) {
    use std::sync::atomic::Ordering;
    use vector_core::media::import::{self, Moved};
    // Originals go a batch at a time, after the rows that replace them are synced.
    const BATCH: usize = 32;
    if MOVING_DOWNLOADS.swap(true, Ordering::AcqRel) {
        return;
    }
    PORT_STOP.store(false, Ordering::Release);
    vector_core::db::spawn_bound(async move {
        struct Done;
        impl Drop for Done {
            fn drop(&mut self) {
                MOVING_DOWNLOADS.store(false, Ordering::Release);
            }
        }
        let _done = Done;
        let stopped = || PORT_STOP.load(Ordering::Acquire) || !vector_core::db::writes_to_live_account();
        let start = tokio::time::Instant::now() + delay;
        while tokio::time::Instant::now() < start && !stopped() {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
        if stopped() || !vector_core::media::keep_media() {
            return;
        }
        if vector_core::media::keep_unencrypted() {
            move_out(&stopped).await;
            return;
        }
        settle_owed(import::settle).await;
        let Ok((Ok(files), elsewhere)) = vector_core::db::blocking_bound(find_old_downloads).await else { return };
        let elsewhere = std::sync::Arc::new(elsewhere);
        let mut owed: Vec<String> = Vec::new();
        for (i, item) in files.iter().enumerate() {
            // The app in the background moves nothing.
            #[cfg(target_os = "android")]
            while !crate::android::background_sync::is_activity_in_foreground() && !stopped() {
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
            if stopped() {
                break;
            }
            let len = std::fs::metadata(&item.path).map_or(0, |m| m.len());
            let (file, others) = (item.clone(), elsewhere.clone());
            match vector_core::db::blocking_bound(move || import::move_in(&file, others.as_ref().as_ref())).await {
                Ok(Ok((outcome, events))) => {
                    if outcome == Moved::Moved {
                        owed.push(item.path.clone());
                    }
                    reflect_move(item, outcome, &events).await;
                }
                Ok(Err(e)) => vector_core::log_warn!("[MediaImport] couldn't move a file in: {}", e),
                Err(_) => break,
            }
            if owed.len() >= BATCH {
                let (batch, others) = (std::mem::take(&mut owed), elsewhere.clone());
                settle_owed(move || import::settle_batch(&batch, others.as_ref().as_ref())).await;
            }
            vector_core::traits::emit_event("media_import_progress", &serde_json::json!({ "done": i + 1, "total": files.len() }));
            let pace = std::time::Duration::from_secs_f64(len as f64 / PORT_RATE).max(std::time::Duration::from_millis(150));
            tokio::time::sleep(pace).await;
        }
        if !owed.is_empty() {
            let others = elsewhere.clone();
            settle_owed(move || import::settle_batch(&owed, others.as_ref().as_ref())).await;
        }
        vector_core::traits::emit_event("media_import_progress", &serde_json::json!({ "done": files.len(), "total": files.len() }));
    });
}

/// Kept files made plain in the public folder, a file at a time and paced, for an account that
/// keeps media unencrypted. Each is whole in one place or the other, so a cut-short launch
/// resumes on the next.
async fn move_out(stopped: &impl Fn() -> bool) {
    let Ok(Ok(kept)) = vector_core::db::blocking_bound(vector_core::db::attachments::kept_names).await else { return };
    for (i, (hash, name, _)) in kept.iter().enumerate() {
        #[cfg(target_os = "android")]
        while !crate::android::background_sync::is_activity_in_foreground() && !stopped() {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        }
        if stopped() {
            break;
        }
        let len = vector_core::media::open(hash).map_or(0, |src| src.len());
        let (hash, name) = (*hash, name.clone());
        match vector_core::db::blocking_bound(move || crate::media_export::unseal(&hash, &name)).await {
            Ok(Ok((path, _))) => reflect_unsealed(&hash, &path.to_string_lossy()).await,
            Ok(Err(e)) => vector_core::log_warn!("[MediaExport] couldn't keep a file unencrypted: {}", e),
            Err(_) => break,
        }
        vector_core::traits::emit_event("media_import_progress", &serde_json::json!({ "done": i + 1, "total": kept.len(), "plain": true }));
        let pace = std::time::Duration::from_secs_f64(len as f64 / PORT_RATE).max(std::time::Duration::from_millis(150));
        tokio::time::sleep(pace).await;
    }
    vector_core::traits::emit_event("media_import_progress", &serde_json::json!({ "done": kept.len(), "total": kept.len(), "plain": true }));
}

/// STATE and the open chat show a kept file from its plain copy.
async fn reflect_unsealed(hash: &[u8; 32], path: &str) {
    let mut state = STATE.lock().await;
    let mut touched = Vec::new();
    for (chat_idx, chat) in state.chats.iter_mut().enumerate() {
        for m in chat.messages.iter_mut() {
            let mut changed = false;
            for att in m.attachments.iter_mut().filter(|a| a.id() == hash && a.downloaded() && a.path().is_empty()) {
                att.set_path(path.to_string());
                changed = true;
            }
            if changed {
                touched.push((chat_idx, *m.id()));
            }
        }
    }
    let updates: Vec<(String, crate::Message)> = touched
        .into_iter()
        .filter_map(|(chat_idx, id)| {
            let chat = &state.chats[chat_idx];
            chat.messages.find_by_hex_id(&crate::util::bytes_to_hex_32(&id)).map(|m| (chat.id().clone(), m.to_message(&state.interner)))
        })
        .collect();
    drop(state);
    for (chat_id, mut message) in updates {
        let old_id = message.id.clone();
        vector_core::traits::emit_message_update(&chat_id, &old_id, &mut message).await;
    }
}

/// Stop the port between files. False when it is still in a file after a while.
async fn stop_media_port() -> bool {
    use std::sync::atomic::Ordering;
    PORT_STOP.store(true, Ordering::Release);
    for _ in 0..150 {
        if !MOVING_DOWNLOADS.load(Ordering::Acquire) {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    false
}

/// Delete originals a move owes. Returns how many went and how many stayed, or None when none
/// could be judged; those stay owed.
pub async fn settle_owed<F>(settle: F) -> Option<(usize, usize)>
where
    F: FnOnce() -> Result<vector_core::media::import::Settled, String> + Send + 'static,
{
    match vector_core::db::blocking_bound(settle).await.ok()? {
        Ok(settled) => {
            #[cfg(target_os = "android")]
            for path in &settled.gone {
                crate::android::storage::scan_file(path);
            }
            Some((settled.gone.len(), settled.stayed))
        }
        Err(e) => {
            vector_core::log_warn!("[MediaImport] originals stay owed for now: {}", e);
            None
        }
    }
}

/// Plain downloads go with files that aren't kept: each file this account alone shows is
/// deleted, and loaded messages stop showing it.
/// This account's old downloads, rows older versions left unmarked included, and what other
/// accounts show.
fn find_old_downloads() -> (Result<Vec<vector_core::media::import::OnDisk>, String>, Option<vector_core::media::import::Elsewhere>) {
    use vector_core::media::import;
    if let Err(e) = import::reclaim_unmarked() {
        vector_core::log_warn!("[MediaImport] couldn't check downloads left unmarked: {}", e);
    }
    (import::on_disk(), import::elsewhere())
}

async fn release_old_downloads() {
    use vector_core::media::import;
    let Ok((Ok(files), elsewhere)) = vector_core::db::blocking_bound(find_old_downloads).await else { return };
    let elsewhere = std::sync::Arc::new(elsewhere);
    for item in files {
        let (file, others) = (item.clone(), elsewhere.clone());
        match vector_core::db::blocking_bound(move || import::release(&file, others.as_ref().as_ref())).await {
            Ok(Ok(events)) => {
                #[cfg(target_os = "android")]
                crate::android::storage::scan_file(&item.path);
                let updates = import::reflect_released(&mut *STATE.lock().await, &item, &events);
                for (chat_id, mut message) in updates {
                    let old_id = message.id.clone();
                    vector_core::traits::emit_message_update(&chat_id, &old_id, &mut message).await;
                }
            }
            Ok(Err(e)) => vector_core::log_warn!("[MediaImport] couldn't let a download go: {}", e),
            Err(_) => return,
        }
    }
}

/// STATE and the open chat follow a moved file.
async fn reflect_move(item: &vector_core::media::import::OnDisk, outcome: vector_core::media::import::Moved, events: &[String]) {
    let updates = vector_core::media::import::reflect(&mut *STATE.lock().await, item, outcome, events);
    for (chat_id, mut message) in updates {
        let old_id = message.id.clone();
        vector_core::traits::emit_message_update(&chat_id, &old_id, &mut message).await;
    }
}

// ============================================================================
// Battery Optimization & Background Service Commands
// ============================================================================

/// Check whether the app is exempt from battery optimizations (Android only).
/// Returns `true` on non-Android platforms (no optimization needed).
#[tauri::command]
pub async fn check_battery_optimized() -> bool {
    #[cfg(target_os = "android")]
    {
        call_battery_helper_bool("isIgnoringBatteryOptimizations")
    }
    #[cfg(not(target_os = "android"))]
    {
        true
    }
}

/// Open the system battery optimization dialog (Android only). No-op on other platforms.
#[tauri::command]
pub async fn request_battery_optimization() {
    #[cfg(target_os = "android")]
    {
        call_battery_helper_void("requestBatteryOptimization");
    }
}

/// Get the background service enabled preference.
/// Returns `true` on non-Android platforms.
#[tauri::command]
pub async fn get_background_service_enabled() -> bool {
    #[cfg(target_os = "android")]
    {
        call_battery_helper_bool("getBackgroundServiceEnabled")
    }
    #[cfg(not(target_os = "android"))]
    {
        true
    }
}

/// Set the background service enabled preference and start/stop the service accordingly.
#[tauri::command]
pub async fn set_background_service_enabled(enabled: bool) {
    #[cfg(target_os = "android")]
    {
        call_battery_helper_set_enabled(enabled);
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = enabled;
    }
}

/// Check whether the user has been prompted for background service setup.
/// Returns `true` on non-Android platforms (no prompt needed).
#[tauri::command]
pub async fn get_background_service_prompted() -> bool {
    #[cfg(target_os = "android")]
    {
        call_battery_helper_bool("getBackgroundServicePrompted")
    }
    #[cfg(not(target_os = "android"))]
    {
        true
    }
}

/// Mark the user as having been prompted for background service setup.
#[tauri::command]
pub async fn set_background_service_prompted() {
    #[cfg(target_os = "android")]
    {
        call_battery_helper_void("setBackgroundServicePrompted");
    }
}

// ============================================================================
// Android JNI helpers for VectorBatteryHelper
// Uses ndk_context (Tauri's Activity context) — always available when Tauri
// commands execute, unlike BG_JAVA_VM which depends on service startup timing.
// ============================================================================

#[cfg(target_os = "android")]
fn call_battery_helper_bool(method: &str) -> bool {
    let default = method == "getBackgroundServiceEnabled";
    crate::android::utils::with_android_context(|env, activity| {
        let class_loader = env.call_method(activity, "getClassLoader", "()Ljava/lang/ClassLoader;", &[])
            .map_err(|e| format!("{:?}", e))?.l().map_err(|e| format!("{:?}", e))?;

        let class_name = env.new_string("io.vectorapp.VectorBatteryHelper")
            .map_err(|e| format!("{:?}", e))?;
        let helper_class = env.call_method(&class_loader, "loadClass", "(Ljava/lang/String;)Ljava/lang/Class;",
            &[jni::objects::JValue::Object(&class_name)])
            .map_err(|e| format!("{:?}", e))?.l().map_err(|e| format!("{:?}", e))?;

        let helper_jclass = jni::objects::JClass::from(helper_class);
        let val = env.call_static_method(&helper_jclass, method, "(Landroid/content/Context;)Z",
            &[activity.into()])
            .map_err(|e| format!("{:?}", e))?.z().map_err(|e| format!("{:?}", e))?;
        Ok(val)
    }).unwrap_or(default)
}

#[cfg(target_os = "android")]
fn call_battery_helper_void(method: &str) {
    let _ = crate::android::utils::with_android_context(|env, activity| {
        let class_loader = env.call_method(activity, "getClassLoader", "()Ljava/lang/ClassLoader;", &[])
            .map_err(|e| format!("{:?}", e))?.l().map_err(|e| format!("{:?}", e))?;

        let class_name = env.new_string("io.vectorapp.VectorBatteryHelper")
            .map_err(|e| format!("{:?}", e))?;
        let helper_class = env.call_method(&class_loader, "loadClass", "(Ljava/lang/String;)Ljava/lang/Class;",
            &[jni::objects::JValue::Object(&class_name)])
            .map_err(|e| format!("{:?}", e))?.l().map_err(|e| format!("{:?}", e))?;

        let helper_jclass = jni::objects::JClass::from(helper_class);
        env.call_static_method(&helper_jclass, method, "(Landroid/content/Context;)V",
            &[activity.into()])
            .map_err(|e| format!("{:?}", e))?;
        Ok(())
    });
}

#[cfg(target_os = "android")]
fn call_battery_helper_set_enabled(enabled: bool) {
    let _ = crate::android::utils::with_android_context(|env, activity| {
        let class_loader = env.call_method(activity, "getClassLoader", "()Ljava/lang/ClassLoader;", &[])
            .map_err(|e| format!("{:?}", e))?.l().map_err(|e| format!("{:?}", e))?;

        let class_name = env.new_string("io.vectorapp.VectorBatteryHelper")
            .map_err(|e| format!("{:?}", e))?;
        let helper_class = env.call_method(&class_loader, "loadClass", "(Ljava/lang/String;)Ljava/lang/Class;",
            &[jni::objects::JValue::Object(&class_name)])
            .map_err(|e| format!("{:?}", e))?.l().map_err(|e| format!("{:?}", e))?;

        let helper_jclass = jni::objects::JClass::from(helper_class);

        // Set the preference
        env.call_static_method(&helper_jclass, "setBackgroundServiceEnabled",
            "(Landroid/content/Context;Z)V",
            &[activity.into(), jni::objects::JValue::Bool(enabled as u8)])
            .map_err(|e| format!("{:?}", e))?;

        // Start or stop the service
        let service_method = if enabled { "startBackgroundService" } else { "stopBackgroundService" };
        env.call_static_method(&helper_jclass, service_method, "(Landroid/content/Context;)V",
            &[activity.into()])
            .map_err(|e| format!("{:?}", e))?;

        Ok(())
    });
}

// ============================================================================
// Device Info
// ============================================================================

/// Returns total system RAM in bytes (0 = unknown, frontend treats as unlimited)
#[tauri::command]
pub async fn get_device_memory() -> u64 {
    get_total_memory()
}

#[cfg(any(target_os = "android", target_os = "linux"))]
fn get_total_memory() -> u64 {
    // Parse /proc/meminfo for MemTotal (always available on Linux/Android)
    if let Ok(contents) = std::fs::read_to_string("/proc/meminfo") {
        for line in contents.lines() {
            if let Some(rest) = line.strip_prefix("MemTotal:") {
                // Format: "       XXXX kB"
                if let Some(kb_str) = rest.trim().strip_suffix("kB").or_else(|| rest.trim().strip_suffix("KB")) {
                    if let Ok(kb) = kb_str.trim().parse::<u64>() {
                        return kb * 1024;
                    }
                }
            }
        }
    }
    0
}

#[cfg(target_os = "macos")]
fn get_total_memory() -> u64 {
    // Use sysctl hw.memsize (returns total bytes directly)
    extern "C" {
        fn sysctl(
            name: *const i32, namelen: u32,
            oldp: *mut std::ffi::c_void, oldlenp: *mut usize,
            newp: *mut std::ffi::c_void, newlen: usize,
        ) -> i32;
    }
    // CTL_HW = 6, HW_MEMSIZE = 24
    let mib: [i32; 2] = [6, 24];
    let mut memsize: u64 = 0;
    let mut size = std::mem::size_of::<u64>();
    unsafe {
        if sysctl(
            mib.as_ptr(), 2,
            &mut memsize as *mut u64 as *mut _,
            &mut size,
            std::ptr::null_mut(), 0,
        ) == 0
        {
            return memsize;
        }
    }
    0
}

#[cfg(windows)]
fn get_total_memory() -> u64 {
    use std::mem;

    #[repr(C)]
    struct MemoryStatusEx {
        dw_length: u32,
        dw_memory_load: u32,
        ull_total_phys: u64,
        ull_avail_phys: u64,
        ull_total_page_file: u64,
        ull_avail_page_file: u64,
        ull_total_virtual: u64,
        ull_avail_virtual: u64,
        ull_avail_extended_virtual: u64,
    }

    extern "system" {
        fn GlobalMemoryStatusEx(lp_buffer: *mut MemoryStatusEx) -> i32;
    }

    let mut status = MemoryStatusEx {
        dw_length: mem::size_of::<MemoryStatusEx>() as u32,
        dw_memory_load: 0,
        ull_total_phys: 0,
        ull_avail_phys: 0,
        ull_total_page_file: 0,
        ull_avail_page_file: 0,
        ull_total_virtual: 0,
        ull_avail_virtual: 0,
        ull_avail_extended_virtual: 0,
    };

    unsafe {
        if GlobalMemoryStatusEx(&mut status) != 0 {
            return status.ull_total_phys;
        }
    }
    0
}

#[cfg(not(any(target_os = "android", target_os = "linux", target_os = "macos", windows)))]
fn get_total_memory() -> u64 {
    0
}

/// Read the log file (errors + panics, written by log_error! and panic hook).
/// Returns the log contents, or an empty string if no log file exists.
#[tauri::command]
pub async fn get_logs(handle: AppHandle) -> String {
    let log_path = match handle.path().app_data_dir() {
        Ok(dir) => dir.join("vector.log"),
        Err(_) => return String::new(),
    };
    std::fs::read_to_string(log_path).unwrap_or_default()
}

// Handler list for this module (for reference):
// - get_platform_features
// - run_maintenance
// - get_storage_info
// - clear_storage
// - clear_storage_category
// - get_device_memory
// - get_crash_log
// - check_battery_optimized
// - request_battery_optimization
// - get_background_service_enabled
// - set_background_service_enabled
// - get_background_service_prompted
// - set_background_service_prompted
