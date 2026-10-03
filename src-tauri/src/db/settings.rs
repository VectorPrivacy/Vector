//! Database settings operations.
//!
//! This module handles:
//! - Theme preferences
//! - Reading the private key (pkey), for Rust only
//! - Generic SQL settings key-value store

use tauri::command;

#[command]
pub fn get_theme() -> Result<Option<String>, String> {
    if let Ok(_npub) = crate::account_manager::get_current_account() {
        return get_sql_setting("theme".to_string());
    }
    Ok(None)
}

/// The stored key, sealed when Local Encryption is on. Rust-only: it never crosses IPC.
pub fn get_pkey() -> Result<Option<String>, String> {
    let conn = crate::account_manager::get_db_connection_guard_static()?;
    let result: Option<String> = conn.query_row(
        "SELECT value FROM settings WHERE key = ?1",
        rusqlite::params!["pkey"],
        |row| row.get(0)
    ).ok();
    Ok(result)
}

/// Set a setting value in SQL database
#[command]
pub fn set_sql_setting(key: String, value: String) -> Result<(), String> {
    if vector_core::db::settings::PROTECTED_SETTINGS.contains(&key.as_str()) {
        return Err(format!("`{key}` is managed by the app and can't be set here"));
    }
    // The media-proxy pick is remembered for a few minutes; a change to the
    // setting must take effect on the next picture, not after that.
    if key == crate::magnitude::SETTING_KEY {
        crate::magnitude::forget_picks();
    }
    if let Ok(_npub) = crate::account_manager::get_current_account() {
        let conn = crate::account_manager::get_write_connection_guard_static()?;
        conn.execute(
            "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
            rusqlite::params![&key, &value],
        ).map_err(|e| format!("Failed to set setting: {}", e))?;
        return Ok(());
    }
    Ok(())
}

/// Get a setting value from SQL database
#[command]
pub fn get_sql_setting(key: String) -> Result<Option<String>, String> {
    if vector_core::db::settings::SECRET_SETTINGS.contains(&key.as_str()) {
        return Err(format!("`{key}` is not readable here"));
    }
    if let Ok(_npub) = crate::account_manager::get_current_account() {
        let conn = crate::account_manager::get_db_connection_guard_static()?;
        let result: Option<String> = conn.query_row(
            "SELECT value FROM settings WHERE key = ?1",
            rusqlite::params![&key],
            |row| row.get(0)
        ).ok();
        return Ok(result);
    }
    Ok(None)
}

#[command]
pub fn remove_setting(key: String) -> Result<bool, String> {
    if vector_core::db::settings::PROTECTED_SETTINGS.contains(&key.as_str()) {
        return Err(format!("`{key}` is managed by the app and can't be removed here"));
    }
    let conn = crate::account_manager::get_write_connection_guard_static()?;
    let rows_affected = conn.execute(
        "DELETE FROM settings WHERE key = ?1",
        rusqlite::params![key],
    ).map_err(|e| format!("Failed to delete setting: {}", e))?;
    Ok(rows_affected > 0)
}
