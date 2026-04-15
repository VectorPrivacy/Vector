//! MLS database operations.
//!
//! Persistence for MLS (Messaging Layer Security):
//! - Group metadata storage
//! - Keypackage index
//! - Event cursors for sync
//! - Device ID storage
//! - Negentropy reconciliation items

use std::collections::HashMap;
use crate::mls::types::{MlsGroupFull, MlsGroup, MlsGroupProfile, EventCursor};

// ============================================================================
// Group Metadata
// ============================================================================

/// Save MLS groups to SQL database.
pub fn save_mls_groups(groups: &[MlsGroupFull]) -> Result<(), String> {
    let conn = super::get_write_connection_guard_static()?;

    for group in groups {
        conn.execute(
            "INSERT OR REPLACE INTO mls_groups (group_id, engine_group_id, creator_pubkey, name, description, avatar_ref, avatar_cached, created_at, updated_at, evicted)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            rusqlite::params![
                group.group.group_id,
                group.group.engine_group_id,
                group.group.creator_pubkey,
                group.profile.name,
                group.profile.description,
                group.profile.avatar_ref,
                group.profile.avatar_cached,
                group.group.created_at as i64,
                group.group.updated_at as i64,
                group.group.evicted as i32,
            ],
        ).map_err(|e| format!("Failed to save MLS group {}: {}", group.group.group_id, e))?;
    }

    Ok(())
}

/// Save a single MLS group — more efficient for adding new groups.
pub fn save_mls_group(group: &MlsGroupFull) -> Result<(), String> {
    let conn = super::get_write_connection_guard_static()?;

    conn.execute(
        "INSERT OR REPLACE INTO mls_groups (group_id, engine_group_id, creator_pubkey, name, description, avatar_ref, avatar_cached, created_at, updated_at, evicted)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        rusqlite::params![
            group.group.group_id,
            group.group.engine_group_id,
            group.group.creator_pubkey,
            group.profile.name,
            group.profile.description,
            group.profile.avatar_ref,
            group.profile.avatar_cached,
            group.group.created_at as i64,
            group.group.updated_at as i64,
            group.group.evicted as i32,
        ],
    ).map_err(|e| format!("Failed to save MLS group {}: {}", group.group.group_id, e))?;

    Ok(())
}

/// Update only the avatar columns for a single group.
pub fn update_mls_group_avatar(
    group_id: &str,
    avatar_cached: &str,
    avatar_ref: Option<&str>,
) -> Result<(), String> {
    let conn = super::get_write_connection_guard_static()?;

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
pub fn clear_all_mls_group_avatar_cache() -> Result<u64, String> {
    let conn = super::get_write_connection_guard_static()?;
    let changed = conn.execute(
        "UPDATE mls_groups SET avatar_cached = NULL WHERE avatar_cached IS NOT NULL",
        [],
    ).map_err(|e| format!("Failed to clear MLS group avatar cache: {}", e))?;
    Ok(changed as u64)
}

/// Load all MLS groups from the database.
pub fn load_mls_groups() -> Result<Vec<MlsGroupFull>, String> {
    let conn = super::get_db_connection_guard_static()?;

    let mut stmt = conn.prepare(
        "SELECT group_id, engine_group_id, creator_pubkey, name, description, avatar_ref, avatar_cached, created_at, updated_at, evicted FROM mls_groups"
    ).map_err(|e| format!("Failed to prepare query: {}", e))?;

    let rows = stmt.query_map([], |row| {
        Ok(MlsGroupFull {
            group: MlsGroup {
                group_id: row.get(0)?,
                engine_group_id: row.get(1)?,
                creator_pubkey: row.get(2)?,
                created_at: row.get::<_, i64>(7)? as u64,
                updated_at: row.get::<_, i64>(8)? as u64,
                evicted: row.get::<_, i32>(9)? != 0,
            },
            profile: MlsGroupProfile {
                name: row.get(3)?,
                description: row.get(4)?,
                avatar_ref: row.get(5)?,
                avatar_cached: row.get(6)?,
            },
        })
    }).map_err(|e| format!("Failed to query mls_groups: {}", e))?;

    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("Failed to collect groups: {}", e))
}

/// Look up the engine_group_id for a given wire group_id (sync, for use in spawn_blocking).
pub fn get_mls_engine_group_id(group_id: &str) -> Result<Option<String>, String> {
    let conn = super::get_write_connection_guard_static()?;
    let mut stmt = conn
        .prepare("SELECT engine_group_id FROM mls_groups WHERE group_id = ?1")
        .map_err(|e| format!("Failed to prepare query: {}", e))?;
    let result = stmt
        .query_row([group_id], |row| row.get::<_, String>(0))
        .ok();
    Ok(result)
}

// ============================================================================
// Keypackage Index
// ============================================================================

/// Save MLS keypackage index (replaces all existing entries).
pub fn save_mls_keypackages(packages: &[serde_json::Value]) -> Result<(), String> {
    let conn = super::get_write_connection_guard_static()?;

    conn.execute("DELETE FROM mls_keypackages", [])
        .map_err(|e| format!("Failed to clear MLS keypackages: {}", e))?;

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

    Ok(())
}

/// Load MLS keypackage index.
pub fn load_mls_keypackages() -> Result<Vec<serde_json::Value>, String> {
    let conn = super::get_db_connection_guard_static()?;

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

    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("Failed to collect MLS keypackages: {}", e))
}

// ============================================================================
// Event Cursors
// ============================================================================

/// Save MLS event cursors (sync progress per group).
pub fn save_mls_event_cursors(cursors: &HashMap<String, EventCursor>) -> Result<(), String> {
    let conn = super::get_write_connection_guard_static()?;

    for (group_id, cursor) in cursors {
        conn.execute(
            "INSERT OR REPLACE INTO mls_event_cursors (group_id, last_seen_event_id, last_seen_at)
             VALUES (?1, ?2, ?3)",
            rusqlite::params![group_id, &cursor.last_seen_event_id, cursor.last_seen_at as i64],
        ).map_err(|e| format!("Failed to save MLS event cursor: {}", e))?;
    }

    Ok(())
}

/// Load MLS event cursors.
pub fn load_mls_event_cursors() -> Result<HashMap<String, EventCursor>, String> {
    let conn = super::get_db_connection_guard_static()?;

    let mut stmt = conn.prepare(
        "SELECT group_id, last_seen_event_id, last_seen_at FROM mls_event_cursors"
    ).map_err(|e| format!("Failed to prepare MLS event cursors query: {}", e))?;

    let rows = stmt.query_map([], |row| {
        let group_id: String = row.get(0)?;
        let last_seen_at: i64 = row.get(2)?;
        let cursor = EventCursor {
            last_seen_event_id: row.get(1)?,
            last_seen_at: last_seen_at as u64,
        };
        Ok((group_id, cursor))
    }).map_err(|e| format!("Failed to query MLS event cursors: {}", e))?;

    rows.collect::<Result<HashMap<_, _>, _>>()
        .map_err(|e| format!("Failed to collect MLS event cursors: {}", e))
}

// ============================================================================
// Negentropy Reconciliation
// ============================================================================

/// Load processed MLS event IDs as (EventId, Timestamp) pairs for NIP-77 negentropy.
pub fn load_mls_negentropy_items(since: Option<u64>) -> Result<Vec<(nostr_sdk::EventId, nostr_sdk::Timestamp)>, String> {
    let conn = super::get_db_connection_guard_static()
        .map_err(|_| "No DB connection".to_string())?;

    let (sql, params): (&str, Vec<Box<dyn rusqlite::types::ToSql>>) = if let Some(ts) = since {
        ("SELECT event_id, created_at FROM mls_processed_events WHERE created_at >= ?1",
         vec![Box::new(ts as i64)])
    } else {
        ("SELECT event_id, created_at FROM mls_processed_events",
         vec![])
    };

    let mut stmt = conn.prepare(sql)
        .map_err(|e| format!("Failed to prepare MLS negentropy query: {}", e))?;

    let items: Vec<_> = stmt.query_map(rusqlite::params_from_iter(params.iter()), |row| {
        let event_id_hex: String = row.get(0)?;
        let created_at: i64 = row.get(1)?;
        Ok((event_id_hex, created_at))
    }).map_err(|e| format!("Failed to query mls_processed_events: {}", e))?
    .filter_map(|r| r.ok())
    .filter_map(|(hex, ts)| {
        nostr_sdk::EventId::from_hex(&hex).ok().map(|eid| {
            (eid, nostr_sdk::Timestamp::from_secs(ts as u64))
        })
    })
    .collect();

    Ok(items)
}

// ============================================================================
// Device ID
// ============================================================================

/// Save MLS device ID to the settings table.
pub fn save_mls_device_id(device_id: &str) -> Result<(), String> {
    let conn = super::get_write_connection_guard_static()?;

    conn.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES ('mls_device_id', ?1)",
        rusqlite::params![device_id],
    ).map_err(|e| format!("Failed to save MLS device ID: {}", e))?;

    Ok(())
}

/// Load MLS device ID from the settings table.
pub fn load_mls_device_id() -> Result<Option<String>, String> {
    let conn = super::get_db_connection_guard_static()?;

    let device_id: Option<String> = conn.query_row(
        "SELECT value FROM settings WHERE key = 'mls_device_id'",
        [],
        |row| row.get(0),
    ).ok();

    Ok(device_id)
}
