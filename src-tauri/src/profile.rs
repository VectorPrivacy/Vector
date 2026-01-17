use nostr_sdk::prelude::*;
use tauri::Emitter;
use tauri_plugin_fs::FsExt;

use crate::{NOSTR_CLIENT, STATE, TAURI_APP};
use crate::db;
use crate::message::AttachmentFile;
use crate::image_cache::{self, CacheResult};

#[cfg(target_os = "android")]
use crate::android::filesystem;

#[derive(serde::Serialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Profile {
    pub id: String,
    pub name: String,
    pub display_name: String,
    pub nickname: String,
    pub lud06: String,
    pub lud16: String,
    pub banner: String,
    pub avatar: String,
    pub about: String,
    pub website: String,
    pub nip05: String,
    pub status: Status,
    pub last_updated: u64,
    pub mine: bool,
    pub muted: bool,
    pub bot: bool,
    /// Local cached path for avatar image (for offline support)
    pub avatar_cached: String,
    /// Local cached path for banner image (for offline support)
    pub banner_cached: String,
}

impl Default for Profile {
    fn default() -> Self {
        Self::new()
    }
}

impl Profile {
    pub fn new() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            display_name: String::new(),
            nickname: String::new(),
            lud06: String::new(),
            lud16: String::new(),
            banner: String::new(),
            avatar: String::new(),
            about: String::new(),
            website: String::new(),
            nip05: String::new(),
            status: Status::new(),
            last_updated: 0,
            mine: false,
            muted: false,
            bot: false,
            avatar_cached: String::new(),
            banner_cached: String::new(),
        }
    }

    /// Merge Nostr Metadata with this Vector Profile
    /// 
    /// Returns `true` if any fields were updated, `false`` otherwise
    pub fn from_metadata(&mut self, meta: Metadata) -> bool {
        let mut changed = false;
        
        // Name
        if let Some(name) = meta.name {
            if self.name != name {
                self.name = name;
                changed = true;
            }
        }

        // Display Name
        if let Some(name) = meta.display_name {
            if self.display_name != name {
                self.display_name = name;
                changed = true;
            }
        }

        // lud06 (LNURL)
        if let Some(lud06) = meta.lud06 {
            if self.lud06 != lud06 {
                self.lud06 = lud06;
                changed = true;
            }
        }

        // lud16 (Lightning Address)
        if let Some(lud16) = meta.lud16 {
            if self.lud16 != lud16 {
                self.lud16 = lud16;
                changed = true;
            }
        }

        // Banner
        if let Some(banner) = meta.banner {
            if self.banner != banner {
                self.banner = banner;
                self.banner_cached = String::new(); // Clear stale cache when URL changes
                changed = true;
            }
        }

        // Picture (Vector Avatar)
        if let Some(picture) = meta.picture {
            if self.avatar != picture {
                self.avatar = picture;
                self.avatar_cached = String::new(); // Clear stale cache when URL changes
                changed = true;
            }
        }

        // About (Vector Bio)
        if let Some(about) = meta.about {
            if self.about != about {
                self.about = about;
                changed = true;
            }
        }

        // Website
        if let Some(website) = meta.website {
            if self.website != website {
                self.website = website;
                changed = true;
            }
        }

        // NIP-05
        if let Some(nip05) = meta.nip05 {
            if self.nip05 != nip05 {
                self.nip05 = nip05;
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
            
            if self.bot != bot_value {
                self.bot = bot_value;
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
        if let Some(profile) = state.get_profile_mut(npub) {
            let mut updated = false;

            if !avatar_cached.is_empty() && profile.avatar_cached != avatar_cached {
                profile.avatar_cached = avatar_cached;
                updated = true;
            }

            if !banner_cached.is_empty() && profile.banner_cached != banner_cached {
                profile.banner_cached = banner_cached;
                updated = true;
            }

            if updated {
                // Emit update to frontend with cached paths
                handle.emit("profile_update", &profile).ok();

                // Save to database
                let profile_clone = profile.clone();
                drop(state); // Release lock before async DB operation
                db::set_profile(handle.clone(), profile_clone).await.ok();
            }
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

    // Get all profiles that need caching
    let profiles_to_cache: Vec<(String, String, String)> = {
        let state = STATE.lock().await;
        state.profiles.iter()
            .filter(|p| {
                // Cache if has avatar URL but no cached path
                (!p.avatar.is_empty() && p.avatar_cached.is_empty()) ||
                // Or has banner URL but no cached path
                (!p.banner.is_empty() && p.banner_cached.is_empty())
            })
            .map(|p| (p.id.clone(), p.avatar.clone(), p.banner.clone()))
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
                    // Update profile
                    let mut state = STATE.lock().await;
                    if let Some(profile) = state.get_profile_mut(&npub) {
                        if profile.avatar_cached.is_empty() {
                            profile.avatar_cached = path;
                            handle.emit("profile_update", &profile).ok();
                            let profile_clone = profile.clone();
                            drop(state);
                            db::set_profile(handle.clone(), profile_clone).await.ok();
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
                    if let Some(profile) = state.get_profile_mut(&npub) {
                        if profile.banner_cached.is_empty() {
                            profile.banner_cached = path;
                            handle.emit("profile_update", &profile).ok();
                            let profile_clone = profile.clone();
                            drop(state);
                            db::set_profile(handle.clone(), profile_clone).await.ok();
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
    let signer = match client.signer().await {
        Ok(s) => s,
        Err(_) => return false,
    };
    let my_public_key = match signer.get_public_key().await {
        Ok(pk) => pk,
        Err(_) => return false,
    };

    // Fetch immutable copies of our updateable profile parts (or, quickly generate a new one to pass to the fetching logic)
    let old_status: Status;
    {
        let mut state = STATE.lock().await;
        old_status = match state.get_profile(&npub) {
            Some(p) => p.status.clone(),
            None => {
                // Create a new profile
                let mut new_profile = Profile::new();
                new_profile.id = npub.clone();
                state.profiles.push(new_profile);
                Status::new()
            }
        }
        .clone();
    }

    // Attempt to fetch their status, if one exists
    let status_filter = Filter::new()
        .author(profile_pubkey)
        .kind(Kind::from_u16(30315))
        .limit(1);

    let status = match client
        .fetch_events(status_filter, std::time::Duration::from_secs(15))
        .await
    {
        Ok(res) => {
            // Make sure they have a status available
            if !res.is_empty() {
                let status_event = res.first().unwrap();
                // Simple status recognition: last, general-only, no URLs, Metadata or Expiry considered
                // TODO: comply with expiries, accept more "d" types, allow URLs
                Status {
                    title: status_event.content.clone(),
                    purpose: status_event
                        .tags
                        .first()
                        .unwrap()
                        .content()
                        .unwrap()
                        .to_string(),
                    url: String::from(""),
                }
            } else {
                // Relays didn't find anything? We'll ignore this and use our previous status
                old_status
            }
        }
        Err(_) => old_status,
    };

    // Attempt to fetch their Metadata profile
    let fetch_result = client
        .fetch_metadata(profile_pubkey, std::time::Duration::from_secs(15))
        .await;
    
    match fetch_result {
        Ok(meta) => {
            if meta.is_some() {
                // If it's ours, mark it as such
                let mut state = STATE.lock().await;
                let profile_mutable = state.get_profile_mut(&npub).unwrap();
                profile_mutable.mine = my_public_key == profile_pubkey;

                // Update the Status, and track changes
                let status_changed = profile_mutable.status != status;
                profile_mutable.status = status;

                // Update the Metadata, and track changes
                let metadata_changed = profile_mutable.from_metadata(meta.unwrap());

                // Apply the current update time
                profile_mutable.last_updated = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs();

                // If there's any change between our Old and New profile, emit an update
                if status_changed || metadata_changed {
                    let handle = TAURI_APP.get().unwrap();
                    handle.emit("profile_update", &profile_mutable).unwrap();

                    // Cache this profile in our DB, too
                    db::set_profile(handle.clone(), profile_mutable.clone()).await.unwrap();

                    // Cache avatar/banner images in the background for offline access
                    let npub_clone = npub.clone();
                    let avatar_url = profile_mutable.avatar.clone();
                    let banner_url = profile_mutable.banner.clone();
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
                    profile.last_updated = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_secs();
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
    let signer = client.signer().await.unwrap();
    let my_public_key = signer.get_public_key().await.unwrap();

    // Get our profile
    let mut meta: Metadata;
    let mut state = STATE.lock().await;
    let profile = state
        .get_profile(&my_public_key.to_bech32().unwrap())
        .unwrap();

    // We'll apply the changes to the previous profile and carry-on the rest
    meta = Metadata::new().name(if name.is_empty() {
        &profile.name
    } else {
        &name
    });

    // Optional avatar
    let avatar_url_str = if avatar.is_empty() {
        profile.avatar.as_str()
    } else {
        avatar.as_str()
    };
    if !avatar_url_str.is_empty() {
        if let Ok(url) = Url::parse(avatar_url_str) {
            meta = meta.picture(url);
        }
    }

    // Optional banner
    let banner_url_str = if banner.is_empty() {
        profile.banner.as_str()
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
        meta = meta.display_name(&profile.display_name);
    }

    // Add about
    meta = meta.about(if about.is_empty() {
        &profile.about
    } else {
        &about
    });

    // Add website
    if !profile.website.is_empty() {
        if let Ok(url) = Url::parse(&profile.website) {
            meta = meta.website(url);
        }
    }

    // Add nip05
    if !profile.nip05.is_empty() {
        meta = meta.nip05(&profile.nip05);
    }

    // Add lud06
    if !profile.lud06.is_empty() {
        meta = meta.lud06(&profile.lud06);
    }

    // Add lud16
    if !profile.lud16.is_empty() {
        meta = meta.lud16(&profile.lud16);
    }

    // Serialize the metadata to JSON for the event content
    let metadata_json = serde_json::to_string(&meta).unwrap();

    // Create the metadata event with the Vector tag
    let metadata_event = EventBuilder::new(Kind::Metadata, metadata_json)
        .tag(Tag::custom(TagKind::Custom(String::from("client").into()), vec!["vector"]));

    // Broadcast the profile update
    match client.send_event_builder(metadata_event).await {
        Ok(_) => {
            // Apply our Metadata to our Profile
            let npub = my_public_key.to_bech32().unwrap();
            let profile_mutable = state
                .get_profile_mut(&npub)
                .unwrap();
            profile_mutable.from_metadata(meta);

            // Update the frontend
            let handle = TAURI_APP.get().unwrap();
            handle.emit("profile_update", &profile_mutable).unwrap();

            // Save to database
            let profile_clone = profile_mutable.clone();
            let avatar_url = profile_mutable.avatar.clone();
            let banner_url = profile_mutable.banner.clone();
            drop(state); // Release lock before async operations

            db::set_profile(handle.clone(), profile_clone).await.ok();

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
    let signer = client.signer().await.unwrap();
    let my_public_key = signer.get_public_key().await.unwrap();

    // Build and broadcast the status
    let status_builder = EventBuilder::new(Kind::from_u16(30315), status.as_str())
        .tag(Tag::custom(TagKind::d(), vec!["general"]));
    match client.send_event_builder(status_builder).await {
        Ok(_) => {
            // Add the status to our profile
            let mut state = STATE.lock().await;
            let profile = state
                .get_profile_mut(&my_public_key.to_bech32().unwrap())
                .unwrap();
            profile.status.purpose = String::from("general");
            profile.status.title = status;

            // Update the frontend
            let handle = TAURI_APP.get().unwrap();
            handle.emit("profile_update", &profile).unwrap();
            true
        }
        Err(_) => false,
    }
}

#[tauri::command]
pub async fn upload_avatar(filepath: String) -> Result<String, String> {
    let handle = TAURI_APP.get().unwrap();

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
                bytes,
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

    // Upload the file to the server using Blossom with automatic failover
    let client = NOSTR_CLIENT.get().expect("Nostr client not initialized");
    let signer = client.signer().await.unwrap();
    let servers = crate::get_blossom_servers();

    // Upload using Blossom with failover
    crate::blossom::upload_blob_with_failover(
        signer.clone(),
        servers,
        attachment_file.bytes,
        Some(mime_type.as_str())
    )
    .await
}


/// Toggles the muted status of a profile
#[tauri::command]
pub async fn toggle_muted(npub: String) -> bool {
    let handle = TAURI_APP.get().unwrap();

    let muted = match STATE.lock().await.get_profile_mut(&npub) {
        Some(profile) => {
            profile.muted = !profile.muted;

            // Update the frontend
            handle.emit("profile_muted", serde_json::json!({
                "profile_id": &profile.id,
                "value": &profile.muted
            })).unwrap();

            // Save to DB
            db::set_profile(handle.clone(), profile.clone()).await.unwrap();

            profile.muted
        }
        None => false
    };

    // Refresh unread badge count to reflect mute changes immediately
    let _ = crate::update_unread_counter(handle.clone()).await;
    muted
}

/// Sets a nickname for a profile
#[tauri::command]
pub async fn set_nickname(npub: String, nickname: String) -> bool {
    let handle = TAURI_APP.get().unwrap();
    let mut state = STATE.lock().await;

    match state.get_profile_mut(&npub) {
        Some(profile) => {
            profile.nickname = nickname;

            // Update the frontend
            handle.emit("profile_nick_changed", serde_json::json!({
                "profile_id": &profile.id,
                "value": &profile.nickname
            })).unwrap();

            // Save to DB
            db::set_profile(handle.clone(), profile.clone()).await.unwrap();

            true
        }
        None => false
    }
}
