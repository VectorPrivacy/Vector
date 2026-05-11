//! Settings key-value store operations.

/// Get a SQL setting by key.
pub fn get_sql_setting(key: String) -> Result<Option<String>, String> {
    let conn = super::get_db_connection_guard_static()?;
    let result: Option<String> = conn.query_row(
        "SELECT value FROM settings WHERE key = ?1",
        rusqlite::params![key],
        |row| row.get(0),
    ).ok();
    Ok(result)
}

/// Set a SQL setting key-value pair.
pub fn set_sql_setting(key: String, value: String) -> Result<(), String> {
    let conn = super::get_write_connection_guard_static()?;
    conn.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
        rusqlite::params![key, value],
    ).map_err(|e| format!("Failed to set setting: {}", e))?;
    Ok(())
}

/// Remove a setting by key.
pub fn remove_setting(key: &str) -> Result<(), String> {
    let conn = super::get_write_connection_guard_static()?;
    conn.execute("DELETE FROM settings WHERE key = ?1", rusqlite::params![key])
        .map_err(|e| format!("Failed to remove setting: {}", e))?;
    Ok(())
}

/// Get the stored private key (bech32 nsec).
pub fn get_pkey() -> Result<Option<String>, String> {
    let conn = super::get_db_connection_guard_static()?;
    Ok(conn.query_row(
        "SELECT value FROM settings WHERE key = 'pkey'",
        [],
        |row| row.get(0),
    ).ok())
}

/// Set the stored private key.
pub fn set_pkey(pkey: &str) -> Result<(), String> {
    let conn = super::get_write_connection_guard_static()?;
    conn.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES ('pkey', ?1)",
        rusqlite::params![pkey],
    ).map_err(|e| format!("Failed to set pkey: {}", e))?;
    Ok(())
}

/// Get the stored seed phrase (may be encrypted).
pub fn get_seed() -> Result<Option<String>, String> {
    let conn = super::get_db_connection_guard_static()?;
    Ok(conn.query_row(
        "SELECT value FROM settings WHERE key = 'seed'",
        [],
        |row| row.get(0),
    ).ok())
}

/// Set the seed phrase (should be encrypted before calling).
pub fn set_seed(seed: &str) -> Result<(), String> {
    let conn = super::get_write_connection_guard_static()?;
    conn.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES ('seed', ?1)",
        rusqlite::params![seed],
    ).map_err(|e| format!("Failed to set seed: {}", e))?;
    Ok(())
}

/// Atomically commit the four settings written during new-account setup:
/// the (possibly-encrypted) pkey, the `encryption_enabled` flag, the
/// `security_type` (only when encrypted), and the (already-encrypted) seed
/// phrase. Wrapping these in a single transaction makes the new-account
/// flow crash-safe: either all four land or none do. The previous design
/// wrote them through four separate `set_sql_setting` calls, which left a
/// window where pkey was persisted but `encryption_enabled` was not — the
/// next boot would then mis-interpret the encrypted blob as plaintext nsec
/// and brick the account.
///
/// `security_type` is `Some(_)` for encrypted accounts and `None` for
/// skip-encryption flows (passing `Some("")` would write an empty string,
/// which `resolve_encryption_enabled` treats as encrypted — not what we
/// want for the skip path).
pub fn commit_account_setup(
    pkey: &str,
    encryption_enabled: bool,
    security_type: Option<&str>,
    encrypted_seed: Option<&str>,
) -> Result<(), String> {
    let mut conn = super::get_write_connection_guard_static()?;
    let tx = conn.transaction()
        .map_err(|e| format!("Failed to begin tx: {}", e))?;
    tx.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES ('pkey', ?1)",
        rusqlite::params![pkey],
    ).map_err(|e| format!("Failed to set pkey: {}", e))?;
    tx.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES ('encryption_enabled', ?1)",
        rusqlite::params![if encryption_enabled { "true" } else { "false" }],
    ).map_err(|e| format!("Failed to set encryption_enabled: {}", e))?;
    if let Some(st) = security_type {
        tx.execute(
            "INSERT OR REPLACE INTO settings (key, value) VALUES ('security_type', ?1)",
            rusqlite::params![st],
        ).map_err(|e| format!("Failed to set security_type: {}", e))?;
    } else {
        // Skip path: ensure no stale security_type from a previous setup
        // attempt lingers (would mis-route `resolve_encryption_enabled`).
        tx.execute(
            "DELETE FROM settings WHERE key = 'security_type'",
            [],
        ).map_err(|e| format!("Failed to clear security_type: {}", e))?;
    }
    if let Some(seed) = encrypted_seed {
        tx.execute(
            "INSERT OR REPLACE INTO settings (key, value) VALUES ('seed', ?1)",
            rusqlite::params![seed],
        ).map_err(|e| format!("Failed to set seed: {}", e))?;
    }
    tx.commit().map_err(|e| format!("Failed to commit tx: {}", e))?;
    Ok(())
}
