//! ID cache — maps chat identifiers and npubs to SQLite row IDs.
//!
//! All lookups are cached in memory after first DB hit. Caches are
//! preloaded at boot and cleared on account switch.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

// Row ids belong to ONE account's database. Resolving a chat or npub against
// another account's cache returns a live id for the wrong row, which reads and
// writes then follow silently.
struct ChatIds;
struct UserIds;

fn chat_id_cache() -> Arc<RwLock<HashMap<String, i64>>> {
    crate::db::current_session().scoped::<ChatIds, _>()
}

fn user_id_cache() -> Arc<RwLock<HashMap<String, i64>>> {
    crate::db::current_session().scoped::<UserIds, _>()
}

/// Drop a chat's cached identifier→id mapping. Call after deleting a chat row so
/// a later recreate doesn't reuse the stale (now-deleted) integer id.
pub fn forget_chat_id(chat_identifier: &str) {
    chat_id_cache().write().unwrap().remove(chat_identifier);
}

/// Lookup-only: get integer chat ID from identifier. Errors if not found.
pub fn get_chat_id_by_identifier(chat_identifier: &str) -> Result<i64, String> {
    // Fast path: cache hit
    {
        let owner = chat_id_cache();
        let cache = owner.read().unwrap();
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
        let owner = chat_id_cache();
        let mut cache = owner.write().unwrap();
        cache.insert(chat_identifier.to_string(), id);
    }

    Ok(id)
}

/// Get or create integer chat ID from identifier.
pub fn get_or_create_chat_id(chat_identifier: &str) -> Result<i64, String> {
    // Fast path: cache hit
    {
        let owner = chat_id_cache();
        let cache = owner.read().unwrap();
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
        // Create stub chat entry. Discriminant must match ChatType::to_i32:
        // 0 = DirectMessage (npub), 2 = Community (non-npub). Value 1 was the
        // retired MlsGroup variant and is dropped by the get_all_chats load filter,
        // so a non-npub stub MUST be 2 or the chat vanishes on reload.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap()
            .as_secs() as i64;
        let chat_type: i32 = if chat_identifier.starts_with("npub1") { 0 } else { 2 };

        // A DM stub's id IS its counterparty: write the participant now, or the
        // row boots with an empty roster and every participant-keyed lookup
        // (attachment downloads) misses forever.
        let participants = if chat_type == 0 {
            format!("[\"{}\"]", chat_identifier)
        } else {
            "[]".to_string()
        };

        conn.execute(
            "INSERT INTO chats (chat_identifier, chat_type, participants, created_at) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![chat_identifier, chat_type, participants, now],
        ).map_err(|e| format!("Failed to create chat stub: {}", e))?;

        conn.last_insert_rowid()
    };

    // Update cache
    {
        let owner = chat_id_cache();
        let mut cache = owner.write().unwrap();
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
        let owner = user_id_cache();
        let cache = owner.read().unwrap();
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
        let owner = user_id_cache();
        let mut cache = owner.write().unwrap();
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

        let owner = chat_id_cache();
        let mut cache = owner.write().unwrap();
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

        let owner = user_id_cache();
        let mut cache = owner.write().unwrap();
        for row in rows.flatten() {
            cache.insert(row.0, row.1);
        }
    }

    Ok(())
}

/// Clear all ID caches (call on account switch).
pub fn clear_id_caches() {
    chat_id_cache().write().unwrap().clear();
    user_id_cache().write().unwrap().clear();
}
