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
static TRUSTED_PRIVATE_NIP96: &str = "https://medea-1-swiss.vectorapp.io";
static PRIVATE_NIP96_CONFIG: OnceCell<ServerConfig> = OnceCell::new();


static MNEMONIC_SEED: OnceCell<String> = OnceCell::new();
static ENCRYPTION_KEY: OnceCell<[u8; 32]> = OnceCell::new();
static NOSTR_CLIENT: OnceCell<Client> = OnceCell::new();
static TAURI_APP: OnceCell<AppHandle> = OnceCell::new();
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

            // Run chat migration if needed
            if db_migration::is_migration_needed(&handle).await.unwrap_or(true) {
                // Get all existing messages from the old profile-based storage
                let profile_messages = db::old_get_all_messages(&handle).await.unwrap_or_else(|e| {
                    eprintln!("Failed to get profile messages for migration: {}", e);
                    Vec::new()
                });
                
                if !profile_messages.is_empty() {
                    println!("Migrating {} messages from profile-based to chat-based storage...", profile_messages.len());
                    
                    // Emit migration start event to frontend
                    handle.emit("progress_operation", serde_json::json!({
                        "type": "start",
                        "message": "Migrating chats"
                    })).unwrap();
                    
                    // Perform the migration
                    match db_migration::migrate_profile_messages_to_chats(&handle, profile_messages).await {
                        Ok(chats) => {
                            println!("Successfully migrated {} chats", chats.len());
                            
                            // Load the migrated chats into state (we already have the lock)
                            for chat in chats {
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
                                
                                state.chats.push(chat);
                            }
                            
                            // Mark migration as complete
                            if let Err(e) = db_migration::complete_migration(handle.clone()).await {
                                eprintln!("Failed to mark chat migration as complete: {}", e);
                            }
                        },
                        Err(e) => {
                            eprintln!("Failed to migrate chats: {}", e);
                            // Emit error event
                            handle.emit("progress_operation", serde_json::json!({
                                "type": "error",
                                "message": format!("Chat migration error: {}", e)
                            })).unwrap();
                        }
                    }
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
    
                        // Sort the chats by their last received message
                        state.chats.sort_by(|a, b| b.last_message_time().cmp(&a.last_message_time()));
                    }
                } else {
                    eprintln!("Failed to load chats from database: {:?}", slim_chats_result);
                }
                
                // Send the state to our frontend to signal finalised init with a full state
                handle.emit("init_finished", serde_json::json!({
                    "profiles": &state.profiles,
                    "chats": &state.chats
                })).unwrap();
            } else {
                println!("[INIT] Checking filesystem integrity...");
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
    
    if updated_count > 0 {
        println!("Successfully updated {} attachment references in database", updated_count);
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
                
                println!("Migrating timestamp for message {}: {} -> {}", message.id, old_timestamp, message.at);
                
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
    
    if updated_count > 0 {
        println!("Successfully migrated {} message timestamps from seconds to milliseconds", updated_count);
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
                    None => false /* No Reference (Note ID) supplied */,
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
                let extension = match mime_type {
                    // Images
                    "image/png" => "png",
                    "image/jpeg" | "image/jpg" => "jpg",
                    "image/gif" => "gif",
                    "image/webp" => "webp",
                    "image/svg+xml" => "svg",
                    "image/bmp" | "image/x-ms-bmp" => "bmp",
                    "image/x-icon" | "image/vnd.microsoft.icon" => "ico",
                    "image/tiff" => "tiff",
                    
                    // Raw Images
                    "image/x-adobe-dng" => "dng",
                    "image/x-canon-cr2" => "cr2",
                    "image/x-nikon-nef" => "nef",
                    "image/x-sony-arw" => "arw",
                    
                    // Audio
                    "audio/wav" | "audio/x-wav" | "audio/wave" => "wav",
                    "audio/mp3" | "audio/mpeg" => "mp3",
                    "audio/flac" => "flac",
                    "audio/ogg" => "ogg",
                    "audio/mp4" => "m4a",
                    "audio/aac" | "audio/x-aac" => "aac",
                    "audio/x-ms-wma" => "wma",
                    "audio/opus" => "opus",
                    
                    // Videos
                    "video/mp4" => "mp4",
                    "video/webm" => "webm",
                    "video/quicktime" => "mov",
                    "video/x-msvideo" => "avi",
                    "video/x-matroska" => "mkv",
                    "video/x-flv" => "flv",
                    "video/x-ms-wmv" => "wmv",
                    "video/mpeg" => "mpg",
                    "video/3gpp" => "3gp",
                    "video/ogg" => "ogv",
                    "video/mp2t" => "ts",
                    
                    // Documents
                    "application/pdf" => "pdf",
                    "application/msword" => "doc",
                    "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => "docx",
                    "application/vnd.ms-excel" => "xls",
                    "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" => "xlsx",
                    "application/vnd.ms-powerpoint" => "ppt",
                    "application/vnd.openxmlformats-officedocument.presentationml.presentation" => "pptx",
                    "application/vnd.oasis.opendocument.text" => "odt",
                    "application/vnd.oasis.opendocument.spreadsheet" => "ods",
                    "application/vnd.oasis.opendocument.presentation" => "odp",
                    "application/rtf" => "rtf",
                    
                    // Text/Data
                    "text/plain" => "txt",
                    "text/markdown" => "md",
                    "text/csv" => "csv",
                    "application/json" => "json",
                    "application/xml" | "text/xml" => "xml",
                    "application/x-yaml" | "text/yaml" => "yaml",
                    "application/toml" => "toml",
                    "application/sql" => "sql",
                    
                    // Archives
                    "application/zip" => "zip",
                    "application/x-rar-compressed" | "application/vnd.rar" => "rar",
                    "application/x-7z-compressed" => "7z",
                    "application/x-tar" => "tar",
                    "application/gzip" => "gz",
                    "application/x-bzip2" => "bz2",
                    "application/x-xz" => "xz",
                    "application/x-iso9660-image" => "iso",
                    "application/x-apple-diskimage" => "dmg",
                    "application/vnd.android.package-archive" => "apk",
                    "application/java-archive" => "jar",
                    
                    // 3D Files
                    "model/obj" => "obj",
                    "model/gltf+json" => "gltf",
                    "model/gltf-binary" => "glb",
                    "model/stl" | "application/sla" => "stl",
                    "model/vnd.collada+xml" => "dae",
                    
                    // Code
                    "text/javascript" | "application/javascript" => "js",
                    "text/typescript" | "application/typescript" => "ts",
                    "text/x-python" | "application/x-python" => "py",
                    "text/x-rust" => "rs",
                    "text/x-go" => "go",
                    "text/x-java" => "java",
                    "text/x-c" => "c",
                    "text/x-c++" => "cpp",
                    "text/x-csharp" => "cs",
                    "text/x-ruby" => "rb",
                    "text/x-php" => "php",
                    "text/x-swift" => "swift",
                    
                    // Web
                    "text/html" => "html",
                    "text/css" => "css",
                    
                    // Other
                    "application/x-msdownload" | "application/x-dosexec" => "exe",
                    "application/x-msi" => "msi",
                    "application/x-font-ttf" | "font/ttf" => "ttf",
                    "application/x-font-otf" | "font/otf" => "otf",
                    "font/woff" => "woff",
                    "font/woff2" => "woff2",
                    
                    // Fallback - extract extension from mime subtype
                    _ => mime_type.split('/').nth(1).unwrap_or("bin"),
                };

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
            // Check if this chat involves the specified profile
            if chat.has_participant(&npub) {
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
            if chat.has_participant(&npub) {
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
                if chat.has_participant(&npub) {
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
                if chat.has_participant(&npub) {
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
                    if chat.has_participant(&npub) {
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
    crypto::internal_decrypt(ciphertext, password).await
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
            message::fetch_msg_metadata,
            fetch_messages,
            warmup_nip96_servers,
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
            #[cfg(all(not(target_os = "android"), feature = "whisper"))]
            whisper::delete_whisper_model,
            #[cfg(all(not(target_os = "android"), feature = "whisper"))]
            whisper::list_models
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
