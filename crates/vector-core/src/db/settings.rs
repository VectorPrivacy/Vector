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
