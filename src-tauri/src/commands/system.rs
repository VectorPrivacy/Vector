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
    /// Whether this build looks for and installs its own updates. Off in the
    /// F-Droid flavour, where the store owns updates.
    pub self_update: bool,
    /// Whether videos can be compressed before sending (the `video` build and an encoder).
    pub video_compression: bool,
    /// Whether this build can host an embedded video player.
    pub players: bool,
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
        self_update: !cfg!(feature = "fdroid"),
        video_compression: crate::message::video_compression::available(),
        // F-Droid counts a built-in YouTube player as promoting a non-free network service.
        players: !cfg!(feature = "fdroid"),
    }
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
) -> Result<usize, String> {
    vector_core::db::scoped(async move {
        // Deletion is confined to the download dir: a stale or symlinked
        // attachment path must not be able to reach files elsewhere on disk
        let download_dir = vector_core::db::get_download_dir().canonicalize().ok();

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
                        match &download_dir {
                            Some(dir) if real.starts_with(dir) => {
                                let _ = std::fs::remove_file(&real);
                            }
                            _ => continue,
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
        // Clear all disk caches (images, sounds, etc.) by nuking the cache directory
        let cache_dir = handle.path().app_data_dir()
            .map_err(|e| format!("Failed to get app data dir: {}", e))?
            .join("cache");
        if cache_dir.exists() {
            let _ = std::fs::remove_dir_all(&cache_dir);
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
            if matches {
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

    let updated_chats = clear_attachment_files(&handle, None).await?;
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
            clear_attachment_files(&handle, Some(&ext_set)).await?;
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

/// How many of this account's downloads still sit in the download folder.
#[tauri::command]
pub async fn count_downloads_on_disk() -> Result<usize, String> {
    vector_core::db::blocking_bound(|| vector_core::media::import::on_disk().map(|files| files.len())).await?
}

static MOVING_DOWNLOADS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Move this account's downloads out of the download folder and into Vector, reporting each
/// file as `media_import_progress`. Stops if the account changes.
#[tauri::command]
pub async fn move_downloads_into_vector() -> Result<serde_json::Value, String> {
    use std::sync::atomic::Ordering;
    use vector_core::media::import::{self, Moved};
    if MOVING_DOWNLOADS.swap(true, Ordering::AcqRel) {
        return Err("Already moving files into Vector".to_string());
    }
    vector_core::db::spawn_bound(async move {
        struct Done;
        impl Drop for Done {
            fn drop(&mut self) {
                MOVING_DOWNLOADS.store(false, Ordering::Release);
            }
        }
        let _done = Done;
        let (files, elsewhere) = vector_core::db::blocking_bound(|| (import::on_disk(), import::paths_elsewhere())).await?;
        let (files, elsewhere) = (files?, std::sync::Arc::new(elsewhere));
        let (mut moved, mut shared, mut skipped, mut failed) = (0usize, 0usize, 0usize, 0usize);
        for (i, item) in files.iter().enumerate() {
            if !vector_core::db::writes_to_live_account() {
                break;
            }
            let (file, others) = (item.clone(), elsewhere.clone());
            match vector_core::db::blocking_bound(move || import::move_in(&file, others.as_ref().as_ref())).await? {
                Ok((outcome, events)) => {
                    match outcome {
                        Moved::Moved => moved += 1,
                        Moved::Shared => shared += 1,
                        Moved::Mismatch => skipped += 1,
                    }
                    reflect_move(item, outcome, &events).await;
                }
                Err(e) => {
                    failed += 1;
                    vector_core::log_warn!("[MediaImport] couldn't move a file in: {}", e);
                }
            }
            vector_core::traits::emit_event("media_import_progress", &serde_json::json!({ "done": i + 1, "total": files.len() }));
        }
        Ok(serde_json::json!({ "moved": moved, "shared": shared, "skipped": skipped, "failed": failed }))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// STATE and the open chat follow a moved file.
async fn reflect_move(item: &vector_core::media::import::OnDisk, outcome: vector_core::media::import::Moved, events: &[String]) {
    let updates = vector_core::media::import::reflect(&mut *STATE.lock().await, item, outcome, events);
    #[cfg(target_os = "android")]
    if outcome == vector_core::media::import::Moved::Moved {
        crate::android::storage::scan_file(&item.path);
    }
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
