//! ID cache — maps chat identifiers and npubs to SQLite row IDs.
//!
//! All lookups are cached in memory after first DB hit. Caches are
//! preloaded at boot and cleared on account switch.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, RwLock};

static CHAT_ID_CACHE: LazyLock<Arc<RwLock<HashMap<String, i64>>>> =
    LazyLock::new(|| Arc::new(RwLock::new(HashMap::new())));

static USER_ID_CACHE: LazyLock<Arc<RwLock<HashMap<String, i64>>>> =
    LazyLock::new(|| Arc::new(RwLock::new(HashMap::new())));

/// Lookup-only: get integer chat ID from identifier. Errors if not found.
pub fn get_chat_id_by_identifier(chat_identifier: &str) -> Result<i64, String> {
    // Fast path: cache hit
    {
        let cache = CHAT_ID_CACHE.read().unwrap();
        if let Some(&id) = cache.get(chat_identifier) {
            return Ok(id);
        }
    }

    // Cache miss: query DB
    let conn = super::get_db_connection_guard_static()?;
    let id: i64 = conn.query_row(
        "SELECT id FROM chats WHERE chat_identifier = ?1",
        rusqlite::params![chat_identifier],
        |row| row.get(0)
    ).map_err(|_| format!("Chat not found: {}", chat_identifier))?;

    // Update cache
    {
        let mut cache = CHAT_ID_CACHE.write().unwrap();
        cache.insert(chat_identifier.to_string(), id);
    }

    Ok(id)
}

/// Get or create integer chat ID from identifier.
pub fn get_or_create_chat_id(chat_identifier: &str) -> Result<i64, String> {
    // Fast path: cache hit
    {
        let cache = CHAT_ID_CACHE.read().unwrap();
        if let Some(&id) = cache.get(chat_identifier) {
            return Ok(id);
        }
    }

    let conn = super::get_db_connection_guard_static()?;

    // Try existing
    let existing: Option<i64> = conn.query_row(
        "SELECT id FROM chats WHERE chat_identifier = ?1",
        rusqlite::params![chat_identifier],
        |row| row.get(0)
    ).ok();

    let id = if let Some(id) = existing {
        id
    } else {
        // Create stub chat entry
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap()
            .as_secs() as i64;
        let chat_type: i32 = if chat_identifier.starts_with("npub1") { 0 } else { 1 };

        conn.execute(
            "INSERT INTO chats (chat_identifier, chat_type, participants, created_at) VALUES (?1, ?2, '[]', ?3)",
            rusqlite::params![chat_identifier, chat_type, now],
        ).map_err(|e| format!("Failed to create chat stub: {}", e))?;

        conn.last_insert_rowid()
    };

    // Update cache
    {
        let mut cache = CHAT_ID_CACHE.write().unwrap();
        cache.insert(chat_identifier.to_string(), id);
    }

    Ok(id)
}

/// Get or create integer user ID from npub. Returns None for empty npub.
pub fn get_or_create_user_id(npub: &str) -> Result<Option<i64>, String> {
    if npub.is_empty() {
        return Ok(None);
    }

    // Fast path: cache hit
    {
        let cache = USER_ID_CACHE.read().unwrap();
        if let Some(&id) = cache.get(npub) {
            return Ok(Some(id));
        }
    }

    let conn = super::get_db_connection_guard_static()?;

    let existing: Option<i64> = conn.query_row(
        "SELECT id FROM profiles WHERE npub = ?1",
        rusqlite::params![npub],
        |row| row.get(0)
    ).ok();

    let id = if let Some(id) = existing {
        id
    } else {
        conn.execute(
            "INSERT INTO profiles (npub, name, display_name) VALUES (?1, '', '')",
            rusqlite::params![npub],
        ).map_err(|e| format!("Failed to create profile stub: {}", e))?;
        conn.last_insert_rowid()
    };

    // Update cache
    {
        let mut cache = USER_ID_CACHE.write().unwrap();
        cache.insert(npub.to_string(), id);
    }

    Ok(Some(id))
}

/// Preload all ID mappings into memory cache (call at boot).
pub fn preload_id_caches() -> Result<(), String> {
    let conn = match super::get_db_connection_guard_static() {
        Ok(c) => c,
        Err(_) => return Ok(()), // No DB yet, skip
    };

    // Load chat ID mappings
    {
        let mut stmt = conn.prepare("SELECT chat_identifier, id FROM chats")
            .map_err(|e| format!("Failed to prepare chat query: {}", e))?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        }).map_err(|e| format!("Failed to query chats: {}", e))?;

        let mut cache = CHAT_ID_CACHE.write().unwrap();
        for row in rows.flatten() {
            cache.insert(row.0, row.1);
        }
    }

    // Load user ID mappings
    {
        let mut stmt = conn.prepare("SELECT npub, id FROM profiles")
            .map_err(|e| format!("Failed to prepare user query: {}", e))?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        }).map_err(|e| format!("Failed to query profiles: {}", e))?;

        let mut cache = USER_ID_CACHE.write().unwrap();
        for row in rows.flatten() {
            cache.insert(row.0, row.1);
        }
    }

    Ok(())
}

/// Clear all ID caches (call on account switch).
pub fn clear_id_caches() {
    CHAT_ID_CACHE.write().unwrap().clear();
    USER_ID_CACHE.write().unwrap().clear();
}
