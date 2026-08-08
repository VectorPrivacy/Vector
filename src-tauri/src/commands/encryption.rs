//! Encryption toggle and migration commands.
//!
//! This module handles:
//! - Checking encryption status
//! - Bulk decryption migration (disable encryption)
//! - Bulk encryption migration (enable encryption)
//! - Event queue management during migration

use tauri::{command, AppHandle, Emitter, Runtime};
use zeroize::Zeroize;
use crate::crypto::{encrypt_with_key, decrypt_with_key};
use crate::state::{close_processing_gate, open_processing_gate, set_encryption_enabled, PENDING_EVENTS};
use crate::stored_event::event_kind;

/// Set when an encryption migration (encrypt/decrypt/rekey) is mid-flight.
/// `reset_session()` checks this to refuse a swap while data is being
/// transformed — yanking the DB connection mid-transaction would leave the
/// account in a half-migrated state.
pub(crate) static MIGRATION_IN_PROGRESS: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// RAII guard for `MIGRATION_IN_PROGRESS` — clears the flag on drop so a
/// migration that returns early or panics can't leave the guard stuck.
struct MigrationGuard;
impl MigrationGuard {
    /// Exclusive entry: two concurrent migrations racing the same vault is
    /// the split-key shape all over again (the loser's rollback clears the
    /// winner's key), so a second entrant is refused, never queued.
    fn try_enter() -> Result<Self, String> {
        MIGRATION_IN_PROGRESS
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            )
            .map_err(|_| "A migration is already in progress".to_string())?;
        Ok(Self)
    }
}
impl Drop for MigrationGuard {
    fn drop(&mut self) {
        MIGRATION_IN_PROGRESS.store(false, std::sync::atomic::Ordering::Release);
    }
}

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
    // then read back from the same atomic so we agree with the rest of the
    // app about the missing-row case (resolved via `security_type`).
    crate::state::init_encryption_enabled();
    let enabled = vector_core::state::is_encryption_enabled_fast();

    let security_type = if enabled {
        crate::db::get_sql_setting("security_type".to_string())
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
/// Account existence is derived from CURRENT_ACCOUNT (set by boot_select_account at startup).
/// Private key is NEVER returned — use login_from_stored_key to authenticate.
#[derive(serde::Serialize)]
pub struct BootEncryptionInfo {
    pub account_exists: bool,
    pub enabled: bool,
    pub security_type: String,
    /// "local" or "bunker". Lets the frontend tweak loading copy ("Connecting
    /// to Signer…" instead of "Connecting…") for bunker accounts at boot.
    pub signer_type: String,
}

#[command]
pub fn get_encryption_and_key<R: Runtime>(handle: AppHandle<R>) -> Result<BootEncryptionInfo, String> {
    // CURRENT_ACCOUNT may be None when called after a webview reload that
    // followed a `swap_session` — `boot_select_account` only runs at Tauri
    // startup, not on reload. Honor the active-account marker file here so
    // the post-swap reload lands on the right account. If the marker is
    // missing AND multiple accounts exist on disk, recover by pointing at
    // the first one — without this the user gets dumped on the bare
    // Create / Login screen even when they have valid accounts available
    // (e.g. an old delete-flow run that didn't repoint the marker, or
    // Add Profile abort paths). The frontend's picker pill then lets them
    // jump to the account they actually wanted.
    // CURRENT_ACCOUNT is set by `boot_select_account` at Tauri startup.
    // It can be None at this point in two cases:
    //   1. Multiple accounts on disk with no marker — boot intentionally
    //      returned None so the frontend can show the picker.
    //   2. A `swap_session` cleared in-memory state, then the frontend
    //      reloaded; only the marker file (set by `set_active_account`
    //      before the swap) tells us where to land.
    //
    // We honor the marker but never auto-pick from `list_accounts` —
    // "first alphabetically" is the wrong default for multi-account
    // installs; the frontend's picker pill exists precisely so the user
    // can make this choice.
    let has_account = if crate::account_manager::get_current_account().is_ok() {
        true
    } else if let Some(npub) = vector_core::db::read_active_account_file().ok().flatten() {
        match crate::account_manager::set_current_account(npub.clone()) {
            Ok(()) => {
                let _ = vector_core::db::init_database(&npub);
                true
            }
            Err(_) => false,
        }
    } else {
        false
    };

    if !has_account {
        return Ok(BootEncryptionInfo {
            account_exists: false,
            enabled: false,
            security_type: "pin".to_string(),
            signer_type: "local".to_string(),
        });
    }
    let _ = handle;

    // Seed the cached flag from the new account's DB and read it back from
    // the same atomic. Historically the two helpers had divergent missing-
    // row defaults (one defaulted false, the other true), which silently
    // mis-routed boots through the wrong login path after a swap. Both now
    // delegate to `state::resolve_encryption_enabled_from_db` so this is
    // robust against either being called independently.
    crate::state::init_encryption_enabled();
    let mut enabled = vector_core::state::is_encryption_enabled_fast();

    // Self-heal: legacy accounts predating the `encryption_enabled` row
    // store an encrypted pkey with no flag set. A pkey that doesn't begin
    // with `nsec1` is ciphertext — backfill the flags so the boot routes
    // to the PIN screen. Default to "pin" because passwords didn't exist
    // before the security_type row was introduced.
    let mut healed_security_type: Option<String> = None;
    if !enabled {
        if let Ok(Some(stored)) = vector_core::db::get_pkey() {
            if !stored.starts_with("nsec1") {
                let _ = vector_core::db::set_sql_setting(
                    "encryption_enabled".to_string(),
                    "true".to_string(),
                );
                if crate::db::get_sql_setting("security_type".to_string())
                    .ok().flatten().is_none()
                {
                    let _ = vector_core::db::set_sql_setting(
                        "security_type".to_string(),
                        "pin".to_string(),
                    );
                    healed_security_type = Some("pin".to_string());
                }
                vector_core::state::set_encryption_enabled(true);
                enabled = true;
            }
        }
    }

    let security_type = if enabled {
        healed_security_type
            .or_else(|| crate::db::get_sql_setting("security_type".to_string()).ok().flatten())
            .unwrap_or_else(|| "pin".to_string())
    } else {
        "pin".to_string()
    };

    let signer_type = vector_core::db::get_signer_type()
        .unwrap_or_else(|_| "local".to_string());

    Ok(BootEncryptionInfo { account_exists: true, enabled, security_type, signer_type })
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
    // Mark migration in flight so reset_session() refuses to fire mid-tx.
    let _guard = MigrationGuard::try_enter()?;

    // Close the processing gate — events are queued until we reopen
    close_processing_gate();

    // Do the actual migration work
    let result = disable_encryption_work(&handle);

    // ALWAYS reopen gate and drain queued events, regardless of success/failure.
    // This prevents the gate from being stuck closed forever (audit C2).
    drain_pending_events(&handle).await;

    match result {
        Ok(()) => {
            // No vault key means nothing for biometrics to unlock.
            crate::commands::biometric::clear_biometric_enrollment();
            let _ = handle.emit("encryption_migration_complete", ());
            Ok(())
        }
        Err(e) => Err(e),
    }
}

/// Inner work for disable_encryption — separated so the outer function
/// can guarantee the processing gate is always reopened.
fn disable_encryption_work<R: Runtime>(handle: &AppHandle<R>) -> Result<(), String> {
    // Read the encryption key from the guarded vault
    let mut key: [u8; 32] = crate::ENCRYPTION_KEY.get()
        .ok_or("No encryption key available".to_string())?;

    // Run transactional migration (all-or-nothing via SQLite transaction, audit C3/C4)
    let result = disable_encryption_transactional(handle, &key);

    // Zeroize local key copy
    key.zeroize();

    // Clear the guarded vault (only after successful commit)
    if result.is_ok() {
        crate::ENCRYPTION_KEY.clear(&[&crate::MY_SECRET_KEY]);
    }

    result
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
    biometric_wrap: Option<String>,
) -> Result<(), String> {
    let _guard = MigrationGuard::try_enter()?;
    // Already-enabled guard: re-running would derive a key from the given
    // credential, fail verification against the existing ciphertext, roll
    // back, and CLEAR the correct in-session vault key on the way out.
    if vector_core::state::is_encryption_enabled_fast() {
        return Err("Encryption is already enabled".to_string());
    }
    // Derive key from credential (this is the slow Argon2 step)
    let key = crate::crypto::hash_pass(credential).await;
    crate::ENCRYPTION_KEY.set(key, &[&crate::MY_SECRET_KEY]);

    // Keyless account: the canary (its only wrong-PIN detector at boot) rides
    // the migration transaction below, encrypted under the new key directly.
    let nip55_canary = if vector_core::signer_kind() == vector_core::SignerKind::Nip55 {
        Some(encrypt_with_key(crate::commands::account::NIP55_PIN_CANARY, &key))
    } else {
        None
    };

    // Close the processing gate
    close_processing_gate();

    // Do the actual migration work
    let result = enable_encryption_work(&handle, &security_type, biometric_wrap.as_deref(), nip55_canary.as_deref());

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
            crate::ENCRYPTION_KEY.clear(&[&crate::MY_SECRET_KEY]);
            Err(e)
        }
    }
}

/// Inner work for enable_encryption — separated so the outer function
/// can guarantee the processing gate is always reopened.
fn enable_encryption_work<R: Runtime>(
    handle: &AppHandle<R>,
    security_type: &str,
    biometric_wrap: Option<&str>,
    nip55_canary: Option<&str>,
) -> Result<(), String> {
    // Read the encryption key from the guarded vault
    let mut key: [u8; 32] = crate::ENCRYPTION_KEY.get()
        .ok_or("No encryption key available".to_string())?;

    // Run transactional migration (all-or-nothing via SQLite transaction, audit C3/C4)
    let result = enable_encryption_transactional(handle, &key, security_type, biometric_wrap, nip55_canary);
    key.zeroize();

    result
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
                event_kind::CHAT_MESSAGE as i32,
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
    decrypt_setting_in_tx(&tx, "bunker_url", key, |v| v.starts_with("bunker://"))?;
    decrypt_pivx_in_tx(&tx, key)?;
    decrypt_community_in_tx(&tx, key)?;

    // 4. Verify plaintext state within the transaction (before committing)
    verify_plaintext_state_in_tx(&tx)?;

    // 5. Update flags within the transaction
    tx.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES ('encryption_enabled', 'false')",
        [],
    ).map_err(|e| format!("Failed to update encryption_enabled: {}", e))?;

    // Plaintext account needs no wrong-PIN detector and no unlock method —
    // drop both in-tx so no reader can infer a mode that no longer exists.
    tx.execute(
        "DELETE FROM settings WHERE key = 'nip55_pin_check'",
        [],
    ).map_err(|e| format!("Failed to clear pin canary: {}", e))?;
    tx.execute(
        "DELETE FROM settings WHERE key = 'security_type'",
        [],
    ).map_err(|e| format!("Failed to clear security_type: {}", e))?;

    // The biometric wrap holds the PRE-migration derived key — drop it in the
    // SAME transaction so no crash window can leave a stale wrap behind.
    tx.execute(
        "DELETE FROM settings WHERE key = 'biometric_wrapped_key'",
        [],
    ).map_err(|e| format!("Failed to clear biometric wrap: {}", e))?;

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
    biometric_wrap: Option<&str>,
    nip55_canary: Option<&str>,
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
                event_kind::CHAT_MESSAGE as i32,
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
    encrypt_setting_in_tx(&tx, "bunker_url", key, |v| v.starts_with("bunker://"))?;
    encrypt_pivx_in_tx(&tx, key)?;
    encrypt_community_in_tx(&tx, key)?;

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

    // Keyless-account canary: same atomicity rule as the wrap — the only
    // wrong-PIN detector must land with the flags that make it load-bearing.
    if let Some(c) = nip55_canary {
        tx.execute(
            "INSERT OR REPLACE INTO settings (key, value) VALUES ('nip55_pin_check', ?1)",
            rusqlite::params![c],
        ).map_err(|e| format!("Failed to set pin canary: {}", e))?;
    }

    // Biometric-only enable carries its wrap INTO this transaction — the row
    // is the account's sole credential, so it must land atomically with the
    // flags that lock the store to it. Otherwise drop any stale wrap (it
    // holds a pre-migration key) in the same transaction.
    match biometric_wrap {
        Some(w) => {
            tx.execute(
                "INSERT OR REPLACE INTO settings (key, value) VALUES ('biometric_wrapped_key', ?1)",
                rusqlite::params![w],
            ).map_err(|e| format!("Failed to set biometric wrap: {}", e))?;
        }
        None => {
            tx.execute(
                "DELETE FROM settings WHERE key = 'biometric_wrapped_key'",
                [],
            ).map_err(|e| format!("Failed to clear biometric wrap: {}", e))?;
        }
    }

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
// Community (Concord) at-rest migration
// ============================================================================
//
// The community tables hold their own secrets + identifying metadata, wrapped the same way as the
// pkey/event-content/PIVX fields. These helpers re-wrap them in lockstep with the three lifecycle
// flows. One direction-agnostic engine: `dec` decrypts with the given key first, then `enc` encrypts
// with the given key — so enable = enc only, disable = dec only, rekey = dec(old) then enc(new). The
// discriminators (text `looks_encrypted`; blob 32-byte-raw vs longer) make every step idempotent, so
// a re-run or the one-time backfill never double-wraps or fails on already-migrated rows.

// Idempotency discriminator for both directions is the AEAD tag itself — "already encrypted under
// key `k`" iff the value decrypts cleanly under `k` — NOT a hex/length content heuristic. A content
// heuristic (`looks_encrypted`) silently mis-classifies plaintext that happens to be bare hex (invite
// tokens, creator pubkeys) as ciphertext and skips wrapping it; the tag check can't be fooled (a
// 16-byte Poly1305 tag won't validate on non-ciphertext), and it never double-wraps on re-run.

fn xform_text(v: &str, enc: Option<&[u8; 32]>, dec: Option<&[u8; 32]>) -> Result<String, String> {
    let mut s = v.to_string();
    if let Some(k) = dec {
        // Try to decrypt; anything that isn't ciphertext under `k` fails the tag and is kept as-is.
        if let Ok(p) = decrypt_with_key(&s, k) {
            s = p;
        }
    }
    if let Some(k) = enc {
        // Encrypt unless it already decrypts under `k` (already wrapped → leave it).
        if decrypt_with_key(&s, k).is_err() {
            s = encrypt_with_key(&s, k);
        }
    }
    Ok(s)
}

fn xform_text_opt(v: &Option<String>, enc: Option<&[u8; 32]>, dec: Option<&[u8; 32]>) -> Result<Option<String>, String> {
    v.as_deref().map(|s| xform_text(s, enc, dec)).transpose()
}

fn xform_blob(v: &[u8], enc: Option<&[u8; 32]>, dec: Option<&[u8; 32]>) -> Result<Vec<u8>, String> {
    let mut b = v.to_vec();
    if let Some(k) = dec {
        // A raw 32-byte key (or anything not ciphertext under `k`) fails the tag and is kept as-is.
        if let Ok(p) = crate::crypto::decrypt_blob_with_key(&b, k) {
            b = p;
        }
    }
    if let Some(k) = enc {
        if crate::crypto::decrypt_blob_with_key(&b, k).is_err() {
            b = crate::crypto::encrypt_blob_with_key(&b, k)?;
        }
    }
    Ok(b)
}

#[cfg(test)]
mod xform_tests {
    use super::*;
    const K: [u8; 32] = [7u8; 32];
    const K2: [u8; 32] = [9u8; 32];

    #[test]
    fn bare_hex_plaintext_is_encrypted_not_skipped() {
        // C1 regression: a 64-char-hex plaintext (invite token / creator pubkey) must be wrapped,
        // never mistaken for ciphertext.
        let token = "de".repeat(32); // 64 lowercase hex chars
        let wrapped = xform_text(&token, Some(&K), None).unwrap();
        assert_ne!(wrapped, token, "bare-hex plaintext must be encrypted");
        assert_eq!(decrypt_with_key(&wrapped, &K).unwrap(), token);
    }

    #[test]
    fn encrypt_is_idempotent() {
        let token = "de".repeat(32);
        let once = xform_text(&token, Some(&K), None).unwrap();
        let twice = xform_text(&once, Some(&K), None).unwrap();
        assert_eq!(once, twice, "re-running encrypt must not double-wrap");
        assert_eq!(decrypt_with_key(&twice, &K).unwrap(), token);
    }

    #[test]
    fn disable_then_reenable_roundtrips_bare_hex() {
        let token = "ab".repeat(32);
        let enc = xform_text(&token, Some(&K), None).unwrap();
        let dec = xform_text(&enc, None, Some(&K)).unwrap();
        assert_eq!(dec, token, "disable decrypts back to plaintext");
        let re = xform_text(&dec, Some(&K), None).unwrap();
        assert_eq!(decrypt_with_key(&re, &K).unwrap(), token, "re-enable wraps the bare-hex again");
    }

    #[test]
    fn rekey_rewraps_bare_hex_under_new_key() {
        let token = "cd".repeat(32);
        let old = xform_text(&token, Some(&K), None).unwrap();
        let new = xform_text(&old, Some(&K2), Some(&K)).unwrap(); // dec old, enc new
        assert!(decrypt_with_key(&new, &K).is_err(), "no longer under old key");
        assert_eq!(decrypt_with_key(&new, &K2).unwrap(), token, "now under new key");
    }

    #[test]
    fn nonhex_plaintext_roundtrips() {
        for v in ["general", "{\"roles\":[]}", "wss://relay.example", ""] {
            let enc = xform_text(v, Some(&K), None).unwrap();
            assert_eq!(decrypt_with_key(&enc, &K).unwrap(), v);
            assert_eq!(xform_text(&enc, None, Some(&K)).unwrap(), v);
        }
    }

    /// The Concord tables exactly as the sweep addresses them (columns it does
    /// not touch omitted).
    fn migrate_test_schema(conn: &rusqlite::Connection) {
        conn.execute_batch(
            "CREATE TABLE communities (community_id TEXT, server_root_key BLOB, name TEXT, relays TEXT, description TEXT, icon TEXT, banner TEXT, banlist TEXT, owner_attestation TEXT, roles TEXT, invite_registry TEXT, owner_pubkey TEXT, owner_salt TEXT, meta_extra TEXT, control_pk TEXT, control_root BLOB);
             CREATE TABLE community_channels (channel_id TEXT, channel_key BLOB, name TEXT);
             CREATE TABLE community_epoch_keys (key BLOB);
             CREATE TABLE community_message_keys (outer_event_id TEXT, ephemeral_secret BLOB, relays TEXT);
             CREATE TABLE pending_community_invites (community_id TEXT, bundle_json TEXT, inviter_npub TEXT);
             CREATE TABLE community_public_invites (token TEXT, url TEXT, label TEXT);
             CREATE TABLE community_invite_link_sets (creator TEXT, locators TEXT);",
        ).unwrap();
    }

    /// Sweep parity for the v2 columns: the owner commitment and the CORD-02 §2
    /// control pair must survive enable → rekey → disable. A missed column stays
    /// under the old key — the pair silently drops to read-only on load, and a
    /// garbled owner commitment fails the whole v2 load.
    #[test]
    fn v2_control_pair_survives_enable_rekey_disable() {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        migrate_test_schema(&conn);
        let control_root: Vec<u8> = vec![0x5Au8; 32];
        conn.execute(
            "INSERT INTO communities (community_id, server_root_key, name, relays, banlist, roles, invite_registry, owner_pubkey, owner_salt, meta_extra, control_pk, control_root)
             VALUES (?1, ?2, 'n', '[]', '[]', '{}', '[]', ?3, ?4, '{\"custom\":null,\"extra\":{}}', ?5, ?6)",
            rusqlite::params!["cc".repeat(32), vec![0x11u8; 32], "ab".repeat(32), "cd".repeat(32), "ef".repeat(32), control_root],
        ).unwrap();
        let read = |c: &rusqlite::Connection| -> (String, String, Vec<u8>) {
            c.query_row("SELECT owner_pubkey, control_pk, control_root FROM communities", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            }).unwrap()
        };

        let tx = conn.transaction().unwrap();
        encrypt_community_in_tx(&tx, &K).unwrap();
        tx.commit().unwrap();
        let (owner_pk, control_pk, root) = read(&conn);
        assert_ne!(owner_pk, "ab".repeat(32), "owner commitment must be wrapped after enable");
        assert_ne!(control_pk, "ef".repeat(32), "control_pk must be wrapped after enable");
        assert_ne!(root, vec![0x5Au8; 32], "control_root must be wrapped after enable");

        let tx = conn.transaction().unwrap();
        rekey_community_in_tx(&tx, &K, &K2).unwrap();
        tx.commit().unwrap();

        let tx = conn.transaction().unwrap();
        decrypt_community_in_tx(&tx, &K2).unwrap();
        tx.commit().unwrap();
        let (owner_pk, control_pk, root) = read(&conn);
        assert_eq!(owner_pk, "ab".repeat(32), "owner commitment survives enable → rekey → disable");
        assert_eq!(control_pk, "ef".repeat(32), "control_pk survives enable → rekey → disable");
        assert_eq!(root, vec![0x5Au8; 32], "control_root survives enable → rekey → disable");
    }

    /// Sweep parity: every encrypted community_public_invites column must survive
    /// enable → rekey → disable (a column missed by the sweep garbles on the first rekey).
    #[test]
    fn public_invite_label_survives_enable_rekey_disable() {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        migrate_test_schema(&conn);
        conn.execute(
            "INSERT INTO community_public_invites (token, url, label) VALUES (?1, ?2, ?3)",
            rusqlite::params!["ab".repeat(32), "https://vectorapp.io/invite#x", "Reddit"],
        ).unwrap();
        let read_label = |c: &rusqlite::Connection| -> String {
            c.query_row("SELECT label FROM community_public_invites", [], |r| r.get(0)).unwrap()
        };

        let tx = conn.transaction().unwrap();
        encrypt_community_in_tx(&tx, &K).unwrap();
        tx.commit().unwrap();
        assert_ne!(read_label(&conn), "Reddit", "label must be wrapped after enable");

        let tx = conn.transaction().unwrap();
        rekey_community_in_tx(&tx, &K, &K2).unwrap();
        tx.commit().unwrap();

        let tx = conn.transaction().unwrap();
        decrypt_community_in_tx(&tx, &K2).unwrap();
        tx.commit().unwrap();
        assert_eq!(read_label(&conn), "Reddit", "label must survive enable → rekey → disable");
    }

    #[test]
    fn blob_roundtrip_and_idempotent() {
        let raw = [0x42u8; 32];
        let enc = xform_blob(&raw, Some(&K), None).unwrap();
        assert_eq!(enc.len(), 60);
        assert_eq!(xform_blob(&enc, Some(&K), None).unwrap(), enc, "blob encrypt idempotent");
        assert_eq!(xform_blob(&enc, None, Some(&K)).unwrap(), raw.to_vec());
        // rekey path for blobs
        let re = xform_blob(&enc, Some(&K2), Some(&K)).unwrap();
        assert_eq!(crate::crypto::decrypt_blob_with_key(&re, &K2).unwrap(), raw.to_vec());
    }
}

fn migrate_community_in_tx(
    tx: &rusqlite::Transaction,
    enc: Option<&[u8; 32]>,
    dec: Option<&[u8; 32]>,
) -> Result<(), String> {
    // communities: 2 secret BLOBs + identifying text (some nullable). Every
    // encrypted column save_community / save_community_v2 writes MUST appear
    // here, or it stays under the old key after a rekey and garbles on read.
    #[allow(clippy::type_complexity)]
    let rows: Vec<(String, Vec<u8>, String, String, Option<String>, Option<String>, Option<String>, String, Option<String>, String, String, Option<String>, Option<String>, Option<String>, Option<String>, Option<Vec<u8>>)> = {
        let mut stmt = tx.prepare(
            "SELECT community_id, server_root_key, name, relays, description, icon, banner, banlist, owner_attestation, roles, invite_registry, owner_pubkey, owner_salt, meta_extra, control_pk, control_root FROM communities",
        ).map_err(|e| format!("prepare communities: {e}"))?;
        let mapped = stmt.query_map([], |r| Ok((
            r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?,
            r.get::<_, Option<String>>(4)?, r.get::<_, Option<String>>(5)?, r.get::<_, Option<String>>(6)?,
            r.get::<_, String>(7)?, r.get::<_, Option<String>>(8)?, r.get::<_, String>(9)?, r.get::<_, String>(10)?,
            r.get::<_, Option<String>>(11)?, r.get::<_, Option<String>>(12)?, r.get::<_, Option<String>>(13)?,
            r.get::<_, Option<String>>(14)?, r.get::<_, Option<Vec<u8>>>(15)?,
        ))).map_err(|e| format!("query communities: {e}"))?;
        mapped.filter_map(|r| r.ok()).collect()
    };
    for (id, root, name, relays, desc, icon, banner, banlist, owner, roles, registry, owner_pk, owner_salt, meta_extra, control_pk, control_root) in rows {
        tx.execute(
            "UPDATE communities SET server_root_key=?1, name=?2, relays=?3, description=?4, icon=?5,
                banner=?6, banlist=?7, owner_attestation=?8, roles=?9, invite_registry=?10,
                owner_pubkey=?11, owner_salt=?12, meta_extra=?13, control_pk=?14, control_root=?15 WHERE community_id=?16",
            rusqlite::params![
                xform_blob(&root, enc, dec)?, xform_text(&name, enc, dec)?, xform_text(&relays, enc, dec)?,
                xform_text_opt(&desc, enc, dec)?, xform_text_opt(&icon, enc, dec)?, xform_text_opt(&banner, enc, dec)?,
                xform_text(&banlist, enc, dec)?, xform_text_opt(&owner, enc, dec)?, xform_text(&roles, enc, dec)?,
                xform_text(&registry, enc, dec)?, xform_text_opt(&owner_pk, enc, dec)?, xform_text_opt(&owner_salt, enc, dec)?,
                xform_text_opt(&meta_extra, enc, dec)?, xform_text_opt(&control_pk, enc, dec)?,
                control_root.as_deref().map(|b| xform_blob(b, enc, dec)).transpose()?, id,
            ],
        ).map_err(|e| format!("update communities: {e}"))?;
    }

    // community_channels: channel_key BLOB + name.
    let chans: Vec<(String, Vec<u8>, String)> = {
        let mut stmt = tx.prepare("SELECT channel_id, channel_key, name FROM community_channels")
            .map_err(|e| format!("prepare channels: {e}"))?;
        let mapped = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?, r.get::<_, String>(2)?)))
            .map_err(|e| format!("query channels: {e}"))?;
        mapped.filter_map(|r| r.ok()).collect()
    };
    for (cid, key, name) in chans {
        tx.execute(
            "UPDATE community_channels SET channel_key=?1, name=?2 WHERE channel_id=?3",
            rusqlite::params![xform_blob(&key, enc, dec)?, xform_text(&name, enc, dec)?, cid],
        ).map_err(|e| format!("update channel: {e}"))?;
    }

    // community_epoch_keys: key BLOB (rowid-keyed — coordinate columns aren't unique enough alone).
    let eks: Vec<(i64, Vec<u8>)> = {
        let mut stmt = tx.prepare("SELECT rowid, key FROM community_epoch_keys")
            .map_err(|e| format!("prepare epoch keys: {e}"))?;
        let mapped = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?)))
            .map_err(|e| format!("query epoch keys: {e}"))?;
        mapped.filter_map(|r| r.ok()).collect()
    };
    for (rowid, key) in eks {
        tx.execute("UPDATE community_epoch_keys SET key=?1 WHERE rowid=?2",
            rusqlite::params![xform_blob(&key, enc, dec)?, rowid])
            .map_err(|e| format!("update epoch key: {e}"))?;
    }

    // community_message_keys: ephemeral_secret BLOB + relays.
    let mks: Vec<(String, Vec<u8>, String)> = {
        let mut stmt = tx.prepare("SELECT outer_event_id, ephemeral_secret, relays FROM community_message_keys")
            .map_err(|e| format!("prepare msg keys: {e}"))?;
        let mapped = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?, r.get::<_, String>(2)?)))
            .map_err(|e| format!("query msg keys: {e}"))?;
        mapped.filter_map(|r| r.ok()).collect()
    };
    for (oid, secret, relays) in mks {
        tx.execute("UPDATE community_message_keys SET ephemeral_secret=?1, relays=?2 WHERE outer_event_id=?3",
            rusqlite::params![xform_blob(&secret, enc, dec)?, xform_text(&relays, enc, dec)?, oid])
            .map_err(|e| format!("update msg key: {e}"))?;
    }

    // pending_community_invites: bundle_json + inviter_npub.
    let pend: Vec<(String, String, String)> = {
        let mut stmt = tx.prepare("SELECT community_id, bundle_json, inviter_npub FROM pending_community_invites")
            .map_err(|e| format!("prepare pending: {e}"))?;
        let mapped = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))
            .map_err(|e| format!("query pending: {e}"))?;
        mapped.filter_map(|r| r.ok()).collect()
    };
    for (id, bundle, inviter) in pend {
        tx.execute("UPDATE pending_community_invites SET bundle_json=?1, inviter_npub=?2 WHERE community_id=?3",
            rusqlite::params![xform_text(&bundle, enc, dec)?, xform_text(&inviter, enc, dec)?, id])
            .map_err(|e| format!("update pending: {e}"))?;
    }

    // community_public_invites: token + url + label (rowid-keyed — token is itself wrapped).
    let pubs: Vec<(i64, String, String, Option<String>)> = {
        let mut stmt = tx.prepare("SELECT rowid, token, url, label FROM community_public_invites")
            .map_err(|e| format!("prepare public invites: {e}"))?;
        let mapped = stmt.query_map([], |r| Ok((
            r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, Option<String>>(3)?,
        ))).map_err(|e| format!("query public invites: {e}"))?;
        mapped.filter_map(|r| r.ok()).collect()
    };
    for (rowid, token, url, label) in pubs {
        tx.execute("UPDATE community_public_invites SET token=?1, url=?2, label=?3 WHERE rowid=?4",
            rusqlite::params![xform_text(&token, enc, dec)?, xform_text(&url, enc, dec)?, xform_text_opt(&label, enc, dec)?, rowid])
            .map_err(|e| format!("update public invite: {e}"))?;
    }

    // community_invite_link_sets: creator + locators (rowid-keyed — creator is itself wrapped).
    let sets: Vec<(i64, String, String)> = {
        let mut stmt = tx.prepare("SELECT rowid, creator, locators FROM community_invite_link_sets")
            .map_err(|e| format!("prepare link sets: {e}"))?;
        let mapped = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))
            .map_err(|e| format!("query link sets: {e}"))?;
        mapped.filter_map(|r| r.ok()).collect()
    };
    for (rowid, creator, locators) in sets {
        tx.execute("UPDATE community_invite_link_sets SET creator=?1, locators=?2 WHERE rowid=?3",
            rusqlite::params![xform_text(&creator, enc, dec)?, xform_text(&locators, enc, dec)?, rowid])
            .map_err(|e| format!("update link set: {e}"))?;
    }

    Ok(())
}

/// Encrypt all Concord secrets + metadata (enable flow).
fn encrypt_community_in_tx(tx: &rusqlite::Transaction, key: &[u8; 32]) -> Result<(), String> {
    migrate_community_in_tx(tx, Some(key), None)
}
/// Decrypt all Concord secrets + metadata (disable flow).
fn decrypt_community_in_tx(tx: &rusqlite::Transaction, key: &[u8; 32]) -> Result<(), String> {
    migrate_community_in_tx(tx, None, Some(key))
}
/// Re-wrap all Concord secrets + metadata old key → new key (PIN-rekey flow).
fn rekey_community_in_tx(tx: &rusqlite::Transaction, old_key: &[u8; 32], new_key: &[u8; 32]) -> Result<(), String> {
    migrate_community_in_tx(tx, Some(new_key), Some(old_key))
}

/// One-time backfill for an account that already had Local Encryption ON *before* Concord at-rest
/// encryption shipped: its community rows are still plaintext. Wrap them once, gated by a per-account
/// settings flag. Idempotent (the field discriminators skip already-wrapped rows), so a crash mid-pass
/// re-runs next login. No-op when encryption is off, the key vault is empty, or the flag is set.
/// Best-effort at the call site — a failure leaves the flag unset and retries, never blocks login.
pub fn backfill_community_at_rest() -> Result<(), String> {
    if !vector_core::state::is_encryption_enabled_fast() {
        return Ok(());
    }
    if vector_core::db::get_sql_setting("community_at_rest_encrypted".to_string())
        .ok()
        .flatten()
        .as_deref()
        == Some("1")
    {
        return Ok(());
    }
    let mut key = match crate::ENCRYPTION_KEY.get() {
        Some(k) => k,
        None => return Ok(()), // locked / not yet derived — retry on a later login
    };
    let conn = vector_core::db::get_write_connection_guard_static()?;
    let tx = conn.unchecked_transaction().map_err(|e| format!("backfill tx: {e}"))?;
    let res = encrypt_community_in_tx(&tx, &key);
    key.zeroize();
    res?;
    tx.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES ('community_at_rest_encrypted', '1')",
        [],
    )
    .map_err(|e| format!("backfill flag: {e}"))?;
    tx.commit().map_err(|e| format!("backfill commit: {e}"))?;
    println!("[Encryption] Community at-rest backfill complete");
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

/// Does `key` provably decrypt this account's material? Checks whichever
/// anchor the account shape has: the pkey ciphertext (local/bunker) or the
/// canary (keyless NIP-55). No anchor (plaintext pkey, no canary) = nothing to
/// contradict, so accept — matching every other credential check in the app.
fn old_key_is_valid<R: Runtime>(handle: &AppHandle<R>, key: &[u8; 32]) -> bool {
    let Ok(conn) = crate::account_manager::get_db_connection_guard(handle) else {
        return false;
    };
    let pkey: Option<String> = conn
        .query_row("SELECT value FROM settings WHERE key = 'pkey'", [], |r| r.get(0))
        .ok();
    if let Some(p) = pkey {
        if !p.starts_with("nsec") {
            return matches!(decrypt_with_key(&p, key), Ok(d) if d.starts_with("nsec"));
        }
    }
    let canary: Option<String> = conn
        .query_row("SELECT value FROM settings WHERE key = 'nip55_pin_check'", [], |r| r.get(0))
        .ok();
    if let Some(c) = canary {
        return matches!(
            decrypt_with_key(&c, key),
            Ok(d) if d == crate::commands::account::NIP55_PIN_CANARY
        );
    }
    true
}

/// Re-key from the CURRENTLY UNLOCKED vault key to `new_key`, flipping
/// `security_type` and the biometric wrap in the same transaction.
///
/// This is how the two encryption modes swap: the old key comes from the live
/// session (the user is unlocked, so no credential needs retyping — and the
/// biometric-side credential is one nobody knows by design). Plaintext never
/// touches disk: every row is decrypted and re-encrypted in memory inside one
/// transaction, exactly like Change PIN.
pub(crate) async fn rekey_from_vault<R: Runtime>(
    handle: AppHandle<R>,
    mut new_key: [u8; 32],
    security_type: &str,
    biometric_wrap: Option<String>,
    session: vector_core::state::SessionGuard,
) -> Result<(), String> {
    let _guard = MigrationGuard::try_enter()?;
    // Re-validated INSIDE the guard: `reset_session` refuses while a migration
    // is in flight, so from here the account cannot change under us. Before the
    // guard, a swap could have landed while we derived the key — re-keying then
    // would rewrite the swapped-in account's store with this key.
    if !session.is_valid() {
        new_key.zeroize();
        return Err("Account changed, nothing was modified".to_string());
    }
    if !vector_core::state::is_encryption_enabled_fast() {
        new_key.zeroize();
        return Err("Local Encryption is not enabled".to_string());
    }
    let mut old_key: [u8; 32] = crate::ENCRYPTION_KEY
        .get()
        .ok_or("Session is locked, unlock before changing the security mode")?;

    // The old key drives a whole-database re-encrypt, and rows that fail to
    // decrypt are SKIPPED rather than fatal — so a wrong old key would commit
    // an account full of orphaned ciphertext. Prove it round-trips first.
    if !old_key_is_valid(&handle, &old_key) {
        old_key.zeroize();
        new_key.zeroize();
        return Err("Session key does not match this account".to_string());
    }

    close_processing_gate();
    let result = rekey_encryption_transactional(
        &handle, &old_key, &new_key, security_type, biometric_wrap.as_deref(),
    );
    old_key.zeroize();

    // Vault flips to the new key BEFORE the drain so queued events encrypt
    // under the key the database now holds.
    if result.is_ok() {
        crate::ENCRYPTION_KEY.set(new_key, &[&crate::MY_SECRET_KEY]);
    }
    new_key.zeroize();
    drain_pending_events(&handle).await;

    match result {
        Ok(()) => {
            let _ = handle.emit("encryption_migration_complete", ());
            Ok(())
        }
        Err(e) => Err(e),
    }
}

/// Switch an encrypted account to a typed credential (PIN or password),
/// dropping any OS-held wrap. Used to leave biometric mode, and as the
/// credential-change path that doesn't require retyping the old one.
#[command]
pub async fn switch_to_credential<R: Runtime>(
    handle: AppHandle<R>,
    credential: String,
    security_type: String,
) -> Result<(), String> {
    if credential.trim().is_empty() {
        return Err("Credential must not be empty".to_string());
    }
    if security_type != "pin" && security_type != "password" {
        return Err("Invalid security type".to_string());
    }
    let session = vector_core::state::SessionGuard::capture();
    let _switch = crate::commands::biometric::SwitchGuard::try_enter()?;
    let new_key = crate::crypto::hash_pass(credential).await;
    rekey_from_vault(handle, new_key, &security_type, None, session).await
}

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
    let _guard = MigrationGuard::try_enter()?;

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
    let result = rekey_encryption_transactional(&handle, &old_key, &new_key, &security_type, None);

    // 5. Update vault to new key BEFORE draining queued events.
    // Events queued during the rekey must be encrypted with the NEW key so they
    // match the rest of the database. Previously, the vault update happened AFTER
    // drain, causing drained events to be encrypted with the old key — orphaning
    // them permanently (the new key couldn't decrypt them, and subsequent rekeys
    // silently skipped them).
    if result.is_ok() {
        crate::ENCRYPTION_KEY.set(new_key, &[&crate::MY_SECRET_KEY]);
    }

    // 6. ALWAYS reopen gate and drain queued events (audit C2)
    drain_pending_events(&handle).await;

    match result {
        Ok(()) => {
            // The biometric wrap holds the OLD derived key — clear it rather
            // than silently unlocking into a key that no longer decrypts
            // anything. Re-enabling in Settings re-wraps the new key.
            crate::commands::biometric::clear_biometric_enrollment();
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
    biometric_wrap: Option<&str>,
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
                event_kind::CHAT_MESSAGE as i32,
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
    // A keyless (NIP-55) account has no pkey, so the canary is its ONLY
    // wrong-credential detector; a bunker account cannot log in without its
    // url. Both are key-derived, so both move with the key or the account is
    // locked out at the NEXT boot rather than at the failing operation.
    rekey_setting_in_tx(&tx, "nip55_pin_check", old_key, new_key)?;
    rekey_setting_in_tx(&tx, "bunker_url", old_key, new_key)?;
    rekey_pivx_in_tx(&tx, old_key, new_key)?;
    rekey_community_in_tx(&tx, old_key, new_key)?;

    // 4. Verify re-keyed state within the transaction (before committing)
    verify_encrypted_state_in_tx(&tx, new_key)?;

    // 5. Update metadata within the same transaction
    tx.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES ('security_type', ?1)",
        rusqlite::params![security_type],
    ).map_err(|e| format!("Failed to update security_type: {}", e))?;

    // The wrap always moves with the key it protects, inside this transaction:
    // Some = the new key's wrap (switching TO biometric), None = drop any wrap
    // (credential change, or switching AWAY from biometric). A crash can never
    // leave a wrap that holds a key the store no longer uses.
    match biometric_wrap {
        Some(w) => {
            tx.execute(
                "INSERT OR REPLACE INTO settings (key, value) VALUES ('biometric_wrapped_key', ?1)",
                rusqlite::params![w],
            ).map_err(|e| format!("Failed to set biometric wrap: {}", e))?;
        }
        None => {
            tx.execute(
                "DELETE FROM settings WHERE key = 'biometric_wrapped_key'",
                [],
            ).map_err(|e| format!("Failed to clear biometric wrap: {}", e))?;
        }
    }

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
        // A row that isn't ciphertext under `old_key` and isn't hex at all was
        // written plaintext (e.g. bunker_url stored before encryption was
        // enabled) — wrap it under the new key rather than failing the rekey.
        let looks_encrypted = encrypted_val.len() >= 56
            && encrypted_val.bytes().all(|b| b.is_ascii_hexdigit());
        let plaintext = match decrypt_with_key(&encrypted_val, old_key) {
            Ok(p) => p,
            Err(_) if !looks_encrypted => encrypted_val.clone(),
            Err(_) => return Err(format!("Failed to decrypt setting '{}'", key)),
        };

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

#[cfg(test)]
mod rekey_setting_tests {
    use super::*;
    use crate::crypto::{encrypt_with_key, decrypt_with_key};
    const OLD: [u8; 32] = [3u8; 32];
    const NEW: [u8; 32] = [5u8; 32];

    fn conn() -> rusqlite::Connection {
        let c = rusqlite::Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT);").unwrap();
        c
    }
    fn put(c: &rusqlite::Connection, k: &str, v: &str) {
        c.execute("INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
            rusqlite::params![k, v]).unwrap();
    }
    fn get(c: &rusqlite::Connection, k: &str) -> String {
        c.query_row("SELECT value FROM settings WHERE key = ?1", rusqlite::params![k], |r| r.get(0)).unwrap()
    }

    /// The bug that shipped: a keyless account's canary stayed under the dead
    /// key, so the NEXT cold boot rejected the correct credential.
    #[test]
    fn canary_survives_rekey() {
        let mut c = conn();
        put(&c, "nip55_pin_check", &encrypt_with_key(crate::commands::account::NIP55_PIN_CANARY, &OLD));
        let tx = c.transaction().unwrap();
        rekey_setting_in_tx(&tx, "nip55_pin_check", &OLD, &NEW).unwrap();
        tx.commit().unwrap();
        assert_eq!(
            decrypt_with_key(&get(&c, "nip55_pin_check"), &NEW).unwrap(),
            crate::commands::account::NIP55_PIN_CANARY
        );
    }

    /// Same shape for bunker accounts: an un-rekeyed url = permanent login failure.
    #[test]
    fn bunker_url_survives_rekey() {
        let mut c = conn();
        let url = "bunker://abc?relay=wss://relay.example";
        put(&c, "bunker_url", &encrypt_with_key(url, &OLD));
        let tx = c.transaction().unwrap();
        rekey_setting_in_tx(&tx, "bunker_url", &OLD, &NEW).unwrap();
        tx.commit().unwrap();
        assert_eq!(decrypt_with_key(&get(&c, "bunker_url"), &NEW).unwrap(), url);
    }

    /// A row written before encryption was enabled is plaintext: wrap it under
    /// the new key rather than failing the whole migration.
    #[test]
    fn plaintext_row_is_adopted_not_fatal() {
        let mut c = conn();
        let url = "bunker://plain?relay=wss://r.example";
        put(&c, "bunker_url", url);
        let tx = c.transaction().unwrap();
        rekey_setting_in_tx(&tx, "bunker_url", &OLD, &NEW).unwrap();
        tx.commit().unwrap();
        assert_eq!(decrypt_with_key(&get(&c, "bunker_url"), &NEW).unwrap(), url);
    }

    /// Ciphertext that does NOT belong to `old_key` must abort the rekey — the
    /// alternative is committing a value nobody can ever decrypt.
    #[test]
    fn wrong_old_key_aborts() {
        let mut c = conn();
        put(&c, "pkey", &encrypt_with_key("nsec1example", &[9u8; 32]));
        let tx = c.transaction().unwrap();
        assert!(rekey_setting_in_tx(&tx, "pkey", &OLD, &NEW).is_err());
    }

    /// Absent rows are a no-op (accounts legitimately lack pkey, seed, canary).
    #[test]
    fn missing_row_is_noop() {
        let mut c = conn();
        let tx = c.transaction().unwrap();
        rekey_setting_in_tx(&tx, "nip55_pin_check", &OLD, &NEW).unwrap();
        tx.commit().unwrap();
        assert_eq!(
            c.query_row("SELECT count(*) FROM settings", [], |r| r.get::<_, i64>(0)).unwrap(), 0
        );
    }
}
