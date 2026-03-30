use nostr_sdk::prelude::*;
use tauri::Emitter;

#[cfg(not(target_os = "android"))]
use std::sync::Arc;
#[cfg(not(target_os = "android"))]
use tauri_plugin_fs::FsExt;

use crate::{NOSTR_CLIENT, STATE, TAURI_APP};
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

#[tauri::command]
pub async fn update_profile(name: String, avatar: String, banner: String, about: String) -> bool {
    let client = NOSTR_CLIENT.get().expect("Nostr client not initialized");

    // Grab our pubkey
    let my_public_key = *crate::MY_PUBLIC_KEY.get().expect("Public key not initialized");

    // Build metadata from current profile, then drop the lock before network I/O
    let meta = {
        let state = STATE.lock().await;
        let profile = state
            .get_profile(&my_public_key.to_bech32().unwrap())
            .unwrap();

        // We'll apply the changes to the previous profile and carry-on the rest
        let mut meta = Metadata::new().name(if name.is_empty() {
            &*profile.name
        } else {
            name.as_str()
        });

        // Optional avatar
        let avatar_url_str: &str = if avatar.is_empty() {
            &profile.avatar
        } else {
            avatar.as_str()
        };
        if !avatar_url_str.is_empty() {
            if let Ok(url) = Url::parse(avatar_url_str) {
                meta = meta.picture(url);
            }
        }

        // Optional banner
        let banner_url_str: &str = if banner.is_empty() {
            &profile.banner
        } else {
            banner.as_str()
        };
        if !banner_url_str.is_empty() {
            if let Ok(url) = Url::parse(banner_url_str) {
                meta = meta.banner(url);
            }
        }

        // Add display_name
        if !profile.display_name.is_empty() {
            meta = meta.display_name(&*profile.display_name);
        }

        // Add about
        meta = meta.about(if about.is_empty() {
            &*profile.about
        } else {
            about.as_str()
        });

        // Add website
        if !profile.website.is_empty() {
            if let Ok(url) = Url::parse(&*profile.website) {
                meta = meta.website(url);
            }
        }

        // Add nip05
        if !profile.nip05.is_empty() {
            meta = meta.nip05(&*profile.nip05);
        }

        // Add lud06
        if !profile.lud06.is_empty() {
            meta = meta.lud06(&*profile.lud06);
        }

        // Add lud16
        if !profile.lud16.is_empty() {
            meta = meta.lud16(&*profile.lud16);
        }

        meta
    }; // Drop STATE lock before network I/O

    // Serialize the metadata to JSON for the event content
    let metadata_json = serde_json::to_string(&meta).unwrap();

    // Create the metadata event
    let metadata_event = EventBuilder::new(Kind::Metadata, metadata_json)
        .tag(Tag::custom(TagKind::Custom(String::from("client").into()), vec!["vector"]));

    // Sign and broadcast the profile update (no lock held during network I/O)
    // Uses first-ACK send so UI updates as soon as the fastest relay responds
    let Ok(event) = client.sign_event_builder(metadata_event).await else {
        return false;
    };
    match crate::inbox_relays::send_event_pool_first_ok(client, &event).await {
        Ok(_) => {
            // Re-acquire lock to apply metadata to our profile
            let npub = my_public_key.to_bech32().unwrap();
            let (slim, avatar_url, banner_url) = {
                let mut state = STATE.lock().await;
                let id = state.interner.lookup(&npub).unwrap();
                let (avatar_url, banner_url) = {
                    let profile_mutable = state.get_profile_mut_by_id(id).unwrap();
                    profile_mutable.from_metadata(meta);
                    (profile_mutable.avatar.to_string(), profile_mutable.banner.to_string())
                };

                let slim = state.serialize_profile(id).unwrap();
                let handle = TAURI_APP.get().unwrap();
                handle.emit("profile_update", &slim).unwrap();

                (slim, avatar_url, banner_url)
            }; // Drop STATE lock before async operations

            db::set_profile(slim).await.ok();

            // Cache avatar/banner images in the background for offline access
            let npub_clone = npub.clone();
            tokio::spawn(async move {
                cache_profile_images(&npub_clone, &avatar_url, &banner_url).await;
            });

            true
        }
        Err(_) => false
    }
}

#[tauri::command]
pub async fn update_status(status: String) -> bool {
    let client = NOSTR_CLIENT.get().expect("Nostr client not initialized");

    // Grab our pubkey
    let my_public_key = *crate::MY_PUBLIC_KEY.get().expect("Public key not initialized");

    // Build and broadcast the status
    let status_builder = EventBuilder::new(Kind::from_u16(30315), status.as_str())
        .tag(Tag::custom(TagKind::d(), vec!["general"]));
    let Ok(event) = client.sign_event_builder(status_builder).await else {
        return false;
    };
    match crate::inbox_relays::send_event_pool_first_ok(client, &event).await {
        Ok(_) => {
            // Add the status to our profile
            let mut state = STATE.lock().await;
            let npub = my_public_key.to_bech32().unwrap();
            let id = state.interner.lookup(&npub).unwrap();
            {
                let profile = state.get_profile_mut_by_id(id).unwrap();
                profile.status_purpose = "general".into();
                profile.status_title = status.into_boxed_str();
            }

            // Update the frontend
            let slim = state.serialize_profile(id).unwrap();
            let handle = TAURI_APP.get().unwrap();
            handle.emit("profile_update", &slim).unwrap();
            true
        }
        Err(_) => false,
    }
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


/// Blocks a user by npub. DM events from blocked users are dropped after decryption.
/// Group messages are stored but filtered in the UI.
#[tauri::command]
pub async fn block_user(npub: String) -> bool {
    // Prevent blocking yourself (would break Notes/Bookmarks and self-DM processing)
    if let Some(&my_pk) = crate::MY_PUBLIC_KEY.get() {
        if my_pk.to_bech32().ok().as_deref() == Some(npub.as_str()) {
            return false;
        }
    }

    let handle = TAURI_APP.get().unwrap();
    let mut state = STATE.lock().await;

    // Create profile if it doesn't exist (can block someone with no prior contact)
    if state.interner.lookup(&npub).is_none() {
        let new_profile = Profile::new();
        state.insert_or_replace_profile(&npub, new_profile);
    }

    if let Some(id) = state.interner.lookup(&npub) {
        {
            let profile = match state.get_profile_mut_by_id(id) {
                Some(p) => p,
                None => return false,
            };
            profile.flags.set_blocked(true);
        }
        let slim = state.serialize_profile(id).unwrap();
        handle.emit("profile_update", &slim).ok();
        drop(state);
        db::set_profile(slim).await.ok();
        true
    } else {
        false
    }
}

/// Unblocks a user by npub.
#[tauri::command]
pub async fn unblock_user(npub: String) -> bool {
    let handle = TAURI_APP.get().unwrap();
    let mut state = STATE.lock().await;

    if let Some(id) = state.interner.lookup(&npub) {
        {
            let profile = match state.get_profile_mut_by_id(id) {
                Some(p) => p,
                None => return false,
            };
            profile.flags.set_blocked(false);
        }
        let slim = state.serialize_profile(id).unwrap();
        handle.emit("profile_update", &slim).ok();
        drop(state);
        db::set_profile(slim).await.ok();
        true
    } else {
        false
    }
}

/// Returns all blocked profiles for the Settings blocked users list.
#[tauri::command]
pub async fn get_blocked_users() -> Vec<crate::db::SlimProfile> {
    let state = STATE.lock().await;
    state.profiles.iter()
        .filter(|p| p.flags.is_blocked())
        .filter_map(|p| state.serialize_profile(p.id))
        .collect()
}

/// Sets a nickname for a profile
#[tauri::command]
pub async fn set_nickname(npub: String, nickname: String) -> bool {
    let handle = TAURI_APP.get().unwrap();
    let mut state = STATE.lock().await;

    if let Some(id) = state.interner.lookup(&npub) {
        {
            let profile = match state.get_profile_mut_by_id(id) {
                Some(p) => p,
                None => return false,
            };
            profile.nickname = nickname.into_boxed_str();
            handle.emit("profile_nick_changed", serde_json::json!({
                "profile_id": &npub,
                "value": &*profile.nickname
            })).unwrap();
        }
        let slim = state.serialize_profile(id).unwrap();
        drop(state);
        db::set_profile(slim).await.unwrap();
        true
    } else {
        false
    }
}
