//! Database maintenance operations.
//!
//! This module handles:
//! - Database vacuuming to reclaim space and optimize performance
//! - Automatic scheduled maintenance checks

use tauri::{AppHandle, Runtime};

/// Vacuum the database to reclaim space and optimize performance
pub fn vacuum_database<R: Runtime>(handle: &AppHandle<R>) -> Result<(), String> {
    let conn = crate::account_manager::get_write_connection_guard(handle)?;

    conn.execute_batch("VACUUM;")
        .map_err(|e| format!("Failed to vacuum database: {}", e))?;

    println!("[DB] Database vacuumed successfully");
    Ok(())
}

/// Check if vacuum is needed and perform it if so
/// Vacuums if it hasn't been done in the last 7 days
pub async fn check_and_vacuum_if_needed<R: Runtime>(handle: &AppHandle<R>) -> Result<(), String> {
    let should_vacuum = {
        let conn = crate::account_manager::get_db_connection_guard(handle)?;

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
        vacuum_database(handle)?;

        // Update last vacuum timestamp
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        let conn = crate::account_manager::get_write_connection_guard(handle)?;
        conn.execute(
            "INSERT OR REPLACE INTO settings (key, value) VALUES ('last_vacuum', ?1)",
            rusqlite::params![now.to_string()],
        ).map_err(|e| format!("Failed to update last_vacuum: {}", e))?;
    }

    Ok(())
}
