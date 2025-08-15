use nostr_sdk::prelude::*;
use tauri::Emitter;
use tauri_plugin_fs::FsExt;

use crate::{NOSTR_CLIENT, STATE, TAURI_APP, PUBLIC_NIP96_CONFIG};
use crate::db;
use crate::message::AttachmentFile;

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
    /// Deprecated: Moved to Chat.last_read. This field is only kept for migration purposes.
    /// Follow-up plan to drop this field:
    /// 1. In the next release, stop using this field in the migration process
    /// 2. In a subsequent release, remove this field from the struct and all related code
    pub last_read: String,
    pub status: Status,
    pub last_updated: u64,
    pub typing_until: u64,
    pub mine: bool,
    pub muted: bool,
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
            last_read: String::new(),
            status: Status::new(),
            last_updated: 0,
            typing_until: 0,
            mine: false,
            muted: false,
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
                changed = true;
            }
        }
        
        // Picture (Vector Avatar)
        if let Some(picture) = meta.picture {
            if self.avatar != picture {
                self.avatar = picture;
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

#[tauri::command]
pub async fn load_profile(npub: String) -> bool {
    let client = NOSTR_CLIENT.get().expect("Nostr client not initialized");

    // Convert the Bech32 String in to a PublicKey
    let profile_pubkey = PublicKey::from_bech32(npub.as_str()).unwrap();

    // Grab our pubkey to check for profiles belonging to us
    let signer = client.signer().await.unwrap();
    let my_public_key = signer.get_public_key().await.unwrap();

    // Fetch immutable copies of our updateable profile parts (or, quickly generate a new one to pass to the fetching logic)
    // Mutex Scope: we want to hold this lock as short as possible, given this function is "spammed" for very fast profile cache hit checks
    let old_status: Status;
    {
        let mut state = STATE.lock().await;
        old_status = match state.get_profile(&npub) {
            Some(p) => {
                // If the profile has been refreshed in the last 30s, return it's cached version
                if p.last_updated + 30 > std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs()
                    {
                        return true;
                    }
                p.status.clone()
            },
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
    match client
        .fetch_metadata(profile_pubkey, std::time::Duration::from_secs(15))
        .await
    {
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
                }
                return true;
            } else {
                return false;
            }
        }
        Err(_) => {
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
    if !avatar.is_empty() || !profile.avatar.is_empty() {
        meta = meta.picture(
            Url::parse(if avatar.is_empty() {
                profile.avatar.as_str()
            } else {
                avatar.as_str()
            })
            .unwrap(),
        );
    }

    // Optional banner
    if !banner.is_empty() || !profile.banner.is_empty() {
        meta = meta.banner(
            Url::parse(if banner.is_empty() {
                profile.banner.as_str()
            } else {
                banner.as_str()
            })
            .unwrap(),
        );
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
        meta = meta.website(Url::parse(&profile.website).unwrap());
    }

    // Add banner
    if !profile.banner.is_empty() {
        meta = meta.banner(Url::parse(&profile.banner).unwrap());
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
            let profile_mutable = state
                .get_profile_mut(&my_public_key.to_bech32().unwrap())
                .unwrap();
            profile_mutable.from_metadata(meta);

            // Update the frontend
            let handle = TAURI_APP.get().unwrap();
            handle.emit("profile_update", &profile_mutable).unwrap();
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

    // Upload the file to the server
    let client = NOSTR_CLIENT.get().expect("Nostr client not initialized");
    let signer = client.signer().await.unwrap();
    let conf = PUBLIC_NIP96_CONFIG.wait();

    // Create upload request
    let upload_request = nostr_sdk::nips::nip96::UploadRequest::new(
        &signer,
        &conf,
        &attachment_file.bytes
    )
    .await
    .map_err(|e| e.to_string())?;

    // Get the upload URL and authorization header
    let upload_url = upload_request.url();
    let auth_header = upload_request.authorization();

    // Create the HTTP client
    let http_client = reqwest::Client::new();

    // Create the multipart form
    let form = reqwest::multipart::Form::new()
        .part("file", reqwest::multipart::Part::bytes(attachment_file.bytes)
            .file_name(format!("avatar.{}", attachment_file.extension))
            .mime_str(mime_type.as_str())
            .map_err(|_| "Failed to set MIME type")?);

    // Make the upload request
    let response = http_client
        .post(upload_url.clone())
        .header("Authorization", auth_header)
        .multipart(form)
        .send()
        .await
        .map_err(|e| format!("Failed to send upload request: {}", e))?;

    // Check if the request was successful
    if !response.status().is_success() {
        return Err(format!("Upload failed with status: {}", response.status()));
    }

    // Parse the response
    let upload_response: nostr_sdk::nips::nip96::UploadResponse = response
        .json()
        .await
        .map_err(|e| format!("Failed to parse upload response: {}", e))?;

    // Extract the URL from the response
    match upload_response.download_url() {
        Ok(url) => Ok(url.to_string()),
        Err(e) => Err(format!("Failed to extract download URL: {}", e))
    }
}


/// Toggles the muted status of a profile
#[tauri::command]
pub async fn toggle_muted(npub: String) -> bool {
    let handle = TAURI_APP.get().unwrap();
    let mut state = STATE.lock().await;

    match state.get_profile_mut(&npub) {
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
    }
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
