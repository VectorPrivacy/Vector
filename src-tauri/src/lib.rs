use std::borrow::Cow;
use lazy_static::lazy_static;
use nostr_sdk::prelude::*;
use once_cell::sync::OnceCell;
use tokio::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager, Runtime, WebviewWindowBuilder, WebviewUrl};
use tauri_plugin_notification::NotificationExt;
use rand::{thread_rng, Rng};
use rand::distributions::Alphanumeric;

mod crypto;

mod db;
use db::SlimProfile;

mod mls;
pub use mls::MlsService;


mod db_migration;
use db_migration::save_chat_messages;

mod voice;
use voice::AudioRecorder;

mod net;

mod upload;

mod util;
use util::{get_file_type_description, calculate_file_hash, is_nonce_filename, migrate_nonce_file_to_hash};

#[cfg(target_os = "android")]
mod android {
    pub mod clipboard;
    pub mod filesystem;
    pub mod permissions;
    pub mod utils;
}

#[cfg(all(not(target_os = "android"), feature = "whisper"))]
mod whisper;

mod message;
pub use message::{Message, Attachment, Reaction};

mod profile;
pub use profile::{Profile, Status};

mod chat;
pub use chat::{Chat, ChatType, ChatMetadata};

mod rumor;
pub use rumor::{RumorEvent, RumorContext, RumorProcessingResult, ConversationType, process_rumor};

/// # Trusted Relay
///
/// The 'Trusted Relay' handles events that MAY have a small amount of public-facing metadata attached (i.e: Expiration tags).
///
/// This relay may be used for events like Typing Indicators, Key Exchanges (forward-secrecy setup) and more.
pub(crate) static TRUSTED_RELAY: &str = "wss://jskitty.cat/nostr";

/// # Trusted Public NIP-96 Server
///
/// A temporary hardcoded NIP-96 server, handling file uploads for public files (Avatars, etc)
static TRUSTED_PUBLIC_NIP96: &str = "https://nostr.build";
static PUBLIC_NIP96_CONFIG: OnceCell<ServerConfig> = OnceCell::new();

/// # Trusted Private NIP-96 Server
///
/// A temporary hardcoded NIP-96 server, handling file uploads for encrypted files (in-chat)
static TRUSTED_PRIVATE_NIP96: &str = "https://medea-1-swiss.vectorapp.io";
static PRIVATE_NIP96_CONFIG: OnceCell<ServerConfig> = OnceCell::new();


static MNEMONIC_SEED: OnceCell<String> = OnceCell::new();
static ENCRYPTION_KEY: OnceCell<[u8; 32]> = OnceCell::new();
pub(crate) static NOSTR_CLIENT: OnceCell<Client> = OnceCell::new();
pub(crate) static TAURI_APP: OnceCell<AppHandle> = OnceCell::new();
// TODO: REMOVE AFTER SEVERAL UPDATES - This static is only needed for the one-time migration from nonce-based to hash-based storage
static PENDING_MIGRATION: OnceCell<std::collections::HashMap<String, (String, String)>> = OnceCell::new();

#[derive(Clone)]
struct PendingInviteAcceptance {
    invite_code: String,
    inviter_pubkey: PublicKey,
}

static PENDING_INVITE: OnceCell<PendingInviteAcceptance> = OnceCell::new();




#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
enum SyncMode {
    ForwardSync,   // Initial sync from most recent message going backward
    BackwardSync,  // Syncing historically old messages
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
                // For now, we'll create a basic chat - in the future this should be more sophisticated
                let mut chat = Chat::new(chat_id.to_string(), ChatType::DirectMessage, vec![]);
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
            
            let unread_count = if last_read_id.is_empty() {
                // No last_read_id set - walk backwards from the end to find unread messages
                let mut unread_messages = 0;
                // Iterate messages in reverse order (newest first)
                for msg in chat.messages.iter().rev() {
                    if msg.mine {
                        // If we find our own message first, everything before it is considered read
                        // (because we responded to those messages)
                        break;
                    } else {
                        // Count non-mine messages (unread)
                        unread_messages += 1;
                    }
                }
                unread_messages
            } else {
                // Last read message ID is set - count messages after it
                if let Some(last_read_index) = chat.messages.iter().position(|msg| msg.id == *last_read_id) {
                    // Count messages after the last read message that are not mine
                    chat.messages.iter().skip(last_read_index + 1).filter(|msg| !msg.mine).count()
                } else {
                    // If last_read_id not found, fall back to walking backwards
                    let mut unread_messages = 0;
                    // Iterate messages in reverse order (newest first)
                    for msg in chat.messages.iter().rev() {
                        if msg.mine {
                            // If we find our own message first, everything before it is considered read
                            break;
                        } else {
                            // Count non-mine messages (unread)
                            unread_messages += 1;
                        }
                    }
                    unread_messages
                }
            };
            
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
        let events = client
            .fetch_events_from(vec![relay_url.unwrap()], filter, std::time::Duration::from_secs(30))
            .await
            .unwrap();

        // Process events without affecting global sync state
        for event in events.into_iter() {
            handle_event(event, false).await;
        }
        
        return; // Exit early for single-relay syncs
    }

    // Regular sync logic with global state management
    let (since_timestamp, until_timestamp) = {
        let mut state = STATE.lock().await;
        
        if init {
            // Load our DB (if we haven't already; i.e: our profile is the single loaded profile since login)
            if state.profiles.len() == 1 {
                let profiles = db::get_all_profiles(&handle).await.unwrap();
                // Load our Profile Cache into the state
                state.merge_db_profiles(profiles).await;

                // Load chats and their messages from database
                let slim_chats_result = db_migration::get_all_chats(&handle).await;
                if let Ok(slim_chats) = slim_chats_result {
                    // Convert slim chats to full chats and load their messages
                    for slim_chat in slim_chats {
                        let mut chat = slim_chat.to_chat();
                        
                        // Load messages for this chat
                        let messages_result = db_migration::get_chat_messages(&handle, &chat.id()).await;
                        if let Ok(messages) = messages_result {
                            // Add all messages to the chat
                            for message in messages {
                                chat.internal_add_message(message);
                            }
                        } else {
                            eprintln!("Failed to load messages for chat {}: {:?}", chat.id(), messages_result);
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
                    // Fall back to old profile-based message loading for migration
                    let msgs = db::old_get_all_messages(&handle).await.unwrap();
                    for (msg, npub) in msgs {
                        // Create chat if it doesn't exist and add message
                        state.add_message_to_participant(&npub, msg);
                    }
                }
            }

            // Run database migrations
            if let Err(e) = db::run_migrations(handle.clone()).await {
                eprintln!("Failed to run database migrations: {}", e);
                // Emit error event if needed
                handle.emit("progress_operation", serde_json::json!({
                    "type": "error",
                    "message": format!("Database migration error: {}", e)
                })).unwrap();
            }

            // If we've just migrated to v2 and no chats are in memory yet,
            // reload chats/messages from DB before any init_finished emit
            if db::get_db_version(handle.clone()).unwrap_or(None).unwrap_or(0) >= 2 && state.chats.is_empty() {
                let slim_chats_result = db_migration::get_all_chats(&handle).await;
                if let Ok(slim_chats) = slim_chats_result {
                    for slim_chat in slim_chats {
                        let mut chat = slim_chat.to_chat();

                        // Load messages for this chat
                        let messages_result = db_migration::get_chat_messages(&handle, &chat.id()).await;
                        if let Ok(messages) = messages_result {
                            for message in messages {
                                chat.internal_add_message(message);
                            }
                        } else {
                            eprintln!("Failed to load messages for chat {}: {:?}", chat.id(), messages_result);
                        }

                        // Ensure profiles exist for chat participants
                        for participant in chat.participants() {
                            if state.get_profile(participant).is_none() {
                                let mut profile = Profile::new();
                                profile.id = participant.clone();
                                profile.mine = false;
                                state.profiles.push(profile);
                            }
                        }

                        // Add chat
                        state.chats.push(chat);
                    }
                    // Keep chats sorted by last message time
                    state.chats.sort_by(|a, b| b.last_message_time().cmp(&a.last_message_time()));
                } else {
                    eprintln!("Failed to load chats after migration: {:?}", slim_chats_result);
                }
            }

            // Check if we need to migrate timestamps from seconds to milliseconds
            // Run timestamp migration without dropping lock
            match migrate_unix_to_millisecond_timestamps(&handle).await {
                Ok(count) => {
                    if count > 0 {
                        println!("Migrated {} message timestamps", count);
                    }
                },
                Err(e) => {
                    eprintln!("Failed to migrate timestamps: {}", e);
                }
            }

            // Check if we have pending migrations to apply to the database
            let has_pending_migrations = PENDING_MIGRATION.get().map(|m| !m.is_empty()).unwrap_or(false);
            
            if has_pending_migrations {
                let migration_map = PENDING_MIGRATION.get().unwrap();
                
                // Emit migration start event to frontend
                handle.emit("progress_operation", serde_json::json!({
                    "type": "start",
                    "message": "Migrating DB"
                })).unwrap();
                
                // Drop state during expensive migration operations
                drop(state);
                
                // Apply the database migrations synchronously
                match update_database_attachment_paths(&handle, migration_map).await {
                    Ok(_) => {
                    },
                    Err(e) => {
                        eprintln!("Failed to update database attachment paths: {}", e);
                        // Emit error event
                        handle.emit("progress_operation", serde_json::json!({
                            "type": "error",
                            "message": format!("Migration error: {}", e.to_string())
                        })).unwrap();
                    }
                }
                
                // Re-acquire state and reload messages to ensure correct paths
                state = STATE.lock().await;
                
                // Reload messages from database with updated paths
                let slim_chats_result = db_migration::get_all_chats(&handle).await;
                if let Ok(slim_chats) = slim_chats_result {
                    // Convert slim chats to full chats and load their messages
                    for slim_chat in slim_chats {
                        let mut chat = slim_chat.to_chat();
                        
                        // Load messages for this chat
                        let messages_result = db_migration::get_chat_messages(&handle, &chat.id()).await;
                        if let Ok(messages) = messages_result {
                            // Add all messages to the chat
                            for message in messages {
                                chat.internal_add_message(message);
                            }
                        } else {
                            eprintln!("Failed to load messages for chat {}: {:?}", chat.id(), messages_result);
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
                    }

                    // Sort the chats by their last received message
                    state.chats.sort_by(|a, b| b.last_message_time().cmp(&a.last_message_time()));
                } else {
                    eprintln!("Failed to load chats from database: {:?}", slim_chats_result);
                }
                
                // Send the state to our frontend to signal finalised init with a full state
                handle.emit("init_finished", serde_json::json!({
                    "profiles": &state.profiles,
                    "chats": &state.chats
                })).unwrap();
            } else {
                // Check if filesystem integrity check is needed
                let needs_integrity_check = state.chats.iter().any(|chat| 
                    chat.messages.iter().any(|msg| 
                        msg.attachments.iter().any(|att| att.downloaded)
                    )
                );
                
                if needs_integrity_check {
                    // Check integrity without dropping state
                    check_attachment_filesystem_integrity(&handle, &mut state).await;
                    
                    // Send the state to our frontend to signal finalised init with a full state
                    handle.emit("init_finished", serde_json::json!({
                        "profiles": &state.profiles,
                        "chats": &state.chats
                    })).unwrap();
                } else {
                    // No integrity check needed, send init immediately
                    handle.emit("init_finished", serde_json::json!({
                        "profiles": &state.profiles,
                        "chats": &state.chats
                    })).unwrap();
                }
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

    let events = if let Some(url) = &relay_url {
        // Fetch from specific relay
        client
            .fetch_events_from(vec![url], filter, std::time::Duration::from_secs(30))
            .await
            .unwrap()
    } else {
        // Fetch from all relays
        client
            .fetch_events(filter, std::time::Duration::from_secs(60))
            .await
            .unwrap()
    };

    // Process events without holding any locks
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

        if relay_url.is_none() {
            handle.emit("sync_finished", ()).unwrap();
        }
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
        for (chat_idx, messages) in chats_with_updates {
            // Since we're iterating over existing indices, we know the chat exists
            let chat = &state.chats[chat_idx];
            let chat_id = chat.id().clone();
            
            // Save all messages for this chat
            let all_messages = messages;
            if let Err(e) = save_chat_messages(handle.clone(), &chat_id, &all_messages).await {
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

/// Updates database attachment paths after login when encryption key is available
/// Returns the number of attachments that were updated
async fn update_database_attachment_paths<R: Runtime>(
    handle: &AppHandle<R>,
    migration_map: &std::collections::HashMap<String, (String, String)>
) -> Result<u32, Box<dyn std::error::Error>> {
    // Get all chats from database
    let slim_chats = db_migration::get_all_chats(handle).await?;
    let mut updated_count = 0;
    
    // Process each chat and its messages
    for slim_chat in slim_chats {
        let chat_id = slim_chat.id.clone();
        let mut messages = db_migration::get_chat_messages(handle, &chat_id).await.unwrap_or_default();
        let mut chat_updated = false;
        
        // Update attachment paths in messages
        for message in &mut messages {
            let mut updated = false;
            
            // Check each attachment
            for attachment in &mut message.attachments {
                // Check if this attachment needs migration based on nonce
                if let Some((old_path, new_path)) = migration_map.get(&attachment.nonce) {
                    // Update the path if it matches the old path or contains the nonce
                    if attachment.path == *old_path || attachment.path.contains(&attachment.nonce) {
                        attachment.path = new_path.clone();
                        attachment.downloaded = true;
                        updated = true;
                        updated_count += 1;
                    }
                }
            }
            
            if updated {
                chat_updated = true;
            }
        }
        
        // Save the messages back if any were updated
        if chat_updated {
            let all_messages = messages;
            if let Err(e) = save_chat_messages(handle.clone(), &chat_id, &all_messages).await {
                eprintln!("Failed to save updated messages in attachment migration: {}", e);
            }
        }
    }
    
    Ok(updated_count)
}

/// Migrates Unix timestamps (in seconds) to millisecond timestamps
/// Returns the number of messages that were updated
async fn migrate_unix_to_millisecond_timestamps<R: Runtime>(
    handle: &AppHandle<R>
) -> Result<u32, Box<dyn std::error::Error>> {
    // Get all chats from database
    let slim_chats = db_migration::get_all_chats(handle).await?;
    
    // Define threshold - timestamps below this are likely in seconds
    // Using year 2000 (946684800000 ms) as a reasonable cutoff
    const MILLISECOND_THRESHOLD: u64 = 946684800000;
    
    let mut updated_count = 0;
    
    // Process each chat and its messages
    for slim_chat in slim_chats {
        let chat_id = slim_chat.id.clone();
        let mut messages = db_migration::get_chat_messages(handle, &chat_id).await.unwrap_or_default();
        let mut chat_updated = false;
        
        // Check each message for timestamp migration
        for message in &mut messages {
            // Check if timestamp appears to be in seconds (too small to be milliseconds)
            if message.at < MILLISECOND_THRESHOLD {
                // Convert seconds to milliseconds
                let old_timestamp = message.at;
                message.at = old_timestamp * 1000;
                updated_count += 1;
                chat_updated = true;
            }
        }
        
        // Save the messages back if any were updated
        if chat_updated {
            let all_messages = messages;
            if let Err(e) = save_chat_messages(handle.clone(), &chat_id, &all_messages).await {
                eprintln!("Failed to save updated messages in timestamp migration: {}", e);
            }
        }
    }
    
    Ok(updated_count as u32)
}

// TODO: REMOVE AFTER SEVERAL UPDATES - This migration code is only needed for users upgrading from nonce-based to hash-based storage
/// Migrates nonce-based attachment files to hash-based files at startup
/// Returns a map of nonce->new_path for database updates after login
async fn migrate_nonce_files_to_hash<R: Runtime>(handle: &AppHandle<R>) -> Result<std::collections::HashMap<String, (String, String)>, Box<dyn std::error::Error>> {
    // Choose the appropriate base directory based on platform
    let base_directory = if cfg!(target_os = "ios") {
        tauri::path::BaseDirectory::Document
    } else {
        tauri::path::BaseDirectory::Download
    };

    // Resolve the directory path using the determined base directory
    let dir = handle.path().resolve("vector", base_directory)?;
    
    // Check if the directory exists
    // TODO: note that, if we allow Vector to utilise non-default paths per-attachment (i.e: do not copy files to the Vector dir during upload, but use the original path), then this assumption must be changed.
    if !dir.exists() {
        return Ok(std::collections::HashMap::new());
    }

    // Track migrated files for database updates
    let mut migration_map: std::collections::HashMap<String, (String, String)> = std::collections::HashMap::new();
    
    // Read all files in the directory
    let mut total_files = 0;
    let mut migrated_files = 0;
    
    // First pass: count nonce-based files
    for entry in std::fs::read_dir(&dir)? {
        let entry = entry?;
        if let Some(filename) = entry.path().file_name() {
            if is_nonce_filename(&filename.to_string_lossy()) {
                total_files += 1;
            }
        }
    }

    // If there are files to migrate, show progress
    if total_files > 0 {
        // Emit initial migration status
        handle.emit("migration_start", serde_json::json!({
            "total": total_files,
            "current": 0,
            "status": "Starting file migration..."
        })).unwrap();
    }
    
    // Second pass: migrate files
    for entry in std::fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        
        // Skip if it's not a file
        if !path.is_file() {
            continue;
        }
        
        // Get the filename
        if let Some(filename) = path.file_name() {
            let filename_str = filename.to_string_lossy();
            
            // Check if this is a nonce-based filename
            if is_nonce_filename(&filename_str) {
                // Extract nonce from filename (format: {nonce}.{extension})
                if let Some(nonce) = filename_str.split('.').next() {
                    // Migrate the file
                    match migrate_nonce_file_to_hash(&path) {
                        Ok(new_filename) => {
                            migrated_files += 1;
                            
                            // Update progress
                            handle.emit("migration_progress", serde_json::json!({
                                "total": total_files,
                                "current": migrated_files,
                                "status": format!("Migrated file {} of {}", migrated_files, total_files)
                            })).unwrap();
                            
                            // Store the old and new paths for database update
                            let old_path = path.to_string_lossy().to_string();
                            let new_path = dir.join(&new_filename).to_string_lossy().to_string();
                            migration_map.insert(nonce.to_string(), (old_path, new_path));
                        }
                        Err(e) => {
                            eprintln!("Failed to migrate {}: {}", filename_str, e);
                        }
                    }
                }
            }
        }
    }
    
    // Emit completion for file migration
    if total_files > 0 {
        handle.emit("migration_complete", serde_json::json!({
            "total": total_files,
            "migrated": migrated_files,
            "status": "File migration complete!"
        })).unwrap();
    }
    
    Ok(migration_map)
}



/// Pre-fetch the configs from our preferred NIP-96 servers to speed up uploads
#[tauri::command]
async fn warmup_nip96_servers() -> bool {
    use nostr_sdk::nips::nip96::{get_server_config_url, ServerConfig};
    use reqwest::Client;
    
    // Create HTTP client with timeout
    let client = match Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build() {
        Ok(client) => client,
        Err(_) => {
            return false;
        }
    };

    // Public Fileserver
    if PUBLIC_NIP96_CONFIG.get().is_none() {
        let config_url = match get_server_config_url(&Url::parse(TRUSTED_PUBLIC_NIP96).unwrap()) {
            Ok(url) => url,
            Err(_) => {
                return false;
            }
        };
        
        // Fetch the JSON configuration from the URL
        let config_json = match client.get(config_url.to_string()).send().await {
            Ok(response) => {
                match response.text().await {
                    Ok(json) => json,
                    Err(_) => {
                        return false;
                    }
                }
            },
            Err(_) => {
                return false;
            }
        };
        
        let _ = match ServerConfig::from_json(&config_json) {
            Ok(conf) => {
                PUBLIC_NIP96_CONFIG.set(conf).unwrap();
                true
            },
            Err(_) => {
                false
            }
        };
    }

    // Private Fileserver
    if PRIVATE_NIP96_CONFIG.get().is_none() {
        let config_url = match get_server_config_url(&Url::parse(TRUSTED_PRIVATE_NIP96).unwrap()) {
            Ok(url) => url,
            Err(_) => {
                return false;
            }
        };
        
        // Fetch the JSON configuration from the URL
        let config_json = match client.get(config_url.to_string()).send().await {
            Ok(response) => {
                match response.text().await {
                    Ok(json) => json,
                    Err(_) => {
                        return false;
                    }
                }
            },
            Err(_) => {
                return false;
            }
        };
        
        let _ = match ServerConfig::from_json(&config_json) {
            Ok(conf) => {
                PRIVATE_NIP96_CONFIG.set(conf).unwrap();
                true
            },
            Err(_) => {
                false
            }
        };
    }

    // We've got the configs for all our servers, nice!
    true
}



#[tauri::command]
async fn start_typing(receiver: String) -> bool {
    let client = NOSTR_CLIENT.get().expect("Nostr client not initialized");
    let signer = client.signer().await.unwrap();
    let my_public_key = signer.get_public_key().await.unwrap();
    let my_pubkey_short: String = my_public_key.to_hex().chars().take(8).collect();

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
                    [TRUSTED_RELAY],
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
            let group_short: String = group_id.chars().take(8).collect();
            println!("[TYPING] 📤 Sending MLS group typing indicator: me={} → group={}", my_pubkey_short, group_short);
            
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
            match mls::send_mls_message(&group_id, rumor).await {
                Ok(_) => {
                    println!("[TYPING] ✅ MLS group typing indicator sent successfully");
                    true
                }
                Err(e) => {
                    eprintln!("[TYPING] ❌ Failed to send MLS group typing indicator: {}", e);
                    false
                }
            }
        }
    }
}

#[tauri::command]
async fn get_chat_messages(chat_id: String, limit: Option<usize>) -> Result<Vec<Message>, String> {
    let state = STATE.lock().await;
    
    match state.get_chat(&chat_id) {
        Some(chat) => {
            let messages = if let Some(lim) = limit {
                // Return last N messages
                let start = chat.messages.len().saturating_sub(lim);
                chat.messages[start..].to_vec()
            } else {
                // Return all messages
                chat.messages.clone()
            };
            Ok(messages)
        }
        None => Ok(Vec::new()), // Chat doesn't exist yet, return empty
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
                                    Ok(_) => {
                                        println!("[MLS][live][welcome] processed wrapper_id={}", wrapper_id);
                                        return true;
                                    }
                                    Err(e) => {
                                        eprintln!("[MLS][live][welcome] process_welcome failed wrapper_id={} err={}", wrapper_id, e);
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
                        // Notify UI so invites list can refresh via list_pending_mls_welcomes()
                        if let Some(app) = TAURI_APP.get() {
                            let _ = app.emit("mls_invite_received", serde_json::json!({
                                "wrapper_event_id": wrapper_id.to_hex()
                            }));
                        }
                        return true;
                    } else {
                        return false;
                    }
                } else {
                    eprintln!("[MLS][live][welcome] failed to convert rumor to UnsignedEvent");
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
                        RumorProcessingResult::TextMessage(msg) => {
                            handle_text_message(msg, &contact, is_mine, is_new).await
                        }
                        RumorProcessingResult::FileAttachment(msg) => {
                            handle_file_attachment(msg, &contact, is_mine, is_new).await
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
                        RumorProcessingResult::Ignored => false,
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
async fn handle_text_message(msg: Message, contact: &str, is_mine: bool, is_new: bool) -> bool {
    // Send an OS notification for incoming messages (do this before locking state)
    if !is_mine && is_new {
        // Clone necessary data for notification (avoid holding lock during notification)
        let display_info = {
            let state = STATE.lock().await;
            match state.get_profile(contact) {
                Some(profile) => {
                    if profile.muted {
                        None // Profile is muted, don't send notification
                    } else {
                        // Profile is not muted, send notification
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
                // No profile, send notification with default name
                None => Some((String::from("New Message"), msg.content.clone())),
            }
        };
            
        // Send notification outside of state lock
        if let Some((display_name, content)) = display_info {
            show_notification(display_name, content);
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

        // Save the chat/messages to DB (chat_id = contact npub for DMs)
        if let Some(handle) = TAURI_APP.get() {
            // Get all messages for this chat and save them
            let all_messages = {
                let state = STATE.lock().await;
                state.get_chat(contact).map(|chat| chat.messages.clone()).unwrap_or_default()
            };
            let _ = save_chat_messages(handle.clone(), contact, &all_messages).await;
        }
        // Ensure OS badge is updated immediately after accepting the message
        if let Some(handle) = TAURI_APP.get() {
            let _ = update_unread_counter(handle.clone()).await;
        }
    }

    was_msg_added_to_state
}

/// Handle a processed file attachment
async fn handle_file_attachment(msg: Message, contact: &str, is_mine: bool, is_new: bool) -> bool {
    // Get file extension for notification
    let extension = msg.attachments.first()
        .map(|att| att.extension.clone())
        .unwrap_or_else(|| String::from("file"));

    // Send an OS notification for incoming files (do this before locking state)
    if !is_mine && is_new {
        // Clone necessary data for notification (avoid holding lock during notification)
        let display_info = {
            let state = STATE.lock().await;
            match state.get_profile(contact) {
                Some(profile) => {
                    if profile.muted {
                        None // Profile is muted, don't send notification
                    } else {
                        // Profile is not muted, send notification
                        let display_name = if !profile.nickname.is_empty() {
                            profile.nickname.clone()
                        } else if !profile.name.is_empty() {
                            profile.name.clone()
                        } else {
                            String::from("New Message")
                        };
                        // Create a "description" of the attachment file
                        Some((display_name, extension.clone()))
                    }
                }
                // No profile, send notification with default name
                None => Some((String::from("New Message"), extension.clone())),
            }
        };
        
        // Send notification outside of state lock
        if let Some((display_name, file_extension)) = display_info {
            show_notification(display_name, "Sent a ".to_string() + &get_file_type_description(&file_extension));
        }
    }

    // Add the message to the state and clear typing indicator for sender
    let (was_msg_added_to_state, active_typers) = {
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
    
    // Emit typing update to clear the indicator on frontend
    if let Some(handle) = TAURI_APP.get() {
        let _ = handle.emit("typing-update", serde_json::json!({
            "conversation_id": contact,
            "typers": active_typers
        }));
    }

    // If accepted in-state: commit to the DB and emit to the frontend
    if was_msg_added_to_state {
        // Send it to the frontend
        if let Some(handle) = TAURI_APP.get() {
            handle.emit("message_new", serde_json::json!({
                "message": &msg,
                "chat_id": contact
            })).unwrap();
        }

        // Save the chat/messages to DB (chat_id = contact npub for DMs)
        if let Some(handle) = TAURI_APP.get() {
            // Get all messages for this chat and save them
            let all_messages = {
                let state = STATE.lock().await;
                state.get_chat(contact).map(|chat| chat.messages.clone()).unwrap_or_default()
            };
            let _ = save_chat_messages(handle.clone(), contact, &all_messages).await;
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

    // Save all messages for the chat with the new reaction to our DB (outside of state lock)
    if let Some(chat_id) = chat_id_for_save {
        if let Some(handle) = TAURI_APP.get() {
            // Get all messages for this chat
            let all_messages = {
                let state = STATE.lock().await;
                state.get_chat(&chat_id).map(|chat| chat.messages.clone()).unwrap_or_default()
            };
            let _ = save_chat_messages(handle.clone(), &chat_id, &all_messages).await;
        }
    }

    reaction_added
}

// OLD IMPLEMENTATION BELOW - TO BE REMOVED AFTER VERIFICATION
/*
#[tauri::command]
async fn handle_event_old(event: Event, is_new: bool) -> bool {
    let client = NOSTR_CLIENT.get().expect("Nostr client not initialized");

    // Grab our pubkey
    let signer = client.signer().await.unwrap();
    let my_public_key = signer.get_public_key().await.unwrap();

    // Unwrap the gift wrap
    match client.unwrap_gift_wrap(&event).await {
        Ok(UnwrappedGift { rumor, sender }) => {
            // Handle MLS Welcome messages (group invites) - these need special processing
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
                                    Ok(_) => {
                                        println!("[MLS][live][welcome] processed wrapper_id={}", wrapper_id);
                                        return true;
                                    }
                                    Err(e) => {
                                        eprintln!("[MLS][live][welcome] process_welcome failed wrapper_id={} err={}", wrapper_id, e);
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
                        // Notify UI so invites list can refresh via list_pending_mls_welcomes()
                        if let Some(app) = TAURI_APP.get() {
                            let _ = app.emit("mls_invite_received", serde_json::json!({
                                "wrapper_event_id": wrapper_id.to_hex()
                            }));
                        }
                        return true;
                    } else {
                        return false;
                    }
                } else {
                    eprintln!("[MLS][live][welcome] failed to convert rumor to UnsignedEvent");
                    return false;
                }
            }

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

                // Extract milliseconds from custom tag if present
                let ms_timestamp = match rumor.tags.find(TagKind::Custom(Cow::Borrowed("ms"))) {
                    Some(ms_tag) => {
                        // Get the ms value and append it to the timestamp
                        if let Some(ms_str) = ms_tag.content() {
                            if let Ok(ms_value) = ms_str.parse::<u64>() {
                                // Validate that ms is between 0-999
                                if ms_value <= 999 {
                                    rumor.created_at.as_u64() * 1000 + ms_value
                                } else {
                                    // Invalid ms value, ignore it
                                    rumor.created_at.as_u64() * 1000
                                }
                            } else {
                                rumor.created_at.as_u64() * 1000
                            }
                        } else {
                            rumor.created_at.as_u64() * 1000
                        }
                    }
                    None => rumor.created_at.as_u64() * 1000
                };

                // Create the Message
                let msg = Message {
                    id: rumor.id.unwrap().to_hex(),
                    content: rumor.content,
                    replied_to,
                    preview_metadata: None,
                    at: ms_timestamp,
                    attachments: Vec::new(),
                    reactions: Vec::new(),
                    mine: is_mine,
                    pending: false,
                    failed: false,
                };

            // Send an OS notification for incoming messages (do this before locking state)
            if !is_mine && is_new {
                // Clone necessary data for notification (avoid holding lock during notification)
                let display_info = {
                    let state = STATE.lock().await;
                    match state.get_profile(&contact) {
                        Some(profile) => {
                            if profile.muted {
                                None // Profile is muted, don't send notification
                            } else {
                                // Profile is not muted, send notification
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
                        // No profile, send notification with default name
                        None => Some((String::from("New Message"), msg.content.clone())),
                    }
                };
                    
                    // Send notification outside of state lock
                    if let Some((display_name, content)) = display_info {
                        show_notification(display_name, content);
                    }
                }

                // Add the message to the state and handle database save in one operation to avoid multiple locks
                let (was_msg_added_to_state, _should_emit, _should_save) = {
                    let mut state = STATE.lock().await;
                    let was_added = state.add_message_to_participant(&contact, msg.clone());
                    (was_added, was_added, was_added)
                };

                // If accepted in-state: commit to the DB and emit to the frontend
                if was_msg_added_to_state {
                    // Send it to the frontend
                    if let Some(handle) = TAURI_APP.get() {
                        handle.emit("message_new", serde_json::json!({
                            "message": &msg,
                            "chat_id": &contact
                        })).unwrap();
                    }

                    // Save the chat/messages to DB (chat_id = contact npub for DMs)
                    if let Some(handle) = TAURI_APP.get() {
                        // Get all messages for this chat and save them
                        let all_messages = {
                            let state = STATE.lock().await;
                            state.get_chat(&contact).map(|chat| chat.messages.clone()).unwrap_or_default()
                        };
                        let _ = save_chat_messages(handle.clone(), &contact, &all_messages).await;
                    }
                    // Ensure OS badge is updated immediately after accepting the message
                    if let Some(handle) = TAURI_APP.get() {
                        let _ = update_unread_counter(handle.clone()).await;
                    }
                }

                was_msg_added_to_state
            }
            // Emoji Reaction (NIP-25)
            else if rumor.kind == Kind::Reaction {
                match rumor.tags.find(TagKind::e()) {
                    Some(react_reference_tag) => {
                        // Add the reaction to the appropriate chat message
                        let reaction = Reaction {
                            id: rumor.id.unwrap().to_hex(),
                            reference_id: react_reference_tag.content().unwrap().to_string(),
                            author_id: rumor.pubkey.to_hex(),
                            emoji: rumor.content.clone(),
                        };

                        // Find the chat containing the referenced message and add the reaction
                        // Use a single lock scope to avoid nested locks
                        let (reaction_added, chat_id_for_save, _message_for_save) = {
                            let mut state = STATE.lock().await;
                            let reaction_added = if let Some((chat_id, msg_mut)) = state.find_chat_and_message_mut(&react_reference_tag.content().unwrap()) {
                                msg_mut.add_reaction(reaction, Some(chat_id))
                            } else {
                                // Message not found in any chat - this can happen during sync
                                // TODO: track these "ahead" reactions and re-apply them once sync has finished
                                false
                            };
                            
                            // If reaction was added, get the message for saving
                            let message_for_save = if reaction_added {
                                // Find the message again for saving
                                state.find_message(&react_reference_tag.content().unwrap())
                                    .map(|(chat, message)| (chat.id().clone(), message.clone()))
                            } else {
                                None
                            };
                            
                            (reaction_added, message_for_save.as_ref().map(|(chat_id, _)| chat_id.clone()), message_for_save.map(|(_, message)| message))
                        };

                        // Save all messages for the chat with the new reaction to our DB (outside of state lock)
                        if let Some(chat_id) = chat_id_for_save {
                            if let Some(handle) = TAURI_APP.get() {
                                // Get all messages for this chat
                                let all_messages = {
                                    let state = STATE.lock().await;
                                    state.get_chat(&chat_id).map(|chat| chat.messages.clone()).unwrap_or_default()
                                };
                                let _ = save_chat_messages(handle.clone(), &chat_id, &all_messages).await;
                            }
                        }

                        reaction_added
                    }
                    None => false,
                }
            }
            // Files and Images
            else if rumor.kind == Kind::from_u16(15) {
                // Extract our AES-GCM decryption key and nonce
                let decryption_key = rumor.tags.find(TagKind::Custom(Cow::Borrowed("decryption-key"))).unwrap().content().unwrap();
                let decryption_nonce = rumor.tags.find(TagKind::Custom(Cow::Borrowed("decryption-nonce"))).unwrap().content().unwrap();

                // Extract the original file hash (ox tag) if present
                let original_file_hash = rumor.tags.find(TagKind::Custom(Cow::Borrowed("ox"))).map(|tag| tag.content().unwrap_or(""));

                // Extract the content storage URL
                let content_url = rumor.content;

                // Extract image metadata if provided
                let img_meta: Option<message::ImageMetadata> = {
                    let blurhash_opt = rumor.tags.find(TagKind::Custom(Cow::Borrowed("blurhash")))
                        .and_then(|tag| tag.content())
                        .map(|s| s.to_string());
                    
                    let dimensions_opt = rumor.tags.find(TagKind::Custom(Cow::Borrowed("dim")))
                        .and_then(|tag| tag.content())
                        .and_then(|s| {
                            // Parse "width-x-height" format
                            let parts: Vec<&str> = s.split('x').collect();
                            if parts.len() == 2 {
                                let width = parts[0].parse::<u32>().ok()?;
                                let height = parts[1].parse::<u32>().ok()?;
                                Some((width, height))
                            } else {
                                None
                            }
                        });
                    
                    // Only create ImageMetadata if we have all required fields
                    match (blurhash_opt, dimensions_opt) {
                        (Some(blurhash), Some((width, height))) => {
                            Some(message::ImageMetadata {
                                blurhash,
                                width,
                                height,
                            })
                        },
                        _ => None
                    }
                };

                // Figure out the file extension from the mime-type
                let mime_type = rumor.tags.find(TagKind::Custom(Cow::Borrowed("file-type"))).unwrap().content().unwrap();
                let extension = crate::util::extension_from_mime(mime_type);

                let handle = TAURI_APP.get().unwrap();
                // Choose the appropriate base directory based on platform
                let base_directory = if cfg!(target_os = "ios") {
                    tauri::path::BaseDirectory::Document
                } else {
                    tauri::path::BaseDirectory::Download
                };

                // Resolve the directory path using the determined base directory
                let dir = handle.path().resolve("vector", base_directory).unwrap();
                
                // Grab the reported file size - it's noteworthy that this COULD be missing or wrong, so must be treated as an assumption or guide
                let reported_size = rumor.tags
                    .find(TagKind::Custom(Cow::Borrowed("size")))
                    .map_or(0, |tag| tag.content().unwrap_or("0").parse().unwrap_or(0));

                // Check for existing local files based on ox tag first
                let mut file_hash = String::new();
                let mut file_path = std::path::PathBuf::new();
                let mut size: u64 = reported_size;
                let mut downloaded: bool = false;
                let mut found_existing_file = false;
                
                // If we have an ox tag (original file hash), check if a local file exists with that hash
                if let Some(ox_hash) = original_file_hash {
                    if !ox_hash.is_empty() {
                        // Check if a local file exists with this hash across all messages
                        let state = STATE.lock().await;
                        for chat in &state.chats {
                            for message in &chat.messages {
                                for attachment in &message.attachments {
                                    if attachment.id == ox_hash && attachment.downloaded {
                                        // Found existing attachment with same original hash
                                        file_path = std::path::PathBuf::from(&attachment.path);
                                        file_hash = ox_hash.to_string();
                                        size = attachment.size;
                                        downloaded = true;
                                        found_existing_file = true;
                                        break;
                                    }
                                }
                            }
                        }
                    }
                }
                
                if !found_existing_file {
                    // Check if a hash-based file might already exist
                    // We need to download to check the hash, but only for small files during sync
                    if is_new && reported_size > 0 && reported_size <= 262144 {
                        // Try to download and check if hash file exists
                        let temp_attachment = Attachment {
                            id: decryption_nonce.to_string(),
                            key: decryption_key.to_string(),
                            nonce: decryption_nonce.to_string(),
                            extension: extension.to_string(),
                            url: content_url.clone(),
                            path: String::new(), // Temporary, will be set below
                            size: reported_size,
                            img_meta: img_meta.clone(),
                            downloading: false,
                            downloaded: false
                        };
                        
                        // Download to check hash
                        if let Ok(encrypted_data) = net::download_silent(&content_url, Some(std::time::Duration::from_secs(5))).await {
                            // Calculate hash without saving
                            if let Ok(decrypted_data) = crypto::decrypt_data(&encrypted_data, &temp_attachment.key, &temp_attachment.nonce) {
                                file_hash = calculate_file_hash(&decrypted_data);
                                let hash_file_path = dir.join(format!("{}.{}", file_hash, extension));
                                
                                if hash_file_path.exists() {
                                    // Hash file already exists!
                                    file_path = hash_file_path;
                                    downloaded = true;
                                    size = reported_size;
                                } else {
                                    // Save the new file with hash name
                                    if let Err(_e) = std::fs::write(&hash_file_path, decrypted_data) {
                                        downloaded = false;
                                        size = reported_size;
                                        // Still set path to where it WILL be downloaded
                                        file_path = hash_file_path;
                                    } else {
                                        file_path = hash_file_path;
                                        downloaded = true;
                                        size = reported_size;
                                    }
                                }
                            } else {
                                // Failed to decrypt for hash check, fall back to nonce placeholder
                                file_path = dir.join(format!("{}.{}", decryption_nonce, extension));
                                downloaded = false;
                                size = reported_size;
                            }
                        } else {
                            // Failed to download for hash check, fall back to nonce placeholder
                            file_path = dir.join(format!("{}.{}", decryption_nonce, extension));
                            downloaded = false;
                            size = reported_size;
                        }
                    } else {
                        // File too large, size unknown, or during historical sync
                        // Use nonce as placeholder - this will be updated to hash-based
                        // when the file is actually downloaded via download_attachment
                        file_path = dir.join(format!("{}.{}", decryption_nonce, extension));
                        downloaded = false;
                        size = reported_size;
                    }
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

                // Extract milliseconds from custom tag if present (same as for text messages)
                let ms_timestamp = match rumor.tags.find(TagKind::Custom(Cow::Borrowed("ms"))) {
                    Some(ms_tag) => {
                        // Get the ms value and append it to the timestamp
                        if let Some(ms_str) = ms_tag.content() {
                            if let Ok(ms_value) = ms_str.parse::<u64>() {
                                // Validate that ms is between 0-999
                                if ms_value <= 999 {
                                    rumor.created_at.as_u64() * 1000 + ms_value
                                } else {
                                    // Invalid ms value, ignore it
                                    rumor.created_at.as_u64() * 1000
                                }
                            } else {
                                rumor.created_at.as_u64() * 1000
                            }
                        } else {
                            rumor.created_at.as_u64() * 1000
                        }
                    }
                    None => rumor.created_at.as_u64() * 1000
                };

                // Create an attachment
                // Note: The path will be updated to hash-based when the file is downloaded
                let mut attachments = Vec::new();
                let attachment = Attachment {
                    id: if downloaded { file_hash } else { decryption_nonce.to_string() },
                    key: decryption_key.to_string(),
                    nonce: decryption_nonce.to_string(),
                    extension: extension.to_string(),
                    url: content_url,
                    path: file_path.to_string_lossy().to_string(), // Will be updated to hash-based path on download
                    size,
                    img_meta,
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
                    at: ms_timestamp,
                    attachments,
                    reactions: Vec::new(),
                    mine: is_mine,
                    pending: false,
                    failed: false,
                };

                // Send an OS notification for incoming files (do this before locking state)
                if !is_mine && is_new {
                    // Clone necessary data for notification (avoid holding lock during notification)
                    let display_info = {
                        let state = STATE.lock().await;
                        match state.get_profile(&contact) {
                            Some(profile) => {
                                if profile.muted {
                                    None // Profile is muted, don't send notification
                                } else {
                                    // Profile is not muted, send notification
                                    let display_name = if !profile.nickname.is_empty() {
                                        profile.nickname.clone()
                                    } else if !profile.name.is_empty() {
                                        profile.name.clone()
                                    } else {
                                        String::from("New Message")
                                    };
                                    // Create a "description" of the attachment file
                                    Some((display_name, extension.to_string()))
                                }
                            }
                            // No profile, send notification with default name
                            None => Some((String::from("New Message"), extension.to_string())),
                        }
                    };
                    
                    // Send notification outside of state lock
                    if let Some((display_name, file_extension)) = display_info {
                        show_notification(display_name, "Sent a ".to_string() + &get_file_type_description(&file_extension));
                    }
                }

                // Add the message to the state and handle database save in one operation to avoid multiple locks
                let (was_msg_added_to_state, _should_emit, _should_save) = {
                    let mut state = STATE.lock().await;
                    let was_added = state.add_message_to_participant(&contact, msg.clone());
                    (was_added, was_added, was_added)
                };

                // If accepted in-state: commit to the DB and emit to the frontend
                if was_msg_added_to_state {
                    // Send it to the frontend
                    if let Some(handle) = TAURI_APP.get() {
                        handle.emit("message_new", serde_json::json!({
                            "message": &msg,
                            "chat_id": &contact
                        })).unwrap();
                    }

                    // Save the chat/messages to DB (chat_id = contact npub for DMs)
                    if let Some(handle) = TAURI_APP.get() {
                        // Get all messages for this chat and save them
                        let all_messages = {
                            let state = STATE.lock().await;
                            state.get_chat(&contact).map(|chat| chat.messages.clone()).unwrap_or_default()
                        };
                        let _ = save_chat_messages(handle.clone(), &contact, &all_messages).await;
                    }
                    // Ensure OS badge is updated immediately after accepting the attachment
                    if let Some(handle) = TAURI_APP.get() {
                        let _ = update_unread_counter(handle.clone()).await;
                    }
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
                                        let mut state = STATE.lock().await;
                                        match state.get_profile_mut(&rumor.pubkey.to_bech32().unwrap()) {
                                            Some(profile) => {
                                                // Apply typing indicator
                                                profile.typing_until = expiry_timestamp;

                                                // Update the frontend
                                                let handle = TAURI_APP.get().unwrap();
                                                handle.emit("profile_update", &profile).unwrap();
                                                true
                                            }
                                            None => false,
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
            }

            // Live MLS Welcome inside GiftWrap (process immediately, no manual sync required)
            else if rumor.kind == Kind::MlsWelcome {
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
                                    Ok(_) => {
                                        println!("[MLS][live][welcome] processed wrapper_id={}", wrapper_id);
                                        return true;
                                    }
                                    Err(e) => {
                                        eprintln!("[MLS][live][welcome] process_welcome failed wrapper_id={} err={}", wrapper_id, e);
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
                        // Notify UI so invites list can refresh via list_pending_mls_welcomes()
                        if let Some(app) = TAURI_APP.get() {
                            let _ = app.emit("mls_invite_received", serde_json::json!({
                                "wrapper_event_id": wrapper_id.to_hex()
                            }));
                        }
                        true
                    } else {
                        false
                    }
                } else {
                    eprintln!("[MLS][live][welcome] failed to convert rumor to UnsignedEvent");
                    false
                }
            } else {
                false
            }
        }
        Err(_) => false,
    }
}*/

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
                        // Read encrypted "mls_groups" without holding store across await
                        let enc_opt: Option<String> = {
                            let store = db::get_store(&handle);
                            match store.get("mls_groups") {
                                Some(v) if v.is_string() => Some(v.as_str().unwrap().to_string()),
                                _ => None,
                            }
                        };
                        // Decrypt after store is dropped
                        let is_member: bool = if let Some(enc) = enc_opt {
                            match crypto::internal_decrypt(enc, None).await {
                                Ok(json) => {
                                    if let Ok(arr) = serde_json::from_str::<Vec<serde_json::Value>>(&json) {
                                        arr.iter().any(|g| {
                                            g.get("group_id").and_then(|s| s.as_str()) == Some(group_wire_id.as_str()) ||
                                            g.get("engine_group_id").and_then(|s| s.as_str()) == Some(group_wire_id.as_str())
                                        })
                                    } else { false }
                                }
                                Err(_) => false,
                            }
                        } else { false };

                        // Not a member - ignore this group message
                        if !is_member {
                            return Ok(false);
                        }

                        // Resolve my pubkey bech32 for 'mine' flag
                        let my_pubkey_bech32 = {
                            let client = NOSTR_CLIENT.get().unwrap();
                            if let Ok(signer) = client.signer().await {
                                if let Ok(pk) = signer.get_public_key().await {
                                    pk.to_bech32().unwrap()
                                } else {
                                    String::new()
                                }
                            } else {
                                String::new()
                            }
                        };

                        // Process with non-Send MLS engine on a blocking thread (no awaits in scope)
                        let app_handle = TAURI_APP.get().unwrap().clone();
                        let my_npub_for_block = my_pubkey_bech32.clone();
                        let group_id_for_persist = group_wire_id.clone();
                        
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
                                                            RumorProcessingResult::TextMessage(message) | RumorProcessingResult::FileAttachment(message) => {
                                                                // Clear typing indicator for this sender (they just sent a message)
                                                                let sender_npub = msg.pubkey.to_bech32().unwrap_or_default();
                                                                let (was_added, active_typers) = {
                                                                    let mut state = crate::STATE.lock().await;
                                                                    
                                                                    // Add message to chat
                                                                    let added = state.add_message_to_chat(&group_id_for_persist, message.clone());
                                                                    
                                                                    // Clear typing indicator for sender
                                                                    let typers = if let Some(chat) = state.get_chat_mut(&group_id_for_persist) {
                                                                        chat.update_typing_participant(sender_npub, 0); // 0 = clear immediately
                                                                        chat.get_active_typers()
                                                                    } else {
                                                                        Vec::new()
                                                                    };
                                                                    
                                                                    (added, typers)
                                                                };
                                                                
                                                                // Emit typing update to clear the indicator on frontend
                                                                if let Some(handle) = TAURI_APP.get() {
                                                                    let _ = handle.emit("typing-update", serde_json::json!({
                                                                        "conversation_id": group_id_for_persist,
                                                                        "typers": active_typers
                                                                    }));
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
                                                                            use crate::db_migration::{save_chat, save_chat_messages};
                                                                            let _ = save_chat(handle.clone(), &chat).await;
                                                                            let _ = save_chat_messages(handle.clone(), &group_id_for_persist, &chat.messages).await;
                                                                        }
                                                                    }
                                                                    Some(message)
                                                                } else {
                                                                    None
                                                                }
                                                            }
                                                            RumorProcessingResult::Reaction(reaction) => {
                                                                // Handle reactions in real-time
                                                                let was_added = {
                                                                    let mut state = crate::STATE.lock().await;
                                                                    if let Some((chat_id, msg)) = state.find_chat_and_message_mut(&reaction.reference_id) {
                                                                        msg.add_reaction(reaction.clone(), Some(chat_id))
                                                                    } else {
                                                                        false
                                                                    }
                                                                };
                                                                
                                                                if was_added {
                                                                    println!("[MLS][live] Reaction added: {}", reaction.reference_id);
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
                                                                
                                                                println!("[MLS][live] Typing indicator processed for group: {}", group_id_for_persist);
                                                                None // Don't emit as message
                                                            }
                                                            RumorProcessingResult::Ignored => None,
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
                                        // Other message types (Proposal, Commit, etc.) are not persisted as chat messages here
                                        _ => None,
                                    }
                                }
                                Err(e) => {
                                    if !e.to_string().contains("group not found") {
                                        eprintln!("[MLS] live process_message failed (id={}): {}", ev.id, e);
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
                                "group_id": group_wire_id,
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

#[derive(serde::Serialize)]
struct RelayInfo {
    url: String,
    status: String,
}

/// Get all relays with their current status
#[tauri::command]
async fn get_relays() -> Result<Vec<RelayInfo>, String> {
    let client = NOSTR_CLIENT.get().expect("Nostr client not initialized");
    
    // Get all relays
    let relays = client.relays().await;
    
    // Convert to our RelayInfo format
    let relay_infos: Vec<RelayInfo> = relays
        .into_iter()
        .map(|(url, relay)| {
            let status = relay.status();
            RelayInfo {
                url: url.to_string(),
                status: match status {
                    RelayStatus::Initialized => "initialized",
                    RelayStatus::Pending => "pending",
                    RelayStatus::Connecting => "connecting",
                    RelayStatus::Connected => "connected",
                    RelayStatus::Disconnected => "disconnected",
                    RelayStatus::Terminated => "terminated",
                    RelayStatus::Banned => "banned",
                    RelayStatus::Sleeping => "sleeping",
                }.to_string(),
            }
        })
        .collect();
    
    Ok(relay_infos)
}

/// Monitor relay pool connection status changes
#[tauri::command]
async fn monitor_relay_connections() -> Result<bool, String> {
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
                    // Emit relay status update to frontend
                    handle_clone.emit("relay_status_change", serde_json::json!({
                        "url": relay_url.to_string(),
                        "status": match status {
                            RelayStatus::Initialized => "initialized",
                            RelayStatus::Pending => "pending",
                            RelayStatus::Connecting => "connecting",
                            RelayStatus::Connected => "connected",
                            RelayStatus::Disconnected => "disconnected",
                            RelayStatus::Terminated => "terminated",
                            RelayStatus::Banned => "banned",
                            RelayStatus::Sleeping => "sleeping",
                        }
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
                            let url_string = relay_url.to_string();
                            tokio::spawn(async move {
                                // Call fetch_messages with the specific relay URL
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
                    
                    match result {
                        Ok(Ok(events)) => {
                            // Check if we actually got events or just an empty response
                            if events.is_empty() && elapsed.as_secs() >= 2 {
                                // Empty response after 2+ seconds means relay is not responding properly
                                unhealthy_relays.push((url.clone(), relay.clone()));
                            }
                            // else: Healthy - got response quickly
                        }
                        Ok(Err(_)) => {
                            // Query failed
                            unhealthy_relays.push((url.clone(), relay.clone()));
                        }
                        Err(_) => {
                            // Timeout
                            unhealthy_relays.push((url.clone(), relay.clone()));
                        }
                    }
                } else if status == RelayStatus::Terminated || status == RelayStatus::Disconnected {
                    // Already disconnected, add to reconnect list
                    unhealthy_relays.push((url.clone(), relay.clone()));
                }
            }
            
            // Force reconnect unhealthy relays
            for (url, relay) in unhealthy_relays {
                // First disconnect if needed
                if relay.status() == RelayStatus::Connected {
                    let _ = relay.disconnect();
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                }
                
                // Try to reconnect
                let _ = relay.try_connect(std::time::Duration::from_secs(10)).await;
                
                // Emit status update
                handle_health.emit("relay_health_check", serde_json::json!({
                    "url": url.to_string(),
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
        #[cfg(target_os = "android")]
        {
            handle
                .notification()
                .builder()
                .title(title)
                .body(&content)
                .large_body(&content)
                // Android-specific notification extensions
                .icon("ic_notification")
                .summary("Private Message")
                .large_icon("ic_large_icon")
                .show()
                .unwrap_or_else(|e| eprintln!("Failed to send notification: {}", e));
        }
        
        #[cfg(not(target_os = "android"))]
        {
            handle
                .notification()
                .builder()
                .title(title)
                .body(&content)
                .large_body(&content)
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

#[tauri::command]
async fn download_attachment(npub: String, msg_id: String, attachment_id: String) -> bool {
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

    // Decrypt and save the file
    let result = decrypt_and_save_attachment(handle, &encrypted_data, &attachment).await;
    
    // Process the result
    match result {
        Err(error) => {
            // Handle decryption/saving error
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
                    // Grab all messages for this chat and save them.
                    let all_messages = {
                        state.get_chat(&npub).map(|chat| chat.messages.clone()).unwrap_or_default()
                    };
                    // Drop the STATE lock before performing async I/O
                    drop(state);
                    let _ = save_chat_messages(handle.clone(), &npub, &all_messages).await;
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
    client.pool().add_relay(TRUSTED_RELAY, RelayOptions::new().reconnect(false)).await.unwrap();

    // Add a couple common Nostr relays
    client.pool().add_relay("wss://auth.nostr1.com", RelayOptions::new().reconnect(false)).await.unwrap();
    client.pool().add_relay("wss://relay.damus.io", RelayOptions::new().reconnect(false)).await.unwrap();

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
                        // Send only to trusted relay
                        match client.send_event_to([TRUSTED_RELAY], &event).await {
                            Ok(_) => println!("Successfully broadcast invite acceptance to trusted relay"),
                            Err(e) => eprintln!("Failed to broadcast invite acceptance: {}", e),
                        }
                    }
                    Err(e) => eprintln!("Failed to sign invite acceptance event: {}", e),
                }
            });
        }
    }

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
            println!("[MLS] Ensuring persistent device KeyPackage...");
            match bootstrap_mls_device_keypackage().await {
                Ok(info) => {
                    let device_id = info.get("device_id").and_then(|v| v.as_str()).unwrap_or("");
                    let cached = info.get("cached").and_then(|v| v.as_bool()).unwrap_or(false);
                    println!("[MLS] Device KeyPackage ready: device_id={}, cached={}", device_id, cached);
                }
                Err(e) => eprintln!("[MLS] Device KeyPackage bootstrap failed: {}", e),
            }
        });

        // Non-blocking post-decrypt MLS sync for joined groups (run in blocking thread to avoid Send constraints)
        tokio::task::spawn_blocking(move || {
            // Allow keypackage publish/smoke test to start first
            std::thread::sleep(std::time::Duration::from_millis(800));
            let rt = tokio::runtime::Handle::current();
            rt.block_on(async {
                println!("[MLS] Spawning post-decrypt MLS group sync...");
                match sync_mls_groups_now(None).await {
                    Ok((processed, new_msgs)) => {
                        println!("[MLS] Post-decrypt MLS sync finished: processed={}, new={}", processed, new_msgs);
                    }
                    Err(e) => {
                        // Best-effort; do not affect login flow
                        eprintln!("[MLS] Post-decrypt MLS sync failed: {}", e);
                    }
                }
            });
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
    os: String,
    // Add more features here as needed
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

    PlatformFeatures {
        transcription: cfg!(all(not(target_os = "android"), feature = "whisper")),
        os: os.to_string(),
    }
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
    
    // Read the wav file and resample
    match whisper::resample_audio(path, 16000) {
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
    if let Ok(Some(existing_code)) = db::get_invite_code(handle.clone()) {
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
    
    let events = client
        .fetch_events(filter, std::time::Duration::from_secs(10))
        .await
        .map_err(|e| e.to_string())?;
    
    // Look for existing invite events
    for event in events {
        if event.content == "vector_invite" {
            // Extract the r tag (invite code)
            if let Some(r_tag) = event.tags.find(TagKind::Custom(Cow::Borrowed("r"))) {
                if let Some(code) = r_tag.content() {
                    // Store it locally
                    db::set_invite_code(handle.clone(), code.to_string())
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
    
    // Send only to trusted relay
    client.send_event_to([TRUSTED_RELAY], &event).await.map_err(|e| e.to_string())?;
    
    // Store locally
    db::set_invite_code(handle.clone(), new_code.clone())
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
    
    let events = client
        .fetch_events(filter, std::time::Duration::from_secs(10))
        .await
        .map_err(|e| e.to_string())?;
    
    // Find the invite event
    let invite_event = events
        .into_iter()
        .find(|e| e.content == "vector_invite")
        .ok_or("Invite code not found")?;
    
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

/// Get the count of unique users who accepted invites from a given npub
#[tauri::command]
async fn get_invited_users(npub: String) -> Result<u32, String> {
    let client = NOSTR_CLIENT.get().ok_or("Nostr client not initialized")?;
    
    // Convert npub to PublicKey
    let inviter_pubkey = PublicKey::from_bech32(&npub).map_err(|e| e.to_string())?;
    
    // First, get the inviter's invite code from the trusted relay
    let filter = Filter::new()
        .author(inviter_pubkey)
        .kind(Kind::ApplicationSpecificData)
        .custom_tag(SingleLetterTag::lowercase(Alphabet::D), "vector")
        .limit(100);
    
    let events = client
        .fetch_events_from(vec![TRUSTED_RELAY], filter, std::time::Duration::from_secs(10))
        .await
        .map_err(|e| e.to_string())?;
    
    // Find the invite event and extract the invite code
    let invite_code = events
        .iter()
        .find(|e| e.content == "vector_invite")
        .and_then(|e| e.tags.find(TagKind::Custom(Cow::Borrowed("r"))))
        .and_then(|tag| tag.content())
        .ok_or("No invite code found for this user")?;
    
    // Now fetch all acceptance events for this invite code from the trusted relay
    let acceptance_filter = Filter::new()
        .kind(Kind::ApplicationSpecificData)
        .custom_tag(SingleLetterTag::lowercase(Alphabet::D), invite_code)
        .limit(1000); // Allow fetching many acceptances
    
    let acceptance_events = client
        .fetch_events_from(vec![TRUSTED_RELAY], acceptance_filter, std::time::Duration::from_secs(10))
        .await
        .map_err(|e| e.to_string())?;
    
    // Filter for acceptance events that reference our inviter and collect unique acceptors
    let mut unique_acceptors = std::collections::HashSet::new();
    
    for event in acceptance_events {
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
// MLS Tauri Commands


/// Bootstrap this device's MLS KeyPackage: ensure device_id, publish if missing, and cache reference
#[tauri::command]
async fn bootstrap_mls_device_keypackage() -> Result<serde_json::Value, String> {
    // Access handle and client
    let handle = TAURI_APP.get().ok_or("App handle not initialized")?.clone();
    let client = NOSTR_CLIENT.get().ok_or("Nostr client not initialized")?;

    // Ensure a persistent device_id exists (store access scoped before awaits)
    let device_id: String = {
        let store = db::get_store(&handle);
        match store.get("mls_device_id") {
            Some(v) if v.is_string() => v.as_str().unwrap().to_string(),
            _ => {
                let id: String = thread_rng()
                    .sample_iter(&Alphanumeric)
                    .take(12)
                    .map(char::from)
                    .collect::<String>()
                    .to_lowercase();
                store.set("mls_device_id".to_string(), serde_json::json!(id.clone()));
                id
            }
        }
    };

    // Resolve my pubkey (awaits before any MLS engine is created)
    let signer = client.signer().await.map_err(|e| e.to_string())?;
    let my_pubkey = signer.get_public_key().await.map_err(|e| e.to_string())?;
    let owner_pubkey_b32 = my_pubkey.to_bech32().map_err(|e| e.to_string())?;

    // Load existing keypackage index and verify it exists on relay before returning cached
    let cached_kp_ref: Option<String> = {
        let store = db::get_store(&handle);
        let index: Vec<serde_json::Value> = match store.get("mls_keypackage_index") {
            Some(v) => serde_json::from_value(v.clone()).unwrap_or_default(),
            None => Vec::new(),
        };

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
            
            match client.fetch_events_from(
                vec![TRUSTED_RELAY],
                filter,
                std::time::Duration::from_secs(5)
            ).await {
                Ok(events) => {
                    // Check if we got any events - if so, the cached KeyPackage exists on relay
                    if events.iter().next().is_some() {
                        println!("[MLS][KeyPackage] Verified on relay, using cached");
                        return Ok(serde_json::json!({
                            "device_id": device_id,
                            "owner_pubkey": owner_pubkey_b32,
                            "keypackage_ref": ref_id,
                            "cached": true
                        }));
                    } else {
                        println!("[MLS][KeyPackage] Not found on relay, creating new one");
                        // Fall through to create new KeyPackage
                    }
                }
                _ => {
                    println!("[MLS][KeyPackage] Not found on relay, creating new one");
                    // Fall through to create new KeyPackage
                }
            }
        }
    }

    // Create device KeyPackage using persistent MLS engine inside a no-await scope
    let (kp_encoded, kp_tags) = {
        let mls_service = MlsService::new_persistent(&handle).map_err(|e| e.to_string())?;
        let engine = mls_service.engine().map_err(|e| e.to_string())?;
        let relay_url = nostr_sdk::RelayUrl::parse(TRUSTED_RELAY).map_err(|e| e.to_string())?;
        engine
            .create_key_package_for_event(&my_pubkey, [relay_url])
            .map_err(|e| e.to_string())?
    }; // engine and mls_service dropped here before any await

    // Build and sign event with nostr client
    let kp_event = client
        .sign_event_builder(EventBuilder::new(Kind::MlsKeyPackage, kp_encoded).tags(kp_tags))
        .await
        .map_err(|e| e.to_string())?;

    // Publish to TRUSTED_RELAY
    client
        .send_event_to([TRUSTED_RELAY], &kp_event)
        .await
        .map_err(|e| e.to_string())?;

    // Upsert into mls_keypackage_index (re-acquire store after awaits)
    {
        let store = db::get_store(&handle);
        let mut index: Vec<serde_json::Value> = match store.get("mls_keypackage_index") {
            Some(v) => serde_json::from_value(v.clone()).unwrap_or_default(),
            None => Vec::new(),
        };
        let now = Timestamp::now().as_u64();
        index.push(serde_json::json!({
            "owner_pubkey": owner_pubkey_b32,
            "device_id": device_id,
            "keypackage_ref": kp_event.id.to_hex(),
            "fetched_at": now,
            "expires_at": 0u64
        }));
        store.set("mls_keypackage_index".to_string(), serde_json::json!(index));
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
    - Frontend should await loadMLSGroups() to persist/refresh local list, then openChat(group_id) to navigate immediately.
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
    create_mls_group(name.to_string(), None, initial_member_devices).await
}
/// Send a message to an MLS group
#[tauri::command]
async fn send_mls_group_message(
    group_id: String,
    text: String,
    replied_to: Option<String>,
) -> Result<String, String> {
    // Run non-Send MLS engine work on blocking thread; drive async via current runtime
    tokio::task::spawn_blocking(move || {
        let handle = TAURI_APP.get().ok_or("App handle not initialized")?.clone();
        let rt = tokio::runtime::Handle::current();
        rt.block_on(async move {
            let mls = MlsService::new_persistent(&handle).map_err(|e| e.to_string())?;
            mls.send_group_message(&group_id, &text, replied_to)
                .await
                .map_err(|e| e.to_string())
        })
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))?
}
#[tauri::command]
async fn mls_create_group_simple(name: String) -> Result<String, String> {
    // Minimal: avoid holding non-Send types across await points
    let handle = TAURI_APP.get().ok_or("App handle not initialized")?.clone();

    // Resolve creator pubkey
    let client = NOSTR_CLIENT.get().ok_or("Nostr client not initialized")?;
    let signer = client.signer().await.map_err(|e| e.to_string())?;
    let my_pubkey = signer.get_public_key().await.map_err(|e| e.to_string())?;
    let creator_pubkey_b32 = my_pubkey.to_bech32().map_err(|e| e.to_string())?;

    // Generate a 128-bit hex group_id using time + rng (limit RNG scope to avoid !Send across await)
    let group_id = {
        let mut rng = thread_rng();
        format!("{:016x}{:016x}", Timestamp::now().as_u64(), rng.gen::<u64>())
    };

    // Prepare metadata timestamps
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_secs();

    // Read encrypted groups from store WITHOUT awaiting while holding the store
    let enc_opt: Option<String> = {
        let store = db::get_store(&handle);
        match store.get("mls_groups") {
            Some(v) if v.is_string() => Some(v.as_str().unwrap().to_string()),
            _ => None,
        }
    };

    // Decrypt (await) after store is dropped
    let mut groups: Vec<serde_json::Value> = if let Some(enc) = enc_opt {
        match crypto::internal_decrypt(enc, None).await {
            Ok(json) => serde_json::from_str::<Vec<serde_json::Value>>(&json).unwrap_or_default(),
            Err(_) => Vec::new(),
        }
    } else {
        Vec::new()
    };

    // Append new metadata entry
    groups.push(serde_json::json!({
        "group_id": group_id,
        "creator_pubkey": creator_pubkey_b32,
        "name": name,
        "avatar_ref": serde_json::Value::Null,
        "created_at": now_secs,
        "updated_at": now_secs
    }));

    // Encrypt with await
    let json = serde_json::to_string(&groups).map_err(|e| e.to_string())?;
    let encrypted = crypto::internal_encrypt(json, None).await;

    // Write back to store after await (re-acquire store)
    {
        let store = db::get_store(&handle);
        store.set("mls_groups".to_string(), serde_json::json!(encrypted));
    }

    Ok(group_id)
}

#[tauri::command]
async fn mls_send_group_message_simple(group_id: String, text: String) -> Result<String, String> {
    // Minimal: validate group exists via JSON store, then return placeholder message_id
    let handle = TAURI_APP.get().ok_or("App handle not initialized")?.clone();

    // Load and decrypt groups
    let store = db::get_store(&handle);
    let groups: Vec<serde_json::Value> = if let Some(v) = store.get("mls_groups") {
        if let Some(enc) = v.as_str() {
            match crypto::internal_decrypt(enc.to_string(), None).await {
                Ok(json) => serde_json::from_str::<Vec<serde_json::Value>>(&json).unwrap_or_default(),
                Err(_) => Vec::new(),
            }
        } else {
            Vec::new()
        }
    } else {
        Vec::new()
    };

    // Validate group exists
    let exists = groups.iter().any(|g| g.get("group_id").and_then(|v| v.as_str()) == Some(group_id.as_str()));
    if !exists {
        return Err("Group not found".to_string());
    }

    // Generate placeholder message_id
    let mut rng = thread_rng();
    let message_id = format!("{:016x}{:016x}", Timestamp::now().as_u64(), rng.gen::<u64>());

    println!("[MLS] send_group_message_simple group_id={}, msg_id={}, len={}", group_id, message_id, text.len());

    // TODO: Wire nostr-mls create_message + publish (Kind 445) and local encrypted storage (mls_messages_{group_id}, mls_timeline_{group_id})

    Ok(message_id)
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

/// Remove a member device from an MLS group
#[tauri::command]
async fn remove_mls_member_device(
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
            mls.remove_member_device(&group_id, &member_npub, &device_id)
                .await
                .map_err(|e| e.to_string())
        })
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))?
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
                // Multi-group sync: read encrypted "mls_groups", iterate group_ids, and sync each
                let store = db::get_store(&handle);
                let enc_opt = match store.get("mls_groups") {
                    Some(v) if v.is_string() => Some(v.as_str().unwrap().to_string()),
                    _ => None,
                };

                let group_ids: Vec<String> = if let Some(enc) = enc_opt {
                    match crypto::internal_decrypt(enc, None).await {
                        Ok(json) => {
                            let arr: Vec<serde_json::Value> = serde_json::from_str(&json).unwrap_or_default();
                            arr.into_iter()
                                .filter_map(|v| v.get("group_id").and_then(|s| s.as_str()).map(|s| s.to_string()))
                                .collect()
                        }
                        Err(_) => Vec::new(),
                    }
                } else {
                    Vec::new()
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
                }

                Ok((total_processed, total_new))
            }
        })
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))?
}

    /// Scan trusted relay for GiftWraps addressed to us, unwrap and ingest MLS Welcomes
#[tauri::command]
async fn sync_mls_welcomes_now() -> Result<u32, String> {
    use nostr_sdk::prelude::*;

    let client = NOSTR_CLIENT.get().ok_or("Nostr client not initialized")?;
    let handle = TAURI_APP.get().ok_or("App handle not initialized")?.clone();

    // Resolve my pubkey for filter
    let signer = client.signer().await.map_err(|e| e.to_string())?;
    let my_public_key = signer.get_public_key().await.map_err(|e| e.to_string())?;
    let my_npub = my_public_key.to_bech32().unwrap_or_default();

    println!(
        "[MLS][welcomes] begin sync for npub={}, relay={}",
        my_npub,
        TRUSTED_RELAY
    );

    // Filter GiftWraps "to me" (Kind 1059) - standard format for MLS Welcomes
    let filter = Filter::new()
        .pubkey(my_public_key)
        .kind(Kind::GiftWrap)
        .limit(2000);

    let events = match client
        .fetch_events_from(vec![TRUSTED_RELAY], filter, std::time::Duration::from_secs(15))
        .await
    {
        Ok(evts) => {
            println!("[MLS][welcomes] fetched {} GiftWrap events", evts.len());
            evts
        }
        Err(e) => {
            eprintln!("[MLS][welcomes] fetch failed: {}", e);
            return Err(e.to_string());
        }
    };

    if events.is_empty() {
        println!("[MLS][welcomes] no GiftWraps found for npub={}", my_npub);
        return Ok(0);
    }

    // Ingest welcomes using non-Send MLS engine on blocking thread
    tokio::task::spawn_blocking(move || {
        let rt = tokio::runtime::Handle::current();
        rt.block_on(async move {
            println!("[MLS][welcomes] unwrap phase starting ({} wrappers)...", events.len());

            // Phase 1: unwrap all GiftWraps (async) BEFORE acquiring engine to avoid awaits with engine in scope
            let mut unwrapped: Vec<(nostr_sdk::EventId, nostr_sdk::UnsignedEvent)> = Vec::new();
            let mut unwrap_failures: u32 = 0;

            for wrapper in events {
                let wid = wrapper.id;
                // Log receipt before unwrap for direct grepping
                let author_b32 = wrapper.pubkey.to_bech32().unwrap_or_default();
                println!("[MLS][welcomes][recv] wrapper_id={}, author={}", wid, author_b32);
                match client.unwrap_gift_wrap(&wrapper).await {
                    Ok(u) => {
                        // Convert rumor Event -> UnsignedEvent via JSON round-trip
                        match serde_json::to_string(&u.rumor)
                            .ok()
                            .and_then(|s| nostr_sdk::UnsignedEvent::from_json(s.as_bytes()).ok())
                        {
                            Some(unsigned) => {
                                println!("[MLS][welcomes] unwrapped wrapper_id={}", wid);
                                unwrapped.push((wid, unsigned));
                            }
                            None => {
                                unwrap_failures = unwrap_failures.saturating_add(1);
                                eprintln!(
                                    "[MLS][welcomes] failed to convert rumor to UnsignedEvent for wrapper_id={}",
                                    wid
                                );
                            }
                        }
                    }
                    Err(e) => {
                        unwrap_failures = unwrap_failures.saturating_add(1);
                        eprintln!("[MLS][welcomes] unwrap_gift_wrap failed wrapper_id={} err={}", wid, e);
                    }
                }
            }

            println!(
                "[MLS][welcomes] unwrap phase complete: successes={}, failures={}",
                unwrapped.len(),
                unwrap_failures
            );

            // Phase 2: create engine and process welcomes (no awaits while engine is in scope)
            let mls = match MlsService::new_persistent(&handle) {
                Ok(s) => {
                    println!("[MLS][welcomes] persistent engine opened");
                    s
                }
                Err(e) => {
                    eprintln!("[MLS][welcomes] failed to init persistent engine: {}", e);
                    return Err(e.to_string());
                }
            };
            let engine = match mls.engine() {
                Ok(e) => e,
                Err(e) => {
                    eprintln!("[MLS][welcomes] failed to get engine: {}", e);
                    return Err(e.to_string());
                }
            };

            let mut ingested: u32 = 0;
            for (wrapper_id, unsigned) in unwrapped.iter() {
                match engine.process_welcome(wrapper_id, unsigned) {
                    Ok(_welcome) => {
                        ingested = ingested.saturating_add(1);
                        println!(
                            "[MLS][welcomes] processed welcome wrapper_id={}",
                            wrapper_id
                        );
                    }
                    Err(e) => {
                        // Not a welcome or invalid; continue
                        eprintln!(
                            "[MLS][welcomes] process_welcome failed wrapper_id={} err={}",
                            wrapper_id,
                            e
                        );
                        continue;
                    }
                }
            }

            println!("[MLS][welcomes] done: ingested={}", ingested);
            Ok::<u32, String>(ingested)
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
    tokio::task::spawn_blocking(move || {
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
                    group_admin_pubkeys: w.group_admin_pubkeys.iter().map(|pk| pk.to_hex()).collect(),
                    group_relays: w.group_relays.iter().map(|r| r.to_string()).collect(),
                    welcomer: w.welcomer.to_hex(),
                    member_count: w.member_count,
                });
            }

            Ok(out)
        })
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))?
}

/// Accept an MLS welcome by its welcome (rumor) event id hex
#[tauri::command]
async fn accept_mls_welcome(welcome_event_id_hex: String) -> Result<bool, String> {
    // Run non-Send MLS engine work on blocking thread; drive async via current runtime
    tokio::task::spawn_blocking(move || {
        let handle = TAURI_APP.get().ok_or("App handle not initialized")?.clone();
        let rt = tokio::runtime::Handle::current();
        rt.block_on(async move {
            let mls = MlsService::new_persistent(&handle).map_err(|e| e.to_string())?;
            
            // Get welcome details and accept it (engine work in no-await scope)
            let (nostr_group_id, engine_group_id, group_name, welcomer_hex) = {
                let engine = mls.engine().map_err(|e| e.to_string())?;
                
                let id = nostr_sdk::EventId::from_hex(&welcome_event_id_hex).map_err(|e| e.to_string())?;
                let welcome_opt = engine.get_welcome(&id).map_err(|e| e.to_string())?;
                let welcome = welcome_opt.ok_or_else(|| "Welcome not found".to_string())?;
                
                // Extract metadata before accepting
                let nostr_group_id_bytes = welcome.nostr_group_id.clone();
                let group_name = welcome.group_name.clone();
                let welcomer_hex = welcome.welcomer.to_hex();
                
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
                
                (nostr_group_id, engine_group_id, group_name, welcomer_hex)
            }; // engine dropped here
            
            // Now persist the group metadata (awaitable section)
            let mut groups = mls.read_groups().await.map_err(|e| e.to_string())?;
            
            // Check if group already exists (idempotent)
            let exists = groups.iter().any(|g| g.group_id == nostr_group_id);
            
            if !exists {
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
                };
                
                groups.push(metadata.clone());
                mls.write_groups(&groups).await.map_err(|e| e.to_string())?;
                
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
                        if let Err(e) = db_migration::save_chat(handle.clone(), chat).await {
                            eprintln!("[MLS] Failed to save chat after welcome acceptance: {}", e);
                        }
                    }
                }
                
                println!("[MLS] Persisted group metadata after accept: group_id={}", nostr_group_id);
            } else {
                println!("[MLS] Group already exists in metadata: group_id={}", nostr_group_id);
            }

            // Emit event so the UI can refresh welcome lists and group lists
            if let Some(app) = TAURI_APP.get() {
                let _ = app.emit("mls_welcome_accepted", serde_json::json!({
                    "welcome_event_id": welcome_event_id_hex,
                    "group_id": nostr_group_id
                }));
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

            Ok(true)
        })
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))?
}

/// Process an incoming MLS event from the nostr network
#[tauri::command]
async fn process_mls_event(
    event_json: String,
) -> Result<bool, String> {
    // Run non-Send MLS engine work on a blocking thread; drive async via current runtime
    tokio::task::spawn_blocking(move || {
        let handle = TAURI_APP.get().ok_or("App handle not initialized")?.clone();
        let rt = tokio::runtime::Handle::current();
        rt.block_on(async move {
            let mls = MlsService::new_persistent(&handle).map_err(|e| e.to_string())?;
            mls.process_incoming_event(&event_json)
                .await
                .map_err(|e| e.to_string())
        })
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))?
}

#[tauri::command]
async fn list_mls_groups() -> Result<Vec<String>, String> {
    // Read and decrypt "mls_groups" and return group_id list
    let handle = TAURI_APP.get().ok_or("App handle not initialized")?.clone();

    // Read encrypted value from store without awaiting while holding the store
    let enc_opt: Option<String> = {
        let store = db::get_store(&handle);
        match store.get("mls_groups") {
            Some(v) if v.is_string() => Some(v.as_str().unwrap().to_string()),
            _ => None,
        }
    };

    if let Some(enc) = enc_opt {
        match crypto::internal_decrypt(enc, None).await {
            Ok(json) => {
                let arr: Vec<serde_json::Value> = serde_json::from_str(&json).unwrap_or_default();
                let ids = arr
                    .into_iter()
                    .filter_map(|v| v.get("group_id").and_then(|s| s.as_str()).map(|s| s.to_string()))
                    .collect::<Vec<String>>();
                Ok(ids)
            }
            Err(e) => Err(format!("Failed to decrypt mls_groups: {:?}", e)),
        }
    } else {
        Ok(Vec::new())
    }
}

#[derive(serde::Serialize, Clone)]
struct MlsGroupInfo {
    group_id: String,
    engine_group_id: String,
    name: String,
    avatar_ref: Option<String>,
    created_at: u64,
    updated_at: u64,
    // Total number of members in the group (computed from persistent MLS engine)
    member_count: u32,
}

/// Return detailed MLS group metadata so the frontend can render group names and avatars
#[tauri::command]
async fn list_mls_groups_detailed() -> Result<Vec<MlsGroupInfo>, String> {
    // Run in a blocking thread so the outer future is Send (Tauri requires commands' futures to be Send)
    tokio::task::spawn_blocking(move || {
        // Acquire handle in blocking context
        let handle = TAURI_APP.get().ok_or("App handle not initialized")?.clone();

        // Use current runtime to drive our small async decrypt step
        let rt = tokio::runtime::Handle::current();
        rt.block_on(async move {
            // Read encrypted value from store without awaiting while holding the store
            let enc_opt: Option<String> = {
                let store = db::get_store(&handle);
                match store.get("mls_groups") {
                    Some(v) if v.is_string() => Some(v.as_str().unwrap().to_string()),
                    _ => None,
                }
            };

            if let Some(enc) = enc_opt {
                // Decrypt after releasing the store reference
                match crypto::internal_decrypt(enc, None).await {
                    Ok(json) => {
                        // Parse as generic JSON to be resilient to legacy entries
                        let arr: Vec<serde_json::Value> = serde_json::from_str(&json).unwrap_or_default();

                        // Try to open persistent MLS engine for computing member counts
                        // This block must avoid awaits while engine is in scope
                        let maybe_engine = match MlsService::new_persistent(&handle) {
                            Ok(svc) => svc.engine().ok(),
                            Err(_) => None,
                        };

                        // Needed to decode engine group ids
                        use mdk_core::prelude::GroupId;

                        // Build enriched infos with member_count computed from engine (fallback to 0 on any failure)
                        let infos = arr
                            .into_iter()
                            .map(|v| {
                                let group_id = v
                                    .get("group_id")
                                    .and_then(|s| s.as_str())
                                    .unwrap_or_default()
                                    .to_string();

                                let engine_group_id = v
                                    .get("engine_group_id")
                                    .and_then(|s| s.as_str())
                                    .unwrap_or("")
                                    .to_string();

                                let name = v
                                    .get("name")
                                    .and_then(|s| s.as_str())
                                    .unwrap_or("")
                                    .to_string();

                                let avatar_ref = v
                                    .get("avatar_ref")
                                    .and_then(|s| s.as_str())
                                    .map(|s| s.to_string());

                                let created_at = v
                                    .get("created_at")
                                    .and_then(|n| n.as_u64())
                                    .unwrap_or(0);

                                let updated_at = v
                                    .get("updated_at")
                                    .and_then(|n| n.as_u64())
                                    .unwrap_or(created_at);

                                // Prefer engine_group_id for engine lookup; fallback to wire group_id
                                let engine_id_hex = if !engine_group_id.is_empty() {
                                    engine_group_id.clone()
                                } else {
                                    group_id.clone()
                                };

                                // Default to 0 members if engine not available or lookup fails
                                let mut member_count: u32 = 0;

                                if let Some(engine) = &maybe_engine {
                                    if let Ok(bytes) = hex::decode(&engine_id_hex) {
                                        let gid = GroupId::from_slice(&bytes);
                                        if let Ok(pks) = engine.get_members(&gid) {
                                            member_count = pks.len() as u32;
                                        }
                                    }
                                }

                                MlsGroupInfo {
                                    group_id,
                                    engine_group_id,
                                    name,
                                    avatar_ref,
                                    created_at,
                                    updated_at,
                                    member_count,
                                }
                            })
                            .collect::<Vec<MlsGroupInfo>>();

                        Ok(infos)
                    }
                    Err(e) => Err(format!("Failed to decrypt mls_groups: {:?}", e)),
                }
            } else {
                Ok(Vec::new())
            }
        })
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))?
}


#[derive(serde::Serialize, Clone)]
struct GroupMembers {
    group_id: String,
    engine_group_id: String,
    members: Vec<String>, // npubs
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

            // Try to resolve members via engine API
            use mdk_core::prelude::GroupId;
            use nostr_sdk::prelude::PublicKey;

            // Decode engine id to GroupId; fallback to using wire id bytes if needed
            let mut members: Vec<String> = Vec::new();
            let mut engine_gid_hex = engine_id.clone();


            // Preferred path: use engine_group_id if it’s valid hex
            if let Ok(gid_bytes) = hex::decode(&engine_id) {
                let gid = GroupId::from_slice(&gid_bytes);
                // Attempt API: get_group_members(&GroupId)
                match engine.get_members(&gid) {
                    Ok(pk_list) => {
                        members = pk_list
                            .into_iter()
                            .map(|pk| pk.to_bech32().unwrap_or_else(|_| pk.to_hex()))
                            .collect();
                    }
                    Err(_) => {
                        // Fallback: enumerate engine groups and match by ids, then use any available member list on entry
                        if let Ok(groups) = engine.get_groups() {
                            for g in groups {
                                let gid_hex = hex::encode(g.mls_group_id.as_slice());
                                let wire_hex = hex::encode(&g.nostr_group_id);
                                if gid_hex == engine_id || wire_hex == wire_id {
                                    // Try common field names for members (SDK variations)
                                    #[allow(unused_mut)]
                                    let mut found = false;
                                    // If the group entry exposes `members` (Vec<PublicKey>)
                                    #[allow(unused_variables)]
                                    let maybe_members = {
                                        // This block intentionally uses pattern matching guarded by cfg to avoid compile issues if field doesn't exist
                                        // We will try to access a field named `members` via debug string as last resort (no-op here).
                                        None::<Vec<PublicKey>>
                                    };
                                    if let Some(pks) = maybe_members {
                                        members = pks
                                            .into_iter()
                                            .map(|pk| pk.to_bech32().unwrap_or_else(|_| pk.to_hex()))
                                            .collect();
                                        found = true;
                                    }
                                    if !found {
                                        // If not available, keep empty; engine.get_group_members is the canonical path
                                    }
                                    engine_gid_hex = gid_hex;
                                    break;
                                }
                            }
                        }
                    }
                }
            } else {
                // engine_id was not hex; try match by wire id in engine list
                if let Ok(groups) = engine.get_groups() {
                    for g in groups {
                        let wire_hex = hex::encode(&g.nostr_group_id);
                        if wire_hex == wire_id {
                            engine_gid_hex = hex::encode(g.mls_group_id.as_slice());
                            // Attempt direct engine API with resolved engine id
                            if let Ok(gid_bytes) = hex::decode(&engine_gid_hex) {
                                let gid = GroupId::from_slice(&gid_bytes);
                                if let Ok(pk_list) = engine.get_members(&gid) {
                                    members = pk_list
                                        .into_iter()
                                        .map(|pk| pk.to_bech32().unwrap_or_else(|_| pk.to_hex()))
                                        .collect();
                                }
                            }
                            break;
                        }
                    }
                }
            }

            Ok(GroupMembers {
                group_id: wire_id,
                engine_group_id: engine_gid_hex,
                members,
            })
        })
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))?
}

/// Leave an MLS group
/// TODO: Implement MLS leave operation
#[tauri::command]
async fn leave_mls_group(
    group_id: String,
    /*
    UI error mapping and behavior for rust.refresh_keypackages_for_contact()
    - Invalid npub format:
      • PublicKey::from_bech32(&npub) -> Err(String). The exact string is bubbled to the frontend and shown verbatim.
    - Network fetch failures:
      • client.fetch_events_from(... Kind::MlsKeyPackage ...) -> Err(String). Bubbled as actionable error text.
    - Empty result (no KeyPackages):
      • Returns Ok(vec![]) here. In rust.create_group_chat(), an empty device list yields "No device keypackages found for {npub}" and aborts group creation (atomic create semantics).
    - Index persistence:
      • Updates plaintext "mls_keypackage_index" synchronously after network await; avoids awaits while store is held to prevent deadlocks.
    - Return value:
      • Vec(device_id, keypackage_ref); where both are currently the KeyPackage event id (hex). Device selection policy is currently "first result"; can evolve to "newest by fetched_at" later.
    */
) -> Result<(), String> {
    // TODO: Implement leave operation via MlsService
    let _ = group_id;
    Err("Not implemented".to_string())
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
        .limit(200);

    // Fetch from TRUSTED_RELAY with short timeout
    let events = client
        .fetch_events_from(vec![TRUSTED_RELAY], filter, std::time::Duration::from_secs(10))
        .await
        .map_err(|e| e.to_string())?;

    // Prepare results and index entries
    let owner_pubkey_b32 = contact_pubkey.to_bech32().map_err(|e| e.to_string())?;
    let mut results: Vec<(String, String)> = Vec::with_capacity(events.len());
    let mut new_entries: Vec<serde_json::Value> = Vec::with_capacity(events.len());

    for e in events {
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
    let store = db::get_store(&handle);

    // Load existing index
    let mut index: Vec<serde_json::Value> = match store.get("mls_keypackage_index") {
        Some(v) => serde_json::from_value(v.clone()).unwrap_or_default(),
        None => Vec::new(),
    };

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
    store.set("mls_keypackage_index".to_string(), serde_json::json!(index));

    Ok(results)
}

/// Check MLS group health and identify groups that need re-syncing

/// Remove orphaned MLS groups from metadata that are not in engine state

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
            #[cfg(desktop)]
            app.handle().plugin(tauri_plugin_updater::Builder::new().build())?;
            #[cfg(desktop)]
            app.handle().plugin(tauri_plugin_process::init())?;
            
            let handle = app.app_handle().clone();

            // Check if we need to migrate files
            // Note: this is restricted to Desktop only, since Vector Mobile didn't even exist before the Migration was done
            // Note: this entire block can be removed once sufficient versions have passed.
            #[cfg(not(any(target_os = "ios", target_os = "android")))]
            {
                let needs_migration = tauri::async_runtime::block_on(async {
                    // Choose the appropriate base directory based on platform
                    let base_directory = if cfg!(target_os = "ios") {
                        tauri::path::BaseDirectory::Document
                    } else {
                        tauri::path::BaseDirectory::Download
                    };

                    // Resolve the directory path using the determined base directory
                    if let Ok(dir) = handle.path().resolve("vector", base_directory) {
                        if dir.exists() {
                            // Count nonce-based files
                            if let Ok(entries) = std::fs::read_dir(&dir) {
                                for entry in entries {
                                    if let Ok(entry) = entry {
                                        if let Some(filename) = entry.path().file_name() {
                                            if is_nonce_filename(&filename.to_string_lossy()) {
                                                return true; // Found at least one file to migrate
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    false
                });

                // If we need to migrate, create migration window first
                if needs_migration {
                    // Hide the main window initially
                    if let Some(main_window) = app.get_webview_window("main") {
                        let _ = main_window.hide();
                    }

                    // Create migration window
                    let _ = WebviewWindowBuilder::new(
                        app,
                        "migration",
                        WebviewUrl::App("migration.html".into())
                    )
                    .title("Vector - File Migration")
                    .inner_size(500.0, 300.0)
                    .resizable(false)
                    .center()
                    .build()
                    .expect("Failed to create migration window");

                    // Clone handle for the migration task
                    let handle_clone = handle.clone();
                    let main_window = app.get_webview_window("main").unwrap();
                    
                    // Run migration in a separate task
                    tauri::async_runtime::spawn(async move {
                        match migrate_nonce_files_to_hash(&handle_clone).await {
                            Ok(migration_map) => {
                                // Store the migration map for database update after login
                                if !migration_map.is_empty() {
                                    let _ = PENDING_MIGRATION.set(migration_map);
                                }
                            }
                            Err(e) => eprintln!("Failed to migrate attachment files: {}", e),
                        }
                        
                        // After migration, show main window and close migration window
                        let _ = main_window.show();
                        if let Some(mig_win) = handle_clone.get_webview_window("migration") {
                            let _ = mig_win.close();
                        }
                    });
                }
            }

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

            // Startup log: persistent MLS device_id if present
            {
                let store = db::get_store(&handle);
                if let Some(v) = store.get("mls_device_id") {
                    if let Some(id) = v.as_str() {
                        println!("[MLS] Found persistent mls_device_id at startup: {}", id);
                    }
                }
            }

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
            db::get_whisper_auto_translate,
            db::set_whisper_auto_translate,
            db::get_whisper_auto_transcribe,
            db::set_whisper_auto_transcribe,
            db::get_whisper_model_name,
            db::set_whisper_model_name,
            db::get_pkey,
            db::set_pkey,
            db::get_seed,
            db::set_seed,
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
            message::react,
            message::react_to_message,
            message::fetch_msg_metadata,
            fetch_messages,
            warmup_nip96_servers,
            get_chat_messages,
            generate_blurhash_preview,
            download_attachment,
            login,
            notifs,
            get_relays,
            monitor_relay_connections,
            start_typing,
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
            db::get_invite_code,
            db::set_invite_code,
            export_keys,
            bootstrap_mls_device_keypackage,
            // Simple MLS command wrappers for console/manual testing:
            mls_create_group_simple,
            mls_send_group_message_simple,
            // MLS core commands
            create_group_chat,
            create_mls_group,
            send_mls_group_message,
            sync_mls_groups_now,
            list_mls_groups,
            list_mls_groups_detailed,
            // MLS welcome/invite commands
            sync_mls_welcomes_now,
            list_pending_mls_welcomes,
            accept_mls_welcome,
            // MLS advanced helpers
            process_mls_event,
            add_mls_member_device,
            remove_mls_member_device,
            get_mls_group_members,
            leave_mls_group,
            refresh_keypackages_for_contact,
            #[cfg(all(not(target_os = "android"), feature = "whisper"))]
            whisper::delete_whisper_model,
            #[cfg(all(not(target_os = "android"), feature = "whisper"))]
            whisper::list_models
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
