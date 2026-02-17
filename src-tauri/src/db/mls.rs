//! MLS database operations.
//!
//! This module handles persistence for MLS (Messaging Layer Security):
//! - Group metadata storage
//! - Keypackage index
//! - Event cursors for sync
//! - Device ID storage

use std::collections::HashMap;
use tauri::{AppHandle, Runtime};

/// Save MLS groups to SQL database (plaintext columns)
pub async fn save_mls_groups<R: Runtime>(
    handle: AppHandle<R>,
    groups: &[crate::mls::MlsGroupMetadata],
) -> Result<(), String> {
    let conn = crate::account_manager::get_write_connection_guard(&handle)?;

    for group in groups {
        conn.execute(
            "INSERT OR REPLACE INTO mls_groups (group_id, engine_group_id, creator_pubkey, name, description, avatar_ref, avatar_cached, created_at, updated_at, evicted)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            rusqlite::params![
                group.group_id,
                group.engine_group_id,
                group.creator_pubkey,
                group.name,
                group.description,
                group.avatar_ref,
                group.avatar_cached,
                group.created_at as i64,
                group.updated_at as i64,
                group.evicted as i32,
            ],
        ).map_err(|e| format!("Failed to save MLS group {}: {}", group.group_id, e))?;
    }

    println!("[SQL] Saved {} MLS groups to mls_groups table", groups.len());

    Ok(())
}

/// Save a single MLS group to SQL database (plaintext columns) - more efficient for adding new groups
pub async fn save_mls_group<R: Runtime>(
    handle: AppHandle<R>,
    group: &crate::mls::MlsGroupMetadata,
) -> Result<(), String> {
    let conn = crate::account_manager::get_write_connection_guard(&handle)?;

    conn.execute(
        "INSERT OR REPLACE INTO mls_groups (group_id, engine_group_id, creator_pubkey, name, description, avatar_ref, avatar_cached, created_at, updated_at, evicted)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        rusqlite::params![
            group.group_id,
            group.engine_group_id,
            group.creator_pubkey,
            group.name,
            group.description,
            group.avatar_ref,
            group.avatar_cached,
            group.created_at as i64,
            group.updated_at as i64,
            group.evicted as i32,
        ],
    ).map_err(|e| format!("Failed to save MLS group {}: {}", group.group_id, e))?;

    println!("[SQL] Saved 1 MLS group to mls_groups table");

    Ok(())
}

/// Update only the avatar_cached (and optionally avatar_ref) columns for a single group.
/// This avoids a full load-all + save cycle when only the avatar changed.
pub fn update_mls_group_avatar<R: Runtime>(
    handle: &AppHandle<R>,
    group_id: &str,
    avatar_cached: &str,
    avatar_ref: Option<&str>,
) -> Result<(), String> {
    let conn = crate::account_manager::get_write_connection_guard(handle)?;

    if let Some(ref_url) = avatar_ref {
        conn.execute(
            "UPDATE mls_groups SET avatar_cached = ?1, avatar_ref = ?2 WHERE group_id = ?3",
            rusqlite::params![avatar_cached, ref_url, group_id],
        ).map_err(|e| format!("Failed to update group avatar: {}", e))?;
    } else {
        conn.execute(
            "UPDATE mls_groups SET avatar_cached = ?1 WHERE group_id = ?2",
            rusqlite::params![avatar_cached, group_id],
        ).map_err(|e| format!("Failed to update group avatar: {}", e))?;
    }

    Ok(())
}

/// Clear avatar_cached for all MLS groups (used when cache is purged).
pub fn clear_all_mls_group_avatar_cache<R: Runtime>(
    handle: &AppHandle<R>,
) -> Result<u64, String> {
    let conn = crate::account_manager::get_write_connection_guard(handle)?;
    let changed = conn.execute(
        "UPDATE mls_groups SET avatar_cached = NULL WHERE avatar_cached IS NOT NULL",
        [],
    ).map_err(|e| format!("Failed to clear MLS group avatar cache: {}", e))?;
    Ok(changed as u64)
}

/// Load MLS groups from SQL database (plaintext columns)
pub async fn load_mls_groups<R: Runtime>(
    handle: &AppHandle<R>,
) -> Result<Vec<crate::mls::MlsGroupMetadata>, String> {
    let conn = crate::account_manager::get_db_connection_guard(handle)?;

    let mut stmt = conn.prepare(
        "SELECT group_id, engine_group_id, creator_pubkey, name, description, avatar_ref, avatar_cached, created_at, updated_at, evicted FROM mls_groups"
    ).map_err(|e| format!("Failed to prepare query: {}", e))?;

    let rows = stmt.query_map([], |row| {
        Ok(crate::mls::MlsGroupMetadata {
            group_id: row.get(0)?,
            engine_group_id: row.get(1)?,
            creator_pubkey: row.get(2)?,
            name: row.get(3)?,
            description: row.get(4)?,
            avatar_ref: row.get(5)?,
            avatar_cached: row.get(6)?,
            created_at: row.get::<_, i64>(7)? as u64,
            updated_at: row.get::<_, i64>(8)? as u64,
            evicted: row.get::<_, i32>(9)? != 0,
        })
    }).map_err(|e| format!("Failed to query mls_groups: {}", e))?;

    let groups: Vec<crate::mls::MlsGroupMetadata> = rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("Failed to collect groups: {}", e))?;

    Ok(groups)
}

/// Save MLS keypackage index to SQL database (plaintext)
pub async fn save_mls_keypackages<R: Runtime>(
    handle: AppHandle<R>,
    packages: &[serde_json::Value],
) -> Result<(), String> {
    let conn = crate::account_manager::get_write_connection_guard(&handle)?;

    // Clear existing keypackages
    conn.execute("DELETE FROM mls_keypackages", [])
        .map_err(|e| format!("Failed to clear MLS keypackages: {}", e))?;

    // Insert new keypackages
    for pkg in packages {
        let owner_pubkey = pkg.get("owner_pubkey").and_then(|v| v.as_str()).unwrap_or("");
        let device_id = pkg.get("device_id").and_then(|v| v.as_str()).unwrap_or("");
        let keypackage_ref = pkg.get("keypackage_ref").and_then(|v| v.as_str()).unwrap_or("");
        let created_at = pkg.get("created_at").and_then(|v| v.as_u64());
        let fetched_at = pkg.get("fetched_at").and_then(|v| v.as_u64()).unwrap_or(0);
        let expires_at = pkg.get("expires_at").and_then(|v| v.as_u64()).unwrap_or(0);

        conn.execute(
            "INSERT INTO mls_keypackages (owner_pubkey, device_id, keypackage_ref, created_at, fetched_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![owner_pubkey, device_id, keypackage_ref, created_at.map(|v| v as i64), fetched_at as i64, expires_at as i64],
        ).map_err(|e| format!("Failed to insert MLS keypackage: {}", e))?;
    }

    println!("[SQL] Saved {} MLS keypackages", packages.len());

    Ok(())
}

/// Load MLS keypackage index from SQL database (plaintext)
pub async fn load_mls_keypackages<R: Runtime>(
    handle: &AppHandle<R>,
) -> Result<Vec<serde_json::Value>, String> {
    let conn = crate::account_manager::get_db_connection_guard(handle)?;

    let mut stmt = conn.prepare(
        "SELECT owner_pubkey, device_id, keypackage_ref, created_at, fetched_at, expires_at FROM mls_keypackages"
    ).map_err(|e| format!("Failed to prepare MLS keypackages query: {}", e))?;

    let rows = stmt.query_map([], |row| {
        let created_at: Option<i64> = row.get(3)?;
        let fetched_at: i64 = row.get(4)?;
        let expires_at: i64 = row.get(5)?;
        let mut json = serde_json::json!({
            "owner_pubkey": row.get::<_, String>(0)?,
            "device_id": row.get::<_, String>(1)?,
            "keypackage_ref": row.get::<_, String>(2)?,
            "fetched_at": fetched_at as u64,
            "expires_at": expires_at as u64,
        });
        if let Some(ts) = created_at {
            json["created_at"] = serde_json::json!(ts as u64);
        }
        Ok(json)
    }).map_err(|e| format!("Failed to query MLS keypackages: {}", e))?;

    let packages: Vec<serde_json::Value> = rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("Failed to collect MLS keypackages: {}", e))?;

    Ok(packages)
}

/// Save MLS event cursors to SQL database (plaintext)
pub async fn save_mls_event_cursors<R: Runtime>(
    handle: AppHandle<R>,
    cursors: &HashMap<String, crate::mls::EventCursor>,
) -> Result<(), String> {
    let conn = crate::account_manager::get_write_connection_guard(&handle)?;

    for (group_id, cursor) in cursors {
        conn.execute(
            "INSERT OR REPLACE INTO mls_event_cursors (group_id, last_seen_event_id, last_seen_at)
             VALUES (?1, ?2, ?3)",
            rusqlite::params![group_id, &cursor.last_seen_event_id, cursor.last_seen_at as i64],
        ).map_err(|e| format!("Failed to save MLS event cursor: {}", e))?;
    }


    Ok(())
}

/// Load MLS event cursors from SQL database (plaintext)
pub async fn load_mls_event_cursors<R: Runtime>(
    handle: &AppHandle<R>,
) -> Result<HashMap<String, crate::mls::EventCursor>, String> {
    let conn = crate::account_manager::get_db_connection_guard(handle)?;

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

    Ok(cursors)
}

/// Save MLS device ID to SQL database (plaintext)
pub async fn save_mls_device_id<R: Runtime>(
    handle: AppHandle<R>,
    device_id: &str,
) -> Result<(), String> {
    let conn = crate::account_manager::get_write_connection_guard(&handle)?;

    conn.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES ('mls_device_id', ?1)",
        rusqlite::params![device_id],
    ).map_err(|e| format!("Failed to save MLS device ID to SQL: {}", e))?;

    println!("[SQL] Saved MLS device ID");

    Ok(())
}

/// Load MLS device ID from SQL database (plaintext)
pub async fn load_mls_device_id<R: Runtime>(
    handle: &AppHandle<R>,
) -> Result<Option<String>, String> {
    let conn = crate::account_manager::get_db_connection_guard(handle)?;

    let device_id: Option<String> = conn.query_row(
        "SELECT value FROM settings WHERE key = 'mls_device_id'",
        [],
        |row| row.get(0)
    ).ok();


    Ok(device_id)
}
