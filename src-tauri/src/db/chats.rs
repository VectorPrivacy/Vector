//! Chat database operations.
//!
//! This module handles:
//! - SlimChatDB struct for efficient database storage
//! - Chat CRUD operations
//! - ID cache management for chats and users

use serde::{Deserialize, Serialize};

use crate::{Chat, ChatType};
use crate::message::compact::{encode_message_id, decode_message_id, NpubInterner};
use super::{CHAT_ID_CACHE, USER_ID_CACHE};

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
pub(crate) fn get_or_create_chat_id(
    chat_identifier: &str,
) -> Result<i64, String> {
    // Check cache first (fast path - no DB access)
    {
        let cache = CHAT_ID_CACHE.read().unwrap();
        if let Some(&id) = cache.get(chat_identifier) {
            return Ok(id);
        }
    }

    // Cache miss - uses write connection since we may INSERT
    // RAII guard ensures connection is returned even on error
    let conn = crate::account_manager::get_write_connection_guard_static()?;

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

    // Connection auto-returned by guard drop

    // Update cache with the ID
    {
        let mut cache = CHAT_ID_CACHE.write().unwrap();
        cache.insert(chat_identifier.to_string(), id);
    }

    Ok(id)
}

/// Helper function to get integer chat ID from identifier (lookup only, no creation)
/// Returns an error if the chat doesn't exist
pub fn get_chat_id_by_identifier(
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
    // RAII guard ensures connection is returned even on error
    let conn = crate::account_manager::get_db_connection_guard_static()?;

    let id: i64 = conn.query_row(
        "SELECT id FROM chats WHERE chat_identifier = ?1",
        rusqlite::params![chat_identifier],
        |row| row.get(0)
    ).map_err(|_| format!("Chat not found: {}", chat_identifier))?;

    // Connection auto-returned by guard drop

    // Update cache
    {
        let mut cache = CHAT_ID_CACHE.write().unwrap();
        cache.insert(chat_identifier.to_string(), id);
    }

    Ok(id)
}

/// Helper function to get or create integer user ID from npub
/// Uses in-memory cache for maximum speed, only hits DB on cache miss
pub(crate) fn get_or_create_user_id(
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
    // RAII guard ensures connection is returned even on error
    let conn = crate::account_manager::get_db_connection_guard_static()?;

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

    // Connection auto-returned by guard drop

    // Update cache with the ID (write to both DB and cache)
    {
        let mut cache = USER_ID_CACHE.write().unwrap();
        cache.insert(npub.to_string(), id);
    }

    Ok(Some(id))
}

/// Preload all ID mappings into memory cache on app startup
/// This ensures all subsequent lookups are instant (no DB access)
pub async fn preload_id_caches() -> Result<(), String> {
    let _npub = match crate::account_manager::get_current_account() {
        Ok(n) => n,
        Err(_) => return Ok(()), // No account selected, skip
    };

    // RAII guard ensures connection is returned even on error
    let conn = crate::account_manager::get_db_connection_guard_static()?;

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

    // Connection auto-returned by guard drop
    Ok(())
}

/// Clear ID caches (useful when switching accounts)
pub fn clear_id_caches() {
    CHAT_ID_CACHE.write().unwrap().clear();
    USER_ID_CACHE.write().unwrap().clear();
}

impl SlimChatDB {
    /// Create from a Chat, resolving interned handles to strings for DB storage
    pub fn from_chat(chat: &Chat, interner: &NpubInterner) -> Self {
        SlimChatDB {
            id: chat.id().clone(),
            chat_type: chat.chat_type().clone(),
            participants: chat.participants().iter()
                .filter_map(|&h| interner.resolve(h).map(|s| s.to_string()))
                .collect(),
            last_read: if *chat.last_read() == [0u8; 32] {
                String::new()
            } else {
                decode_message_id(chat.last_read())
            },
            created_at: chat.created_at(),
            metadata: chat.metadata().clone(),
            muted: chat.muted(),
        }
    }

    /// Convert back to full Chat (messages will be loaded separately)
    pub fn to_chat(&self, interner: &mut NpubInterner) -> Chat {
        let handles: Vec<u16> = self.participants.iter().map(|p| interner.intern(p)).collect();
        let mut chat = Chat::new(self.id.clone(), self.chat_type.clone(), handles);
        chat.last_read = if self.last_read.is_empty() {
            [0u8; 32]
        } else {
            encode_message_id(&self.last_read)
        };
        chat.created_at = self.created_at;
        chat.metadata = self.metadata.clone();
        chat.muted = self.muted;
        chat
    }
}

/// Get all chats from the database
pub async fn get_all_chats() -> Result<Vec<SlimChatDB>, String> {
    // RAII guard ensures connection is returned even on error
    let conn = crate::account_manager::get_db_connection_guard_static()?;

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

    // stmt and conn auto-dropped (conn guard returns to pool)
    Ok(chats)
}

/// Save a single chat to the database
///
/// Takes a pre-built `SlimChatDB` so callers can build it while holding STATE
/// (cheap — just metadata, no messages), drop the lock, then call this.
pub async fn save_slim_chat(slim_chat: SlimChatDB) -> Result<(), String> {
    // RAII guard ensures connection is returned even on error
    let conn = crate::account_manager::get_write_connection_guard_static()?;

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

    // Connection auto-returned by guard drop
    Ok(())
}

/// Delete a chat and all its messages from the database
pub async fn delete_chat(chat_id: &str) -> Result<(), String> {
    // RAII guard ensures connection is returned even on error
    let conn = crate::account_manager::get_write_connection_guard_static()?;

    conn.execute(
        "DELETE FROM chats WHERE id = ?1",
        rusqlite::params![chat_id],
    ).map_err(|e| format!("Failed to delete chat: {}", e))?;

    println!("[DB] Deleted chat and messages from SQL: {}", chat_id);

    // Connection auto-returned by guard drop
    Ok(())
}