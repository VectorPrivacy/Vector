use std::borrow::Cow;
use lazy_static::lazy_static;
use nostr_sdk::prelude::*;
use once_cell::sync::OnceCell;
use tokio::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager, Runtime};
use tauri_plugin_notification::NotificationExt;
use rand::{thread_rng, Rng};
use rand::distributions::Alphanumeric;

mod crypto;

mod db;
use db::SlimProfile;

mod account_manager;

mod mls;
pub use mls::MlsService;


use db::save_chat_messages;

mod voice;
use voice::AudioRecorder;

mod net;

mod blossom;

mod util;
use util::{get_file_type_description, calculate_file_hash, format_bytes};

#[cfg(target_os = "android")]
#[path = "android/mod.rs"]
mod android;

#[cfg(all(not(target_os = "android"), feature = "whisper"))]
mod whisper;

mod message;
pub use message::{Message, Attachment, Reaction};

mod profile;
pub use profile::{Profile, Status};

mod profile_sync;

mod chat;
pub use chat::{Chat, ChatType, ChatMetadata};

mod rumor;
pub use rumor::{RumorEvent, RumorContext, RumorProcessingResult, ConversationType, process_rumor};

// Flat event storage layer (protocol-aligned)
mod stored_event;
pub use stored_event::{StoredEvent, StoredEventBuilder, event_kind};

mod deep_link;

// Mini Apps (WebXDC-compatible) support
mod miniapps;

// Image caching for avatars, banners, and Mini App icons
mod image_cache;

// PIVX Promos (addressless cryptocurrency payments)
mod pivx;

// Audio processing: resampling (all platforms) + notification playback (desktop only)
mod audio;

/// # Trusted Relays
///
/// The 'Trusted Relays' handle events that MAY have a small amount of public-facing metadata attached (i.e: Expiration tags).
///
/// These relays may be used for events like Typing Indicators, Key Exchanges (forward-secrecy setup) and more.
/// Multiple relays provide redundancy for critical operations.
pub(crate) static TRUSTED_RELAYS: &[&str] = &[
    "wss://jskitty.cat/nostr",
    "wss://asia.vectorapp.io/nostr",
    "wss://nostr.computingcache.com",
];

/// # Blossom Media Servers
///
/// A list of Blossom servers for file uploads with automatic failover.
/// The system will try each server in order until one succeeds.
static BLOSSOM_SERVERS: OnceCell<std::sync::Mutex<Vec<String>>> = OnceCell::new();

/// Initialize default Blossom servers
fn init_blossom_servers() -> Vec<String> {
    vec![
        "https://blossom.primal.net".to_string(),
    ]
}

/// Get the list of Blossom servers (internal function)
pub(crate) fn get_blossom_servers() -> Vec<String> {
    BLOSSOM_SERVERS
        .get_or_init(|| std::sync::Mutex::new(init_blossom_servers()))
        .lock()
        .unwrap()
        .clone()
}


static MNEMONIC_SEED: OnceCell<String> = OnceCell::new();
static ENCRYPTION_KEY: OnceCell<[u8; 32]> = OnceCell::new();
pub(crate) static NOSTR_CLIENT: OnceCell<Client> = OnceCell::new();
pub(crate) static TAURI_APP: OnceCell<AppHandle> = OnceCell::new();

#[derive(Clone)]
struct PendingInviteAcceptance {
    invite_code: String,
    inviter_pubkey: PublicKey,
}

static PENDING_INVITE: OnceCell<PendingInviteAcceptance> = OnceCell::new();

// Track which MLS welcomes we've already sent notifications for (by wrapper_event_id)
lazy_static! {
    static ref NOTIFIED_WELCOMES: Mutex<std::collections::HashSet<String>> = Mutex::new(std::collections::HashSet::new());
}

// TEMPORARY cache of wrapper_event_ids for fast duplicate detection during INIT SYNC ONLY
// - Populated at init with recent wrapper_ids (last 30 days) to avoid SQL queries for each historical event
// - Only used for historical sync events (is_new = false), NOT for real-time new events
// - Cleared when sync finishes to free memory
lazy_static! {
    static ref WRAPPER_ID_CACHE: Mutex<std::collections::HashSet<String>> = Mutex::new(std::collections::HashSet::new());
}




#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
enum SyncMode {
    ForwardSync,   // Initial sync from most recent message going backward
    BackwardSync,  // Syncing historically old messages
    DeepRescan,    // Deep rescan mode - continues until 30 days of no events
    Finished       // Sync complete
}

#[derive(serde::Serialize, Clone, Debug)]
struct ChatState {
    profiles: Vec<Profile>,
    chats: Vec<Chat>,
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
            chats: Vec::new(),
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

            self.profiles[position] = full_profile;
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

    /// Get a chat by ID
    fn get_chat(&self, id: &str) -> Option<&Chat> {
        self.chats.iter().find(|c| c.id == id)
    }
    
    /// Get a mutable chat by ID
    fn get_chat_mut(&mut self, id: &str) -> Option<&mut Chat> {
        self.chats.iter_mut().find(|c| c.id == id)
    }

    /// Create a new chat for a DM with a specific user
    fn create_dm_chat(&mut self, their_npub: &str) -> String {
        // Check if chat already exists
        if self.get_chat(&their_npub).is_none() {
            let chat = Chat::new_dm(their_npub.to_string());
            self.chats.push(chat);
        }
        
        their_npub.to_string()
    }

    /// Create or get an MLS group chat
    fn create_or_get_mls_group_chat(&mut self, group_id: &str, participants: Vec<String>) -> String {
        // Check if chat already exists
        if self.get_chat(group_id).is_none() {
            let chat = Chat::new_mls_group(group_id.to_string(), participants);
            self.chats.push(chat);
        }
        
        group_id.to_string()
    }

    /// Add a message to a chat via its ID
    fn add_message_to_chat(&mut self, chat_id: &str, message: Message) -> bool {
        let is_msg_added = match self.get_chat_mut(chat_id) {
            Some(chat) => {
                // Add the message to the existing chat
                chat.internal_add_message(message)
            },
            None => {
                // Chat doesn't exist, create it and add the message
                // Determine chat type based on chat_id format
                let chat = if chat_id.starts_with("npub1") {
                    // DM chat: use the chat_id as the participant
                    Chat::new_dm(chat_id.to_string())
                } else {
                    // MLS group: participants will be set later
                    Chat::new(chat_id.to_string(), ChatType::MlsGroup, vec![])
                };
                let mut chat = chat;
                let was_added = chat.internal_add_message(message);
                self.chats.push(chat);
                was_added
            }
        };

        // Sort our chat positions based on last message time
        self.chats.sort_by(|a, b| {
            // Get last message time for both chats
            let a_time = a.last_message_time();
            let b_time = b.last_message_time();

            // Compare timestamps in reverse order (newest first)
            b_time.cmp(&a_time)
        });

        is_msg_added
    }


    /// Add a message to a chat via its participant npub
    fn add_message_to_participant(&mut self, their_npub: &str, message: Message) -> bool {
        // Ensure profiles exist for the participant
        if self.get_profile(their_npub).is_none() {
            // Create a basic profile for the participant
            let mut profile = Profile::new();
            profile.id = their_npub.to_string();
            profile.mine = false; // It's not our profile
            
            // Update the frontend about the new profile
            if let Some(handle) = TAURI_APP.get() {
                handle.emit("profile_update", &profile).unwrap();
            }
            
            // Add to our profiles list
            self.profiles.push(profile);
        }
        
        // Create or get the chat ID
        let chat_id = self.create_dm_chat(their_npub);
        
        // Add the message to the chat
        self.add_message_to_chat(&chat_id, message)
    }
    
    /// Count unread messages across all profiles
    fn count_unread_messages(&self) -> u32 {
        let mut total_unread = 0;
         
        // Count unread messages in all chats
        for chat in &self.chats {
            // Skip muted chats entirely
            if chat.muted {
                continue;
            }

            // Skip chats where the corresponding profile is muted (for DMs)
            let mut skip_for_profile_mute = false;
            match chat.chat_type {
                ChatType::DirectMessage => {
                    // For DMs, chat.id is the other participant's npub
                    if let Some(profile) = self.get_profile(&chat.id) {
                        if profile.muted {
                            skip_for_profile_mute = true;
                        }
                    }
                }
                ChatType::MlsGroup => {
                    // For MLS groups, muting is handled at the chat level (already checked above)
                    // No additional profile-level muting needed
                }
            }
            if skip_for_profile_mute {
                continue;
            }

            // Find the last read message ID for this chat
            let last_read_id = &chat.last_read;
            
            // Walk backwards from the end to count unread messages
            // Stop when we hit: 1) our own message, or 2) the last_read message
            let mut unread_count = 0;
            for msg in chat.messages.iter().rev() {
                // If we hit our own message, stop - we clearly read everything before it
                if msg.mine {
                    break;
                }
                
                // If we hit the last_read message, stop - everything at and before this is read
                if !last_read_id.is_empty() && msg.id == *last_read_id {
                    break;
                }
                
                // Count this message as unread
                unread_count += 1;
            }
            
            total_unread += unread_count as u32;
        }
        
        total_unread
    }

    /// Find a message by its ID across all chats
    fn find_message(&self, message_id: &str) -> Option<(&Chat, &Message)> {
        for chat in &self.chats {
            if let Some(message) = chat.messages.iter().find(|m| m.id == message_id) {
                return Some((chat, message));
            }
        }
        None
    }

    /// Find a chat and message by message ID across all chats (mutable)
    fn find_chat_and_message_mut(&mut self, message_id: &str) -> Option<(&str, &mut Message)> {
        for chat in &mut self.chats {
            if let Some(message) = chat.messages.iter_mut().find(|m| m.id == message_id) {
                return Some((&chat.id, message));
            }
        }
        None
    }

}

lazy_static! {
    static ref STATE: Mutex<ChatState> = Mutex::new(ChatState::new());
}

#[tauri::command]
async fn fetch_messages<R: Runtime>(
    handle: AppHandle<R>,
    init: bool,
    relay_url: Option<String>
) {
    let client = NOSTR_CLIENT.get().expect("Nostr client not initialized");

    // Grab our pubkey
    let signer = client.signer().await.unwrap();
    let my_public_key = signer.get_public_key().await.unwrap();

    // If relay_url is provided, this is a single-relay sync that bypasses global state
    if relay_url.is_some() {
        // Single relay sync - always fetch last 2 days
        let now = Timestamp::now();
        let two_days_ago = now.as_u64() - (60 * 60 * 24 * 2);
        
        let filter = Filter::new()
            .pubkey(my_public_key)
            .kind(Kind::GiftWrap)
            .since(Timestamp::from_secs(two_days_ago))
            .until(now);

        // Fetch from specific relay only
        let mut events = client
            .stream_events_from(vec![relay_url.unwrap()], filter, std::time::Duration::from_secs(30))
            .await
            .unwrap();

        // Process events without affecting global sync state
        while let Some(event) = events.next().await {
            handle_event(event, false).await;
        }
        
        // Also sync MLS group messages after single-relay reconnection
        if let Err(e) = sync_mls_groups_now(None).await {
            eprintln!("[Single-Relay Sync] Failed to sync MLS groups: {}", e);
        }
        
        return; // Exit early for single-relay syncs
    }

    // Regular sync logic with global state management
    let (since_timestamp, until_timestamp) = {
        let mut state = STATE.lock().await;
        
        if init {
            // Set current account for SQL mode if profile database exists
            // This must be done BEFORE loading chats/messages so SQL mode is active
            let signer = client.signer().await.unwrap();
            let my_public_key = signer.get_public_key().await.unwrap();
            let npub = my_public_key.to_bech32().unwrap();
            
            let app_data = handle.path().app_data_dir().ok();
            if let Some(data_dir) = app_data {
                let profile_db = data_dir.join(&npub).join("vector.db");
                if profile_db.exists() {
                    let _ = crate::account_manager::set_current_account(npub.clone());
                    println!("[Startup] Set current account for SQL mode: {}", npub);
                }
            }
            
            // Load our DB (if we haven't already; i.e: our profile is the single loaded profile since login)
            let mut needs_integrity_check = false;
            if state.profiles.len() == 1 {
                let profiles = db::get_all_profiles(&handle).await.unwrap();
                // Load our Profile Cache into the state
                state.merge_db_profiles(profiles).await;

                // Spawn background task to cache profile images for offline support
                tokio::spawn(async {
                    profile::cache_all_profile_images().await;
                });

                // Load chats and their messages from database
                let slim_chats_result = db::get_all_chats(&handle).await;
                if let Ok(slim_chats) = slim_chats_result {
                    // Load MLS groups to check for evicted status
                    let mls_groups: Option<Vec<mls::MlsGroupMetadata>> =
                        db::load_mls_groups(&handle).await.ok();
                    
                    // Convert slim chats to full chats and load their messages
                    for slim_chat in slim_chats {
                        let mut chat = slim_chat.to_chat();
                        
                        // Skip MLS group chats that are marked as evicted
                        // MLS group chat IDs are just the group_id (no prefix)
                        if chat.chat_type == ChatType::MlsGroup {
                            if let Some(ref groups) = mls_groups {
                                if let Some(group) = groups.iter().find(|g| g.group_id.as_str() == chat.id()) {
                                    if group.evicted {
                                        println!("[Startup] Skipping evicted MLS group chat: {}", chat.id());
                                        continue; // Skip this chat
                                    }
                                }
                            }
                        }
                        
                        // Load only the last message for preview (optimization: full messages loaded on-demand by frontend)
                        let last_messages_result = db::get_chat_last_messages(&handle, &chat.id(), 1).await;
                        if let Ok(last_messages) = last_messages_result {
                            for message in last_messages {
                                // Check if this message has downloaded attachments (for integrity check)
                                if !needs_integrity_check && message.attachments.iter().any(|att| att.downloaded) {
                                    needs_integrity_check = true;
                                }
                                chat.internal_add_message(message);
                            }
                        } else {
                            eprintln!("Failed to load last message for chat {}: {:?}", chat.id(), last_messages_result);
                        }
                        
                        // Ensure profiles exist for all chat participants
                        for participant in chat.participants() {
                            if state.get_profile(participant).is_none() {
                                // Create a basic profile for the participant
                                let mut profile = Profile::new();
                                profile.id = participant.clone();
                                profile.mine = false; // It's not our profile
                                state.profiles.push(profile);
                            }
                        }

                        // Add chat to state
                        state.chats.push(chat);

                        // Sort the chats by their last received message
                        state.chats.sort_by(|a, b| b.last_message_time().cmp(&a.last_message_time()));
                    }
                } else {
                    eprintln!("Failed to load chats from database: {:?}", slim_chats_result);
                }
            }
            
            if needs_integrity_check {
                // Clean up empty file attachments first
                cleanup_empty_file_attachments(&handle, &mut state).await;
                
                // Check integrity without dropping state
                check_attachment_filesystem_integrity(&handle, &mut state).await;
                
                // Preload ID caches for maximum performance
                if let Err(e) = db::preload_id_caches(&handle).await {
                    eprintln!("[Cache] Failed to preload ID caches: {}", e);
                }
                
                // Preload wrapper_event_ids for fast duplicate detection during sync
                // Load last 30 days of wrapper_ids to cover typical sync window
                if let Ok(wrapper_ids) = db::load_recent_wrapper_ids(&handle, 30).await {
                    let mut cache = WRAPPER_ID_CACHE.lock().await;
                    *cache = wrapper_ids;
                }
                
                // Send the state to our frontend to signal finalised init with a full state
                handle.emit("init_finished", serde_json::json!({
                    "profiles": &state.profiles,
                    "chats": &state.chats
                })).unwrap();
            } else {
                // Even if no integrity check needed, still clean up empty files
                cleanup_empty_file_attachments(&handle, &mut state).await;
                
                // Preload ID caches for maximum performance
                if let Err(e) = db::preload_id_caches(&handle).await {
                    eprintln!("[Cache] Failed to preload ID caches: {}", e);
                }
                
                // Preload wrapper_event_ids for fast duplicate detection during sync
                // Load last 30 days of wrapper_ids to cover typical sync window
                if let Ok(wrapper_ids) = db::load_recent_wrapper_ids(&handle, 30).await {
                    let mut cache = WRAPPER_ID_CACHE.lock().await;
                    *cache = wrapper_ids;
                }
                
                // No integrity check needed, send init immediately
                handle.emit("init_finished", serde_json::json!({
                    "profiles": &state.profiles,
                    "chats": &state.chats
                })).unwrap();
            }

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
        } else if state.sync_mode == SyncMode::DeepRescan {
            // Deep rescan mode - scan backwards in 2-day increments until 30 days of no events
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
            // Only emit if this is not a single-relay sync
            if relay_url.is_none() {
                handle.emit("sync_finished", ()).unwrap();
            }
            return;
        }
    }

    // Emit our current "Sync Range" to the frontend (only for general syncs, not single-relay)
    if relay_url.is_none() {
        handle.emit("sync_progress", serde_json::json!({
            "since": since_timestamp.as_u64(),
            "until": until_timestamp.as_u64(),
            "mode": format!("{:?}", STATE.lock().await.sync_mode)
        })).unwrap();
    }

    // Fetch GiftWraps related to us within the time window
    let filter = Filter::new()
        .pubkey(my_public_key)
        .kind(Kind::GiftWrap)
        .since(since_timestamp)
        .until(until_timestamp);

    let mut event_stream = if let Some(url) = &relay_url {
        // Fetch from specific relay
        client
            .stream_events_from(vec![url], filter, std::time::Duration::from_secs(30))
            .await
            .unwrap()
    } else {
        // Fetch from all relays
        client
            .stream_events(filter, std::time::Duration::from_secs(60))
            .await
            .unwrap()
    };

    // Count total events fetched (for DeepRescan) and new messages added (for other modes)
    // We'll compute total count while iterating; placeholder will be set after loop
    let mut new_messages_count: u16 = 0;
    while let Some(event) = event_stream.next().await {
        // Count the amount of accepted (new) events
        if handle_event(event, false).await {
            new_messages_count += 1;
        }
    }

    // After processing all events, total_events_count equals the number of processed events
    let total_events_count = new_messages_count as u16;
    let should_continue = {
        let mut state = STATE.lock().await;
        let mut continue_sync = true;

        // Increment total iterations counter
        state.sync_total_iterations += 1;

        // For DeepRescan, use total events count; for other modes, use new messages count
        let events_found = if state.sync_mode == SyncMode::DeepRescan {
            total_events_count
        } else {
            new_messages_count
        };

        // Update state based on if events were found
        if events_found > 0 {
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
                // Time to switch mode - calculate oldest timestamp while holding lock
                let mut oldest_timestamp = None;
                
                // Check each chat's messages for oldest timestamp
                for chat in &state.chats {
                    if let Some(oldest_msg_time) = chat.last_message_time() {
                        match oldest_timestamp {
                            None => oldest_timestamp = Some(oldest_msg_time),
                            Some(current_oldest) => {
                                if oldest_msg_time < current_oldest {
                                    oldest_timestamp = Some(oldest_msg_time);
                                }
                            }
                        }
                    }
                }

                // Switch to backward sync mode
                state.sync_mode = SyncMode::BackwardSync;
                state.sync_empty_iterations = 0;
                state.sync_total_iterations = 0;

                if let Some(oldest_ts) = oldest_timestamp {
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
        } else if state.sync_mode == SyncMode::DeepRescan {
            // For deep rescan, continue until:
            // No messages found for 15 consecutive iterations (30 days of no events)
            // Each iteration is 2 days, so 15 iterations = 30 days
            let enough_empty_iterations = state.sync_empty_iterations >= 15;

            if enough_empty_iterations {
                // We've completed deep rescan
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
        if relay_url.is_none() {
            handle.emit("sync_slice_finished", ()).unwrap();
        }
    } else {
        // We're done with sync - update state first, then emit event
        {
            let mut state = STATE.lock().await;
            state.sync_mode = SyncMode::Finished;
            state.is_syncing = false;
            state.sync_empty_iterations = 0;
            state.sync_total_iterations = 0;
        } // Release lock before emitting event
        
        // Clear the wrapper_id cache - it's only needed during sync
        {
            let mut cache = WRAPPER_ID_CACHE.lock().await;
            let cache_size = cache.len();
            cache.clear();
            cache.shrink_to_fit();
            // Each entry: 64-char hex String (~88 bytes) + HashSet overhead (~48 bytes) ≈ 136 bytes
            println!("[Startup] Sync Complete - Dumped NIP-59 Decryption Cache (~{} KB Memory freed)", (cache_size * 136) / 1024);
        }

        if relay_url.is_none() {
            handle.emit("sync_finished", ()).unwrap();
            
            // Now that regular sync is complete and chats are loaded, sync MLS groups
            // This ensures chat data is in memory before MLS tries to sync participants
            let handle_clone = handle.clone();
            tokio::task::spawn(async move {
                // Small delay to ensure init_finished has been processed
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                if let Err(e) = sync_mls_groups_now(None).await {
                    eprintln!("[MLS] Post-sync MLS group sync failed: {}", e);
                }
                
                // After MLS sync completes, check if weekly VACUUM is needed
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                if let Err(e) = db::check_and_vacuum_if_needed(&handle_clone).await {
                    eprintln!("[Maintenance] Weekly VACUUM check failed: {}", e);
                }
            });
        }
    }
}

/// Removes attachments with empty file hash from all messages
/// Also removes messages that have ONLY corrupted attachments (no content)
/// This cleans up corrupted uploads that resulted in 0-byte files
async fn cleanup_empty_file_attachments<R: Runtime>(
    handle: &AppHandle<R>,
    state: &mut ChatState,
) {
    const EMPTY_FILE_HASH: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    let mut cleaned_count = 0;
    let mut chats_to_update = Vec::new();
    
    for chat in &mut state.chats {
        let mut chat_had_changes = false;
        
        // First pass: remove attachments with empty file hash
        for message in &mut chat.messages {
            let original_count = message.attachments.len();
            
            // Remove attachments with empty file hash in their URL
            message.attachments.retain(|attachment| {
                !attachment.url.contains(EMPTY_FILE_HASH)
            });
            
            let removed = original_count - message.attachments.len();
            if removed > 0 {
                cleaned_count += removed;
                chat_had_changes = true;
            }
        }
        
        // Second pass: remove messages that are now empty (no content, no attachments)
        let messages_before = chat.messages.len();
        chat.messages.retain(|message| {
            !message.content.is_empty() || !message.attachments.is_empty()
        });
        
        if chat.messages.len() < messages_before {
            chat_had_changes = true;
        }
        
        // If this chat had changes, save all its messages
        if chat_had_changes {
            chats_to_update.push((chat.id(), chat.messages.clone()));
        }
    }
    
    // Save updated chats to database
    for (chat_id, messages) in chats_to_update {
        if let Err(e) = save_chat_messages(handle.clone(), &chat_id, &messages).await {
            eprintln!("Failed to save chat after cleaning empty attachments: {}", e);
        }
    }
    
    if cleaned_count > 0 {
        eprintln!("Cleaned up {} empty file attachments", cleaned_count);
    }
}

/// Checks if downloaded attachments still exist on the filesystem
/// Sets downloaded=false for any missing files and updates the database
async fn check_attachment_filesystem_integrity<R: Runtime>(
    handle: &AppHandle<R>,
    state: &mut ChatState,
) {
    let mut total_checked = 0;
    let mut chats_with_updates = std::collections::HashMap::new();
    
    // Capture the starting timestamp
    let start_time = std::time::Instant::now();
    
    // First pass: count total attachments to check
    let mut total_attachments = 0;
    for chat in &state.chats {
        for message in &chat.messages {
            for attachment in &message.attachments {
                if attachment.downloaded {
                    total_attachments += 1;
                }
            }
        }
    }
    
    // Iterate through all chats and their messages with mutable access to update downloaded status
    for (chat_idx, chat) in state.chats.iter_mut().enumerate() {
        let mut updated_messages = Vec::new();
        
        for message in &mut chat.messages {
            let mut message_updated = false;
            
            for attachment in &mut message.attachments {
                // Only check attachments that are marked as downloaded
                if attachment.downloaded {
                    total_checked += 1;
                    
                    // Emit progress every 2 attachments or on the last one, but only if process has taken >1 second
                    if (total_checked % 2 == 0 || total_checked == total_attachments) && start_time.elapsed().as_secs() >= 1 {
                        handle.emit("progress_operation", serde_json::json!({
                            "type": "progress",
                            "current": total_checked,
                            "total": total_attachments,
                            "message": "Checking file integrity"
                        })).unwrap();
                    }
                    
                    // Check if the file exists on the filesystem
                    let file_path = std::path::Path::new(&attachment.path);
                    if !file_path.exists() {
                        // File is missing, set downloaded to false
                        attachment.downloaded = false;
                        message_updated = true;
                        attachment.path = String::new();
                    }
                }
            }
            
            // If any attachment in this message was updated, we need to save the message
            if message_updated {
                updated_messages.push(message.clone());
            }
        }
        
        // If any messages in this chat were updated, store them for database update
        if !updated_messages.is_empty() {
            chats_with_updates.insert(chat_idx, updated_messages);
        }
    }
    
    // Update database for any messages with missing attachments
    if !chats_with_updates.is_empty() {
        // Only emit progress if process has taken >1 second
        if start_time.elapsed().as_secs() >= 1 {
            handle.emit("progress_operation", serde_json::json!({
                "type": "progress",
                "total": chats_with_updates.len(),
                "current": 0,
                "message": "Updating database..."
            })).unwrap();
        }
        
        // Save updated messages for each chat that had changes
        let mut saved_count = 0;
        let total_chats = chats_with_updates.len();
        for (chat_idx, _updated_messages) in chats_with_updates {
            // Since we're iterating over existing indices, we know the chat exists
            let chat = &state.chats[chat_idx];
            let chat_id = chat.id().clone();

            // Save
            let all_messages = &chat.messages;
            if let Err(e) = save_chat_messages(handle.clone(), &chat_id, all_messages).await {
                eprintln!("Failed to update messages after filesystem check: {}", e);
            } else {
                saved_count += 1;
            }
            
            // Emit progress for database updates, but only if process has taken >1 second
            if ((saved_count) % 5 == 0 || saved_count == total_chats) && start_time.elapsed().as_secs() >= 1 {
                handle.emit("progress_operation", serde_json::json!({
                    "type": "progress",
                    "current": saved_count,
                    "total": total_chats,
                    "message": "Updating database"
                })).unwrap();
            }
        }
    }
}

#[tauri::command]
async fn start_typing(receiver: String) -> bool {
    let client = NOSTR_CLIENT.get().expect("Nostr client not initialized");
    let signer = client.signer().await.unwrap();
    let my_public_key = signer.get_public_key().await.unwrap();

    // Check if this is a group chat (group IDs are hex, not bech32)
    match PublicKey::from_bech32(receiver.as_str()) {
        Ok(pubkey) => {
            // This is a DM - use NIP-17 gift wrapping
            
            // Build and broadcast the Typing Indicator
            let rumor = EventBuilder::new(Kind::ApplicationSpecificData, "typing")
                .tag(Tag::public_key(pubkey))
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
            // Note: we set a 30-second expiry so that relays can purge typing indicators quickly
            let expiry_time = Timestamp::from_secs(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs()
                    + 30,
            );
            match client
                .gift_wrap_to(
                    TRUSTED_RELAYS.iter().copied(),
                    &pubkey,
                    rumor,
                    [Tag::expiration(expiry_time)],
                )
                .await
            {
                Ok(_) => true,
                Err(_) => false,
            }
        }
        Err(_) => {
            // This is a group chat - use MLS
            let group_id = receiver.clone();

            // Build the typing indicator rumor
            let rumor = EventBuilder::new(Kind::ApplicationSpecificData, "typing")
                .tag(Tag::custom(TagKind::d(), vec!["vector"]))
                .tag(Tag::expiration(Timestamp::from_secs(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_secs()
                        + 30,
                )))
                .build(my_public_key);

            // Send via MLS
            match mls::send_mls_message(&group_id, rumor, None).await {
                Ok(_) => true,
                Err(_e) => false,
            }
        }
    }
}

/// Send a WebXDC peer advertisement to chat participants
/// This announces our Iroh node address for realtime channel peer discovery
#[tauri::command]
async fn send_webxdc_peer_advertisement(
    receiver: String,
    topic_id: String,
    node_addr: String,
) -> bool {
    let client = NOSTR_CLIENT.get().expect("Nostr client not initialized");
    let signer = client.signer().await.unwrap();
    let my_public_key = signer.get_public_key().await.unwrap();
    let my_npub = my_public_key.to_bech32().unwrap_or_else(|_| "unknown".to_string());

    println!("[WEBXDC] Sending peer advertisement: my_npub={}, receiver={}, topic={}", my_npub, receiver, topic_id);
    log::info!("Sending WebXDC peer advertisement to {} for topic {}", receiver, topic_id);
    log::debug!("Node address: {}", node_addr);

    // Check if this is a group chat (group IDs are hex, not bech32)
    match PublicKey::from_bech32(receiver.as_str()) {
        Ok(pubkey) => {
            // This is a DM - use NIP-17 gift wrapping

            // Build the peer advertisement rumor
            let rumor = EventBuilder::new(Kind::ApplicationSpecificData, "peer-advertisement")
                .tag(Tag::public_key(pubkey))
                .tag(Tag::custom(TagKind::d(), vec!["vector-webxdc-peer"]))
                .tag(Tag::custom(TagKind::custom("webxdc-topic"), vec![topic_id]))
                .tag(Tag::custom(TagKind::custom("webxdc-node-addr"), vec![node_addr]))
                .tag(Tag::expiration(Timestamp::from_secs(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_secs()
                        + 300, // 5 minute expiry
                )))
                .build(my_public_key);

            // Gift Wrap and send to receiver via our Trusted Relays
            let expiry_time = Timestamp::from_secs(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs()
                    + 300,
            );
            match client
                .gift_wrap_to(
                    TRUSTED_RELAYS.iter().copied(),
                    &pubkey,
                    rumor,
                    [Tag::expiration(expiry_time)],
                )
                .await
            {
                Ok(_) => true,
                Err(_) => false,
            }
        }
        Err(_) => {
            // This is a group chat - use MLS
            let group_id = receiver.clone();
            
            // Build the peer advertisement rumor
            let rumor = EventBuilder::new(Kind::ApplicationSpecificData, "peer-advertisement")
                .tag(Tag::custom(TagKind::d(), vec!["vector-webxdc-peer"]))
                .tag(Tag::custom(TagKind::custom("webxdc-topic"), vec![topic_id]))
                .tag(Tag::custom(TagKind::custom("webxdc-node-addr"), vec![node_addr]))
                .tag(Tag::expiration(Timestamp::from_secs(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_secs()
                        + 300,
                )))
                .build(my_public_key);

            // Send via MLS
            match mls::send_mls_message(&group_id, rumor, None).await {
                Ok(_) => true,
                Err(_e) => false,
            }
        }
    }
}

/// Get paginated messages for a chat directly from the database
/// Also adds the messages to the backend state for cache synchronization
#[tauri::command]
async fn get_chat_messages_paginated<R: Runtime>(
    handle: AppHandle<R>,
    chat_id: String,
    limit: usize,
    offset: usize,
) -> Result<Vec<Message>, String> {
    // Load messages from database
    let messages = db::get_chat_messages_paginated(&handle, &chat_id, limit, offset).await?;
    
    // Also add these messages to the backend state for cache synchronization
    // This ensures operations like fetch_msg_metadata can find the messages
    if !messages.is_empty() {
        let mut state = STATE.lock().await;
        if let Some(chat) = state.chats.iter_mut().find(|c| c.id == chat_id) {
            for msg in &messages {
                // Only add if not already present (avoid duplicates)
                if !chat.messages.iter().any(|m| m.id == msg.id) {
                    chat.messages.push(msg.clone());
                }
            }
            // Sort messages by timestamp to maintain order
            chat.messages.sort_by_key(|m| m.at);
        }
    }
    
    Ok(messages)
}

/// Get the total message count for a chat
#[tauri::command]
async fn get_chat_message_count<R: Runtime>(
    handle: AppHandle<R>,
    chat_id: String,
) -> Result<usize, String> {
    db::get_chat_message_count(&handle, &chat_id).await
}

/// Get message views (composed from events table) for a chat
/// This is the new event-based approach that computes reactions from flat events
#[tauri::command]
async fn get_message_views<R: Runtime>(
    handle: AppHandle<R>,
    chat_id: String,
    limit: usize,
    offset: usize,
) -> Result<Vec<Message>, String> {
    // Convert chat identifier to database ID
    let chat_int_id = db::get_chat_id_by_identifier(&handle, &chat_id)?;

    // Get materialized message views from events
    let messages = db::get_message_views(&handle, chat_int_id, limit, offset).await?;

    // Sync to backend state for cache compatibility (uses binary search for efficient insertion)
    if !messages.is_empty() {
        let mut state = STATE.lock().await;
        if let Some(chat) = state.chats.iter_mut().find(|c| c.id == chat_id) {
            for msg in messages.iter().cloned() {
                chat.internal_add_message(msg);
            }
        }
    }

    Ok(messages)
}

/// Get messages around a specific message ID (for scrolling to replied-to messages)
/// Loads messages from (target - context_before) to the most recent
#[tauri::command]
async fn get_messages_around_id<R: Runtime>(
    handle: AppHandle<R>,
    chat_id: String,
    target_message_id: String,
    context_before: usize,
) -> Result<Vec<Message>, String> {
    let messages = db::get_messages_around_id(&handle, &chat_id, &target_message_id, context_before).await?;

    // Sync to backend state so fetch_msg_metadata and other functions can find these messages
    if !messages.is_empty() {
        let mut state = STATE.lock().await;
        if let Some(chat) = state.chats.iter_mut().find(|c| c.id == chat_id) {
            for msg in messages.iter().cloned() {
                chat.internal_add_message(msg);
            }
        }
    }

    Ok(messages)
}

/// Evict messages from the backend cache for a specific chat
/// Called by frontend when LRU eviction occurs to keep caches in sync
#[tauri::command]
async fn evict_chat_messages(chat_id: String, keep_count: usize) -> Result<(), String> {
    let mut state = STATE.lock().await;
    if let Some(chat) = state.chats.iter_mut().find(|c| c.id == chat_id) {
        let total = chat.messages.len();
        if total > keep_count {
            // Keep only the last `keep_count` messages (most recent)
            let drain_count = total - keep_count;
            chat.messages.drain(0..drain_count);
        }
    }
    Ok(())
}

/// Build and return the file hash index for deduplication
/// Returns a map of file_hash -> attachment reference data
#[tauri::command]
async fn get_file_hash_index<R: Runtime>(
    handle: AppHandle<R>,
) -> Result<std::collections::HashMap<String, db::AttachmentRef>, String> {
    db::build_file_hash_index(&handle).await
}

#[tauri::command]
async fn handle_event(event: Event, is_new: bool) -> bool {
    // Get the wrapper (giftwrap) event ID for duplicate detection
    let wrapper_event_id = event.id.to_hex();
    
    // For historical sync events (is_new = false), use the wrapper_id cache for fast duplicate detection
    // For real-time new events (is_new = true), skip cache checks - they're guaranteed to be new
    if !is_new {
        // Check in-memory cache first (O(1) lookup, no SQL overhead)
        // This cache is only populated during init and cleared after sync finishes
        {
            let cache = WRAPPER_ID_CACHE.lock().await;
            if cache.contains(&wrapper_event_id) {
                // Already processed this giftwrap, skip (cache hit)
                return false;
            }
        }
        
        // Cache miss - check database as fallback (for events older than cache window)
        if let Some(handle) = TAURI_APP.get() {
            if let Ok(exists) = db::wrapper_event_exists(handle, &wrapper_event_id).await {
                if exists {
                    // Already processed this giftwrap, skip (DB hit)
                    return false;
                }
            }
        }
    }

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

            // Special handling for MLS Welcomes (not processed by rumor processor)
            if rumor.kind == Kind::MlsWelcome {
                // Convert rumor Event -> UnsignedEvent
                let unsigned_opt = serde_json::to_string(&rumor)
                    .ok()
                    .and_then(|s| nostr_sdk::UnsignedEvent::from_json(s.as_bytes()).ok());

                if let Some(unsigned) = unsigned_opt {
                    // Outer giftwrap id is our wrapper id for dedup/logs
                    let wrapper_id = event.id;
                    let app_handle = TAURI_APP.get().cloned();

                    // Use blocking thread for non-Send MLS engine
                    let processed = tokio::task::spawn_blocking(move || {
                        if app_handle.is_none() {
                            return false;
                        }
                        let handle = app_handle.unwrap();
                        let svc = MlsService::new_persistent(&handle);
                        if let Ok(mls) = svc {
                            if let Ok(engine) = mls.engine() {
                                match engine.process_welcome(&wrapper_id, &unsigned) {
                                    Ok(_) => return true,
                                    Err(e) => {
                                        eprintln!("[MLS] Failed to process welcome: {}", e);
                                        return false;
                                    }
                                }
                            }
                        }
                        false
                    })
                    .await
                    .unwrap_or(false);

                    if processed {
                        // Only notify UI after initial sync is complete
                        // During initial sync, invites are processed but not emitted to avoid UI updates before chats are loaded
                        let should_emit = {
                            let state = STATE.lock().await;
                            state.sync_mode == SyncMode::Finished || !state.is_syncing
                        };
                        
                        if should_emit {
                            if let Some(app) = TAURI_APP.get() {
                                let _ = app.emit("mls_invite_received", serde_json::json!({
                                    "wrapper_event_id": wrapper_id.to_hex()
                                }));
                            }
                        }
                        return true;
                    } else {
                        return false;
                    }
                } else {
                    eprintln!("[MLS] Failed to convert rumor to UnsignedEvent");
                    return false;
                }
            }

            // Convert rumor to RumorEvent for protocol-agnostic processing
            let rumor_event = RumorEvent {
                id: rumor.id.unwrap(),
                kind: rumor.kind,
                content: rumor.content.clone(),
                tags: rumor.tags.clone(),
                created_at: rumor.created_at,
                pubkey: rumor.pubkey,
            };

            let rumor_context = RumorContext {
                sender,
                is_mine,
                conversation_id: contact.clone(),
                conversation_type: ConversationType::DirectMessage,
            };

            // Process the rumor using our protocol-agnostic processor
            match process_rumor(rumor_event, rumor_context).await {
                Ok(result) => {
                    match result {
                        RumorProcessingResult::TextMessage(mut msg) => {
                            // Set the wrapper event ID for database storage
                            msg.wrapper_event_id = Some(wrapper_event_id.clone());
                            handle_text_message(msg, &contact, is_mine, is_new, &wrapper_event_id).await
                        }
                        RumorProcessingResult::FileAttachment(mut msg) => {
                            // Set the wrapper event ID for database storage
                            msg.wrapper_event_id = Some(wrapper_event_id.clone());
                            handle_file_attachment(msg, &contact, is_mine, is_new, &wrapper_event_id).await
                        }
                        RumorProcessingResult::Reaction(reaction) => {
                            handle_reaction(reaction, &contact).await
                        }
                        RumorProcessingResult::TypingIndicator { profile_id, until } => {
                            // Update the chat's typing participants
                            let active_typers = {
                                let mut state = STATE.lock().await;
                                // For DMs, the chat_id is the contact's npub
                                if let Some(chat) = state.get_chat_mut(&contact) {
                                    chat.update_typing_participant(profile_id.clone(), until);
                                    chat.get_active_typers()
                                } else {
                                    vec![]
                                }
                            };
                            
                            // Emit typing update event to frontend
                            if let Some(handle) = TAURI_APP.get() {
                                let _ = handle.emit("typing-update", serde_json::json!({
                                    "conversation_id": contact,
                                    "typers": active_typers,
                                }));
                            }
                            
                            true
                        }
                        RumorProcessingResult::WebxdcPeerAdvertisement { topic_id, node_addr } => {
                            // Handle WebXDC peer advertisement - add peer to realtime channel
                            handle_webxdc_peer_advertisement(&topic_id, &node_addr).await
                        }
                        RumorProcessingResult::UnknownEvent(mut event) => {
                            // Store unknown events for future compatibility
                            event.wrapper_event_id = Some(wrapper_event_id.clone());
                            handle_unknown_event(event, &contact).await
                        }
                        RumorProcessingResult::Ignored => false,
                        RumorProcessingResult::PivxPayment { gift_code, amount_piv, address, message_id, event } => {
                            // Save PIVX payment event to database
                            if let Some(handle) = TAURI_APP.get() {
                                let event_timestamp = event.created_at;
                                let _ = db::save_pivx_payment_event(handle, &contact, event).await;

                                // Emit PIVX payment event to frontend for DMs
                                let _ = handle.emit("pivx_payment_received", serde_json::json!({
                                    "conversation_id": contact,
                                    "gift_code": gift_code,
                                    "amount_piv": amount_piv,
                                    "address": address,
                                    "message_id": message_id,
                                    "sender": sender,
                                    "is_mine": is_mine,
                                    "at": event_timestamp * 1000,
                                }));
                            }
                            true
                        }
                        RumorProcessingResult::Edit { message_id, new_content, edited_at, mut event } => {
                            // Skip if this edit event was already processed (deduplication)
                            if let Some(handle) = TAURI_APP.get() {
                                if db::event_exists(handle, &event.id).unwrap_or(false) {
                                    return true; // Already processed, skip
                                }

                                // Save edit event to database with proper chat_id
                                if let Ok(chat_id) = db::get_chat_id_by_identifier(handle, &contact) {
                                    event.chat_id = chat_id;
                                }
                                event.wrapper_event_id = Some(wrapper_event_id.clone());
                                let _ = db::save_event(handle, &event).await;
                            }

                            // Update message in state and emit to frontend
                            let mut state = STATE.lock().await;
                            if let Some(chat) = state.get_chat_mut(&contact) {
                                if let Some(msg) = chat.get_message_mut(&message_id) {
                                    msg.apply_edit(new_content, edited_at);

                                    // Emit update to frontend
                                    if let Some(handle) = TAURI_APP.get() {
                                        let _ = handle.emit("message_update", serde_json::json!({
                                            "old_id": &message_id,
                                            "message": &msg,
                                            "chat_id": &contact
                                        }));
                                    }
                                }
                            }
                            true
                        }
                    }
                }
                Err(e) => {
                    eprintln!("Failed to process rumor: {}", e);
                    false
                }
            }
        }
        Err(_) => false,
    }
}

/// Handle a processed text message
async fn handle_text_message(mut msg: Message, contact: &str, is_mine: bool, is_new: bool, wrapper_event_id: &str) -> bool {
    // Check if message already exists in database (important for sync with partial message loading)
    if let Some(handle) = TAURI_APP.get() {
        if let Ok(exists) = db::message_exists_in_db(&handle, &msg.id).await {
            if exists {
                // Message already in DB but we got here (wrapper check passed)
                // Try to backfill the wrapper_event_id for future fast lookups
                // If backfill fails (message already has a different wrapper), add this wrapper to cache
                // to prevent repeated processing of duplicate giftwraps
                if let Ok(updated) = db::update_wrapper_event_id(&handle, &msg.id, wrapper_event_id).await {
                    if !updated {
                        // Message has a different wrapper_id - add this duplicate wrapper to cache
                        let mut cache = WRAPPER_ID_CACHE.lock().await;
                        cache.insert(wrapper_event_id.to_string());
                    }
                }
                return false;
            }
        }
    }

    // Populate reply context before emitting (for replies to old messages not in frontend cache)
    if !msg.replied_to.is_empty() {
        if let Some(handle) = TAURI_APP.get() {
            let _ = db::populate_reply_context(&handle, &mut msg).await;
        }
    }

    // Add the message to the state and handle database save in one operation to avoid multiple locks
    let was_msg_added_to_state = {
        let mut state = STATE.lock().await;
        state.add_message_to_participant(contact, msg.clone())
    };

    // If accepted in-state: commit to the DB and emit to the frontend
    if was_msg_added_to_state {
        // Send it to the frontend
        if let Some(handle) = TAURI_APP.get() {
            handle.emit("message_new", serde_json::json!({
                "message": &msg,
                "chat_id": contact
            })).unwrap();
        }

        // Send OS notification for incoming messages (only after confirming message is new)
        if !is_mine && is_new {
            let display_info = {
                let state = STATE.lock().await;
                match state.get_profile(contact) {
                    Some(profile) => {
                        if profile.muted {
                            None // Profile is muted, don't send notification
                        } else {
                            let display_name = if !profile.nickname.is_empty() {
                                profile.nickname.clone()
                            } else if !profile.name.is_empty() {
                                profile.name.clone()
                            } else {
                                String::from("New Message")
                            };
                            Some((display_name, msg.content.clone()))
                        }
                    }
                    None => Some((String::from("New Message"), msg.content.clone())),
                }
            };
            if let Some((display_name, content)) = display_info {
                let notification = NotificationData::direct_message(display_name, content);
                show_notification_generic(notification);
            }
        }

        // Save the new message to DB (chat_id = contact npub for DMs)
        if let Some(handle) = TAURI_APP.get() {
            // Only save the single new message (efficient!)
            let _ = db::save_message(handle.clone(), contact, &msg).await;
        }
        // Ensure OS badge is updated immediately after accepting the message
        if let Some(handle) = TAURI_APP.get() {
            let _ = update_unread_counter(handle.clone()).await;
        }
    }

    was_msg_added_to_state
}

/// Handle a processed file attachment
async fn handle_file_attachment(mut msg: Message, contact: &str, is_mine: bool, is_new: bool, wrapper_event_id: &str) -> bool {
    // Check if message already exists in database (important for sync with partial message loading)
    if let Some(handle) = TAURI_APP.get() {
        if let Ok(exists) = db::message_exists_in_db(&handle, &msg.id).await {
            if exists {
                // Message already in DB but we got here (wrapper check passed)
                // Try to backfill the wrapper_event_id for future fast lookups
                // If backfill fails (message already has a different wrapper), add this wrapper to cache
                // to prevent repeated processing of duplicate giftwraps
                if let Ok(updated) = db::update_wrapper_event_id(&handle, &msg.id, wrapper_event_id).await {
                    if !updated {
                        // Message has a different wrapper_id - add this duplicate wrapper to cache
                        let mut cache = WRAPPER_ID_CACHE.lock().await;
                        cache.insert(wrapper_event_id.to_string());
                    }
                }
                return false;
            }
        }
    }

    // Populate reply context before emitting (for replies to old messages not in frontend cache)
    if !msg.replied_to.is_empty() {
        if let Some(handle) = TAURI_APP.get() {
            let _ = db::populate_reply_context(&handle, &mut msg).await;
        }
    }

    // Get file extension for notification
    let extension = msg.attachments.first()
        .map(|att| att.extension.clone())
        .unwrap_or_else(|| String::from("file"));

    // Add the message to the state and clear typing indicator for sender
    let (was_msg_added_to_state, _active_typers) = {
        let mut state = STATE.lock().await;
        let added = state.add_message_to_participant(contact, msg.clone());
        
        // Clear typing indicator for the sender (they just sent a message)
        let typers = if let Some(chat) = state.get_chat_mut(contact) {
            chat.update_typing_participant(contact.to_string(), 0); // 0 = clear immediately
            chat.get_active_typers()
        } else {
            Vec::new()
        };
        
        (added, typers)
    };

    // If accepted in-state: commit to the DB and emit to the frontend
    if was_msg_added_to_state {
        // Send it to the frontend
        if let Some(handle) = TAURI_APP.get() {
            handle.emit("message_new", serde_json::json!({
                "message": &msg,
                "chat_id": contact
            })).unwrap();
        }

        // Send OS notification for incoming files (only after confirming message is new)
        if !is_mine && is_new {
            let display_info = {
                let state = STATE.lock().await;
                match state.get_profile(contact) {
                    Some(profile) => {
                        if profile.muted {
                            None // Profile is muted, don't send notification
                        } else {
                            let display_name = if !profile.nickname.is_empty() {
                                profile.nickname.clone()
                            } else if !profile.name.is_empty() {
                                profile.name.clone()
                            } else {
                                String::from("New Message")
                            };
                            Some((display_name, extension.clone()))
                        }
                    }
                    None => Some((String::from("New Message"), extension.clone())),
                }
            };
            if let Some((display_name, file_extension)) = display_info {
                let file_description = "Sent a ".to_string() + &get_file_type_description(&file_extension);
                let notification = NotificationData::direct_message(display_name, file_description);
                show_notification_generic(notification);
            }
        }

        // Save the new message to DB (chat_id = contact npub for DMs)
        if let Some(handle) = TAURI_APP.get() {
            // Only save the single new message (efficient!)
            let _ = db::save_message(handle.clone(), contact, &msg).await;
        }
        // Ensure OS badge is updated immediately after accepting the attachment
        if let Some(handle) = TAURI_APP.get() {
            let _ = update_unread_counter(handle.clone()).await;
        }
    }

    was_msg_added_to_state
}

/// Handle a processed reaction
async fn handle_reaction(reaction: Reaction, _contact: &str) -> bool {
    // Find the chat containing the referenced message and add the reaction
    // Use a single lock scope to avoid nested locks
    let (reaction_added, chat_id_for_save) = {
        let mut state = STATE.lock().await;
        let reaction_added = if let Some((chat_id, msg_mut)) = state.find_chat_and_message_mut(&reaction.reference_id) {
            msg_mut.add_reaction(reaction.clone(), Some(chat_id))
        } else {
            // Message not found in any chat - this can happen during sync
            // TODO: track these "ahead" reactions and re-apply them once sync has finished
            false
        };
        
        // If reaction was added, get the chat_id for saving
        let chat_id_for_save = if reaction_added {
            state.find_message(&reaction.reference_id)
                .map(|(chat, _)| chat.id().clone())
        } else {
            None
        };
        
        (reaction_added, chat_id_for_save)
    };

    // Save the updated message with the new reaction to our DB (outside of state lock)
    if let Some(chat_id) = chat_id_for_save {
        if let Some(handle) = TAURI_APP.get() {
            // Get only the message that was updated
            let updated_message = {
                let state = STATE.lock().await;
                state.find_message(&reaction.reference_id)
                    .map(|(_, msg)| msg.clone())
            };
            
            if let Some(msg) = updated_message {
                let _ = db::save_message(handle.clone(), &chat_id, &msg).await;
            }
        }
    }

    reaction_added
}

/// Handle an unknown event type - store for future compatibility
async fn handle_unknown_event(mut event: StoredEvent, contact: &str) -> bool {
    // Get the chat_id for this contact
    if let Some(handle) = TAURI_APP.get() {
        match db::get_chat_id_by_identifier(&handle, contact) {
            Ok(chat_id) => {
                event.chat_id = chat_id;
                // Save the event to the database
                if let Err(e) = db::save_event(&handle, &event).await {
                    eprintln!("Failed to save unknown event: {}", e);
                    return false;
                }
                // Emit event to frontend (it can render as "Unknown Event" placeholder)
                let _ = handle.emit("event_new", serde_json::json!({
                    "event": event,
                    "chat_id": contact
                }));
                true
            }
            Err(_) => {
                // Chat doesn't exist yet, skip this event
                eprintln!("Cannot save unknown event: chat not found for {}", contact);
                false
            }
        }
    } else {
        false
    }
}

/// Handle a WebXDC peer advertisement - add the peer to our realtime channel
async fn handle_webxdc_peer_advertisement(topic_id: &str, node_addr_encoded: &str) -> bool {
    use crate::miniapps::realtime::{decode_topic_id, decode_node_addr};
    
    println!("[WEBXDC] Received peer advertisement for topic {}", topic_id);
    
    // Decode the topic ID
    let topic = match decode_topic_id(topic_id) {
        Ok(t) => t,
        Err(e) => {
            log::warn!("Failed to decode topic ID in peer advertisement: {}", e);
            return false;
        }
    };
    
    // Decode the node address
    let node_addr = match decode_node_addr(node_addr_encoded) {
        Ok(addr) => addr,
        Err(e) => {
            log::warn!("Failed to decode node address in peer advertisement: {}", e);
            return false;
        }
    };
    
    // Get the MiniApps state and add the peer
    if let Some(handle) = TAURI_APP.get() {
        let state = handle.state::<miniapps::state::MiniAppsState>();
        
        // Check if we have an active realtime channel for this topic
        // We need to find any instance that has this topic active
        let has_channel = {
            let channels = state.realtime_channels.read().await;
            println!("[WEBXDC] Checking {} active channels for topic match", channels.len());
            for (label, ch) in channels.iter() {
                println!("[WEBXDC]   Channel '{}': topic={}, active={}",
                    label,
                    crate::miniapps::realtime::encode_topic_id(&ch.topic),
                    ch.active);
            }
            channels.values().any(|ch| ch.topic == topic && ch.active)
        };
        
        println!("[WEBXDC] has_channel for topic {}: {}", topic_id, has_channel);
        
        if has_channel {
            println!("[WEBXDC] Found active channel for topic {}, adding peer", topic_id);
            // Get the realtime manager and add the peer
            match state.realtime.get_or_init().await {
                Ok(iroh) => {
                    match iroh.add_peer(topic, node_addr.clone()).await {
                        Ok(_) => {
                            println!("[WEBXDC] Successfully added peer {} to realtime channel topic {}",
                                node_addr.node_id, topic_id);
                            return true;
                        }
                        Err(e) => {
                            println!("[WEBXDC] ERROR: Failed to add peer to realtime channel: {}", e);
                        }
                    }
                }
                Err(e) => {
                    println!("[WEBXDC] ERROR: Failed to get realtime manager: {}", e);
                }
            }
        } else {
            // Store as pending peer - we'll add them when we join the channel
            println!("[WEBXDC] Storing pending peer for topic {} (no active channel yet)", topic_id);
            state.add_pending_peer(topic, node_addr).await;
            
            // Emit event to frontend so it can update the UI (show "Click to Join" and player count)
            let pending_count = state.get_pending_peer_count(&topic).await;
            if let Some(main_window) = handle.get_webview_window("main") {
                use tauri::Emitter;
                let _ = main_window.emit("miniapp_realtime_status", serde_json::json!({
                    "topic": topic_id,
                    "peer_count": pending_count,
                    "is_active": false,
                    "has_pending_peers": true,
                }));
                println!("[WEBXDC] Emitted miniapp_realtime_status event: topic={}, pending_count={}", topic_id, pending_count);
            }
            
            return true;
        }
    }
    
    false
}

/*
MLS live subscriptions overview (using Marmot/MDK):
- GiftWrap subscription (Kind::GiftWrap):
  • Carries DMs/files and also MLS Welcomes. Welcomes are detected after unwrap in handle_event()
    when rumor.kind == Kind::MlsWelcome. We immediately persist via the MDK engine on a blocking
    thread (spawn_blocking) and emit "mls_invite_received" so the frontend can refresh
    list_pending_mls_welcomes without a manual sync.

- MLS Group Messages subscription (Kind::MlsGroupMessage):
  • Subscribed live in parallel to GiftWraps. We extract the wire group id from the 'h' tag and
    check membership using encrypted metadata (mls_groups). If a message is for a group we belong to,
    we process it via the MDK engine on a blocking thread, then persist to "mls_messages_{group_id}"
    and "mls_timeline_{group_id}" and emit "mls_message_new" for immediate UI updates.
  • For non-members: We attempt to process as a Welcome message (for invites from MDK-compatible clients).

- Deduplication:
  • Real-time path uses the same keys as sync (inner_event_id, wrapper_event_id). We only insert if
    inner_event_id is not present in the group messages map, and append to the timeline if absent.
    This prevents duplicates when subsequent explicit sync covers the same events.

- Send-boundary:
  • All MDK engine interactions occur inside tokio::task::spawn_blocking. We avoid awaits
    while holding the engine to respect non-Send constraints required by Tauri command futures.

- Privacy & logging:
  • We do not log plaintext message content. Logs are limited to ids, counts, kinds, and outcomes
    to aid QA without leaking sensitive content.
*/

#[tauri::command]
async fn list_group_cursors() -> Result<serde_json::Value, String> {
    tokio::task::spawn_blocking(move || {
        let handle = TAURI_APP.get().ok_or("App handle not initialized")?.clone();
        let rt = tokio::runtime::Handle::current();
        rt.block_on(async move {
            let mls = MlsService::new_persistent(&handle).map_err(|e| e.to_string())?;
            let cursors = mls.read_event_cursors().await.map_err(|e| e.to_string())?;
            serde_json::to_value(&cursors).map_err(|e| e.to_string())
        })
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))?
}

#[tauri::command]
async fn notifs() -> Result<bool, String> {
    let client = NOSTR_CLIENT.get().expect("Nostr client not initialized");

    // Grab our pubkey
    let signer = client.signer().await.map_err(|e| e.to_string())?;
    let pubkey = signer.get_public_key().await.map_err(|e| e.to_string())?;

    // Live GiftWraps to us (DMs, files, MLS welcomes)
    let giftwrap_filter = Filter::new()
        .pubkey(pubkey)
        .kind(Kind::GiftWrap)
        .limit(0);

    // Live MLS group wrappers (Kind::MlsGroupMessage). Broad subscribe; we'll filter by membership in handler.
    let mls_msg_filter = Filter::new()
        .kind(Kind::MlsGroupMessage)
        .limit(0);

    // Subscribe to both filters
    let gift_sub_id = match client.subscribe(giftwrap_filter, None).await {
        Ok(id) => id.val,
        Err(e) => return Err(e.to_string()),
    };
    let mls_sub_id = match client.subscribe(mls_msg_filter, None).await {
        Ok(id) => id.val,
        Err(e) => return Err(e.to_string()),
    };

    // Begin watching for notifications from our subscriptions
    match client
        .handle_notifications(|notification| async {
            if let RelayPoolNotification::Event { event, subscription_id, .. } = notification {
                if subscription_id == gift_sub_id {
                    // Handle DMs/files/vector-specific + MLS welcomes inside giftwrap
                    handle_event(*event, true).await;
                } else if subscription_id == mls_sub_id {
                    // Handle live MLS group message wrappers
                    let ev = (*event).clone();

                    // Extract group wire id from 'h' tag
                    let group_wire_id_opt = ev
                        .tags
                        .find(TagKind::SingleLetter(SingleLetterTag::lowercase(Alphabet::H)))
                        .and_then(|t| t.content().map(|s| s.to_string()));

                    if let Some(group_wire_id) = group_wire_id_opt {
                        // Check if we are a member of this group (metadata check) without constructing MLS engine
                        let handle = TAURI_APP.get().unwrap().clone();
                        let is_member: bool = if let Ok(groups) = db::load_mls_groups(&handle).await {
                            groups.iter().any(|g| {
                                g.group_id == group_wire_id || g.engine_group_id == group_wire_id
                            })
                        } else { false };

                        // Not a member - ignore this group message
                        if !is_member {
                            return Ok(false);
                        }
                        
                        // Resolve my pubkey for filtering and 'mine' flag
                        let (my_pubkey, my_pubkey_bech32) = {
                            let client = NOSTR_CLIENT.get().unwrap();
                            if let Ok(signer) = client.signer().await {
                                if let Ok(pk) = signer.get_public_key().await {
                                    (Some(pk), pk.to_bech32().unwrap())
                                } else {
                                    (None, String::new())
                                }
                            } else {
                                (None, String::new())
                            }
                        };
                        
                        // Skip processing our own events - they're already processed locally when sent
                        if let Some(my_pk) = my_pubkey {
                            if ev.pubkey == my_pk {
                                return Ok(false);
                            }
                        }

                        // Process with non-Send MLS engine on a blocking thread (no awaits in scope)
                        let app_handle = TAURI_APP.get().unwrap().clone();
                        let my_npub_for_block = my_pubkey_bech32.clone();
                        let group_id_for_persist = group_wire_id.clone();
                        let group_id_for_emit = group_wire_id.clone();
                        
                        // Process message and persist in one blocking operation to avoid Send issues
                        let emit_record = tokio::task::spawn_blocking(move || {
                            // Use runtime handle to drive async operations from blocking context
                            let rt = tokio::runtime::Handle::current();
                            
                            // Create MLS service and process message
                            let svc = MlsService::new_persistent(&app_handle).ok()?;
                            let engine = svc.engine().ok()?;

                            match engine.process_message(&ev) {
                                Ok(res) => {
                                    // Use unified storage via process_rumor
                                    match res {
                                        mdk_core::prelude::MessageProcessingResult::ApplicationMessage(msg) => {
                                            // Convert to RumorEvent for protocol-agnostic processing
                                            let rumor_event = crate::rumor::RumorEvent {
                                                id: msg.id,
                                                kind: msg.kind,
                                                content: msg.content.clone(),
                                                tags: msg.tags.clone(),
                                                created_at: msg.created_at,
                                                pubkey: msg.pubkey,
                                            };
    
                                            let is_mine = !my_npub_for_block.is_empty() && msg.pubkey.to_bech32().unwrap() == my_npub_for_block;
    
                                            // Process through unified rumor processor
                                            let processed = rt.block_on(async {
                                                use crate::rumor::{process_rumor, RumorContext, ConversationType, RumorProcessingResult};
                                                
                                                let rumor_context = RumorContext {
                                                    sender: msg.pubkey,
                                                    is_mine,
                                                    conversation_id: group_id_for_persist.clone(),
                                                    conversation_type: ConversationType::MlsGroup,
                                                };
                                                
                                                match process_rumor(rumor_event, rumor_context).await {
                                                    Ok(result) => {
                                                        match result {
                                                            RumorProcessingResult::TextMessage(mut message) => {
                                                                // Populate reply context for old messages not in frontend cache
                                                                if !message.replied_to.is_empty() {
                                                                    if let Some(handle) = TAURI_APP.get() {
                                                                        let _ = db::populate_reply_context(&handle, &mut message).await;
                                                                    }
                                                                }

                                                                // Clear typing indicator for this sender (they just sent a message)
                                                                let sender_npub = msg.pubkey.to_bech32().unwrap_or_default();

                                                                let (was_added, _active_typers, should_notify) = {
                                                                    let mut state = crate::STATE.lock().await;

                                                                    // Add message to chat
                                                                    let added = state.add_message_to_chat(&group_id_for_persist, message.clone());
                                                                    
                                                                    // Check if we should send notification (not muted, not mine)
                                                                    let notify = if let Some(chat) = state.get_chat(&group_id_for_persist) {
                                                                        !chat.muted && !message.mine
                                                                    } else {
                                                                        false
                                                                    };
                                                                    
                                                                    // Clear typing indicator for sender
                                                                    let typers = if let Some(chat) = state.get_chat_mut(&group_id_for_persist) {
                                                                        chat.update_typing_participant(sender_npub.clone(), 0); // 0 = clear immediately
                                                                        chat.get_active_typers()
                                                                    } else {
                                                                        Vec::new()
                                                                    };
                                                                    
                                                                    (added, typers, notify)
                                                                };
                                                                
                                                                // Send OS notification for new group messages
                                                                if was_added && should_notify {
                                                                    // Get sender name and group name for notification
                                                                    let (sender_name, group_name) = {
                                                                        let state = crate::STATE.lock().await;
                                                                        
                                                                        let sender = if let Some(profile) = state.get_profile(&sender_npub) {
                                                                            if !profile.nickname.is_empty() {
                                                                                profile.nickname.clone()
                                                                            } else if !profile.name.is_empty() {
                                                                                profile.name.clone()
                                                                            } else {
                                                                                "Someone".to_string()
                                                                            }
                                                                        } else {
                                                                            "Someone".to_string()
                                                                        };
                                                                        
                                                                        let group = if let Some(chat) = state.get_chat(&group_id_for_persist) {
                                                                            chat.metadata.get_name().unwrap_or("Group Chat").to_string()
                                                                        } else {
                                                                            "Group Chat".to_string()
                                                                        };
                                                                        
                                                                        (sender, group)
                                                                    };
                                                                    
                                                                    // Create notification for text message
                                                                    let notification = NotificationData::group_message(sender_name, group_name, message.content.clone());
                                                                    show_notification_generic(notification);
                                                                }
                                                                
                                                                // Save to database if message was added
                                                                if was_added {
                                                                    if let Some(handle) = TAURI_APP.get() {
                                                                        // Get chat and save it
                                                                        let chat_to_save = {
                                                                            let state = crate::STATE.lock().await;
                                                                            state.get_chat(&group_id_for_persist).cloned()
                                                                        };
                                                                        
                                                                        if let Some(chat) = chat_to_save {
                                                                            use crate::db::{save_chat, save_chat_messages};
                                                                            let _ = save_chat(handle.clone(), &chat).await;
                                                                            let _ = save_chat_messages(handle.clone(), &group_id_for_persist, &chat.messages).await;
                                                                        }
                                                                    }
                                                                    Some(message)
                                                                } else {
                                                                    None
                                                                }
                                                            }
                                                            RumorProcessingResult::FileAttachment(mut message) => {
                                                                // Populate reply context for old messages not in frontend cache
                                                                if !message.replied_to.is_empty() {
                                                                    if let Some(handle) = TAURI_APP.get() {
                                                                        let _ = db::populate_reply_context(&handle, &mut message).await;
                                                                    }
                                                                }

                                                                // Clear typing indicator for this sender (they just sent a message)
                                                                let sender_npub = msg.pubkey.to_bech32().unwrap_or_default();
                                                                let is_file = true;

                                                                let (was_added, _active_typers, should_notify) = {
                                                                    let mut state = crate::STATE.lock().await;

                                                                    // Add message to chat
                                                                    let added = state.add_message_to_chat(&group_id_for_persist, message.clone());
                                                                    
                                                                    // Check if we should send notification (not muted, not mine)
                                                                    let notify = if let Some(chat) = state.get_chat(&group_id_for_persist) {
                                                                        !chat.muted && !message.mine
                                                                    } else {
                                                                        false
                                                                    };
                                                                    
                                                                    // Clear typing indicator for sender
                                                                    let typers = if let Some(chat) = state.get_chat_mut(&group_id_for_persist) {
                                                                        chat.update_typing_participant(sender_npub.clone(), 0); // 0 = clear immediately
                                                                        chat.get_active_typers()
                                                                    } else {
                                                                        Vec::new()
                                                                    };
                                                                    
                                                                    (added, typers, notify)
                                                                };
                                                                
                                                                // Send OS notification for new group messages
                                                                if was_added && should_notify {
                                                                    // Get sender name and group name for notification
                                                                    let (sender_name, group_name) = {
                                                                        let state = crate::STATE.lock().await;
                                                                        
                                                                        let sender = if let Some(profile) = state.get_profile(&sender_npub) {
                                                                            if !profile.nickname.is_empty() {
                                                                                profile.nickname.clone()
                                                                            } else if !profile.name.is_empty() {
                                                                                profile.name.clone()
                                                                            } else {
                                                                                "Someone".to_string()
                                                                            }
                                                                        } else {
                                                                            "Someone".to_string()
                                                                        };
                                                                        
                                                                        let group = if let Some(chat) = state.get_chat(&group_id_for_persist) {
                                                                            chat.metadata.get_name().unwrap_or("Group Chat").to_string()
                                                                        } else {
                                                                            "Group Chat".to_string()
                                                                        };
                                                                        
                                                                        (sender, group)
                                                                    };
                                                                    
                                                                    // Create appropriate notification (both text and files use group_message)
                                                                    let content = if is_file {
                                                                        let extension = message.attachments.first()
                                                                            .map(|att| att.extension.clone())
                                                                            .unwrap_or_else(|| String::from("file"));
                                                                        "Sent a ".to_string() + &get_file_type_description(&extension)
                                                                    } else {
                                                                        message.content.clone()
                                                                    };
                                                                    let notification = NotificationData::group_message(sender_name, group_name, content);
                                                                    
                                                                    show_notification_generic(notification);
                                                                }
                                                                
                                                                // Save to database if message was added
                                                                if was_added {
                                                                    if let Some(handle) = TAURI_APP.get() {
                                                                        // Get chat and save it
                                                                        let chat_to_save = {
                                                                            let state = crate::STATE.lock().await;
                                                                            state.get_chat(&group_id_for_persist).cloned()
                                                                        };
                                                                        
                                                                        if let Some(chat) = chat_to_save {
                                                                            use crate::db::save_chat;
                                                                            let _ = save_chat(handle.clone(), &chat).await;
                                                                            let _ = db::save_message(handle.clone(), &group_id_for_persist, &message).await;
                                                                        }
                                                                    }
                                                                    Some(message)
                                                                } else {
                                                                    None
                                                                }
                                                            }
                                                            RumorProcessingResult::Reaction(reaction) => {
                                                                // Handle reactions in real-time
                                                                let (was_added, chat_id_for_save) = {
                                                                    let mut state = crate::STATE.lock().await;
                                                                    let added = if let Some((chat_id, msg)) = state.find_chat_and_message_mut(&reaction.reference_id) {
                                                                        msg.add_reaction(reaction.clone(), Some(chat_id))
                                                                    } else {
                                                                        false
                                                                    };
                                                                    
                                                                    // Get chat_id for saving if reaction was added
                                                                    let chat_id_for_save = if added {
                                                                        state.find_message(&reaction.reference_id)
                                                                            .map(|(chat, _)| chat.id().clone())
                                                                    } else {
                                                                        None
                                                                    };
                                                                    
                                                                    (added, chat_id_for_save)
                                                                };
                                                                
                                                                // Save the updated message to database immediately (like DM reactions)
                                                                if was_added {
                                                                    if let Some(chat_id) = chat_id_for_save {
                                                                        if let Some(handle) = TAURI_APP.get() {
                                                                            let updated_message = {
                                                                                let state = crate::STATE.lock().await;
                                                                                state.find_message(&reaction.reference_id)
                                                                                    .map(|(_, msg)| msg.clone())
                                                                            };
                                                                            
                                                                            if let Some(msg) = updated_message {
                                                                                let _ = db::save_message(handle.clone(), &chat_id, &msg).await;
                                                                            }
                                                                        }
                                                                    }
                                                                }
                                                                
                                                                None // Don't emit as message
                                                            }
                                                            RumorProcessingResult::TypingIndicator { profile_id, until } => {
                                                                // Handle typing indicators in real-time
                                                                let active_typers = {
                                                                    let mut state = crate::STATE.lock().await;
                                                                    if let Some(chat) = state.get_chat_mut(&group_id_for_persist) {
                                                                        chat.update_typing_participant(profile_id.clone(), until);
                                                                        chat.get_active_typers()
                                                                    } else {
                                                                        Vec::new()
                                                                    }
                                                                };
                                                                
                                                                // Emit typing update event
                                                                if let Some(handle) = TAURI_APP.get() {
                                                                    let _ = handle.emit("typing-update", serde_json::json!({
                                                                        "conversation_id": group_id_for_persist,
                                                                        "typers": active_typers
                                                                    }));
                                                                }
                                                                
                                                                None // Don't emit as message
                                                            }
                                                            RumorProcessingResult::WebxdcPeerAdvertisement { topic_id, node_addr } => {
                                                                // Handle WebXDC peer advertisement - add peer to realtime channel
                                                                handle_webxdc_peer_advertisement(&topic_id, &node_addr).await;
                                                                None // Don't emit as message
                                                            }
                                                            RumorProcessingResult::UnknownEvent(mut event) => {
                                                                // Store unknown events for future compatibility
                                                                // Get chat_id and save the event
                                                                if let Some(handle) = TAURI_APP.get() {
                                                                    if let Ok(chat_id) = db::get_chat_id_by_identifier(handle, &group_id_for_persist) {
                                                                        event.chat_id = chat_id;
                                                                        let _ = db::save_event(handle, &event).await;
                                                                    }
                                                                }
                                                                None // Don't emit as message
                                                            }
                                                            RumorProcessingResult::Ignored => None,
                                                            RumorProcessingResult::PivxPayment { gift_code, amount_piv, address, message_id, event } => {
                                                                // Save PIVX payment event and emit to frontend
                                                                if let Some(handle) = TAURI_APP.get() {
                                                                    let event_timestamp = event.created_at;
                                                                    let _ = db::save_pivx_payment_event(handle, &group_id_for_persist, event).await;

                                                                    let sender_npub = msg.pubkey.to_bech32().unwrap_or_default();
                                                                    let _ = handle.emit("pivx_payment_received", serde_json::json!({
                                                                        "conversation_id": group_id_for_persist,
                                                                        "gift_code": gift_code,
                                                                        "amount_piv": amount_piv,
                                                                        "address": address,
                                                                        "message_id": message_id,
                                                                        "sender": sender_npub,
                                                                        "is_mine": is_mine,
                                                                        "at": event_timestamp * 1000,
                                                                    }));
                                                                }
                                                                None // Don't emit as message
                                                            }
                                                            RumorProcessingResult::Edit { message_id, new_content, edited_at, event } => {
                                                                // Skip if this edit event was already processed (deduplication)
                                                                if let Some(handle) = TAURI_APP.get() {
                                                                    if db::event_exists(handle, &event.id).unwrap_or(false) {
                                                                        return None; // Already processed, skip
                                                                    }

                                                                    // Save edit event to database
                                                                    if let Ok(chat_id) = db::get_chat_id_by_identifier(handle, &group_id_for_persist) {
                                                                        let mut event_with_chat = event;
                                                                        event_with_chat.chat_id = chat_id;
                                                                        let _ = db::save_event(handle, &event_with_chat).await;
                                                                    }
                                                                }

                                                                // Update message in state and emit to frontend
                                                                let mut state = crate::STATE.lock().await;
                                                                if let Some(chat) = state.get_chat_mut(&group_id_for_persist) {
                                                                    if let Some(msg) = chat.get_message_mut(&message_id) {
                                                                        msg.apply_edit(new_content, edited_at);

                                                                        // Emit update to frontend
                                                                        if let Some(handle) = TAURI_APP.get() {
                                                                            let _ = handle.emit("message_update", serde_json::json!({
                                                                                "old_id": &message_id,
                                                                                "message": &msg,
                                                                                "chat_id": &group_id_for_persist
                                                                            }));
                                                                        }
                                                                    }
                                                                }
                                                                None // Don't emit as message
                                                            }
                                                        }
                                                    }
                                                    Err(e) => {
                                                        eprintln!("[MLS][live] Failed to process rumor: {}", e);
                                                        None
                                                    }
                                                }
                                            });
    
                                            processed
                                        }
                                        mdk_core::prelude::MessageProcessingResult::Commit { mls_group_id } => {
                                            // Commit processed - member list may have changed
                                            // Check if we're still a member of this group
                                            let my_pubkey_hex = my_npub_for_block.clone();
                                            
                                            // Only evict if we can POSITIVELY CONFIRM removal
                                            let membership_check = engine.get_members(&mls_group_id)
                                                .ok()
                                                .and_then(|members| {
                                                    nostr_sdk::PublicKey::from_bech32(&my_pubkey_hex)
                                                        .ok()
                                                        .map(|pk| members.contains(&pk))
                                                });
                                            
                                            match membership_check {
                                                Some(false) => {
                                                    // Successfully checked and confirmed NOT a member - evict!
                                                    eprintln!("[MLS] Eviction detected via Commit - group: {}", group_id_for_persist);
                                                    
                                                    // Perform full cleanup using the helper method
                                                    rt.block_on(async {
                                                        if let Err(e) = svc.cleanup_evicted_group(&group_id_for_persist).await {
                                                            eprintln!("[MLS] Failed to cleanup evicted group: {}", e);
                                                        }
                                                    });
                                                }
                                                Some(true) => {
                                                    // Still a member, just update the UI
                                                    if let Some(handle) = TAURI_APP.get() {
                                                        handle.emit("mls_group_updated", serde_json::json!({
                                                            "group_id": group_id_for_persist
                                                        })).ok();
                                                    }
                                                }
                                                None => {
                                                    // Check failed - don't evict, just update UI
                                                    if let Some(handle) = TAURI_APP.get() {
                                                        handle.emit("mls_group_updated", serde_json::json!({
                                                            "group_id": group_id_for_persist
                                                        })).ok();
                                                    }
                                                }
                                            }
                                            None
                                        }
                                        mdk_core::prelude::MessageProcessingResult::Proposal(_proposal) => {
                                            // Proposal received (e.g., leave proposal)
                                            // Emit event to notify UI that group state may have changed
                                            if let Some(handle) = TAURI_APP.get() {
                                                handle.emit("mls_group_updated", serde_json::json!({
                                                    "group_id": group_id_for_persist
                                                })).ok();
                                            }
                                            None
                                        }
                                        mdk_core::prelude::MessageProcessingResult::Unprocessable { mls_group_id: _ } => {
                                            // Unprocessable result - could be many reasons (out of order, can't decrypt, etc.)
                                            // Don't try to detect eviction here - wait for next message to trigger error-based detection
                                            None
                                        }
                                        // Other message types (ExternalJoinProposal) are not persisted as chat messages
                                        _ => None,
                                    }
                                }
                                Err(e) => {
                                    let error_msg = e.to_string();
                                    
                                    // Check if this is an eviction error
                                    if error_msg.contains("evicted from it") ||
                                       error_msg.contains("after being evicted") ||
                                       error_msg.contains("own leaf not found") {
                                        eprintln!("[MLS] Eviction detected in live subscription - group: {}", group_id_for_persist);
                                        
                                        // Perform full cleanup using the helper method
                                        rt.block_on(async {
                                            if let Err(e) = svc.cleanup_evicted_group(&group_id_for_persist).await {
                                                eprintln!("[MLS] Failed to cleanup evicted group: {}", e);
                                            }
                                        });
                                    } else if !error_msg.contains("group not found") {
                                        eprintln!("[MLS] live process_message failed (id={}): {}", ev.id, error_msg);
                                    }
                                    None
                                }
                            }
                        })
                        .await
                        .unwrap_or(None);

                        if let Some(record) = emit_record {
                            // Emit UI event (no MLS operations here, just event emission)
                            let _ = handle.emit("mls_message_new", serde_json::json!({
                                "group_id": group_id_for_emit,
                                "message": record
                            }));
                        }
                    }
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

/// Default relays that come pre-configured
pub(crate) const DEFAULT_RELAYS: &[&str] = &[
    "wss://jskitty.cat/nostr",        // TRUSTED_RELAY
    "wss://asia.vectorapp.io/nostr",  // TRUSTED_RELAY
    "wss://nostr.computingcache.com", // TRUSTED_RELAY
    "wss://relay.damus.io",
];

/// Check if a URL is a default relay
fn is_default_relay(url: &str) -> bool {
    let normalized = url.trim().trim_end_matches('/').to_lowercase();
    DEFAULT_RELAYS.iter().any(|r| r.to_lowercase() == normalized)
}

// ============================================================================
// Relay Metrics & Logging
// ============================================================================

use std::collections::{HashMap, VecDeque};
use std::sync::RwLock;
use once_cell::sync::Lazy;

/// Metrics tracked per relay
#[derive(serde::Serialize, Clone, Debug)]
pub struct RelayMetrics {
    pub ping_ms: Option<u64>,
    pub bytes_up: u64,
    pub bytes_down: u64,
    pub last_check: Option<u64>,  // Unix timestamp
    pub events_received: u64,
    pub events_sent: u64,
}

impl Default for RelayMetrics {
    fn default() -> Self {
        Self {
            ping_ms: None,
            bytes_up: 0,
            bytes_down: 0,
            last_check: None,
            events_received: 0,
            events_sent: 0,
        }
    }
}

/// A single log entry for a relay
#[derive(serde::Serialize, Clone, Debug)]
pub struct RelayLog {
    pub timestamp: u64,  // Unix timestamp
    pub level: String,   // "info", "warn", "error"
    pub message: String,
}

/// Global storage for relay metrics
static RELAY_METRICS: Lazy<RwLock<HashMap<String, RelayMetrics>>> =
    Lazy::new(|| RwLock::new(HashMap::new()));

/// Global storage for relay logs (max 10 per relay)
static RELAY_LOGS: Lazy<RwLock<HashMap<String, VecDeque<RelayLog>>>> =
    Lazy::new(|| RwLock::new(HashMap::new()));

/// Add a log entry for a relay
fn add_relay_log(url: &str, level: &str, message: &str) {
    let normalized = url.trim().trim_end_matches('/').to_lowercase();
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let log = RelayLog {
        timestamp,
        level: level.to_string(),
        message: message.to_string(),
    };

    if let Ok(mut logs) = RELAY_LOGS.write() {
        let relay_logs = logs.entry(normalized).or_insert_with(VecDeque::new);
        relay_logs.push_front(log);
        // Keep only last 10 logs
        while relay_logs.len() > 10 {
            relay_logs.pop_back();
        }
    }
}

/// Update metrics for a relay
fn update_relay_metrics(url: &str, update_fn: impl FnOnce(&mut RelayMetrics)) {
    let normalized = url.trim().trim_end_matches('/').to_lowercase();
    if let Ok(mut metrics) = RELAY_METRICS.write() {
        let relay_metrics = metrics.entry(normalized).or_insert_with(RelayMetrics::default);
        update_fn(relay_metrics);
    }
}

/// Get metrics for a relay
#[tauri::command]
async fn get_relay_metrics(url: String) -> Result<RelayMetrics, String> {
    let normalized = url.trim().trim_end_matches('/').to_lowercase();
    let metrics = RELAY_METRICS.read()
        .map_err(|_| "Failed to read metrics")?
        .get(&normalized)
        .cloned()
        .unwrap_or_default();
    Ok(metrics)
}

/// Get logs for a relay
#[tauri::command]
async fn get_relay_logs(url: String) -> Result<Vec<RelayLog>, String> {
    let normalized = url.trim().trim_end_matches('/').to_lowercase();
    let logs = RELAY_LOGS.read()
        .map_err(|_| "Failed to read logs")?
        .get(&normalized)
        .map(|l| l.iter().cloned().collect())
        .unwrap_or_default();
    Ok(logs)
}

// ============================================================================

#[derive(serde::Serialize)]
struct RelayInfo {
    url: String,
    status: String,
    is_default: bool,
    is_custom: bool,
    enabled: bool,
    mode: String,
}

/// Get all relays with their current status
#[tauri::command]
async fn get_relays<R: Runtime>(handle: AppHandle<R>) -> Result<Vec<RelayInfo>, String> {
    let client = NOSTR_CLIENT.get().ok_or("Nostr client not initialized")?;

    // Get custom relays from DB
    let custom_relays = get_custom_relays(handle.clone()).await.unwrap_or_default();
    let disabled_defaults = get_disabled_default_relays(&handle).await.unwrap_or_default();

    // Get all connected relays from client pool
    let pool_relays = client.relays().await;

    let mut relay_infos: Vec<RelayInfo> = Vec::new();

    // First, add all default relays (even if disabled)
    for default_url in DEFAULT_RELAYS {
        let url_str = default_url.to_string();
        let is_disabled = disabled_defaults.iter().any(|d| d.to_lowercase() == url_str.to_lowercase());

        // Check if this relay is in the pool
        let (status, mode) = if let Some((_, relay)) = pool_relays.iter().find(|(u, _)| u.to_string().to_lowercase() == url_str.to_lowercase()) {
            let status = match relay.status() {
                RelayStatus::Initialized => "initialized",
                RelayStatus::Pending => "pending",
                RelayStatus::Connecting => "connecting",
                RelayStatus::Connected => "connected",
                RelayStatus::Disconnected => "disconnected",
                RelayStatus::Terminated => "terminated",
                RelayStatus::Banned => "banned",
                RelayStatus::Sleeping => "sleeping",
            };
            (status.to_string(), "both".to_string())
        } else {
            ("disabled".to_string(), "both".to_string())
        };

        relay_infos.push(RelayInfo {
            url: url_str,
            status,
            is_default: true,
            is_custom: false,
            enabled: !is_disabled,
            mode,
        });
    }

    // Then add custom relays
    for custom in &custom_relays {
        // Check if this relay is in the pool
        let status = if let Some((_, relay)) = pool_relays.iter().find(|(u, _)| u.to_string().to_lowercase() == custom.url.to_lowercase()) {
            match relay.status() {
                RelayStatus::Initialized => "initialized",
                RelayStatus::Pending => "pending",
                RelayStatus::Connecting => "connecting",
                RelayStatus::Connected => "connected",
                RelayStatus::Disconnected => "disconnected",
                RelayStatus::Terminated => "terminated",
                RelayStatus::Banned => "banned",
                RelayStatus::Sleeping => "sleeping",
            }.to_string()
        } else {
            "disabled".to_string()
        };

        relay_infos.push(RelayInfo {
            url: custom.url.clone(),
            status,
            is_default: false,
            is_custom: true,
            enabled: custom.enabled,
            mode: custom.mode.clone(),
        });
    }

    Ok(relay_infos)
}

/// Get the list of Blossom media servers (Tauri command)
#[tauri::command]
async fn get_media_servers() -> Vec<String> {
    get_blossom_servers()
}

// ============================================================================
// Custom Relay Management
// ============================================================================

/// Saved custom relay entry with optional metadata
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
struct CustomRelay {
    url: String,
    enabled: bool,
    #[serde(default = "default_relay_mode")]
    mode: String,  // "read" | "write" | "both"
}

fn default_relay_mode() -> String {
    "both".to_string()
}

/// Validate a relay URL format (must be wss://)
fn validate_relay_url(url: &str) -> Result<String, String> {
    let trimmed = url.trim();

    // Must start with wss:// (secure WebSocket only)
    if !trimmed.starts_with("wss://") {
        return Err("Relay URL must start with wss://".to_string());
    }

    // Basic URL validation - must have host after protocol
    let after_protocol = &trimmed[6..];
    if after_protocol.is_empty() {
        return Err("Relay URL must include a host".to_string());
    }

    // Don't allow trailing slashes for consistency
    let normalized = trimmed.trim_end_matches('/');

    Ok(normalized.to_string())
}

/// Get the list of custom relays from settings
#[tauri::command]
async fn get_custom_relays<R: Runtime>(handle: AppHandle<R>) -> Result<Vec<CustomRelay>, String> {
    // Check if an account is selected
    if crate::account_manager::get_current_account().is_err() {
        return Ok(vec![]);
    }

    let conn = crate::account_manager::get_db_connection(&handle)?;

    let result: Option<String> = conn.query_row(
        "SELECT value FROM settings WHERE key = ?1",
        rusqlite::params!["custom_relays"],
        |row| row.get(0)
    ).ok();

    crate::account_manager::return_db_connection(conn);

    match result {
        Some(json_str) => {
            serde_json::from_str(&json_str)
                .map_err(|e| format!("Failed to parse custom relays: {}", e))
        }
        None => Ok(vec![])
    }
}

/// Save the list of custom relays to settings
async fn save_custom_relays<R: Runtime>(handle: &AppHandle<R>, relays: &[CustomRelay]) -> Result<(), String> {
    if crate::account_manager::get_current_account().is_err() {
        return Err("No account selected".to_string());
    }

    let json_str = serde_json::to_string(relays)
        .map_err(|e| format!("Failed to serialize relays: {}", e))?;

    let conn = crate::account_manager::get_db_connection(handle)?;

    conn.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
        rusqlite::params!["custom_relays", json_str],
    ).map_err(|e| format!("Failed to save custom relays: {}", e))?;

    crate::account_manager::return_db_connection(conn);
    Ok(())
}

/// Get the list of disabled default relays from settings
async fn get_disabled_default_relays<R: Runtime>(handle: &AppHandle<R>) -> Result<Vec<String>, String> {
    if crate::account_manager::get_current_account().is_err() {
        return Ok(vec![]);
    }

    let conn = crate::account_manager::get_db_connection(handle)?;

    let result: Option<String> = conn.query_row(
        "SELECT value FROM settings WHERE key = ?1",
        rusqlite::params!["disabled_default_relays"],
        |row| row.get(0)
    ).ok();

    crate::account_manager::return_db_connection(conn);

    match result {
        Some(json_str) => {
            serde_json::from_str(&json_str)
                .map_err(|e| format!("Failed to parse disabled default relays: {}", e))
        }
        None => Ok(vec![])
    }
}

/// Save the list of disabled default relays to settings
async fn save_disabled_default_relays<R: Runtime>(handle: &AppHandle<R>, relays: &[String]) -> Result<(), String> {
    if crate::account_manager::get_current_account().is_err() {
        return Err("No account selected".to_string());
    }

    let json_str = serde_json::to_string(relays)
        .map_err(|e| format!("Failed to serialize disabled relays: {}", e))?;

    let conn = crate::account_manager::get_db_connection(handle)?;

    conn.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
        rusqlite::params!["disabled_default_relays", json_str],
    ).map_err(|e| format!("Failed to save disabled default relays: {}", e))?;

    crate::account_manager::return_db_connection(conn);
    Ok(())
}

/// Toggle a default relay's enabled state
#[tauri::command]
async fn toggle_default_relay<R: Runtime>(handle: AppHandle<R>, url: String, enabled: bool) -> Result<bool, String> {
    // Verify it's actually a default relay
    if !is_default_relay(&url) {
        return Err("Not a default relay".to_string());
    }

    let normalized_url = url.trim().trim_end_matches('/').to_string();
    let mut disabled = get_disabled_default_relays(&handle).await?;

    if enabled {
        // Remove from disabled list
        disabled.retain(|d| d.to_lowercase() != normalized_url.to_lowercase());
    } else {
        // Add to disabled list if not already there
        if !disabled.iter().any(|d| d.to_lowercase() == normalized_url.to_lowercase()) {
            disabled.push(normalized_url.clone());
        }
    }

    save_disabled_default_relays(&handle, &disabled).await?;

    // Update the relay pool
    if let Some(client) = NOSTR_CLIENT.get() {
        if enabled {
            // Re-add the relay
            match client.pool().add_relay(&normalized_url, RelayOptions::new().reconnect(false)).await {
                Ok(_) => {
                    let _ = client.pool().connect_relay(&normalized_url).await;
                    println!("[Relay] Enabled default relay: {}", normalized_url);
                }
                Err(e) => eprintln!("[Relay] Failed to enable default relay: {}", e),
            }
        } else {
            // Remove the relay from pool
            if let Err(e) = client.pool().remove_relay(&normalized_url).await {
                eprintln!("[Relay] Note: Could not disable default relay in pool: {}", e);
            } else {
                println!("[Relay] Disabled default relay: {}", normalized_url);
            }
        }
    }

    Ok(true)
}

/// Helper to build RelayOptions based on mode
fn relay_options_for_mode(mode: &str) -> RelayOptions {
    let opts = RelayOptions::new().reconnect(false);
    match mode {
        "read" => opts.write(false),
        "write" => opts.read(false),
        _ => opts, // "both" - default read and write enabled
    }
}

/// Add a custom relay URL
#[tauri::command]
async fn add_custom_relay<R: Runtime>(handle: AppHandle<R>, url: String, mode: Option<String>) -> Result<CustomRelay, String> {
    // Validate and normalize the URL
    let normalized_url = validate_relay_url(&url)?;

    // Validate mode
    let relay_mode = mode.unwrap_or_else(|| "both".to_string());
    if !["read", "write", "both"].contains(&relay_mode.as_str()) {
        return Err("Invalid mode. Must be 'read', 'write', or 'both'".to_string());
    }

    // Get existing relays
    let mut relays = get_custom_relays(handle.clone()).await?;

    // Check for duplicates (case-insensitive)
    let url_lower = normalized_url.to_lowercase();
    if relays.iter().any(|r| r.url.to_lowercase() == url_lower) {
        return Err("Relay already exists".to_string());
    }

    // Don't allow adding default relays as custom
    if is_default_relay(&normalized_url) {
        return Err("Cannot add default relay as custom relay".to_string());
    }

    // Create new relay entry
    let new_relay = CustomRelay {
        url: normalized_url,
        enabled: true,
        mode: relay_mode.clone(),
    };

    relays.push(new_relay.clone());

    // Save to settings
    save_custom_relays(&handle, &relays).await?;

    // If we're already connected, add this relay to the pool immediately
    if let Some(client) = NOSTR_CLIENT.get() {
        if client.relays().await.len() > 0 {
            match client.pool().add_relay(&new_relay.url, relay_options_for_mode(&relay_mode)).await {
                Ok(_) => {
                    println!("[Relay] Added custom relay to pool: {} (mode: {})", new_relay.url, relay_mode);
                    // Connect to the new relay
                    if let Err(e) = client.pool().connect_relay(&new_relay.url).await {
                        eprintln!("[Relay] Failed to connect to new relay: {}", e);
                    }
                }
                Err(e) => eprintln!("[Relay] Failed to add relay to pool: {}", e),
            }
        }
    }

    Ok(new_relay)
}

/// Remove a custom relay URL
#[tauri::command]
async fn remove_custom_relay<R: Runtime>(handle: AppHandle<R>, url: String) -> Result<bool, String> {
    let mut relays = get_custom_relays(handle.clone()).await?;

    let url_lower = url.to_lowercase();
    let original_len = relays.len();
    relays.retain(|r| r.url.to_lowercase() != url_lower);

    if relays.len() == original_len {
        return Ok(false); // Relay not found
    }

    // Save updated list
    save_custom_relays(&handle, &relays).await?;

    // Remove from active pool if connected
    if let Some(client) = NOSTR_CLIENT.get() {
        if let Err(e) = client.pool().remove_relay(&url).await {
            // Log but don't fail - relay might not be in pool
            eprintln!("[Relay] Note: Could not remove relay from pool: {}", e);
        } else {
            println!("[Relay] Removed custom relay from pool: {}", url);
        }
    }

    Ok(true)
}

/// Toggle a custom relay's enabled state
#[tauri::command]
async fn toggle_custom_relay<R: Runtime>(handle: AppHandle<R>, url: String, enabled: bool) -> Result<bool, String> {
    let mut relays = get_custom_relays(handle.clone()).await?;

    let url_lower = url.to_lowercase();
    let mut found = false;
    let mut relay_mode = "both".to_string();

    for relay in relays.iter_mut() {
        if relay.url.to_lowercase() == url_lower {
            relay.enabled = enabled;
            relay_mode = relay.mode.clone();
            found = true;
            break;
        }
    }

    if !found {
        return Err("Relay not found".to_string());
    }

    // Save updated list
    save_custom_relays(&handle, &relays).await?;

    // Update the relay pool
    if let Some(client) = NOSTR_CLIENT.get() {
        if enabled {
            // Add and connect with proper mode
            match client.pool().add_relay(&url, relay_options_for_mode(&relay_mode)).await {
                Ok(_) => {
                    let _ = client.pool().connect_relay(&url).await;
                    println!("[Relay] Enabled custom relay: {} (mode: {})", url, relay_mode);
                }
                Err(e) => eprintln!("[Relay] Failed to enable relay: {}", e),
            }
        } else {
            // Disconnect and remove
            if let Err(e) = client.pool().remove_relay(&url).await {
                eprintln!("[Relay] Note: Could not disable relay in pool: {}", e);
            } else {
                println!("[Relay] Disabled custom relay: {}", url);
            }
        }
    }

    Ok(true)
}

/// Update a custom relay's mode (read/write/both)
#[tauri::command]
async fn update_relay_mode<R: Runtime>(handle: AppHandle<R>, url: String, mode: String) -> Result<bool, String> {
    // Validate mode
    if !["read", "write", "both"].contains(&mode.as_str()) {
        return Err("Invalid mode. Must be 'read', 'write', or 'both'".to_string());
    }

    let mut relays = get_custom_relays(handle.clone()).await?;

    let url_lower = url.to_lowercase();
    let mut found = false;
    let mut is_enabled = false;

    for relay in relays.iter_mut() {
        if relay.url.to_lowercase() == url_lower {
            relay.mode = mode.clone();
            is_enabled = relay.enabled;
            found = true;
            break;
        }
    }

    if !found {
        return Err("Relay not found".to_string());
    }

    // Save updated list
    save_custom_relays(&handle, &relays).await?;

    // If relay is currently enabled, reconnect with new mode
    if is_enabled {
        if let Some(client) = NOSTR_CLIENT.get() {
            // Remove and re-add with new options
            let _ = client.pool().remove_relay(&url).await;
            match client.pool().add_relay(&url, relay_options_for_mode(&mode)).await {
                Ok(_) => {
                    let _ = client.pool().connect_relay(&url).await;
                    println!("[Relay] Updated relay mode: {} -> {}", url, mode);
                }
                Err(e) => eprintln!("[Relay] Failed to update relay mode: {}", e),
            }
        }
    }

    Ok(true)
}

/// Validate a relay URL without saving it
#[tauri::command]
async fn validate_relay_url_cmd(url: String) -> Result<String, String> {
    validate_relay_url(&url)
}

// ============================================================================

/// Monitor relay pool connection status changes
#[tauri::command]
async fn monitor_relay_connections() -> Result<bool, String> {
    // Guard against multiple invocations (e.g., from hot-reloads in debug mode)
    static MONITOR_STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if MONITOR_STARTED.swap(true, std::sync::atomic::Ordering::SeqCst) {
        // Already running, return success without spawning duplicate tasks
        return Ok(false);
    }

    let client = NOSTR_CLIENT.get().expect("Nostr client not initialized");
    let handle = TAURI_APP.get().unwrap().clone();

    // Get the monitor and subscribe to real-time notifications
    let monitor = client.monitor().ok_or("Failed to get monitor")?;
    let mut receiver = monitor.subscribe();
    
    // Spawn a task to handle real-time relay status notifications
    let handle_clone = handle.clone();
    tokio::spawn(async move {
        while let Ok(notification) = receiver.recv().await {
            match notification {
                MonitorNotification::StatusChanged { relay_url, status } => {
                    let url_str = relay_url.to_string();
                    let status_str = match status {
                        RelayStatus::Initialized => "initialized",
                        RelayStatus::Pending => "pending",
                        RelayStatus::Connecting => "connecting",
                        RelayStatus::Connected => "connected",
                        RelayStatus::Disconnected => "disconnected",
                        RelayStatus::Terminated => "terminated",
                        RelayStatus::Banned => "banned",
                        RelayStatus::Sleeping => "sleeping",
                    };

                    // Log the status change
                    let log_level = match status {
                        RelayStatus::Connected => "info",
                        RelayStatus::Disconnected | RelayStatus::Terminated => "warn",
                        RelayStatus::Banned => "error",
                        _ => "info",
                    };
                    add_relay_log(&url_str, log_level, &format!("Status changed to {}", status_str));

                    // Emit relay status update to frontend
                    handle_clone.emit("relay_status_change", serde_json::json!({
                        "url": url_str,
                        "status": status_str
                    })).unwrap();

                    // Handle reconnection logic
                    match status {
                        RelayStatus::Disconnected => {
                            // The aggressive health check system will handle reconnection
                            // No action needed here to avoid race conditions
                        }
                        RelayStatus::Terminated => {
                            // Relay connection terminated (hard disconnect)
                        }
                        RelayStatus::Connected => {
                            // When a relay reconnects, fetch last 2 days of messages from just that relay
                            let handle_inner = handle_clone.clone();
                            let url_string = url_str.clone();
                            tokio::spawn(async move {
                                // fetch_messages handles both DM and MLS group syncing for single-relay reconnections
                                fetch_messages(handle_inner, false, Some(url_string)).await;
                            });
                        }
                        _ => {}
                    }
                }
            }
        }
    });
    
    // Spawn aggressive health check task
    let client_health = client.clone();
    let handle_health = handle.clone();
    tokio::spawn(async move {
        // Wait 60 seconds before starting health checks
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        
        loop {
            // Get all relays
            let relays = client_health.relays().await;
            let mut unhealthy_relays = Vec::new();
            
            for (url, relay) in &relays {
                let status = relay.status();
                
                // Only test relays that claim to be connected
                if status == RelayStatus::Connected {
                    // Create a simple query to test connectivity
                    let test_filter = Filter::new()
                        .kinds(vec![Kind::Metadata])
                        .limit(1);
                    
                    // Try to fetch with short timeout
                    let start = std::time::Instant::now();
                    let result = tokio::time::timeout(
                        std::time::Duration::from_secs(3),
                        client_health.fetch_events_from(
                            vec![url.to_string()],
                            test_filter,
                            std::time::Duration::from_secs(2)
                        )
                    ).await;
                    
                    let elapsed = start.elapsed();
                    
                    let url_str = url.to_string();
                    let ping_ms = elapsed.as_millis() as u64;
                    let now_secs = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();

                    match result {
                        Ok(Ok(events)) => {
                            // Check if we actually got events or just an empty response
                            if events.is_empty() && elapsed.as_secs() >= 2 {
                                // Empty response after 2+ seconds means relay is not responding properly
                                unhealthy_relays.push((url.clone(), relay.clone()));
                                add_relay_log(&url_str, "warn", "Health check failed: slow/empty response");
                            } else {
                                // Healthy - record ping time
                                update_relay_metrics(&url_str, |m| {
                                    m.ping_ms = Some(ping_ms);
                                    m.last_check = Some(now_secs);
                                });
                            }
                        }
                        Ok(Err(e)) => {
                            // Query failed
                            unhealthy_relays.push((url.clone(), relay.clone()));
                            add_relay_log(&url_str, "warn", &format!("Health check failed: {}", e));
                        }
                        Err(_) => {
                            // Timeout
                            unhealthy_relays.push((url.clone(), relay.clone()));
                            add_relay_log(&url_str, "warn", "Health check failed: timeout");
                        }
                    }
                } else if status == RelayStatus::Terminated || status == RelayStatus::Disconnected {
                    // Already disconnected, add to reconnect list
                    unhealthy_relays.push((url.clone(), relay.clone()));
                }
            }

            // Force reconnect unhealthy relays
            for (url, relay) in unhealthy_relays {
                let url_str = url.to_string();
                // First disconnect if needed
                if relay.status() == RelayStatus::Connected {
                    let _ = relay.disconnect();
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                }

                // Try to reconnect
                add_relay_log(&url_str, "info", "Attempting reconnection...");
                let _ = relay.try_connect(std::time::Duration::from_secs(10)).await;

                // Emit status update
                handle_health.emit("relay_health_check", serde_json::json!({
                    "url": url_str,
                    "healthy": false,
                    "action": "force_reconnect"
                })).unwrap();
            }
            
            // Wait 15 seconds before next health check round
            tokio::time::sleep(std::time::Duration::from_secs(15)).await;
        }
    });
    
    // Keep the original periodic terminated relay check
    tokio::spawn(async move {
        // Wait 30 seconds before starting the polling loop
        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        
        loop {
            // Check all relays every 5 seconds
            let relays = client.relays().await;
            
            for (_url, relay) in relays {
                let status = relay.status();
                
                // If relay is terminated, attempt to reconnect
                if status == RelayStatus::Terminated {
                    let _ = relay.try_connect(std::time::Duration::from_secs(5)).await;
                }
            }
            
            // Wait 5 seconds before next check
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        }
    });
    
    Ok(true)
}

/// Notification type enum for different kinds of notifications
#[derive(Debug, Clone, Copy, PartialEq)]
enum NotificationType {
    DirectMessage,
    GroupMessage,
    GroupInvite,
}

/// Generic notification data structure
#[derive(Debug, Clone)]
#[allow(dead_code)]
struct NotificationData {
    notification_type: NotificationType,
    title: String,
    body: String,
    /// Optional group name for group-related notifications
    group_name: Option<String>,
    /// Optional sender name
    sender_name: Option<String>,
}

impl NotificationData {
    /// Create a DM notification (works for both text and file attachments)
    fn direct_message(sender_name: String, content: String) -> Self {
        Self {
            notification_type: NotificationType::DirectMessage,
            title: sender_name.clone(),
            body: content,
            group_name: None,
            sender_name: Some(sender_name),
        }
    }

    /// Create a group message notification (works for both text and file attachments)
    fn group_message(sender_name: String, group_name: String, content: String) -> Self {
        Self {
            notification_type: NotificationType::GroupMessage,
            title: format!("{} - {}", sender_name, group_name),
            body: content,
            group_name: Some(group_name),
            sender_name: Some(sender_name),
        }
    }

    /// Create a group invite notification
    fn group_invite(group_name: String, inviter_name: String) -> Self {
        Self {
            notification_type: NotificationType::GroupInvite,
            title: format!("Group Invite: {}", group_name),
            body: format!("Invited by {}", inviter_name),
            group_name: Some(group_name),
            sender_name: Some(inviter_name),
        }
    }
}

/// Show an OS notification with generic notification data
fn show_notification_generic(data: NotificationData) {
    let handle = TAURI_APP.get().unwrap();

    // Only send notifications if the app is not focused
    if !handle
        .webview_windows()
        .iter()
        .next()
        .unwrap()
        .1
        .is_focused()
        .unwrap()
    {
        // Play notification sound (non-blocking, desktop only)
        #[cfg(desktop)]
        {
            let handle_clone = handle.clone();
            std::thread::spawn(move || {
                if let Err(e) = audio::play_notification_if_enabled(&handle_clone) {
                    eprintln!("Failed to play notification sound: {}", e);
                }
            });
        }

        #[cfg(target_os = "android")]
        {
            // Determine summary based on notification type
            let summary = match data.notification_type {
                NotificationType::DirectMessage => "Private Message",
                NotificationType::GroupMessage => "Group Message",
                NotificationType::GroupInvite => "Group Invite",
            };
            
            handle
                .notification()
                .builder()
                .title(&data.title)
                .body(&data.body)
                .large_body(&data.body)
                .icon("ic_notification")
                .summary(summary)
                .large_icon("ic_large_icon")
                .show()
                .unwrap_or_else(|e| eprintln!("Failed to send notification: {}", e));
        }
        
        #[cfg(not(target_os = "android"))]
        {
            handle
                .notification()
                .builder()
                .title(&data.title)
                .body(&data.body)
                .large_body(&data.body)
                .show()
                .unwrap_or_else(|e| eprintln!("Failed to send notification: {}", e));
        }
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
    
    // Calculate the hash of the decrypted file
    let file_hash = calculate_file_hash(&decrypted_data);
    
    // Choose the appropriate base directory based on platform
    let base_directory = if cfg!(target_os = "ios") {
        tauri::path::BaseDirectory::Document
    } else {
        tauri::path::BaseDirectory::Download
    };

    // Resolve the directory path using the determined base directory
    let dir = handle.path().resolve("vector", base_directory).unwrap();
    
    // Use hash-based filename
    let file_path = dir.join(format!("{}.{}", file_hash, attachment.extension));

    // Create the vector directory if it doesn't exist
    std::fs::create_dir_all(&dir).map_err(|e| format!("Failed to create directory: {}", e))?;

    // Save the file to disk
    std::fs::write(&file_path, decrypted_data).map_err(|e| format!("Failed to write file: {}", e))?;
    
    Ok(file_path)
}

#[tauri::command]
async fn generate_blurhash_preview(npub: String, msg_id: String) -> Result<String, String> {
    // Get the first attachment from the message by searching through chats
    let img_meta = {
        let state = STATE.lock().await;
        
        // Search through all chats to find the message
        let mut found_attachment = None;
        
        for chat in &state.chats {
            // Check if this is the target chat (works for both DMs and group chats)
            let is_target_chat = match &chat.chat_type {
                ChatType::MlsGroup => chat.id == npub,
                ChatType::DirectMessage => chat.has_participant(&npub),
            };
            
            if is_target_chat {
                // Look for the message in this chat
                if let Some(message) = chat.messages.iter().find(|m| m.id == msg_id) {
                    // Get the first attachment
                    if let Some(attachment) = message.attachments.first() {
                        found_attachment = attachment.img_meta.clone();
                        break;
                    }
                }
            }
        }
        
        found_attachment.ok_or_else(|| "No image attachment found".to_string())?
    };
    
    // Generate the Base64 image using the decode_blurhash_to_base64 function
    let base64_image = util::decode_blurhash_to_base64(
        &img_meta.blurhash,
        img_meta.width,
        img_meta.height,
        1.0 // Default punch value
    );

    Ok(base64_image)
}

/// Generic blurhash decoder - converts a blurhash string to a base64 data URL
/// Used by the GIF picker for placeholder backgrounds
#[tauri::command]
fn decode_blurhash(blurhash: String, width: u32, height: u32) -> String {
    util::decode_blurhash_to_base64(&blurhash, width, height, 1.0)
}

#[tauri::command]
async fn download_attachment(npub: String, msg_id: String, attachment_id: String) -> bool {
    let handle = TAURI_APP.get().unwrap();

    // Grab the attachment's metadata by searching through chats
    let attachment = {
        let mut state = STATE.lock().await;

        // Find the message and attachment in chats
        let mut found_attachment = None;
        for chat in &mut state.chats {
            // For group chats, npub is the group_id; for DMs, it's a participant npub
            let is_target_chat = match &chat.chat_type {
                ChatType::MlsGroup => chat.id == npub,
                ChatType::DirectMessage => chat.has_participant(&npub),
            };

            if is_target_chat {
                if let Some(message) = chat.messages.iter_mut().find(|m| m.id == msg_id) {
                    if let Some(attachment) = message.attachments.iter_mut().find(|a| a.id == attachment_id) {
                        // Check that we're not already downloading
                        if attachment.downloading {
                            return false;
                        }

                        // Check if file already exists on disk (downloaded but flag was wrong)
                        let base_directory = if cfg!(target_os = "ios") {
                            tauri::path::BaseDirectory::Document
                        } else {
                            tauri::path::BaseDirectory::Download
                        };
                        
                        if let Ok(vector_dir) = handle.path().resolve("vector", base_directory) {
                            let file_path = vector_dir.join(format!("{}.{}", &attachment.id, &attachment.extension));
                            if file_path.exists() {
                                // File already exists! Update the state and return success
                                attachment.downloaded = true;
                                attachment.path = file_path.to_string_lossy().to_string();
                                
                                // Emit success event
                                handle.emit("attachment_download_result", serde_json::json!({
                                    "profile_id": npub,
                                    "msg_id": msg_id,
                                    "id": attachment_id,
                                    "success": true,
                                    "result": file_path.to_string_lossy().to_string()
                                })).unwrap();
                                
                                // Also update the database
                                let chat_id_for_db = chat.id().to_string();
                                let msg_id_clone = msg_id.clone();
                                let attachment_id_clone = attachment_id.clone();
                                let path_str = file_path.to_string_lossy().to_string();
                                drop(state); // Release lock before DB call
                                
                                let _ = db::update_attachment_downloaded_status(
                                    handle,
                                    &chat_id_for_db,
                                    &msg_id_clone,
                                    &attachment_id_clone,
                                    true,
                                    &path_str
                                );
                                
                                return true;
                            }
                        }

                        // Enable the downloading flag to prevent re-calls
                        attachment.downloading = true;
                        found_attachment = Some(attachment.clone());
                        break;
                    }
                }
            }
        }

        if found_attachment.is_none() {
            eprintln!("Attachment not found for download: {} in message {}", attachment_id, msg_id);
            return false;
        }

        found_attachment.unwrap()
    };

    // Begin our download progress events
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
            
            // Find and update the attachment status
            for chat in &mut state.chats {
                let is_target_chat = match &chat.chat_type {
                    ChatType::MlsGroup => chat.id == npub,
                    ChatType::DirectMessage => chat.has_participant(&npub),
                };
                
                if is_target_chat {
                    if let Some(message) = chat.messages.iter_mut().find(|m| m.id == msg_id) {
                        if let Some(attachment) = message.attachments.iter_mut().find(|a| a.id == attachment_id) {
                            attachment.downloading = false;
                            attachment.downloaded = false;
                            break;
                        }
                    }
                }
            }

            // Emit the error
            handle.emit("attachment_download_result", serde_json::json!({
                "profile_id": npub,
                "msg_id": msg_id,
                "id": attachment_id,
                "success": false,
                "result": error
            })).unwrap();
            return false;
        }
    };

    // Check if we got a reasonable amount of data
    if encrypted_data.len() < 16 {
        eprintln!("Downloaded file too small: {} bytes for attachment {}", encrypted_data.len(), attachment_id);
        let mut state = STATE.lock().await;
        
        // Find and update the attachment status
        for chat in &mut state.chats {
            let is_target_chat = match &chat.chat_type {
                ChatType::MlsGroup => chat.id == npub,
                ChatType::DirectMessage => chat.has_participant(&npub),
            };
            
            if is_target_chat {
                if let Some(message) = chat.messages.iter_mut().find(|m| m.id == msg_id) {
                    if let Some(attachment) = message.attachments.iter_mut().find(|a| a.id == attachment_id) {
                        attachment.downloading = false;
                        attachment.downloaded = false;
                        break;
                    }
                }
            }
        }
        
        // Emit a more helpful error
        let error_msg = format!("Downloaded file too small ({} bytes). URL may be invalid or expired.", encrypted_data.len());
        handle.emit("attachment_download_result", serde_json::json!({
            "profile_id": npub,
            "msg_id": msg_id,
            "id": attachment_id,
            "success": false,
            "result": error_msg
        })).unwrap();
        return false;
    }
    
    // Decrypt and save the file
    let result = decrypt_and_save_attachment(handle, &encrypted_data, &attachment).await;
    
    // Process the result
    match result {
        Err(error) => {
            // Check if this is a corrupted attachment (decryption failure)
            let is_decryption_error = error.contains("aead") || error.contains("decrypt");
            
            if is_decryption_error {
                eprintln!("Decryption failed for attachment {}: corrupted keys/data mismatch", attachment_id);
            }
            
            // Handle decryption/saving error
            let mut state = STATE.lock().await;
            
            // Find and update the attachment status
            let mut should_remove = false;
            for chat in &mut state.chats {
                let is_target_chat = match &chat.chat_type {
                    ChatType::MlsGroup => chat.id == npub,
                    ChatType::DirectMessage => chat.has_participant(&npub),
                };
                
                if is_target_chat {
                    if let Some(message) = chat.messages.iter_mut().find(|m| m.id == msg_id) {
                        if let Some(attachment) = message.attachments.iter_mut().find(|a| a.id == attachment_id) {
                            attachment.downloading = false;
                            attachment.downloaded = false;
                            
                            // If it's a decryption error, mark for removal as it's corrupted
                            if is_decryption_error {
                                eprintln!("Marking corrupted attachment for removal: {}", attachment_id);
                                should_remove = true;
                            }
                            break;
                        }
                    }
                }
            }
            
            // Remove corrupted attachment if needed and save
            if should_remove {
                // Collect chat_id and messages to save
                let save_data: Option<(String, Vec<Message>)> = {
                    let mut result = None;
                    for chat in &mut state.chats {
                        let is_target_chat = match &chat.chat_type {
                            ChatType::MlsGroup => chat.id == npub,
                            ChatType::DirectMessage => chat.has_participant(&npub),
                        };
                        
                        if is_target_chat {
                            let chat_id = chat.id().to_string();
                            
                            if let Some(message) = chat.messages.iter_mut().find(|m| m.id == msg_id) {
                                let original_count = message.attachments.len();
                                message.attachments.retain(|a| a.id != attachment_id);
                                if message.attachments.len() < original_count {
                                    result = Some((chat_id, vec![message.clone()]));
                                }
                                break;
                            }
                        }
                    }
                    result
                };
                
                // Drop state and save
                drop(state);
                if let Some((chat_id, messages)) = save_data {
                    let _ = save_chat_messages(handle.clone(), &chat_id, &messages).await;
                }
            }

            // Emit the error
            handle.emit("attachment_download_result", serde_json::json!({
                "profile_id": npub,
                "msg_id": msg_id,
                "id": attachment_id,
                "success": false,
                "result": if should_remove {
                    "Corrupted attachment removed. Please re-send the file.".to_string()
                } else {
                    error
                }
            })).unwrap();
            return false;
        }
        Ok(hash_file_path) => {
            // Successfully decrypted and saved
            // Extract the hash from the filename (format: {hash}.{extension})
            let file_hash = hash_file_path.file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or(&attachment_id)
                .to_string();
            
            // Update state with successful download
            {
                let mut state = STATE.lock().await;
                
                // Find and update the attachment
                for chat in &mut state.chats {
                    let is_target_chat = match &chat.chat_type {
                        ChatType::MlsGroup => chat.id == npub,
                        ChatType::DirectMessage => chat.has_participant(&npub),
                    };
                    
                    if is_target_chat {
                        if let Some(message) = chat.messages.iter_mut().find(|m| m.id == msg_id) {
                            if let Some(attachment_index) = message.attachments.iter().position(|a| a.id == attachment_id) {
                                let attachment = &mut message.attachments[attachment_index];
                                attachment.id = file_hash.clone(); // Update ID from nonce to hash
                                attachment.downloading = false;
                                attachment.downloaded = true;
                                attachment.path = hash_file_path.to_string_lossy().to_string(); // Update to hash-based path
                                break;
                            }
                        }
                    }
                }

                // Emit the finished download with both old and new IDs
                handle.emit("attachment_download_result", serde_json::json!({
                    "profile_id": npub,
                    "msg_id": msg_id,
                    "old_id": attachment_id,
                    "id": file_hash,
                    "success": true,
                })).unwrap();

                // Persist updated message/attachment metadata to the database
                if let Some(handle) = TAURI_APP.get() {
                    // Find and save only the updated message
                    let updated_chat = state.get_chat(&npub).unwrap();
                    let updated_message = {
                        updated_chat.messages.iter().find(|m| m.id == msg_id).cloned()
                    }.unwrap();

                    // Update the frontend state
                    handle.emit("message_update", serde_json::json!({
                        "old_id": &updated_message.id,
                        "message": updated_message.clone(),
                        "chat_id": updated_chat.id()
                    })).unwrap();

                    // Drop the STATE lock before performing async I/O
                    drop(state);

                    let _ = db::save_message(handle.clone(), &npub, &updated_message).await;
                }
            }
            
            true
        }
    }
}

#[derive(serde::Serialize, Clone)]
struct LoginKeyPair {
    public: String,
    private: String,
}

/// # Debug Hot-Reload State Sync
///
/// This command ONLY compiles in debug builds and provides a fast-path for
/// frontend hot-reloads during development. When the frontend hot-reloads,
/// the backend retains all state, so we can skip the entire login/decrypt
/// flow and just bulk-send the existing state back to the frontend.
///
/// Returns:
/// - `Ok(json)` with full state if backend is already initialized
/// - `Err(...)` if backend is not initialized (frontend should do normal login)
#[cfg(debug_assertions)]
#[tauri::command]
async fn debug_hot_reload_sync() -> Result<serde_json::Value, String> {
    // Check if we have an active Nostr client (meaning we're already logged in)
    let client = match NOSTR_CLIENT.get() {
        Some(c) => c,
        None => return Err("Backend not initialized - perform normal login".to_string()),
    };
    
    // Get the current user's public key
    let signer = client.signer().await.map_err(|e| format!("Signer error: {}", e))?;
    let my_npub = signer.get_public_key().await
        .map_err(|e| format!("Public key error: {}", e))?
        .to_bech32()
        .map_err(|e| format!("Bech32 error: {}", e))?;
    
    // Get the full state
    let state = STATE.lock().await;
    
    // Verify we have meaningful state (not just an empty initialized state)
    if state.profiles.is_empty() && state.chats.is_empty() {
        return Err("Backend state is empty - perform normal login".to_string());
    }
    
    // Return the full state for the frontend to hydrate
    println!("[Debug Hot-Reload] Sending cached state to frontend ({} profiles, {} chats)",
             state.profiles.len(), state.chats.len());
    
    Ok(serde_json::json!({
        "success": true,
        "npub": my_npub,
        "profiles": &state.profiles,
        "chats": &state.chats,
        "is_syncing": state.is_syncing,
        "sync_mode": format!("{:?}", state.sync_mode)
    }))
}

#[tauri::command]
async fn login(import_key: String) -> Result<LoginKeyPair, String> {
    let keys: Keys;

    // If we're already logged in (i.e: Developer Mode with frontend hot-loading), just return the existing keys.
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
        .opts(ClientOptions::new().gossip(false))
        .monitor(Monitor::new(1024))
        .build();
    NOSTR_CLIENT.set(client).unwrap();

    // Add our profile (at least, the npub of it) to our state
    let npub = keys.public_key.to_bech32().unwrap();
    let mut profile = Profile::new();
    profile.id = npub.clone();
    profile.mine = true;
    STATE.lock().await.profiles.push(profile);

    // Initialize profile database and set as current account
    // Always init DB - this handles both new and existing accounts:
    // - Creates schema if needed (IF NOT EXISTS)
    // - Runs any pending migrations atomically
    // - Pools the connection for fast subsequent access
    if let Some(handle) = TAURI_APP.get() {
        if let Err(e) = account_manager::init_profile_database(handle, &npub).await {
            eprintln!("[Login] Failed to initialize profile database: {}", e);
        } else if let Err(e) = account_manager::set_current_account(npub.clone()) {
            eprintln!("[Login] Failed to set current account: {}", e);
        } else {
            println!("[Login] Database initialized and account set: {}", npub);
        }
    }

    // Return our npub to the frontend client
    Ok(LoginKeyPair {
        public: npub,
        private: keys.secret_key().to_bech32().unwrap(),
    })
}

/// Returns `true` if the client has connected, `false` if it was already connected
#[tauri::command]
async fn connect<R: Runtime>(handle: AppHandle<R>) -> bool {
    let client = NOSTR_CLIENT.get().expect("Nostr client not initialized");

    // If we're already connected to some relays - skip and tell the frontend our client is already online
    if client.relays().await.len() > 0 {
        return false;
    }

    // Get disabled default relays
    let disabled_defaults = get_disabled_default_relays(&handle).await.unwrap_or_default();

    // Add default relays (unless disabled)
    for default_url in DEFAULT_RELAYS {
        let is_disabled = disabled_defaults.iter().any(|d| d.to_lowercase() == default_url.to_lowercase());
        if !is_disabled {
            match client.pool().add_relay(*default_url, RelayOptions::new().reconnect(false)).await {
                Ok(_) => {
                    println!("[Relay] Added default relay: {}", default_url);
                    add_relay_log(default_url, "info", "Added to relay pool");
                }
                Err(e) => {
                    eprintln!("[Relay] Failed to add default relay {}: {}", default_url, e);
                    add_relay_log(default_url, "error", &format!("Failed to add: {}", e));
                }
            }
        } else {
            println!("[Relay] Skipping disabled default relay: {}", default_url);
            add_relay_log(default_url, "info", "Skipped (disabled by user)");
        }
    }

    // Add user's custom relays (if any)
    match get_custom_relays(handle.clone()).await {
        Ok(custom_relays) => {
            for relay in custom_relays {
                if relay.enabled {
                    match client.pool().add_relay(&relay.url, relay_options_for_mode(&relay.mode)).await {
                        Ok(_) => {
                            println!("[Relay] Added custom relay: {} (mode: {})", relay.url, relay.mode);
                            add_relay_log(&relay.url, "info", &format!("Added to relay pool (mode: {})", relay.mode));
                        }
                        Err(e) => {
                            eprintln!("[Relay] Failed to add custom relay {}: {}", relay.url, e);
                            add_relay_log(&relay.url, "error", &format!("Failed to add: {}", e));
                        }
                    }
                }
            }
        }
        Err(e) => eprintln!("[Relay] Failed to load custom relays: {}", e),
    }

    // Connect!
    client.connect().await;
    true
}



// Tauri command that uses the crypto module
#[tauri::command]
async fn encrypt(input: String, password: Option<String>) -> String {
    let res = crypto::internal_encrypt(input, password).await;

    // If we have one; save the in-memory seedphrase in an encrypted at-rest format
    match MNEMONIC_SEED.get() {
        Some(seed) => {
            // Save the seed phrase to the database
            let handle = TAURI_APP.get().unwrap();
            let _ = db::set_seed(handle.clone(), seed.to_string()).await;
        }
        _ => ()
    }

    // Check if we have a pending invite acceptance to broadcast
    if let Some(pending_invite) = PENDING_INVITE.get() {
        // Get the Nostr client
        if let Some(client) = NOSTR_CLIENT.get() {
            // Clone the data we need before the async block
            let invite_code = pending_invite.invite_code.clone();
            let inviter_pubkey = pending_invite.inviter_pubkey.clone();
            
            // Spawn the broadcast in a separate task to avoid blocking
            tokio::spawn(async move {
                // Create and publish the acceptance event
                let event_builder = EventBuilder::new(Kind::ApplicationSpecificData, "vector_invite_accepted")
                    .tag(Tag::custom(TagKind::Custom("l".into()), vec!["vector"]))
                    .tag(Tag::custom(TagKind::Custom("d".into()), vec![invite_code.as_str()]))
                    .tag(Tag::public_key(inviter_pubkey));
                
                // Build the event
                match client.sign_event_builder(event_builder).await {
                    Ok(event) => {
                        // Send only to trusted relays
                        match client.send_event_to(TRUSTED_RELAYS.iter().copied(), &event).await {
                            Ok(_) => println!("Successfully broadcast invite acceptance to trusted relays"),
                            Err(e) => eprintln!("Failed to broadcast invite acceptance: {}", e),
                        }
                    }
                    Err(e) => eprintln!("Failed to sign invite acceptance event: {}", e),
                }
            });
        }
    }

    // Bootstrap MLS device keypackage for newly created accounts (non-blocking)
    // This ensures keypackages are published immediately after PIN setup, not just on restart
    tokio::spawn(async move {
        // Brief delay to allow encryption key to be set
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        
        // Skip if no account selected (migration pending)
        if crate::account_manager::get_current_account().is_err() {
            println!("[MLS] Skipping KeyPackage bootstrap - no account selected (migration may be pending)");
            return;
        }
        
        println!("[MLS] Ensuring persistent device KeyPackage after PIN setup...");
        match regenerate_device_keypackage(true).await {
            Ok(info) => {
                let device_id = info.get("device_id").and_then(|v| v.as_str()).unwrap_or("");
                let cached = info.get("cached").and_then(|v| v.as_bool()).unwrap_or(false);
                println!("[MLS] Device KeyPackage ready: device_id={}, cached={}", device_id, cached);
            }
            Err(e) => eprintln!("[MLS] Device KeyPackage bootstrap failed: {}", e),
        }
    });

    res
}

// Tauri command that uses the crypto module
#[tauri::command]
async fn decrypt(ciphertext: String, password: Option<String>) -> Result<String, ()> {
    // Perform decryption
    let res = crypto::internal_decrypt(ciphertext, password).await;

    // On success, ensure persistent device KeyPackage and run non-blocking smoke test
    if res.is_ok() {
        // Best-effort persistent device KeyPackage bootstrap (non-blocking)
        tokio::spawn(async move {
            // brief delay to allow any post-login setup to settle
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            
            // Skip if no account selected (migration pending)
            if crate::account_manager::get_current_account().is_err() {
                println!("[MLS] Skipping KeyPackage bootstrap - no account selected (migration may be pending)");
                return;
            }
            
            println!("[MLS] Ensuring persistent device KeyPackage...");
            match regenerate_device_keypackage(true).await {
                Ok(info) => {
                    let device_id = info.get("device_id").and_then(|v| v.as_str()).unwrap_or("");
                    let cached = info.get("cached").and_then(|v| v.as_bool()).unwrap_or(false);
                    println!("[MLS] Device KeyPackage ready: device_id={}, cached={}", device_id, cached);
                }
                Err(e) => eprintln!("[MLS] Device KeyPackage bootstrap failed: {}", e),
            }
        });
    }

    res
}

#[tauri::command]
async fn start_recording() -> Result<(), String> {
    #[cfg(target_os = "android")] 
    {
        // Check if we already have permission
        if !android::permissions::check_audio_permission().unwrap() {
            // This will block until the user responds to the permission dialog
            let granted = android::permissions::request_audio_permission_blocking()?;
            
            if !granted {
                return Err("Audio permission denied by user".to_string());
            }
        }
    }

    AudioRecorder::global().start()
}

#[tauri::command]
async fn stop_recording() -> Result<Vec<u8>, String> {
    AudioRecorder::global().stop()
}

#[tauri::command]
async fn deep_rescan<R: Runtime>(handle: AppHandle<R>) -> Result<bool, String> {
    // Check if a scan is already in progress
    {
        let state = STATE.lock().await;
        if state.is_syncing {
            return Err("Already Scanning! Please wait for the current scan to finish.".to_string());
        }
    }

    // Start a deep rescan by forcing DeepRescan mode
    {
        let mut state = STATE.lock().await;
        let now = Timestamp::now();
        
        // Set up for deep rescan starting from now
        state.is_syncing = true;
        state.sync_mode = SyncMode::DeepRescan;
        state.sync_empty_iterations = 0;
        state.sync_total_iterations = 0;
        
        // Start with a 2-day window from now
        let two_days_ago = now.as_u64() - (60 * 60 * 24 * 2);
        state.sync_window_start = two_days_ago;
        state.sync_window_end = now.as_u64();
    }

    // Trigger the first fetch
    fetch_messages(handle, false, None).await;
    
    Ok(true)
}

#[tauri::command]
async fn is_scanning() -> bool {
    let state = STATE.lock().await;
    state.is_syncing
}

#[tauri::command]
async fn logout<R: Runtime>(handle: AppHandle<R>) {
    // Lock the state to ensure nothing is added to the DB before restart
    let _guard = STATE.lock().await;

    // Close the database connection pool BEFORE attempting to delete files
    // This is critical on Windows where open file handles prevent deletion
    account_manager::close_db_connection();

    // Delete the current account's profile directory (SQL database and MLS data)
    if let Ok(npub) = account_manager::get_current_account() {
        if let Ok(profile_dir) = account_manager::get_profile_directory(&handle, &npub) {
            if profile_dir.exists() {
                if let Err(e) = std::fs::remove_dir_all(&profile_dir) {
                    eprintln!("[Logout] Failed to remove profile directory: {}", e);
                }
            }
        }
    }

    // Delete the downloads folder (vector folder in Downloads or Documents on iOS)
    let base_directory = if cfg!(target_os = "ios") {
        tauri::path::BaseDirectory::Document
    } else {
        tauri::path::BaseDirectory::Download
    };
    
    if let Ok(downloads_dir) = handle.path().resolve("vector", base_directory) {
        if downloads_dir.exists() {
            let _ = std::fs::remove_dir_all(&downloads_dir);
        }
    }

    // Delete the legacy MLS folder in AppData (for backwards compatibility)
    if let Ok(mls_dir) = handle.path().resolve("mls", tauri::path::BaseDirectory::AppData) {
        if mls_dir.exists() {
            let _ = std::fs::remove_dir_all(&mls_dir);
        }
    }

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
        .opts(ClientOptions::new().gossip(false))
        .monitor(Monitor::new(1024))
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

    // Store npub temporarily - database will be created when set_pkey is called (after user sets PIN)
    // This prevents creating "dead accounts" if user quits before setting a PIN
    account_manager::set_pending_account(npub.clone())?;

    // Return the keypair in the same format as the login function
    Ok(LoginKeyPair {
        public: npub,
        private: keys.secret_key().to_bech32().map_err(|e| e.to_string())?,
    })
}

/// Export account keys (nsec and seed phrase if available)
#[tauri::command]
async fn export_keys() -> Result<serde_json::Value, String> {
    // Try to get nsec from database first
    let handle = TAURI_APP.get().unwrap();
    let nsec = if let Some(enc_pkey) = db::get_pkey(handle.clone())? {
        // Decrypt the nsec
        match crypto::internal_decrypt(enc_pkey, None).await {
            Ok(decrypted_nsec) => decrypted_nsec,
            Err(_) => return Err("Failed to decrypt nsec".to_string()),
        }
    } else {
        return Err("No nsec found in database".to_string());
    };
    
    // Try to get seed phrase from memory first
    let seed_phrase = if let Some(seed) = MNEMONIC_SEED.get() {
        Some(seed.clone())
    } else {
        // If not in memory, try to get from database
        if ENCRYPTION_KEY.get().is_some() {
            match db::get_seed(handle.clone()).await {
                Ok(Some(seed)) => Some(seed),
                Ok(None) => None,
                Err(_) => None,
            }
        } else {
            None
        }
    };
    
    // Create response object
    let response = serde_json::json!({
        "nsec": nsec,
        "seed_phrase": seed_phrase
    });
    
    Ok(response)
}

/// Updates the OS taskbar badge with the count of unread messages
/// Platform feature list structure
#[derive(serde::Serialize, Clone)]
struct PlatformFeatures {
    transcription: bool,
    notification_sounds: bool,
    os: String,
    is_mobile: bool,
    debug_mode: bool,
}

/// Returns a list of platform-specific features available
#[tauri::command]
async fn get_platform_features() -> PlatformFeatures {
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

    PlatformFeatures {
        transcription: cfg!(all(not(target_os = "android"), feature = "whisper")),
        notification_sounds: cfg!(desktop),
        os: os.to_string(),
        is_mobile,
        debug_mode: cfg!(debug_assertions),
    }
}

/// Run periodic maintenance tasks to keep memory usage low
/// Called every ~45s from the JS profile sync loop
///
/// Current tasks:
/// - Purge expired notification sound cache (10 min TTL, desktop only)
/// - Cleanup stale in-progress download tracking entries
///
/// Future tasks could include:
/// - Image cache cleanup
/// - Temporary file cleanup
/// - Memory pressure responses
#[tauri::command]
async fn run_maintenance() {
    // Audio: purge expired notification sound cache (desktop only)
    #[cfg(desktop)]
    audio::check_cache_ttl();

    // Cleanup stale download tracking entries
    image_cache::cleanup_stale_downloads().await;
}

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

#[cfg(all(not(target_os = "android"), feature = "whisper"))]
#[tauri::command]
async fn transcribe<R: Runtime>(handle: AppHandle<R>, file_path: String, model_name: String, translate: bool) -> Result<whisper::TranscriptionResult, String> {
    // Convert the file path to a Path
    let path = std::path::Path::new(&file_path);
    
    // Check if the file exists
    if !path.exists() {
        return Err(format!("File does not exist: {}", file_path));
    }
    
    // Decode and resample to 16kHz for Whisper
    match audio::decode_and_resample(path, 16000) {
        Ok(audio_data) => {
            // Pass the resampled audio to the whisper transcribe function
            match whisper::transcribe(&handle, &model_name, translate, audio_data).await {
                Ok(result) => Ok(result),
                Err(e) => Err(format!("Transcription error: {}", e.to_string()))
            }
        },
        Err(e) => Err(format!("Audio processing error: {}", e.to_string()))
    }
}

#[cfg(any(target_os = "android", not(feature = "whisper")))]
#[tauri::command]
async fn transcribe<R: Runtime>(_handle: AppHandle<R>, _file_path: String, _model_name: String, _translate: bool) -> Result<String, String> {
    Err("Whisper transcription is not supported on this platform".to_string())
}

#[cfg(all(not(target_os = "android"), feature = "whisper"))]
#[tauri::command]
async fn download_whisper_model<R: Runtime>(handle: AppHandle<R>, model_name: String) -> Result<String, String> {
    // Download (or simply return the cached path of) a Whisper Model
    match whisper::download_whisper_model(&handle, &model_name).await {
        Ok(path) => Ok(path),
        Err(e) => Err(format!("Model Download error: {}", e.to_string()))
    }
}

#[cfg(any(target_os = "android", not(feature = "whisper")))]
#[tauri::command]
async fn download_whisper_model<R: Runtime>(_handle: AppHandle<R>, _model_name: String) -> Result<String, String> {
    Err("Whisper model download is not supported on this platform".to_string())
}

/// Generate a random alphanumeric invite code
fn generate_invite_code() -> String {
    thread_rng()
        .sample_iter(&Alphanumeric)
        .take(8)
        .map(char::from)
        .collect::<String>()
        .to_uppercase()
}

/// Generate or retrieve existing invite code for the current user
#[tauri::command]
async fn get_or_create_invite_code() -> Result<String, String> {
    let handle = TAURI_APP.get().ok_or("App handle not initialized")?;
    
    // Check if we already have a stored invite code
    if let Ok(Some(existing_code)) = db::get_sql_setting(handle.clone(), "invite_code".to_string()) {
        return Ok(existing_code);
    }
    
    // No local code found, check the network
    let client = NOSTR_CLIENT.get().ok_or("Nostr client not initialized")?;
    
    // Get our public key
    let signer = client.signer().await.map_err(|e| e.to_string())?;
    let my_public_key = signer.get_public_key().await.map_err(|e| e.to_string())?;
    
    // Check if we've already published an invite on the network
    let filter = Filter::new()
        .author(my_public_key)
        .kind(Kind::ApplicationSpecificData)
        .custom_tag(SingleLetterTag::lowercase(Alphabet::D), "vector")
        .limit(100);
    
    let mut events = client
        .stream_events(filter, std::time::Duration::from_secs(10))
        .await
        .map_err(|e| e.to_string())?;
    
    // Look for existing invite events
    while let Some(event) = events.next().await {
        if event.content == "vector_invite" {
            // Extract the r tag (invite code)
            if let Some(r_tag) = event.tags.find(TagKind::Custom(Cow::Borrowed("r"))) {
                if let Some(code) = r_tag.content() {
                    // Store it locally
                    db::set_sql_setting(handle.clone(), "invite_code".to_string(), code.to_string())
                        .map_err(|e| e.to_string())?;
                    return Ok(code.to_string());
                }
            }
        }
    }
    
    // No existing invite found anywhere, generate a new one
    let new_code = generate_invite_code();
    
    // Create and publish the invite event
    let event_builder = EventBuilder::new(Kind::ApplicationSpecificData, "vector_invite")
        .tag(Tag::custom(TagKind::d(), vec!["vector"]))
        .tag(Tag::custom(TagKind::Custom("r".into()), vec![new_code.as_str()]));
    
    // Build the event
    let event = client.sign_event_builder(event_builder).await.map_err(|e| e.to_string())?;

    // Send only to trusted relays
    client.send_event_to(TRUSTED_RELAYS.iter().copied(), &event).await.map_err(|e| e.to_string())?;
    
    // Store locally
    db::set_sql_setting(handle.clone(), "invite_code".to_string(), new_code.clone())
        .map_err(|e| e.to_string())?;
    
    Ok(new_code)
}

/// Accept an invite code from another user (deferred until after encryption setup)
#[tauri::command]
async fn accept_invite_code(invite_code: String) -> Result<String, String> {
    let client = NOSTR_CLIENT.get().ok_or("Nostr client not initialized")?;
    
    // Validate invite code format (8 alphanumeric characters)
    if invite_code.len() != 8 || !invite_code.chars().all(|c| c.is_alphanumeric()) {
        return Err("Invalid invite code format".to_string());
    }
    
    // Search for the invite event
    let filter = Filter::new()
        .kind(Kind::ApplicationSpecificData)
        .custom_tag(SingleLetterTag::lowercase(Alphabet::D), "vector")
        .custom_tag(SingleLetterTag::lowercase(Alphabet::R), &invite_code)
        .limit(1);
    
    
    // Find the invite event
    let mut events = client
        .stream_events_from(TRUSTED_RELAYS.to_vec(), filter, std::time::Duration::from_secs(10))
        .await
        .map_err(|e| e.to_string())?;
    
    let invite_event = {
        let mut found: Option<nostr_sdk::Event> = None;
        while let Some(event) = events.next().await {
            if event.content == "vector_invite" {
                found = Some(event);
                break;
            }
        }
        found.ok_or("Invite code not found")?
    };
    
    // Get the inviter's public key
    let inviter_pubkey = invite_event.pubkey;
    let inviter_npub = inviter_pubkey.to_bech32().map_err(|e| e.to_string())?;
    
    // Get our public key
    let signer = client.signer().await.map_err(|e| e.to_string())?;
    let my_public_key = signer.get_public_key().await.map_err(|e| e.to_string())?;
    
    // Check if we're trying to accept our own invite
    if inviter_pubkey == my_public_key {
        return Err("Cannot accept your own invite code".to_string());
    }
    
    // Store the pending invite acceptance (will be broadcast after encryption setup)
    let pending_invite = PendingInviteAcceptance {
        invite_code: invite_code.clone(),
        inviter_pubkey: inviter_pubkey.clone(),
    };
    
    // Try to set the pending invite, ignore if already set
    let _ = PENDING_INVITE.set(pending_invite);
    
    // Return the inviter's npub so the frontend can initiate a chat
    Ok(inviter_npub)
}

/// Get storage information for the Vector directory
#[tauri::command]
async fn get_storage_info() -> Result<serde_json::Value, String> {
    let handle = TAURI_APP.get().ok_or("App handle not initialized")?;
    
    // Determine the base directory (Downloads on most platforms, Documents on iOS)
    let base_directory = if cfg!(target_os = "ios") {
        tauri::path::BaseDirectory::Document
    } else {
        tauri::path::BaseDirectory::Download
    };
    
    // Resolve the vector directory path
    let vector_dir = handle.path().resolve("vector", base_directory)
        .map_err(|e| format!("Failed to resolve vector directory: {}", e))?;
    
    // Check if directory exists
    if !vector_dir.exists() {
        return Ok(serde_json::json!({
            "path": vector_dir.to_string_lossy().to_string(),
            "total_bytes": 0,
            "file_count": 0,
            "type_distribution": {}
        }));
    }
    
    // Calculate total size and file count
    let mut total_bytes = 0;
    let mut file_count = 0;
    
    // Track file type distribution by size
    let mut type_distribution = std::collections::HashMap::new();
    
    // Walk through all files in the directory
    if let Ok(entries) = std::fs::read_dir(&vector_dir) {
        for entry in entries.flatten() {
            if let Ok(metadata) = entry.metadata() {
                if metadata.is_file() {
                    let file_size = metadata.len();
                    total_bytes += file_size;
                    file_count += 1;
                    
                    // Get file extension
                    if let Some(extension) = entry.file_name().to_string_lossy().split('.').last() {
                        let extension = extension.to_lowercase();
                        *type_distribution.entry(extension).or_insert(0) += file_size;
                    }
                }
            }
        }
    }

    // Calculate Whisper models size if whisper feature is enabled
    #[cfg(all(not(target_os = "android"), feature = "whisper"))]
    {
        // Calculate total size of downloaded Whisper models
        let mut ai_models_size = 0;
        for model in whisper::MODELS {
            if whisper::is_model_downloaded(&handle, model.name) {
                // Convert MB to bytes (model sizes are in MB)
                ai_models_size += (model.size as u64) * 1024 * 1024;
            }
        }

        if ai_models_size > 0 {
            // Add AI models to type distribution
            *type_distribution.entry("ai_models".to_string()).or_insert(0) += ai_models_size;
            total_bytes += ai_models_size;
        }
    }

    // Calculate image cache size (avatars, banners, miniapp icons)
    // Cache is global (not per-account) for deduplication across accounts
    if let Ok(cache_size) = image_cache::get_cache_size(handle) {
        if cache_size > 0 {
            *type_distribution.entry("cache".to_string()).or_insert(0) += cache_size;
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

/// Clear all downloaded attachments from messages and return freed storage space
#[tauri::command]
async fn clear_storage() -> Result<serde_json::Value, String> {
    let handle = TAURI_APP.get().ok_or("App handle not initialized")?;
    
    // First, get the total storage size before clearing
    let storage_info_before = get_storage_info().await.map_err(|e| format!("Failed to get storage info before clearing: {}", e))?;
    let total_bytes_before = storage_info_before["total_bytes"].as_u64().unwrap_or(0);
    
    // Lock the state to access all chats and messages
    let mut state = STATE.lock().await;
    
    // Track which chats have been updated to avoid duplicate saves
    let mut updated_chats = std::collections::HashSet::new();
    
    // Process each chat to clear attachment metadata in messages
    for chat in &mut state.chats {
        let mut messages_to_update = Vec::new();
        
        // Iterate through all messages in this chat
        for message in &mut chat.messages {
            let mut attachment_updated = false;
            
            // Iterate through all attachments and reset their properties
            for attachment in &mut message.attachments {
                if attachment.downloaded || !attachment.path.is_empty() {
                    // Delete the file, if it exists
                    if std::fs::exists(&attachment.path).unwrap_or(false) {
                        let _ = std::fs::remove_file(&attachment.path);
                    }
                    // Reset attachment properties
                    attachment.downloaded = false;
                    attachment.downloading = false;
                    attachment.path = String::new();
                    attachment_updated = true;
                }
            }
            
            // If any attachment was updated, add to messages to update
            if attachment_updated {
                messages_to_update.push(message.clone());
            }
        }
        
        // If we have messages to update, save them to the database
        if !messages_to_update.is_empty() {
            // Save updated messages to database
            db::save_chat_messages(handle.clone(), chat.id(), &messages_to_update).await
                .map_err(|e| format!("Failed to save updated messages for chat {}: {}", chat.id(), e))?;
            
            // Emit message_update events for each updated message
            for message in &messages_to_update {
                handle.emit("message_update", serde_json::json!({
                    "old_id": &message.id,
                    "message": message,
                    "chat_id": chat.id()
                })).map_err(|e| format!("Failed to emit message_update for chat {}: {}", chat.id(), e))?;
            }
            
            updated_chats.insert(chat.id().to_string());
        }
    }
    
    // Clear all disk caches (images, sounds, etc.) by nuking the cache directory
    let cache_dir = handle.path().app_data_dir()
        .map_err(|e| format!("Failed to get app data dir: {}", e))?
        .join("cache");
    if cache_dir.exists() {
        let _ = std::fs::remove_dir_all(&cache_dir);
    }

    // Clear in-memory notification sound cache (desktop only)
    #[cfg(desktop)]
    audio::purge_sound_cache();

    // Clear cached paths from all profiles in state and database
    for profile in &mut state.profiles {
        if !profile.avatar_cached.is_empty() || !profile.banner_cached.is_empty() {
            profile.avatar_cached = String::new();
            profile.banner_cached = String::new();
            db::set_profile(handle.clone(), profile.clone()).await.ok();
        }
    }

    // Get storage info after clearing to calculate freed space
    // Need to drop the state lock first since get_storage_info needs it
    drop(state);
    let storage_info_after = get_storage_info().await.map_err(|e| format!("Failed to get storage info after clearing: {}", e))?;
    let total_bytes_after = storage_info_after["total_bytes"].as_u64().unwrap_or(0);

    // Calculate freed space
    let freed_bytes = total_bytes_before.saturating_sub(total_bytes_after);

    // Return the freed storage information
    Ok(serde_json::json!({
        "freed_bytes": freed_bytes,
        "freed_formatted": format_bytes(freed_bytes),
        "updated_chats": updated_chats.len()
    }))
}

/// Get the count of unique users who accepted invites from a given npub
#[tauri::command]
async fn get_invited_users(npub: String) -> Result<u32, String> {
    let client = NOSTR_CLIENT.get().ok_or("Nostr client not initialized")?;
    
    // Convert npub to PublicKey
    let inviter_pubkey = PublicKey::from_bech32(&npub).map_err(|e| e.to_string())?;
    
    // First, get the inviter's invite code from the trusted relays
    let filter = Filter::new()
        .author(inviter_pubkey)
        .kind(Kind::ApplicationSpecificData)
        .custom_tag(SingleLetterTag::lowercase(Alphabet::D), "vector")
        .limit(100);

    let mut events = client
        .stream_events_from(TRUSTED_RELAYS.to_vec(), filter, std::time::Duration::from_secs(10))
        .await
        .map_err(|e| e.to_string())?;

    // Find the invite event and extract the invite code
    let mut invite_code_opt = None;
    while let Some(event) = events.next().await {
        if event.content == "vector_invite" {
            if let Some(tag) = event.tags.find(TagKind::Custom(Cow::Borrowed("r"))) {
                if let Some(content) = tag.content() {
                    invite_code_opt = Some(content.to_string());
                    break;
                }
            }
        }
    }
    let invite_code = invite_code_opt.ok_or("No invite code found for this user")?;

    // Now fetch all acceptance events for this invite code from the trusted relays
    let acceptance_filter = Filter::new()
        .kind(Kind::ApplicationSpecificData)
        .custom_tag(SingleLetterTag::lowercase(Alphabet::D), invite_code)
        .limit(1000); // Allow fetching many acceptances

    let mut acceptance_events = client
        .stream_events_from(TRUSTED_RELAYS.to_vec(), acceptance_filter, std::time::Duration::from_secs(10))
        .await
        .map_err(|e| e.to_string())?;
    
    // Filter for acceptance events that reference our inviter and collect unique acceptors
    let mut unique_acceptors = std::collections::HashSet::new();
    
    while let Some(event) = acceptance_events.next().await {
        if event.content == "vector_invite_accepted" {
            // Check if this acceptance references our inviter
            let references_inviter = event.tags
                .iter()
                .any(|tag| {
                    if let Some(TagStandard::PublicKey { public_key, .. }) = tag.as_standardized() {
                        *public_key == inviter_pubkey
                    } else {
                        false
                    }
                });
            
            if references_inviter {
                unique_acceptors.insert(event.pubkey);
            }
        }
    }
    
    Ok(unique_acceptors.len() as u32)
}

// Guy Fawkes Day 2025 - V for Vector Badge (Event Ended)
const FAWKES_DAY_START: u64 = 1762300800; // 2025-11-05 00:00:00 UTC
const FAWKES_DAY_END: u64 = 1762387200;   // 2025-11-06 00:00:00 UTC

/// Check if a user has the Guy Fawkes Day badge
/// Verifies they have a valid badge claim event from the November 5, 2025 event
#[tauri::command]
async fn check_fawkes_badge(npub: String) -> Result<bool, String> {
    let client = NOSTR_CLIENT.get().ok_or("Nostr client not initialized")?;
    
    // Convert npub to PublicKey
    let user_pubkey = PublicKey::from_bech32(&npub).map_err(|e| e.to_string())?;
    
    // Fetch the user's badge claim event
    let filter = Filter::new()
        .author(user_pubkey)
        .kind(Kind::ApplicationSpecificData)
        .custom_tag(SingleLetterTag::lowercase(Alphabet::D), "fawkes_2025")
        .limit(10);

    let mut events = client
        .stream_events_from(TRUSTED_RELAYS.to_vec(), filter, std::time::Duration::from_secs(10))
        .await
        .map_err(|e| e.to_string())?;
    
    // Check if they have a valid badge claim from the event period
    while let Some(event) = events.next().await {
        if event.content == "fawkes_badge_claimed" {
            let timestamp = event.created_at.as_u64();
            // Verify the timestamp is within the valid event window
            if timestamp >= FAWKES_DAY_START && timestamp < FAWKES_DAY_END {
                return Ok(true);
            }
        }
    }
    
    Ok(false)
}
// MLS Tauri Commands


/// Regenerate this device's MLS KeyPackage. If `cache` is true, attempt to reuse an existing
/// cached KeyPackage if it exists on the relay; otherwise always generate a fresh one.
/// Load MLS device ID for the current account
#[tauri::command]
async fn load_mls_device_id() -> Result<Option<String>, String> {
    let handle = TAURI_APP.get().ok_or("App handle not initialized")?.clone();
    match db::load_mls_device_id(&handle).await {
        Ok(Some(id)) => Ok(Some(id)),
        Ok(None) => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

/// Load MLS keypackages for the current account
#[tauri::command]
async fn load_mls_keypackages() -> Result<Vec<serde_json::Value>, String> {
    let handle = TAURI_APP.get().ok_or("App handle not initialized")?.clone();
    db::load_mls_keypackages(&handle).await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn regenerate_device_keypackage(cache: bool) -> Result<serde_json::Value, String> {
    // Access handle and client
    let handle = TAURI_APP.get().ok_or("App handle not initialized")?.clone();
    let client = NOSTR_CLIENT.get().ok_or("Nostr client not initialized")?;

    // Ensure a persistent device_id exists
    let device_id: String = match db::load_mls_device_id(&handle).await {
        Ok(Some(id)) => id,
        _ => {
            let id: String = thread_rng()
                .sample_iter(&Alphanumeric)
                .take(12)
                .map(char::from)
                .collect::<String>()
                .to_lowercase();
            let _ = db::save_mls_device_id(handle.clone(), &id).await;
            id
        }
    };

    // Resolve my pubkey (awaits before any MLS engine is created)
    let signer = client.signer().await.map_err(|e| e.to_string())?;
    let my_pubkey = signer.get_public_key().await.map_err(|e| e.to_string())?;
    let owner_pubkey_b32 = my_pubkey.to_bech32().map_err(|e| e.to_string())?;

    // If caching is requested, attempt to load and verify an existing KeyPackage
    if cache {
        // Load existing keypackage index and verify it exists on relay before returning cached
        let cached_kp_ref: Option<String> = {
            let index = db::load_mls_keypackages(&handle).await.unwrap_or_default();

            index.iter().find(|entry| {
                entry.get("owner_pubkey").and_then(|v| v.as_str()) == Some(owner_pubkey_b32.as_str())
                    && entry.get("device_id").and_then(|v| v.as_str()) == Some(device_id.as_str())
            })
            .and_then(|existing| existing.get("keypackage_ref").and_then(|v| v.as_str()).map(|s| s.to_string()))
        };

        // If we have a cached reference, verify it exists on the relay
        if let Some(ref_id) = cached_kp_ref {
            println!("[MLS][KeyPackage] Found cached reference {}, verifying on relay...", ref_id);
            
            // Try to fetch the event from the relay to verify it exists
            if let Ok(event_id) = nostr_sdk::EventId::from_hex(&ref_id) {
                let filter = Filter::new()
                    .id(event_id)
                    .kind(Kind::MlsKeyPackage)
                    .limit(1);

                match client.stream_events_from(
                    TRUSTED_RELAYS.to_vec(),
                    filter,
                    std::time::Duration::from_secs(5)
                ).await {
                    Ok(mut events) => {
                        // Check if we got any events - if so, the cached KeyPackage exists on relay
                        if events.next().await.is_some() {
                            return Ok(serde_json::json!({
                                "device_id": device_id,
                                "owner_pubkey": owner_pubkey_b32,
                                "keypackage_ref": ref_id,
                                "cached": true
                            }));
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    // Create device KeyPackage using persistent MLS engine inside a no-await scope
    let (kp_encoded, kp_tags) = {
        let mls_service = MlsService::new_persistent(&handle).map_err(|e| e.to_string())?;
        let engine = mls_service.engine().map_err(|e| e.to_string())?;
        let relay_urls: Vec<nostr_sdk::RelayUrl> = TRUSTED_RELAYS
            .iter()
            .filter_map(|r| nostr_sdk::RelayUrl::parse(r).ok())
            .collect();
        engine
            .create_key_package_for_event(&my_pubkey, relay_urls)
            .map_err(|e| e.to_string())?
    }; // engine and mls_service dropped here before any await

    // Build and sign event with nostr client
    let kp_event = client
        .sign_event_builder(EventBuilder::new(Kind::MlsKeyPackage, kp_encoded).tags(kp_tags))
        .await
        .map_err(|e| e.to_string())?;

    // Publish to TRUSTED_RELAYS
    client
        .send_event_to(TRUSTED_RELAYS.iter().copied(), &kp_event)
        .await
        .map_err(|e| e.to_string())?;

    // Upsert into mls_keypackage_index
    {
        let mut index = db::load_mls_keypackages(&handle).await.unwrap_or_default();
        let now = Timestamp::now().as_u64();
        index.push(serde_json::json!({
            "owner_pubkey": owner_pubkey_b32,
            "device_id": device_id,
            "keypackage_ref": kp_event.id.to_hex(),
            "fetched_at": now,
            "expires_at": 0u64
        }));
        let _ = db::save_mls_keypackages(handle.clone(), &index).await;
    }

    Ok(serde_json::json!({
        "device_id": device_id,
        "owner_pubkey": owner_pubkey_b32,
        "keypackage_ref": kp_event.id.to_hex(),
        "cached": false
    }))
}

/// Create a new MLS group with initial member devices
#[tauri::command]
async fn create_mls_group(
    name: String,
    avatar_ref: Option<String>,
    initial_member_devices: Vec<(String, String)>,
) -> Result<String, String> {
    // Use tokio::task::spawn_blocking to run the non-Send MlsService in a blocking context
    tokio::task::spawn_blocking(move || {
        // Get handle in blocking context
        let handle = TAURI_APP.get().ok_or("App handle not initialized")?.clone();
        
        // Use tokio runtime to run async code from blocking context
        let rt = tokio::runtime::Handle::current();
        rt.block_on(async move {
            let mls = MlsService::new_persistent(&handle).map_err(|e| e.to_string())?;
            mls.create_group(&name, avatar_ref.as_deref(), &initial_member_devices)
                .await
                .map_err(|e| e.to_string())
        })
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))?
}

/// Create an MLS group from a group name + member npubs (multi-device aware)
/// - Validates non-empty group name and at least one member
/// - For each member npub, refreshes their latest device keypackage(s)
/// - If any member fails refresh or has zero keypackages, aborts with a clear error
/// - Creates the MLS group and persists metadata so it's immediately discoverable
///
/// Note on device selection policy:
/// - refresh_keypackages_for_contact(npub) returns Vec<(device_id, keypackage_ref)>
/// - For now we choose the first returned device as the member's device to add
///   This can be evolved to pick "newest" by fetched_at if exposed; UI can later allow device selection.
///
/// Frontend will invoke this command via: invoke('create_group_chat', { groupName, memberIds })
#[tauri::command]
async fn create_group_chat(group_name: String, member_ids: Vec<String>) -> Result<String, String> {
    // Input validation
    /*
    Error mapping for UI (Create Group)
    - "Group name must not be empty": validation error. Frontend disables Create until non-empty; if surfaced, show inline status.
    - "Select at least one member to create a group": validation error. Frontend disables Create until at least one contact is selected; if surfaced, show inline status.
    - "Failed to refresh device keypackage for {npub}: {error}": hard failure for a specific member during preflight refresh. Abort creation and show this exact string in popup/toast and inline status.
    - "No device keypackages found for {npub}": hard failure when contact has zero devices/keypackages after refresh. Abort creation and show verbatim.
    - Any error bubbled from create_mls_group(...): engine/storage/network issues are propagated as user-facing strings. Surface them verbatim in the UI.

    Success path
    - Returns group_id (wire id used for relay 'h' tag filtering).
    - Backend also emits "mls_group_initial_sync" so the list view updates without restart.
    */
    let name = group_name.trim();
    if name.is_empty() {
        return Err("Group name must not be empty".to_string());
    }
    if member_ids.is_empty() {
        return Err("Select at least one member to create a group".to_string());
    }

    // For each member id (npub), refresh keypackages and pick one device to add
    let mut initial_member_devices: Vec<(String, String)> = Vec::with_capacity(member_ids.len());

    for npub in member_ids {
        // Attempt to refresh and fetch device keypackages for this contact
        // If this fails for any reason, abort group creation with actionable error text
        let devices = refresh_keypackages_for_contact(npub.clone()).await.map_err(|e| {
            format!("Failed to refresh device keypackage for {}: {}", npub, e)
        })?;

        // Choose a device. Currently: first entry. Future: prefer newest by fetched_at if available.
        let (device_id, _kp_ref) = devices
            .into_iter()
            .next()
            .ok_or_else(|| format!("No device keypackages found for {}", npub))?;

        // Shape required by create_mls_group: (member_npub, device_id)
        initial_member_devices.push((npub, device_id));
    }

    // Delegate to existing helper that persists metadata, publishes welcomes and emits UI events
    // avatar_ref: None for now (out of scope for this subtask)
    let result = create_mls_group(name.to_string(), None, initial_member_devices).await;

    if result.is_ok() {
        tokio::spawn(async {
            if let Err(err) = regenerate_device_keypackage(false).await {
                eprintln!("[MLS] Failed to regenerate device KeyPackage after group creation: {}", err);
            }
        });
    }

    result
}

/// Add a member device to an MLS group
#[tauri::command]
async fn add_mls_member_device(
    group_id: String,
    member_npub: String,
    device_id: String,
) -> Result<(), String> {
    // Run non-Send MLS engine work on a blocking thread; drive async via current runtime
    tokio::task::spawn_blocking(move || {
        let handle = TAURI_APP.get().ok_or("App handle not initialized")?.clone();
        let rt = tokio::runtime::Handle::current();
        rt.block_on(async move {
            let mls = MlsService::new_persistent(&handle).map_err(|e| e.to_string())?;
            mls.add_member_device(&group_id, &member_npub, &device_id)
                .await
                .map_err(|e| e.to_string())
        })
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))?
}

/// Invite a new member to an existing MLS group
/// Similar to create_group_chat, this refreshes the member's keypackages and adds them to the group
#[tauri::command]
async fn invite_member_to_group(
    group_id: String,
    member_npub: String,
) -> Result<(), String> {
    // Refresh keypackages for the new member
    let devices = refresh_keypackages_for_contact(member_npub.clone()).await.map_err(|e| {
        format!("Failed to refresh device keypackage for {}: {}", member_npub, e)
    })?;

    // Choose the first device (same policy as group creation)
    let (device_id, _kp_ref) = devices
        .into_iter()
        .next()
        .ok_or_else(|| format!("No device keypackages found for {}", member_npub))?;

    // Run non-Send MLS engine work on a blocking thread
    let group_id_clone = group_id.clone();
    tokio::task::spawn_blocking(move || {
        let handle = TAURI_APP.get().ok_or("App handle not initialized")?.clone();
        let rt = tokio::runtime::Handle::current();
        rt.block_on(async move {
            let mls = MlsService::new_persistent(&handle).map_err(|e| e.to_string())?;
            mls.add_member_device(&group_id_clone, &member_npub, &device_id)
                .await
                .map_err(|e| e.to_string())
        })
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))??;
    
    // Sync participants array after adding member
    sync_mls_group_participants(group_id).await?;
    
    Ok(())
}

/// Remove a member device from an MLS group
#[tauri::command]
async fn remove_mls_member_device(
    group_id: String,
    member_npub: String,
    device_id: String,
) -> Result<(), String> {
    // Run non-Send MLS engine work on a blocking thread; drive async via current runtime
    let group_id_clone = group_id.clone();
    tokio::task::spawn_blocking(move || {
        let handle = TAURI_APP.get().ok_or("App handle not initialized")?.clone();
        let rt = tokio::runtime::Handle::current();
        rt.block_on(async move {
            let mls = MlsService::new_persistent(&handle).map_err(|e| e.to_string())?;
            mls.remove_member_device(&group_id_clone, &member_npub, &device_id)
                .await
                .map_err(|e| e.to_string())
        })
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))??;
    
    // Sync participants array after removing member
    sync_mls_group_participants(group_id).await?;
    
    Ok(())
}

/// Sync MLS groups with the network
/// If group_id is provided, sync only that group
/// If None, sync all groups (placeholder for now)
#[tauri::command]
async fn sync_mls_groups_now(
    group_id: Option<String>,
) -> Result<(u32, u32), String> {
    // Run non-Send MLS engine work on blocking thread; drive async via current runtime
    tokio::task::spawn_blocking(move || {
        let handle = TAURI_APP.get().ok_or("App handle not initialized")?.clone();
        let rt = tokio::runtime::Handle::current();
        rt.block_on(async move {
            let mls = MlsService::new_persistent(&handle).map_err(|e| e.to_string())?;

            if let Some(id) = group_id {
                // Sync specific group since last cursor
                mls.sync_group_since_cursor(&id)
                    .await
                    .map_err(|e| e.to_string())
            } else {
                // Multi-group sync: load MLS groups from SQL and sync each
                let group_ids: Vec<String> = match db::load_mls_groups(&handle).await {
                    Ok(groups) => {
                        groups.into_iter()
                            .filter(|g| !g.evicted) // Skip evicted groups
                            .map(|g| g.group_id)
                            .collect()
                    }
                    Err(e) => {
                        eprintln!("Failed to load MLS groups: {}", e);
                        Vec::new()
                    }
                };

                let mut total_processed: u32 = 0;
                let mut total_new: u32 = 0;

                for gid in group_ids {
                    match mls.sync_group_since_cursor(&gid).await {
                        Ok((processed, new_msgs)) => {
                            total_processed = total_processed.saturating_add(processed);
                            total_new = total_new.saturating_add(new_msgs);
                        }
                        Err(e) => {
                            eprintln!("[MLS] sync_group_since_cursor failed for {}: {}", gid, e);
                        }
                    }
                    
                    // Sync participants array to ensure it matches actual group members
                    if let Err(e) = sync_mls_group_participants(gid.clone()).await {
                        eprintln!("[MLS] Failed to sync participants for group {}: {}", gid, e);
                    }
                }

                Ok((total_processed, total_new))
            }
        })
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))?
}


/// Simplified representation of a pending MLS Welcome for UI
#[derive(serde::Serialize)]
struct SimpleWelcome {
    // Welcome event id (rumor id) hex
    id: String,
    // Wrapper id carrying the welcome (giftwrap id) hex
    wrapper_event_id: String,
    // Group metadata
    nostr_group_id: String,
    group_name: String,
    group_description: Option<String>,
    group_image_url: Option<String>,
    // Admins (npub strings if possible are not available here; expose hex pubkeys)
    group_admin_pubkeys: Vec<String>,
    // Relay URLs
    group_relays: Vec<String>,
    // Welcomer (hex)
    welcomer: String,
    member_count: u32,
}

/// List pending MLS welcomes (invites)
#[tauri::command]
async fn list_pending_mls_welcomes() -> Result<Vec<SimpleWelcome>, String> {
    // Run non-Send MLS engine work on blocking thread; drive async via current runtime
    let welcomes: Vec<SimpleWelcome> = tokio::task::spawn_blocking(move || {
        let handle = TAURI_APP.get().ok_or("App handle not initialized")?.clone();
        let rt = tokio::runtime::Handle::current();
        rt.block_on(async move {
            let mls = MlsService::new_persistent(&handle).map_err(|e| e.to_string())?;
            let engine = mls.engine().map_err(|e| e.to_string())?;

            let pending = engine.get_pending_welcomes().map_err(|e| e.to_string())?;

            let mut out: Vec<SimpleWelcome> = Vec::with_capacity(pending.len());
            for w in pending {
                out.push(SimpleWelcome {
                    id: w.id.to_hex(),
                    wrapper_event_id: w.wrapper_event_id.to_hex(),
                    nostr_group_id: hex::encode(w.nostr_group_id),
                    group_name: w.group_name.clone(),
                    group_description: Some(w.group_description.clone()),
                    group_image_url: None, // MDK uses group_image_hash/key/nonce instead of URL
                    group_admin_pubkeys: w.group_admin_pubkeys.iter()
                        .filter_map(|pk| pk.to_bech32().ok())
                        .collect(),
                    group_relays: w.group_relays.iter().map(|r| r.to_string()).collect(),
                    welcomer: w.welcomer.to_bech32().map_err(|e| e.to_string())?,
                    member_count: w.member_count,
                });
            }

            Ok::<Vec<SimpleWelcome>, String>(out)
        })
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))??;
    
    // Send notifications for new welcomes (outside blocking task)
    // Only notify for welcomes we haven't notified about before
    {
        let mut notified = NOTIFIED_WELCOMES.lock().await;
        
        for welcome in &welcomes {
            // Skip if we've already notified about this welcome
            if notified.contains(&welcome.wrapper_event_id) {
                continue;
            }
            
            // Get inviter's display name
            let inviter_name = {
                let state = STATE.lock().await;
                if let Some(profile) = state.get_profile(&welcome.welcomer) {
                    if !profile.nickname.is_empty() {
                        profile.nickname.clone()
                    } else if !profile.name.is_empty() {
                        profile.name.clone()
                    } else {
                        "Someone".to_string()
                    }
                } else {
                    "Someone".to_string()
                }
            };
            
            let notification = NotificationData::group_invite(welcome.group_name.clone(), inviter_name);
            show_notification_generic(notification);
            
            // Mark this welcome as notified
            notified.insert(welcome.wrapper_event_id.clone());
        }
    }
    
    Ok(welcomes)
}

/// Accept an MLS welcome by its welcome (rumor) event id hex
#[tauri::command]
async fn accept_mls_welcome(welcome_event_id_hex: String) -> Result<bool, String> {
    // Run non-Send MLS engine work on blocking thread; drive async via current runtime
    let accepted = tokio::task::spawn_blocking(move || {
        let handle = TAURI_APP.get().ok_or("App handle not initialized")?.clone();
        let rt = tokio::runtime::Handle::current();
        rt.block_on(async move {
            let mls = MlsService::new_persistent(&handle).map_err(|e| e.to_string())?;
            
            // Get welcome details and accept it (engine work in no-await scope)
            let (nostr_group_id, engine_group_id, group_name, welcomer_hex, wrapper_event_id_hex) = {
                let engine = mls.engine().map_err(|e| e.to_string())?;
                
                let id = nostr_sdk::EventId::from_hex(&welcome_event_id_hex).map_err(|e| e.to_string())?;
                let welcome_opt = engine.get_welcome(&id).map_err(|e| e.to_string())?;
                let welcome = welcome_opt.ok_or_else(|| "Welcome not found".to_string())?;
                
                // Extract metadata before accepting
                let nostr_group_id_bytes = welcome.nostr_group_id.clone();
                let group_name = welcome.group_name.clone();
                let welcomer_hex = welcome.welcomer.to_hex();
                let wrapper_event_id_hex = welcome.wrapper_event_id.to_hex();

                // Accept the welcome - this updates engine state internally
                engine.accept_welcome(&welcome).map_err(|e| e.to_string())?;
                
                // The nostr_group_id is used for wire protocol (h tag on relays)
                let nostr_group_id = hex::encode(&nostr_group_id_bytes);
                
                // After accepting the welcome, get the actual group from the engine to find its internal ID
                // This follows the pattern from the SDK example
                let engine_group_id = {
                    // Get all groups from the engine (should include the one we just joined)
                    let groups = engine.get_groups()
                        .map_err(|e| format!("Failed to get groups after accepting welcome: {}", e))?;
                    
                    // Find the group that matches our nostr_group_id
                    let matching_group = groups.iter()
                        .find(|g| hex::encode(&g.nostr_group_id) == nostr_group_id);
                    
                    if let Some(group) = matching_group {
                        // Found the group - use its internal MLS group ID
                        let engine_id = hex::encode(group.mls_group_id.as_slice());
                        println!("[MLS] Found group in engine after accept:");
                        println!("[MLS]   - nostr_group_id matches: {}", nostr_group_id);
                        println!("[MLS]   - engine mls_group_id: {}", engine_id);
                        engine_id
                    } else {
                        // This shouldn't happen, but fallback to nostr_group_id
                        eprintln!("[MLS] Warning: Could not find group in engine after accepting welcome");
                        eprintln!("[MLS] Groups in engine: {}", groups.len());
                        for g in groups.iter() {
                            eprintln!("[MLS]   - Group: nostr_id={}, mls_id={}",
                                     hex::encode(&g.nostr_group_id),
                                     hex::encode(g.mls_group_id.as_slice()));
                        }
                        // Use the nostr_group_id as fallback
                        nostr_group_id.clone()
                    }
                };
                
                // Log for debugging
                println!("[MLS] Welcome accepted:");
                println!("[MLS]   - wire_id (h tag): {}", nostr_group_id);
                println!("[MLS]   - engine_group_id: {}", engine_group_id);
                println!("[MLS]   - group_name: {}", group_name);
                
                (nostr_group_id, engine_group_id, group_name, welcomer_hex, wrapper_event_id_hex)
            }; // engine dropped here
            
            // Now persist the group metadata (awaitable section)
            let mut groups = mls.read_groups().await.map_err(|e| e.to_string())?;
            
            // Check if group already exists or was previously evicted
            let existing_index = groups.iter().position(|g| g.group_id == nostr_group_id);
            
            if let Some(idx) = existing_index {
                // Group exists - check if it was evicted and we're being re-invited
                if groups[idx].evicted {
                    println!("[MLS] Re-invited to previously evicted group: {}", nostr_group_id);
                    // Clear the evicted flag and update metadata
                    groups[idx].evicted = false;
                    groups[idx].updated_at = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_err(|e| e.to_string())?
                        .as_secs();
                    // Update only the specific group instead of all groups
                    crate::db::save_mls_group(handle.clone(), &groups[idx]).await.map_err(|e| e.to_string())?;
                    mls::emit_group_metadata_event(&groups[idx]);
                } else {
                    println!("[MLS] Group already exists in metadata: group_id={}", nostr_group_id);
                }
            } else {
                // Build metadata for the accepted group
                let now_secs = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_err(|e| e.to_string())?
                    .as_secs();
                
                let metadata = mls::MlsGroupMetadata {
                    group_id: nostr_group_id.clone(),         // Wire ID for relay filtering (h tag)
                    engine_group_id: engine_group_id.clone(), // Internal engine ID for local operations
                    creator_pubkey: welcomer_hex,             // The welcomer becomes the creator from our perspective
                    name: group_name,
                    avatar_ref: None,
                    created_at: now_secs,
                    updated_at: now_secs,
                    evicted: false,                           // Accepting a welcome means we're joining, not evicted
                };
                
                crate::db::save_mls_group(handle.clone(), &metadata).await.map_err(|e| e.to_string())?;
                mls::emit_group_metadata_event(&metadata);
                
                // Create the Chat in STATE with metadata and save to disk
                {
                    let mut state = STATE.lock().await;
                    let chat_id = state.create_or_get_mls_group_chat(&nostr_group_id, vec![]);
                    
                    // Set metadata from MlsGroupMetadata
                    if let Some(chat) = state.get_chat_mut(&chat_id) {
                        chat.metadata.set_name(metadata.name.clone());
                        // Member count will be updated during sync when we process messages
                    }
                    
                    // Save chat to disk
                    if let Some(chat) = state.get_chat(&chat_id) {
                        if let Err(e) = db::save_chat(handle.clone(), chat).await {
                            eprintln!("[MLS] Failed to save chat after welcome acceptance: {}", e);
                        }
                    }
                }
                
                println!("[MLS] Persisted group metadata after accept: group_id={}", nostr_group_id);
            }

            // Remove this welcome from the notified set since it's been accepted
            {
                let mut notified = NOTIFIED_WELCOMES.lock().await;
                notified.remove(&wrapper_event_id_hex);
            }
            
            // Emit event so the UI can refresh welcome lists and group lists
            if let Some(app) = TAURI_APP.get() {
                let _ = app.emit("mls_welcome_accepted", serde_json::json!({
                    "welcome_event_id": welcome_event_id_hex,
                    "group_id": nostr_group_id
                }));
            }

            // Sync the participants array with actual group members from the engine
            if let Err(e) = sync_mls_group_participants(nostr_group_id.clone()).await {
                eprintln!("[MLS] Failed to sync participants after welcome accept: {}", e);
            }

            // Immediately prefetch recent MLS messages for this group so the chat list shows previews
            // and ordering without requiring the user to open the chat. This loads a recent slice
            // (48h window by default in sync_group_since_cursor) rather than full history.
            match mls.sync_group_since_cursor(&nostr_group_id).await {
                Ok((processed, new_msgs)) => {
                    println!("[MLS] Post-accept initial sync: processed={}, new={}", processed, new_msgs);
                    // Optional: let UI know initial sync finished for this group
                    if let Some(app) = TAURI_APP.get() {
                        let _ = app.emit("mls_group_initial_sync", serde_json::json!({
                            "group_id": nostr_group_id,
                            "processed": processed,
                            "new": new_msgs
                        }));
                    }
                }
                Err(e) => {
                    eprintln!("[MLS] Post-accept initial sync failed for group {}: {}", nostr_group_id, e);
                }
            }

            Ok::<bool, String>(true)
        })
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))??;

    if accepted {
        tokio::spawn(async {
            if let Err(err) = regenerate_device_keypackage(false).await {
                eprintln!("[MLS] Failed to regenerate device KeyPackage after accepting welcome: {}", err);
            }
        });
    }

    Ok(accepted)
}

#[tauri::command]
async fn list_mls_groups() -> Result<Vec<String>, String> {
    let handle = TAURI_APP.get().ok_or("App handle not initialized")?.clone();
    match db::load_mls_groups(&handle).await {
        Ok(groups) => {
            let ids = groups.into_iter()
                .map(|g| g.group_id)
                .collect();
            Ok(ids)
        }
        Err(e) => Err(format!("Failed to load MLS groups: {}", e)),
    }
}

#[tauri::command]
async fn get_mls_group_metadata() -> Result<Vec<serde_json::Value>, String> {
    let handle = TAURI_APP.get().ok_or("App handle not initialized")?.clone();
    let groups = db::load_mls_groups(&handle)
        .await
        .map_err(|e| format!("Failed to load MLS group metadata: {}", e))?;

    Ok(groups
        .iter()
        .filter(|meta| !meta.evicted)
        .map(|meta| mls::metadata_to_frontend(meta))
        .collect())
}

#[derive(serde::Serialize, Clone)]
struct GroupMembers {
    group_id: String,
    members: Vec<String>, // npubs
    admins: Vec<String>,  // admin npubs
}

/// Sync the participants array for an MLS group chat with the actual members from the engine
/// This ensures chat.participants is always up-to-date
async fn sync_mls_group_participants(group_id: String) -> Result<(), String> {
    // Get actual members from the engine
    let group_members = get_mls_group_members(group_id.clone()).await?;
    
    // Update the chat's participants array
    let mut state = STATE.lock().await;
    if let Some(chat) = state.get_chat_mut(&group_id) {
        let old_count = chat.participants.len();
        chat.participants = group_members.members.clone();
        let new_count = chat.participants.len();
        
        if old_count != new_count {
            eprintln!(
                "[MLS] Synced participants for group {}: {} -> {} members",
                &group_id[..8],
                old_count,
                new_count
            );
        }
        
        // Save updated chat to disk
        let chat_clone = chat.clone();
        drop(state);
        
        if let Some(handle) = TAURI_APP.get() {
            if let Err(e) = db::save_chat(handle.clone(), &chat_clone).await {
                eprintln!("[MLS] Failed to save chat after syncing participants: {}", e);
            }
        }
    } else {
        drop(state);
        eprintln!("[MLS] Chat not found when syncing participants: {}", group_id);
    }
    
    Ok(())
}

/// Get members (npubs) of an MLS group from the persistent engine (on-demand)
#[tauri::command]
async fn get_mls_group_members(group_id: String) -> Result<GroupMembers, String> {
    // Run engine operations on a blocking thread so the outer future is Send
    tokio::task::spawn_blocking(move || {
        let handle = TAURI_APP.get().ok_or("App handle not initialized")?.clone();
        let rt = tokio::runtime::Handle::current();
        rt.block_on(async move {
            // Initialise persistent MLS
            let mls = MlsService::new_persistent(&handle).map_err(|e| e.to_string())?;
            // Map wire-id/engine-id using encrypted metadata
            let meta_groups = mls.read_groups().await.unwrap_or_default();
            let (wire_id, engine_id) = if let Some(m) = meta_groups
                .iter()
                .find(|g| g.group_id == group_id || (!g.engine_group_id.is_empty() && g.engine_group_id == group_id))
            {
                (
                    m.group_id.clone(),
                    if !m.engine_group_id.is_empty() { m.engine_group_id.clone() } else { m.group_id.clone() },
                )
            } else {
                (group_id.clone(), group_id.clone())
            };

            // Acquire non-Send engine; all calls below must be non-await while engine is in scope
            let engine = mls.engine().map_err(|e| e.to_string())?;
            use mdk_core::prelude::GroupId;

            let mut members: Vec<String> = Vec::new();
            let mut admins: Vec<String> = Vec::new();
            if let Ok(gid_bytes) = hex::decode(&engine_id) {
                // Decode engine id to GroupId
                let gid = GroupId::from_slice(&gid_bytes);
                
                // Get members via engine API
                if let Ok(pk_list) = engine.get_members(&gid) {
                    members = pk_list
                        .into_iter()
                        .filter_map(|pk| pk.to_bech32().ok())
                        .collect();
                }
                
                // Get admins from the group
                if let Ok(groups) = engine.get_groups() {
                    for g in groups {
                        let gid_hex = hex::encode(g.mls_group_id.as_slice());
                        if gid_hex == engine_id {
                            admins = g.admin_pubkeys.iter()
                                .filter_map(|pk| pk.to_bech32().ok())
                                .collect();
                            break;
                        }
                    }
                }
            }

            Ok(GroupMembers {
                group_id: wire_id,
                members,
                admins,
            })
        })
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))?
}

/// Leave an MLS group
/// TODO: Implement MLS leave operation
#[tauri::command]
async fn leave_mls_group(group_id: String) -> Result<(), String> {
    // Run non-Send MLS engine work on a blocking thread
    tokio::task::spawn_blocking(move || {
        let handle = TAURI_APP.get().ok_or("App handle not initialized")?.clone();
        let rt = tokio::runtime::Handle::current();
        rt.block_on(async move {
            let mls = MlsService::new_persistent(&handle).map_err(|e| e.to_string())?;
            mls.leave_group(&group_id)
                .await
                .map_err(|e| e.to_string())
        })
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))?
}

//// Refresh keypackages for a contact from TRUSTED_RELAY
//// Fetches Kind::MlsKeyPackage from the contact, updates local index, and returns (device_id, keypackage_ref)
#[tauri::command]
async fn refresh_keypackages_for_contact(
    npub: String,
) -> Result<Vec<(String, String)>, String> {
    // Resolve contact pubkey
    let contact_pubkey = PublicKey::from_bech32(&npub).map_err(|e| e.to_string())?;

    // Access client
    let client = NOSTR_CLIENT.get().ok_or("Nostr client not initialized")?;

    // Build filter: author(contact) + MlsKeyPackage
    let filter = Filter::new()
        .author(contact_pubkey)
        .kind(Kind::MlsKeyPackage)
        // Only need the newest KeyPackage
        .limit(1);

    // Fetch from TRUSTED_RELAYS with short timeout
    let mut events = client
        .stream_events_from(TRUSTED_RELAYS.to_vec(), filter, std::time::Duration::from_secs(10))
        .await
        .map_err(|e| e.to_string())?;

    // Prepare results and index entries
    let owner_pubkey_b32 = contact_pubkey.to_bech32().map_err(|e| e.to_string())?;
    let mut results: Vec<(String, String)> = Vec::new();
    let mut new_entries: Vec<serde_json::Value> = Vec::new();

    while let Some(e) = events.next().await {
        // Use event id as synthetic device_id when not explicitly provided by remote
        let device_id = e.id.to_hex();
        let keypackage_ref = e.id.to_hex();

        results.push((device_id.clone(), keypackage_ref.clone()));

        new_entries.push(serde_json::json!({
            "owner_pubkey": owner_pubkey_b32,
            "device_id": device_id,
            "keypackage_ref": keypackage_ref,
            "fetched_at": Timestamp::now().as_u64(),
            "expires_at": 0u64
        }));
    }

    // Update local plaintext index after network await
    let handle = TAURI_APP.get().ok_or("App handle not initialized")?.clone();

    // Load existing index
    let mut index = db::load_mls_keypackages(&handle).await.unwrap_or_default();

    // Remove any existing entries for this owner+device_id to avoid duplicates
    for new_entry in &new_entries {
        let owner = new_entry.get("owner_pubkey").and_then(|v| v.as_str()).unwrap_or_default();
        let device = new_entry.get("device_id").and_then(|v| v.as_str()).unwrap_or_default();
        index.retain(|entry| {
            let same_owner = entry.get("owner_pubkey").and_then(|v| v.as_str()) == Some(owner);
            let same_device = entry.get("device_id").and_then(|v| v.as_str()) == Some(device);
            !(same_owner && same_device)
        });
    }

    // Append new entries and persist
    index.extend(new_entries.into_iter());
    let _ = db::save_mls_keypackages(handle.clone(), &index).await;

    Ok(results)
}

/// Check MLS group health and identify groups that need re-syncing

/// Remove orphaned MLS groups from metadata that are not in engine state

#[tauri::command]
async fn queue_profile_sync(npub: String, priority: String, force_refresh: bool) -> Result<(), String> {
    let sync_priority = match priority.as_str() {
        "critical" => profile_sync::SyncPriority::Critical,
        "high" => profile_sync::SyncPriority::High,
        "medium" => profile_sync::SyncPriority::Medium,
        "low" => profile_sync::SyncPriority::Low,
        _ => return Err(format!("Invalid priority: {}", priority)),
    };
    
    profile_sync::queue_profile_sync(npub, sync_priority, force_refresh).await;
    Ok(())
}

#[tauri::command]
async fn queue_chat_profiles_sync(chat_id: String, is_opening: bool) -> Result<(), String> {
    profile_sync::queue_chat_profiles(chat_id, is_opening).await;
    Ok(())
}

#[tauri::command]
async fn refresh_profile_now(npub: String) -> Result<(), String> {
    profile_sync::refresh_profile_now(npub).await;
    Ok(())
}

#[tauri::command]
async fn sync_all_profiles() -> Result<(), String> {
    profile_sync::sync_all_profiles().await;
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    #[cfg(target_os = "linux")]
    {
        // WebKitGTK can be quite funky cross-platform: as a result, we'll fallback to a more compatible renderer
        // In theory, this will make Vector run more consistently across a wider range of Linux Desktop distros.
        std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
    }

    let mut builder = tauri::Builder::default()
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_deep_link::init())
        // Register the webxdc:// custom protocol for Mini Apps (async to avoid deadlocks on Windows)
        .register_asynchronous_uri_scheme_protocol("webxdc", miniapps::scheme::miniapp_protocol)
        // Register Mini Apps state
        .manage(miniapps::state::MiniAppsState::new());

    // MCP Bridge plugin for AI-assisted debugging (desktop debug builds only)
    #[cfg(all(debug_assertions, desktop))]
    {
        builder = builder.plugin(tauri_plugin_mcp_bridge::init());
    }

    // Desktop-only plugins
    #[cfg(desktop)]
    {
        // Window state plugin: saves and restores window position, size, maximized state, etc.
        // Exclude VISIBLE flag so window starts hidden (we show it after content loads to prevent white flash)
        use tauri_plugin_window_state::StateFlags;
        builder = builder.plugin(
            tauri_plugin_window_state::Builder::new()
                .with_state_flags(StateFlags::all() & !StateFlags::VISIBLE)
                .build()
        );
        
        // Single-instance plugin: ensures deep links are passed to existing instance
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            // Handle deep links from single-instance (Windows/Linux)
            let urls: Vec<String> = args.iter()
                .filter(|arg| arg.starts_with("vector://") || arg.contains("vectorapp.io"))
                .cloned()
                .collect();
            if !urls.is_empty() {
                deep_link::handle_deep_link(app, urls);
            }
            // Focus the existing window
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.set_focus();
            }
        }));
    }

    builder
        .setup(|app| {
            #[cfg(desktop)]
            app.handle().plugin(tauri_plugin_updater::Builder::new().build())?;
            #[cfg(desktop)]
            app.handle().plugin(tauri_plugin_process::init())?;
            
            let handle = app.app_handle().clone();

            // Setup a graceful shutdown for our Nostr subscriptions
            let window = app.get_webview_window("main").unwrap();
            #[cfg(desktop)]
            let handle_for_window_state = handle.clone();
            window.on_window_event(move |event| {
                match event {
                    // This catches when the window is being closed
                    tauri::WindowEvent::CloseRequested { .. } => {
                        // Save window state (position, size, maximized, etc.) before closing
                        #[cfg(desktop)]
                        {
                            use tauri_plugin_window_state::{AppHandleExt, StateFlags};
                            let _ = handle_for_window_state.save_window_state(StateFlags::all());
                        }
                        
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

            // Auto-select account on startup if one exists but isn't selected
            {
                let handle_clone = handle.clone();
                let _ = account_manager::auto_select_account(&handle_clone);
            }

            // Startup log: persistent MLS device_id if present
            {
                let handle_clone = handle.clone();
                tauri::async_runtime::spawn(async move {
                    if let Ok(Some(id)) = db::load_mls_device_id(&handle_clone).await {
                        println!("[MLS] Found persistent mls_device_id at startup: {}", id);
                    }
                });
            }

            // Set as our accessible static app handle
            TAURI_APP.set(handle.clone()).unwrap();
            
            // Start the profile sync background processor
            tauri::async_runtime::spawn(async {
                profile_sync::start_profile_sync_processor().await;
            });
            
            // Start the Mini Apps pending peer cleanup task (runs every 5 minutes)
            {
                let handle_for_cleanup = handle.clone();
                tauri::async_runtime::spawn(async move {
                    loop {
                        // Wait 5 minutes between cleanups
                        tokio::time::sleep(std::time::Duration::from_secs(300)).await;
                        
                        // Get the MiniAppsState and run cleanup
                        let state = handle_for_cleanup.state::<miniapps::state::MiniAppsState>();
                        state.cleanup_expired_pending_peers().await;
                    }
                });
            }

            // Setup deep link listener for macOS/iOS/Android
            // On these platforms, deep links are received as events rather than CLI args
            #[cfg(any(target_os = "macos", target_os = "ios", target_os = "android"))]
            {
                use tauri_plugin_deep_link::DeepLinkExt;
                let handle_for_deep_link = handle.clone();
                let _listener_id = app.deep_link().on_open_url(move |event| {
                    let urls: Vec<String> = event.urls().iter().map(|u| u.to_string()).collect();
                    deep_link::handle_deep_link(&handle_for_deep_link, urls);
                });
            }
            
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            db::get_theme,
            db::get_pkey,
            db::set_pkey,
            db::get_seed,
            db::set_seed,
            db::get_sql_setting,
            db::set_sql_setting,
            db::remove_setting,
            profile::load_profile,
            profile::update_profile,
            profile::update_status,
            profile::upload_avatar,
            chat::mark_as_read,
            profile::toggle_muted,
            profile::set_nickname,
            message::message,
            message::paste_message,
            message::voice_message,
            message::file_message,
            message::file_message_compressed,
            message::forward_attachment,
            message::get_file_info,
            message::cache_android_file,
            message::cache_file_bytes,
            message::get_cached_file_info,
            message::get_cached_image_preview,
            message::start_cached_bytes_compression,
            message::get_cached_bytes_compression_status,
            message::send_cached_file,
            message::send_file_bytes,
            message::clear_cached_file,
            message::clear_android_file_cache,
            message::clear_all_android_file_cache,
            message::get_image_preview_base64,
            message::start_image_precompression,
            message::get_compression_status,
            message::clear_compression_cache,
            message::send_cached_compressed_file,
            message::react_to_message,
            message::edit_message,
            message::fetch_msg_metadata,
            fetch_messages,
            deep_rescan,
            is_scanning,
            get_chat_messages_paginated,
            get_message_views,
            get_messages_around_id,
            get_chat_message_count,
            get_file_hash_index,
            evict_chat_messages,
            generate_blurhash_preview,
            decode_blurhash,
            download_attachment,
            login,
            #[cfg(debug_assertions)]
            debug_hot_reload_sync,
            notifs,
            get_relays,
            get_media_servers,
            // Custom relay management
            get_custom_relays,
            add_custom_relay,
            remove_custom_relay,
            toggle_custom_relay,
            toggle_default_relay,
            update_relay_mode,
            validate_relay_url_cmd,
            get_relay_metrics,
            get_relay_logs,
            monitor_relay_connections,
            start_typing,
            send_webxdc_peer_advertisement,
            connect,
            encrypt,
            decrypt,
            start_recording,
            stop_recording,
            update_unread_counter,
            logout,
            create_account,
            get_platform_features,
            transcribe,
            download_whisper_model,
            get_or_create_invite_code,
            accept_invite_code,
            get_invited_users,
            check_fawkes_badge,
            get_storage_info,
            clear_storage,
            load_mls_device_id,
            load_mls_keypackages,
            export_keys,
            regenerate_device_keypackage,
            // MLS core commands
            create_group_chat,
            create_mls_group,
            sync_mls_groups_now,
            list_mls_groups,
            get_mls_group_metadata,
            // MLS welcome/invite commands
            list_pending_mls_welcomes,
            accept_mls_welcome,
            // MLS advanced helpers
            add_mls_member_device,
            invite_member_to_group,
            remove_mls_member_device,
            get_mls_group_members,
            leave_mls_group,
            list_group_cursors,
            refresh_keypackages_for_contact,
            // Profile sync commands
            queue_profile_sync,
            queue_chat_profiles_sync,
            refresh_profile_now,
            sync_all_profiles,
            // Deep link commands
            deep_link::get_pending_deep_link,
            // Account manager commands
            account_manager::get_current_account,
            account_manager::list_all_accounts,
            account_manager::check_any_account_exists,
            account_manager::switch_account,
            // Mini Apps commands
            miniapps::commands::miniapp_load_info,
            miniapps::commands::miniapp_load_info_from_bytes,
            miniapps::commands::miniapp_open,
            miniapps::commands::miniapp_close,
            miniapps::commands::miniapp_get_updates,
            miniapps::commands::miniapp_send_update,
            miniapps::commands::miniapp_list_open,
            // Mini Apps realtime channel commands (Iroh P2P)
            miniapps::commands::miniapp_join_realtime_channel,
            miniapps::commands::miniapp_send_realtime_data,
            miniapps::commands::miniapp_leave_realtime_channel,
            miniapps::commands::miniapp_add_realtime_peer,
            miniapps::commands::miniapp_get_realtime_node_addr,
            miniapps::commands::miniapp_get_realtime_status,
            // Mini Apps history commands
            miniapps::commands::miniapp_record_opened,
            miniapps::commands::miniapp_get_history,
            miniapps::commands::miniapp_remove_from_history,
            miniapps::commands::miniapp_toggle_favorite,
            miniapps::commands::miniapp_set_favorite,
            // Mini Apps marketplace commands
            miniapps::commands::marketplace_fetch_apps,
            miniapps::commands::marketplace_get_cached_apps,
            miniapps::commands::marketplace_get_app,
            miniapps::commands::marketplace_get_app_by_hash,
            miniapps::commands::marketplace_get_install_status,
            miniapps::commands::marketplace_install_app,
            miniapps::commands::marketplace_check_installed,
            miniapps::commands::marketplace_sync_install_status,
            miniapps::commands::marketplace_add_trusted_publisher,
            miniapps::commands::marketplace_open_app,
            miniapps::commands::marketplace_uninstall_app,
            miniapps::commands::marketplace_update_app,
            miniapps::commands::marketplace_publish_app,
            miniapps::commands::marketplace_get_trusted_publisher,
            // Mini App permissions commands
            miniapps::commands::miniapp_get_available_permissions,
            miniapps::commands::miniapp_get_granted_permissions,
            miniapps::commands::miniapp_get_granted_permissions_for_window,
            miniapps::commands::miniapp_set_permission,
            miniapps::commands::miniapp_set_permissions,
            miniapps::commands::miniapp_has_permission_prompt,
            miniapps::commands::miniapp_revoke_all_permissions,
            // Image cache commands
            image_cache::get_or_cache_image,
            image_cache::clear_image_cache,
            image_cache::get_image_cache_stats,
            image_cache::cache_url_image,
            // PIVX Promos commands
            pivx::pivx_create_promo,
            pivx::pivx_get_promo_balance,
            pivx::pivx_get_wallet_balance,
            pivx::pivx_list_promos,
            pivx::pivx_sweep_promo,
            pivx::pivx_set_wallet_address,
            pivx::pivx_get_wallet_address,
            pivx::pivx_claim_from_message,
            pivx::pivx_import_promo,
            pivx::pivx_refresh_balances,
            pivx::pivx_send_payment,
            pivx::pivx_send_existing_promo,
            pivx::pivx_get_chat_payments,
            pivx::pivx_check_address_balance,
            pivx::pivx_withdraw,
            pivx::pivx_get_currencies,
            pivx::pivx_get_price,
            pivx::pivx_set_preferred_currency,
            pivx::pivx_get_preferred_currency,
            // Notification sound commands (desktop only)
            #[cfg(desktop)]
            audio::get_notification_settings,
            #[cfg(desktop)]
            audio::set_notification_settings,
            #[cfg(desktop)]
            audio::preview_notification_sound,
            #[cfg(desktop)]
            audio::select_custom_notification_sound,
            // Maintenance (periodic cleanup tasks)
            run_maintenance,
            #[cfg(all(not(target_os = "android"), feature = "whisper"))]
            whisper::delete_whisper_model,
            #[cfg(all(not(target_os = "android"), feature = "whisper"))]
            whisper::list_models
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
