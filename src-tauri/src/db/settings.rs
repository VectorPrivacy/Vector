//! Database settings operations.
//!
//! This module handles:
//! - Theme preferences
//! - Private key (pkey) storage
//! - Seed phrase storage (encrypted)
//! - Generic SQL settings key-value store

use tauri::command;

use crate::crypto::{maybe_encrypt, maybe_decrypt};

#[command]
pub fn get_theme() -> Result<Option<String>, String> {
    if let Ok(_npub) = crate::account_manager::get_current_account() {
        return get_sql_setting("theme".to_string());
    }
    Ok(None)
}

#[command]
pub async fn set_pkey<R: tauri::Runtime>(handle: tauri::AppHandle<R>, pkey: String) -> Result<(), String> {
    // Check if there's a pending account (new account creation flow)
    if let Ok(Some(npub)) = crate::account_manager::get_pending_account() {
        // Stop any previous account's TorService BEFORE init_profile_database
        // hydrates the new account's Tor pref cache. Otherwise there's a
        // window where cache reflects the new (empty/false) pref while the
        // slot still holds the old service: transport_state() returns
        // Disabled, but the old proxy is still up. Anything that builds an
        // HTTP client during that window goes Direct.
        crate::commands::tor::stop_and_join_if_running().await;

        // Initialize database for the pending account
        crate::account_manager::init_profile_database(&handle, &npub).await?;
        crate::account_manager::set_current_account(npub.clone())?;
        crate::account_manager::clear_pending_account()?;

        // Start a fresh TorService for the new account if its pref says so.
        // The slot is already empty from the stop above, so this is just the
        // "start if wanted" half of sync_to_active_account.
        if let Err(e) = crate::commands::tor::sync_to_active_account().await {
            eprintln!("[Account] Tor start for new account failed: {}", e);
        }

        // Now save the pkey to the newly created database
        let conn = crate::account_manager::get_write_connection_guard_static()?;
        conn.execute(
            "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
            rusqlite::params!["pkey", pkey],
        ).map_err(|e| format!("Failed to insert pkey: {}", e))?;

        // Bootstrap MLS keypackage for the new account (cache=true: no-op if already published).
        // PIN/Password flows trigger this via encrypt/decrypt commands, but Skip Encryption
        // bypasses both, so we ensure it here as the common new-account path.
        tokio::spawn(async {
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            match crate::commands::mls::regenerate_device_keypackage(true).await {
                Ok(info) => {
                    let device_id = info.get("device_id").and_then(|v| v.as_str()).unwrap_or("");
                    let cached = info.get("cached").and_then(|v| v.as_bool()).unwrap_or(false);
                    println!("[MLS] Device KeyPackage ready: device_id={}, cached={}", device_id, cached);
                }
                Err(e) => println!("[MLS] Device KeyPackage bootstrap FAILED: {}", e),
            }
        });

        return Ok(());
    }

    let conn = crate::account_manager::get_write_connection_guard_static()?;
    conn.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
        rusqlite::params!["pkey", pkey],
    ).map_err(|e| format!("Failed to insert pkey: {}", e))?;

    Ok(())
}

#[command]
pub fn get_pkey() -> Result<Option<String>, String> {
    let conn = crate::account_manager::get_db_connection_guard_static()?;
    let result: Option<String> = conn.query_row(
        "SELECT value FROM settings WHERE key = ?1",
        rusqlite::params!["pkey"],
        |row| row.get(0)
    ).ok();
    Ok(result)
}

#[command]
pub async fn set_seed(seed: String) -> Result<(), String> {
    let stored_seed = maybe_encrypt(seed).await;
    let conn = crate::account_manager::get_write_connection_guard_static()?;
    conn.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
        rusqlite::params!["seed", stored_seed],
    ).map_err(|e| format!("Failed to insert seed: {}", e))?;
    Ok(())
}

#[command]
pub async fn get_seed() -> Result<Option<String>, String> {
    let conn = crate::account_manager::get_db_connection_guard_static()?;
    let stored_seed: Option<String> = conn.query_row(
        "SELECT value FROM settings WHERE key = ?1",
        rusqlite::params!["seed"],
        |row| row.get(0)
    ).ok();

    if let Some(seed_value) = stored_seed {
        match maybe_decrypt(seed_value).await {
            Ok(decrypted) => return Ok(Some(decrypted)),
            Err(_) => return Err("Failed to decrypt seed phrase".to_string()),
        }
    }
    Ok(None)
}

/// Set a setting value in SQL database
#[command]
pub fn set_sql_setting(key: String, value: String) -> Result<(), String> {
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
    let conn = crate::account_manager::get_write_connection_guard_static()?;
    let rows_affected = conn.execute(
        "DELETE FROM settings WHERE key = ?1",
        rusqlite::params![key],
    ).map_err(|e| format!("Failed to delete setting: {}", e))?;
    Ok(rows_affected > 0)
}
