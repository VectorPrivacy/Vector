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

/// Backfill marketplace_id + installed_version for history entries that predate tracking.
/// Matches the blossom hash in src_url filenames against the marketplace cache.
/// Only hash-based matching is used — name matching is intentionally avoided to prevent
/// phishing when public publishing is enabled.
pub fn backfill_marketplace_ids(apps: &[super::super::miniapps::marketplace::MarketplaceApp]) -> Result<u32, String> {
    let conn = crate::account_manager::get_write_connection_guard_static()?;

    // Get history entries missing marketplace_id
    let mut stmt = conn.prepare(
        "SELECT id, src_url FROM miniapps_history WHERE marketplace_id IS NULL"
    ).map_err(|e| format!("Failed to query orphan history: {}", e))?;

    let orphans: Vec<(i64, String)> = stmt.query_map([], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
    }).map_err(|e| format!("Failed to iterate orphans: {}", e))?
      .filter_map(|r| r.ok())
      .collect();

    let mut updated = 0u32;
    for (id, src_url) in &orphans {
        // Extract hash from filename (e.g. ".../c7069f82...6fae.xdc" → "c7069f82...6fae")
        let hash = src_url
            .rsplit('/')
            .next()
            .and_then(|f| f.strip_suffix(".xdc"));
        let hash = match hash {
            Some(h) => h,
            None => continue,
        };

        // Exact hash match → user has this exact file from the marketplace
        if let Some(app) = apps.iter().find(|a| a.blossom_hash == hash) {
            conn.execute(
                "UPDATE miniapps_history SET marketplace_id = ?1, installed_version = ?2 WHERE id = ?3",
                rusqlite::params![app.id, app.version, id],
            ).map_err(|e| format!("Failed to backfill marketplace_id: {}", e))?;
            updated += 1;
        }
    }

    Ok(updated)
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

// ============================================================================
// Marketplace Cache Functions
// ============================================================================

/// Save marketplace apps to the SQLite cache (full replace).
/// Upserts all provided apps and deletes any IDs not in the new set.
pub fn save_marketplace_cache(apps: &[crate::miniapps::marketplace::MarketplaceApp]) -> Result<(), String> {
    let mut conn = crate::account_manager::get_write_connection_guard_static()?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;

    let tx = conn.transaction()
        .map_err(|e| format!("Failed to start marketplace cache transaction: {}", e))?;

    // Upsert each app
    for app in apps {
        let json = serde_json::to_string(app)
            .map_err(|e| format!("Failed to serialize marketplace app {}: {}", app.id, e))?;
        tx.execute(
            "INSERT OR REPLACE INTO marketplace_cache (id, data, fetched_at) VALUES (?1, ?2, ?3)",
            rusqlite::params![app.id, json, now],
        ).map_err(|e| format!("Failed to upsert marketplace app {}: {}", app.id, e))?;
    }

    // Delete any cached apps that are no longer in the fetched set
    if !apps.is_empty() {
        let placeholders: Vec<String> = (1..=apps.len()).map(|i| format!("?{}", i)).collect();
        let sql = format!(
            "DELETE FROM marketplace_cache WHERE id NOT IN ({})",
            placeholders.join(",")
        );
        let ids: Vec<&str> = apps.iter().map(|a| a.id.as_str()).collect();
        let params: Vec<&dyn rusqlite::types::ToSql> = ids.iter().map(|s| s as &dyn rusqlite::types::ToSql).collect();
        tx.execute(&sql, params.as_slice())
            .map_err(|e| format!("Failed to clean stale marketplace cache entries: {}", e))?;
    }

    tx.commit()
        .map_err(|e| format!("Failed to commit marketplace cache: {}", e))?;

    Ok(())
}

/// Load all marketplace apps from the SQLite cache.
pub fn load_marketplace_cache() -> Result<Vec<crate::miniapps::marketplace::MarketplaceApp>, String> {
    let conn = crate::account_manager::get_db_connection_guard_static()?;

    let mut stmt = conn.prepare(
        "SELECT data FROM marketplace_cache"
    ).map_err(|e| format!("Failed to prepare marketplace cache query: {}", e))?;

    let apps: Vec<crate::miniapps::marketplace::MarketplaceApp> = stmt.query_map([], |row| {
        let json: String = row.get(0)?;
        serde_json::from_str(&json).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })
    })
    .map_err(|e| format!("Failed to query marketplace cache: {}", e))?
    .filter_map(|r| r.ok())
    .collect();

    Ok(apps)
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

// ============================================================================
// Peer Advertisement Queries (for realtime channel discovery)
// ============================================================================

/// A persisted peer advertisement record from SQLite.
pub struct PeerAdvertisementRecord {
    pub npub: String,
    pub node_addr_encoded: String,
}

/// Get active peer advertisements for a topic.
///
/// Returns advertisements where the sender's most recent event for this topic
/// is a "peer-advertisement" (not invalidated by a subsequent "peer-left").
/// Excludes our own npub. Uses `reference_id` = topic_encoded (indexed).
pub fn get_active_peer_advertisements(
    topic_encoded: &str,
    my_npub: &str,
) -> Result<Vec<PeerAdvertisementRecord>, String> {
    let conn = crate::account_manager::get_db_connection_guard_static()?;

    let mut stmt = conn.prepare(
        r#"
        SELECT e.npub, e.tags, e.created_at
        FROM events e
        INNER JOIN (
            SELECT npub, MAX(created_at) as max_ts
            FROM events
            WHERE kind = 30078
              AND reference_id = ?1
              AND content IN ('peer-advertisement', 'peer-left')
              AND npub IS NOT NULL
              AND npub != ?2
            GROUP BY npub
        ) latest ON e.npub = latest.npub AND e.created_at = latest.max_ts
        WHERE e.kind = 30078
          AND e.reference_id = ?1
          AND e.content = 'peer-advertisement'
        "#
    ).map_err(|e| format!("Failed to prepare peer advertisement query: {}", e))?;

    let records = stmt.query_map(
        rusqlite::params![topic_encoded, my_npub],
        |row| {
            let npub: String = row.get(0)?;
            let tags_json: String = row.get(1)?;
            let _: i64 = row.get(2)?; // created_at consumed by query join

            // Extract node_addr from tags JSON
            let tags: Vec<Vec<String>> = serde_json::from_str(&tags_json).unwrap_or_default();
            let node_addr = tags.iter()
                .find(|t| t.first().map(|s| s.as_str()) == Some("webxdc-node-addr"))
                .and_then(|t| t.get(1))
                .cloned()
                .unwrap_or_default();

            let _: i64 = row.get(2)?; // created_at used by query ordering

            Ok(PeerAdvertisementRecord {
                npub,
                node_addr_encoded: node_addr,
            })
        }
    ).map_err(|e| format!("Failed to query peer advertisements: {}", e))?;

    Ok(records.filter_map(|r| r.ok()).filter(|r| !r.node_addr_encoded.is_empty()).collect())
}