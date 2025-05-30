use std::borrow::Cow;
use lazy_static::lazy_static;
use nostr_sdk::prelude::*;
use once_cell::sync::OnceCell;
use tokio::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager, Runtime, WebviewWindowBuilder, WebviewUrl};
use tauri_plugin_notification::NotificationExt;

mod crypto;

mod db;
use db::SlimProfile;

mod voice;
use voice::AudioRecorder;

mod net;

mod upload;

mod util;
use util::{get_file_type_description, calculate_file_hash, is_nonce_filename, migrate_nonce_file_to_hash};

mod message;
pub use message::{Message, Attachment, Reaction};

mod profile;
pub use profile::{Profile, Status};

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
// TODO: REMOVE AFTER SEVERAL UPDATES - This static is only needed for the one-time migration from nonce-based to hash-based storage
static PENDING_MIGRATION: OnceCell<std::collections::HashMap<String, (String, String)>> = OnceCell::new();




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

            // Check if we have pending migrations to apply to the database
            let has_pending_migrations = PENDING_MIGRATION.get().map(|m| !m.is_empty()).unwrap_or(false);
            
            if has_pending_migrations {
                // Drop state before migration
                drop(state);
                
                let migration_map = PENDING_MIGRATION.get().unwrap();
                println!("Applying pending database migrations for {} files", migration_map.len());
                
                // Emit migration start event to frontend
                handle.emit("progress_operation", serde_json::json!({
                    "type": "start",
                    "message": "Migrating DB"
                })).unwrap();
                
                // Apply the database migrations synchronously
                match update_database_attachment_paths(&handle, migration_map).await {
                    Ok(_) => (/* Yay! */),
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
                let messages = db::get_all_messages(&handle).await.unwrap();
                
                // Clear existing messages and reload with updated paths
                for profile in &mut state.profiles {
                    profile.messages.clear();
                }
                
                // Add each message to the state with updated paths
                for (msg, npub) in messages {
                    state.add_message(&npub, msg);
                }
                
                // Send the state to our frontend to signal finalised init with a full state
                handle.emit("init_finished", &state.profiles).unwrap();
            } else {
                // Check that our filesystem hasn't changed since the app was last opened
                // i.e: if an attachment file was deleted, we should mark it's attachment with "downloaded = false"
                check_attachment_filesystem_integrity(&handle, &mut state).await;

                // Send the state to our frontend to signal finalised init with a full state
                handle.emit("init_finished", &state.profiles).unwrap();
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

/// Checks if downloaded attachments still exist on the filesystem
/// Sets downloaded=false for any missing files and updates the database
async fn check_attachment_filesystem_integrity<R: Runtime>(
    handle: &AppHandle<R>,
    state: &mut ChatState,
) {
    let mut total_checked = 0;
    let mut updates_needed = Vec::new();
    
    // Capture the starting timestamp
    let start_time = std::time::Instant::now();
    
    // First pass: count total attachments to check
    let mut total_attachments = 0;
    for profile in &state.profiles {
        for message in &profile.messages {
            for attachment in &message.attachments {
                if attachment.downloaded {
                    total_attachments += 1;
                }
            }
        }
    }
    
    // Iterate through all profiles and their messages
    for (profile_idx, profile) in state.profiles.iter_mut().enumerate() {
        for (message_idx, message) in profile.messages.iter_mut().enumerate() {
            let mut message_updated = false;
            
            for attachment in message.attachments.iter_mut() {
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
                        // File is missing, mark as not downloaded
                        attachment.downloaded = false;
                        message_updated = true;
                    }
                }
            }
            
            // Mark this message for database update if any attachment was updated
            if message_updated {
                updates_needed.push((profile_idx, message_idx));
            }
        }
    }
    
    // Update database for any messages with missing attachments
    if !updates_needed.is_empty() {
        // Only emit progress if process has taken >1 second
        if start_time.elapsed().as_secs() >= 1 {
            handle.emit("progress_operation", serde_json::json!({
                "type": "progress",
                "total": updates_needed.len(),
                "current": 0,
                "message": "Updating database..."
            })).unwrap();
        }
        
        for (i, (profile_idx, message_idx)) in updates_needed.iter().enumerate() {
            let message = state.profiles[*profile_idx].messages[*message_idx].clone();
            let contact_id = state.profiles[*profile_idx].id.clone();
            
            if let Err(e) = db::save_message(handle.clone(), message, contact_id).await {
                eprintln!("Failed to update message after filesystem check: {}", e);
            }
            
            // Emit progress for database updates, but only if process has taken >1 second
            if ((i + 1) % 5 == 0 || i + 1 == updates_needed.len()) && start_time.elapsed().as_secs() >= 1 {
                handle.emit("progress_operation", serde_json::json!({
                    "type": "progress",
                    "current": i + 1,
                    "total": updates_needed.len(),
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
    // Get all messages from database
    let messages = db::get_all_messages(handle).await?;
    let mut updated_count = 0;
    let total_messages = messages.len();
    let mut processed_messages = 0;
    
    // Update attachment paths in messages
    for (mut message, contact_id) in messages {
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
        
        // Save the message back if it was updated
        if updated {
            db::save_message(handle.clone(), message, contact_id).await?;
        }
        
        // Emit progress
        processed_messages += 1;
        if processed_messages % 10 == 0 || processed_messages == total_messages {
            handle.emit("progress_operation", serde_json::json!({
                "type": "progress",
                "current": processed_messages,
                "total": total_messages,
                "message": "Migrating DB"
            })).unwrap();
        }
    }
    
    if updated_count > 0 {
        println!("Successfully updated {} attachment references in database", updated_count);
    }
    
    Ok(updated_count)
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

                // Extract the original file hash (ox tag) if present
                let original_file_hash = rumor.tags.find(TagKind::Custom(Cow::Borrowed("ox"))).map(|tag| tag.content().unwrap_or(""));

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
                        for profile in &state.profiles {
                            for message in &profile.messages {
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
    match result {
        Err(error) => {
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
                let mut_profile = state.get_profile_mut(&npub).unwrap();

                // Store all necessary IDs first
                let profile_id = mut_profile.id.clone();
                let msg_id_clone = msg_id.clone();
                let old_attachment_id = attachment_id.clone();

                // Update the attachment status, ID (from nonce to hash), and path
                let mut_msg = mut_profile.get_message_mut(&msg_id).unwrap();
                
                // Find the attachment by the old ID (nonce) and update it
                if let Some(attachment_index) = mut_msg.attachments.iter().position(|a| a.id == old_attachment_id) {
                    let mut_attachment = &mut mut_msg.attachments[attachment_index];
                    mut_attachment.id = file_hash.clone(); // Update ID from nonce to hash
                    mut_attachment.downloading = false;
                    mut_attachment.downloaded = true;
                    mut_attachment.path = hash_file_path.to_string_lossy().to_string(); // Update to hash-based path
                }

                // Emit the finished download with both old and new IDs
                handle.emit("attachment_download_result", serde_json::json!({
                    "profile_id": profile_id,
                    "msg_id": msg_id_clone,
                    "old_id": old_attachment_id,
                    "id": file_hash,
                    "success": true,
                })).unwrap();

                // Save to the DB with updated ID and path
                db::save_message(handle.clone(), mut_msg.clone(), npub).await.unwrap();
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

    res
}

// Tauri command that uses the crypto module
#[tauri::command]
async fn decrypt(ciphertext: String, password: Option<String>) -> Result<String, ()> {
    crypto::internal_decrypt(ciphertext, password).await
}

#[tauri::command]
async fn start_recording() -> Result<(), String> {
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

            // Check if we need to migrate files
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
            } else {
                // No migration needed, run it silently just in case
                tauri::async_runtime::block_on(async {
                    match migrate_nonce_files_to_hash(&handle).await {
                        Ok(migration_map) => {
                            // Store the migration map for database update after login
                            if !migration_map.is_empty() {
                                let _ = PENDING_MIGRATION.set(migration_map);
                            }
                        }
                        Err(e) => eprintln!("Failed to migrate attachment files: {}", e),
                    }
                });
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
            db::get_pkey,
            db::set_pkey,
            db::get_seed,
            db::set_seed,
            db::remove_setting,
            profile::load_profile,
            profile::update_profile,
            profile::update_status,
            profile::upload_avatar,
            profile::mark_as_read,
            message::message,
            message::paste_message,
            message::voice_message,
            message::file_message,
            message::react,
            message::fetch_msg_metadata,
            fetch_messages,
            warmup_nip96_servers,
            download_attachment,
            login,
            notifs,
            start_typing,
            connect,
            encrypt,
            decrypt,
            start_recording,
            stop_recording,
            update_unread_counter,
            logout,
            create_account
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
