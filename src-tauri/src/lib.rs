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


mod db_migration;
use db_migration::save_chat_messages;

mod db_sql_migration;

mod voice;
use voice::AudioRecorder;

mod net;

mod blossom;

mod util;
use util::{get_file_type_description, calculate_file_hash, format_bytes};

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

mod profile_sync;

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

/// Perform Store-to-SQL migration
/// This command is called by the frontend when migration is needed
#[tauri::command]
async fn perform_database_migration<R: Runtime>(
    handle: AppHandle<R>
) -> Result<(), String> {
    // Perform the migration (no password needed - uses decrypted key from Nostr client)
    db_sql_migration::migrate_store_to_sql(handle.clone()).await?;
    
    // Auto-select the first account after migration
    if let Ok(Some(npub)) = account_manager::auto_select_account(&handle) {
        println!("[Migration] Auto-selected account after migration: {}", npub);
    } else {
        println!("[Migration] Warning: No account found after migration");
    }
    
    // After successful migration, trigger fetch_messages to load the new database
    tauri::async_runtime::spawn(async move {
        fetch_messages(handle, true, None).await;
    });
    
    Ok(())
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
            // Check if Store-to-SQL migration is needed
            if db_sql_migration::is_sql_migration_needed(&handle).await.unwrap_or(false) {
                // Migration is needed - this will be triggered by the frontend
                // The frontend should call a migration command with the user's password
                // For now, we'll emit a special event to notify the frontend
                handle.emit("migration_needed", serde_json::json!({
                    "message": "Database upgrade required. Please enter your password to continue."
                })).ok();
                
                // Don't proceed with normal initialization until migration is complete
                // The frontend will call the migration command, which will then call fetch_messages again
                return;
            }
            
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

                // Load chats and their messages from database
                let slim_chats_result = db_migration::get_all_chats(&handle).await;
                if let Ok(slim_chats) = slim_chats_result {
                    // Load MLS groups to check for evicted status
                    let mls_groups: Option<Vec<mls::MlsGroupMetadata>> =
                        db_migration::load_mls_groups(&handle).await.ok();
                    
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
                        
                        // Load messages for this chat
                        let messages_result = db_migration::get_chat_messages(&handle, &chat.id()).await;
                        if let Ok(messages) = messages_result {
                            // Add all messages to the chat and check for downloaded attachments
                            for message in messages {
                                // Check if this message has downloaded attachments (for integrity check)
                                if !needs_integrity_check && message.attachments.iter().any(|att| att.downloaded) {
                                    needs_integrity_check = true;
                                }
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
            }
            
            if needs_integrity_check {
                // Clean up empty file attachments first
                cleanup_empty_file_attachments(&handle, &mut state).await;
                
                // Check integrity without dropping state
                check_attachment_filesystem_integrity(&handle, &mut state).await;
                
                // Preload ID caches for maximum performance
                if let Err(e) = db_migration::preload_id_caches(&handle).await {
                    eprintln!("[Cache] Failed to preload ID caches: {}", e);
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
                if let Err(e) = db_migration::preload_id_caches(&handle).await {
                    eprintln!("[Cache] Failed to preload ID caches: {}", e);
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

    // Count total events fetched (for DeepRescan) and new messages added (for other modes)
    let total_events_count = events.len() as u16;
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
                if let Err(e) = db_sql_migration::check_and_vacuum_if_needed(&handle_clone).await {
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
                Ok(_) => true,
                Err(_e) => false,
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
            
        // Send notification outside of state lock using new generic system
        if let Some((display_name, content)) = display_info {
            let notification = NotificationData::direct_message(display_name, content);
            show_notification_generic(notification);
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

        // Save the new message to DB (chat_id = contact npub for DMs)
        if let Some(handle) = TAURI_APP.get() {
            // Only save the single new message (efficient!)
            let _ = db_migration::save_message(handle.clone(), contact, &msg).await;
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
        
        // Send notification outside of state lock using new generic system
        if let Some((display_name, file_extension)) = display_info {
            let file_description = "Sent a ".to_string() + &get_file_type_description(&file_extension);
            let notification = NotificationData::direct_message(display_name, file_description);
            show_notification_generic(notification);
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

    // If accepted in-state: commit to the DB and emit to the frontend
    if was_msg_added_to_state {
        // Send it to the frontend
        if let Some(handle) = TAURI_APP.get() {
            handle.emit("message_new", serde_json::json!({
                "message": &msg,
                "chat_id": contact
            })).unwrap();
        }

        // Save the new message to DB (chat_id = contact npub for DMs)
        if let Some(handle) = TAURI_APP.get() {
            // Only save the single new message (efficient!)
            let _ = db_migration::save_message(handle.clone(), contact, &msg).await;
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
                let _ = db_migration::save_message(handle.clone(), &chat_id, &msg).await;
            }
        }
    }

    reaction_added
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
                        let is_member: bool = if let Ok(groups) = db_migration::load_mls_groups(&handle).await {
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
                                                            RumorProcessingResult::TextMessage(message) => {
                                                                // Clear typing indicator for this sender (they just sent a message)
                                                                let sender_npub = msg.pubkey.to_bech32().unwrap_or_default();
                                                                
                                                                let (was_added, active_typers, should_notify) = {
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
                                                            RumorProcessingResult::FileAttachment(message) => {
                                                                // Clear typing indicator for this sender (they just sent a message)
                                                                let sender_npub = msg.pubkey.to_bech32().unwrap_or_default();
                                                                let is_file = true;
                                                                
                                                                let (was_added, active_typers, should_notify) = {
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
                                                                            use crate::db_migration::save_chat;
                                                                            let _ = save_chat(handle.clone(), &chat).await;
                                                                            let _ = db_migration::save_message(handle.clone(), &group_id_for_persist, &message).await;
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
                                                                                let _ = db_migration::save_message(handle.clone(), &chat_id, &msg).await;
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

/// Get the list of Blossom media servers (Tauri command)
#[tauri::command]
async fn get_media_servers() -> Vec<String> {
    get_blossom_servers()
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

                    let _ = db_migration::save_message(handle.clone(), &npub, &updated_message).await;
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
    if let Some(handle) = TAURI_APP.get() {
        let app_data = handle.path().app_local_data_dir().ok();
        if let Some(data_dir) = app_data {
            let profile_db = data_dir.join(&npub).join("vector.db");
            if profile_db.exists() {
                // Existing account - just set as current
                let _ = crate::account_manager::set_current_account(npub.clone());
                println!("[Login] Set current account for SQL mode: {}", npub);
            } else {
                // Check if Store-to-SQL migration is needed before creating new database
                let migration_needed = db_sql_migration::is_sql_migration_needed(handle).await.unwrap_or(false);
                if migration_needed {
                    println!("[Login] Store migration needed - database will be created during migration");
                    // Don't create database yet - migration will handle it
                    // Just return the keys and let fetch_messages trigger the migration
                } else {
                    // New account - initialize database and set as current
                    if let Err(e) = account_manager::init_profile_database(handle, &npub).await {
                        eprintln!("[Login] Failed to initialize profile database: {}", e);
                    } else if let Err(e) = account_manager::set_current_account(npub.clone()) {
                        eprintln!("[Login] Failed to set current account: {}", e);
                    } else {
                        println!("[Login] Initialized new profile database and set current account: {}", npub);
                    }
                }
            }
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
        match bootstrap_mls_device_keypackage().await {
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
            match bootstrap_mls_device_keypackage().await {
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

    // Delete the current account's profile directory (SQL database)
    if let Ok(npub) = account_manager::get_current_account() {
        if let Ok(profile_dir) = account_manager::get_profile_directory(&handle, &npub) {
            if profile_dir.exists() {
                let _ = std::fs::remove_dir_all(&profile_dir);
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

    // Delete the MLS folder in AppData
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
    os: String,
    is_mobile: bool,
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

    let is_mobile = cfg!(target_os = "android") || cfg!(target_os = "ios");

    PlatformFeatures {
        transcription: cfg!(all(not(target_os = "android"), feature = "whisper")),
        os: os.to_string(),
        is_mobile,
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
    
    // Send only to trusted relay
    client.send_event_to([TRUSTED_RELAY], &event).await.map_err(|e| e.to_string())?;
    
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
            db_migration::save_chat_messages(handle.clone(), chat.id(), &messages_to_update).await
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
    
    // Get storage info after clearing to calculate freed space
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
    
    let events = client
        .fetch_events_from(vec![TRUSTED_RELAY], filter, std::time::Duration::from_secs(10))
        .await
        .map_err(|e| e.to_string())?;
    
    // Check if they have a valid badge claim from the event period
    for event in events {
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


/// Bootstrap this device's MLS KeyPackage: ensure device_id, publish if missing, and cache reference
#[tauri::command]
async fn bootstrap_mls_device_keypackage() -> Result<serde_json::Value, String> {
    // Access handle and client
    let handle = TAURI_APP.get().ok_or("App handle not initialized")?.clone();
    let client = NOSTR_CLIENT.get().ok_or("Nostr client not initialized")?;

    // Ensure a persistent device_id exists
    let device_id: String = match db_migration::load_mls_device_id(&handle).await {
        Ok(Some(id)) => id,
        _ => {
            let id: String = thread_rng()
                .sample_iter(&Alphanumeric)
                .take(12)
                .map(char::from)
                .collect::<String>()
                .to_lowercase();
            let _ = db_migration::save_mls_device_id(handle.clone(), &id).await;
            id
        }
    };

    // Resolve my pubkey (awaits before any MLS engine is created)
    let signer = client.signer().await.map_err(|e| e.to_string())?;
    let my_pubkey = signer.get_public_key().await.map_err(|e| e.to_string())?;
    let owner_pubkey_b32 = my_pubkey.to_bech32().map_err(|e| e.to_string())?;

    // Load existing keypackage index and verify it exists on relay before returning cached
    let cached_kp_ref: Option<String> = {
        let index = db_migration::load_mls_keypackages(&handle).await.unwrap_or_default();

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
                        return Ok(serde_json::json!({
                            "device_id": device_id,
                            "owner_pubkey": owner_pubkey_b32,
                            "keypackage_ref": ref_id,
                            "cached": true
                        }));
                    }
                    // Fall through to create new KeyPackage
                }
                _ => {
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

    // Upsert into mls_keypackage_index
    {
        let mut index = db_migration::load_mls_keypackages(&handle).await.unwrap_or_default();
        let now = Timestamp::now().as_u64();
        index.push(serde_json::json!({
            "owner_pubkey": owner_pubkey_b32,
            "device_id": device_id,
            "keypackage_ref": kp_event.id.to_hex(),
            "fetched_at": now,
            "expires_at": 0u64
        }));
        let _ = db_migration::save_mls_keypackages(handle.clone(), &index).await;
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

    // Append new metadata entry
    let new_group = mls::MlsGroupMetadata {
        group_id: group_id.clone(),
        engine_group_id: String::new(),
        creator_pubkey: creator_pubkey_b32,
        name,
        avatar_ref: None,
        created_at: now_secs,
        updated_at: now_secs,
        evicted: false,
    };

    // Save only the new group to SQL/store (more efficient)
    let _ = db_migration::save_mls_group(handle.clone(), &new_group).await;

    Ok(group_id)
}

#[tauri::command]
async fn mls_send_group_message_simple(group_id: String, text: String) -> Result<String, String> {
    // Minimal: validate group exists via JSON store, then return placeholder message_id
    let handle = TAURI_APP.get().ok_or("App handle not initialized")?.clone();

    // Load groups
    let groups = db_migration::load_mls_groups(&handle).await.unwrap_or_default();

    // Validate group exists
    let exists = groups.iter().any(|g| g.group_id == group_id);
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
                let group_ids: Vec<String> = match db_migration::load_mls_groups(&handle).await {
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
    tokio::task::spawn_blocking(move || {
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
                    crate::db_migration::save_mls_group(handle.clone(), &groups[idx]).await.map_err(|e| e.to_string())?;
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
                
                crate::db_migration::save_mls_group(handle.clone(), &metadata).await.map_err(|e| e.to_string())?;
                
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
    let handle = TAURI_APP.get().ok_or("App handle not initialized")?.clone();
    match db_migration::load_mls_groups(&handle).await {
        Ok(groups) => {
            let ids = groups.into_iter()
                .map(|g| g.group_id)
                .collect();
            Ok(ids)
        }
        Err(e) => Err(format!("Failed to load MLS groups: {}", e)),
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
            // Load groups from SQL/store
            let groups = db_migration::load_mls_groups(&handle).await.unwrap_or_default();

            // Try to open persistent MLS engine for computing member counts
            // This block must avoid awaits while engine is in scope
            let maybe_engine = match MlsService::new_persistent(&handle) {
                Ok(svc) => svc.engine().ok(),
                Err(_) => None,
            };

            // Needed to decode engine group ids
            use mdk_core::prelude::GroupId;

            // Build enriched infos with member_count computed from engine (fallback to 0 on any failure)
            let infos = groups
                .into_iter()
                .filter_map(|g| {
                    // Skip evicted groups
                    if g.evicted {
                        return None;
                    }

                    // Prefer engine_group_id for engine lookup; fallback to wire group_id
                    let engine_id_hex = if !g.engine_group_id.is_empty() {
                        g.engine_group_id.clone()
                    } else {
                        g.group_id.clone()
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

                    Some(MlsGroupInfo {
                        group_id: g.group_id,
                        engine_group_id: g.engine_group_id,
                        name: g.name,
                        avatar_ref: g.avatar_ref,
                        created_at: g.created_at,
                        updated_at: g.updated_at,
                        member_count,
                    })
                })
                .collect::<Vec<MlsGroupInfo>>();

            Ok(infos)
        })
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))?
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
            if let Err(e) = db_migration::save_chat(handle.clone(), &chat_clone).await {
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

    // Load existing index
    let mut index = db_migration::load_mls_keypackages(&handle).await.unwrap_or_default();

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
    let _ = db_migration::save_mls_keypackages(handle.clone(), &index).await;

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

            // Auto-select account on startup if one exists but isn't selected
            {
                let handle_clone = handle.clone();
                if let Ok(Some(_npub)) = account_manager::auto_select_account(&handle_clone) {
                    // Clean up old Store files on 2nd boot (SQL database exists = migration complete)
                    if let Err(e) = db_sql_migration::cleanup_old_store_files(&handle_clone) {
                        eprintln!("[Cleanup] Failed to remove old Store files: {}", e);
                    }
                }
            }

            // Startup log: persistent MLS device_id if present
            {
                let handle_clone = handle.clone();
                tauri::async_runtime::spawn(async move {
                    if let Ok(Some(id)) = db_migration::load_mls_device_id(&handle_clone).await {
                        println!("[MLS] Found persistent mls_device_id at startup: {}", id);
                    }
                });
            }

            // Set as our accessible static app handle
            TAURI_APP.set(handle).unwrap();
            
            // Start the profile sync background processor
            tauri::async_runtime::spawn(async {
                profile_sync::start_profile_sync_processor().await;
            });
            
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
            message::react_to_message,
            message::fetch_msg_metadata,
            fetch_messages,
            perform_database_migration,
            deep_rescan,
            is_scanning,
            get_chat_messages,
            generate_blurhash_preview,
            download_attachment,
            login,
            notifs,
            get_relays,
            get_media_servers,
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
            check_fawkes_badge,
            get_storage_info,
            clear_storage,
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
            list_pending_mls_welcomes,
            accept_mls_welcome,
            // MLS advanced helpers
            process_mls_event,
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
            // Account manager commands
            account_manager::get_current_account,
            account_manager::list_all_accounts,
            account_manager::check_any_account_exists,
            account_manager::switch_account,
            #[cfg(all(not(target_os = "android"), feature = "whisper"))]
            whisper::delete_whisper_model,
            #[cfg(all(not(target_os = "android"), feature = "whisper"))]
            whisper::list_models
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
