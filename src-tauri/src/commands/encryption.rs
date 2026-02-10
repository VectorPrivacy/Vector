//! Encryption toggle and migration commands.
//!
//! This module handles:
//! - Checking encryption status
//! - Bulk decryption migration (disable encryption)
//! - Bulk encryption migration (enable encryption)
//! - Event queue management during migration

use tauri::{command, AppHandle, Emitter, Runtime};
use crate::crypto::{encrypt_with_key, decrypt_with_key, is_encryption_enabled};
use crate::state::{close_processing_gate, open_processing_gate, set_encryption_enabled, PENDING_EVENTS};
use crate::stored_event::event_kind;

/// Progress update for encryption migration
#[derive(serde::Serialize, Clone)]
pub struct MigrationProgress {
    pub total: usize,
    pub completed: usize,
    pub phase: String,
}

/// Get current encryption status
///
/// If `npub` is provided, initializes the account's database first (if not already done).
/// This allows the frontend to check encryption status before prompting for PIN.
///
/// Returns: `{ enabled: bool, account_exists: bool }`
/// - `enabled`: whether encryption is enabled for this account
/// - `account_exists`: whether this account had an existing database (false = new account)
#[command]
pub async fn get_encryption_status<R: Runtime>(
    handle: AppHandle<R>,
    npub: Option<String>,
) -> Result<serde_json::Value, String> {
    let account_exists = if let Some(ref npub) = npub {
        // Check if this account's database already exists
        let profile_dir = crate::account_manager::get_profile_directory(&handle, npub)?;
        let db_exists = profile_dir.join("vector.db").exists();

        // Initialize the database (creates if needed, runs migrations)
        crate::account_manager::init_profile_database(&handle, npub).await?;

        // Set as current account so subsequent commands work
        crate::account_manager::set_current_account(npub.clone())?;

        db_exists
    } else {
        // No npub provided - assume account is already initialized
        true
    };

    // Initialize the cached flag from DB (first call seeds the AtomicBool)
    crate::state::init_encryption_enabled(&handle);
    let enabled = is_encryption_enabled();

    let security_type = if enabled {
        crate::db::get_sql_setting(handle.clone(), "security_type".to_string())
            .ok().flatten().unwrap_or_else(|| "pin".to_string())
    } else {
        "pin".to_string()
    };

    Ok(serde_json::json!({
        "enabled": enabled,
        "account_exists": account_exists,
        "security_type": security_type
    }))
}

/// Combined boot query: account existence + encryption status.
/// Account existence is derived from CURRENT_ACCOUNT (set by auto_select_account at startup).
/// Private key is NEVER returned — use login_from_stored_key to authenticate.
#[derive(serde::Serialize)]
pub struct BootEncryptionInfo {
    pub account_exists: bool,
    pub enabled: bool,
    pub security_type: String,
}

#[command]
pub fn get_encryption_and_key<R: Runtime>(handle: AppHandle<R>) -> Result<BootEncryptionInfo, String> {
    // auto_select_account already ran at Tauri startup — just check the result
    let has_account = crate::account_manager::get_current_account().is_ok();

    if !has_account {
        return Ok(BootEncryptionInfo {
            account_exists: false,
            enabled: false,
            security_type: "pin".to_string(),
        });
    }

    // Initialize the cached flag from DB (first call seeds the AtomicBool)
    crate::state::init_encryption_enabled(&handle);
    let enabled = is_encryption_enabled();

    let security_type = if enabled {
        crate::db::get_sql_setting(handle.clone(), "security_type".to_string())
            .ok().flatten().unwrap_or_else(|| "pin".to_string())
    } else {
        "pin".to_string()
    };

    Ok(BootEncryptionInfo { account_exists: true, enabled, security_type })
}

/// Disable encryption - bulk decrypt all encrypted content
///
/// This command:
/// 1. Closes the processing gate (queues incoming events)
/// 2. Bulk decrypts all message content, seed phrase, and PIVX keys
/// 3. Sets encryption_enabled = false
/// 4. Opens the gate and drains queued events
///
/// CRASH SAFE: All mutations wrapped in a single SQLite transaction.
/// If the app crashes before COMMIT, the database stays fully encrypted (audit C3/C4).
///
/// GATE SAFE: The processing gate is ALWAYS reopened on both success and error
/// paths (audit C2).
#[command]
pub async fn disable_encryption<R: Runtime>(handle: AppHandle<R>) -> Result<(), String> {
    // Close the processing gate — events are queued until we reopen
    close_processing_gate();

    // Do the actual migration work
    let result = disable_encryption_work(&handle);

    // ALWAYS reopen gate and drain queued events, regardless of success/failure.
    // This prevents the gate from being stuck closed forever (audit C2).
    drain_pending_events(&handle).await;

    match result {
        Ok(()) => {
            let _ = handle.emit("encryption_migration_complete", ());
            Ok(())
        }
        Err(e) => Err(e),
    }
}

/// Inner work for disable_encryption — separated so the outer function
/// can guarantee the processing gate is always reopened.
fn disable_encryption_work<R: Runtime>(handle: &AppHandle<R>) -> Result<(), String> {
    // Read the encryption key
    let key: [u8; 32] = {
        let guard = crate::ENCRYPTION_KEY.read().unwrap();
        (*guard).ok_or("No encryption key available".to_string())?
    };

    // Run transactional migration (all-or-nothing via SQLite transaction, audit C3/C4)
    disable_encryption_transactional(handle, &key)?;

    // Clear the encryption key from memory (only after successful commit)
    {
        let mut guard = crate::ENCRYPTION_KEY.write().unwrap();
        *guard = None;
    }

    Ok(())
}

/// Enable encryption - bulk encrypt all plaintext content
///
/// This command:
/// 1. Derives encryption key from credential (slow Argon2 step)
/// 2. Closes the processing gate
/// 3. Bulk encrypts all message content, seed phrase, and PIVX keys
/// 4. Sets encryption_enabled = true
/// 5. Opens the gate and drains queued events
///
/// CRASH SAFE: All mutations wrapped in a single SQLite transaction.
/// If the app crashes before COMMIT, the database stays fully plaintext (audit C3/C4).
///
/// GATE SAFE: The processing gate is ALWAYS reopened on both success and error
/// paths (audit C2).
#[command]
pub async fn enable_encryption<R: Runtime>(
    handle: AppHandle<R>,
    credential: String,
    security_type: String,
) -> Result<(), String> {
    // Derive key from credential (this is the slow Argon2 step)
    let key = crate::crypto::hash_pass(credential).await;
    {
        let mut guard = crate::ENCRYPTION_KEY.write().unwrap();
        *guard = Some(key);
    }

    // Close the processing gate
    close_processing_gate();

    // Do the actual migration work
    let result = enable_encryption_work(&handle, &security_type);

    // ALWAYS reopen gate and drain queued events, regardless of success/failure (audit C2)
    drain_pending_events(&handle).await;

    match result {
        Ok(()) => {
            let _ = handle.emit("encryption_migration_complete", ());
            Ok(())
        }
        Err(e) => {
            // Transaction rolled back — database is still plaintext.
            // Clear key from memory so maybe_encrypt doesn't encrypt new events
            // while old events remain plaintext (would create mixed state).
            {
                let mut guard = crate::ENCRYPTION_KEY.write().unwrap();
                *guard = None;
            }
            Err(e)
        }
    }
}

/// Inner work for enable_encryption — separated so the outer function
/// can guarantee the processing gate is always reopened.
fn enable_encryption_work<R: Runtime>(
    handle: &AppHandle<R>,
    security_type: &str,
) -> Result<(), String> {
    // Read the encryption key
    let key: [u8; 32] = {
        let guard = crate::ENCRYPTION_KEY.read().unwrap();
        (*guard).ok_or("No encryption key available".to_string())?
    };

    // Run transactional migration (all-or-nothing via SQLite transaction, audit C3/C4)
    enable_encryption_transactional(handle, &key, security_type)?;

    Ok(())
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Drain queued events after migration completes
///
/// IMPORTANT: Opens the processing gate INSIDE the lock to prevent race condition.
/// Without this, events could be pushed to the queue after we drain but before
/// the gate opens, causing them to be lost until the next migration.
async fn drain_pending_events<R: Runtime>(_handle: &AppHandle<R>) {
    let events = {
        let mut queue = PENDING_EVENTS.lock().await;
        // Open gate INSIDE the lock - this ensures no events can be
        // pushed after we drain (they'll go through normal processing instead)
        open_processing_gate();
        std::mem::take(&mut *queue)
    };

    let count = events.len();
    for (event, is_new) in events {
        crate::services::handle_event(event, is_new).await;
    }

    // Log drain stats
    println!("[Encryption] Drained {} queued events", count);
}

// ============================================================================
// Transactional Migrations (Crash-Safe)
// ============================================================================

/// Disable encryption inside a single SQLite transaction.
///
/// All mutations (event decryption, seed/pkey/PIVX decryption, flag updates)
/// are wrapped in one transaction on the write connection. If anything fails
/// or the app crashes before COMMIT, the database stays fully encrypted.
/// This prevents the mixed plaintext/ciphertext state (audit C3/C4).
///
/// Memory-efficient: collects only event IDs upfront, processes one at a time.
fn disable_encryption_transactional<R: Runtime>(
    handle: &AppHandle<R>,
    key: &[u8; 32],
) -> Result<(), String> {
    let mut conn = crate::account_manager::get_write_connection_guard(handle)?;
    let tx = conn.transaction()
        .map_err(|e| format!("Failed to begin transaction: {}", e))?;

    // Set migration state inside the transaction — rolls back with everything else on crash
    tx.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES ('migration_state', 'decrypting')",
        [],
    ).map_err(|e| format!("Failed to set migration_state: {}", e))?;

    // 1. Collect encrypted event IDs (memory-efficient: just ID strings)
    let all_ids: Vec<String> = {
        let mut stmt = tx.prepare(
            r#"SELECT id FROM events
               WHERE kind IN (?1, ?2, ?3)
               AND length(content) >= 56
               AND content NOT GLOB '*[^0-9a-f]*'"#,
        ).map_err(|e| format!("Failed to prepare ID query: {}", e))?;

        let rows = stmt.query_map(
            rusqlite::params![
                event_kind::MLS_CHAT_MESSAGE as i32,
                event_kind::PRIVATE_DIRECT_MESSAGE as i32,
                event_kind::MESSAGE_EDIT as i32,
            ],
            |row| row.get::<_, String>(0),
        ).map_err(|e| format!("Failed to query IDs: {}", e))?;

        rows.filter_map(|r| r.ok()).collect()
    };

    let total = all_ids.len();
    let mut completed = 0;
    let mut last_emitted_percent: i32 = -1;
    let mut skipped = 0;

    // 2. Decrypt each event within the transaction
    for id in &all_ids {
        let content: Option<String> = tx.query_row(
            "SELECT content FROM events WHERE id = ?1",
            rusqlite::params![id],
            |row| row.get(0),
        ).ok();

        if let Some(content) = content {
            match decrypt_with_key(&content, key) {
                Ok(plaintext) => {
                    tx.execute(
                        "UPDATE events SET content = ?1 WHERE id = ?2",
                        rusqlite::params![plaintext, id],
                    ).map_err(|e| format!("Failed to update event: {}", e))?;
                }
                Err(_) => {
                    // Content looks encrypted (hex) but isn't — skip it
                    println!("[Encryption] Skipping event {} - looks encrypted but isn't", id);
                    skipped += 1;
                }
            }
        }

        completed += 1;
        let current_percent = if total > 0 {
            ((completed as f64 / total as f64) * 100.0) as i32
        } else {
            100
        };

        if current_percent >= last_emitted_percent + 5 {
            last_emitted_percent = current_percent;
            let _ = handle.emit(
                "encryption_migration_progress",
                MigrationProgress {
                    total,
                    completed,
                    phase: "decrypting".to_string(),
                },
            );
        }
    }

    // 3. Decrypt settings and PIVX keys within the same transaction
    let _ = handle.emit(
        "encryption_migration_progress",
        MigrationProgress {
            total,
            completed,
            phase: "finalizing".to_string(),
        },
    );

    decrypt_setting_in_tx(&tx, "seed", key, |v| v.contains(' '))?;
    decrypt_setting_in_tx(&tx, "pkey", key, |v| v.starts_with("nsec"))?;
    decrypt_pivx_in_tx(&tx, key)?;

    // 4. Verify plaintext state within the transaction (before committing)
    verify_plaintext_state_in_tx(&tx)?;

    // 5. Update flags within the transaction
    tx.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES ('encryption_enabled', 'false')",
        [],
    ).map_err(|e| format!("Failed to update encryption_enabled: {}", e))?;

    tx.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES ('migration_state', '')",
        [],
    ).map_err(|e| format!("Failed to clear migration_state: {}", e))?;

    // 6. COMMIT — the atomic point. Everything succeeds or nothing does.
    tx.commit().map_err(|e| format!("Failed to commit disable transaction: {}", e))?;

    // Update cached flag now that transaction committed successfully
    set_encryption_enabled(false);

    if skipped > 0 {
        println!("[Encryption] Skipped {} false-positive hex events", skipped);
    }
    println!("[Encryption] Disable complete: decrypted {} events", total - skipped);

    Ok(())
}

/// Enable encryption inside a single SQLite transaction.
///
/// All mutations (event encryption, seed/pkey/PIVX encryption, flag updates)
/// are wrapped in one transaction on the write connection. If anything fails
/// or the app crashes before COMMIT, the database stays fully plaintext.
/// This prevents the mixed plaintext/ciphertext state (audit C3/C4).
///
/// Memory-efficient: collects only event IDs upfront, processes one at a time.
fn enable_encryption_transactional<R: Runtime>(
    handle: &AppHandle<R>,
    key: &[u8; 32],
    security_type: &str,
) -> Result<(), String> {
    let mut conn = crate::account_manager::get_write_connection_guard(handle)?;
    let tx = conn.transaction()
        .map_err(|e| format!("Failed to begin transaction: {}", e))?;

    // Set migration state inside the transaction — rolls back with everything else on crash
    tx.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES ('migration_state', 'encrypting')",
        [],
    ).map_err(|e| format!("Failed to set migration_state: {}", e))?;

    // 1. Collect plaintext event IDs (memory-efficient: just ID strings)
    let all_ids: Vec<String> = {
        let mut stmt = tx.prepare(
            r#"SELECT id FROM events
               WHERE kind IN (?1, ?2, ?3)
               AND (length(content) < 56 OR content GLOB '*[^0-9a-f]*')"#,
        ).map_err(|e| format!("Failed to prepare ID query: {}", e))?;

        let rows = stmt.query_map(
            rusqlite::params![
                event_kind::MLS_CHAT_MESSAGE as i32,
                event_kind::PRIVATE_DIRECT_MESSAGE as i32,
                event_kind::MESSAGE_EDIT as i32,
            ],
            |row| row.get::<_, String>(0),
        ).map_err(|e| format!("Failed to query IDs: {}", e))?;

        rows.filter_map(|r| r.ok()).collect()
    };

    let total = all_ids.len();
    let mut completed = 0;
    let mut last_emitted_percent: i32 = -1;

    // 2. Encrypt each event within the transaction
    for id in &all_ids {
        let content: Option<String> = tx.query_row(
            "SELECT content FROM events WHERE id = ?1",
            rusqlite::params![id],
            |row| row.get(0),
        ).ok();

        if let Some(content) = content {
            let encrypted = encrypt_with_key(&content, key);
            tx.execute(
                "UPDATE events SET content = ?1 WHERE id = ?2",
                rusqlite::params![encrypted, id],
            ).map_err(|e| format!("Failed to update event: {}", e))?;
        }

        completed += 1;
        let current_percent = if total > 0 {
            ((completed as f64 / total as f64) * 100.0) as i32
        } else {
            100
        };

        if current_percent >= last_emitted_percent + 5 {
            last_emitted_percent = current_percent;
            let _ = handle.emit(
                "encryption_migration_progress",
                MigrationProgress {
                    total,
                    completed,
                    phase: "encrypting".to_string(),
                },
            );
        }
    }

    // 3. Encrypt settings and PIVX keys within the same transaction
    let _ = handle.emit(
        "encryption_migration_progress",
        MigrationProgress {
            total,
            completed,
            phase: "finalizing".to_string(),
        },
    );

    encrypt_setting_in_tx(&tx, "seed", key, |v| v.contains(' '))?;
    encrypt_setting_in_tx(&tx, "pkey", key, |v| v.starts_with("nsec"))?;
    encrypt_pivx_in_tx(&tx, key)?;

    // 4. Verify encrypted state within the transaction (before committing)
    verify_encrypted_state_in_tx(&tx, key)?;

    // 5. Update flags within the transaction
    tx.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES ('encryption_enabled', 'true')",
        [],
    ).map_err(|e| format!("Failed to update encryption_enabled: {}", e))?;

    tx.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES ('security_type', ?1)",
        rusqlite::params![security_type],
    ).map_err(|e| format!("Failed to update security_type: {}", e))?;

    tx.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES ('migration_state', '')",
        [],
    ).map_err(|e| format!("Failed to clear migration_state: {}", e))?;

    // 6. COMMIT — the atomic point. Everything succeeds or nothing does.
    tx.commit().map_err(|e| format!("Failed to commit enable transaction: {}", e))?;

    // Update cached flag now that transaction committed successfully
    set_encryption_enabled(true);

    println!("[Encryption] Enable complete: encrypted {} events", total);

    Ok(())
}

// ============================================================================
// In-Transaction Helpers
// ============================================================================

/// Decrypt a setting value within a transaction.
fn decrypt_setting_in_tx(
    tx: &rusqlite::Transaction,
    key_name: &str,
    key: &[u8; 32],
    is_plaintext: fn(&str) -> bool,
) -> Result<(), String> {
    let val: Option<String> = tx.query_row(
        "SELECT value FROM settings WHERE key = ?1",
        rusqlite::params![key_name],
        |row| row.get(0),
    ).ok();

    if let Some(current) = val {
        if is_plaintext(&current) {
            return Ok(()); // Already plaintext, nothing to decrypt
        }

        let decrypted = decrypt_with_key(&current, key)
            .map_err(|_| format!("Failed to decrypt setting '{}'", key_name))?;

        tx.execute(
            "UPDATE settings SET value = ?1 WHERE key = ?2",
            rusqlite::params![decrypted, key_name],
        ).map_err(|e| format!("Failed to update setting '{}': {}", key_name, e))?;
    }

    Ok(())
}

/// Encrypt a setting value within a transaction.
/// Only encrypts if the value is currently plaintext.
fn encrypt_setting_in_tx(
    tx: &rusqlite::Transaction,
    key_name: &str,
    key: &[u8; 32],
    is_plaintext: fn(&str) -> bool,
) -> Result<(), String> {
    let val: Option<String> = tx.query_row(
        "SELECT value FROM settings WHERE key = ?1",
        rusqlite::params![key_name],
        |row| row.get(0),
    ).ok();

    if let Some(current) = val {
        if is_plaintext(&current) {
            let encrypted = encrypt_with_key(&current, key);
            tx.execute(
                "UPDATE settings SET value = ?1 WHERE key = ?2",
                rusqlite::params![encrypted, key_name],
            ).map_err(|e| format!("Failed to update setting '{}': {}", key_name, e))?;
        }
    }

    Ok(())
}

/// Decrypt all PIVX promo private keys within a transaction.
fn decrypt_pivx_in_tx(
    tx: &rusqlite::Transaction,
    key: &[u8; 32],
) -> Result<(), String> {
    let keys: Vec<(i64, String)> = {
        let mut stmt = tx.prepare("SELECT id, privkey_encrypted FROM pivx_promos")
            .map_err(|e| format!("Failed to prepare PIVX query: {}", e))?;
        let rows = stmt.query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)))
            .map_err(|e| format!("Failed to query PIVX promos: {}", e))?;
        rows.filter_map(|r| r.ok()).collect()
    };

    for (id, key_val) in keys {
        // Only decrypt if longer than a raw key (64 chars) — meaning it's encrypted
        if key_val.len() > 64 {
            let decrypted = decrypt_with_key(&key_val, key)
                .map_err(|_| format!("Failed to decrypt PIVX key {}", id))?;
            tx.execute(
                "UPDATE pivx_promos SET privkey_encrypted = ?1 WHERE id = ?2",
                rusqlite::params![decrypted, id],
            ).map_err(|e| format!("Failed to update PIVX key: {}", e))?;
        }
    }

    Ok(())
}

/// Encrypt all PIVX promo private keys within a transaction.
fn encrypt_pivx_in_tx(
    tx: &rusqlite::Transaction,
    key: &[u8; 32],
) -> Result<(), String> {
    let keys: Vec<(i64, String)> = {
        let mut stmt = tx.prepare("SELECT id, privkey_encrypted FROM pivx_promos")
            .map_err(|e| format!("Failed to prepare PIVX query: {}", e))?;
        let rows = stmt.query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)))
            .map_err(|e| format!("Failed to query PIVX promos: {}", e))?;
        rows.filter_map(|r| r.ok()).collect()
    };

    for (id, key_val) in keys {
        // Raw PIVX keys are exactly 64 hex chars; encrypted output is always longer
        if key_val.len() <= 64 {
            let encrypted = encrypt_with_key(&key_val, key);
            tx.execute(
                "UPDATE pivx_promos SET privkey_encrypted = ?1 WHERE id = ?2",
                rusqlite::params![encrypted, id],
            ).map_err(|e| format!("Failed to update PIVX key: {}", e))?;
        }
    }

    Ok(())
}

// ============================================================================
// Post-Migration Verification (In-Transaction)
// ============================================================================

/// Verify all critical data is plaintext within a transaction.
/// Catches any bugs (double-encryption, missed fields) BEFORE committing.
fn verify_plaintext_state_in_tx(tx: &rusqlite::Transaction) -> Result<(), String> {
    // Verify pkey is plaintext (starts with "nsec")
    if let Ok(pkey) = tx.query_row::<String, _, _>(
        "SELECT value FROM settings WHERE key = 'pkey'", [], |row| row.get(0),
    ) {
        if !pkey.starts_with("nsec") {
            return Err(format!(
                "VERIFICATION FAILED: pkey is not plaintext after decryption (len={}, prefix={}). \
                 Aborting to protect your data — encryption status was NOT changed.",
                pkey.len(), &pkey[..pkey.len().min(8)]
            ));
        }
    }

    // Verify seed is plaintext (contains spaces = BIP39 mnemonic)
    if let Ok(seed) = tx.query_row::<String, _, _>(
        "SELECT value FROM settings WHERE key = 'seed'", [], |row| row.get(0),
    ) {
        if !seed.contains(' ') {
            return Err(
                "VERIFICATION FAILED: seed is not plaintext after decryption. \
                 Aborting to protect your data — encryption status was NOT changed."
                    .to_string(),
            );
        }
    }

    // Verify PIVX keys are raw (exactly 64 hex chars)
    let mut stmt = tx.prepare("SELECT id, length(privkey_encrypted) FROM pivx_promos")
        .map_err(|e| format!("Verification query failed: {}", e))?;
    let bad_keys: Vec<i64> = stmt
        .query_map([], |row| {
            let id: i64 = row.get(0)?;
            let len: i64 = row.get(1)?;
            Ok((id, len))
        })
        .map_err(|e| format!("Verification query failed: {}", e))?
        .filter_map(|r| r.ok())
        .filter(|(_, len)| *len > 64)
        .map(|(id, _)| id)
        .collect();

    if !bad_keys.is_empty() {
        return Err(format!(
            "VERIFICATION FAILED: {} PIVX key(s) still encrypted (IDs: {:?}). \
             Aborting to protect your data — encryption status was NOT changed.",
            bad_keys.len(), bad_keys
        ));
    }

    println!("[Encryption] Verification passed: all data confirmed plaintext");
    Ok(())
}

/// Verify all critical data is encrypted within a transaction.
/// Decrypts each value to confirm it round-trips back to valid plaintext.
fn verify_encrypted_state_in_tx(
    tx: &rusqlite::Transaction,
    key: &[u8; 32],
) -> Result<(), String> {
    // Verify pkey is encrypted and round-trips to nsec
    if let Ok(pkey) = tx.query_row::<String, _, _>(
        "SELECT value FROM settings WHERE key = 'pkey'", [], |row| row.get(0),
    ) {
        if pkey.starts_with("nsec") {
            return Err(
                "VERIFICATION FAILED: pkey is still plaintext after encryption. \
                 Aborting to protect your data — encryption status was NOT changed."
                    .to_string(),
            );
        }
        match decrypt_with_key(&pkey, key) {
            Ok(decrypted) if decrypted.starts_with("nsec") => {}
            Ok(decrypted) => {
                return Err(format!(
                    "VERIFICATION FAILED: pkey decrypts but not to nsec (len={}, prefix={}). \
                     Possible double-encryption. Aborting.",
                    decrypted.len(), &decrypted[..decrypted.len().min(6)]
                ));
            }
            Err(_) => {
                return Err(
                    "VERIFICATION FAILED: pkey cannot be decrypted with current key. Aborting."
                        .to_string(),
                );
            }
        }
    }

    // Verify seed is encrypted and round-trips to BIP39
    if let Ok(seed) = tx.query_row::<String, _, _>(
        "SELECT value FROM settings WHERE key = 'seed'", [], |row| row.get(0),
    ) {
        if seed.contains(' ') {
            return Err(
                "VERIFICATION FAILED: seed is still plaintext after encryption. Aborting."
                    .to_string(),
            );
        }
        match decrypt_with_key(&seed, key) {
            Ok(decrypted) if decrypted.contains(' ') => {}
            Ok(_) => {
                return Err(
                    "VERIFICATION FAILED: seed decrypts but not to BIP39 mnemonic. \
                     Possible double-encryption. Aborting."
                        .to_string(),
                );
            }
            Err(_) => {
                return Err(
                    "VERIFICATION FAILED: seed cannot be decrypted with current key. Aborting."
                        .to_string(),
                );
            }
        }
    }

    println!("[Encryption] Verification passed: all data confirmed encrypted and round-trips correctly");
    Ok(())
}

// ============================================================================
// Credential Verification (no secrets cross IPC)
// ============================================================================

/// Verify a credential (PIN/password) without returning any key material.
///
/// Reads the encrypted pkey from the database, derives the Argon2 key from
/// the given credential, and attempts to decrypt. Returns Ok(()) if the
/// credential is correct, Err otherwise. The private key never leaves Rust.
#[command]
pub async fn verify_credential<R: Runtime>(
    handle: AppHandle<R>,
    credential: String,
) -> Result<(), String> {
    let key = crate::crypto::hash_pass(credential).await;

    let conn = crate::account_manager::get_db_connection_guard(&handle)?;
    let pkey: Option<String> = conn
        .query_row("SELECT value FROM settings WHERE key = 'pkey'", [], |row| row.get(0))
        .ok();

    if let Some(ref encrypted_pkey) = pkey {
        match decrypt_with_key(encrypted_pkey, &key) {
            Ok(decrypted) if decrypted.starts_with("nsec") => Ok(()),
            _ => Err("Incorrect credential.".to_string()),
        }
    } else {
        Err("No private key found — cannot verify credential.".to_string())
    }
}

// ============================================================================
// Re-Keying (Change PIN/Password)
// ============================================================================

/// Re-key all encrypted data with a new credential (PIN or password).
///
/// Forensically safe: plaintext never touches disk — each row is decrypted in
/// memory and immediately re-encrypted with the new key before being written.
///
/// CRASH SAFE: The entire re-key is wrapped in a single SQLite transaction.
/// If ANY step fails (or the app crashes before COMMIT), the transaction
/// auto-rolls back and the database remains entirely on the old key.
/// This prevents the catastrophic "split-key" state (audit C1).
///
/// GATE SAFE: The processing gate is ALWAYS reopened on both success and
/// error paths (audit C2).
#[command]
pub async fn rekey_encryption<R: Runtime>(
    handle: AppHandle<R>,
    old_credential: String,
    new_credential: String,
    security_type: String,
) -> Result<(), String> {
    // 1. Derive old key and verify it by test-decrypting pkey
    let old_key = crate::crypto::hash_pass(old_credential).await;
    {
        let conn = crate::account_manager::get_db_connection_guard(&handle)?;
        let pkey: Option<String> = conn
            .query_row("SELECT value FROM settings WHERE key = 'pkey'", [], |row| row.get(0))
            .ok();

        if let Some(ref encrypted_pkey) = pkey {
            match decrypt_with_key(encrypted_pkey, &old_key) {
                Ok(decrypted) if decrypted.starts_with("nsec") => {}
                _ => return Err("Incorrect current credential.".to_string()),
            }
        } else {
            return Err("No private key found — cannot verify credential.".to_string());
        }
    }

    // 2. Derive new key
    let new_key = crate::crypto::hash_pass(new_credential).await;

    // 3. Close processing gate
    close_processing_gate();

    // 4. Perform transactional re-key (all-or-nothing via SQLite transaction)
    let result = rekey_encryption_transactional(&handle, &old_key, &new_key, &security_type);

    // 5. ALWAYS reopen gate and drain queued events (audit C2)
    drain_pending_events(&handle).await;

    match result {
        Ok(()) => {
            // Update global ENCRYPTION_KEY to new key ONLY after successful commit
            {
                let mut guard = crate::ENCRYPTION_KEY.write().unwrap();
                *guard = Some(new_key);
            }
            let _ = handle.emit("encryption_migration_complete", ());
            println!("[Rekey] Re-keying complete");
            Ok(())
        }
        Err(e) => Err(e),
    }
}

/// Perform the entire re-key operation inside a single SQLite transaction.
///
/// If ANY step fails (or the app crashes), the transaction auto-rolls back
/// and the database remains entirely on the old key. This prevents the
/// catastrophic "split-key" state where some data uses the old key and
/// some uses the new key (audit C1).
///
/// Memory-efficient: collects only event IDs upfront (small strings),
/// then processes one event at a time within the transaction.
fn rekey_encryption_transactional<R: Runtime>(
    handle: &AppHandle<R>,
    old_key: &[u8; 32],
    new_key: &[u8; 32],
    security_type: &str,
) -> Result<(), String> {
    use crate::crypto::{encrypt_with_key, decrypt_with_key};

    let mut conn = crate::account_manager::get_write_connection_guard(handle)?;

    // Begin transaction — auto-rolls back on drop if not committed
    let tx = conn.transaction()
        .map_err(|e| format!("Failed to begin transaction: {}", e))?;

    // Set migration state inside the transaction — rolls back with everything else on crash
    tx.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES ('migration_state', 'rekeying')",
        [],
    ).map_err(|e| format!("Failed to set migration_state: {}", e))?;

    // 1. Collect all encrypted event IDs (memory-efficient: just ID strings)
    let all_ids: Vec<String> = {
        let mut stmt = tx.prepare(
            r#"SELECT id FROM events
               WHERE kind IN (?1, ?2, ?3)
               AND length(content) >= 56
               AND content NOT GLOB '*[^0-9a-f]*'"#,
        ).map_err(|e| format!("Failed to prepare ID query: {}", e))?;

        let rows = stmt.query_map(
            rusqlite::params![
                event_kind::MLS_CHAT_MESSAGE as i32,
                event_kind::PRIVATE_DIRECT_MESSAGE as i32,
                event_kind::MESSAGE_EDIT as i32,
            ],
            |row| row.get::<_, String>(0),
        ).map_err(|e| format!("Failed to query IDs: {}", e))?;

        rows.filter_map(|r| r.ok()).collect()
    };

    let total = all_ids.len();
    let mut completed = 0;
    let mut last_emitted_percent: i32 = -1;

    // 2. Re-key each event (one at a time, memory-efficient)
    for id in &all_ids {
        let content: Option<String> = tx.query_row(
            "SELECT content FROM events WHERE id = ?1",
            rusqlite::params![id],
            |row| row.get(0),
        ).ok();

        if let Some(content) = content {
            match decrypt_with_key(&content, old_key) {
                Ok(plaintext) => {
                    let re_encrypted = encrypt_with_key(&plaintext, new_key);
                    tx.execute(
                        "UPDATE events SET content = ?1 WHERE id = ?2",
                        rusqlite::params![re_encrypted, id],
                    ).map_err(|e| format!("Failed to update event: {}", e))?;
                }
                Err(_) => {
                    // Content looks encrypted but can't be decrypted — skip
                    println!("[Rekey] Skipping event {} - decrypt failed", id);
                }
            }
        }

        completed += 1;
        let current_percent = if total > 0 {
            ((completed as f64 / total as f64) * 100.0) as i32
        } else {
            100
        };

        if current_percent >= last_emitted_percent + 5 {
            last_emitted_percent = current_percent;
            let _ = handle.emit(
                "encryption_migration_progress",
                MigrationProgress {
                    total,
                    completed,
                    phase: "rekeying".to_string(),
                },
            );
        }
    }

    // 3. Re-key settings within the same transaction
    let _ = handle.emit(
        "encryption_migration_progress",
        MigrationProgress {
            total,
            completed,
            phase: "finalizing".to_string(),
        },
    );

    rekey_setting_in_tx(&tx, "pkey", old_key, new_key)?;
    rekey_setting_in_tx(&tx, "seed", old_key, new_key)?;
    rekey_pivx_in_tx(&tx, old_key, new_key)?;

    // 4. Verify re-keyed state within the transaction (before committing)
    verify_encrypted_state_in_tx(&tx, new_key)?;

    // 5. Update metadata within the same transaction
    tx.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES ('security_type', ?1)",
        rusqlite::params![security_type],
    ).map_err(|e| format!("Failed to update security_type: {}", e))?;

    tx.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES ('migration_state', '')",
        [],
    ).map_err(|e| format!("Failed to clear migration_state: {}", e))?;

    // 6. COMMIT — the atomic point. Everything succeeds or nothing does.
    tx.commit().map_err(|e| format!("Failed to commit re-key transaction: {}", e))?;

    Ok(())
}

/// Re-key a single settings value within a transaction.
fn rekey_setting_in_tx(
    tx: &rusqlite::Transaction,
    key: &str,
    old_key: &[u8; 32],
    new_key: &[u8; 32],
) -> Result<(), String> {
    use crate::crypto::{encrypt_with_key, decrypt_with_key};

    let val: Option<String> = tx.query_row(
        "SELECT value FROM settings WHERE key = ?1",
        rusqlite::params![key],
        |row| row.get(0),
    ).ok();

    if let Some(encrypted_val) = val {
        let plaintext = decrypt_with_key(&encrypted_val, old_key)
            .map_err(|_| format!("Failed to decrypt setting '{}'", key))?;

        let re_encrypted = encrypt_with_key(&plaintext, new_key);

        tx.execute(
            "UPDATE settings SET value = ?1 WHERE key = ?2",
            rusqlite::params![re_encrypted, key],
        ).map_err(|e| format!("Failed to update setting '{}': {}", key, e))?;
    }

    Ok(())
}

/// Re-key all PIVX promo private keys within a transaction.
fn rekey_pivx_in_tx(
    tx: &rusqlite::Transaction,
    old_key: &[u8; 32],
    new_key: &[u8; 32],
) -> Result<(), String> {
    use crate::crypto::{encrypt_with_key, decrypt_with_key};

    let keys: Vec<(i64, String)> = {
        let mut stmt = tx.prepare("SELECT id, privkey_encrypted FROM pivx_promos")
            .map_err(|e| format!("Failed to prepare PIVX query: {}", e))?;
        let rows = stmt.query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)))
            .map_err(|e| format!("Failed to query PIVX promos: {}", e))?;
        rows.filter_map(|r| r.ok()).collect()
    };

    for (id, key_val) in keys {
        // Only re-key if longer than a raw key (64 chars) — meaning it's encrypted
        if key_val.len() > 64 {
            let decrypted = decrypt_with_key(&key_val, old_key)
                .map_err(|_| format!("Failed to decrypt PIVX key {}", id))?;
            let re_encrypted = encrypt_with_key(&decrypted, new_key);

            tx.execute(
                "UPDATE pivx_promos SET privkey_encrypted = ?1 WHERE id = ?2",
                rusqlite::params![re_encrypted, id],
            ).map_err(|e| format!("Failed to update PIVX key: {}", e))?;
        }
    }

    Ok(())
}
