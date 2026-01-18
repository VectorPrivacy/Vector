use serde::{Deserialize, Serialize};
use tauri::{AppHandle, command, Runtime};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use once_cell::sync::Lazy;

use crate::{Profile, Status, Message, Chat, ChatType, Attachment, Reaction};
use crate::message::EditEntry;
use crate::crypto::{internal_encrypt, internal_decrypt};
use crate::stored_event::{StoredEvent, event_kind};

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
    avatar_cached: String,
    banner_cached: String,
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
            avatar_cached: String::new(),
            banner_cached: String::new(),
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
            avatar_cached: profile.avatar_cached.clone(),
            banner_cached: profile.banner_cached.clone(),
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
            avatar_cached: self.avatar_cached.clone(),
            banner_cached: self.banner_cached.clone(),
        }
    }
}

// Function to get all profiles
pub async fn get_all_profiles<R: Runtime>(handle: &AppHandle<R>) -> Result<Vec<SlimProfile>, String> {
    let conn = crate::account_manager::get_db_connection(handle)?;

    let mut stmt = conn.prepare("SELECT npub, name, display_name, nickname, lud06, lud16, banner, avatar, about, website, nip05, status_content, status_url, muted, bot, avatar_cached, banner_cached FROM profiles")
        .map_err(|e| format!("Failed to prepare statement: {}", e))?;

    let profiles = stmt.query_map([], |row| {
        // Get cached paths and validate they exist on disk
        let avatar_cached: String = row.get(15)?;
        let banner_cached: String = row.get(16)?;

        // Only use cached paths if the files actually exist
        let validated_avatar_cached = if !avatar_cached.is_empty() && std::path::Path::new(&avatar_cached).exists() {
            avatar_cached
        } else {
            String::new()
        };
        let validated_banner_cached = if !banner_cached.is_empty() && std::path::Path::new(&banner_cached).exists() {
            banner_cached
        } else {
            String::new()
        };

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
            avatar_cached: validated_avatar_cached,
            banner_cached: validated_banner_cached,
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
        "INSERT INTO profiles (npub, name, display_name, nickname, lud06, lud16, banner, avatar, about, website, nip05, status_content, status_url, muted, bot, avatar_cached, banner_cached)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)
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
            bot = excluded.bot,
            avatar_cached = excluded.avatar_cached,
            banner_cached = excluded.banner_cached",
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
            profile.avatar_cached,
            profile.banner_cached,
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

        // Determine chat type and participants based on chat_identifier format
        // DM chats have npub as identifier, MLS groups have hex group ID
        let (chat_type, participants_json) = if chat_identifier.starts_with("npub1") {
            // DM chat: participant is the other party (the chat_identifier itself)
            (0, format!("[\"{}\"]", chat_identifier))
        } else {
            // MLS group: participants managed separately, start with empty
            (1, "[]".to_string())
        };

        conn.execute(
            "INSERT INTO chats (chat_identifier, chat_type, participants, last_read, created_at, metadata, muted)
             VALUES (?1, ?2, ?3, '', ?4, '{}', 0)",
            rusqlite::params![chat_identifier, chat_type, participants_json, now as i64],
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

/// Helper function to get integer chat ID from identifier (lookup only, no creation)
/// Returns an error if the chat doesn't exist
pub fn get_chat_id_by_identifier<R: Runtime>(
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

    let id: i64 = conn.query_row(
        "SELECT id FROM chats WHERE chat_identifier = ?1",
        rusqlite::params![chat_identifier],
        |row| row.get(0)
    ).map_err(|_| format!("Chat not found: {}", chat_identifier))?;

    crate::account_manager::return_db_connection(conn);

    // Update cache
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
///
/// This function saves to the `events` table using the flat event architecture.
/// The old `messages` table is no longer written to - it only exists for
/// backward compatibility with migrated data (read-only fallback).
pub async fn save_message<R: Runtime>(
    handle: AppHandle<R>,
    chat_id: &str,
    message: &Message
) -> Result<(), String> {
    // Get or create integer chat ID
    let chat_int_id = get_or_create_chat_id(&handle, chat_id)?;

    // Get or create integer user ID from npub
    let user_int_id = if let Some(ref npub_str) = message.npub {
        get_or_create_user_id(&handle, npub_str)?
    } else {
        None
    };

    // Save to events table (flat event architecture)
    let event = message_to_stored_event(message, chat_int_id, user_int_id);
    save_event(&handle, &event).await?;

    // Also save any reactions as separate kind=7 events
    for reaction in &message.reactions {
        // Check if reaction event already exists to avoid duplicates
        if !event_exists(&handle, &reaction.id)? {
            let user_id = get_or_create_user_id(&handle, &reaction.author_id)?;
            let is_mine = if let Ok(current_npub) = crate::account_manager::get_current_account() {
                reaction.author_id == current_npub
            } else {
                false
            };
            save_reaction_event(&handle, reaction, chat_int_id, user_id, is_mine).await?;
        }
    }

    Ok(())
}

/// Convert a Message to a StoredEvent for the flat event architecture
fn message_to_stored_event(message: &Message, chat_id: i64, user_id: Option<i64>) -> StoredEvent {
    // Determine event kind based on whether message has attachments
    let kind = if !message.attachments.is_empty() {
        event_kind::FILE_ATTACHMENT
    } else {
        event_kind::PRIVATE_DIRECT_MESSAGE
    };

    // Build tags array
    let mut tags: Vec<Vec<String>> = Vec::new();

    // Add millisecond precision tag
    let ms = message.at % 1000;
    if ms > 0 {
        tags.push(vec!["ms".to_string(), ms.to_string()]);
    }

    // Add reply reference tag if present
    if !message.replied_to.is_empty() {
        tags.push(vec![
            "e".to_string(),
            message.replied_to.clone(),
            "".to_string(),
            "reply".to_string(),
        ]);
    }

    // Add attachments as JSON tag for file messages
    if !message.attachments.is_empty() {
        if let Ok(attachments_json) = serde_json::to_string(&message.attachments) {
            tags.push(vec!["attachments".to_string(), attachments_json]);
        }
    }

    StoredEvent {
        id: message.id.clone(),
        kind,
        chat_id,
        user_id,
        content: message.content.clone(),
        tags,
        reference_id: None,
        created_at: message.at / 1000, // Convert ms to seconds
        received_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0),
        mine: message.mine,
        pending: message.pending,
        failed: message.failed,
        wrapper_event_id: message.wrapper_event_id.clone(),
        npub: message.npub.clone(),
    }
}

/// Save multiple messages for a specific chat (batch operation)
///
/// This uses the flat event architecture - each message is stored as an event.
/// Reactions are stored as separate kind=7 events.
pub async fn save_chat_messages<R: Runtime>(
    handle: AppHandle<R>,
    chat_id: &str,
    messages: &[Message]
) -> Result<(), String> {
    // Skip if no messages to save
    if messages.is_empty() {
        return Ok(());
    }

    // Save each message using the event-based save_message function
    for message in messages {
        if let Err(e) = save_message(handle.clone(), chat_id, message).await {
            eprintln!("Failed to save message {}: {}", &message.id[..8.min(message.id.len())], e);
        }
    }

    Ok(())
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
    use crate::stored_event::event_kind;

    let mut index: HashMap<String, AttachmentRef> = HashMap::new();

    let conn = crate::account_manager::get_db_connection(handle)?;

    // Query file attachment events (kind=15) from the events table
    // Attachments are stored in the tags field as JSON
    let attachment_data: Vec<(String, String, String)> = {
        let mut stmt = conn.prepare(
            "SELECT e.id, c.chat_identifier, e.tags
             FROM events e
             JOIN chats c ON e.chat_id = c.id
             WHERE e.kind = ?1"
        ).map_err(|e| format!("Failed to prepare attachment query: {}", e))?;

        let rows = stmt.query_map(rusqlite::params![event_kind::FILE_ATTACHMENT], |row| {
            Ok((
                row.get::<_, String>(0)?, // event_id (message_id)
                row.get::<_, String>(1)?, // chat_identifier
                row.get::<_, String>(2)?, // tags JSON
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
    for (message_id, chat_id, tags_json) in attachment_data {
        // Parse tags to find the "attachments" tag
        let tags: Vec<Vec<String>> = serde_json::from_str(&tags_json).unwrap_or_default();

        // Find the attachments tag: ["attachments", "<json>"]
        let attachments_json = tags.iter()
            .find(|tag| tag.first().map(|s| s.as_str()) == Some("attachments"))
            .and_then(|tag| tag.get(1))
            .map(|s| s.as_str())
            .unwrap_or("[]");

        // Parse the attachments JSON
        let attachments: Vec<crate::Attachment> = serde_json::from_str(attachments_json)
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
/// NOTE: This now uses the events table via get_message_views
pub async fn get_chat_messages_paginated<R: Runtime>(
    handle: &AppHandle<R>,
    chat_id: &str,
    limit: usize,
    offset: usize,
) -> Result<Vec<Message>, String> {
    // Get integer chat ID
    let chat_int_id = get_chat_id_by_identifier(handle, chat_id)?;
    // Use the events-based message views
    get_message_views(handle, chat_int_id, limit, offset).await
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

    // Count message events (kind 14 = DM, kind 15 = file) from events table
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM events WHERE chat_id = ?1 AND kind IN (14, 15)",
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
    // Get integer chat ID
    let chat_int_id = get_chat_id_by_identifier(handle, chat_id)?;
    // Use the events-based message views
    get_message_views(handle, chat_int_id, count, 0).await
}

/// Get messages around a specific message ID
/// Returns messages from (target - context_before) to the most recent
/// This is used for scrolling to old replied-to messages
pub async fn get_messages_around_id<R: Runtime>(
    handle: &AppHandle<R>,
    chat_id: &str,
    target_message_id: &str,
    context_before: usize,
) -> Result<Vec<Message>, String> {
    let chat_int_id = get_chat_id_by_identifier(handle, chat_id)?;

    // First, find the timestamp of the target message (don't require chat_id match in case of edge cases)
    let target_timestamp: i64 = {
        let conn = crate::account_manager::get_db_connection(handle)?;
        // Try to find in the specified chat first
        let ts_result = conn.query_row(
            "SELECT created_at FROM events WHERE id = ?1 AND chat_id = ?2",
            rusqlite::params![target_message_id, chat_int_id],
            |row| row.get(0)
        );

        let ts = match ts_result {
            Ok(t) => t,
            Err(_) => {
                // Message not found in specified chat, try finding it anywhere
                conn.query_row(
                    "SELECT created_at FROM events WHERE id = ?1",
                    rusqlite::params![target_message_id],
                    |row| row.get(0)
                ).map_err(|e| format!("Target message not found in any chat: {}", e))?
            }
        };
        crate::account_manager::return_db_connection(conn);
        ts
    };

    // Count how many messages are older than the target in this chat
    let older_count: i64 = {
        let conn = crate::account_manager::get_db_connection(handle)?;
        let count = conn.query_row(
            "SELECT COUNT(*) FROM events WHERE chat_id = ?1 AND kind IN (14, 15) AND created_at < ?2",
            rusqlite::params![chat_int_id, target_timestamp],
            |row| row.get(0)
        ).map_err(|e| format!("Failed to count older messages: {}", e))?;
        crate::account_manager::return_db_connection(conn);
        count
    };

    // Get total message count for this chat
    let total_count = get_chat_message_count(handle, chat_id).await?;

    // Calculate the starting position (from oldest = 0)
    // We want messages from (target - context_before) to the newest
    let start_position = (older_count as usize).saturating_sub(context_before);

    // get_message_views uses ORDER BY created_at DESC, so:
    // - offset 0 = newest message
    // - To get messages from position P to newest with DESC ordering, use offset=0, limit=(total - P)
    let limit = total_count.saturating_sub(start_position);

    // offset = 0 to start from the newest and get all messages back to start_position
    get_message_views(handle, chat_int_id, limit, 0).await
}

/// Check if a message/event exists in the database by its ID
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

    // Check in events table (unified storage)
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM events WHERE id = ?1)",
        rusqlite::params![message_id],
        |row| row.get(0)
    ).map_err(|e| format!("Failed to check event existence: {}", e))?;

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

    // Check in events table (unified storage)
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM events WHERE wrapper_event_id = ?1)",
        rusqlite::params![wrapper_event_id],
        |row| row.get(0)
    ).map_err(|e| format!("Failed to check wrapper event existence: {}", e))?;

    crate::account_manager::return_db_connection(conn);

    Ok(exists)
}

/// Update the wrapper event ID for an existing event
/// This is called when we process an event that was previously stored without its wrapper ID
/// Returns: Ok(true) if updated, Ok(false) if event already had a wrapper_id (duplicate giftwrap)
pub async fn update_wrapper_event_id<R: Runtime>(
    handle: &AppHandle<R>,
    event_id: &str,
    wrapper_event_id: &str,
) -> Result<bool, String> {
    // Try to get a database connection - if it fails, we're not using DB mode
    let conn = match crate::account_manager::get_db_connection(handle) {
        Ok(c) => c,
        Err(_) => return Ok(false), // No DB, nothing to update
    };

    // Update in events table (unified storage)
    let rows_updated = match conn.execute(
        "UPDATE events SET wrapper_event_id = ?1 WHERE id = ?2 AND (wrapper_event_id IS NULL OR wrapper_event_id = '')",
        rusqlite::params![wrapper_event_id, event_id],
    ) {
        Ok(n) => n,
        Err(e) => {
            crate::account_manager::return_db_connection(conn);
            return Err(format!("Failed to update wrapper event ID: {}", e));
        }
    };

    crate::account_manager::return_db_connection(conn);

    // Returns true if backfill succeeded, false if event already has a wrapper_id (duplicate giftwrap)
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

    // Calculate timestamp for N days ago (in seconds, matching events.created_at)
    let cutoff_secs = (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs())
        .saturating_sub(days * 24 * 60 * 60);

    // Query all wrapper_event_ids from recent events
    let result: Result<Vec<String>, _> = {
        let mut stmt = conn.prepare(
            "SELECT wrapper_event_id FROM events
             WHERE wrapper_event_id IS NOT NULL
             AND wrapper_event_id != ''
             AND created_at >= ?1"
        ).map_err(|e| format!("Failed to prepare wrapper_id query: {}", e))?;

        let rows = stmt.query_map(rusqlite::params![cutoff_secs as i64], |row| {
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
    _chat_id: &str,  // No longer needed - we query by event ID directly
    msg_id: &str,
    attachment_id: &str,
    downloaded: bool,
    path: &str,
) -> Result<(), String> {
    let conn = crate::account_manager::get_db_connection(handle)?;

    // Get the current tags JSON from the events table
    let tags_json: String = conn.query_row(
        "SELECT tags FROM events WHERE id = ?1",
        rusqlite::params![msg_id],
        |row| row.get(0)
    ).map_err(|e| format!("Event not found: {}", e))?;

    // Parse the tags
    let mut tags: Vec<Vec<String>> = serde_json::from_str(&tags_json).unwrap_or_default();

    // Find the "attachments" tag
    let attachments_tag_idx = tags.iter().position(|tag| {
        tag.first().map(|s| s.as_str()) == Some("attachments")
    });

    let attachments_json = attachments_tag_idx
        .and_then(|idx| tags.get(idx))
        .and_then(|tag| tag.get(1))
        .map(|s| s.as_str())
        .unwrap_or("[]");

    // Parse and update the attachment
    let mut attachments: Vec<Attachment> = serde_json::from_str(attachments_json).unwrap_or_default();

    if let Some(att) = attachments.iter_mut().find(|a| a.id == attachment_id) {
        att.downloaded = downloaded;
        att.downloading = false;
        att.path = path.to_string();
    } else {
        crate::account_manager::return_db_connection(conn);
        return Err("Attachment not found in event".to_string());
    }

    // Serialize the updated attachments back to JSON
    let updated_attachments_json = serde_json::to_string(&attachments)
        .map_err(|e| format!("Failed to serialize attachments: {}", e))?;

    // Update the tags array - either update existing "attachments" tag or add new one
    if let Some(idx) = attachments_tag_idx {
        tags[idx] = vec!["attachments".to_string(), updated_attachments_json];
    } else {
        tags.push(vec!["attachments".to_string(), updated_attachments_json]);
    }

    // Serialize the tags back to JSON
    let updated_tags_json = serde_json::to_string(&tags)
        .map_err(|e| format!("Failed to serialize tags: {}", e))?;

    // Update the event in the database
    conn.execute(
        "UPDATE events SET tags = ?1 WHERE id = ?2",
        rusqlite::params![updated_tags_json, msg_id],
    ).map_err(|e| format!("Failed to update event: {}", e))?;

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

// ============================================================================
// Mini App Permissions Functions
// ============================================================================

/// Get all granted permissions for a Mini App by its file hash
/// Returns a comma-separated string of granted permission names
pub fn get_miniapp_granted_permissions<R: Runtime>(
    handle: &AppHandle<R>,
    file_hash: &str,
) -> Result<String, String> {
    let conn = crate::account_manager::get_db_connection(handle)?;

    let mut stmt = conn.prepare(
        "SELECT permission FROM miniapp_permissions WHERE file_hash = ?1 AND granted = 1"
    ).map_err(|e| format!("Failed to prepare permission query: {}", e))?;

    let permissions: Vec<String> = stmt.query_map(rusqlite::params![file_hash], |row| {
        row.get::<_, String>(0)
    })
    .map_err(|e| format!("Failed to query permissions: {}", e))?
    .filter_map(|r| r.ok())
    .collect();

    drop(stmt);
    crate::account_manager::return_db_connection(conn);

    Ok(permissions.join(","))
}

/// Set the granted status of a permission for a Mini App by its file hash
pub fn set_miniapp_permission<R: Runtime>(
    handle: &AppHandle<R>,
    file_hash: &str,
    permission: &str,
    granted: bool,
) -> Result<(), String> {
    let conn = crate::account_manager::get_db_connection(handle)?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;

    conn.execute(
        r#"
        INSERT INTO miniapp_permissions (file_hash, permission, granted, granted_at)
        VALUES (?1, ?2, ?3, ?4)
        ON CONFLICT(file_hash, permission) DO UPDATE SET
            granted = excluded.granted,
            granted_at = CASE WHEN excluded.granted = 1 THEN excluded.granted_at ELSE granted_at END
        "#,
        rusqlite::params![file_hash, permission, granted as i32, if granted { Some(now) } else { None::<i64> }],
    ).map_err(|e| format!("Failed to set Mini App permission: {}", e))?;

    crate::account_manager::return_db_connection(conn);
    Ok(())
}

/// Set multiple permissions at once for a Mini App by its file hash
/// Uses a transaction to ensure all permissions are set atomically
pub fn set_miniapp_permissions<R: Runtime>(
    handle: &AppHandle<R>,
    file_hash: &str,
    permissions: &[(&str, bool)],
) -> Result<(), String> {
    let mut conn = crate::account_manager::get_db_connection(handle)?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;

    let tx = conn.transaction()
        .map_err(|e| format!("Failed to start transaction: {}", e))?;

    for (permission, granted) in permissions {
        tx.execute(
            r#"
            INSERT INTO miniapp_permissions (file_hash, permission, granted, granted_at)
            VALUES (?1, ?2, ?3, ?4)
            ON CONFLICT(file_hash, permission) DO UPDATE SET
                granted = excluded.granted,
                granted_at = CASE WHEN excluded.granted = 1 THEN excluded.granted_at ELSE granted_at END
            "#,
            rusqlite::params![file_hash, permission, *granted as i32, if *granted { Some(now) } else { None::<i64> }],
        ).map_err(|e| format!("Failed to set Mini App permission: {}", e))?;
    }

    tx.commit()
        .map_err(|e| format!("Failed to commit permission changes: {}", e))?;

    crate::account_manager::return_db_connection(conn);
    Ok(())
}

/// Check if an app has been prompted for permissions yet (by file hash)
/// Returns true if any permission record exists for this hash
pub fn has_miniapp_permission_prompt<R: Runtime>(
    handle: &AppHandle<R>,
    file_hash: &str,
) -> Result<bool, String> {
    let conn = crate::account_manager::get_db_connection(handle)?;

    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM miniapp_permissions WHERE file_hash = ?1)",
        rusqlite::params![file_hash],
        |row| row.get(0)
    ).map_err(|e| format!("Failed to check permission prompt: {}", e))?;

    crate::account_manager::return_db_connection(conn);
    Ok(exists)
}

/// Revoke all permissions for a Mini App by its file hash
pub fn revoke_all_miniapp_permissions<R: Runtime>(
    handle: &AppHandle<R>,
    file_hash: &str,
) -> Result<(), String> {
    let conn = crate::account_manager::get_db_connection(handle)?;

    conn.execute(
        "DELETE FROM miniapp_permissions WHERE file_hash = ?1",
        rusqlite::params![file_hash],
    ).map_err(|e| format!("Failed to revoke Mini App permissions: {}", e))?;

    crate::account_manager::return_db_connection(conn);
    Ok(())
}

/// Copy all permissions from one file hash to another (for app updates)
pub fn copy_miniapp_permissions<R: Runtime>(
    handle: &AppHandle<R>,
    old_hash: &str,
    new_hash: &str,
) -> Result<(), String> {
    let conn = crate::account_manager::get_db_connection(handle)?;

    // Copy all permission records from old hash to new hash
    conn.execute(
        r#"
        INSERT OR REPLACE INTO miniapp_permissions (file_hash, permission, granted, granted_at)
        SELECT ?2, permission, granted, granted_at
        FROM miniapp_permissions
        WHERE file_hash = ?1
        "#,
        rusqlite::params![old_hash, new_hash],
    ).map_err(|e| format!("Failed to copy Mini App permissions: {}", e))?;

    crate::account_manager::return_db_connection(conn);
    Ok(())
}

// ============================================================================
// Event Storage Functions (Flat Event-Based Architecture)
// ============================================================================

/// Save a StoredEvent to the events table
///
/// This is the primary storage function for the flat event architecture.
/// All events (messages, reactions, attachments, etc.) are stored as flat rows.
pub async fn save_event<R: Runtime>(
    handle: &AppHandle<R>,
    event: &StoredEvent,
) -> Result<(), String> {
    let conn = crate::account_manager::get_db_connection(handle)?;

    // Serialize tags to JSON
    let tags_json = serde_json::to_string(&event.tags)
        .unwrap_or_else(|_| "[]".to_string());

    // For message and edit events, encrypt the content
    let content = if event.kind == event_kind::PRIVATE_DIRECT_MESSAGE
        || event.kind == event_kind::MESSAGE_EDIT
    {
        internal_encrypt(event.content.clone(), None).await
    } else {
        event.content.clone()
    };

    // Use INSERT OR IGNORE to skip if event already exists (deduplication)
    conn.execute(
        r#"
        INSERT OR IGNORE INTO events (
            id, kind, chat_id, user_id, content, tags, reference_id,
            created_at, received_at, mine, pending, failed, wrapper_event_id, npub
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
        "#,
        rusqlite::params![
            event.id,
            event.kind as i32,
            event.chat_id,
            event.user_id,
            content,
            tags_json,
            event.reference_id,
            event.created_at as i64,
            event.received_at as i64,
            event.mine as i32,
            event.pending as i32,
            event.failed as i32,
            event.wrapper_event_id,
            event.npub,
        ],
    ).map_err(|e| format!("Failed to save event: {}", e))?;

    crate::account_manager::return_db_connection(conn);
    Ok(())
}

/// Save a PIVX payment event to the events table
///
/// Resolves the chat_id from the conversation identifier and saves the event.
pub async fn save_pivx_payment_event<R: Runtime>(
    handle: &AppHandle<R>,
    conversation_id: &str,
    mut event: StoredEvent,
) -> Result<(), String> {
    // Resolve chat_id from conversation identifier
    let chat_id = get_or_create_chat_id(handle, conversation_id)?;
    event.chat_id = chat_id;

    // Save the event
    save_event(handle, &event).await
}

/// Get PIVX payment events for a chat
///
/// Returns all PIVX payment events (kind 30078 with d=pivx-payment tag) for a conversation.
pub async fn get_pivx_payments_for_chat<R: Runtime>(
    handle: &AppHandle<R>,
    conversation_id: &str,
) -> Result<Vec<StoredEvent>, String> {
    let conn = crate::account_manager::get_db_connection(handle)?;

    // Get chat_id from conversation identifier
    let chat_id: i64 = conn.query_row(
        "SELECT id FROM chats WHERE chat_identifier = ?1",
        rusqlite::params![conversation_id],
        |row| row.get(0)
    ).map_err(|_| "Chat not found")?;

    // Query events with kind=30078 and check for pivx-payment tag
    let payments = {
        let mut stmt = conn.prepare(
            r#"
            SELECT id, kind, chat_id, user_id, content, tags, reference_id,
                   created_at, received_at, mine, pending, failed, wrapper_event_id, npub
            FROM events
            WHERE chat_id = ?1 AND kind = ?2
            ORDER BY created_at ASC
            "#
        ).map_err(|e| format!("Failed to prepare statement: {}", e))?;

        let rows = stmt.query_map(
            rusqlite::params![chat_id, event_kind::APPLICATION_SPECIFIC as i32],
            |row| {
                let tags_json: String = row.get(5)?;
                let tags: Vec<Vec<String>> = serde_json::from_str(&tags_json).unwrap_or_default();

                Ok(StoredEvent {
                    id: row.get(0)?,
                    kind: row.get::<_, i32>(1)? as u16,
                    chat_id: row.get(2)?,
                    user_id: row.get(3)?,
                    content: row.get(4)?,
                    tags,
                    reference_id: row.get(6)?,
                    created_at: row.get::<_, i64>(7)? as u64,
                    received_at: row.get::<_, i64>(8)? as u64,
                    mine: row.get::<_, i32>(9)? != 0,
                    pending: row.get::<_, i32>(10)? != 0,
                    failed: row.get::<_, i32>(11)? != 0,
                    wrapper_event_id: row.get(12)?,
                    npub: row.get(13)?,
                })
            }
        ).map_err(|e| format!("Failed to query events: {}", e))?;

        // Filter for PIVX payment events (d tag = "pivx-payment")
        let mut payments = Vec::new();
        for row in rows {
            let event = row.map_err(|e| format!("Failed to read event: {}", e))?;
            // Check if this is a PIVX payment (has d=pivx-payment tag)
            let is_pivx = event.tags.iter().any(|tag| {
                tag.len() >= 2 && tag[0] == "d" && tag[1] == "pivx-payment"
            });
            if is_pivx {
                payments.push(event);
            }
        }
        payments
    }; // stmt dropped here, releasing borrow on conn

    crate::account_manager::return_db_connection(conn);
    Ok(payments)
}

/// Save a reaction as a kind=7 event in the events table
///
/// Reactions are stored as separate events referencing the message they react to.
/// This is the Nostr-standard way to store reactions (NIP-25).
pub async fn save_reaction_event<R: Runtime>(
    handle: &AppHandle<R>,
    reaction: &Reaction,
    chat_id: i64,
    user_id: Option<i64>,
    mine: bool,
) -> Result<(), String> {
    let event = StoredEvent {
        id: reaction.id.clone(),
        kind: event_kind::REACTION,
        chat_id,
        user_id,
        content: reaction.emoji.clone(),
        tags: vec![
            vec!["e".to_string(), reaction.reference_id.clone()],
        ],
        reference_id: Some(reaction.reference_id.clone()),
        created_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        received_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0),
        mine,
        pending: false,
        failed: false,
        wrapper_event_id: None,
        npub: Some(reaction.author_id.clone()),
    };

    save_event(handle, &event).await
}

/// Save a message edit as a kind=16 event in the events table
///
/// Edit events reference the original message and contain the new content.
/// The content is encrypted just like DM content.
pub async fn save_edit_event<R: Runtime>(
    handle: &AppHandle<R>,
    edit_id: &str,
    message_id: &str,
    new_content: &str,
    chat_id: i64,
    user_id: Option<i64>,
    npub: &str,
) -> Result<(), String> {
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);

    let event = StoredEvent {
        id: edit_id.to_string(),
        kind: event_kind::MESSAGE_EDIT,
        chat_id,
        user_id,
        content: new_content.to_string(),
        tags: vec![
            vec!["e".to_string(), message_id.to_string(), "".to_string(), "edit".to_string()],
        ],
        reference_id: Some(message_id.to_string()),
        created_at: now_secs,
        received_at: now_ms,
        mine: true,
        pending: false,
        failed: false,
        wrapper_event_id: None,
        npub: Some(npub.to_string()),
    };

    save_event(handle, &event).await
}

/// Check if an event exists in the events table
pub fn event_exists<R: Runtime>(
    handle: &AppHandle<R>,
    event_id: &str,
) -> Result<bool, String> {
    let conn = crate::account_manager::get_db_connection(handle)?;

    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM events WHERE id = ?1)",
        rusqlite::params![event_id],
        |row| row.get(0),
    ).map_err(|e| format!("Failed to check event existence: {}", e))?;

    crate::account_manager::return_db_connection(conn);
    Ok(exists)
}

/// Check if an event exists by wrapper event ID (for deduplication during sync)
pub fn event_exists_by_wrapper<R: Runtime>(
    handle: &AppHandle<R>,
    wrapper_event_id: &str,
) -> Result<bool, String> {
    let conn = crate::account_manager::get_db_connection(handle)?;

    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM events WHERE wrapper_event_id = ?1)",
        rusqlite::params![wrapper_event_id],
        |row| row.get(0),
    ).map_err(|e| format!("Failed to check event by wrapper: {}", e))?;

    crate::account_manager::return_db_connection(conn);
    Ok(exists)
}

/// Get events for a chat with pagination
///
/// Returns events ordered by created_at descending (newest first).
/// Optionally filter by event kinds.
pub async fn get_events<R: Runtime>(
    handle: &AppHandle<R>,
    chat_id: i64,
    kinds: Option<&[u16]>,
    limit: usize,
    offset: usize,
) -> Result<Vec<StoredEvent>, String> {
    // Do all SQLite work synchronously in a block to avoid Send issues
    let events: Vec<StoredEvent> = {
        let conn = crate::account_manager::get_db_connection(handle)?;

        // Build query based on whether kinds filter is provided
        let events = if let Some(k) = kinds {
            // Build numbered placeholders for the IN clause
            // chat_id=?1, kinds=?2,?3,..., limit=?N, offset=?N+1
            let kind_placeholders: String = (0..k.len())
                .map(|i| format!("?{}", i + 2))
                .collect::<Vec<_>>()
                .join(",");
            let limit_param = k.len() + 2;
            let offset_param = k.len() + 3;

            let sql = format!(
                r#"
                SELECT id, kind, chat_id, user_id, content, tags, reference_id,
                       created_at, received_at, mine, pending, failed, wrapper_event_id, npub
                FROM events
                WHERE chat_id = ?1 AND kind IN ({})
                ORDER BY created_at DESC
                LIMIT ?{} OFFSET ?{}
                "#,
                kind_placeholders, limit_param, offset_param
            );

            let mut stmt = conn.prepare(&sql)
                .map_err(|e| format!("Failed to prepare events query: {}", e))?;

            // Use rusqlite params! macro with dynamic kinds
            let result: Vec<StoredEvent> = match k.len() {
                1 => {
                    let rows = stmt.query_map(
                        rusqlite::params![chat_id, k[0] as i32, limit as i64, offset as i64],
                        parse_event_row
                    ).map_err(|e| format!("Failed to query events: {}", e))?;
                    rows.filter_map(|r| r.ok()).collect()
                },
                2 => {
                    let rows = stmt.query_map(
                        rusqlite::params![chat_id, k[0] as i32, k[1] as i32, limit as i64, offset as i64],
                        parse_event_row
                    ).map_err(|e| format!("Failed to query events: {}", e))?;
                    rows.filter_map(|r| r.ok()).collect()
                },
                _ => return Err("Unsupported number of kinds".to_string()),
            };

            drop(stmt);
            result
        } else {
            let sql = r#"
                SELECT id, kind, chat_id, user_id, content, tags, reference_id,
                       created_at, received_at, mine, pending, failed, wrapper_event_id, npub
                FROM events
                WHERE chat_id = ?1
                ORDER BY created_at DESC
                LIMIT ?2 OFFSET ?3
            "#;

            let mut stmt = conn.prepare(sql)
                .map_err(|e| format!("Failed to prepare events query: {}", e))?;

            let rows = stmt.query_map(
                rusqlite::params![chat_id, limit as i64, offset as i64],
                parse_event_row
            ).map_err(|e| format!("Failed to query events: {}", e))?;
            let result: Vec<StoredEvent> = rows.filter_map(|r| r.ok()).collect();

            drop(stmt);
            result
        };

        crate::account_manager::return_db_connection(conn);
        events
    };

    // Decrypt message content for text messages (this is the async part)
    let mut decrypted_events = Vec::with_capacity(events.len());
    for mut event in events {
        if event.kind == event_kind::PRIVATE_DIRECT_MESSAGE {
            event.content = internal_decrypt(event.content, None).await
                .unwrap_or_else(|_| "[Decryption failed]".to_string());
        }
        decrypted_events.push(event);
    }

    Ok(decrypted_events)
}

/// Helper function to parse a row into a StoredEvent
fn parse_event_row(row: &rusqlite::Row) -> rusqlite::Result<StoredEvent> {
    let tags_json: String = row.get(5)?;
    let tags: Vec<Vec<String>> = serde_json::from_str(&tags_json).unwrap_or_default();

    Ok(StoredEvent {
        id: row.get(0)?,
        kind: row.get::<_, i32>(1)? as u16,
        chat_id: row.get(2)?,
        user_id: row.get(3)?,
        content: row.get(4)?,
        tags,
        reference_id: row.get(6)?,
        created_at: row.get::<_, i64>(7)? as u64,
        received_at: row.get::<_, i64>(8)? as u64,
        mine: row.get::<_, i32>(9)? != 0,
        pending: row.get::<_, i32>(10)? != 0,
        failed: row.get::<_, i32>(11)? != 0,
        wrapper_event_id: row.get(12)?,
        npub: row.get(13)?,
    })
}

/// Get events that reference specific message IDs
///
/// Used to fetch reactions and attachments for a set of messages.
/// This is the core function for building materialized views.
pub async fn get_related_events<R: Runtime>(
    handle: &AppHandle<R>,
    reference_ids: &[String],
) -> Result<Vec<StoredEvent>, String> {
    if reference_ids.is_empty() {
        return Ok(Vec::new());
    }

    let conn = crate::account_manager::get_db_connection(handle)?;

    let placeholders: String = reference_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!(
        r#"
        SELECT id, kind, chat_id, user_id, content, tags, reference_id,
               created_at, received_at, mine, pending, failed, wrapper_event_id, npub
        FROM events
        WHERE reference_id IN ({})
        ORDER BY created_at ASC
        "#,
        placeholders
    );

    let mut stmt = conn.prepare(&sql)
        .map_err(|e| format!("Failed to prepare related events query: {}", e))?;

    let params: Vec<&dyn rusqlite::ToSql> = reference_ids.iter()
        .map(|s| s as &dyn rusqlite::ToSql)
        .collect();

    let events: Vec<StoredEvent> = stmt.query_map(params.as_slice(), |row| {
        let tags_json: String = row.get(5)?;
        let tags: Vec<Vec<String>> = serde_json::from_str(&tags_json).unwrap_or_default();

        Ok(StoredEvent {
            id: row.get(0)?,
            kind: row.get::<_, i32>(1)? as u16,
            chat_id: row.get(2)?,
            user_id: row.get(3)?,
            content: row.get(4)?,
            tags,
            reference_id: row.get(6)?,
            created_at: row.get::<_, i64>(7)? as u64,
            received_at: row.get::<_, i64>(8)? as u64,
            mine: row.get::<_, i32>(9)? != 0,
            pending: row.get::<_, i32>(10)? != 0,
            failed: row.get::<_, i32>(11)? != 0,
            wrapper_event_id: row.get(12)?,
            npub: row.get(13)?,
        })
    })
    .map_err(|e| format!("Failed to query related events: {}", e))?
    .filter_map(|r| r.ok())
    .collect();

    // Drop statement to release borrow on conn
    drop(stmt);
    crate::account_manager::return_db_connection(conn);
    Ok(events)
}

/// Context data for a replied-to message
struct ReplyContext {
    content: String,
    npub: Option<String>,
    has_attachment: bool,
}

/// Fetch reply context for a list of message IDs
/// Returns a HashMap of message_id -> ReplyContext
async fn get_reply_contexts<R: Runtime>(
    handle: &AppHandle<R>,
    message_ids: &[String],
) -> Result<HashMap<String, ReplyContext>, String> {
    if message_ids.is_empty() {
        return Ok(HashMap::new());
    }

    // Do all SQLite work synchronously in a block to avoid Send issues
    let (events, edits): (Vec<(String, i32, String, Option<String>)>, Vec<(String, String)>) = {
        let conn = crate::account_manager::get_db_connection(handle)?;

        // Build placeholders for IN clause
        let placeholders: String = (0..message_ids.len())
            .map(|i| format!("?{}", i + 1))
            .collect::<Vec<_>>()
            .join(",");

        // Query original messages
        let sql = format!(
            r#"
            SELECT id, kind, content, npub
            FROM events
            WHERE id IN ({})
            "#,
            placeholders
        );

        let mut stmt = conn.prepare(&sql)
            .map_err(|e| format!("Failed to prepare reply context query: {}", e))?;

        // Build params as String refs for the query
        let params: Vec<&str> = message_ids.iter().map(|s| s.as_str()).collect();
        let params_dyn: Vec<&dyn rusqlite::ToSql> = params.iter().map(|s| s as &dyn rusqlite::ToSql).collect();

        let rows = stmt.query_map(params_dyn.as_slice(), |row| {
            Ok((
                row.get::<_, String>(0)?, // id
                row.get::<_, i32>(1)?,    // kind
                row.get::<_, String>(2)?, // content
                row.get::<_, Option<String>>(3)?, // npub
            ))
        }).map_err(|e| format!("Failed to query reply contexts: {}", e))?;

        let events_result: Vec<(String, i32, String, Option<String>)> = rows.filter_map(|r| r.ok()).collect();
        drop(stmt);

        // Query latest edits for these messages (most recent edit per message)
        let edit_sql = format!(
            r#"
            SELECT reference_id, content
            FROM events
            WHERE kind = {} AND reference_id IN ({})
            ORDER BY created_at DESC
            "#,
            event_kind::MESSAGE_EDIT,
            placeholders
        );

        let mut edit_stmt = conn.prepare(&edit_sql)
            .map_err(|e| format!("Failed to prepare edit query: {}", e))?;

        let edit_rows = edit_stmt.query_map(params_dyn.as_slice(), |row| {
            Ok((
                row.get::<_, String>(0)?, // reference_id (original message id)
                row.get::<_, String>(1)?, // content (edited content)
            ))
        }).map_err(|e| format!("Failed to query edits: {}", e))?;

        let edits_result: Vec<(String, String)> = edit_rows.filter_map(|r| r.ok()).collect();
        drop(edit_stmt);

        crate::account_manager::return_db_connection(conn);
        (events_result, edits_result)
    };

    // Build a map of message_id -> latest edit content (first one since ordered DESC)
    let mut latest_edits: HashMap<String, String> = HashMap::new();
    for (ref_id, content) in edits {
        // Only keep the first (most recent) edit for each message
        latest_edits.entry(ref_id).or_insert(content);
    }

    // Process events and decrypt content (async part)
    let mut contexts = HashMap::new();
    for (id, kind, original_content, npub) in events {
        let has_attachment = kind == event_kind::FILE_ATTACHMENT as i32;

        // Use latest edit content if available, otherwise use original
        let content_to_decrypt = latest_edits.get(&id).cloned().unwrap_or(original_content);

        // Decrypt content for text messages
        let decrypted_content = if kind == event_kind::PRIVATE_DIRECT_MESSAGE as i32 {
            internal_decrypt(content_to_decrypt, None).await
                .unwrap_or_else(|_| "[Decryption failed]".to_string())
        } else {
            // File attachments don't have displayable content
            String::new()
        };

        contexts.insert(id, ReplyContext {
            content: decrypted_content,
            npub,
            has_attachment,
        });
    }

    Ok(contexts)
}

/// Populate reply context for a single message before emitting to frontend
/// This is used for real-time messages that don't go through get_message_views
pub async fn populate_reply_context<R: Runtime>(
    handle: &AppHandle<R>,
    message: &mut Message,
) -> Result<(), String> {
    if message.replied_to.is_empty() {
        return Ok(());
    }

    let contexts = get_reply_contexts(handle, &[message.replied_to.clone()]).await?;

    if let Some(ctx) = contexts.get(&message.replied_to) {
        message.replied_to_content = Some(ctx.content.clone());
        message.replied_to_npub = ctx.npub.clone();
        message.replied_to_has_attachment = Some(ctx.has_attachment);
    }

    Ok(())
}

/// Get message events with their reactions composed (materialized view)
///
/// This function performs a single efficient query to get messages and their
/// related events, then composes them into Message structs for the frontend.
pub async fn get_message_views<R: Runtime>(
    handle: &AppHandle<R>,
    chat_id: i64,
    limit: usize,
    offset: usize,
) -> Result<Vec<Message>, String> {
    // Step 1: Get message events (kind 14 and 15)
    let message_kinds = [event_kind::PRIVATE_DIRECT_MESSAGE, event_kind::FILE_ATTACHMENT];
    let message_events = get_events(handle, chat_id, Some(&message_kinds), limit, offset).await?;

    if message_events.is_empty() {
        return Ok(Vec::new());
    }

    // Step 2: Get related events (reactions, edits) for these messages
    let message_ids: Vec<String> = message_events.iter().map(|e| e.id.clone()).collect();
    let related_events = get_related_events(handle, &message_ids).await?;

    // Group reactions and edits by message ID
    let mut reactions_by_msg: HashMap<String, Vec<Reaction>> = HashMap::new();
    let mut edits_by_msg: HashMap<String, Vec<(u64, String)>> = HashMap::new(); // (timestamp, content)

    for event in related_events {
        if let Some(ref_id) = &event.reference_id {
            match event.kind {
                k if k == event_kind::REACTION => {
                    let reaction = Reaction {
                        id: event.id.clone(),
                        reference_id: ref_id.clone(),
                        author_id: event.npub.clone().unwrap_or_default(),
                        emoji: event.content.clone(),
                    };
                    reactions_by_msg.entry(ref_id.clone()).or_default().push(reaction);
                }
                k if k == event_kind::MESSAGE_EDIT => {
                    // Edit content is encrypted, decrypt it here
                    let decrypted_content = internal_decrypt(event.content.clone(), None).await
                        .unwrap_or_else(|_| event.content.clone());
                    let timestamp_ms = event.created_at * 1000; // Convert to ms
                    edits_by_msg.entry(ref_id.clone()).or_default().push((timestamp_ms, decrypted_content));
                }
                _ => {}
            }
        }
    }

    // Sort edits by timestamp (chronologically)
    for edits in edits_by_msg.values_mut() {
        edits.sort_by_key(|(ts, _)| *ts);
    }

    // Step 3: Parse attachments from event tags OR fall back to messages table
    // New events have attachments in tags, old migrated events need messages table lookup
    let mut attachments_by_msg: HashMap<String, Vec<Attachment>> = HashMap::new();
    let mut events_needing_legacy_lookup: Vec<String> = Vec::new();

    for event in &message_events {
        if event.kind != event_kind::FILE_ATTACHMENT {
            continue;
        }

        // Try to parse attachments from the "attachments" tag (new events)
        if let Some(attachments_json) = event.get_tag("attachments") {
            if let Ok(attachments) = serde_json::from_str::<Vec<Attachment>>(attachments_json) {
                attachments_by_msg.insert(event.id.clone(), attachments);
                continue;
            }
        }

        // No attachments tag found - this is an old migrated event, need legacy lookup
        events_needing_legacy_lookup.push(event.id.clone());
    }

    // Fall back to messages table for old migrated events without attachments tag
    // NOTE: This is a legacy fallback - the messages table may have been dropped
    if !events_needing_legacy_lookup.is_empty() {
        let conn = crate::account_manager::get_db_connection(handle)?;

        // Check if messages table exists before querying it
        let has_messages_table: bool = conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='messages'",
            [],
            |row| row.get::<_, i32>(0)
        ).map(|count| count > 0).unwrap_or(false);

        if has_messages_table {
            for msg_id in &events_needing_legacy_lookup {
                if let Ok(attachments_json) = conn.query_row::<String, _, _>(
                    "SELECT attachments FROM messages WHERE id = ?1",
                    rusqlite::params![msg_id],
                    |row| row.get(0),
                ) {
                    if let Ok(attachments) = serde_json::from_str::<Vec<Attachment>>(&attachments_json) {
                        attachments_by_msg.insert(msg_id.to_string(), attachments);
                    }
                }
            }
        }
        crate::account_manager::return_db_connection(conn);
    }

    // Step 4: Compose Message structs (with decryption and edit application)
    let mut messages = Vec::with_capacity(message_events.len());
    for event in message_events {
        // Calculate derived values before moving ownership
        let replied_to = event.get_reply_reference().unwrap_or("").to_string();
        let at = event.timestamp_ms();
        let reactions = reactions_by_msg.remove(&event.id).unwrap_or_default();

        // Get attachments from the lookup map (for kind=15 file messages)
        let attachments = attachments_by_msg.remove(&event.id).unwrap_or_default();

        // Get original content (already decrypted by get_events())
        let original_content = if event.kind == event_kind::FILE_ATTACHMENT {
            // File attachment content is just an encrypted hash - don't display
            String::new()
        } else {
            // Content already decrypted by get_events()
            event.content.clone()
        };

        // Check for edits and build edit history
        let (content, edited, edit_history) = if let Some(edits) = edits_by_msg.remove(&event.id) {
            // Build edit history: original + all edits
            let mut history = Vec::with_capacity(edits.len() + 1);

            // Add original as first entry
            history.push(EditEntry {
                content: original_content.clone(),
                edited_at: at,
            });

            // Add all edits
            for (edit_ts, edit_content) in &edits {
                history.push(EditEntry {
                    content: edit_content.clone(),
                    edited_at: *edit_ts,
                });
            }

            // Use the latest edit's content
            let latest_content = edits.last()
                .map(|(_, c)| c.clone())
                .unwrap_or(original_content);

            (latest_content, true, Some(history))
        } else {
            (original_content, false, None)
        };

        let message = Message {
            id: event.id,
            content,
            replied_to,
            replied_to_content: None, // Populated below
            replied_to_npub: None,    // Populated below
            replied_to_has_attachment: None, // Populated below
            preview_metadata: None, // TODO: Parse from tags if needed
            attachments,
            reactions,
            at,
            pending: event.pending,
            failed: event.failed,
            mine: event.mine,
            npub: event.npub,
            wrapper_event_id: event.wrapper_event_id,
            edited,
            edit_history,
        };
        messages.push(message);
    }

    // Step 5: Fetch reply context for messages that have replies
    // Collect all replied_to IDs that are non-empty
    let reply_ids: Vec<String> = messages
        .iter()
        .filter(|m| !m.replied_to.is_empty())
        .map(|m| m.replied_to.clone())
        .collect();

    if !reply_ids.is_empty() {
        // Fetch the replied-to events from the database
        let reply_contexts = get_reply_contexts(handle, &reply_ids).await?;

        // Populate reply context for each message
        for message in &mut messages {
            if !message.replied_to.is_empty() {
                if let Some(ctx) = reply_contexts.get(&message.replied_to) {
                    message.replied_to_content = Some(ctx.content.clone());
                    message.replied_to_npub = ctx.npub.clone();
                    message.replied_to_has_attachment = Some(ctx.has_attachment);
                }
            }
        }
    }

    Ok(messages)
}

/// Update an event's pending/failed status
pub fn update_event_status<R: Runtime>(
    handle: &AppHandle<R>,
    event_id: &str,
    pending: bool,
    failed: bool,
) -> Result<(), String> {
    let conn = crate::account_manager::get_db_connection(handle)?;

    conn.execute(
        "UPDATE events SET pending = ?1, failed = ?2 WHERE id = ?3",
        rusqlite::params![pending as i32, failed as i32, event_id],
    ).map_err(|e| format!("Failed to update event status: {}", e))?;

    crate::account_manager::return_db_connection(conn);
    Ok(())
}

/// Delete an event by ID
pub fn delete_event<R: Runtime>(
    handle: &AppHandle<R>,
    event_id: &str,
) -> Result<(), String> {
    let conn = crate::account_manager::get_db_connection(handle)?;

    conn.execute(
        "DELETE FROM events WHERE id = ?1",
        rusqlite::params![event_id],
    ).map_err(|e| format!("Failed to delete event: {}", e))?;

    crate::account_manager::return_db_connection(conn);
    Ok(())
}

/// Get the total count of message events in a chat
pub fn get_message_count<R: Runtime>(
    handle: &AppHandle<R>,
    chat_id: i64,
) -> Result<i64, String> {
    let conn = crate::account_manager::get_db_connection(handle)?;

    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM events WHERE chat_id = ?1 AND kind IN (?2, ?3)",
        rusqlite::params![
            chat_id,
            event_kind::PRIVATE_DIRECT_MESSAGE as i32,
            event_kind::FILE_ATTACHMENT as i32
        ],
        |row| row.get(0),
    ).map_err(|e| format!("Failed to count messages: {}", e))?;

    crate::account_manager::return_db_connection(conn);
    Ok(count)
}

/// Get the storage version from settings
pub fn get_storage_version<R: Runtime>(
    handle: &AppHandle<R>,
) -> Result<i32, String> {
    let conn = crate::account_manager::get_db_connection(handle)?;

    let version: i32 = conn.query_row(
        "SELECT CAST(value AS INTEGER) FROM settings WHERE key = 'storage_version'",
        [],
        |row| row.get(0),
    ).unwrap_or(1); // Default to version 1 (old format)

    crate::account_manager::return_db_connection(conn);
    Ok(version)
}