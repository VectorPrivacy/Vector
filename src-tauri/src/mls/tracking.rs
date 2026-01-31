//! MLS event tracking and deduplication.
//!
//! This module handles:
//! - Tracking which MLS wrapper events have been processed
//! - Cleanup of old processed events
//! - Legacy database migration/cleanup

// =============================================================================
// EventTracker: Tracks which MLS wrapper events have been processed
// This enables robust deduplication across live subscriptions and sync cycles.
// =============================================================================

/// Check if an MLS event has already been processed.
/// Returns true if the event_id exists in mls_processed_events table.
pub fn is_mls_event_processed<R: tauri::Runtime>(handle: &tauri::AppHandle<R>, event_id: &str) -> bool {
    let conn = match crate::account_manager::get_db_connection(handle) {
        Ok(c) => c,
        Err(_) => return false,
    };

    let exists: bool = conn.query_row(
        "SELECT 1 FROM mls_processed_events WHERE event_id = ?1",
        rusqlite::params![event_id],
        |_| Ok(true)
    ).unwrap_or(false);

    crate::account_manager::return_db_connection(conn);
    exists
}

/// Mark an MLS event as processed.
/// Uses INSERT OR IGNORE to handle race conditions safely.
pub fn track_mls_event_processed<R: tauri::Runtime>(
    handle: &tauri::AppHandle<R>,
    event_id: &str,
    group_id: &str,
    event_created_at: u64,
) -> Result<(), String> {
    let conn = crate::account_manager::get_db_connection(handle)?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    conn.execute(
        "INSERT OR IGNORE INTO mls_processed_events (event_id, group_id, created_at, processed_at) VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![event_id, group_id, event_created_at as i64, now as i64],
    ).map_err(|e| format!("Failed to track processed event: {}", e))?;

    crate::account_manager::return_db_connection(conn);
    Ok(())
}

/// Cleanup old processed events to prevent unbounded table growth.
/// Removes events older than the specified age (in seconds).
/// Call this periodically (e.g., once per sync cycle).
pub fn cleanup_old_processed_events<R: tauri::Runtime>(
    handle: &tauri::AppHandle<R>,
    max_age_secs: u64,
) -> Result<usize, String> {
    let conn = crate::account_manager::get_db_connection(handle)?;

    let cutoff = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        .saturating_sub(max_age_secs);

    let deleted = conn.execute(
        "DELETE FROM mls_processed_events WHERE processed_at < ?1",
        rusqlite::params![cutoff as i64],
    ).map_err(|e| format!("Failed to cleanup old events: {}", e))?;

    crate::account_manager::return_db_connection(conn);
    Ok(deleted)
}

/// Message record for persisting decrypted MLS messages

/// TODO(v0.3.2+): Remove this function and its call site in `new_persistent()` once
/// v0.2.x users have fully migrated. It adds a per-init `SELECT` against `sqlite_master`
/// that becomes unnecessary after the legacy transition window.
///
/// Wipe an old MDK database (v0.2.x) that is incompatible with the new unified MDK engine.
/// The old engine used separate OpenMLS/MDK storage connections with incompatible
/// cryptographic state — no migration path exists, so a clean slate is the only option.
/// Users will need to be re-invited to their groups.
///
/// Detection: the old dual-connection architecture created an `openmls_sqlite_storage_migrations`
/// table (OpenMLS's own migration tracker). The new unified MDK never creates this table.
///
/// Returns `true` if a legacy database was detected and wiped.
pub(super) fn wipe_legacy_mls_database(db_path: &std::path::Path) -> bool {
    let is_legacy = match rusqlite::Connection::open(db_path) {
        Ok(conn) => conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='openmls_sqlite_storage_migrations'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap_or(0)
            > 0,
        Err(_) => false,
    };

    if is_legacy {
        println!("[MLS] Detected legacy v0.2.x MLS database — wiping for clean v0.3.0 start...");
        if let Err(e) = std::fs::remove_file(db_path) {
            eprintln!("[MLS] Failed to remove legacy database: {}", e);
        }
        // Also remove SQLite journal/WAL sidecar files
        let _ = std::fs::remove_file(db_path.with_extension("db-shm"));
        let _ = std::fs::remove_file(db_path.with_extension("db-wal"));
        let _ = std::fs::remove_file(db_path.with_extension("db-journal"));
        println!("[MLS] Legacy database wiped. Groups will need to be re-joined.");
    }

    is_legacy
}