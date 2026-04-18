//! MLS event tracking and deduplication.
//!
//! Handles:
//! - Tracking which MLS wrapper events have been processed
//! - Legacy database migration/cleanup

/// Check if an MLS event has already been processed.
/// Returns true if the event_id exists in mls_processed_events table.
pub fn is_mls_event_processed(event_id: &str) -> bool {
    let conn = match crate::db::get_db_connection_guard_static() {
        Ok(c) => c,
        Err(_) => return false,
    };

    conn.query_row(
        "SELECT 1 FROM mls_processed_events WHERE event_id = ?1",
        rusqlite::params![event_id],
        |_| Ok(true),
    ).unwrap_or(false)
}

/// Mark an MLS event as processed.
/// Uses INSERT OR IGNORE to handle race conditions safely.
pub fn track_mls_event_processed(
    event_id: &str,
    group_id: &str,
    event_created_at: u64,
) -> Result<(), String> {
    let conn = crate::db::get_write_connection_guard_static()?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    conn.execute(
        "INSERT OR IGNORE INTO mls_processed_events (event_id, group_id, created_at, processed_at) VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![event_id, group_id, event_created_at as i64, now as i64],
    ).map_err(|e| format!("Failed to track processed event: {}", e))?;

    Ok(())
}

/// Wipe an old MDK database (v0.2.x) that is incompatible with the current engine.
///
/// Detection: the old dual-connection architecture created an
/// `openmls_sqlite_storage_migrations` table. The new unified MDK never creates this.
///
/// Returns `true` if a legacy database was detected and wiped.
pub fn wipe_legacy_mls_database(db_path: &std::path::Path) -> bool {
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
        println!("[MLS] Detected legacy v0.2.x MLS database — wiping for clean start...");
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wipe_legacy_nonexistent_path() {
        let tmp = std::env::temp_dir().join("vector_test_wipe_nonexistent.db");
        let _ = std::fs::remove_file(&tmp);
        assert!(!wipe_legacy_mls_database(&tmp));
    }

    #[test]
    fn wipe_legacy_modern_db() {
        let tmp = std::env::temp_dir().join("vector_test_wipe_modern.db");
        let _ = std::fs::remove_file(&tmp);

        // Create a modern DB (no openmls migration table)
        let conn = rusqlite::Connection::open(&tmp).unwrap();
        conn.execute("CREATE TABLE some_table (id INTEGER)", []).unwrap();
        drop(conn);

        assert!(!wipe_legacy_mls_database(&tmp));
        assert!(tmp.exists()); // Not wiped
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn wipe_legacy_old_db() {
        let tmp = std::env::temp_dir().join("vector_test_wipe_legacy.db");
        let _ = std::fs::remove_file(&tmp);

        // Create a legacy DB with the marker table
        let conn = rusqlite::Connection::open(&tmp).unwrap();
        conn.execute("CREATE TABLE openmls_sqlite_storage_migrations (id INTEGER)", []).unwrap();
        drop(conn);

        assert!(wipe_legacy_mls_database(&tmp));
        assert!(!tmp.exists()); // Wiped
    }
}
