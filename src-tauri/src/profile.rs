use tauri::Emitter;

#[cfg(not(target_os = "android"))]
use std::sync::Arc;
#[cfg(not(target_os = "android"))]
use tauri_plugin_fs::FsExt;

use crate::{STATE, TAURI_APP};
use crate::db;
use crate::image_cache::{self, CacheResult};
#[cfg(not(target_os = "android"))]
use crate::message::AttachmentFile;

#[cfg(target_os = "android")]
use crate::android::filesystem;

pub use vector_core::profile::{Profile, ProfileFlags};

/// Cache profile images (avatar and banner) in the background
///
/// This downloads and caches the avatar/banner images for offline access.
/// Cache is stored globally (not per-account) for deduplication across accounts.
pub async fn cache_profile_images(npub: &str, avatar_url: &str, banner_url: &str) {
    let handle = match TAURI_APP.get() {
        Some(h) => h,
        None => return,
    };

    let mut avatar_cached = String::new();
    let mut banner_cached = String::new();

    // Cache avatar if URL exists
    if !avatar_url.is_empty() {
        match image_cache::cache_avatar(handle, avatar_url).await {
            CacheResult::Cached(path) | CacheResult::AlreadyCached(path) => {
                avatar_cached = path;
            }
            CacheResult::Failed(e) => {
                log_warn!("[Profile] Failed to cache avatar for {}: {}", npub, e);
            }
        }
    }

    // Cache banner if URL exists
    if !banner_url.is_empty() {
        match image_cache::cache_banner(handle, banner_url).await {
            CacheResult::Cached(path) | CacheResult::AlreadyCached(path) => {
                banner_cached = path;
            }
            CacheResult::Failed(e) => {
                log_warn!("[Profile] Failed to cache banner for {}: {}", npub, e);
            }
        }
    }

    // Update the profile with cached paths if we got any
    if !avatar_cached.is_empty() || !banner_cached.is_empty() {
        let mut state = STATE.lock().await;
        let id = match state.interner.lookup(npub) {
            Some(id) => id,
            None => return,
        };
        let updated = if let Some(profile) = state.get_profile_mut_by_id(id) {
            let mut changed = false;
            if !avatar_cached.is_empty() && *profile.avatar_cached != *avatar_cached {
                profile.avatar_cached = avatar_cached.into_boxed_str();
                changed = true;
            }
            if !banner_cached.is_empty() && *profile.banner_cached != *banner_cached {
                profile.banner_cached = banner_cached.into_boxed_str();
                changed = true;
            }
            changed
        } else { false };

        if updated {
            let slim = state.serialize_profile(id).unwrap();
            handle.emit("profile_update", &slim).ok();
            drop(state);
            db::set_profile(slim).await.ok();
        }
    }
}

/// Cache images for all profiles that have avatar/banner URLs but no cached paths
/// Called on startup to populate the cache for existing profiles
/// Cache is stored globally (not per-account) for deduplication across accounts.
pub async fn cache_all_profile_images() {
    let handle = match TAURI_APP.get() {
        Some(h) => h,
        None => return,
    };

    // Get all profiles that need caching (resolve npub from interner)
    let profiles_to_cache: Vec<(String, String, String)> = {
        let state = STATE.lock().await;
        state.profiles.iter()
            .filter(|p| {
                (!p.avatar.is_empty() && p.avatar_cached.is_empty()) ||
                (!p.banner.is_empty() && p.banner_cached.is_empty())
            })
            .filter_map(|p| {
                state.interner.resolve(p.id)
                    .map(|npub| (npub.to_string(), p.avatar.to_string(), p.banner.to_string()))
            })
            .collect()
    };

    if profiles_to_cache.is_empty() {
        return;
    }

    log_info!("[Profile] Caching images for {} profiles", profiles_to_cache.len());

    // Spawn caching tasks for each profile (they run concurrently with semaphore limiting)
    for (npub, avatar_url, banner_url) in profiles_to_cache {
        let handle = handle.clone();
        tokio::spawn(async move {
            // Cache avatar if needed
            if !avatar_url.is_empty() {
                if let CacheResult::Cached(path) | CacheResult::AlreadyCached(path) =
                    image_cache::cache_avatar(&handle, &avatar_url).await
                {
                    let mut state = STATE.lock().await;
                    if let Some(id) = state.interner.lookup(&npub) {
                        let needs_emit = {
                            if let Some(profile) = state.get_profile_mut_by_id(id) {
                                if profile.avatar_cached.is_empty() {
                                    profile.avatar_cached = path.into_boxed_str();
                                    true
                                } else { false }
                            } else { false }
                        };
                        if needs_emit {
                            let slim = state.serialize_profile(id).unwrap();
                            handle.emit("profile_update", &slim).ok();
                            drop(state);
                            db::set_profile(slim).await.ok();
                        }
                    }
                }
            }

            // Cache banner if needed
            if !banner_url.is_empty() {
                if let CacheResult::Cached(path) | CacheResult::AlreadyCached(path) =
                    image_cache::cache_banner(&handle, &banner_url).await
                {
                    let mut state = STATE.lock().await;
                    if let Some(id) = state.interner.lookup(&npub) {
                        let needs_emit = {
                            if let Some(profile) = state.get_profile_mut_by_id(id) {
                                if profile.banner_cached.is_empty() {
                                    profile.banner_cached = path.into_boxed_str();
                                    true
                                } else { false }
                            } else { false }
                        };
                        if needs_emit {
                            let slim = state.serialize_profile(id).unwrap();
                            handle.emit("profile_update", &slim).ok();
                            drop(state);
                            db::set_profile(slim).await.ok();
                        }
                    }
                }
            }
        });
    }
}

/// Fetch a profile's metadata and status from relays.
/// Delegates to vector-core's `load_profile` with `TauriProfileSyncHandler`.
#[tauri::command]
pub async fn load_profile(npub: String) -> bool {
    vector_core::profile::sync::load_profile(
        npub,
        &crate::profile_sync::TauriProfileSyncHandler,
    ).await
}

/// Update the current user's profile metadata and broadcast to relays.
/// Delegates to vector-core with `TauriProfileSyncHandler`.
#[tauri::command]
pub async fn update_profile(name: String, avatar: String, banner: String, about: String) -> bool {
    vector_core::profile::sync::update_profile(
        name, avatar, banner, about,
        &crate::profile_sync::TauriProfileSyncHandler,
    ).await
}

/// Update the current user's status and broadcast to relays.
/// Delegates to vector-core (no handler needed — status is ephemeral).
#[tauri::command]
pub async fn update_status(status: String) -> bool {
    vector_core::profile::sync::update_status(status).await
}

/// Uploads an avatar or banner image with progress reporting
/// `upload_type` should be "avatar" or "banner" to specify which is being uploaded
#[tauri::command]
pub async fn upload_avatar(filepath: String, upload_type: Option<String>) -> Result<String, String> {
    let handle = TAURI_APP.get().unwrap();
    let upload_type = upload_type.unwrap_or_else(|| "avatar".to_string());

    // Grab the file as AttachmentFile
    let attachment_file = {
        #[cfg(not(target_os = "android"))]
        {
            // Read file bytes
            let bytes = handle.fs().read(std::path::Path::new(&filepath))
                .map_err(|_| "Image couldn't be loaded from disk")?;

            // Extract extension from filepath
            let extension = filepath
                .rsplit('.')
                .next()
                .unwrap_or("bin")
                .to_lowercase();

            AttachmentFile {
                bytes: Arc::new(bytes),
                img_meta: None,
                extension,
                name: String::new(),
            }
        }
        #[cfg(target_os = "android")]
        {
            filesystem::read_android_uri(filepath)?
        }
    };

    // Format a Mime Type from the file extension
    let mime_type = crate::util::mime_from_extension_safe(&attachment_file.extension, true)
        .map_err(|_| "File type is not allowed for avatars (only images are permitted)")?;

    // Upload the file to the server using Blossom with automatic failover and progress
    let signer = crate::MY_SECRET_KEY.to_keys().expect("Keys not initialized");
    let servers = crate::get_blossom_servers();

    // Create progress callback that emits events to frontend
    let handle_clone = handle.clone();
    let upload_type_clone = upload_type.clone();
    let progress_callback: crate::blossom::ProgressCallback = std::sync::Arc::new(move |percentage, bytes_uploaded| {
        let payload = serde_json::json!({
            "type": upload_type_clone,
            "progress": percentage.unwrap_or(0),
            "bytes": bytes_uploaded.unwrap_or(0)
        });
        handle_clone.emit("profile_upload_progress", payload)
            .map_err(|_| "Failed to emit progress event".to_string())
    });

    // Keep a copy of bytes for pre-caching
    let bytes_for_cache = attachment_file.bytes.clone();

    // Upload using Blossom with progress tracking and failover
    let upload_url = crate::blossom::upload_blob_with_progress_and_failover(
        signer.clone(),
        servers,
        attachment_file.bytes,
        Some(mime_type.as_str()),
        progress_callback,
        None, // No retries per server
        None, // Default retry spacing
        None, // No cancel flag
    )
    .await?;

    // Pre-cache the uploaded image so it displays immediately without re-downloading
    let image_type = if upload_type == "banner" {
        image_cache::ImageType::Banner
    } else {
        image_cache::ImageType::Avatar
    };
    image_cache::precache_image_bytes(&handle, &upload_url, &bytes_for_cache, image_type);

    Ok(upload_url)
}


/// Block a user by npub.
#[tauri::command]
pub async fn block_user(npub: String) -> bool {
    vector_core::profile::sync::block_user(npub, &crate::profile_sync::TauriProfileSyncHandler).await
}

/// Unblock a user by npub.
#[tauri::command]
pub async fn unblock_user(npub: String) -> bool {
    vector_core::profile::sync::unblock_user(npub, &crate::profile_sync::TauriProfileSyncHandler).await
}

/// Returns all blocked profiles.
#[tauri::command]
pub async fn get_blocked_users() -> Vec<crate::db::SlimProfile> {
    vector_core::profile::sync::get_blocked_users().await
}

/// Set a nickname for a profile.
#[tauri::command]
pub async fn set_nickname(npub: String, nickname: String) -> bool {
    vector_core::profile::sync::set_nickname(npub, nickname, &crate::profile_sync::TauriProfileSyncHandler).await
}
