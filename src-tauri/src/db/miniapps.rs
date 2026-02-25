//! Mini Apps database operations.
//!
//! This module handles:
//! - Mini App history (recently opened apps)
//! - Mini App favorites
//! - Mini App permissions (per-app permission grants)
//! - Marketplace app version tracking

use serde::{Deserialize, Serialize};

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
pub fn record_miniapp_opened(
    name: String,
    src_url: String,
    attachment_ref: String,
) -> Result<(), String> {
    record_miniapp_opened_with_metadata(name, src_url, attachment_ref, String::new(), None, None)
}

/// Record a Mini App being opened with additional metadata (categories, marketplace_id, and version)
pub fn record_miniapp_opened_with_metadata(
    name: String,
    src_url: String,
    attachment_ref: String,
    categories: String,
    marketplace_id: Option<String>,
    installed_version: Option<String>,
) -> Result<(), String> {
    let conn = crate::account_manager::get_write_connection_guard_static()?;

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


    Ok(())
}

/// Get Mini Apps history sorted by last opened (most recent first)
pub fn get_miniapps_history(
    limit: Option<i64>,
) -> Result<Vec<MiniAppHistoryEntry>, String> {
    let conn = crate::account_manager::get_db_connection_guard_static()?;

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

    Ok(result)
}

/// Toggle the favorite status of a Mini App by its ID
pub fn toggle_miniapp_favorite(
    id: i64,
) -> Result<bool, String> {
    let conn = crate::account_manager::get_write_connection_guard_static()?;

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


    Ok(new_state)
}

/// Set the favorite status of a Mini App by its ID
pub fn set_miniapp_favorite(
    id: i64,
    is_favorite: bool,
) -> Result<(), String> {
    let conn = crate::account_manager::get_write_connection_guard_static()?;

    conn.execute(
        "UPDATE miniapps_history SET is_favorite = ?1 WHERE id = ?2",
        rusqlite::params![if is_favorite { 1 } else { 0 }, id],
    ).map_err(|e| format!("Failed to set Mini App favorite: {}", e))?;


    Ok(())
}

/// Remove a Mini App from history by name
pub fn remove_miniapp_from_history(
    name: &str,
) -> Result<(), String> {
    let conn = crate::account_manager::get_write_connection_guard_static()?;

    conn.execute(
        "DELETE FROM miniapps_history WHERE name = ?1",
        rusqlite::params![name],
    ).map_err(|e| format!("Failed to remove Mini App from history: {}", e))?;


    Ok(())
}

/// Update the installed version for a marketplace app
pub fn update_miniapp_version(
    marketplace_id: &str,
    version: &str,
) -> Result<(), String> {
    let conn = crate::account_manager::get_write_connection_guard_static()?;

    conn.execute(
        "UPDATE miniapps_history SET installed_version = ?1 WHERE marketplace_id = ?2",
        rusqlite::params![version, marketplace_id],
    ).map_err(|e| format!("Failed to update Mini App version: {}", e))?;


    Ok(())
}

/// Get the installed version for a marketplace app by its marketplace_id
pub fn get_miniapp_installed_version(
    marketplace_id: &str,
) -> Result<Option<String>, String> {
    let conn = crate::account_manager::get_db_connection_guard_static()?;

    let result = conn.query_row(
        "SELECT installed_version FROM miniapps_history WHERE marketplace_id = ?1",
        rusqlite::params![marketplace_id],
        |row| row.get::<_, Option<String>>(0)
    );



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
pub fn get_miniapp_granted_permissions(
    file_hash: &str,
) -> Result<String, String> {
    let conn = crate::account_manager::get_db_connection_guard_static()?;

    let mut stmt = conn.prepare(
        "SELECT permission FROM miniapp_permissions WHERE file_hash = ?1 AND granted = 1"
    ).map_err(|e| format!("Failed to prepare permission query: {}", e))?;

    let permissions: Vec<String> = stmt.query_map(rusqlite::params![file_hash], |row| {
        row.get::<_, String>(0)
    })
    .map_err(|e| format!("Failed to query permissions: {}", e))?
    .filter_map(|r| r.ok())
    .collect();

    Ok(permissions.join(","))
}

/// Set the granted status of a permission for a Mini App by its file hash
pub fn set_miniapp_permission(
    file_hash: &str,
    permission: &str,
    granted: bool,
) -> Result<(), String> {
    let conn = crate::account_manager::get_write_connection_guard_static()?;

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


    Ok(())
}

/// Set multiple permissions at once for a Mini App by its file hash
/// Uses a transaction to ensure all permissions are set atomically
pub fn set_miniapp_permissions(
    file_hash: &str,
    permissions: &[(&str, bool)],
) -> Result<(), String> {
    let mut conn = crate::account_manager::get_write_connection_guard_static()?;

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

    Ok(())
}

/// Check if an app has been prompted for permissions yet (by file hash)
/// Returns true if any permission record exists for this hash
pub fn has_miniapp_permission_prompt(
    file_hash: &str,
) -> Result<bool, String> {
    let conn = crate::account_manager::get_db_connection_guard_static()?;

    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM miniapp_permissions WHERE file_hash = ?1)",
        rusqlite::params![file_hash],
        |row| row.get(0)
    ).map_err(|e| format!("Failed to check permission prompt: {}", e))?;


    Ok(exists)
}

/// Revoke all permissions for a Mini App by its file hash
pub fn revoke_all_miniapp_permissions(
    file_hash: &str,
) -> Result<(), String> {
    let conn = crate::account_manager::get_write_connection_guard_static()?;

    conn.execute(
        "DELETE FROM miniapp_permissions WHERE file_hash = ?1",
        rusqlite::params![file_hash],
    ).map_err(|e| format!("Failed to revoke Mini App permissions: {}", e))?;


    Ok(())
}

/// Copy all permissions from one file hash to another (for app updates)
pub fn copy_miniapp_permissions(
    old_hash: &str,
    new_hash: &str,
) -> Result<(), String> {
    let conn = crate::account_manager::get_write_connection_guard_static()?;

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


    Ok(())
}