use nostr_sdk::prelude::*;
use tauri::Emitter;

#[cfg(not(target_os = "android"))]
use std::sync::Arc;
#[cfg(not(target_os = "android"))]
use tauri_plugin_fs::FsExt;

use crate::{NOSTR_CLIENT, STATE, TAURI_APP};
use crate::db;
use crate::image_cache::{self, CacheResult};
use crate::message::compact::secs_to_compact;
#[cfg(not(target_os = "android"))]
use crate::message::AttachmentFile;

#[cfg(target_os = "android")]
use crate::android::filesystem;

// ============================================================================
// ProfileFlags — 3 bools packed into 1 byte
// ============================================================================

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProfileFlags(u8);

impl ProfileFlags {
    const MINE:  u8 = 0b001;
    const MUTED: u8 = 0b010;
    const BOT:   u8 = 0b100;

    #[inline] pub fn is_mine(self) -> bool  { self.0 & Self::MINE != 0 }
    #[inline] pub fn is_muted(self) -> bool { self.0 & Self::MUTED != 0 }
    #[inline] pub fn is_bot(self) -> bool   { self.0 & Self::BOT != 0 }

    #[inline] pub fn set_mine(&mut self, v: bool)  { if v { self.0 |= Self::MINE } else { self.0 &= !Self::MINE } }
    #[inline] pub fn set_muted(&mut self, v: bool) { if v { self.0 |= Self::MUTED } else { self.0 &= !Self::MUTED } }
    #[inline] pub fn set_bot(&mut self, v: bool)   { if v { self.0 |= Self::BOT } else { self.0 &= !Self::BOT } }
}

// ============================================================================
// Profile — compact internal representation
// ============================================================================

/// Internal profile representation. The `id` is a u16 interner handle —
/// the canonical npub string lives in `NpubInterner` (single source of truth).
/// Use `SlimProfile` for serialization boundaries (frontend, DB).
///
/// All string fields use `Box<str>` (16B) instead of `String` (24B) —
/// profile strings are write-once from metadata, never grown in-place.
#[derive(Clone, Debug, PartialEq)]
pub struct Profile {
    /// Interner handle — resolves to npub via `NpubInterner::resolve(id)`
    pub id: u16,
    pub name: Box<str>,
    pub display_name: Box<str>,
    pub nickname: Box<str>,
    pub lud06: Box<str>,
    pub lud16: Box<str>,
    pub banner: Box<str>,
    pub avatar: Box<str>,
    pub about: Box<str>,
    pub website: Box<str>,
    pub nip05: Box<str>,
    /// Status fields flattened (was `Status` struct — saves alignment padding)
    pub status_title: Box<str>,
    pub status_purpose: Box<str>,
    pub status_url: Box<str>,
    /// Compact timestamp: seconds since 2020 epoch (valid until 2156)
    pub last_updated: u32,
    /// Packed boolean flags: mine | muted | bot
    pub flags: ProfileFlags,
    /// Local cached path for avatar image (for offline support)
    pub avatar_cached: Box<str>,
    /// Local cached path for banner image (for offline support)
    pub banner_cached: Box<str>,
}

impl Default for Profile {
    fn default() -> Self {
        Self::new()
    }
}

impl Profile {
    pub fn new() -> Self {
        Self {
            id: crate::message::compact::NO_NPUB,
            name: Box::<str>::default(),
            display_name: Box::<str>::default(),
            nickname: Box::<str>::default(),
            lud06: Box::<str>::default(),
            lud16: Box::<str>::default(),
            banner: Box::<str>::default(),
            avatar: Box::<str>::default(),
            about: Box::<str>::default(),
            website: Box::<str>::default(),
            nip05: Box::<str>::default(),
            status_title: Box::<str>::default(),
            status_purpose: Box::<str>::default(),
            status_url: Box::<str>::default(),
            last_updated: 0,
            flags: ProfileFlags::default(),
            avatar_cached: Box::<str>::default(),
            banner_cached: Box::<str>::default(),
        }
    }

    /// Merge Nostr Metadata with this Vector Profile
    ///
    /// Returns `true` if any fields were updated, `false` otherwise
    pub fn from_metadata(&mut self, meta: Metadata) -> bool {
        let mut changed = false;

        // Name
        if let Some(name) = meta.name {
            if *self.name != *name {
                self.name = name.into_boxed_str();
                changed = true;
            }
        }

        // Display Name
        if let Some(name) = meta.display_name {
            if *self.display_name != *name {
                self.display_name = name.into_boxed_str();
                changed = true;
            }
        }

        // lud06 (LNURL)
        if let Some(lud06) = meta.lud06 {
            if *self.lud06 != *lud06 {
                self.lud06 = lud06.into_boxed_str();
                changed = true;
            }
        }

        // lud16 (Lightning Address)
        if let Some(lud16) = meta.lud16 {
            if *self.lud16 != *lud16 {
                self.lud16 = lud16.into_boxed_str();
                changed = true;
            }
        }

        // Banner
        if let Some(banner) = meta.banner {
            if *self.banner != *banner {
                self.banner = banner.into_boxed_str();
                self.banner_cached = Box::<str>::default(); // Clear stale cache when URL changes
                changed = true;
            }
        }

        // Picture (Vector Avatar)
        if let Some(picture) = meta.picture {
            if *self.avatar != *picture {
                self.avatar = picture.into_boxed_str();
                self.avatar_cached = Box::<str>::default(); // Clear stale cache when URL changes
                changed = true;
            }
        }

        // About (Vector Bio)
        if let Some(about) = meta.about {
            if *self.about != *about {
                self.about = about.into_boxed_str();
                changed = true;
            }
        }

        // Website
        if let Some(website) = meta.website {
            if *self.website != *website {
                self.website = website.into_boxed_str();
                changed = true;
            }
        }

        // NIP-05
        if let Some(nip05) = meta.nip05 {
            if *self.nip05 != *nip05 {
                self.nip05 = nip05.into_boxed_str();
                changed = true;
            }
        }

        // Bot (custom metadata field)
        if let Some(custom) = meta.custom.get("bot") {
            // Parse the bot value - it could be a boolean or a string "true"/"false"
            let bot_value = match custom.as_bool() {
                Some(b) => b,
                None => {
                    // Try parsing as string
                    custom.as_str()
                        .map(|s| s.to_lowercase() == "true")
                        .unwrap_or(false)
                }
            };

            if self.flags.is_bot() != bot_value {
                self.flags.set_bot(bot_value);
                changed = true;
            }
        }

        changed
    }
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq)]
pub struct Status {
    pub title: String,
    pub purpose: String,
    pub url: String,
}

impl Status {
    pub fn new() -> Self {
        Self {
            title: String::new(),
            purpose: String::new(),
            url: String::new(),
        }
    }
}

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
                log::warn!("[Profile] Failed to cache avatar for {}: {}", npub, e);
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
                log::warn!("[Profile] Failed to cache banner for {}: {}", npub, e);
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

    log::info!("[Profile] Caching images for {} profiles", profiles_to_cache.len());

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

#[tauri::command]
pub async fn load_profile(npub: String) -> bool {
    let client = match NOSTR_CLIENT.get() {
        Some(c) => c,
        None => return false,
    };

    // Convert the Bech32 String in to a PublicKey
    let profile_pubkey = match PublicKey::from_bech32(npub.as_str()) {
        Ok(pk) => pk,
        Err(_) => return false,
    };

    // Grab our pubkey to check for profiles belonging to us
    let my_public_key = match crate::MY_PUBLIC_KEY.get() {
        Some(&pk) => pk,
        None => return false,
    };

    // Fetch immutable copies of our updateable profile parts (or, quickly generate a new one to pass to the fetching logic)
    let (old_status_title, old_status_purpose, old_status_url): (String, String, String);
    {
        let mut state = STATE.lock().await;
        match state.get_profile(&npub) {
            Some(p) => {
                old_status_title = p.status_title.to_string();
                old_status_purpose = p.status_purpose.to_string();
                old_status_url = p.status_url.to_string();
            }
            None => {
                // Create a new profile
                let new_profile = Profile::new();
                state.insert_or_replace_profile(&npub, new_profile);
                old_status_title = String::new();
                old_status_purpose = String::new();
                old_status_url = String::new();
            }
        }
    }

    // Attempt to fetch their status, if one exists
    let status_filter = Filter::new()
        .author(profile_pubkey)
        .kind(Kind::from_u16(30315))
        .limit(1);

    let (status_title, status_purpose, status_url) = match client
        .fetch_events(status_filter, std::time::Duration::from_secs(15))
        .await
    {
        Ok(res) => {
            // Make sure they have a status available
            if !res.is_empty() {
                let status_event = res.first().unwrap();
                // Simple status recognition: last, general-only, no URLs, Metadata or Expiry considered
                // TODO: comply with expiries, accept more "d" types, allow URLs
                (
                    status_event.content.clone(),
                    status_event
                        .tags
                        .first()
                        .unwrap()
                        .content()
                        .unwrap()
                        .to_string(),
                    String::new(),
                )
            } else {
                // Relays didn't find anything? We'll ignore this and use our previous status
                (old_status_title, old_status_purpose, old_status_url)
            }
        }
        Err(_) => (old_status_title, old_status_purpose, old_status_url),
    };

    // Attempt to fetch their Metadata profile
    let fetch_result = client
        .fetch_metadata(profile_pubkey, std::time::Duration::from_secs(15))
        .await;
    
    match fetch_result {
        Ok(meta) => {
            if meta.is_some() {
                // If it's ours, mark it as such
                let save_data = {
                    let mut state = STATE.lock().await;
                    let id = state.interner.lookup(&npub).unwrap();
                    let (changed, avatar_url, banner_url) = {
                        let profile_mutable = state.get_profile_mut_by_id(id).unwrap();
                        profile_mutable.flags.set_mine(my_public_key == profile_pubkey);

                        // Update the Status, and track changes
                        let status_changed = *profile_mutable.status_title != *status_title
                            || *profile_mutable.status_purpose != *status_purpose
                            || *profile_mutable.status_url != *status_url;
                        profile_mutable.status_title = status_title.into_boxed_str();
                        profile_mutable.status_purpose = status_purpose.into_boxed_str();
                        profile_mutable.status_url = status_url.into_boxed_str();

                        // Update the Metadata, and track changes
                        let metadata_changed = profile_mutable.from_metadata(meta.unwrap());

                        // Apply the current update time
                        profile_mutable.last_updated = secs_to_compact(
                            std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap()
                                .as_secs()
                        );

                        (status_changed || metadata_changed,
                         profile_mutable.avatar.to_string(),
                         profile_mutable.banner.to_string())
                    };

                    // Only serialize when something actually changed (common case: no change)
                    if changed {
                        let slim = state.serialize_profile(id).unwrap();
                        let handle = TAURI_APP.get().unwrap();
                        handle.emit("profile_update", &slim).unwrap();
                        Some((slim, avatar_url, banner_url))
                    } else {
                        None
                    }
                }; // Drop STATE lock before async operations

                if let Some((slim, avatar_url, banner_url)) = save_data {
                    let handle = TAURI_APP.get().unwrap();
                    db::set_profile(slim).await.unwrap();

                    // Cache avatar/banner images in the background for offline access
                    let npub_clone = npub.clone();
                    tokio::spawn(async move {
                        cache_profile_images(&npub_clone, &avatar_url, &banner_url).await;
                    });
                }
                return true;
            } else {
                // Profile doesn't exist on relays - check if we have it in STATE already
                let mut state = STATE.lock().await;
                if let Some(profile) = state.get_profile_mut(&npub) {
                    // We have the profile in STATE, just update the timestamp so we don't keep retrying
                    profile.last_updated = secs_to_compact(
                        std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap()
                            .as_secs()
                    );
                    return true;
                } else {
                    // Profile truly doesn't exist anywhere
                    return true;
                }
            }
        }
        Err(_) => {
            // Network/relay error - this is a genuine failure
            return false;
        }
    }
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

            let handle = TAURI_APP.get().unwrap();
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
    let signer = crate::MY_KEYS.get().expect("Keys not initialized").clone();
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


/// Toggles the muted status of a profile
#[tauri::command]
pub async fn toggle_muted(npub: String) -> bool {
    let handle = TAURI_APP.get().unwrap();

    let (muted, slim) = {
        let mut state = STATE.lock().await;
        if let Some(id) = state.interner.lookup(&npub) {
            let muted_val = {
                let profile = match state.get_profile_mut_by_id(id) {
                    Some(p) => p,
                    None => return false,
                };
                profile.flags.set_muted(!profile.flags.is_muted());
                handle.emit("profile_muted", serde_json::json!({
                    "profile_id": &npub,
                    "value": profile.flags.is_muted()
                })).unwrap();
                profile.flags.is_muted()
            };
            (muted_val, state.serialize_profile(id))
        } else {
            (false, None)
        }
    }; // Drop STATE lock before async DB operation

    if let Some(slim) = slim {
        db::set_profile(slim).await.unwrap();
    }

    // Refresh unread badge count to reflect mute changes immediately
    let _ = crate::commands::messaging::update_unread_counter(handle.clone()).await;
    muted
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
