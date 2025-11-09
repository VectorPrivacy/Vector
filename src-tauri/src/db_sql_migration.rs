/// Migration from Tauri Store (JSON) to SQL database
///
/// This module handles the one-time migration from the old vector.json store
/// to the new SQL-based storage system with per-account databases.

use tauri::{AppHandle, Runtime, Manager, Emitter};
use serde_json::Value;
use nostr_sdk::prelude::*;

/// Check if Store-to-SQL migration is needed
///
/// Returns true if:
/// 1. vector.json exists in app data directory
/// 2. vector.json contains a valid pkey (indicating a real account)
/// 3. No npub* profile directories with vector.db exist (migration hasn't been done yet)
pub async fn is_sql_migration_needed<R: Runtime>(handle: &AppHandle<R>) -> Result<bool, String> {
    // Check if vector.json exists
    let app_data = handle.path().app_local_data_dir()
        .map_err(|e| format!("Failed to get app data dir: {}", e))?;
    
    let vector_json_path = app_data.join("vector.json");
    
    if !vector_json_path.exists() {
        // No old store file, no migration needed
        return Ok(false);
    }
    
    // Check if vector.json has a valid pkey (indicates a real account)
    if let Ok(json_content) = std::fs::read_to_string(&vector_json_path) {
        if let Ok(store_data) = serde_json::from_str::<Value>(&json_content) {
            let has_pkey = store_data.get("pkey")
                .and_then(|v| v.as_str())
                .map(|s| !s.is_empty())
                .unwrap_or(false);
            
            if !has_pkey {
                println!("[Migration Check] vector.json exists but no pkey - not a valid account, skipping migration");
                return Ok(false);
            }
        }
    }
    
    // Check if any npub* directories exist with vector.db files
    // Profile directories are created directly in app_data (e.g., npub1abc.../vector.db)
    if let Ok(entries) = std::fs::read_dir(&app_data) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let dir_name = path.file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("");
                
                // Check if this is an npub directory with a database
                if dir_name.starts_with("npub1") {
                    let db_path = path.join("vector.db");
                    if db_path.exists() {
                        println!("[Migration Check] Found existing profile database at {}, migration not needed",
                            db_path.display());
                        return Ok(false);
                    }
                }
            }
        }
    }
    
    // vector.json exists with valid pkey but no profile databases found - migration needed
    println!("[Migration Check] vector.json exists with valid pkey but no profile databases - migration needed");
    Ok(true)
}

/// Perform the Store-to-SQL migration
///
/// This function:
/// 1. Gets the current user's npub from the Nostr client
/// 2. Reads vector.json
/// 3. Creates a profile-specific directory
/// 4. Initializes the SQL database
/// 5. Migrates all data (no re-encryption needed - data stays encrypted as-is)
/// 6. Migrates MLS directory
///
/// Note: Old Store files (vector.json, mls/) are kept during migration for safety.
/// They will be cleaned up on the next boot after successful migration.
pub async fn migrate_store_to_sql<R: Runtime>(
    handle: AppHandle<R>
) -> Result<(), String> {
    const TOTAL_STEPS: u32 = 9;
    
    // Emit progress: Starting migration
    handle.emit("progress_operation", serde_json::json!({
        "type": "progress",
        "current": 0,
        "total": TOTAL_STEPS,
        "message": "Upgrading database..."
    })).map_err(|e| format!("Failed to emit progress: {}", e))?;
    
    // Step 1: Get npub from Nostr client (already logged in)
    handle.emit("progress_operation", serde_json::json!({
        "type": "progress",
        "current": 1,
        "total": TOTAL_STEPS,
        "message": "Getting account information..."
    })).map_err(|e| format!("Failed to emit progress: {}", e))?;
    
    let client = crate::NOSTR_CLIENT.get()
        .ok_or("Nostr client not initialized")?;
    let signer = client.signer().await
        .map_err(|e| format!("Failed to get signer: {}", e))?;
    let public_key = signer.get_public_key().await
        .map_err(|e| format!("Failed to get public key: {}", e))?;
    let npub = public_key.to_bech32()
        .map_err(|e| format!("Failed to convert public key to bech32: {}", e))?;
    
    // Step 2: Read vector.json
    handle.emit("progress_operation", serde_json::json!({
        "type": "progress",
        "current": 2,
        "total": TOTAL_STEPS,
        "message": "Reading existing data..."
    })).map_err(|e| format!("Failed to emit progress: {}", e))?;
    
    let app_data = handle.path().app_local_data_dir()
        .map_err(|e| format!("Failed to get app data dir: {}", e))?;
    
    let vector_json_path = app_data.join("vector.json");
    let json_content = std::fs::read_to_string(&vector_json_path)
        .map_err(|e| format!("Failed to read vector.json: {}", e))?;
    
    let store_data: Value = serde_json::from_str(&json_content)
        .map_err(|e| format!("Failed to parse vector.json: {}", e))?;
    
    // Step 3: Create profile directory
    handle.emit("progress_operation", serde_json::json!({
        "type": "progress",
        "current": 3,
        "total": TOTAL_STEPS,
        "message": "Creating profile directory..."
    })).map_err(|e| format!("Failed to emit progress: {}", e))?;
    
    let profile_dir = crate::account_manager::get_profile_directory(&handle, &npub)?;
    std::fs::create_dir_all(&profile_dir)
        .map_err(|e| format!("Failed to create profile directory: {}", e))?;
    
    // Step 4: Initialize SQL database
    handle.emit("progress_operation", serde_json::json!({
        "type": "progress",
        "current": 4,
        "total": TOTAL_STEPS,
        "message": "Initializing new database..."
    })).map_err(|e| format!("Failed to emit progress: {}", e))?;
    
    crate::account_manager::init_profile_database(&handle, &npub).await?;
    
    // Set the current account so migration functions can get DB connections
    crate::account_manager::set_current_account(npub.clone())?;
    
    // Step 5: Migrate profiles
    handle.emit("progress_operation", serde_json::json!({
        "type": "progress",
        "current": 5,
        "total": TOTAL_STEPS,
        "message": "Migrating profiles..."
    })).map_err(|e| format!("Failed to emit progress: {}", e))?;
    
    let npub_id_map = migrate_profiles(&handle, &store_data).await?;
    
    // Step 6: Migrate chats
    handle.emit("progress_operation", serde_json::json!({
        "type": "progress",
        "current": 6,
        "total": TOTAL_STEPS,
        "message": "Migrating chats..."
    })).map_err(|e| format!("Failed to emit progress: {}", e))?;
    
    let chat_id_map = migrate_chats(&handle, &store_data).await?;
    
    // Step 7: Migrate messages
    handle.emit("progress_operation", serde_json::json!({
        "type": "progress",
        "current": 7,
        "total": TOTAL_STEPS,
        "message": "Migrating messages..."
    })).map_err(|e| format!("Failed to emit progress: {}", e))?;
    
    migrate_messages(&handle, &store_data, &chat_id_map, &npub_id_map).await?;
    
    // Step 8: Migrate settings
    handle.emit("progress_operation", serde_json::json!({
        "type": "progress",
        "current": 8,
        "total": TOTAL_STEPS,
        "message": "Migrating settings..."
    })).map_err(|e| format!("Failed to emit progress: {}", e))?;
    
    migrate_settings(&handle, &store_data).await?;
    
    // Step 9: Migrate MLS data
    handle.emit("progress_operation", serde_json::json!({
        "type": "progress",
        "current": 9,
        "total": TOTAL_STEPS,
        "message": "Migrating MLS data..."
    })).map_err(|e| format!("Failed to emit progress: {}", e))?;
    
    // Migrate MLS groups to database
    migrate_mls_groups(&handle, &store_data).await?;
    
    // Copy MLS directory
    let old_mls_dir = app_data.join("mls");
    if old_mls_dir.exists() {
        let new_mls_dir = crate::account_manager::get_mls_directory(&handle, &npub)?;
        crate::account_manager::copy_dir_recursive(&old_mls_dir, &new_mls_dir)?;
        println!("[Migration] Copied MLS directory to {}", new_mls_dir.display());
    }
    
    // Preload ID caches for immediate use
    if let Err(e) = crate::db_migration::preload_id_caches(&handle).await {
        eprintln!("[Migration] Failed to preload ID caches: {}", e);
    }
    
    // Complete
    handle.emit("progress_operation", serde_json::json!({
        "type": "progress",
        "current": TOTAL_STEPS,
        "total": TOTAL_STEPS,
        "message": "Migration complete!"
    })).map_err(|e| format!("Failed to emit progress: {}", e))?;
    
    println!("[Migration] Migration complete! Current account set to: {}", npub);
    
    Ok(())
}

/// Derive npub (public key in bech32 format) from nsec (private key)
#[allow(dead_code)]
fn derive_npub_from_nsec(nsec: &str) -> Result<String, String> {
    use nostr_sdk::prelude::*;
    
    let keys = Keys::parse(nsec)
        .map_err(|e| format!("Failed to parse private key: {}", e))?;
    
    Ok(keys.public_key().to_bech32()
        .map_err(|e| format!("Failed to convert public key to bech32: {}", e))?)
}

/// Migrate profiles from Store to SQL (plaintext - no encryption)
async fn migrate_profiles<R: Runtime>(
    handle: &AppHandle<R>,
    store_data: &Value
) -> Result<std::collections::HashMap<String, i64>, String> {
    // Get encrypted profiles from store (optional - may not exist for new accounts)
    let encrypted_profiles = match store_data.get("profiles").and_then(|v| v.as_str()) {
        Some(enc) => enc,
        None => {
            println!("[Migration] No profiles found in store - skipping profile migration (new account)");
            return Ok(std::collections::HashMap::new());
        }
    };
    
    // Decrypt profiles (password is None - uses default encryption)
    let profiles_json = match crate::crypto::internal_decrypt(encrypted_profiles.to_string(), None).await {
        Ok(json) => json,
        Err(_) => {
            println!("[Migration] Failed to decrypt profiles - may be empty or corrupted, skipping");
            return Ok(std::collections::HashMap::new());
        }
    };
    
    let profiles: Vec<crate::db::SlimProfile> = match serde_json::from_str(&profiles_json) {
        Ok(p) => p,
        Err(e) => {
            println!("[Migration] Failed to parse profiles: {} - skipping", e);
            return Ok(std::collections::HashMap::new());
        }
    };
    
    if profiles.is_empty() {
        println!("[Migration] No profiles to migrate");
        return Ok(std::collections::HashMap::new());
    }
    
    // Get database connection from pool
    let conn = crate::account_manager::get_db_connection(handle)?;
    
    // Insert profiles (plaintext - no encryption needed) and build npub→ID mapping
    let mut npub_id_map = std::collections::HashMap::new();
    
    for slim_profile in profiles {
        // Convert to full profile to access all fields
        let profile = slim_profile.to_profile();
        
        conn.execute(
            "INSERT OR REPLACE INTO profiles (npub, name, display_name, nickname, lud06, lud16, banner, avatar, about, website, nip05, status_content, status_url, muted, bot)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            rusqlite::params![
                profile.id,  // This is the npub
                profile.name,
                profile.display_name,
                profile.nickname,
                profile.lud06,
                profile.lud16,
                profile.banner,
                profile.avatar,
                profile.about,
                profile.website,
                profile.nip05,
                profile.status.title,
                profile.status.url,
                profile.muted as i32,
                profile.bot as i32,
            ],
        ).map_err(|e| format!("Failed to insert profile {}: {}", profile.id, e))?;
        
        // Get the auto-generated integer ID
        let int_id = conn.last_insert_rowid();
        npub_id_map.insert(profile.id.clone(), int_id);
    }
    
    println!("[Migration] Migrated {} profiles, built npub→ID map", npub_id_map.len());
    
    crate::account_manager::return_db_connection(conn);
    Ok(npub_id_map)
}

/// Clean up old Store files after successful migration
///
/// This function should be called on the 2nd boot (when SQL database exists).
/// It removes:
/// - vector.json (old Store database)
/// - mls/ directory (old MLS data, now in npub*/mls/)
pub fn cleanup_old_store_files<R: Runtime>(handle: &AppHandle<R>) -> Result<(), String> {
    let app_data = handle.path().app_local_data_dir()
        .map_err(|e| format!("Failed to get app data dir: {}", e))?;
    
    let mut cleaned_files = Vec::new();
    
    // Remove vector.json
    let vector_json = app_data.join("vector.json");
    if vector_json.exists() {
        std::fs::remove_file(&vector_json)
            .map_err(|e| format!("Failed to remove vector.json: {}", e))?;
        cleaned_files.push("vector.json");
    }
    
    // Remove old mls/ directory
    let old_mls_dir = app_data.join("mls");
    if old_mls_dir.exists() {
        std::fs::remove_dir_all(&old_mls_dir)
            .map_err(|e| format!("Failed to remove mls directory: {}", e))?;
        cleaned_files.push("mls/");
    }
    
    if !cleaned_files.is_empty() {
        println!("[Cleanup] Removed old Store files: {}", cleaned_files.join(", "));
    }
    
    Ok(())
}

/// Perform database maintenance (VACUUM + ANALYZE)
/// Should be called periodically (e.g., once a week) to optimize database performance
pub fn vacuum_database<R: Runtime>(handle: &AppHandle<R>) -> Result<(), String> {
    let conn = crate::account_manager::get_db_connection(handle)?;
    
    println!("[Maintenance] Running VACUUM to compact database...");
    conn.execute("VACUUM", [])
        .map_err(|e| format!("VACUUM failed: {}", e))?;
    
    println!("[Maintenance] Running ANALYZE to update query statistics...");
    conn.execute("ANALYZE", [])
        .map_err(|e| format!("ANALYZE failed: {}", e))?;
    
    // Update last_vacuum timestamp
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    
    conn.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES ('last_vacuum', ?1)",
        rusqlite::params![now.to_string()],
    ).map_err(|e| format!("Failed to update last_vacuum timestamp: {}", e))?;
    
    println!("[Maintenance] Database maintenance complete");
    crate::account_manager::return_db_connection(conn);
    Ok(())
}

/// Check if weekly VACUUM is needed and perform it if so
/// Should be called after init_finished when app is idle
pub async fn check_and_vacuum_if_needed<R: Runtime>(handle: &AppHandle<R>) -> Result<(), String> {    
    let conn = crate::account_manager::get_db_connection(handle)?;
    
    // Get last_vacuum timestamp
    let last_vacuum: Option<i64> = conn.query_row(
        "SELECT value FROM settings WHERE key = 'last_vacuum'",
        [],
        |row| {
            let value: String = row.get(0)?;
            Ok(value.parse::<i64>().unwrap_or(0))
        }
    ).ok();
    
    crate::account_manager::return_db_connection(conn);
    
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    
    const WEEK_IN_SECONDS: i64 = 7 * 24 * 60 * 60;
    
    let should_vacuum = match last_vacuum {
        Some(last) => (now - last) > WEEK_IN_SECONDS,
        None => true, // Never vacuumed, do it now
    };
    
    if should_vacuum {
        println!("[Maintenance] Weekly VACUUM needed (last: {:?})", last_vacuum);
        
        // Run in background to avoid blocking
        let handle_clone = handle.clone();
        tauri::async_runtime::spawn(async move {
            if let Err(e) = vacuum_database(&handle_clone) {
                eprintln!("[Maintenance] Weekly VACUUM failed: {}", e);
            }
        });
    } else {
        println!("[Maintenance] VACUUM not needed yet (last: {} days ago)",
            (now - last_vacuum.unwrap_or(now)) / 86400);
    }
    
    Ok(())
}

/// Migrate MLS groups from Store to SQL (decrypt and store in mls_groups table)
async fn migrate_mls_groups<R: Runtime>(
    handle: &AppHandle<R>,
    store_data: &Value
) -> Result<(), String> {
    // Check if mls_groups exists in Store
    let encrypted_groups = match store_data.get("mls_groups").and_then(|v| v.as_str()) {
        Some(enc) => enc,
        None => {
            println!("[Migration] No MLS groups found in Store");
            return Ok(());
        }
    };
    
    // Decrypt the groups blob
    let groups_json = crate::crypto::internal_decrypt(encrypted_groups.to_string(), None)
        .await
        .map_err(|_| "Failed to decrypt MLS groups".to_string())?;
    
    let groups: Vec<crate::mls::MlsGroupMetadata> = serde_json::from_str(&groups_json)
        .map_err(|e| format!("Failed to parse MLS groups: {}", e))?;
    
    if groups.is_empty() {
        println!("[Migration] No MLS groups to migrate");
        return Ok(());
    }
    
    // Get database connection from pool
    let conn = crate::account_manager::get_db_connection(handle)?;
    
    // Insert each group into mls_groups table (all fields as columns, no encryption)
    for group in &groups {
        conn.execute(
            "INSERT OR REPLACE INTO mls_groups (group_id, engine_group_id, creator_pubkey, name, avatar_ref, created_at, updated_at, evicted)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                group.group_id,
                group.engine_group_id,
                group.creator_pubkey,
                group.name,
                group.avatar_ref,
                group.created_at as i64,
                group.updated_at as i64,
                group.evicted as i32,
            ],
        ).map_err(|e| format!("Failed to insert MLS group {}: {}", group.group_id, e))?;
    }
    
    println!("[Migration] Migrated {} MLS groups to mls_groups table", groups.len());
    
    crate::account_manager::return_db_connection(conn);
    Ok(())
}

/// Migrate chats from Store to SQL (plaintext metadata)
async fn migrate_chats<R: Runtime>(
    handle: &AppHandle<R>,
    store_data: &Value,
) -> Result<std::collections::HashMap<String, i64>, String> {
    // Get encrypted chats from store (DM chats only) - optional for new accounts
    let mut chats: Vec<crate::db_migration::SlimChatDB> = match store_data.get("chats").and_then(|v| v.as_str()) {
        Some(encrypted_chats) => {
            // Decrypt chats (password is None - uses default encryption)
            match crate::crypto::internal_decrypt(encrypted_chats.to_string(), None).await {
                Ok(chats_json) => {
                    match serde_json::from_str(&chats_json) {
                        Ok(c) => c,
                        Err(e) => {
                            println!("[Migration] Failed to parse chats: {} - skipping", e);
                            Vec::new()
                        }
                    }
                },
                Err(_) => {
                    println!("[Migration] Failed to decrypt chats - skipping");
                    Vec::new()
                }
            }
        },
        None => {
            println!("[Migration] No chats found in store - skipping chat migration (new account)");
            Vec::new()
        }
    };
    
    // Also find MLS group chats from chat_messages_* keys
    // Group chats are NOT in the "chats" key, but we can infer them from message keys
    if let Some(obj) = store_data.as_object() {
        for key in obj.keys() {
            if key.starts_with("chat_messages_") {
                let chat_id = key.strip_prefix("chat_messages_").unwrap();
                
                // If it's not an npub (DM), it's a group chat
                if !chat_id.starts_with("npub1") {
                    // Check if we already have this chat
                    if !chats.iter().any(|c| c.id == chat_id) {
                        // Create a SlimChatDB for this group
                        let group_chat = crate::db_migration::SlimChatDB {
                            id: chat_id.to_string(),
                            chat_type: crate::ChatType::MlsGroup,
                            participants: vec![], // Will be synced later from MLS engine
                            last_read: String::new(),
                            created_at: 0, // Will be updated from first message timestamp
                            metadata: crate::chat::ChatMetadata::default(),
                            muted: false,
                        };
                        chats.push(group_chat);
                        println!("[Migration] Created MLS group chat entry for: {}", chat_id);
                    }
                }
            }
        }
    }
    
    // Get database connection from pool
    let conn = crate::account_manager::get_db_connection(handle)?;
    
    // Insert chats (plaintext metadata) and build identifier→ID mapping
    let chat_count = chats.len();
    let mut chat_id_map = std::collections::HashMap::new();
    
    for chat in chats {
        let chat_type_int = chat.chat_type.to_i32();
        let participants_json = serde_json::to_string(&chat.participants)
            .unwrap_or_else(|_| "[]".to_string());
        let metadata_json = serde_json::to_string(&chat.metadata)
            .unwrap_or_else(|_| "{}".to_string());
        
        conn.execute(
            "INSERT OR REPLACE INTO chats (chat_identifier, chat_type, participants, last_read, created_at, metadata, muted)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                &chat.id,
                chat_type_int,
                participants_json,
                chat.last_read,
                chat.created_at as i64,
                metadata_json,
                chat.muted as i32,
            ],
        ).map_err(|e| format!("Failed to insert chat {}: {}", chat.id, e))?;
        
        // Get the auto-generated integer ID
        let int_id = conn.last_insert_rowid();
        chat_id_map.insert(chat.id.clone(), int_id);
    }
    
    println!("[Migration] Migrated {} chats total", chat_count);
    
    crate::account_manager::return_db_connection(conn);
    Ok(chat_id_map)
}

/// Migrate messages from Store to SQL (keep content encrypted as-is)
async fn migrate_messages<R: Runtime>(
    handle: &AppHandle<R>,
    store_data: &Value,
    chat_id_map: &std::collections::HashMap<String, i64>,
    npub_id_map: &std::collections::HashMap<String, i64>
) -> Result<(), String> {
    // In the old Store format, messages are stored with keys like:
    // "chat_messages_npub1..." for DMs
    // "chat_messages_<group_id>" for group chats
    
    // Find all keys that start with "chat_messages_"
    let mut all_messages = Vec::new();
    let mut total_message_count = 0;
    
    if let Some(obj) = store_data.as_object() {
        for (key, value) in obj.iter() {
            if key.starts_with("chat_messages_") {
                // Extract chat_id from the key (everything after "chat_messages_")
                let chat_id = key.strip_prefix("chat_messages_").unwrap().to_string();
                
                // Parse the messages HashMap for this chat
                let messages_map: std::collections::HashMap<String, String> =
                    serde_json::from_value(value.clone())
                        .map_err(|e| format!("Failed to deserialize messages for chat {}: {}", chat_id, e))?;
                
                println!("[Migration] Found {} messages for chat {}", messages_map.len(), chat_id);
                total_message_count += messages_map.len();
                
                // Store chat_id with each message
                for (msg_id, encrypted) in messages_map {
                    all_messages.push((chat_id.clone(), msg_id, encrypted));
                }
            }
        }
    }
    
    if all_messages.is_empty() {
        println!("[Migration] No messages found in store");
        return Ok(());
    }
    
    println!("[Migration] Found {} total messages across {} chats", total_message_count,
        all_messages.iter().map(|(chat_id, _, _)| chat_id).collect::<std::collections::HashSet<_>>().len());
    
    // First, decrypt and prepare all messages (do all async work first)
    let mut prepared_messages = Vec::new();
    let mut error_count = 0;
    
    for (chat_id, _msg_id, encrypted) in all_messages.iter() {
        // Decrypt the message
        match crate::crypto::internal_decrypt(encrypted.clone(), None).await {
            Ok(json) => {
                // Deserialize to SlimMessage
                match serde_json::from_str::<crate::db::SlimMessage>(&json) {
                    Ok(slim) => {
                        // Re-encrypt the content for SQL storage
                        let encrypted_content = crate::crypto::internal_encrypt(slim.content.clone(), None).await;
                        
                        // Store prepared data with the chat_id from the key
                        prepared_messages.push((chat_id.clone(), slim, encrypted_content));
                    },
                    Err(e) => {
                        eprintln!("[Migration] Failed to parse message: {}", e);
                        error_count += 1;
                    }
                }
            },
            Err(_) => {
                eprintln!("[Migration] Failed to decrypt message");
                error_count += 1;
            }
        }
    }
    
    // Now do all database operations synchronously (no awaits while holding transaction)
    let mut conn = crate::account_manager::get_db_connection(handle)?;
    
    // Use a transaction for better performance
    let tx = conn.transaction()
        .map_err(|e| format!("Failed to start transaction: {}", e))?;
    
    let mut migrated_count = 0;
    
    // Insert all prepared messages
    for (chat_identifier, slim, encrypted_content) in prepared_messages {
        // Look up the integer chat_id from the map
        let chat_id = match chat_id_map.get(&chat_identifier) {
            Some(id) => *id,
            None => {
                eprintln!("[Migration] Chat ID not found in map for identifier: {}", chat_identifier);
                error_count += 1;
                continue;
            }
        };
        
        // Look up the integer user_id from npub
        let user_id = if let Some(ref npub_str) = slim.npub {
            npub_id_map.get(npub_str).copied()
        } else {
            None
        };
        
        let attachments_json = serde_json::to_string(&slim.attachments)
            .unwrap_or_else(|_| "[]".to_string());
        let reactions_json = serde_json::to_string(&slim.reactions)
            .unwrap_or_else(|_| "[]".to_string());
        let preview_json = slim.preview_metadata.as_ref()
            .and_then(|p| serde_json::to_string(p).ok());
        
        match tx.execute(
            "INSERT OR REPLACE INTO messages (id, chat_id, content_encrypted, replied_to, preview_metadata, attachments, reactions, at, mine, user_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            rusqlite::params![
                slim.id,
                chat_id,
                encrypted_content,
                slim.replied_to,
                preview_json,
                attachments_json,
                reactions_json,
                slim.at as i64,
                slim.mine as i32,
                user_id,
            ],
        ) {
            Ok(_) => migrated_count += 1,
            Err(e) => {
                eprintln!("[Migration] Failed to insert message: {}", e);
                error_count += 1;
            }
        }
    }
    
    // Commit the transaction
    tx.commit()
        .map_err(|e| format!("Failed to commit messages transaction: {}", e))?;
    
    println!("[Migration] Migrated {} messages ({} errors)", migrated_count, error_count);
    
    crate::account_manager::return_db_connection(conn);
    Ok(())
}

/// Migrate settings from Store to SQL
async fn migrate_settings<R: Runtime>(
    handle: &AppHandle<R>,
    store_data: &Value
) -> Result<(), String> {
    // Get database connection from pool
    let conn = crate::account_manager::get_db_connection(handle)?;
    
    // Migrate each setting
    for (key, value) in store_data.as_object().ok_or("Store data is not an object")? {
        // Skip non-setting keys (data that belongs in other tables or is deprecated)
        if key == "profiles"
            || key == "chats"
            || key == "dbver"                     // Database version (now managed via PRAGMA user_version)
            || key.starts_with("chat_messages_")  // Message cache keys - should NOT be in settings
            || key.starts_with("messages_")       // Old message format
            || key == "mls_groups"                // MLS group state blob (managed by MLS library via filesystem)
            || key == "mls_event_cursors"         // MLS event cursors (now in mls_event_cursors table)
            || key == "mls_keypackage_index"      // Deprecated - keypackages now managed in mls_keypackages table
        {
            println!("[Migration] Skipping non-setting key: {}", key);
            continue;
        }
        
        // Note: mls_device_id IS a legitimate setting and will be migrated
        println!("[Migration] Migrating setting: {}", key);
        
        // Convert value to string, handling different JSON types
        let value_str = if value.is_boolean() {
            // Convert boolean to "true" or "false" string
            if value.as_bool().unwrap() { "true" } else { "false" }
        } else if value.is_string() {
            value.as_str().unwrap()
        } else {
            // For other types (numbers, etc.), convert to string
            &value.to_string()
        };
        
        println!("[Migration] Setting '{}': original type={}, value_str='{}'",
            key,
            if value.is_boolean() { "boolean" } else if value.is_string() { "string" } else { "other" },
            value_str
        );
        
        conn.execute(
            "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
            rusqlite::params![
                key,
                value_str,
            ],
        ).map_err(|e| format!("Failed to insert setting {}: {}", key, e))?;
        
        println!("[Migration] Inserted setting '{}' = '{}'", key, value_str);
    }
    
    crate::account_manager::return_db_connection(conn);
    Ok(())
}