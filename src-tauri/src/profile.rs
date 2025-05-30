use nostr_sdk::prelude::*;
use tauri::{Emitter, Manager};
use tauri_plugin_fs::FsExt;

use crate::{NOSTR_CLIENT, STATE, TAURI_APP, PUBLIC_NIP96_CONFIG};
use crate::db;
use crate::Message;

#[derive(serde::Serialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Profile {
    pub id: String,
    pub name: String,
    pub display_name: String,
    pub lud06: String,
    pub lud16: String,
    pub banner: String,
    pub avatar: String,
    pub about: String,
    pub website: String,
    pub nip05: String,
    pub messages: Vec<Message>,
    pub last_read: String,
    pub status: Status,
    pub last_updated: u64,
    pub typing_until: u64,
    pub mine: bool,
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
            lud06: String::new(),
            lud16: String::new(),
            banner: String::new(),
            avatar: String::new(),
            about: String::new(),
            website: String::new(),
            nip05: String::new(),
            messages: Vec::new(),
            last_read: String::new(),
            status: Status::new(),
            last_updated: 0,
            typing_until: 0,
            mine: false,
        }
    }

    /// Get the last message timestamp
    pub fn last_message_time(&self) -> Option<u64> {
        self.messages.last().map(|msg| msg.at)
    }

    /// Get a mutable message by ID
    pub fn get_message_mut(&mut self, id: &str) -> Option<&mut Message> {
        self.messages.iter_mut().find(|msg| msg.id == id)
    }

    /// Set the Last Received message as the "Last Read" message
    pub fn set_as_read(&mut self) -> bool {
        // Ensure we have at least one message received from them
        for msg in self.messages.iter().rev() {
            if !msg.mine {
                // Found the most recent message from them
                self.last_read = msg.id.clone();
                return true;
            }
        }
        
        // No messages from them, can't mark anything as read
        false
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

    /// Add a Message to this Vector Profile
    /// 
    /// This method internally checks for and avoids duplicate messages.
    pub fn internal_add_message(&mut self, message: Message) -> bool {
        // Make sure we don't add the same message twice
        if self.messages.iter().any(|m| m.id == message.id) {
            // Message is already known by the state
            return false;
        }

        // If it's their message; disable their typing indicator until further indicators are sent
        if !message.mine {
            self.typing_until = 0;
        }

        // Fast path for common cases: newest or oldest messages
        if self.messages.is_empty() {
            // First message
            self.messages.push(message);
        } else if message.at >= self.messages.last().unwrap().at {
            // Common case 1: Latest message (append to end)
            self.messages.push(message);
        } else if message.at <= self.messages.first().unwrap().at {
            // Common case 2: Oldest message (insert at beginning)
            self.messages.insert(0, message);
        } else {
            // Less common case: Message belongs somewhere in the middle
            self.messages.insert(
                self.messages.binary_search_by(|m| m.at.cmp(&message.at)).unwrap_or_else(|idx| idx),
                message
            );
        }
        true
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
pub async fn update_profile(name: String, avatar: String) -> bool {
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

    // Add display_name
    if !profile.display_name.is_empty() {
        meta = meta.display_name(&profile.display_name);
    }

    // Add about
    if !profile.about.is_empty() {
        meta = meta.about(&profile.about);
    }

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

    // Broadcast the profile update
    match client.set_metadata(&meta).await {
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
    // Grab the file
    let handle = TAURI_APP.get().unwrap();
    return match handle.fs().read(std::path::Path::new(&filepath)) {
        Ok(file) => {
            // Format a Mime Type from the file extension
            let mime_type = match filepath.rsplit('.').next().unwrap_or("").to_lowercase().as_str() {
                "png" => "image/png",
                "jpg" | "jpeg" => "image/jpeg",
                "gif" => "image/gif",
                "webp" => "image/webp",
                _ => "application/octet-stream",
            };

            // Upload the file to the server
            let client = NOSTR_CLIENT.get().expect("Nostr client not initialized");
            let signer = client.signer().await.unwrap();
            let conf = PUBLIC_NIP96_CONFIG.wait();
            return match nostr_sdk::nips::nip96::upload_data(&signer, &conf, file, Some(mime_type), None).await {
                Ok(url) => Ok(url.to_string()),
                Err(e) => Err(e.to_string())
            }
        },
        Err(_) => Err(String::from("Image couldn't be loaded from disk"))
    }
}

/// Marks a specific message as read
#[tauri::command]
pub async fn mark_as_read(npub: String) -> bool {
    // Only mark as read if the Window is focused (user may have the chat open but the app in the background)
    let handle = TAURI_APP.get().unwrap();
    if !handle
        .webview_windows()
        .iter()
        .next()
        .unwrap()
        .1
        .is_focused()
        .unwrap() {
            // Update the counter to allow for background badge handling of in-chat messages
            crate::update_unread_counter(handle.clone()).await;
            return false;
        }

    // Get a mutable reference to the profile
    let result = {
        let mut state = STATE.lock().await;
        match state.get_profile_mut(&npub) {
            Some(profile) => profile.set_as_read(),
            None => false
        }
    };
    
    // Update the unread counter if the marking was successful
    if result {
        // Update the badge count
        crate::update_unread_counter(handle.clone()).await;

        // Save the "Last Read" marker to the DB
        db::set_profile(handle.clone(), STATE.lock().await.get_profile(&npub).unwrap().clone()).await.unwrap();
    }
    
    result
}