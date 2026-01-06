use serde::{Deserialize, Serialize};
use tauri::{AppHandle, command, Runtime};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use once_cell::sync::Lazy;

use crate::{Profile, Status, Message, Chat, ChatType, Attachment};
use crate::crypto::{internal_encrypt, internal_decrypt};

/// In-memory cache for chat_identifier → integer ID mappings
/// This avoids database lookups on every message operation
static CHAT_ID_CACHE: Lazy<Arc<RwLock<HashMap<String, i64>>>> =
    Lazy::new(|| Arc::new(RwLock::new(HashMap::new())));

/// In-memory cache for npub → integer ID mappings
/// This avoids database lookups on every message operation
static USER_ID_CACHE: Lazy<Arc<RwLock<HashMap<String, i64>>>> =
    Lazy::new(|| Arc::new(RwLock::new(HashMap::new())));

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct SlimProfile {
    pub id: String,
    name: String,
    display_name: String,
    nickname: String,
    lud06: String,
    lud16: String,
    banner: String,
    avatar: String,
    about: String,
    website: String,
    nip05: String,
    status: Status,
    muted: bool,
    bot: bool,
    // Omitting: messages, last_updated, mine
}

impl Default for SlimProfile {
    fn default() -> Self {
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
            status: Status::new(),
            muted: false,
            bot: false,
        }
    }
}

impl From<&Profile> for SlimProfile {
    fn from(profile: &Profile) -> Self {
        SlimProfile {
            id: profile.id.clone(),
            name: profile.name.clone(),
            display_name: profile.display_name.clone(),
            nickname: profile.nickname.clone(),
            lud06: profile.lud06.clone(),
            lud16: profile.lud16.clone(),
            banner: profile.banner.clone(),
            avatar: profile.avatar.clone(),
            about: profile.about.clone(),
            website: profile.website.clone(),
            nip05: profile.nip05.clone(),
            status: profile.status.clone(),
            muted: profile.muted,
            bot: profile.bot,
        }
    }
}

impl SlimProfile {
    // Convert back to full Profile
    pub fn to_profile(&self) -> crate::Profile {
        crate::Profile {
            id: self.id.clone(),
            name: self.name.clone(),
            display_name: self.display_name.clone(),
            nickname: self.nickname.clone(),
            lud06: self.lud06.clone(),
            lud16: self.lud16.clone(),
            banner: self.banner.clone(),
            avatar: self.avatar.clone(),
            about: self.about.clone(),
            website: self.website.clone(),
            nip05: self.nip05.clone(),
            status: self.status.clone(),
            last_updated: 0,      // Default value
            mine: false,          // Default value
            muted: self.muted,
            bot: self.bot,
        }
    }
}

// Function to get all profiles
pub async fn get_all_profiles<R: Runtime>(handle: &AppHandle<R>) -> Result<Vec<SlimProfile>, String> {
    let conn = crate::account_manager::get_db_connection(handle)?;

    let mut stmt = conn.prepare("SELECT npub, name, display_name, nickname, lud06, lud16, banner, avatar, about, website, nip05, status_content, status_url, muted, bot FROM profiles")
        .map_err(|e| format!("Failed to prepare statement: {}", e))?;

    let profiles = stmt.query_map([], |row| {
        Ok(SlimProfile {
            id: row.get(0)?,  // npub column
            name: row.get(1)?,
            display_name: row.get(2)?,
            nickname: row.get(3)?,
            lud06: row.get(4)?,
            lud16: row.get(5)?,
            banner: row.get(6)?,
            avatar: row.get(7)?,
            about: row.get(8)?,
            website: row.get(9)?,
            nip05: row.get(10)?,
            status: crate::Status {
                title: row.get(11)?,
                purpose: String::new(), // Not stored separately
                url: row.get(12)?,
            },
            muted: row.get::<_, i32>(13)? != 0,
            bot: row.get::<_, i32>(14)? != 0,
        })
    })
    .map_err(|e| format!("Failed to query profiles: {}", e))?
    .collect::<Result<Vec<_>, _>>()
    .map_err(|e| format!("Failed to collect profiles: {}", e))?;

    drop(stmt); // Explicitly drop stmt before returning connection
    crate::account_manager::return_db_connection(conn);
    Ok(profiles)
}


// Public command to set a profile
#[command]
pub async fn set_profile<R: Runtime>(handle: AppHandle<R>, profile: Profile) -> Result<(), String> {
    let conn = crate::account_manager::get_db_connection(&handle)?;

    conn.execute(
        "INSERT INTO profiles (npub, name, display_name, nickname, lud06, lud16, banner, avatar, about, website, nip05, status_content, status_url, muted, bot)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
         ON CONFLICT(npub) DO UPDATE SET
            name = excluded.name,
            display_name = excluded.display_name,
            nickname = excluded.nickname,
            lud06 = excluded.lud06,
            lud16 = excluded.lud16,
            banner = excluded.banner,
            avatar = excluded.avatar,
            about = excluded.about,
            website = excluded.website,
            nip05 = excluded.nip05,
            status_content = excluded.status_content,
            status_url = excluded.status_url,
            muted = excluded.muted,
            bot = excluded.bot",
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
    ).map_err(|e| format!("Failed to insert profile: {}", e))?;

    crate::account_manager::return_db_connection(conn);
    Ok(())
}




#[command]
pub fn get_theme<R: Runtime>(handle: AppHandle<R>) -> Result<Option<String>, String> {
    // Try SQL if account is selected
    if let Ok(_npub) = crate::account_manager::get_current_account() {
        return get_sql_setting(handle.clone(), "theme".to_string());
    }
    Ok(None)
}

#[command]
pub async fn set_pkey<R: Runtime>(handle: AppHandle<R>, pkey: String) -> Result<(), String> {
    // Check if there's a pending account (new account creation flow)
    if let Ok(Some(npub)) = crate::account_manager::get_pending_account() {
        // Initialize database for the pending account
        crate::account_manager::init_profile_database(&handle, &npub).await?;
        crate::account_manager::set_current_account(npub.clone())?;
        crate::account_manager::clear_pending_account()?;

        // Now save the pkey to the newly created database
        let conn = crate::account_manager::get_db_connection(&handle)?;
        conn.execute(
            "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
            rusqlite::params!["pkey", pkey],
        ).map_err(|e| format!("Failed to insert pkey: {}", e))?;
        crate::account_manager::return_db_connection(conn);
        return Ok(());
    }

    let conn = crate::account_manager::get_db_connection(&handle)?;

    conn.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
        rusqlite::params!["pkey", pkey],
    ).map_err(|e| format!("Failed to insert pkey: {}", e))?;

    crate::account_manager::return_db_connection(conn);
    Ok(())
}

#[command]
pub fn get_pkey<R: Runtime>(handle: AppHandle<R>) -> Result<Option<String>, String> {
    let conn = crate::account_manager::get_db_connection(&handle)?;

    let result: Option<String> = conn.query_row(
        "SELECT value FROM settings WHERE key = ?1",
        rusqlite::params!["pkey"],
        |row| row.get(0)
    ).ok();

    crate::account_manager::return_db_connection(conn);
    Ok(result)
}

#[command]
pub async fn set_seed<R: Runtime>(handle: AppHandle<R>, seed: String) -> Result<(), String> {
    let encrypted_seed = internal_encrypt(seed, None).await;

    let conn = crate::account_manager::get_db_connection(&handle)?;

    conn.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
        rusqlite::params!["seed", encrypted_seed],
    ).map_err(|e| format!("Failed to insert seed: {}", e))?;

    crate::account_manager::return_db_connection(conn);
    Ok(())
}

#[command]
pub async fn get_seed<R: Runtime>(handle: AppHandle<R>) -> Result<Option<String>, String> {
    let conn = crate::account_manager::get_db_connection(&handle)?;

    let encrypted_seed: Option<String> = conn.query_row(
        "SELECT value FROM settings WHERE key = ?1",
        rusqlite::params!["seed"],
        |row| row.get(0)
    ).ok();

    crate::account_manager::return_db_connection(conn);

    if let Some(encrypted) = encrypted_seed {
        match internal_decrypt(encrypted, None).await {
            Ok(decrypted) => return Ok(Some(decrypted)),
            Err(_) => return Err("Failed to decrypt seed phrase".to_string()),
        }
    }

    Ok(None)
}

/// Set a setting value in SQL database
#[command]
pub fn set_sql_setting<R: Runtime>(handle: AppHandle<R>, key: String, value: String) -> Result<(), String> {
    if let Ok(_npub) = crate::account_manager::get_current_account() {
        let conn = crate::account_manager::get_db_connection(&handle)?;

        conn.execute(
            "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
            rusqlite::params![&key, &value],
        ).map_err(|e| format!("Failed to set setting: {}", e))?;

        crate::account_manager::return_db_connection(conn);
        return Ok(());
    }
    Ok(())
}

/// Get a setting value from SQL database
#[command]
pub fn get_sql_setting<R: Runtime>(handle: AppHandle<R>, key: String) -> Result<Option<String>, String> {
    if let Ok(_npub) = crate::account_manager::get_current_account() {
        let conn = crate::account_manager::get_db_connection(&handle)?;

        let result: Option<String> = conn.query_row(
            "SELECT value FROM settings WHERE key = ?1",
            rusqlite::params![&key],
            |row| row.get(0)
        ).ok();

        crate::account_manager::return_db_connection(conn);
        return Ok(result);
    }
    Ok(None)
}


#[command]
pub fn remove_setting<R: Runtime>(handle: AppHandle<R>, key: String) -> Result<bool, String> {
    let conn = crate::account_manager::get_db_connection(&handle)?;

    let rows_affected = conn.execute(
        "DELETE FROM settings WHERE key = ?1",
        rusqlite::params![key],
    ).map_err(|e| format!("Failed to delete setting: {}", e))?;

    crate::account_manager::return_db_connection(conn);
    Ok(rows_affected > 0)
}

/// Slim version of Chat for database storage
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SlimChatDB {
    pub id: String,  // The semantic ID (npub or group_id) - used in code
    pub chat_type: ChatType,
    pub participants: Vec<String>,
    pub last_read: String,
    pub created_at: u64,
    pub metadata: crate::ChatMetadata,
    pub muted: bool,
}

/// Helper function to get or create integer chat ID from identifier
/// Uses in-memory cache for maximum speed, only hits DB on cache miss
fn get_or_create_chat_id<R: Runtime>(
    handle: &AppHandle<R>,
    chat_identifier: &str,
) -> Result<i64, String> {
    // Check cache first (fast path - no DB access)
    {
        let cache = CHAT_ID_CACHE.read().unwrap();
        if let Some(&id) = cache.get(chat_identifier) {
            return Ok(id);
        }
    }

    // Cache miss - check database
    let conn = crate::account_manager::get_db_connection(handle)?;

    // Try to get existing ID from database
    let existing_id: Option<i64> = conn.query_row(
        "SELECT id FROM chats WHERE chat_identifier = ?1",
        rusqlite::params![chat_identifier],
        |row| row.get(0)
    ).ok();

    let id = if let Some(id) = existing_id {
        id
    } else {
        // Create new chat entry with minimal data
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        conn.execute(
            "INSERT INTO chats (chat_identifier, chat_type, participants, last_read, created_at, metadata, muted)
             VALUES (?1, 0, '[]', '', ?2, '{}', 0)",
            rusqlite::params![chat_identifier, now as i64],
        ).map_err(|e| format!("Failed to create chat: {}", e))?;

        // Get the auto-generated ID
        conn.last_insert_rowid()
    };

    // Return connection to pool
    crate::account_manager::return_db_connection(conn);

    // Update cache with the ID (write to both DB and cache)
    {
        let mut cache = CHAT_ID_CACHE.write().unwrap();
        cache.insert(chat_identifier.to_string(), id);
    }

    Ok(id)
}

/// Helper function to get or create integer user ID from npub
/// Uses in-memory cache for maximum speed, only hits DB on cache miss
fn get_or_create_user_id<R: Runtime>(
    handle: &AppHandle<R>,
    npub: &str,
) -> Result<Option<i64>, String> {
    // If npub is empty, return None (for messages without author)
    if npub.is_empty() {
        return Ok(None);
    }

    // Check cache first (fast path - no DB access)
    {
        let cache = USER_ID_CACHE.read().unwrap();
        if let Some(&id) = cache.get(npub) {
            return Ok(Some(id));
        }
    }

    // Cache miss - check database
    let conn = crate::account_manager::get_db_connection(handle)?;

    // Try to get existing ID from database
    let existing_id: Option<i64> = conn.query_row(
        "SELECT id FROM profiles WHERE npub = ?1",
        rusqlite::params![npub],
        |row| row.get(0)
    ).ok();

    let id = if let Some(id) = existing_id {
        id
    } else {
        // Create new profile entry with minimal data (just the npub)
        conn.execute(
            "INSERT INTO profiles (npub, name, display_name) VALUES (?1, '', '')",
            rusqlite::params![npub],
        ).map_err(|e| format!("Failed to create profile stub: {}", e))?;

        // Get the auto-generated ID
        conn.last_insert_rowid()
    };

    // Return connection to pool
    crate::account_manager::return_db_connection(conn);

    // Update cache with the ID (write to both DB and cache)
    {
        let mut cache = USER_ID_CACHE.write().unwrap();
        cache.insert(npub.to_string(), id);
    }

    Ok(Some(id))
}

/// Preload all ID mappings into memory cache on app startup
/// This ensures all subsequent lookups are instant (no DB access)
pub async fn preload_id_caches<R: Runtime>(handle: &AppHandle<R>) -> Result<(), String> {
    let _npub = match crate::account_manager::get_current_account() {
        Ok(n) => n,
        Err(_) => return Ok(()), // No account selected, skip
    };

    let conn = crate::account_manager::get_db_connection(handle)?;

    // Load all chat ID mappings
    {
        let mut stmt = conn.prepare("SELECT chat_identifier, id FROM chats")
            .map_err(|e| format!("Failed to prepare chat query: {}", e))?;

        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        }).map_err(|e| format!("Failed to query chats: {}", e))?;

        let mut cache = CHAT_ID_CACHE.write().unwrap();
        cache.clear();

        for row in rows {
            let (identifier, id) = row.map_err(|e| format!("Failed to read chat row: {}", e))?;
            cache.insert(identifier, id);
        }
    }

    // Load all user ID mappings
    {
        let mut stmt = conn.prepare("SELECT npub, id FROM profiles")
            .map_err(|e| format!("Failed to prepare profile query: {}", e))?;

        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        }).map_err(|e| format!("Failed to query profiles: {}", e))?;

        let mut cache = USER_ID_CACHE.write().unwrap();
        cache.clear();

        for row in rows {
            let (npub, id) = row.map_err(|e| format!("Failed to read profile row: {}", e))?;
            cache.insert(npub, id);
        }
    }

    // Return connection to pool
    crate::account_manager::return_db_connection(conn);

    Ok(())
}

/// Clear ID caches (useful when switching accounts)
pub fn clear_id_caches() {
    CHAT_ID_CACHE.write().unwrap().clear();
    USER_ID_CACHE.write().unwrap().clear();
}

impl From<&Chat> for SlimChatDB {
    fn from(chat: &Chat) -> Self {
        SlimChatDB {
            id: chat.id().clone(),
            chat_type: chat.chat_type().clone(),
            participants: chat.participants().clone(),
            last_read: chat.last_read().clone(),
            created_at: chat.created_at(),
            metadata: chat.metadata().clone(),
            muted: chat.muted(),
        }
    }
}

impl SlimChatDB {
    // Convert back to full Chat (messages will be loaded separately)
    pub fn to_chat(&self) -> Chat {
        let mut chat = Chat::new(self.id.clone(), self.chat_type.clone(), self.participants.clone());
        chat.last_read = self.last_read.clone();
        chat.created_at = self.created_at;
        chat.metadata = self.metadata.clone();
        chat.muted = self.muted;
        chat
    }
}

/// Get all chats from the database
pub async fn get_all_chats<R: Runtime>(handle: &AppHandle<R>) -> Result<Vec<SlimChatDB>, String> {
    let conn = crate::account_manager::get_db_connection(handle)?;

    let mut stmt = conn.prepare("SELECT chat_identifier, chat_type, participants, last_read, created_at, metadata, muted FROM chats ORDER BY created_at DESC")
        .map_err(|e| format!("Failed to prepare statement: {}", e))?;

    let rows = stmt.query_map([], |row| {
        let participants_json: String = row.get(2)?;
        let participants: Vec<String> = serde_json::from_str(&participants_json).unwrap_or_default();

        let metadata_json: String = row.get(5)?;
        let metadata: crate::ChatMetadata = serde_json::from_str(&metadata_json).unwrap_or_default();

        let chat_type_int: i32 = row.get(1)?;
        let chat_type = crate::ChatType::from_i32(chat_type_int);

        Ok(SlimChatDB {
            id: row.get(0)?,  // chat_identifier (the semantic ID)
            chat_type,
            participants,
            last_read: row.get(3)?,
            created_at: row.get::<_, i64>(4)? as u64,
            metadata,
            muted: row.get::<_, i32>(6)? != 0,
        })
    })
    .map_err(|e| format!("Failed to query chats: {}", e))?;

    let chats: Vec<SlimChatDB> = rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("Failed to collect chats: {}", e))?;

    drop(stmt); // Explicitly drop stmt before returning connection
    crate::account_manager::return_db_connection(conn);
    Ok(chats)
}

/// Save a single chat to the database
pub async fn save_chat<R: Runtime>(handle: AppHandle<R>, chat: &Chat) -> Result<(), String> {
    let conn = crate::account_manager::get_db_connection(&handle)?;

    let slim_chat = SlimChatDB::from(chat);
    let chat_identifier = &slim_chat.id;

    let chat_type_int = slim_chat.chat_type.to_i32();
    let participants_json = serde_json::to_string(&slim_chat.participants)
        .unwrap_or_else(|_| "[]".to_string());
    let metadata_json = serde_json::to_string(&slim_chat.metadata)
        .unwrap_or_else(|_| "{}".to_string());

    // Use INSERT ... ON CONFLICT DO UPDATE to avoid triggering CASCADE delete
    conn.execute(
        "INSERT INTO chats (chat_identifier, chat_type, participants, last_read, created_at, metadata, muted)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(chat_identifier) DO UPDATE SET
            chat_type = excluded.chat_type,
            participants = excluded.participants,
            last_read = excluded.last_read,
            metadata = excluded.metadata,
            muted = excluded.muted",
        rusqlite::params![
            chat_identifier,
            chat_type_int,
            participants_json,
            slim_chat.last_read,
            slim_chat.created_at as i64,
            metadata_json,
            slim_chat.muted as i32,
        ],
    ).map_err(|e| format!("Failed to upsert chat: {}", e))?;

    crate::account_manager::return_db_connection(conn);
    Ok(())
}

/// Delete a chat and all its messages from the database
pub async fn delete_chat<R: Runtime>(handle: AppHandle<R>, chat_id: &str) -> Result<(), String> {
    let conn = crate::account_manager::get_db_connection(&handle)?;

    conn.execute(
        "DELETE FROM chats WHERE id = ?1",
        rusqlite::params![chat_id],
    ).map_err(|e| format!("Failed to delete chat: {}", e))?;

    println!("[DB] Deleted chat and messages from SQL: {}", chat_id);

    crate::account_manager::return_db_connection(conn);
    Ok(())
}

/// Save a single message to the database (efficient for incremental updates)
pub async fn save_message<R: Runtime>(
    handle: AppHandle<R>,
    chat_id: &str,
    message: &Message
) -> Result<(), String> {
    let npub = crate::account_manager::get_current_account()?;
    let _db_path = crate::account_manager::get_database_path(&handle, &npub)?;

    // Encrypt the message content
    let encrypted_content = internal_encrypt(message.content.clone(), None).await;

    let attachments_json = serde_json::to_string(&message.attachments)
        .unwrap_or_else(|_| "[]".to_string());
    let reactions_json = serde_json::to_string(&message.reactions)
        .unwrap_or_else(|_| "[]".to_string());
    let preview_json = message.preview_metadata.as_ref()
        .and_then(|p| serde_json::to_string(p).ok());

    // Get database connection
    let conn = crate::account_manager::get_db_connection(&handle)?;

    // Get or create integer chat ID
    let chat_int_id = get_or_create_chat_id(&handle, chat_id)?;

    // Get or create integer user ID from npub
    let user_int_id = if let Some(ref npub_str) = message.npub {
        get_or_create_user_id(&handle, npub_str)?
    } else {
        None
    };

    conn.execute(
        "INSERT OR REPLACE INTO messages (id, chat_id, content_encrypted, replied_to, preview_metadata, attachments, reactions, at, mine, user_id, wrapper_event_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        rusqlite::params![
            message.id,
            chat_int_id,
            encrypted_content,
            message.replied_to,
            preview_json,
            attachments_json,
            reactions_json,
            message.at as i64,
            message.mine as i32,
            user_int_id,
            message.wrapper_event_id,
        ],
    ).map_err(|e| format!("Failed to insert message {}: {}", message.id, e))?;

    crate::account_manager::return_db_connection(conn);

    Ok(())
}

/// Save multiple messages for a specific chat (batch operation with transaction)
///
/// Note: This performs UPSERT operations - it only inserts/updates the provided messages,
/// it does NOT delete other messages in the chat. This is safe for incremental updates.
pub async fn save_chat_messages<R: Runtime>(
    handle: AppHandle<R>,
    chat_id: &str,
    messages: &[Message]
) -> Result<(), String> {
    // Skip if no messages to save
    if messages.is_empty() {
        return Ok(());
    }

    // For single message, use the optimized single-message function
    if messages.len() == 1 {
        return save_message(handle, chat_id, &messages[0]).await;
    }

    let npub = crate::account_manager::get_current_account()?;
    let _db_path = crate::account_manager::get_database_path(&handle, &npub)?;

    // Get or create integer chat ID
    let chat_int_id = get_or_create_chat_id(&handle, chat_id)?;


    // Encrypt all messages first and get user IDs (async operations before database transaction)
    let mut encrypted_messages = Vec::new();
    for message in messages {
        // Get or create user ID
        let user_int_id = if let Some(ref npub_str) = message.npub {
            get_or_create_user_id(&handle, npub_str)?
        } else {
            None
        };

        let encrypted_content = internal_encrypt(message.content.clone(), None).await;
        let attachments_json = serde_json::to_string(&message.attachments)
            .unwrap_or_else(|_| "[]".to_string());
        let reactions_json = serde_json::to_string(&message.reactions)
            .unwrap_or_else(|_| "[]".to_string());
        let preview_json = message.preview_metadata.as_ref()
            .and_then(|p| serde_json::to_string(p).ok());

        encrypted_messages.push((
            message.id.clone(),
            encrypted_content,
            message.replied_to.clone(),
            preview_json,
            attachments_json,
            reactions_json,
            message.at,
            message.mine,
            user_int_id,
            message.wrapper_event_id.clone(),
        ));
    }

    // Now do all database operations synchronously
    let mut conn = crate::account_manager::get_db_connection(&handle)?;

    let tx = conn.transaction()
        .map_err(|e| format!("Failed to start transaction: {}", e))?;

    // Use INSERT OR REPLACE to upsert individual messages (preserves other messages in the chat)
    for (id, encrypted_content, replied_to, preview_json, attachments_json, reactions_json, at, mine, user_int_id, wrapper_event_id) in encrypted_messages {
        if let Err(e) = tx.execute(
            "INSERT OR REPLACE INTO messages (id, chat_id, content_encrypted, replied_to, preview_metadata, attachments, reactions, at, mine, user_id, wrapper_event_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            rusqlite::params![
                id,
                chat_int_id,
                encrypted_content,
                replied_to,
                preview_json,
                attachments_json,
                reactions_json,
                at as i64,
                mine as i32,
                user_int_id,
                wrapper_event_id,
            ],
        ) {
            eprintln!("Failed to insert message {} for chat {}: {}",
                &id[..8.min(id.len())], &chat_id[..8.min(chat_id.len())], e);
        }
    }

    let result = match tx.commit() {
        Ok(_) => Ok(()),
        Err(e) => {
            eprintln!("Failed to commit transaction for chat {}: {}",
                &chat_id[..8.min(chat_id.len())], e);
            Err(format!("Failed to commit transaction: {}", e))
        }
    };

    crate::account_manager::return_db_connection(conn);
    result
}

// ============================================================================
// MLS Metadata SQL Functions
// ============================================================================

/// Save MLS groups to SQL database (plaintext columns)
pub async fn save_mls_groups<R: Runtime>(
    handle: AppHandle<R>,
    groups: &[crate::mls::MlsGroupMetadata],
) -> Result<(), String> {
    let conn = crate::account_manager::get_db_connection(&handle)?;

    // Store each group in the mls_groups table (all fields as columns)
    for group in groups {
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
        ).map_err(|e| format!("Failed to save MLS group {}: {}", group.group_id, e))?;
    }

    println!("[SQL] Saved {} MLS groups to mls_groups table", groups.len());
    crate::account_manager::return_db_connection(conn);
    Ok(())
}

/// Save a single MLS group to SQL database (plaintext columns) - more efficient for adding new groups
pub async fn save_mls_group<R: Runtime>(
    handle: AppHandle<R>,
    group: &crate::mls::MlsGroupMetadata,
) -> Result<(), String> {
    let conn = crate::account_manager::get_db_connection(&handle)?;

    // Insert or replace a single group
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
    ).map_err(|e| format!("Failed to save MLS group {}: {}", group.group_id, e))?;

    println!("[SQL] Saved 1 MLS group to mls_groups table");
    crate::account_manager::return_db_connection(conn);
    Ok(())
}

/// Load MLS groups from SQL database (plaintext columns)
pub async fn load_mls_groups<R: Runtime>(
    handle: &AppHandle<R>,
) -> Result<Vec<crate::mls::MlsGroupMetadata>, String> {
    let conn = crate::account_manager::get_db_connection(handle)?;

    // Load from mls_groups table
    let mut stmt = conn.prepare(
        "SELECT group_id, engine_group_id, creator_pubkey, name, avatar_ref, created_at, updated_at, evicted FROM mls_groups"
    ).map_err(|e| format!("Failed to prepare query: {}", e))?;

    let rows = stmt.query_map([], |row| {
        Ok(crate::mls::MlsGroupMetadata {
            group_id: row.get(0)?,
            engine_group_id: row.get(1)?,
            creator_pubkey: row.get(2)?,
            name: row.get(3)?,
            avatar_ref: row.get(4)?,
            created_at: row.get::<_, i64>(5)? as u64,
            updated_at: row.get::<_, i64>(6)? as u64,
            evicted: row.get::<_, i32>(7)? != 0,
        })
    }).map_err(|e| format!("Failed to query mls_groups: {}", e))?;

    let groups: Vec<crate::mls::MlsGroupMetadata> = rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("Failed to collect groups: {}", e))?;

    drop(stmt);
    crate::account_manager::return_db_connection(conn);
    Ok(groups)
}

/// Save MLS keypackage index to SQL database (plaintext)
pub async fn save_mls_keypackages<R: Runtime>(
    handle: AppHandle<R>,
    packages: &[serde_json::Value],
) -> Result<(), String> {
    let conn = crate::account_manager::get_db_connection(&handle)?;

    // Clear existing keypackages
    conn.execute("DELETE FROM mls_keypackages", [])
        .map_err(|e| format!("Failed to clear MLS keypackages: {}", e))?;

    // Insert new keypackages
    for pkg in packages {
        let owner_pubkey = pkg.get("owner_pubkey").and_then(|v| v.as_str()).unwrap_or("");
        let device_id = pkg.get("device_id").and_then(|v| v.as_str()).unwrap_or("");
        let keypackage_ref = pkg.get("keypackage_ref").and_then(|v| v.as_str()).unwrap_or("");
        let fetched_at = pkg.get("fetched_at").and_then(|v| v.as_u64()).unwrap_or(0);
        let expires_at = pkg.get("expires_at").and_then(|v| v.as_u64()).unwrap_or(0);

        conn.execute(
            "INSERT INTO mls_keypackages (owner_pubkey, device_id, keypackage_ref, fetched_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![owner_pubkey, device_id, keypackage_ref, fetched_at as i64, expires_at as i64],
        ).map_err(|e| format!("Failed to insert MLS keypackage: {}", e))?;
    }

    println!("[SQL] Saved {} MLS keypackages", packages.len());
    crate::account_manager::return_db_connection(conn);
    Ok(())
}

/// Load MLS keypackage index from SQL database (plaintext)
pub async fn load_mls_keypackages<R: Runtime>(
    handle: &AppHandle<R>,
) -> Result<Vec<serde_json::Value>, String> {
    let conn = crate::account_manager::get_db_connection(handle)?;

    let mut stmt = conn.prepare(
        "SELECT owner_pubkey, device_id, keypackage_ref, fetched_at, expires_at FROM mls_keypackages"
    ).map_err(|e| format!("Failed to prepare MLS keypackages query: {}", e))?;

    let rows = stmt.query_map([], |row| {
        let fetched_at: i64 = row.get(3)?;
        let expires_at: i64 = row.get(4)?;
        Ok(serde_json::json!({
            "owner_pubkey": row.get::<_, String>(0)?,
            "device_id": row.get::<_, String>(1)?,
            "keypackage_ref": row.get::<_, String>(2)?,
            "fetched_at": fetched_at as u64,
            "expires_at": expires_at as u64,
        }))
    }).map_err(|e| format!("Failed to query MLS keypackages: {}", e))?;

    let packages: Vec<serde_json::Value> = rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("Failed to collect MLS keypackages: {}", e))?;

    drop(stmt);
    crate::account_manager::return_db_connection(conn);
    Ok(packages)
}

/// Save MLS event cursors to SQL database (plaintext)
pub async fn save_mls_event_cursors<R: Runtime>(
    handle: AppHandle<R>,
    cursors: &HashMap<String, crate::mls::EventCursor>,
) -> Result<(), String> {
    let conn = crate::account_manager::get_db_connection(&handle)?;

    for (group_id, cursor) in cursors {
        conn.execute(
            "INSERT OR REPLACE INTO mls_event_cursors (group_id, last_seen_event_id, last_seen_at)
             VALUES (?1, ?2, ?3)",
            rusqlite::params![group_id, &cursor.last_seen_event_id, cursor.last_seen_at as i64],
        ).map_err(|e| format!("Failed to save MLS event cursor: {}", e))?;
    }

    crate::account_manager::return_db_connection(conn);
    Ok(())
}

/// Load MLS event cursors from SQL database (plaintext)
pub async fn load_mls_event_cursors<R: Runtime>(
    handle: &AppHandle<R>,
) -> Result<HashMap<String, crate::mls::EventCursor>, String> {
    let conn = crate::account_manager::get_db_connection(handle)?;

    let mut stmt = conn.prepare(
        "SELECT group_id, last_seen_event_id, last_seen_at FROM mls_event_cursors"
    ).map_err(|e| format!("Failed to prepare MLS event cursors query: {}", e))?;

    let rows = stmt.query_map([], |row| {
        let group_id: String = row.get(0)?;
        let last_seen_at: i64 = row.get(2)?;
        let cursor = crate::mls::EventCursor {
            last_seen_event_id: row.get(1)?,
            last_seen_at: last_seen_at as u64,
        };
        Ok((group_id, cursor))
    }).map_err(|e| format!("Failed to query MLS event cursors: {}", e))?;

    let cursors: HashMap<String, crate::mls::EventCursor> = rows.collect::<Result<HashMap<_, _>, _>>()
        .map_err(|e| format!("Failed to collect MLS event cursors: {}", e))?;

    drop(stmt);
    crate::account_manager::return_db_connection(conn);
    Ok(cursors)
}

/// Save MLS device ID to SQL database (plaintext)
pub async fn save_mls_device_id<R: Runtime>(
    handle: AppHandle<R>,
    device_id: &str,
) -> Result<(), String> {
    let conn = crate::account_manager::get_db_connection(&handle)?;

    conn.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES ('mls_device_id', ?1)",
        rusqlite::params![device_id],
    ).map_err(|e| format!("Failed to save MLS device ID to SQL: {}", e))?;

    println!("[SQL] Saved MLS device ID");
    crate::account_manager::return_db_connection(conn);
    Ok(())
}

/// Load MLS device ID from SQL database (plaintext)
pub async fn load_mls_device_id<R: Runtime>(
    handle: &AppHandle<R>,
) -> Result<Option<String>, String> {
    let conn = crate::account_manager::get_db_connection(handle)?;

    let device_id: Option<String> = conn.query_row(
        "SELECT value FROM settings WHERE key = 'mls_device_id'",
        [],
        |row| row.get(0)
    ).ok();

    crate::account_manager::return_db_connection(conn);
    Ok(device_id)
}

/// Lightweight attachment reference for file deduplication
/// Contains only the data needed to reuse an existing upload
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AttachmentRef {
    /// The SHA256 hash of the original file (used as ID)
    pub hash: String,
    /// The message ID containing this attachment
    pub message_id: String,
    /// The chat ID containing this message
    pub chat_id: String,
    /// The encrypted file URL on the server
    pub url: String,
    /// The encryption key
    pub key: String,
    /// The encryption nonce
    pub nonce: String,
    /// The file extension
    pub extension: String,
    /// The encrypted file size
    pub size: u64,
}

/// Build a file hash index from all attachments in the database
/// This is used for file deduplication without loading full message content
/// Returns a HashMap of file_hash -> AttachmentRef
pub async fn build_file_hash_index<R: Runtime>(
    handle: &AppHandle<R>,
) -> Result<HashMap<String, AttachmentRef>, String> {
    let mut index: HashMap<String, AttachmentRef> = HashMap::new();

    let conn = crate::account_manager::get_db_connection(handle)?;

    // Query all messages with non-empty attachments
    // The attachments field is stored as plaintext JSON, so no decryption needed!
    // Collect all data first, then drop the statement before returning connection
    let attachment_data: Vec<(String, String, String)> = {
        let mut stmt = conn.prepare(
            "SELECT m.id, c.chat_identifier, m.attachments
             FROM messages m
             JOIN chats c ON m.chat_id = c.id
             WHERE m.attachments != '[]'"
        ).map_err(|e| format!("Failed to prepare attachment query: {}", e))?;

        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?, // message_id
                row.get::<_, String>(1)?, // chat_identifier
                row.get::<_, String>(2)?, // attachments JSON
            ))
        }).map_err(|e| format!("Failed to query attachments: {}", e))?;

        // Collect immediately to consume the iterator while stmt is still alive
        let result: Result<Vec<_>, _> = rows.collect();
        result.map_err(|e| format!("Failed to collect attachment rows: {}", e))?
    }; // stmt is dropped here

    // Return connection to pool before processing
    crate::account_manager::return_db_connection(conn);

    // Process the collected data
    const EMPTY_FILE_HASH: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    for (message_id, chat_id, attachments_json) in attachment_data {
        // Parse the attachments JSON
        let attachments: Vec<crate::Attachment> = serde_json::from_str(&attachments_json)
            .unwrap_or_default();

        // Add each attachment to the index (skip empty hashes and empty URLs)
        for attachment in attachments {
            if !attachment.id.is_empty()
                && attachment.id != EMPTY_FILE_HASH
                && !attachment.url.is_empty()
            {
                index.insert(attachment.id.clone(), AttachmentRef {
                    hash: attachment.id,
                    message_id: message_id.clone(),
                    chat_id: chat_id.clone(),
                    url: attachment.url,
                    key: attachment.key,
                    nonce: attachment.nonce,
                    extension: attachment.extension,
                    size: attachment.size,
                });
            }
        }
    }

    Ok(index)
}

/// Get paginated messages for a chat (newest first, with offset)
/// This allows loading messages on-demand instead of all at once
///
/// Parameters:
/// - chat_id: The chat identifier (npub for DMs, group_id for groups)
/// - limit: Maximum number of messages to return
/// - offset: Number of messages to skip from the newest
///
/// Returns messages in chronological order (oldest first within the batch)
pub async fn get_chat_messages_paginated<R: Runtime>(
    handle: &AppHandle<R>,
    chat_id: &str,
    limit: usize,
    offset: usize,
) -> Result<Vec<Message>, String> {
    let conn = crate::account_manager::get_db_connection(handle)?;

    // Get integer chat ID from identifier
    let chat_int_id: i64 = conn.query_row(
        "SELECT id FROM chats WHERE chat_identifier = ?1",
        rusqlite::params![chat_id],
        |row| row.get(0)
    ).map_err(|e| format!("Chat not found: {}", e))?;

    // Query messages with pagination (newest first, then reverse for chronological order)
    // We use a subquery to get the right slice, then order chronologically
    let messages = {
        let mut stmt = conn.prepare(
            "SELECT id, content_encrypted, replied_to, preview_metadata, attachments, reactions, at, mine, user_id
             FROM (
                 SELECT m.id, m.content_encrypted, m.replied_to, m.preview_metadata, m.attachments, m.reactions, m.at, m.mine, p.npub as user_id
                 FROM messages m
                 LEFT JOIN profiles p ON m.user_id = p.id
                 WHERE m.chat_id = ?1
                 ORDER BY m.at DESC
                 LIMIT ?2 OFFSET ?3
             )
             ORDER BY at ASC"
        ).map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let rows = stmt.query_map(rusqlite::params![chat_int_id, limit as i64, offset as i64], |row| {
            Ok((
                row.get::<_, String>(0)?, // id
                row.get::<_, String>(1)?, // content_encrypted
                row.get::<_, String>(2)?, // replied_to
                row.get::<_, Option<String>>(3)?, // preview_metadata
                row.get::<_, String>(4)?, // attachments
                row.get::<_, String>(5)?, // reactions
                row.get::<_, i64>(6)? as u64, // at
                row.get::<_, i32>(7)? != 0, // mine
                row.get::<_, Option<String>>(8)?, // npub
            ))
        }).map_err(|e| format!("Failed to query messages: {}", e))?;

        let result: Result<Vec<_>, _> = rows.collect();
        result.map_err(|e| format!("Failed to collect messages: {}", e))?
    };

    crate::account_manager::return_db_connection(conn);

    // Decrypt content for each message
    let mut result = Vec::with_capacity(messages.len());
    for (id, content_encrypted, replied_to, preview_json, attachments_json, reactions_json, at, mine, npub) in messages {
        let content = internal_decrypt(content_encrypted, None).await
            .unwrap_or_else(|_| "[Decryption failed]".to_string());

        let attachments: Vec<crate::Attachment> = serde_json::from_str(&attachments_json).unwrap_or_default();
        let reactions: Vec<crate::Reaction> = serde_json::from_str(&reactions_json).unwrap_or_default();
        let preview_metadata = preview_json.and_then(|p| serde_json::from_str(&p).ok());

        result.push(Message {
            id,
            content,
            replied_to,
            preview_metadata,
            attachments,
            reactions,
            at,
            pending: false,
            failed: false,
            mine,
            npub,
            wrapper_event_id: None, // Paginated queries don't need wrapper_event_id
        });
    }

    Ok(result)
}

/// Get the total message count for a chat
/// This is useful for the frontend to know how many messages exist without loading them all
pub async fn get_chat_message_count<R: Runtime>(
    handle: &AppHandle<R>,
    chat_id: &str,
) -> Result<usize, String> {
    let conn = crate::account_manager::get_db_connection(handle)?;

    // Get integer chat ID from identifier
    let chat_int_id: i64 = conn.query_row(
        "SELECT id FROM chats WHERE chat_identifier = ?1",
        rusqlite::params![chat_id],
        |row| row.get(0)
    ).map_err(|e| format!("Chat not found: {}", e))?;

    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM messages WHERE chat_id = ?1",
        rusqlite::params![chat_int_id],
        |row| row.get(0)
    ).map_err(|e| format!("Failed to count messages: {}", e))?;

    crate::account_manager::return_db_connection(conn);

    Ok(count as usize)
}

/// Get the last N messages for a chat (for preview purposes)
/// This is optimized for getting just the most recent messages without loading the full history
pub async fn get_chat_last_messages<R: Runtime>(
    handle: &AppHandle<R>,
    chat_id: &str,
    count: usize,
) -> Result<Vec<Message>, String> {
    // Just use paginated with offset 0
    get_chat_messages_paginated(handle, chat_id, count, 0).await
}

/// Check if a message exists in the database by its ID
/// This is used to prevent duplicate processing during sync
pub async fn message_exists_in_db<R: Runtime>(
    handle: &AppHandle<R>,
    message_id: &str,
) -> Result<bool, String> {
    // Try to get a database connection - if it fails, we're not using DB mode
    let conn = match crate::account_manager::get_db_connection(handle) {
        Ok(c) => c,
        Err(_) => return Ok(false), // No DB, let in-memory check handle it
    };

    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM messages WHERE id = ?1)",
        rusqlite::params![message_id],
        |row| row.get(0)
    ).map_err(|e| format!("Failed to check message existence: {}", e))?;

    crate::account_manager::return_db_connection(conn);

    Ok(exists)
}

/// Check if a wrapper (giftwrap) event ID exists in the database
/// This allows skipping the expensive unwrap operation for already-processed events
pub async fn wrapper_event_exists<R: Runtime>(
    handle: &AppHandle<R>,
    wrapper_event_id: &str,
) -> Result<bool, String> {
    // Try to get a database connection - if it fails, we're not using DB mode
    let conn = match crate::account_manager::get_db_connection(handle) {
        Ok(c) => c,
        Err(_) => return Ok(false), // No DB, can't check
    };

    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM messages WHERE wrapper_event_id = ?1)",
        rusqlite::params![wrapper_event_id],
        |row| row.get(0)
    ).map_err(|e| format!("Failed to check wrapper event existence: {}", e))?;

    crate::account_manager::return_db_connection(conn);

    Ok(exists)
}

/// Update the wrapper event ID for an existing message
/// This is called when we process a message that was previously stored without its wrapper ID
/// Returns: Ok(true) if updated, Ok(false) if message already had a wrapper_id (duplicate giftwrap)
pub async fn update_wrapper_event_id<R: Runtime>(
    handle: &AppHandle<R>,
    message_id: &str,
    wrapper_event_id: &str,
) -> Result<bool, String> {
    // Try to get a database connection - if it fails, we're not using DB mode
    let conn = match crate::account_manager::get_db_connection(handle) {
        Ok(c) => c,
        Err(_) => return Ok(false), // No DB, nothing to update
    };

    let rows_updated = match conn.execute(
        "UPDATE messages SET wrapper_event_id = ?1 WHERE id = ?2 AND (wrapper_event_id IS NULL OR wrapper_event_id = '')",
        rusqlite::params![wrapper_event_id, message_id],
    ) {
        Ok(n) => n,
        Err(e) => {
            crate::account_manager::return_db_connection(conn);
            return Err(format!("Failed to update wrapper event ID: {}", e));
        }
    };

    crate::account_manager::return_db_connection(conn);

    // Returns true if backfill succeeded, false if message already has a wrapper_id (duplicate giftwrap)
    Ok(rows_updated > 0)
}

/// Load recent wrapper_event_ids into a HashSet for fast duplicate detection
/// This preloads wrapper_ids from the last N days to avoid SQL queries during sync
pub async fn load_recent_wrapper_ids<R: Runtime>(
    handle: &AppHandle<R>,
    days: u64,
) -> Result<std::collections::HashSet<String>, String> {
    let mut wrapper_ids = std::collections::HashSet::new();

    // Try to get a database connection - if it fails, we're not using DB mode
    let conn = match crate::account_manager::get_db_connection(handle) {
        Ok(c) => c,
        Err(_) => return Ok(wrapper_ids), // No DB, return empty set
    };

    // Calculate timestamp for N days ago (in milliseconds, matching our `at` field)
    let cutoff_ms = (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64)
        .saturating_sub(days * 24 * 60 * 60 * 1000);

    // Query all wrapper_event_ids from recent messages
    let result: Result<Vec<String>, _> = {
        let mut stmt = conn.prepare(
            "SELECT wrapper_event_id FROM messages
             WHERE wrapper_event_id IS NOT NULL
             AND wrapper_event_id != ''
             AND at >= ?1"
        ).map_err(|e| format!("Failed to prepare wrapper_id query: {}", e))?;

        let rows = stmt.query_map(rusqlite::params![cutoff_ms as i64], |row| {
            row.get::<_, String>(0)
        }).map_err(|e| format!("Failed to query wrapper_ids: {}", e))?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("Failed to collect wrapper_ids: {}", e))
    };

    crate::account_manager::return_db_connection(conn);

    match result {
        Ok(ids) => {
            for id in ids {
                wrapper_ids.insert(id);
            }
            Ok(wrapper_ids)
        }
        Err(_) => {
            Ok(wrapper_ids) // Return empty set on error, will fall back to DB queries
        }
    }
}

/// Update the downloaded status of an attachment in the database
pub fn update_attachment_downloaded_status<R: Runtime>(
    handle: &AppHandle<R>,
    chat_id: &str,
    msg_id: &str,
    attachment_id: &str,
    downloaded: bool,
    path: &str,
) -> Result<(), String> {
    let conn = crate::account_manager::get_db_connection(handle)?;

    // Get the current attachments JSON
    let attachments_json: String = conn.query_row(
        "SELECT m.attachments FROM messages m
         JOIN chats c ON m.chat_id = c.id
         WHERE m.id = ?1 AND c.chat_identifier = ?2",
        rusqlite::params![msg_id, chat_id],
        |row| row.get(0)
    ).map_err(|e| format!("Message not found: {}", e))?;

    // Parse and update the attachment
    let mut attachments: Vec<Attachment> = serde_json::from_str(&attachments_json).unwrap_or_default();

    if let Some(att) = attachments.iter_mut().find(|a| a.id == attachment_id) {
        att.downloaded = downloaded;
        att.downloading = false;
        att.path = path.to_string();
    } else {
        crate::account_manager::return_db_connection(conn);
        return Err("Attachment not found in message".to_string());
    }

    // Serialize back to JSON
    let updated_json = serde_json::to_string(&attachments)
        .map_err(|e| format!("Failed to serialize attachments: {}", e))?;

    // Get the chat's integer ID
    let chat_int_id: i64 = conn.query_row(
        "SELECT id FROM chats WHERE chat_identifier = ?1",
        rusqlite::params![chat_id],
        |row| row.get(0)
    ).map_err(|e| format!("Chat not found: {}", e))?;

    // Update the message in the database
    conn.execute(
        "UPDATE messages SET attachments = ?1 WHERE id = ?2 AND chat_id = ?3",
        rusqlite::params![updated_json, msg_id, chat_int_id],
    ).map_err(|e| format!("Failed to update message: {}", e))?;

    crate::account_manager::return_db_connection(conn);

    Ok(())
}

/// Vacuum the database to reclaim space and optimize performance
pub fn vacuum_database<R: Runtime>(handle: &AppHandle<R>) -> Result<(), String> {
    let conn = crate::account_manager::get_db_connection(handle)?;

    conn.execute_batch("VACUUM;")
        .map_err(|e| format!("Failed to vacuum database: {}", e))?;

    crate::account_manager::return_db_connection(conn);
    println!("[DB] Database vacuumed successfully");
    Ok(())
}

/// Check if vacuum is needed and perform it if so
/// Vacuums if it hasn't been done in the last 7 days
pub async fn check_and_vacuum_if_needed<R: Runtime>(handle: &AppHandle<R>) -> Result<(), String> {
    let conn = crate::account_manager::get_db_connection(handle)?;

    // Check when last vacuum was performed
    let last_vacuum: Option<i64> = conn.query_row(
        "SELECT value FROM settings WHERE key = 'last_vacuum'",
        [],
        |row| row.get(0)
    ).ok().and_then(|s: String| s.parse().ok());

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;

    let seven_days_secs = 7 * 24 * 60 * 60;

    let should_vacuum = match last_vacuum {
        Some(last) => now - last > seven_days_secs,
        None => true, // Never vacuumed
    };

    crate::account_manager::return_db_connection(conn);

    if should_vacuum {
        vacuum_database(handle)?;

        // Update last vacuum timestamp
        let conn = crate::account_manager::get_db_connection(handle)?;
        conn.execute(
            "INSERT OR REPLACE INTO settings (key, value) VALUES ('last_vacuum', ?1)",
            rusqlite::params![now.to_string()],
        ).map_err(|e| format!("Failed to update last_vacuum: {}", e))?;
        crate::account_manager::return_db_connection(conn);
    }

    Ok(())
}

// ============================================================================
// Mini Apps History Functions
// ============================================================================

/// Mini App history entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MiniAppHistoryEntry {
    pub id: i64,
    pub name: String,
    pub src_url: String,
    pub attachment_ref: String,
    pub open_count: i64,
    pub last_opened_at: i64,
    pub is_favorite: bool,
    /// Comma-separated list of categories (e.g., "game" or "app")
    pub categories: String,
    /// Optional marketplace app ID for linking to marketplace
    pub marketplace_id: Option<String>,
    /// Installed version of the app (for marketplace apps)
    pub installed_version: Option<String>,
}

/// Record a Mini App being opened (upsert - insert or update)
/// If the same name+src_url combo exists, update the attachment_ref, increment count, and update timestamp
pub fn record_miniapp_opened<R: Runtime>(
    handle: &AppHandle<R>,
    name: String,
    src_url: String,
    attachment_ref: String,
) -> Result<(), String> {
    record_miniapp_opened_with_metadata(handle, name, src_url, attachment_ref, String::new(), None, None)
}

/// Record a Mini App being opened with additional metadata (categories, marketplace_id, and version)
pub fn record_miniapp_opened_with_metadata<R: Runtime>(
    handle: &AppHandle<R>,
    name: String,
    src_url: String,
    attachment_ref: String,
    categories: String,
    marketplace_id: Option<String>,
    installed_version: Option<String>,
) -> Result<(), String> {
    let conn = crate::account_manager::get_db_connection(handle)?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;

    // Use INSERT OR REPLACE with a subquery to preserve/increment open_count
    // Uses UNIQUE(name) constraint - same app name always updates the existing entry
    conn.execute(
        r#"
        INSERT INTO miniapps_history (name, src_url, attachment_ref, open_count, last_opened_at, categories, marketplace_id, installed_version)
        VALUES (?1, ?2, ?3, 1, ?4, ?5, ?6, ?7)
        ON CONFLICT(name) DO UPDATE SET
            src_url = excluded.src_url,
            attachment_ref = excluded.attachment_ref,
            open_count = open_count + 1,
            last_opened_at = excluded.last_opened_at,
            categories = CASE WHEN excluded.categories != '' THEN excluded.categories ELSE categories END,
            marketplace_id = CASE WHEN excluded.marketplace_id IS NOT NULL THEN excluded.marketplace_id ELSE marketplace_id END,
            installed_version = CASE WHEN excluded.installed_version IS NOT NULL THEN excluded.installed_version ELSE installed_version END
        "#,
        rusqlite::params![name, src_url, attachment_ref, now, categories, marketplace_id, installed_version],
    ).map_err(|e| format!("Failed to record Mini App opened: {}", e))?;

    crate::account_manager::return_db_connection(conn);
    Ok(())
}

/// Get Mini Apps history sorted by last opened (most recent first)
pub fn get_miniapps_history<R: Runtime>(
    handle: &AppHandle<R>,
    limit: Option<i64>,
) -> Result<Vec<MiniAppHistoryEntry>, String> {
    let conn = crate::account_manager::get_db_connection(handle)?;

    let limit_val = limit.unwrap_or(50);

    let mut stmt = conn.prepare(
        r#"
        SELECT id, name, src_url, attachment_ref, open_count, last_opened_at, is_favorite, categories, marketplace_id, installed_version
        FROM miniapps_history
        ORDER BY is_favorite DESC, last_opened_at DESC
        LIMIT ?1
        "#
    ).map_err(|e| format!("Failed to prepare Mini Apps history query: {}", e))?;

    let result: Vec<MiniAppHistoryEntry> = {
        let entries = stmt.query_map(rusqlite::params![limit_val], |row| {
            Ok(MiniAppHistoryEntry {
                id: row.get(0)?,
                name: row.get(1)?,
                src_url: row.get(2)?,
                attachment_ref: row.get(3)?,
                open_count: row.get(4)?,
                last_opened_at: row.get(5)?,
                is_favorite: row.get::<_, i64>(6)? != 0,
                categories: row.get(7)?,
                marketplace_id: row.get(8)?,
                installed_version: row.get(9)?,
            })
        }).map_err(|e| format!("Failed to query Mini Apps history: {}", e))?;

        entries.filter_map(|e| e.ok()).collect()
    };

    drop(stmt);
    crate::account_manager::return_db_connection(conn);
    Ok(result)
}

/// Toggle the favorite status of a Mini App by its ID
pub fn toggle_miniapp_favorite<R: Runtime>(
    handle: &AppHandle<R>,
    id: i64,
) -> Result<bool, String> {
    let conn = crate::account_manager::get_db_connection(handle)?;

    // Toggle the is_favorite value and return the new state
    conn.execute(
        "UPDATE miniapps_history SET is_favorite = NOT is_favorite WHERE id = ?1",
        rusqlite::params![id],
    ).map_err(|e| format!("Failed to toggle Mini App favorite: {}", e))?;

    // Get the new favorite state
    let new_state: bool = conn.query_row(
        "SELECT is_favorite FROM miniapps_history WHERE id = ?1",
        rusqlite::params![id],
        |row| row.get::<_, i64>(0).map(|v| v != 0)
    ).map_err(|e| format!("Failed to get Mini App favorite state: {}", e))?;

    crate::account_manager::return_db_connection(conn);
    Ok(new_state)
}

/// Set the favorite status of a Mini App by its ID
pub fn set_miniapp_favorite<R: Runtime>(
    handle: &AppHandle<R>,
    id: i64,
    is_favorite: bool,
) -> Result<(), String> {
    let conn = crate::account_manager::get_db_connection(handle)?;

    conn.execute(
        "UPDATE miniapps_history SET is_favorite = ?1 WHERE id = ?2",
        rusqlite::params![if is_favorite { 1 } else { 0 }, id],
    ).map_err(|e| format!("Failed to set Mini App favorite: {}", e))?;

    crate::account_manager::return_db_connection(conn);
    Ok(())
}

/// Remove a Mini App from history by name
pub fn remove_miniapp_from_history<R: Runtime>(
    handle: &AppHandle<R>,
    name: &str,
) -> Result<(), String> {
    let conn = crate::account_manager::get_db_connection(handle)?;

    conn.execute(
        "DELETE FROM miniapps_history WHERE name = ?1",
        rusqlite::params![name],
    ).map_err(|e| format!("Failed to remove Mini App from history: {}", e))?;

    crate::account_manager::return_db_connection(conn);
    Ok(())
}

/// Update the installed version for a marketplace app
pub fn update_miniapp_version<R: Runtime>(
    handle: &AppHandle<R>,
    marketplace_id: &str,
    version: &str,
) -> Result<(), String> {
    let conn = crate::account_manager::get_db_connection(handle)?;

    conn.execute(
        "UPDATE miniapps_history SET installed_version = ?1 WHERE marketplace_id = ?2",
        rusqlite::params![version, marketplace_id],
    ).map_err(|e| format!("Failed to update Mini App version: {}", e))?;

    crate::account_manager::return_db_connection(conn);
    Ok(())
}

/// Get the installed version for a marketplace app by its marketplace_id
pub fn get_miniapp_installed_version<R: Runtime>(
    handle: &AppHandle<R>,
    marketplace_id: &str,
) -> Result<Option<String>, String> {
    let conn = crate::account_manager::get_db_connection(handle)?;

    let result = conn.query_row(
        "SELECT installed_version FROM miniapps_history WHERE marketplace_id = ?1",
        rusqlite::params![marketplace_id],
        |row| row.get::<_, Option<String>>(0)
    );

    crate::account_manager::return_db_connection(conn);

    match result {
        Ok(version) => Ok(version),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(format!("Failed to get Mini App installed version: {}", e)),
    }
}