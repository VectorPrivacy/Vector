//! Database maintenance operations.
//!
//! This module handles:
//! - Database vacuuming to reclaim space and optimize performance
//! - Automatic scheduled maintenance checks

/// Vacuum the database to reclaim space and optimize performance
pub fn vacuum_database() -> Result<(), String> {
    let conn = crate::account_manager::get_write_connection_guard_static()?;

    conn.execute_batch("VACUUM;")
        .map_err(|e| format!("Failed to vacuum database: {}", e))?;

    println!("[DB] Database vacuumed successfully");
    Ok(())
}

/// Check if vacuum is needed and perform it if so
/// Vacuums if it hasn't been done in the last 7 days
pub async fn check_and_vacuum_if_needed() -> Result<(), String> {
    let should_vacuum = {
        let conn = crate::account_manager::get_db_connection_guard_static()?;

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

        match last_vacuum {
            Some(last) => now - last > seven_days_secs,
            None => true, // Never vacuumed
        }
    }; // read conn drops here — must drop before vacuum_database takes the write conn

    if should_vacuum {
        vacuum_database()?;

        // Update last vacuum timestamp
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        let conn = crate::account_manager::get_write_connection_guard_static()?;
        conn.execute(
            "INSERT OR REPLACE INTO settings (key, value) VALUES ('last_vacuum', ?1)",
            rusqlite::params![now.to_string()],
        ).map_err(|e| format!("Failed to update last_vacuum: {}", e))?;
    }

    Ok(())
}

/// Refresh the query planner's statistics if it hasn't been done in the last day. This is the
/// periodic top-up for long-lived connections; the bulk of the work happens once via
/// `PRAGMA optimize=0x10002` when the DB opens. Plain `optimize` here is a near-no-op unless a table
/// changed materially, so a daily cadence is plenty.
pub async fn check_and_optimize_if_needed() -> Result<(), String> {
    let should_optimize = {
        let conn = crate::account_manager::get_db_connection_guard_static()?;
        let last_optimize: Option<i64> = conn.query_row(
            "SELECT value FROM settings WHERE key = 'last_optimize'",
            [],
            |row| row.get(0),
        ).ok().and_then(|s: String| s.parse().ok());

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        let one_day_secs = 24 * 60 * 60;
        match last_optimize {
            Some(last) => now - last > one_day_secs,
            None => true,
        }
    }; // read conn drops here — optimize_database takes the write conn

    if should_optimize {
        vector_core::db::optimize_database();

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        let conn = crate::account_manager::get_write_connection_guard_static()?;
        conn.execute(
            "INSERT OR REPLACE INTO settings (key, value) VALUES ('last_optimize', ?1)",
            rusqlite::params![now.to_string()],
        ).map_err(|e| format!("Failed to update last_optimize: {}", e))?;
    }

    Ok(())
}
