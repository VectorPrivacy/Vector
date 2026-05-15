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
    // Write an explicit signer_type='local' row so the post-migration
    // invariant "every account has a discriminator on disk" holds for
    // freshly-created local accounts too (the migration only backfills
    // pre-existing rows).
    tx.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES ('signer_type', 'local')",
        [],
    ).map_err(|e| format!("Failed to set signer_type: {}", e))?;
    tx.commit().map_err(|e| format!("Failed to commit tx: {}", e))?;
    Ok(())
}

// ============================================================================
// NIP-46 remote-signer settings (added in migration 27)
// ============================================================================
//
// Three keys back the bunker login flow:
//   - `signer_type`         — "local" | "bunker"
//   - `bunker_url`          — `bunker://...` URI, encrypted-at-rest when the
//                             account uses pin/pass encryption (same path as
//                             pkey). Contains the connection secret.
//   - `bunker_remote_pubkey`— signer pubkey, plaintext (routing info only).
//
// The `bunker_url` getter/setter is `async` because `maybe_encrypt`/
// `maybe_decrypt` await on Argon2id key derivation when the user is logged
// into an encrypted account. The two plaintext fields stay sync.

/// Read the active signer kind from settings. Missing rows pre-date migration
/// 27 and are treated as `"local"` so pre-NIP-46 accounts behave unchanged.
pub fn get_signer_type() -> Result<String, String> {
    let conn = super::get_db_connection_guard_static()?;
    Ok(conn.query_row(
        "SELECT value FROM settings WHERE key = 'signer_type'",
        [],
        |row| row.get::<_, String>(0),
    ).unwrap_or_else(|_| "local".to_string()))
}

/// Persist the signer kind. Accepts the discriminator's `as_setting_str()`
/// form ("local" or "bunker"); other values are accepted but `get_signer_type`
/// will treat them as `local` downstream.
pub fn set_signer_type(value: &str) -> Result<(), String> {
    let conn = super::get_write_connection_guard_static()?;
    conn.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES ('signer_type', ?1)",
        rusqlite::params![value],
    ).map_err(|e| format!("Failed to set signer_type: {}", e))?;
    Ok(())
}

/// Read the `bunker://` URL, decrypting it if the account uses encryption.
/// Returns `Ok(None)` for local accounts (no row), or when decryption fails
/// against an obviously-encrypted blob (likely the user hasn't unlocked yet).
pub async fn get_bunker_url() -> Result<Option<String>, String> {
    let raw: Option<String> = {
        let conn = super::get_db_connection_guard_static()?;
        conn.query_row(
            "SELECT value FROM settings WHERE key = 'bunker_url'",
            [],
            |row| row.get::<_, String>(0),
        ).ok()
    };
    match raw {
        Some(s) => match crate::crypto::maybe_decrypt(s).await {
            Ok(plain) => Ok(Some(plain)),
            Err(_) => Err("bunker_url decryption failed (account locked?)".into()),
        },
        None => Ok(None),
    }
}

/// Persist the `bunker://` URL, encrypting if the account uses encryption.
/// The plaintext form is never written to disk for encrypted accounts.
pub async fn set_bunker_url(url: &str) -> Result<(), String> {
    let stored = crate::crypto::maybe_encrypt(url.to_string()).await;
    let conn = super::get_write_connection_guard_static()?;
    conn.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES ('bunker_url', ?1)",
        rusqlite::params![stored],
    ).map_err(|e| format!("Failed to set bunker_url: {}", e))?;
    Ok(())
}

/// Read the cached remote signer pubkey (hex). Plaintext on disk — it's
/// public-key material with no secrecy implications, and keeping it readable
/// before unlock lets the UI display "Connected to <pubkey>" on the locked
/// account picker without prompting for a password.
pub fn get_bunker_remote_pubkey() -> Result<Option<String>, String> {
    let conn = super::get_db_connection_guard_static()?;
    Ok(conn.query_row(
        "SELECT value FROM settings WHERE key = 'bunker_remote_pubkey'",
        [],
        |row| row.get::<_, String>(0),
    ).ok())
}

/// Persist the cached remote signer pubkey (hex form). Updated after each
/// successful bunker bootstrap — the bootstrap response carries the canonical
/// pubkey, which may differ from any user-supplied form.
pub fn set_bunker_remote_pubkey(pubkey_hex: &str) -> Result<(), String> {
    let conn = super::get_write_connection_guard_static()?;
    conn.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES ('bunker_remote_pubkey', ?1)",
        rusqlite::params![pubkey_hex],
    ).map_err(|e| format!("Failed to set bunker_remote_pubkey: {}", e))?;
    Ok(())
}

/// Atomically commit the four settings written during *bunker* new-account
/// setup: the (possibly-encrypted) client keypair pkey, `encryption_enabled`,
/// `security_type`, plus `signer_type='bunker'`, the (possibly-encrypted)
/// `bunker_url`, and the plaintext `bunker_remote_pubkey`. Wraps the whole
/// commit in a transaction for the same reason as `commit_account_setup` —
/// a half-written bunker account would brick login.
///
/// The seed is intentionally absent: bunker accounts have no local mnemonic
/// (the user's nsec lives on the remote signer; we only hold a client keypair
/// with no recovery phrase).
pub fn commit_bunker_account_setup(
    pkey: &str,
    encryption_enabled: bool,
    security_type: Option<&str>,
    bunker_url_stored: &str,
    bunker_remote_pubkey_hex: &str,
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
        tx.execute(
            "DELETE FROM settings WHERE key = 'security_type'",
            [],
        ).map_err(|e| format!("Failed to clear security_type: {}", e))?;
    }
    tx.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES ('signer_type', 'bunker')",
        [],
    ).map_err(|e| format!("Failed to set signer_type: {}", e))?;
    tx.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES ('bunker_url', ?1)",
        rusqlite::params![bunker_url_stored],
    ).map_err(|e| format!("Failed to set bunker_url: {}", e))?;
    tx.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES ('bunker_remote_pubkey', ?1)",
        rusqlite::params![bunker_remote_pubkey_hex],
    ).map_err(|e| format!("Failed to set bunker_remote_pubkey: {}", e))?;
    // Drop any stale seed from a previous local-account setup on this DB.
    tx.execute("DELETE FROM settings WHERE key = 'seed'", [])
        .map_err(|e| format!("Failed to clear stale seed: {}", e))?;
    tx.commit().map_err(|e| format!("Failed to commit tx: {}", e))?;
    Ok(())
}
