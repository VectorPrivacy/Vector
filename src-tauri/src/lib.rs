use std::borrow::Cow;
use std::sync::Arc;
use argon2::{Argon2, Params, Version};
use lazy_static::lazy_static;
use nostr_sdk::prelude::*;
use once_cell::sync::OnceCell;
use rand::Rng;
use tokio::sync::Mutex;
use chacha20poly1305::{
    aead::{Aead, KeyInit},
    ChaCha20Poly1305, Nonce
};
use ::image::{ImageEncoder, codecs::png::PngEncoder, ExtendedColorType::Rgba8};
use tauri::{AppHandle, Emitter, Manager, Runtime};
use tauri_plugin_clipboard_manager::ClipboardExt;
use tauri_plugin_notification::NotificationExt;
use tauri_plugin_fs::FsExt;
use scraper::{Html, Selector};

mod crypto;

mod db;
use db::SlimProfile;

mod voice;
use voice::AudioRecorder;

mod net;

mod upload;
use upload::{upload_data_with_progress, ProgressCallback};

mod util;
use util::{extract_https_urls, get_file_type_description};

mod whisper;

/// The Maximum byte size that Vector will auto-download.
/// 
/// Files larger than this require explicit user permission to be downloaded.
static MAX_AUTO_DOWNLOAD_BYTES: u64 = 10_485_760;

/// # Trusted Relay
///
/// The 'Trusted Relay' handles events that MAY have a small amount of public-facing metadata attached (i.e: Expiration tags).
///
/// This relay may be used for events like Typing Indicators, Key Exchanges (forward-secrecy setup) and more.
static TRUSTED_RELAY: &str = "wss://jskitty.cat/nostr";

/// # Trusted Public NIP-96 Server
///
/// A temporary hardcoded NIP-96 server, handling file uploads for public files (Avatars, etc)
static TRUSTED_PUBLIC_NIP96: &str = "https://nostr.build";
static PUBLIC_NIP96_CONFIG: OnceCell<ServerConfig> = OnceCell::new();

/// # Trusted Private NIP-96 Server
///
/// A temporary hardcoded NIP-96 server, handling file uploads for encrypted files (in-chat)
static TRUSTED_PRIVATE_NIP96: &str = "https://medea-small.jskitty.cat";
static PRIVATE_NIP96_CONFIG: OnceCell<ServerConfig> = OnceCell::new();


static MNEMONIC_SEED: OnceCell<String> = OnceCell::new();
static ENCRYPTION_KEY: OnceCell<[u8; 32]> = OnceCell::new();
static NOSTR_CLIENT: OnceCell<Client> = OnceCell::new();
static TAURI_APP: OnceCell<AppHandle> = OnceCell::new();

#[derive(serde::Serialize, Clone, Debug, PartialEq)]
pub struct Message {
    id: String,
    content: String,
    replied_to: String,
    preview_metadata: Option<SiteMetadata>,
    attachments: Vec<Attachment>,
    reactions: Vec<Reaction>,
    at: u64,
    pending: bool,
    failed: bool,
    mine: bool,
}

impl Message {
    /// Get an attachment by ID
    fn get_attachment(&self, id: &str) -> Option<&Attachment> {
        self.attachments.iter().find(|p| p.id == id)
    }

    /// Get an attachment by ID
    fn get_attachment_mut(&mut self, id: &str) -> Option<&mut Attachment> {
        self.attachments.iter_mut().find(|p| p.id == id)
    }

    /// Add a Reaction - if it was not already added
    fn add_reaction(&mut self, reaction: Reaction, chat_id: Option<&str>) -> bool {
        // Make sure we don't add the same reaction twice
        if !self.reactions.iter().any(|r| r.id == reaction.id) {
            self.reactions.push(reaction);

            // Update the frontend if a Chat ID was provided
            if let Some(chat) = chat_id {
                let handle = TAURI_APP.get().unwrap();
                handle.emit("message_update", serde_json::json!({
                    "old_id": &self.id,
                    "message": &self,
                    "chat_id": chat
                })).unwrap();
            }
            true
        } else {
            // Reaction was already added previously
            false
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Attachment {
    /// The encryption Nonce as a stringified unique file ID (TODO: change to SHA256 hash)
    id: String,
    // The encryption key
    key: String,
    // The encryption nonce
    nonce: String,
    /// The file extension
    extension: String,
    /// The host URL, typically a NIP-96 server
    url: String,
    /// The storage directory path (typically the ~/Downloads folder)
    path: String,
    /// The download size of the encrypted file
    size: u64,
    /// Whether the file is currently being downloaded or not
    downloading: bool,
    /// Whether the file has been downloaded or not
    downloaded: bool,
}

impl Default for Attachment {
    fn default() -> Self {
        Self {
            id: String::new(),
            key: String::new(),
            nonce: String::new(),
            extension: String::new(),
            url: String::new(),
            path: String::new(),
            size: 0,
            downloading: false,
            downloaded: true,
        }
    }
}

/// A simple pre-upload format to associate a byte stream with a file extension
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
struct AttachmentFile {
    bytes: Vec<u8>,
    extension: String,
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq)]
pub struct Reaction {
    id: String,
    /// The HEX Event ID of the message being reacted to
    reference_id: String,
    /// The HEX ID of the author
    author_id: String,
    /// The emoji of the reaction
    emoji: String,
}

#[derive(serde::Serialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Profile {
    id: String,
    name: String,
    display_name: String,
    lud06: String,
    lud16: String,
    banner: String,
    avatar: String,
    about: String,
    website: String,
    nip05: String,
    messages: Vec<Message>,
    last_read: String,
    status: Status,
    last_updated: u64,
    typing_until: u64,
    mine: bool,
}

impl Default for Profile {
    fn default() -> Self {
        Self::new()
    }
}

impl Profile {
    fn new() -> Self {
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
    fn last_message_time(&self) -> Option<u64> {
        self.messages.last().map(|msg| msg.at)
    }

    /// Get a message by ID
    fn get_message(&self, id: &str) -> Option<&Message> {
        self.messages.iter().find(|msg| msg.id == id)
    }

    /// Get a mutable message by ID
    fn get_message_mut(&mut self, id: &str) -> Option<&mut Message> {
        self.messages.iter_mut().find(|msg| msg.id == id)
    }

    /// Set the Last Received message as the "Last Read" message
    fn set_as_read(&mut self) -> bool {
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
    fn from_metadata(&mut self, meta: Metadata) -> bool {
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
    fn internal_add_message(&mut self, message: Message) -> bool {
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
    title: String,
    purpose: String,
    url: String,
}

impl Status {
    fn new() -> Self {
        Self {
            title: String::new(),
            purpose: String::new(),
            url: String::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
enum SyncMode {
    ForwardSync,   // Initial sync from most recent message going backward
    BackwardSync,  // Syncing historically old messages
    Finished       // Sync complete
}

#[derive(serde::Serialize, Clone, Debug)]
struct ChatState {
    profiles: Vec<Profile>,
    is_syncing: bool,
    sync_window_start: u64,  // Start timestamp of current window
    sync_window_end: u64,    // End timestamp of current window
    sync_mode: SyncMode,
    sync_empty_iterations: u8, // Counter for consecutive empty iterations
    sync_total_iterations: u8, // Counter for total iterations in current mode
}

impl ChatState {
    fn new() -> Self {
        Self {
            profiles: Vec::new(),
            is_syncing: false,
            sync_window_start: 0,
            sync_window_end: 0,
            sync_mode: SyncMode::Finished,
            sync_empty_iterations: 0,
            sync_total_iterations: 0,
        }
    }

    /// Load a Vector Profile in to the state from our SlimProfile database format
    async fn from_db_profile(&mut self, slim: SlimProfile) {
        // Check if profile already exists
        if let Some(position) = self.profiles.iter().position(|profile| profile.id == slim.id) {
            // Replace existing profile
            let mut full_profile = slim.to_profile();

            // Check if this is our profile: we need to mark it as such
            let client = NOSTR_CLIENT.get().expect("Nostr client not initialized");
            let signer = client.signer().await.unwrap();
            let my_public_key = signer.get_public_key().await.unwrap();
            let profile_pubkey = PublicKey::from_bech32(&full_profile.id).unwrap();
            full_profile.mine = my_public_key == profile_pubkey;

            // Preserve existing messages to avoid data loss
            let existing_messages = std::mem::take(&mut self.profiles[position].messages);
            self.profiles[position] = full_profile;
            self.profiles[position].messages = existing_messages;
        } else {
            // Add new profile
            self.profiles.push(slim.to_profile());
        }
    }
    
    /// Merge multiple Vector Profiles from SlimProfile format in to the state at once
    async fn merge_db_profiles(&mut self, slim_profiles: Vec<SlimProfile>) {
        for slim in slim_profiles {
            self.from_db_profile(slim).await;
        }
    }
    
    /// Get a profile by ID
    fn get_profile(&self, id: &str) -> Option<&Profile> {
        self.profiles.iter().find(|p| p.id == id)
    }
    
    /// Get a mutable profile by ID
    fn get_profile_mut(&mut self, id: &str) -> Option<&mut Profile> {
        self.profiles.iter_mut().find(|p| p.id == id)
    }

    /// Add a message to a Vector Profile via it's ID
    fn add_message(&mut self, npub: &str, message: Message) -> bool {
        let is_msg_added = match self.get_profile_mut(npub) {
            Some(profile) =>
                // Add the message to the existing profile
                profile.internal_add_message(message),
            None => {
                // Generate the profile and add the message to it
                let mut profile = Profile::new();
                profile.id = npub.to_string();
                profile.internal_add_message(message);

                // Update the frontend
                let handle = TAURI_APP.get().unwrap();
                handle.emit("profile_update", &profile).unwrap();

                // Push to the Profile (after emission; to save on a clone)
                self.profiles.push(profile);
                true
            }
        };

        // Sort our profile positions based on last message time
        self.profiles.sort_by(|a, b| {
            // Get last message time for both profiles
            let a_time = a.last_message_time();
            let b_time = b.last_message_time();

            // Compare timestamps in reverse order (newest first)
            b_time.cmp(&a_time)
        });

        is_msg_added
    }
    
    /// Count unread messages across all profiles
    fn count_unread_messages(&self) -> u32 {
        let mut total_unread = 0;
         
        for profile in &self.profiles {
            // Skip own profile (mine == true)
            if profile.mine {
                continue;
            }
            
            // If last_read is empty, all messages are unread
            if profile.last_read.is_empty() {
                // Only count messages from others (not mine)
                total_unread += profile.messages.iter()
                    .filter(|msg| !msg.mine)
                    .count() as u32;
                continue;
            }
            
            // Start from newest message, work backwards until we hit last_read
            let mut unread_count = 0;
            for msg in profile.messages.iter().rev() {
                // Only count messages from others, not our own
                if !msg.mine {
                    if msg.id == profile.last_read {
                        // Found the last read message, stop counting
                        break;
                    }
                    unread_count += 1;
                }
            }
            
            total_unread += unread_count;
        }
        
        total_unread
    }
}

lazy_static! {
    static ref STATE: Mutex<ChatState> = Mutex::new(ChatState::new());
}

#[tauri::command]
async fn fetch_messages<R: Runtime>(
    handle: AppHandle<R>,
    init: bool
) {
    let client = NOSTR_CLIENT.get().expect("Nostr client not initialized");

    // Grab our pubkey
    let signer = client.signer().await.unwrap();
    let my_public_key = signer.get_public_key().await.unwrap();

    // Determine the time range to fetch
    let (since_timestamp, until_timestamp) = {
        let mut state = STATE.lock().await;
        
        if init {
            // Load our DB (if we haven't already; i.e: our profile is the single loaded profile since login)
            if state.profiles.len() == 1 {
                let profiles = db::get_all_profiles(&handle).await.unwrap();
                let msgs = db::get_all_messages(&handle).await.unwrap();

                // Load our Profile Cache into the state
                state.merge_db_profiles(profiles).await;

                // Add each message to the state
                for (msg, npub) in msgs {
                    state.add_message(&npub, msg);
                }
            }

            // Send the state to our frontend to signal finalised init with a full state
            handle.emit("init_finished", &state.profiles).unwrap();

            // ALWAYS begin with an initial sync of at least the last 2 days
            let now = Timestamp::now();

            state.is_syncing = true;
            state.sync_mode = SyncMode::ForwardSync;
            state.sync_empty_iterations = 0;
            state.sync_total_iterations = 0;

            // Initial 2-day window: now - 2 days → now
            let two_days_ago = now.as_u64() - (60 * 60 * 24 * 2);

            state.sync_window_start = two_days_ago;
            state.sync_window_end = now.as_u64();

            (
                Timestamp::from_secs(two_days_ago),
                now
            )
        } else if state.sync_mode == SyncMode::ForwardSync {
            // Forward sync (filling gaps from last message to now)
            let window_start = state.sync_window_start;

            // Adjust window for next iteration (go back in time in 2-day increments)
            let new_window_end = window_start;
            let new_window_start = window_start - (60 * 60 * 24 * 2); // Always 2 days

            // Update state with new window
            state.sync_window_start = new_window_start;
            state.sync_window_end = new_window_end;

            (
                Timestamp::from_secs(new_window_start),
                Timestamp::from_secs(new_window_end)
            )
        } else if state.sync_mode == SyncMode::BackwardSync {
            // Backward sync (historically old messages)
            let window_start = state.sync_window_start;

            // Move window backward in time in 2-day increments
            let new_window_end = window_start;
            let new_window_start = window_start - (60 * 60 * 24 * 2); // Always 2 days

            // Update state with new window
            state.sync_window_start = new_window_start;
            state.sync_window_end = new_window_end;

            (
                Timestamp::from_secs(new_window_start),
                Timestamp::from_secs(new_window_end)
            )
        } else {
            // Sync finished or in unknown state
            // Return dummy values, won't be used as we'll end sync
            (Timestamp::now(), Timestamp::now())
        }
    };

    // If sync is finished, emit the finished event and return
    {
        let state = STATE.lock().await;
        if state.sync_mode == SyncMode::Finished {
            handle.emit("sync_finished", ()).unwrap();
            return;
        }
    }

    // Emit our current "Sync Range" to the frontend
    handle.emit("sync_progress", serde_json::json!({
        "since": since_timestamp.as_u64(),
        "until": until_timestamp.as_u64(),
        "mode": format!("{:?}", STATE.lock().await.sync_mode)
    })).unwrap();

    // Fetch GiftWraps related to us within the time window
    let filter = Filter::new()
        .pubkey(my_public_key)
        .kind(Kind::GiftWrap)
        .since(since_timestamp)
        .until(until_timestamp);

    let events = client
        .fetch_events(filter, std::time::Duration::from_secs(30))
        .await
        .unwrap();

    // Decrypt every GiftWrap and process their contents
    let mut new_messages_count: u16 = 0;
    for event in events.into_iter() {
        // Count the amount of accepted (new) events
        if handle_event(event, false).await {
            new_messages_count += 1;
        }
    }

    // Process sync results and determine next steps
    let should_continue = {
        let mut state = STATE.lock().await;
        let mut continue_sync = true;

        // Increment total iterations counter
        state.sync_total_iterations += 1;

        // Update state based on if messages were found
        if new_messages_count > 0 {
            state.sync_empty_iterations = 0;
        } else {
            state.sync_empty_iterations += 1;
        }

        if state.sync_mode == SyncMode::ForwardSync {
            // Forward sync transitions to backward sync after:
            // 1. Finding messages and going 3 more iterations without messages, or
            // 2. Going 5 iterations without finding any messages
            let enough_empty_iterations = state.sync_empty_iterations >= 5;
            let found_then_empty = new_messages_count > 0 && state.sync_empty_iterations >= 3;

            if found_then_empty || enough_empty_iterations {
                // Release the mutex before performing potentially slow operations
                drop(state);

                // Start backward sync from the oldest message
                let oldest_ts_result = get_oldest_message_timestamp().await;

                // Re-acquire mutex after operation
                let mut state = STATE.lock().await;

                // Time to switch mode regardless of result
                state.sync_mode = SyncMode::BackwardSync;
                state.sync_empty_iterations = 0;
                state.sync_total_iterations = 0;

                if let Some(oldest_ts) = oldest_ts_result {
                    state.sync_window_end = oldest_ts;
                    state.sync_window_start = oldest_ts - (60 * 60 * 24 * 2); // 2 days before oldest
                } else {
                    // Still start backward sync, but from recent history
                    let now = Timestamp::now().as_u64();
                    let thirty_days_ago = now - (60 * 60 * 24 * 30);

                    state.sync_window_end = thirty_days_ago;
                    state.sync_window_start = thirty_days_ago - (60 * 60 * 24 * 2);
                }
            }
        } else if state.sync_mode == SyncMode::BackwardSync {
            // For backward sync, continue until:
            // No messages found for 5 consecutive iterations
            let enough_empty_iterations = state.sync_empty_iterations >= 5;

            if enough_empty_iterations {
                // We've completed backward sync
                state.sync_mode = SyncMode::Finished;
                continue_sync = false;
            }
        } else {
            continue_sync = false; // Unknown state, stop syncing
        }

        continue_sync
    };

    if should_continue {
        // Keep synchronising
        handle.emit("sync_slice_finished", ()).unwrap();
    } else {
        // We're done with sync
        let mut state = STATE.lock().await;
        state.sync_mode = SyncMode::Finished;
        state.is_syncing = false;
        state.sync_empty_iterations = 0;
        state.sync_total_iterations = 0;

        handle.emit("sync_finished", ()).unwrap();
    }
}

async fn get_oldest_message_timestamp() -> Option<u64> {
    let state = STATE.lock().await;
    let profiles = &state.profiles;
    
    let mut oldest_timestamp = None;
    
    // Check each profile's messages
    for profile in profiles {
        // If this profile has messages
        if !profile.messages.is_empty() {
            // Since messages are already ordered by time, the first one is the oldest
            if let Some(oldest_msg) = profile.messages.first() {
                match oldest_timestamp {
                    None => oldest_timestamp = Some(oldest_msg.at),
                    Some(current_oldest) => {
                        if oldest_msg.at < current_oldest {
                            oldest_timestamp = Some(oldest_msg.at);
                        }
                    }
                }
            }
        }
    }
    
    oldest_timestamp
}

#[tauri::command]
async fn message(receiver: String, content: String, replied_to: String, file: Option<AttachmentFile>) -> Result<bool, String> {
    // Immediately add the message to our state as "Pending", we'll update it as either Sent (non-pending) or Failed in the future
    let pending_count = STATE
        .lock()
        .await
        .get_profile(&receiver)
        .unwrap_or(&Profile::new())
        .messages
        .iter()
        .filter(|m| m.pending)
        .count();
    // Create persistent pending_id that will live for the entire function
    let pending_id = Arc::new(String::from("pending-") + &pending_count.to_string());
    let msg = Message {
        id: pending_id.as_ref().clone(),
        content,
        replied_to,
        preview_metadata: None,
        at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs(),
        attachments: Vec::new(),
        reactions: Vec::new(),
        pending: true,
        failed: false,
        mine: true,
    };
    STATE.lock().await.add_message(&receiver, msg.clone());

    // Grab our pubkey
    let client = NOSTR_CLIENT.get().expect("Nostr client not initialized");
    let signer = client.signer().await.unwrap();
    let my_public_key = signer.get_public_key().await.unwrap();

    // Convert the Bech32 String in to a PublicKey
    let receiver_pubkey = PublicKey::from_bech32(receiver.clone().as_str()).unwrap();

    // Prepare the NIP-17 rumor
    let handle = TAURI_APP.get().unwrap();
    let mut rumor = if file.is_none() {
        // Send the text message to our frontend
        handle.emit("message_new", serde_json::json!({
            "message": &msg,
            "chat_id": &receiver
        })).unwrap();

        // Text Message
        EventBuilder::private_msg_rumor(receiver_pubkey, msg.content)
    } else {
        let attached_file = file.unwrap();

        // Encrypt the attachment
        let params = crypto::generate_encryption_params();
        let enc_file = crypto::encrypt_data(attached_file.bytes.as_slice(), &params).unwrap();

        // Update the attachment in-state
        {
            // Use a clone of the Arc for this block
            let pending_id_clone = Arc::clone(&pending_id);
            
            // Retrieve the Pending Message
            let mut state = STATE.lock().await;
            let chat = state.get_profile_mut(&receiver).unwrap();
            let message = chat.get_message_mut(pending_id_clone.as_ref()).unwrap();

            // Choose the appropriate base directory based on platform
            let base_directory = if cfg!(target_os = "ios") {
                tauri::path::BaseDirectory::Document
            } else {
                tauri::path::BaseDirectory::Download
            };

            // Resolve the directory path using the determined base directory
            let dir = handle.path().resolve("vector", base_directory).unwrap();

            // Store the nonce-based file name on-disk for future reference
            let nonce_file_path = dir.join(format!("{}.{}", &params.nonce, &attached_file.extension));

            // Create the vector directory if it doesn't exist
            std::fs::create_dir_all(&dir).unwrap();

            // Save the nonce-named file
            std::fs::write(&nonce_file_path, &attached_file.bytes).unwrap();

            // Add the Attachment in-state (with our local path, to prevent re-downloading it accidentally from server)
            message.attachments.push(Attachment {
                // Temp: id will soon become a SHA256 hash of the file
                id: params.nonce.clone(),
                key: params.key.clone(),
                nonce: params.nonce.clone(),
                extension: attached_file.extension.clone(),
                url: String::new(),
                path: nonce_file_path.to_string_lossy().to_string(),
                size: enc_file.len() as u64,
                downloading: false,
                downloaded: true
            });

            // Send the pending file upload to our frontend
            handle.emit("message_new", serde_json::json!({
                "message": &message,
                "chat_id": &receiver
            })).unwrap();
        }

        // Format a Mime Type from the file extension
        let mime_type = match attached_file.extension.as_str() {
            // Images
            "png" => "image/png",
            "jpg" | "jpeg" => "image/jpeg",
            "gif" => "image/gif",
            "webp" => "image/webp",
            // Audio
            "wav" => "audio/wav",
            "mp3" => "audio/mp3",
            // Videos
            "mp4" => "video/mp4",
            "webm" => "video/webm",
            "mov" => "video/quicktime",
            "avi" => "video/x-msvideo",
            "mkv" => "video/x-matroska",
            // Unknown
            _ => "application/octet-stream",
        };

        // Upload the file to the server
        let client = NOSTR_CLIENT.get().expect("Nostr client not initialized");
        let signer = client.signer().await.unwrap();
        let conf = PRIVATE_NIP96_CONFIG.wait();
        let file_size = enc_file.len();
        // Clone the Arc outside the closure for use inside a seperate-threaded progress callback
        let pending_id_for_callback = Arc::clone(&pending_id);
        // Create a progress callback for file uploads
        let progress_callback: ProgressCallback = Box::new(move |percentage, _| {
                // This is a simple callback that logs progress but could be enhanced to emit events
                if let Some(pct) = percentage {
                    handle.emit("attachment_upload_progress", serde_json::json!({
                        "id": pending_id_for_callback.as_ref(),
                        "progress": pct
                    })).unwrap();
                }
            Ok(())
        });

        match upload_data_with_progress(&signer, &conf, enc_file, Some(mime_type), None, progress_callback).await {
            Ok(url) => {
                // Create the attachment rumor
                let attachment_rumor = EventBuilder::new(Kind::from_u16(15), url.to_string());

                // Append decryption keys and file metadata
                attachment_rumor
                    .tag(Tag::public_key(receiver_pubkey))
                    .tag(Tag::custom(TagKind::custom("file-type"), [mime_type]))
                    .tag(Tag::custom(TagKind::custom("size"), [file_size.to_string()]))
                    .tag(Tag::custom(TagKind::custom("encryption-algorithm"), ["aes-gcm"]))
                    .tag(Tag::custom(TagKind::custom("decryption-key"), [params.key.as_str()]))
                    .tag(Tag::custom(TagKind::custom("decryption-nonce"), [params.nonce.as_str()]))
            },
            Err(_) => {
                // The file upload failed: so we mark the message as failed and notify of an error
                let pending_id_for_failure = Arc::clone(&pending_id);
                let mut state = STATE.lock().await;
                let chat = state.get_profile_mut(&receiver).unwrap();
                let failed_msg = chat.get_message_mut(pending_id_for_failure.as_ref()).unwrap();
                failed_msg.failed = true;

                // Update the frontend
                handle.emit("message_update", serde_json::json!({
                    "old_id": pending_id_for_failure.as_ref(),
                    "message": &failed_msg,
                    "chat_id": &receiver
                })).unwrap();

                // Return the error
                return Err(String::from("Failed to upload file"));
            }
        }
    };

    // If a reply reference is included, add the tag
    if !msg.replied_to.is_empty() {
        rumor = rumor.tag(Tag::custom(
            TagKind::e(),
            [msg.replied_to, String::from(""), String::from("reply")],
        ));
    }

    // Build the rumor with our key (unsigned)
    let built_rumor = rumor.build(my_public_key);
    let rumor_id = built_rumor.id.unwrap();

    // Send message to the real receiver
    match client
        .gift_wrap(&receiver_pubkey, built_rumor.clone(), [])
        .await
    {
        Ok(_) => {
            // Send message to our own public key, to allow for message recovering
            match client
                .gift_wrap(&my_public_key, built_rumor, [])
                .await
            {
                Ok(_) => {
                    // Mark the message as a success
                    let pending_id_for_success = Arc::clone(&pending_id);
                    let mut state = STATE.lock().await;
                    let chat = state.get_profile_mut(&receiver).unwrap();
                    let sent_msg = chat.get_message_mut(pending_id_for_success.as_ref()).unwrap();
                    sent_msg.id = rumor_id.to_hex();
                    sent_msg.pending = false;

                    // Update the frontend
                    handle.emit("message_update", serde_json::json!({
                        "old_id": pending_id_for_success.as_ref(),
                        "message": &sent_msg,
                        "chat_id": &receiver
                    })).unwrap();

                    // Save the message to our DB
                    let handle = TAURI_APP.get().unwrap();
                    db::save_message(handle.clone(), sent_msg.clone(), receiver).await.unwrap();
                    return Ok(true);
                }
                Err(_) => {
                    // This is an odd case; the message was sent to the receiver, but NOT ourselves
                    // We'll class it as sent, for now...
                    let pending_id_for_partial = Arc::clone(&pending_id);
                    let mut state = STATE.lock().await;
                    let chat = state.get_profile_mut(&receiver).unwrap();
                    let sent_ish_msg = chat.get_message_mut(pending_id_for_partial.as_ref()).unwrap();
                    sent_ish_msg.id = rumor_id.to_hex();
                    sent_ish_msg.pending = false;

                    // Update the frontend
                    handle.emit("message_update", serde_json::json!({
                        "old_id": pending_id_for_partial.as_ref(),
                        "message": &sent_ish_msg,
                        "chat_id": &receiver
                    })).unwrap();

                    // Save the message to our DB
                    let handle = TAURI_APP.get().unwrap();
                    db::save_message(handle.clone(), sent_ish_msg.clone(), receiver).await.unwrap();
                    return Ok(true);
                }
            }
        }
        Err(_) => {
            // Mark the message as a failure, bad message, bad!
            let pending_id_for_final = Arc::clone(&pending_id);
            let mut state = STATE.lock().await;
            let chat = state.get_profile_mut(&receiver).unwrap();
            let failed_msg = chat.get_message_mut(pending_id_for_final.as_ref()).unwrap();
            failed_msg.failed = true;
            return Ok(false);
        }
    }
}

#[tauri::command]
async fn paste_message<R: Runtime>(handle: AppHandle<R>, receiver: String, replied_to: String, transparent: bool) -> Result<bool, String> {
    // Copy the image from the clipboard
    let img = handle.clipboard().read_image().unwrap();

    // Create the encoder directly with a Vec<u8>
    let mut png_data = Vec::new();
    let encoder = PngEncoder::new(&mut png_data);

    // Get original pixels
    let original_pixels = img.rgba();

    // Windows: check that every image has a non-zero-ish Alpha channel, if not, this is probably a non-PNG/GIF which has had it's Alpha channel nuked
    let mut _transparency_bug_search = false;
    #[cfg(target_os = "windows")]
    {
        _transparency_bug_search = original_pixels.iter().skip(3).step_by(4).all(|&a| a <= 2);
    }

    // For non-transparent images: we need to manually account for the zero'ing out of the Alpha channel
    let pixels = if !transparent || _transparency_bug_search {
        // Only clone if we need to modify
        let mut modified = original_pixels.to_vec();
        modified.iter_mut().skip(3).step_by(4).for_each(|a| *a = 255);
        Cow::Owned(modified)
    } else {
        // No modification needed, use the original data
        Cow::Borrowed(original_pixels)
    };

    // Encode directly from pixels to PNG bytes
    encoder.write_image(
        &pixels,               // raw pixels
        img.width(),           // width
        img.height(),          // height
        Rgba8                  // color type
    ).map_err(|e| e.to_string()).unwrap();

    // Generate an Attachment File
    let attachment_file = AttachmentFile {
        bytes: png_data,
        extension: String::from("png")
    };

    // Message the file to the intended user
    message(receiver, String::new(), replied_to, Some(attachment_file)).await
}

#[tauri::command]
async fn voice_message(receiver: String, replied_to: String, bytes: Vec<u8>) -> Result<bool, String> {
    // Generate an Attachment File
    let attachment_file = AttachmentFile {
        bytes,
        extension: String::from("wav")
    };

    // Message the file to the intended user
    message(receiver, String::new(), replied_to, Some(attachment_file)).await
}

#[tauri::command]
async fn file_message(receiver: String, replied_to: String, file_path: String) -> Result<bool, String> {
    // Parse the file extension
    let ext = file_path.clone().rsplit('.').next().unwrap_or("").to_lowercase();

    // Load the file
    let bytes = std::fs::read(file_path.as_str()).unwrap();

    // Generate an Attachment File
    let attachment_file = AttachmentFile {
        bytes,
        extension: ext.to_string()
    };

    // Message the file to the intended user
    message(receiver, String::new(), replied_to, Some(attachment_file)).await
}

/// Pre-fetch the configs from our preferred NIP-96 servers to speed up uploads
#[tauri::command]
async fn warmup_nip96_servers() -> bool {
    // Public Fileserver
    if PUBLIC_NIP96_CONFIG.get().is_none() {
        let _ = match get_server_config(Url::parse(TRUSTED_PUBLIC_NIP96).unwrap(), None).await {
            Ok(conf) => PUBLIC_NIP96_CONFIG.set(conf),
            Err(_) => return false
        };
    }

    // Private Fileserver
    if PRIVATE_NIP96_CONFIG.get().is_none() {
        let _ = match get_server_config(Url::parse(TRUSTED_PRIVATE_NIP96).unwrap(), None).await {
            Ok(conf) => PRIVATE_NIP96_CONFIG.set(conf),
            Err(_) => return false
        };
    }

    // We've got the configs for all our servers, nice!
    true
}

#[tauri::command]
async fn react(reference_id: String, npub: String, emoji: String) -> Result<bool, ()> {
    let client = NOSTR_CLIENT.get().expect("Nostr client not initialized");

    // Prepare the EventID and Pubkeys for rumor building
    let reference_event = EventId::from_hex(reference_id.as_str()).unwrap();
    let receiver_pubkey = PublicKey::from_bech32(npub.as_str()).unwrap();

    // Grab our pubkey
    let signer = client.signer().await.unwrap();
    let my_public_key = signer.get_public_key().await.unwrap();

    // Build our NIP-25 Reaction rumor
    let rumor = EventBuilder::reaction_extended(
        reference_event,
        receiver_pubkey,
        Some(Kind::PrivateDirectMessage),
        &emoji,
    )
    .build(my_public_key);
    let rumor_id = rumor.id.unwrap();

    // Send reaction to the real receiver
    client
        .gift_wrap(&receiver_pubkey, rumor.clone(), [])
        .await
        .unwrap();

    // Send reaction to our own public key, to allow for recovering
    match client.gift_wrap(&my_public_key, rumor, []).await {
        Ok(_) => {
            // And add our reaction locally
            let reaction = Reaction {
                id: rumor_id.to_hex(),
                reference_id: reference_id.clone(),
                author_id: my_public_key.to_hex(),
                emoji,
            };

            // Commit it to our local state
            let mut state = STATE.lock().await;
            let profile = state.get_profile_mut(&npub).unwrap();
            let chat_id = profile.id.clone();
            let msg = profile.get_message_mut(&reference_id).unwrap();
            let was_reaction_added_to_state = msg.add_reaction(reaction, Some(&chat_id));
            if was_reaction_added_to_state {
                // Save the message's reaction to our DB
                let handle = TAURI_APP.get().unwrap();
                db::save_message(handle.clone(), msg.clone(), npub).await.unwrap();
            }
            return Ok(was_reaction_added_to_state);
        }
        Err(e) => {
            eprintln!("Error: {:?}", e);
            return Ok(false);
        }
    }
}

#[tauri::command]
async fn load_profile(npub: String) -> bool {
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
async fn update_profile(name: String, avatar: String) -> bool {
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
async fn update_status(status: String) -> bool {
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
async fn upload_avatar(filepath: String) -> Result<String, String> {
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
            return match upload_data(&signer, &conf, file, Some(mime_type), None).await {
                Ok(url) => Ok(url.to_string()),
                Err(e) => Err(e.to_string())
            }
        },
        Err(_) => Err(String::from("Image couldn't be loaded from disk"))
    }
}

#[tauri::command]
async fn start_typing(receiver: String) -> bool {
    let client = NOSTR_CLIENT.get().expect("Nostr client not initialized");

    // Convert our Bech32 receiver to a PublicKey
    let receiver_pubkey = PublicKey::from_bech32(receiver.as_str()).unwrap();

    // Grab our pubkey
    let signer = client.signer().await.unwrap();
    let my_public_key = signer.get_public_key().await.unwrap();

    // Build and broadcast the Typing Indicator
    let rumor = EventBuilder::new(Kind::ApplicationSpecificData, "typing")
        .tag(Tag::public_key(receiver_pubkey))
        .tag(Tag::custom(TagKind::d(), vec!["vector"]))
        .tag(Tag::expiration(Timestamp::from_secs(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs()
                + 30,
        )))
        .build(my_public_key);

    // Gift Wrap and send our Typing Indicator to receiver via our Trusted Relay
    // Note: we set a "public-facing" 1-hour expiry so that our trusted NIP-40 relay can purge old Typing Indicators
    let expiry_time = Timestamp::from_secs(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 3600,
    );
    match client
        .gift_wrap_to(
            [TRUSTED_RELAY],
            &receiver_pubkey,
            rumor,
            [Tag::expiration(expiry_time)],
        )
        .await
    {
        Ok(_) => true,
        Err(_) => false,
    }
}

#[tauri::command]
async fn handle_event(event: Event, is_new: bool) -> bool {
    let client = NOSTR_CLIENT.get().expect("Nostr client not initialized");

    // Grab our pubkey
    let signer = client.signer().await.unwrap();
    let my_public_key = signer.get_public_key().await.unwrap();

    // Unwrap the gift wrap
    match client.unwrap_gift_wrap(&event).await {
        Ok(UnwrappedGift { rumor, sender }) => {
            // Check if it's mine
            let is_mine = sender == my_public_key;

            // Attempt to get contact public key (bech32)
            let contact: String = if is_mine {
                // Try to get the first public key from tags
                match rumor.tags.public_keys().next() {
                    Some(pub_key) => match pub_key.to_bech32() {
                        Ok(p_tag_pubkey_bech32) => p_tag_pubkey_bech32,
                        Err(_) => {
                            eprintln!("Failed to convert public key to bech32");
                            // If conversion fails, fall back to sender
                            sender
                                .to_bech32()
                                .expect("Failed to convert sender's public key to bech32")
                        }
                    },
                    None => {
                        eprintln!("No public key tag found");
                        // If no public key found in tags, fall back to sender
                        sender
                            .to_bech32()
                            .expect("Failed to convert sender's public key to bech32")
                    }
                }
            } else {
                // If not is_mine, just use sender's bech32
                sender
                    .to_bech32()
                    .expect("Failed to convert sender's public key to bech32")
            };

            // Direct Message (NIP-17)
            if rumor.kind == Kind::PrivateDirectMessage {
                // Check if the message replies to anything
                let mut replied_to = String::new();
                match rumor.tags.find(TagKind::e()) {
                    Some(tag) => {
                        if tag.is_reply() {
                            // Add the referred Event ID to our `replied_to` field
                            replied_to = tag.content().unwrap().to_string();
                        }
                    }
                    None => (),
                };

                // Send an OS notification for incoming messages
                if !is_mine && is_new {
                    // Find the name of the sender, if we have it
                    let display_name = match STATE.lock().await.get_profile(&contact) {
                        Some(profile) => {
                            // We have a profile, just check for a name
                            match profile.name.is_empty() {
                                true => String::from("New Message"),
                                false => profile.name.clone(),
                            }
                        }
                        // No profile
                        None => String::from("New Message"),
                    };
                    show_notification(display_name, rumor.content.clone());
                }

                // Create the Message
                let msg = Message {
                    id: rumor.id.unwrap().to_hex(),
                    content: rumor.content,
                    replied_to,
                    preview_metadata: None,
                    at: rumor.created_at.as_u64(),
                    attachments: Vec::new(),
                    reactions: Vec::new(),
                    mine: is_mine,
                    pending: false,
                    failed: false,
                };

                // Add the message to the state
                let was_msg_added_to_state = STATE.lock().await.add_message(&contact, msg.clone());

                // If accepted in-state: commit to the DB and emit to the frontend
                if was_msg_added_to_state {
                    // Send it to the frontend
                    let handle = TAURI_APP.get().unwrap();
                    handle.emit("message_new", serde_json::json!({
                        "message": &msg,
                        "chat_id": &contact
                    })).unwrap();

                    // Save the message to our DB
                    let handle = TAURI_APP.get().unwrap();
                    db::save_message(handle.clone(), msg, contact).await.unwrap();
                }

                was_msg_added_to_state
            }
            // Emoji Reaction (NIP-25)
            else if rumor.kind == Kind::Reaction {
                match rumor.tags.find(TagKind::e()) {
                    Some(react_reference_tag) => {
                        // The message ID being 'reacted' to
                        let reference_id = react_reference_tag.content().unwrap();

                        // Create the Reaction
                        let reaction = Reaction {
                            id: rumor.id.unwrap().to_hex(),
                            reference_id: reference_id.to_string(),
                            author_id: sender.to_hex(),
                            emoji: rumor.content,
                        };

                        // Add the reaction
                        // TODO: since we typically sync "backwards", a reaction may be received before we have any
                        // ... concept of the Profile or Message, sometime in the future, we need to track these "ahead"
                        // ... reactions and re-apply them once sync has finished.
                        let mut state = STATE.lock().await;
                        let maybe_profile = state.get_profile_mut(&contact);
                        if maybe_profile.is_some() {
                            let profile = maybe_profile.unwrap();
                            let chat_id = profile.id.clone();
                            let maybe_msg = profile.get_message_mut(&reference_id);
                            if maybe_msg.is_some() {
                                let msg = maybe_msg.unwrap();
                                let was_reaction_added_to_state = msg.add_reaction(reaction, Some(&chat_id));
                                if was_reaction_added_to_state {
                                    // Save the message's reaction to our DB
                                    let handle = TAURI_APP.get().unwrap();
                                    db::save_message(handle.clone(), msg.clone(), contact).await.unwrap();
                                }
                                was_reaction_added_to_state
                            } else {
                                false
                            }
                        } else {
                            false
                        }
                    }
                    None => false /* No Reference (Note ID) supplied */,
                }
            }
            // Files and Images
            else if rumor.kind == Kind::from_u16(15) {
                // Extract our AES-GCM decryption key and nonce
                let decryption_key = rumor.tags.find(TagKind::Custom(Cow::Borrowed("decryption-key"))).unwrap().content().unwrap();
                let decryption_nonce = rumor.tags.find(TagKind::Custom(Cow::Borrowed("decryption-nonce"))).unwrap().content().unwrap();

                // Extract the content storage URL
                let content_url = rumor.content;

                // Figure out the file extension from the mime-type
                let mime_type = rumor.tags.find(TagKind::Custom(Cow::Borrowed("file-type"))).unwrap().content().unwrap();
                let extension = match mime_type.split('/').nth(1) {
                    // Images
                    Some("png") => "png",
                    Some("jpeg") => "jpg",
                    Some("jpg") => "jpg",
                    Some("gif") => "gif",
                    Some("webp") => "webp",
                    // Audio
                    Some("wav") => "wav",
                    Some("x-wav") => "wav",
                    Some("wave") => "wav",
                    Some("mp3") => "mp3",
                    // Videos
                    Some("mp4") => "mp4",
                    Some("webm") => "webm",
                    Some("quicktime") => "mov",
                    Some("x-msvideo") => "avi",
                    Some("x-matroska") => "mkv",
                    // Fallback options
                    Some(ext) => ext,
                    None => "bin",
                };

                // Check if the file exists on our system already
                let handle = TAURI_APP.get().unwrap();
                // Choose the appropriate base directory based on platform
                let base_directory = if cfg!(target_os = "ios") {
                    tauri::path::BaseDirectory::Document
                } else {
                    tauri::path::BaseDirectory::Download
                };

                // Resolve the directory path using the determined base directory
                let dir = handle.path().resolve("vector", base_directory).unwrap();
                let file_path = dir.join(format!("{}.{}", decryption_nonce, extension));
                let size: u64;
                let downloaded: bool;

                // Grab the reported file size - it's noteworthy that this COULD be missing or wrong, so must be treated as an assumption or guide
                let reported_size = rumor.tags
                    .find(TagKind::Custom(Cow::Borrowed("size")))
                    .map_or(0, |tag| tag.content().unwrap_or("0").parse().unwrap_or(0));

                // Is the file already downloaded?
                if file_path.exists() {
                    size = reported_size;
                    downloaded = true;
                }
                // Is the filesize known?
                else if reported_size == 0 {
                    size = 0;
                    downloaded = false;
                }
                // Does it meet our autodownload policy?
                else if reported_size > MAX_AUTO_DOWNLOAD_BYTES {
                    // File size is either unknown, or too large
                    downloaded = false;
                    size = reported_size;
                }
                // Is it small enough to auto-download during sync? (to avoid blocking the sync thread too long)
                else if is_new && reported_size <= 262144 { // 256 KB or less
                    // Small file, download immediately
                    let small_attachment = Attachment {
                        id: decryption_nonce.to_string(),
                        key: decryption_key.to_string(),
                        nonce: decryption_nonce.to_string(),
                        extension: extension.to_string(),
                        url: content_url.clone(),
                        path: file_path.to_string_lossy().to_string(),
                        size: reported_size,
                        downloading: false,
                        downloaded: false
                    };
                    
                    // Download silently (no progress reporting) with a 5-second timeout
                    if let Ok(encrypted_data) = net::download_silent(&content_url, Some(std::time::Duration::from_secs(5))).await {
                        // Decrypt and save the file
                        if let Ok(_) = decrypt_and_save_attachment(handle, &encrypted_data, &small_attachment).await {
                            // Successfully downloaded and decrypted
                            downloaded = true;
                        } else {
                            // Failed to decrypt
                            downloaded = false;
                        }
                    } else {
                        // Failed to download
                        downloaded = false;
                    }
                    
                    size = reported_size;
                }
                // File size is good but larger than our auto-sync threshold
                else {
                    // We'll adjust our metadata to let the frontend know this file is ready for download
                    downloaded = false;
                    size = reported_size;
                }

                // Check if the message replies to anything
                let mut replied_to = String::new();
                match rumor.tags.find(TagKind::e()) {
                    Some(tag) => {
                        if tag.is_reply() {
                            // Add the referred Event ID to our `replied_to` field
                            replied_to = tag.content().unwrap().to_string();
                        }
                    }
                    None => (),
                };

                // Send an OS notification for incoming files
                if !is_mine && is_new {
                    // Find the name of the sender, if we have it
                    let display_name = match STATE.lock().await.get_profile(&contact) {
                        Some(profile) => {
                            // We have a profile, just check for a name
                            match profile.name.is_empty() {
                                true => String::from("New Message"),
                                false => profile.name.clone(),
                            }
                        }
                        // No profile
                        None => String::from("New Message"),
                    };

                    // Create a "description" of the attachment file
                    show_notification(display_name, "Sent a ".to_string() + &get_file_type_description(extension));
                }

                // Create an attachment
                let mut attachments = Vec::new();
                let attachment = Attachment {
                    id: decryption_nonce.to_string(),
                    key: decryption_key.to_string(),
                    nonce: decryption_nonce.to_string(),
                    extension: extension.to_string(),
                    url: content_url,
                    path: file_path.to_string_lossy().to_string(),
                    size,
                    downloading: false,
                    downloaded
                };
                attachments.push(attachment);

                // Create the message
                let msg = Message {
                    id: rumor.id.unwrap().to_hex(),
                    content: String::new(),
                    replied_to,
                    preview_metadata: None,
                    at: rumor.created_at.as_u64(),
                    attachments,
                    reactions: Vec::new(),
                    mine: is_mine,
                    pending: false,
                    failed: false,
                };

                // Add the message to the state
                let was_msg_added_to_state = STATE.lock().await.add_message(&contact, msg.clone());

                // If accepted in-state: commit to the DB and emit to the frontend
                if was_msg_added_to_state {
                    // Send it to the frontend
                    let handle = TAURI_APP.get().unwrap();
                    handle.emit("message_new", serde_json::json!({
                        "message": &msg,
                        "chat_id": &contact
                    })).unwrap();

                    // Save the message to our DB
                    let handle = TAURI_APP.get().unwrap();
                    db::save_message(handle.clone(), msg, contact).await.unwrap();
                }

                was_msg_added_to_state
            }
            // Vector-specific events (NIP-78)
            else if rumor.kind == Kind::ApplicationSpecificData {
                // Ensure the application target is ours
                match rumor.tags.find(TagKind::d()) {
                    Some(d_tag) => {
                        if d_tag.content().unwrap() == "vector" {
                            // Typing Indicator
                            if rumor.content == "typing" {
                                // A NIP-40 expiry must be present
                                match rumor.tags.find(TagKind::Expiration) {
                                    Some(ex_tag) => {
                                        // And it must be within 30 seconds
                                        let expiry_timestamp: u64 =
                                            ex_tag.content().unwrap().parse().unwrap_or(0);
                                        // Check if the expiry timestamp is within 30 seconds from now
                                        let current_timestamp = std::time::SystemTime::now()
                                            .duration_since(std::time::UNIX_EPOCH)
                                            .unwrap()
                                            .as_secs();
                                        if expiry_timestamp <= current_timestamp + 30
                                            && expiry_timestamp > current_timestamp
                                        {
                                            // Now we apply the typing indicator to it's author profile
                                            match STATE.lock().await
                                                .get_profile_mut(&rumor.pubkey.to_bech32().unwrap())
                                            {
                                                Some(profile) => {
                                                    // Apply typing indicator
                                                    profile.typing_until = expiry_timestamp;

                                                    // Update the frontend
                                                    let handle = TAURI_APP.get().unwrap();
                                                    handle.emit("profile_update", &profile).unwrap();
                                                    true
                                                }
                                                None => false, /* Received a Typing Indicator from an unknown contact, ignoring... */
                                            }
                                        } else {
                                            false
                                        }
                                    }
                                    None => false,
                                }
                            } else {
                                false
                            }
                        } else {
                            false
                        }
                    }
                    None => false,
                }
            } else {
                false
            }
        }
        Err(_) => false,
    }
}

#[tauri::command]
async fn notifs() -> Result<bool, String> {
    let client = NOSTR_CLIENT.get().expect("Nostr client not initialized");

    // Grab our pubkey
    let signer = client.signer().await.unwrap();
    let pubkey = signer.get_public_key().await.unwrap();

    // Listen for GiftWraps related to us
    let filter = Filter::new().pubkey(pubkey).kind(Kind::GiftWrap).limit(0);

    // Subscribe to the filter and begin handling incoming events
    let sub_id = match client.subscribe(filter, None).await {
        Ok(id) => id.val,
        Err(e) => return Err(e.to_string()),
    };

    // Begin watching for notifications from our subscription
    match client
        .handle_notifications(|notification| async {
            if let RelayPoolNotification::Event { event, subscription_id, .. } = notification {
                if subscription_id == sub_id {
                    handle_event(*event, true).await;
                }
            }
            Ok(false)
        })
        .await
    {
        Ok(_) => Ok(true),
        Err(e) => Err(e.to_string()),
    }
}

#[tauri::command]
fn show_notification(title: String, content: String) {
    let handle = TAURI_APP.get().unwrap();
    // Only send notifications if the app is not focused
    // TODO: generalise this assumption - it's only used for Message Notifications at the moment
    if !handle
        .webview_windows()
        .iter()
        .next()
        .unwrap()
        .1
        .is_focused()
        .unwrap()
    {
        handle
            .notification()
            .builder()
            .title(title)
            .body(content)
            .show()
            .unwrap_or_else(|e| eprintln!("Failed to send notification: {}", e));
    }
}

/// Decrypts and saves an attachment to disk
/// 
/// Returns the path to the decrypted file if successful, or an error message if unsuccessful
async fn decrypt_and_save_attachment<R: tauri::Runtime>(
    handle: &AppHandle<R>,
    encrypted_data: &[u8],
    attachment: &Attachment
) -> Result<std::path::PathBuf, String> {
    // Attempt to decrypt the attachment
    let decrypted_data = crypto::decrypt_data(encrypted_data, &attachment.key, &attachment.nonce)
        .map_err(|e| e.to_string())?;
    
    // Choose the appropriate base directory based on platform
    let base_directory = if cfg!(target_os = "ios") {
        tauri::path::BaseDirectory::Document
    } else {
        tauri::path::BaseDirectory::Download
    };

    // Resolve the directory path using the determined base directory
    let dir = handle.path().resolve("vector", base_directory).unwrap();
    let file_path = dir.join(format!("{}.{}", attachment.id, attachment.extension));

    // Create the vector directory if it doesn't exist
    std::fs::create_dir_all(&dir).map_err(|e| format!("Failed to create directory: {}", e))?;

    // Save the file to disk
    std::fs::write(&file_path, decrypted_data).map_err(|e| format!("Failed to write file: {}", e))?;
    
    Ok(file_path)
}

#[tauri::command]
async fn download_attachment(npub: String, msg_id: String, attachment_id: String) -> bool {
    // Grab the attachment's metadata
    let attachment = {
        let mut state = STATE.lock().await;
        let mut_attachment = state
            .get_profile_mut(&npub).unwrap()
            .get_message_mut(&msg_id).unwrap()
            .get_attachment_mut(&attachment_id).unwrap();

        // Check that we're not already downloading
        if mut_attachment.downloading {
            return false;
        }

        // Enable the downloading flag to prevent re-calls
        mut_attachment.downloading = true;

        // Return a clone to allow dropping the State Mutex lock during the download
        mut_attachment.clone()
    };

    // Begin our download progress events
    let handle = TAURI_APP.get().unwrap();
    handle.emit("attachment_download_progress", serde_json::json!({
        "id": &attachment.id,
        "progress": 0
    })).unwrap();

    // Download the file - no timeout, allow large downloads to complete
    let encrypted_data = match net::download(&attachment.url, handle, &attachment.id, None).await {
        Ok(data) => data,
        Err(error) => {
            // Handle download error
            let mut state = STATE.lock().await;
            let mut_profile = state.get_profile_mut(&npub).unwrap();

            // Store all necessary IDs first
            let profile_id = mut_profile.id.clone();
            let msg_id_clone = msg_id.clone();
            let attachment_id_clone = attachment_id.clone();

            // Update the attachment status
            let mut_msg = mut_profile.get_message_mut(&msg_id).unwrap();
            let mut_attachment = mut_msg.get_attachment_mut(&attachment_id).unwrap();
            mut_attachment.downloading = false;
            mut_attachment.downloaded = false;

            // Emit the error
            handle.emit("attachment_download_result", serde_json::json!({
                "profile_id": profile_id,
                "msg_id": msg_id_clone,
                "id": attachment_id_clone,
                "success": false,
                "result": error
            })).unwrap();
            return false;
        }
    };

    // Decrypt and save the file
    let result = decrypt_and_save_attachment(handle, &encrypted_data, &attachment).await;
    
    // Process the result
    if let Err(error) = result {
        // Handle decryption/saving error
        let mut state = STATE.lock().await;
        let mut_profile = state.get_profile_mut(&npub).unwrap();

        // Store all necessary IDs first
        let profile_id = mut_profile.id.clone();
        let msg_id_clone = msg_id.clone();
        let attachment_id_clone = attachment_id.clone();

        // Update the attachment status
        let mut_msg = mut_profile.get_message_mut(&msg_id).unwrap();
        let mut_attachment = mut_msg.get_attachment_mut(&attachment_id).unwrap();
        mut_attachment.downloading = false;
        mut_attachment.downloaded = false;

        // Emit the error
        handle.emit("attachment_download_result", serde_json::json!({
            "profile_id": profile_id,
            "msg_id": msg_id_clone,
            "id": attachment_id_clone,
            "success": false,
            "result": error
        })).unwrap();
        return false;
    }

    // Update state with successful download
    {
        let mut state = STATE.lock().await;
        let mut_profile = state.get_profile_mut(&npub).unwrap();

        // Store all necessary IDs first
        let profile_id = mut_profile.id.clone();
        let msg_id_clone = msg_id.clone();
        let attachment_id_clone = attachment_id.clone();

        // Update the attachment status
        let mut_msg = mut_profile.get_message_mut(&msg_id).unwrap();
        let mut_attachment = mut_msg.get_attachment_mut(&attachment_id).unwrap();
        mut_attachment.downloading = false;
        mut_attachment.downloaded = true;

        // Emit the finished download
        handle.emit("attachment_download_result", serde_json::json!({
            "profile_id": profile_id,
            "msg_id": msg_id_clone,
            "id": attachment_id_clone,
            "success": true,
        })).unwrap();

        // Save to the DB
        db::save_message(handle.clone(), mut_msg.clone(), npub).await.unwrap();
    }

    true
}

#[derive(serde::Serialize, Clone)]
struct LoginKeyPair {
    public: String,
    private: String,
}

#[tauri::command]
async fn login(import_key: String) -> Result<LoginKeyPair, String> {
    let keys: Keys;

    // If we're already logged in (i.e: Developer Mode with frontend hot-loading), just return the existing keys.
    // TODO: in the future, with event-based state changes, we need to make sure the state syncs correctly too!
    if let Some(client) = NOSTR_CLIENT.get() {
        let signer = client.signer().await.unwrap();
        let new_keys = Keys::parse(&import_key).unwrap();

        /* Derive our Public Key from the Import and Existing key sets */
        let prev_npub = signer.get_public_key().await.unwrap().to_bech32().unwrap();
        let new_npub = new_keys.public_key.to_bech32().unwrap();
        if prev_npub == new_npub {
            // Simply return the same KeyPair and allow the frontend to continue login as usual
            return Ok(LoginKeyPair {
                public: signer.get_public_key().await.unwrap().to_bech32().unwrap(),
                private: new_keys.secret_key().to_bech32().unwrap(),
            });
        } else {
            // This shouldn't happen in the real-world, but just in case...
            return Err(String::from("An existing Nostr Client instance exists, but a second incompatible key import was requested."));
        }
    }

    // If it's an nsec, import that
    if import_key.starts_with("nsec") {
        match Keys::parse(&import_key) {
            Ok(parsed) => keys = parsed,
            Err(_) => return Err(String::from("Invalid nsec")),
        };
    } else {
        // Otherwise, we'll try importing it as a mnemonic seed phrase (BIP-39)
        match Keys::from_mnemonic(import_key, Some(String::new())) {
            Ok(parsed) => keys = parsed,
            Err(_) => return Err(String::from("Invalid Seed Phrase")),
        };
    }

    // Initialise the Nostr client
    let client = Client::builder()
        .signer(keys.clone())
        .opts(Options::new().gossip(false))
        .build();
    NOSTR_CLIENT.set(client).unwrap();

    // Add our profile (at least, the npub of it) to our state
    let npub = keys.public_key.to_bech32().unwrap();
    let mut profile = Profile::new();
    profile.id = npub.clone();
    profile.mine = true;
    STATE.lock().await.profiles.push(profile);

    // Return our npub to the frontend client
    Ok(LoginKeyPair {
        public: npub,
        private: keys.secret_key().to_bech32().unwrap(),
    })
}

/// Returns `true` if the client has connected, `false` if it was already connected
#[tauri::command]
async fn connect() -> bool {
    let client = NOSTR_CLIENT.get().expect("Nostr client not initialized");

    // If we're already connected to some relays - skip and tell the frontend our client is already online
    if client.relays().await.len() > 0 {
        return false;
    }

    // Add our 'Trusted Relay' (see Rustdoc for TRUSTED_RELAY for more info)
    client.add_relay(TRUSTED_RELAY).await.unwrap();

    // Add a couple common relays, especially with explicit NIP-17 support (thanks 0xchat!)
    client.add_relay("wss://relay.0xchat.com").await.unwrap();
    client.add_relay("wss://auth.nostr1.com").await.unwrap();
    client.add_relay("wss://relay.damus.io").await.unwrap();

    // Connect!
    client.connect().await;
    true
}

// Convert a byte slice to a hex string
fn bytes_to_hex_string(bytes: &[u8]) -> String {
    // Pre-allocate the exact size needed (2 hex chars per byte)
    let mut result = String::with_capacity(bytes.len() * 2);
    
    // Use a lookup table for hex conversion
    const HEX_CHARS: &[u8; 16] = b"0123456789abcdef";
    
    for &b in bytes {
        // Extract high and low nibbles
        let high = b >> 4;
        let low = b & 0xF;
        result.push(HEX_CHARS[high as usize] as char);
        result.push(HEX_CHARS[low as usize] as char);
    }
    
    result
}

// Convert hex string back to bytes for decryption
fn hex_string_to_bytes(s: &str) -> Vec<u8> {
    // Pre-allocate the result vector to avoid resize operations
    let mut result = Vec::with_capacity(s.len() / 2);
    let bytes = s.as_bytes();
    
    // Process bytes directly to avoid UTF-8 decoding overhead
    let mut i = 0;
    while i + 1 < bytes.len() {
        // Convert two hex characters to a single byte
        let high = match bytes[i] {
            b'0'..=b'9' => bytes[i] - b'0',
            b'a'..=b'f' => bytes[i] - b'a' + 10,
            b'A'..=b'F' => bytes[i] - b'A' + 10,
            _ => 0,
        };
        
        let low = match bytes[i + 1] {
            b'0'..=b'9' => bytes[i + 1] - b'0',
            b'a'..=b'f' => bytes[i + 1] - b'a' + 10,
            b'A'..=b'F' => bytes[i + 1] - b'A' + 10,
            _ => 0,
        };
        
        result.push((high << 4) | low);
        i += 2;
    }
    
    result
}

async fn hash_pass(password: String) -> [u8; 32] {
    // 150000 KiB memory size
    let memory = 150000;
    // 10 iterations
    let iterations = 10;
    let params = Params::new(memory, iterations, 1, Some(32)).unwrap();

    // TODO: create a random on-disk salt at first init
    // However, with the nature of this being local software, it won't help a user whom has their system compromised in the first place
    let salt = "vectorvectovectvecvev".as_bytes();

    // Prepare derivation
    let argon = Argon2::new(argon2::Algorithm::Argon2id, Version::V0x13, params);
    let mut key: [u8; 32] = [0; 32];
    argon
        .hash_password_into(password.as_bytes(), salt, &mut key)
        .unwrap();

    key
}

// Internal function for encryption logic
pub async fn internal_encrypt(input: String, password: Option<String>) -> String {
    // Hash our password with Argon2 and use it as the key
    let key = if password.is_none() { 
        ENCRYPTION_KEY.get().unwrap() 
    } else { 
        &hash_pass(password.unwrap()).await 
    };

    // Generate a random 12-byte nonce
    let mut rng = rand::thread_rng();
    let nonce_bytes: [u8; 12] = rng.gen();
    
    // Create the cipher instance
    let cipher = ChaCha20Poly1305::new_from_slice(key)
        .expect("Key should be valid");
    
    // Create the nonce
    let nonce = Nonce::from_slice(&nonce_bytes);
    
    // Encrypt the input
    let ciphertext = cipher
        .encrypt(nonce, input.as_bytes())
        .expect("Encryption should not fail");
    
    // Prepend the nonce to our ciphertext
    let mut buffer = Vec::with_capacity(nonce_bytes.len() + ciphertext.len());
    buffer.extend_from_slice(&nonce_bytes);
    buffer.extend_from_slice(&ciphertext);

    // Save the Encryption Key locally so that we can continually encrypt data post-login
    if ENCRYPTION_KEY.get().is_none() {
        ENCRYPTION_KEY.set(*key).unwrap();
    }

    // Convert the encrypted bytes to a hex string for safe storage/transmission
    bytes_to_hex_string(&buffer)
}

// Internal function for decryption logic
pub async fn internal_decrypt(ciphertext: String, password: Option<String>) -> Result<String, ()> {
    // Check if we're using a password before we potentially move it
    let has_password = password.is_some();

    // Fast path: If we already have an encryption key and no password is provided, avoid unnecessary work
    let key = if let Some(pass) = password {
        // Only hash the password if we actually have one
        &hash_pass(pass).await
    } else if let Some(cached_key) = ENCRYPTION_KEY.get() {
        // Use cached key
        cached_key
    } else {
        // No key available
        return Err(());
    };

    // Convert hex to bytes - use reference to avoid copying the string
    let encrypted_data = match hex_string_to_bytes(ciphertext.as_str()) {
        bytes if bytes.len() >= 12 => bytes,
        _ => return Err(())
    };
    
    // Extract nonce and encrypted data - use slices to avoid copying data
    let (nonce_bytes, actual_ciphertext) = encrypted_data.split_at(12);
    
    // Create the cipher instance
    let cipher = match ChaCha20Poly1305::new_from_slice(key) {
        Ok(c) => c,
        Err(_) => return Err(())
    };
    
    // Create the nonce and decrypt
    let plaintext = match cipher.decrypt(Nonce::from_slice(nonce_bytes), actual_ciphertext) {
        Ok(pt) => pt,
        Err(_) => return Err(())
    };

    // Cache the key if needed - only set if we came from password path
    if has_password && ENCRYPTION_KEY.get().is_none() {
        // This only happens once after login with password
        let _ = ENCRYPTION_KEY.set(*key); // Ignore result as this is non-critical
    }

    // Convert decrypted bytes to string using unsafe version, because SPEED!
    // SAFETY: The plaintext bytes are guaranteed to be valid UTF-8, making this safe, because:
    // 1. They were originally created from a valid UTF-8 string (typically JSON or plaintext)
    // 2. ChaCha20-Poly1305 authenticated decryption ensures the data is intact
    // 3. The decryption process preserves the exact byte patterns
    unsafe {
        Ok(String::from_utf8_unchecked(plaintext))
    }
}

// Tauri command that uses the internal function
#[tauri::command]
async fn encrypt(input: String, password: Option<String>) -> String {
    let res = internal_encrypt(input, password).await;

    // If we have one; save the in-memory seedphrase in an encrypted at-rest format
    match MNEMONIC_SEED.get() {
        Some(seed) => {
            // Save the seed phrase to the database
            let handle = TAURI_APP.get().unwrap();
            let _ = db::set_seed(handle.clone(), seed.to_string()).await;
        }
        _ => ()
    }

    res
}

// Tauri command that uses the internal function
#[tauri::command]
async fn decrypt(ciphertext: String, password: Option<String>) -> Result<String, ()> {
    internal_decrypt(ciphertext, password).await
}

#[tauri::command]
async fn start_recording() -> Result<(), String> {
    AudioRecorder::global().start()
}

#[tauri::command]
async fn stop_recording() -> Result<Vec<u8>, String> {
    AudioRecorder::global().stop()
}

#[derive(serde::Serialize, serde::Deserialize, Clone, PartialEq, Debug)]
pub struct SiteMetadata {
    domain: String,
    og_title: Option<String>,
    og_description: Option<String>,
    og_image: Option<String>,
    og_url: Option<String>,
    og_type: Option<String>,
    title: Option<String>,
    description: Option<String>,
    favicon: Option<String>,  // New field
}

pub async fn fetch_site_metadata(url: &str) -> Result<SiteMetadata, String> {
    // Extract and normalize domain
    let domain = {
        let parts: Vec<&str> = url.split('/').collect();
        if parts.len() >= 3 {
            let mut domain = format!("{}://{}", parts[0].trim_end_matches(':'), parts[2]);
            if !domain.ends_with('/') {
                domain.push('/');
            }
            domain
        } else {
            let mut domain = url.to_string();
            if !domain.ends_with('/') {
                domain.push('/');
            }
            domain
        }
    };
    
    let mut html_chunk = Vec::new();
    
    let client = reqwest::Client::new();
    let mut response = client
        .get(url)
        .header("Range", "bytes=0-32768")
        .send()
        .await
        .map_err(|e| e.to_string())?;
    
    // Read the response in chunks
    loop {
        let chunk = response.chunk().await.map_err(|e| e.to_string())?;
        match chunk {
            Some(data) => {
                html_chunk.extend_from_slice(&data);
                
                if let Ok(current_html) = String::from_utf8(html_chunk.clone()) {
                    if let Some(head_end) = current_html.find("</head>") {
                        html_chunk.truncate(head_end + 7);
                        break;
                    }
                }
            }
            None => break,
        }
    }
    
    let html_string = String::from_utf8(html_chunk).map_err(|e| e.to_string())?;
    let document = Html::parse_document(&html_string);
    let meta_selector = Selector::parse("meta").unwrap();
    let link_selector = Selector::parse("link").unwrap();
    
    let mut metadata = SiteMetadata {
        domain: domain.clone(),
        og_title: None,
        og_description: None,
        og_image: None,
        og_url: Some(String::from(url)),
        og_type: None,
        title: None,
        description: None,
        favicon: None,
    };
    
    // Process favicon links
    let mut favicon_candidates = Vec::new();
    for link in document.select(&link_selector) {
        if let Some(rel) = link.value().attr("rel") {
            if let Some(href) = link.value().attr("href") {
                match rel.to_lowercase().as_str() {
                    "icon" | "shortcut icon" | "apple-touch-icon" => {
                        // Normalize the favicon URL
                        let favicon_url = if href.starts_with("https://") {
                            href.to_string()
                        } else if href.starts_with("//") {
                            format!("https:{}", href)
                        } else if href.starts_with('/') {
                            format!("{}{}", domain.trim_end_matches('/'), href)
                        } else {
                            format!("{}/{}", domain.trim_end_matches('/'), href)
                        };
                        
                        favicon_candidates.push((favicon_url, rel.to_lowercase()));
                    }
                    _ => {}
                }
            }
        }
    }

    // Set favicon with priority order
    if favicon_candidates.is_empty() {
        // If no favicon found in links, try the default /favicon.ico location
        metadata.favicon = Some(format!("{}/favicon.ico", domain.trim_end_matches('/')));
    } else {
        // Priority order:
        // 1. apple-touch-icon (highest quality)
        // 2. icon with .png extension
        // 3. shortcut icon with .png extension
        // 4. any other icon
        // 5. fallback to /favicon.ico
        
        let favicon = favicon_candidates.iter()
            .find(|(_url, rel)| 
                rel == "apple-touch-icon")
            .or_else(|| 
                favicon_candidates.iter()
                    .find(|(url, _)| 
                        url.ends_with(".png")))
            .or_else(|| 
                favicon_candidates.iter()
                    .find(|(_, rel)| 
                        rel == "icon" || rel == "shortcut icon"))
            .map(|(url, _)| url.clone())
            .or_else(|| 
                // Fallback to /favicon.ico
                Some(format!("{}/favicon.ico", domain.trim_end_matches('/')))
            );
        
        metadata.favicon = favicon;
    }
    
    // Process meta tags (existing code)
    for meta in document.select(&meta_selector) {
        let element = meta.value();
        
        if let Some(property) = element.attr("property") {
            if let Some(content) = element.attr("content") {
                match property {
                    "og:title" => metadata.og_title = Some(content.to_string()),
                    "og:description" => metadata.og_description = Some(content.to_string()),
                    "og:image" => {
                        let image_url = if content.starts_with("https://") {
                            content.to_string()
                        } else if content.starts_with("//") {
                            format!("https:{}", content)
                        } else if content.starts_with('/') {
                            format!("{}{}", domain.trim_end_matches('/'), content)
                        } else {
                            format!("{}{}", domain.trim_end_matches('/'), content)
                        };
                        metadata.og_image = Some(image_url);
                    },
                    "og:url" => metadata.og_url = Some(content.to_string()),
                    "og:type" => metadata.og_type = Some(content.to_string()),
                    _ => {}
                }
            }
        }
        
        if let Some(name) = element.attr("name") {
            if let Some(content) = element.attr("content") {
                match name {
                    "description" => metadata.description = Some(content.to_string()),
                    _ => {}
                }
            }
        }
    }
    
    // Extract title from title tag
    if let Some(title_element) = document.select(&Selector::parse("title").unwrap()).next() {
        metadata.title = Some(title_element.text().collect::<String>());
    }
    
    Ok(metadata)
}

#[tauri::command]
async fn fetch_msg_metadata(npub: String, msg_id: String) -> bool {
    // Find the message we're extracting metadata from
    let text = {
        let mut state = STATE.lock().await;
        let chat = state.get_profile_mut(&npub).unwrap();
        let message = chat.get_message_mut(&msg_id).unwrap();
        message.content.clone()
    };

    // Extract URLs from the message
    const MAX_URLS_TO_TRY: usize = 3;
    let urls = extract_https_urls(text.as_str());
    if urls.is_empty() {
        return false;
    }

    // Only try the first few URLs
    for url in urls.into_iter().take(MAX_URLS_TO_TRY) {
        match fetch_site_metadata(&url).await {
            Ok(metadata) => {
                let has_content = metadata.og_title.is_some() 
                    || metadata.og_description.is_some()
                    || metadata.og_image.is_some()
                    || metadata.title.is_some()
                    || metadata.description.is_some();

                // Extracted metadata!
                if has_content {
                    // Re-fetch the message and add our metadata
                    let mut state = STATE.lock().await;
                    let chat = state.get_profile_mut(&npub).unwrap();
                    let msg = chat.get_message_mut(&msg_id).unwrap();
                    msg.preview_metadata = Some(metadata);

                    // Update the renderer
                    let handle = TAURI_APP.get().unwrap();
                    handle.emit("message_update", serde_json::json!({
                        "old_id": &msg_id,
                        "message": &msg,
                        "chat_id": &npub
                    })).unwrap();

                    // Save the new Metadata to the DB
                    db::save_message(handle.clone(), msg.clone(), npub).await.unwrap();
                    return true;
                }
            }
            Err(_) => continue,
        }
    }
    false
}

#[tauri::command]
async fn logout<R: Runtime>(handle: AppHandle<R>) {
    // Lock the state to ensure nothing is added to the DB before restart
    let _guard = STATE.lock().await;

    // Erase the Database completely for a clean logout
    db::nuke(handle.clone()).unwrap();

    // Restart the Core process
    handle.restart();
}

/// Creates a new Nostr keypair derived from a BIP39 Seed Phrase
#[tauri::command]
async fn create_account() -> Result<LoginKeyPair, String> {
    // Generate a BIP39 Mnemonic Seed Phrase
    let mnemonic = bip39::Mnemonic::generate(12).map_err(|e| e.to_string())?;
    let mnemonic_string = mnemonic.to_string();

    // Derive our nsec from our Mnemonic
    let keys = Keys::from_mnemonic(mnemonic_string.clone(), None).map_err(|e| e.to_string())?;

    // Initialise the Nostr client
    let client = Client::builder()
        .signer(keys.clone())
        .opts(Options::new().gossip(false))
        .build();
    NOSTR_CLIENT.set(client).unwrap();

    // Add our profile (at least, the npub of it) to our state
    let npub = keys.public_key.to_bech32().map_err(|e| e.to_string())?;
    let mut profile = Profile::new();
    profile.id = npub.clone();
    profile.mine = true;
    STATE.lock().await.profiles.push(profile);

    // Save the seed in memory, ready for post-pin-setup encryption
    let _ = MNEMONIC_SEED.set(mnemonic_string);

    // Return the keypair in the same format as the login function
    Ok(LoginKeyPair {
        public: npub,
        private: keys.secret_key().to_bech32().map_err(|e| e.to_string())?,
    })
}

/// Marks a specific message as read
#[tauri::command]
async fn mark_as_read(npub: String) -> bool {
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
            update_unread_counter(handle.clone()).await;
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
        update_unread_counter(handle.clone()).await;

        // Save the "Last Read" marker to the DB
        db::set_profile(handle.clone(), STATE.lock().await.get_profile(&npub).unwrap().clone()).await.unwrap();
    }
    
    result
}

/// Updates the OS taskbar badge with the count of unread messages
#[tauri::command]
async fn update_unread_counter<R: Runtime>(handle: AppHandle<R>) -> u32 {
    // Get the count of unread messages from the state
    let unread_count = {
        let state = STATE.lock().await;
        state.count_unread_messages()
    };
    
    // Get the main window
    if let Some(window) = handle.get_webview_window("main") {
        if unread_count > 0 {
            // Platform-specific badge/overlay handling
            #[cfg(target_os = "windows")]
            {
                // On Windows, use overlay icon instead of badge
                let icon = tauri::include_image!("./icons/icon_badge_notification.png");
                let _ = window.set_overlay_icon(Some(icon));
            }
            
            #[cfg(not(any(target_os = "windows", target_os = "ios", target_os = "android")))]
            {
                // On macOS, Linux, etc. use the badge if available
                let _ = window.set_badge_count(Some(unread_count as i64));
            }
        } else {
            // Clear badge/overlay when no unread messages
            #[cfg(target_os = "windows")]
            {
                // Remove the overlay icon on Windows
                let _ = window.set_overlay_icon(None);
            }
            
            #[cfg(not(any(target_os = "windows", target_os = "ios", target_os = "android")))]
            {
                // Clear the badge on other platforms
                let _ = window.set_badge_count(None);
            }
        }
    }
    
    unread_count
}

#[tauri::command]
async fn transcribe<R: Runtime>(handle: AppHandle<R>, file_path: String, model_name: String) -> Result<String, String> {
    // Convert the file path to a Path
    let path = std::path::Path::new(&file_path);
    
    // Check if the file exists
    if !path.exists() {
        return Err(format!("File does not exist: {}", file_path));
    }
    
    // Read the wav file and resample
    match whisper::resample_audio(path, 16000) {
        Ok(audio_data) => {
            // Pass the resampled audio to the whisper transcribe function
            match whisper::transcribe(&handle, &model_name, audio_data).await {
                Ok(text) => Ok(text),
                Err(e) => Err(format!("Transcription error: {}", e.to_string()))
            }
        },
        Err(e) => Err(format!("Audio processing error: {}", e.to_string()))
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    #[cfg(target_os = "linux")]
    {
        // WebKitGTK can be quite funky cross-platform: as a result, we'll fallback to a more compatible renderer
        // In theory, this will make Vector run more consistently across a wider range of Linux Desktop distros.
        std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_store::Builder::new().build())
        .setup(|app| {
            let handle = app.app_handle().clone();

            // Setup a graceful shutdown for our Nostr subscriptions
            let window = app.get_webview_window("main").unwrap();
            window.on_window_event(move |event| {
                match event {
                    // This catches when the window is being closed
                    tauri::WindowEvent::CloseRequested { .. } => {
                        // Cleanly shutdown our Nostr client
                        if let Some(nostr_client) = NOSTR_CLIENT.get() {
                            tauri::async_runtime::block_on(async {
                                // Shutdown the Nostr client
                                nostr_client.shutdown().await;
                            });
                        }
                    }
                    _ => {}
                }
            });

            // Set as our accessible static app handle
            TAURI_APP.set(handle).unwrap();
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            db::get_db,
            db::get_db_version,
            db::set_db_version,
            db::get_theme,
            db::set_theme,
            db::get_pkey,
            db::set_pkey,
            db::get_seed,
            db::set_seed,
            db::remove_setting,
            fetch_messages,
            message,
            paste_message,
            voice_message,
            file_message,
            warmup_nip96_servers,
            react,
            download_attachment,
            login,
            notifs,
            load_profile,
            update_profile,
            update_status,
            upload_avatar,
            start_typing,
            connect,
            encrypt,
            decrypt,
            start_recording,
            stop_recording,
            fetch_msg_metadata,
            mark_as_read,
            update_unread_counter,
            logout,
            create_account,
            transcribe,
            whisper::list_models
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
