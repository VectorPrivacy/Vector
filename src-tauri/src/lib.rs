use std::borrow::Cow;
use argon2::{Argon2, Params, Version};
use lazy_static::lazy_static;
use nostr_sdk::prelude::*;
use once_cell::sync::OnceCell;
use rand::Rng;
use tokio::sync::Mutex;
use aes::Aes256;
use aes_gcm::AesGcm;
use chacha20poly1305::{
    aead::{Aead, AeadInPlace, KeyInit},
    ChaCha20Poly1305, Nonce
};
use generic_array::{GenericArray, typenum::U16};
use ::image::{ImageEncoder, codecs::png::PngEncoder, ExtendedColorType::Rgba8};
use tauri::{AppHandle, Emitter, Manager, Runtime};
use tauri_plugin_notification::NotificationExt;
use tauri_plugin_fs::FsExt;
use scraper::{Html, Selector};

mod db;
use db::SlimProfile;

mod voice;
use voice::AudioRecorder;

mod util;
use util::extract_https_urls;

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

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq)]
pub struct Attachment {
    /// The encryption Nonce as the unique file ID
    id: String,
    /// The file extension
    extension: String,
    /// The storage directory path (typically the ~/Downloads folder)
    path: String,
    /// Whether the file has been downloaded or not
    downloaded: bool,
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
pub struct Profile {
    id: String,
    name: String,
    avatar: String,
    messages: Vec<Message>,
    status: Status,
    last_updated: u64,
    typing_until: u64,
    mine: bool,
}

impl Profile {
    fn new() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            avatar: String::new(),
            messages: Vec::new(),
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

    /// Get a mutable message by ID
    fn get_message_mut(&mut self, id: &str) -> Option<&mut Message> {
        self.messages.iter_mut().find(|msg| msg.id == id)
    }

    /// Merge Nostr Metadata with this Vector Profile
    fn from_metadata(&mut self, meta: Metadata) {
        self.name = meta.name.unwrap_or(self.name.clone());
        self.avatar = meta.picture.unwrap_or(self.avatar.clone());
    }

    /// Add a Message to this Vector Profile
    /// 
    /// This method internally checks for and avoids duplicate messages.
    fn internal_add_message(&mut self, message: Message) -> bool {
        // Make sure we don't add the same message twice
        if !self.messages.iter().any(|m| m.id == message.id) {
            // If it's their message; disable their typing indicator until further indicators are sent
            if !message.mine {
                self.typing_until = 0;
            }
            self.messages.push(message);
            // TODO: use appending/prepending and splicing, rather than sorting each message!
            // This is very expensive, but will do for now as a stop-gap.
            self.messages.sort_by(|a, b| a.at.cmp(&b.at));
            true
        } else {
            // Message is already known by the state
            false
        }
    }

    /// Add a Reaction to a Message
    fn internal_add_reaction(&mut self, msg_id: &str, reaction: Reaction) -> bool {
        // Find the message being reacted to
        match self.get_message_mut(msg_id) {
            Some(msg) => {
                // Make sure we don't add the same reaction twice
                if !msg.reactions.iter().any(|r| r.id == reaction.id) {
                    msg.reactions.push(reaction);

                    // Update the frontend
                    let handle = TAURI_APP.get().unwrap();
                    handle.emit("message_update", serde_json::json!({
                        "old_id": &msg.id,
                        "message": &msg,
                        "chat_id": &self.id
                    })).unwrap();
                    true
                } else {
                    // Reaction was already added previously
                    false
                }
            }
            None => false,
        }
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

#[derive(serde::Serialize, Clone, Debug)]
struct ChatState {
    profiles: Vec<Profile>,
    days_to_sync: u8
}

impl ChatState {
    fn new() -> Self {
        Self {
            profiles: Vec::new(),
            days_to_sync: 2
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

    /// Add a Reaction to a Message in a Vector Profile
    fn add_reaction(&mut self, npub: &str, msg_id: &str, reaction: Reaction) -> bool {
        // Get the profile
        match self.get_profile_mut(npub) {
            // If the profile is found; add the reaction to the profile's message
            Some(profile) => profile.internal_add_reaction(msg_id, reaction),
            None => false,
        }
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
    let days_to_search = STATE.lock().await.days_to_sync as u64;
    let (since_timestamp, until_timestamp) = if init {
        // Load our DB (if we haven't already; i.e: our profile is the single loaded profile since login)
        let mut state = STATE.lock().await;
        if state.profiles.len() == 1 {
            let profiles = db::get_all_profiles(&handle).await.unwrap();
            let msgs = db::get_all_messages(&handle).await.unwrap();

            // Load our Profile Cache in to the state
            state.merge_db_profiles(profiles).await;

            // Add each message to the state, keeping the earliest known message
            for (msg, npub) in msgs {
                state.add_message(&npub, msg);
            }
        }

        // Send the state to our frontend to signal finalised init with a full state
        handle.emit("init_finished", &state.profiles).unwrap();

        // Now fetch messages from the given period, to fill any "gaps" since the app was last opened
        (
            Timestamp::from_secs(Timestamp::now().as_u64() - (60 * 60 * 24 * days_to_search)),
            Timestamp::now()
        )
    } else {
        // Find the oldest message timestamp from our state
        match get_oldest_message_timestamp().await {
            Some(oldest_ts) => {
                // Fetch the period before our oldest message
                let since = Timestamp::from_secs(oldest_ts - (60 * 60 * 24 * days_to_search));
                let until = Timestamp::from_secs(oldest_ts);
                (since, until)
            },
            None => {
                // No messages in DB yet, do an initial fetch
                (
                    Timestamp::from_secs(Timestamp::now().as_u64() - (60 * 60 * 24 * days_to_search)),
                    Timestamp::now()
                )
            }
        }
    };

    // Emit our current "Sync Range" to the frontend
    handle.emit("sync_progress", serde_json::json!({
        "since": since_timestamp.as_u64(),
        "until": until_timestamp.as_u64()
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

    // If no messages were retrieved; we bump our search radius until a maximum of 10 days
    let max_search_range_reached = days_to_search >= 10;
    if new_messages_count == 0 && !max_search_range_reached {
        STATE.lock().await.days_to_sync += 2;
    } else {
        STATE.lock().await.days_to_sync = 2;
    }

    // Once we've searched a 10-day slice without new messages; we give up and finish sync
    if max_search_range_reached {
        handle.emit("sync_finished", serde_json::json!({
            "since": since_timestamp.as_u64(),
            "until": until_timestamp.as_u64()
        })).unwrap();
    } else {
        // Keep synchronising
        handle.emit("sync_slice_finished", ()).unwrap();
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
    let pending_id = String::from("pending-") + &pending_count.to_string();
    let msg = Message {
        id: pending_id.clone(),
        content: content.clone(),
        replied_to: replied_to.clone(),
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

    // Send the pending message to our frontend
    let handle = TAURI_APP.get().unwrap();
    handle.emit("message_new", serde_json::json!({
        "message": &msg,
        "chat_id": &receiver
    })).unwrap();

    // Grab our pubkey
    let client = NOSTR_CLIENT.get().expect("Nostr client not initialized");
    let signer = client.signer().await.unwrap();
    let my_public_key = signer.get_public_key().await.unwrap();

    // Convert the Bech32 String in to a PublicKey
    let receiver_pubkey = PublicKey::from_bech32(receiver.clone().as_str()).unwrap();

    // Prepare the NIP-17 rumor
    let mut rumor = if file.is_none() {
        // Text Message
        EventBuilder::private_msg_rumor(receiver_pubkey, content.clone())
    } else {
        let attached_file = file.unwrap();

        // Encrypt the attachment
        let params = generate_encryption_params();
        let enc_file = encrypt_data(attached_file.bytes.as_slice(), &params).unwrap();

        // Update the attachment in-state
        {
            // Retrieve the Pending Message
            let mut state = STATE.lock().await;
            let chat = state.get_profile_mut(&receiver).unwrap();
            let message = chat.get_message_mut(&pending_id).unwrap();

            // Store the nonce-based file name on-disk for future reference
            let dir = handle.path().resolve("vector", tauri::path::BaseDirectory::Download).unwrap();
            let nonce_file_path = dir.join(format!("{}.{}", params.nonce.clone(), attached_file.extension.clone()));

            // Create the vector directory if it doesn't exist
            std::fs::create_dir_all(&dir).unwrap();

            // Save the nonce-named file
            std::fs::write(&nonce_file_path, &attached_file.bytes).unwrap();

            // Add the Attachment in-state (with our local path, to prevent re-downloading it accidentally from server)
            message.attachments.push(Attachment {
                id: params.nonce.clone(),
                extension: attached_file.extension.clone(),
                path: nonce_file_path.to_string_lossy().to_string(),
                downloaded: true
            });

            // Update the frontend
            handle.emit("message_update", serde_json::json!({
                "old_id": &pending_id,
                "message": &message,
                "chat_id": &receiver
            })).unwrap();
        }

        // Format a Mime Type from the file extension
        let mime_type = match attached_file.extension.to_lowercase().as_str() {
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
        match upload_data(&signer, &conf, enc_file, Some(mime_type), None).await {
            Ok(url) => {
                // Create the attachment rumor
                let attachment_rumor = EventBuilder::new(Kind::from_u16(15), url.to_string());

                // Append decryption keys and file metadata
                attachment_rumor
                    .tag(Tag::public_key(receiver_pubkey))
                    .tag(Tag::custom(TagKind::custom("file-type"), [mime_type]))
                    .tag(Tag::custom(TagKind::custom("encryption-algorithm"), ["aes-gcm"]))
                    .tag(Tag::custom(TagKind::custom("decryption-key"), [params.key.as_str()]))
                    .tag(Tag::custom(TagKind::custom("decryption-nonce"), [params.nonce.as_str()]))
            },
            Err(_) => {
                // The file upload failed: so we mark the message as failed and notify of an error
                let mut state = STATE.lock().await;
                let chat = state.get_profile_mut(&receiver).unwrap();
                let failed_msg = chat.get_message_mut(&pending_id).unwrap();
                failed_msg.failed = true;

                // Update the frontend
                handle.emit("message_update", serde_json::json!({
                    "old_id": &pending_id,
                    "message": &failed_msg,
                    "chat_id": &receiver
                })).unwrap();

                // Return the error
                return Err(String::from("Failed to upload file"));
            }
        }
    };

    // If a reply reference is included, add the tag
    if !replied_to.is_empty() {
        rumor = rumor.tag(Tag::custom(
            TagKind::e(),
            [replied_to, String::from(""), String::from("reply")],
        ));
    }

    // Build the rumor with our key (unsigned)
    let built_rumor = rumor.build(my_public_key);

    // Send message to the real receiver
    match client
        .gift_wrap(&receiver_pubkey, built_rumor.clone(), [])
        .await
    {
        Ok(_) => {
            // Send message to our own public key, to allow for message recovering
            match client
                .gift_wrap(&my_public_key, built_rumor.clone(), [])
                .await
            {
                Ok(_) => {
                    // Mark the message as a success
                    let mut state = STATE.lock().await;
                    let chat = state.get_profile_mut(&receiver).unwrap();
                    let sent_msg = chat.get_message_mut(&pending_id).unwrap();
                    sent_msg.id = built_rumor.id.unwrap().to_hex();
                    sent_msg.pending = false;

                    // Save the message to our DB
                    let handle = TAURI_APP.get().unwrap();
                    db::save_message(handle.clone(), sent_msg.clone(), receiver.clone()).await.unwrap();

                    // Update the frontend
                    handle.emit("message_update", serde_json::json!({
                        "old_id": &pending_id,
                        "message": &sent_msg,
                        "chat_id": &receiver
                    })).unwrap();
                    return Ok(true);
                }
                Err(_) => {
                    // This is an odd case; the message was sent to the receiver, but NOT ourselves
                    // We'll class it as sent, for now...
                    let mut state = STATE.lock().await;
                    let chat = state.get_profile_mut(&receiver).unwrap();
                    let sent_ish_msg = chat.get_message_mut(&pending_id).unwrap();
                    sent_ish_msg.id = built_rumor.id.unwrap().to_hex();
                    sent_ish_msg.pending = false;

                    // Save the message to our DB
                    let handle = TAURI_APP.get().unwrap();
                    db::save_message(handle.clone(), sent_ish_msg.clone(), receiver.clone()).await.unwrap();

                    // Update the frontend
                    handle.emit("message_update", serde_json::json!({
                        "old_id": &pending_id,
                        "message": &sent_ish_msg,
                        "chat_id": &receiver
                    })).unwrap();
                    return Ok(true);
                }
            }
        }
        Err(_) => {
            // Mark the message as a failure, bad message, bad!
            let mut state = STATE.lock().await;
            let chat = state.get_profile_mut(&receiver).unwrap();
            let failed_msg = chat.get_message_mut(&pending_id).unwrap();
            failed_msg.failed = true;
            return Ok(false);
        }
    }
}

#[tauri::command]
async fn paste_message(receiver: String, replied_to: String, pixels: Vec<u8>, width: u32, height: u32) -> Result<bool, String> {
    // Create the encoder directly with a Vec<u8>
    let mut png_data = Vec::new();
    let encoder = PngEncoder::new(&mut png_data);

    // Encode directly from pixels to PNG bytes
    encoder.write_image(
        &pixels,            // raw pixels
        width,              // width
        height,             // height
        Rgba8               // color type
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
        emoji.clone(),
    )
    .build(my_public_key);

    // Send reaction to the real receiver
    client
        .gift_wrap(&receiver_pubkey, rumor.clone(), [])
        .await
        .unwrap();

    // Send reaction to our own public key, to allow for recovering
    match client.gift_wrap(&my_public_key, rumor, []).await {
        Ok(response) => {
            // And add our reaction locally
            let reaction = Reaction {
                id: response.id().to_hex(),
                reference_id: reference_id.clone(),
                author_id: my_public_key.to_hex(),
                emoji,
            };
            return Ok(STATE
                .lock()
                .await
                .add_reaction(&npub, &reference_id, reaction));
        }
        Err(e) => {
            eprintln!("Error: {:?}", e);
            return Ok(false);
        }
    }
}

#[tauri::command]
async fn load_profile(npub: String) -> Result<bool, ()> {
    let client = NOSTR_CLIENT.get().expect("Nostr client not initialized");

    // Convert the Bech32 String in to a PublicKey
    let profile_pubkey = PublicKey::from_bech32(npub.as_str()).unwrap();

    // Grab our pubkey to check for profiles belonging to us
    let signer = client.signer().await.unwrap();
    let my_public_key = signer.get_public_key().await.unwrap();

    // Fetch an immutable profile from the cache (or, quickly generate a new one to pass to the fetching logic)
    // Mutex Scope: we want to hold this lock as short as possible, given this function is "spammed" for very fast profile cache hit checks
    let profile: Profile;
    {
        let mut state = STATE.lock().await;
        profile = match state.get_profile(&npub) {
            Some(p) => p,
            None => {
                // Create a new profile
                let mut new_profile = Profile::new();
                new_profile.id = npub.clone();
                state.profiles.push(new_profile);
                state.get_profile(&npub).unwrap()
            }
        }
        .clone();

        // If the profile has been refreshed in the last 30s, return it's cached version
        if profile.last_updated + 30
            > std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs()
        {
            return Ok(true);
        }
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
                profile.status.clone()
            }
        }
        Err(_e) => profile.status.clone(),
    };

    // Attempt to fetch their Metadata profile
    match client
        .fetch_metadata(profile_pubkey, std::time::Duration::from_secs(15))
        .await
    {
        Ok(meta) => {
            // If it's ours, mark it as such
            let mut state = STATE.lock().await;
            let profile_mutable = state.get_profile_mut(&npub).unwrap();
            let old_profile = profile_mutable.clone();
            profile_mutable.mine = my_public_key == profile_pubkey;
            // Update the Status
            profile_mutable.status = status;
            // Update the Metadata
            profile_mutable.from_metadata(meta);
            // If there's any change between our Old and New profile, emit an update
            if *profile_mutable != old_profile {
                let handle = TAURI_APP.get().unwrap();
                handle.emit("profile_update", &profile_mutable).unwrap();

                // Cache this profile in our DB, too
                db::set_profile(handle.clone(), profile_mutable.clone()).await.unwrap();
            }
            // And apply the current update time
            profile_mutable.last_updated = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs();
            return Ok(true);
        }
        Err(_) => {
            return Ok(false);
        }
    }
}

#[tauri::command]
async fn update_profile(name: String, avatar: String) -> Result<Profile, ()> {
    let client = NOSTR_CLIENT.get().expect("Nostr client not initialized");

    // Grab our pubkey
    let signer = client.signer().await.unwrap();
    let my_public_key = signer.get_public_key().await.unwrap();

    // Get our profile
    let mut meta: Metadata;
    let mut state = STATE.lock().await;
    let profile = state
        .get_profile(&my_public_key.to_bech32().unwrap())
        .unwrap()
        .clone();

    // We'll apply the changes to the previous profile and carry-on the rest
    meta = Metadata::new().name(if name.is_empty() {
        profile.name.clone()
    } else {
        name
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

    // Broadcast the profile update
    match client.set_metadata(&meta).await {
        Ok(_event) => {
            // Apply our Metadata to our Profile
            let profile_mutable = state
                .get_profile_mut(&my_public_key.to_bech32().unwrap())
                .unwrap();
            profile_mutable.from_metadata(meta);

            // Update the frontend
            let handle = TAURI_APP.get().unwrap();
            handle.emit("profile_update", &profile_mutable).unwrap();
            Ok(profile_mutable.clone())
        }
        Err(_e) => Err(()),
    }
}

#[tauri::command]
async fn update_status(status: String) -> Result<Profile, ()> {
    let client = NOSTR_CLIENT.get().expect("Nostr client not initialized");

    // Grab our pubkey
    let signer = client.signer().await.unwrap();
    let my_public_key = signer.get_public_key().await.unwrap();

    // Build and broadcast the status
    let status_builder = EventBuilder::new(Kind::from_u16(30315), status.as_str())
        .tag(Tag::custom(TagKind::d(), vec!["general"]));
    match client.send_event_builder(status_builder).await {
        Ok(_event) => {
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
            Ok(profile.clone())
        }
        Err(_e) => Err(()),
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
async fn start_typing(receiver: String) -> Result<bool, ()> {
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
            rumor.clone(),
            [Tag::expiration(expiry_time)],
        )
        .await
    {
        Ok(_) => Ok(true),
        Err(_) => Ok(false),
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
                    // Save the message to our DB
                    let handle = TAURI_APP.get().unwrap();
                    db::save_message(handle.clone(), msg.clone(), contact.clone()).await.unwrap();

                    // Send it to the frontend
                    let handle = TAURI_APP.get().unwrap();
                    handle.emit("message_new", serde_json::json!({
                        "message": &msg,
                        "chat_id": &contact
                    })).unwrap();
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
                            emoji: rumor.content.clone(),
                        };

                        // Add the reaction
                        STATE.lock().await.add_reaction(&contact, &reference_id, reaction)
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
                let dir = handle.path().resolve("vector", tauri::path::BaseDirectory::Download).unwrap();
                let file_path = dir.join(format!("{}.{}", decryption_nonce, extension));
                if !file_path.exists() {
                    // No file! Try fetching it
                    let req = reqwest::Client::new();
                    let res = req.get(content_url.clone()).send().await;
                    if res.is_err() {
                        // TEMP: reaaaallly improve this area!
                        println!("Weird file: {}", content_url);
                        return false;
                    }
                    let response = res.unwrap();
                    let file_contents = response.bytes().await.unwrap().to_vec();

                    // Decrypt the file
                    let decryption = decrypt_data(file_contents.as_slice(), decryption_key, decryption_nonce);
                    if decryption.is_err() {
                        println!("Failed to decrypt: {}", content_url);
                        return false;
                    }
                    let decrypted_file = decryption.unwrap();

                    // Create the vector directory if it doesn't exist
                    std::fs::create_dir_all(&dir).unwrap();

                    // Save the file to disk
                    std::fs::write(&file_path, &decrypted_file).unwrap();
                }

                // Create an attachment
                let mut attachments = Vec::new();
                let attachment = Attachment {
                    id: decryption_nonce.to_string(),
                    extension: extension.to_string(),
                    path: file_path.to_string_lossy().to_string(),
                    downloaded: true
                };
                attachments.push(attachment);

                // Create the message
                let msg = Message {
                    id: rumor.id.unwrap().to_hex(),
                    content: String::new(),
                    replied_to: String::new(),
                    preview_metadata: None,
                    at: rumor.created_at.as_u64(),
                    attachments: attachments,
                    reactions: Vec::new(),
                    mine: is_mine,
                    pending: false,
                    failed: false,
                };

                // Add the message to the state
                let was_msg_added_to_state = STATE.lock().await.add_message(&contact, msg.clone());

                // If accepted in-state: commit to the DB and emit to the frontend
                if was_msg_added_to_state {
                    // Save the message to our DB
                    let handle = TAURI_APP.get().unwrap();
                    db::save_message(handle.clone(), msg.clone(), contact.clone()).await.unwrap();

                    // Send it to the frontend
                    let handle = TAURI_APP.get().unwrap();
                    handle.emit("message_new", serde_json::json!({
                        "message": &msg,
                        "chat_id": &contact
                    })).unwrap();
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
        Err(_e) => false,
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

/// Represents encryption parameters
#[derive(Debug)]
pub struct EncryptionParams {
    pub key: String,    // Hex string
    pub nonce: String,  // Hex string
}

/// Generates random encryption parameters (key and nonce)
pub fn generate_encryption_params() -> EncryptionParams {
    let mut rng = rand::thread_rng();
    
    // Generate 32 byte key (for AES-256)
    let key: [u8; 32] = rng.gen();
    // Generate 16 byte nonce (to match 0xChat)
    let nonce: [u8; 16] = rng.gen();
    
    EncryptionParams {
        key: hex::encode(key),
        nonce: hex::encode(nonce),
    }
}

/// Encrypts data using AES-256-GCM with a 16-byte nonce
pub fn encrypt_data(data: &[u8], params: &EncryptionParams) -> Result<Vec<u8>, String> {
    // Decode key and nonce from hex
    let key_bytes = hex::decode(&params.key).unwrap();
    let nonce_bytes = hex::decode(&params.nonce).unwrap();

    // Initialize AES-GCM cipher
    let cipher = AesGcm::<Aes256, U16>::new(
        GenericArray::from_slice(&key_bytes)
    );

    // Prepare nonce
    let nonce = GenericArray::from_slice(&nonce_bytes);

    // Create output buffer
    let mut buffer = data.to_vec();

    // Encrypt in place and get authentication tag
    let tag = cipher
        .encrypt_in_place_detached(nonce, &[], &mut buffer)
        .map_err(|_| String::from("Failed to Encrypt Data"))?;

    // Append the authentication tag to the encrypted data
    buffer.extend_from_slice(tag.as_slice());

    Ok(buffer)
}

pub fn decrypt_data(encrypted_data: &[u8], key_hex: &str, nonce_hex: &str) -> Result<Vec<u8>, String> {
    // Verify minimum size requirements
    if encrypted_data.len() < 16 {
        return Err(String::from("Invalid Input"));
    }

    // Decode key and nonce from hex
    let key_bytes = hex::decode(key_hex).unwrap();
    let nonce_bytes = hex::decode(nonce_hex).unwrap();

    // Split input into ciphertext and authentication tag
    let (ciphertext, tag_bytes) = encrypted_data.split_at(encrypted_data.len() - 16);

    // Initialize AES-GCM cipher
    let cipher = AesGcm::<Aes256, U16>::new(
        GenericArray::from_slice(&key_bytes)
    );

    // Prepare nonce and tag
    let nonce = GenericArray::from_slice(&nonce_bytes);
    let tag = aes_gcm::Tag::from_slice(tag_bytes);

    // Create output buffer
    let mut buffer = ciphertext.to_vec();

    // Perform decryption
    let decryption = cipher
        .decrypt_in_place_detached(nonce, &[], &mut buffer, tag);

    // Check that it went well
    if decryption.is_err() {
        return Err(decryption.unwrap_err().to_string());
    }

    Ok(buffer)
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

// Convert string to bytes, ensuring we're dealing with the raw content
fn string_to_bytes(s: &str) -> Vec<u8> {
    s.as_bytes().to_vec()
}

// Convert bytes to string, but we'll use hex encoding for encrypted data
fn bytes_to_hex_string(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

// Convert hex string back to bytes for decryption
fn hex_string_to_bytes(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
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
    // Hash our password with ramped-up Argon2 and use it as the key
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
    let plaintext = string_to_bytes(&input);
    let ciphertext = cipher
        .encrypt(nonce, plaintext.as_ref())
        .expect("Encryption should not fail");
    
    // Prepend the nonce to our ciphertext
    let mut buffer = nonce_bytes.to_vec();
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
    // Hash our password with ramped-up Argon2 and use it as the key
    let key = if password.is_none() { 
        ENCRYPTION_KEY.get().unwrap() 
    } else { 
        &hash_pass(password.unwrap()).await 
    };

    // Convert hex to bytes
    let encrypted_data = match hex_string_to_bytes(&ciphertext) {
        bytes if bytes.len() >= 12 => bytes,
        _ => return Err(())
    };
    
    // Extract nonce and encrypted data
    let nonce_bytes = &encrypted_data[..12];
    let actual_ciphertext = &encrypted_data[12..];
    
    // Create the cipher instance
    let cipher = match ChaCha20Poly1305::new_from_slice(key) {
        Ok(c) => c,
        Err(_) => return Err(())
    };
    
    // Create the nonce
    let nonce = Nonce::from_slice(nonce_bytes);
    
    // Decrypt
    let plaintext = match cipher.decrypt(nonce, actual_ciphertext) {
        Ok(pt) => pt,
        Err(_) => return Err(())
    };

    // Save the Encryption Key locally so that we can continually decrypt data post-login
    if ENCRYPTION_KEY.get().is_none() {
        ENCRYPTION_KEY.set(*key).unwrap();
    }

    // Convert decrypted bytes back to string
    match String::from_utf8(plaintext) {
        Ok(decrypted) => Ok(decrypted),
        Err(_) => Err(()),
    }
}

// Tauri command that uses the internal function
#[tauri::command]
async fn encrypt(input: String, password: Option<String>) -> String {
    internal_encrypt(input, password).await
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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
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
                                // Unsubscribe from all relay subscriptions
                                nostr_client.unsubscribe_all().await;
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
            db::remove_setting,
            fetch_messages,
            message,
            paste_message,
            voice_message,
            file_message,
            warmup_nip96_servers,
            react,
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
            logout
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
