//! Encryption toggle and migration commands.
//!
//! This module handles:
//! - Checking encryption status
//! - Bulk decryption migration (disable encryption)
//! - Bulk encryption migration (enable encryption)
//! - Event queue management during migration
//!
//! The at-rest sweep itself is `vector_core::db::at_rest`.

use tauri::{command, AppHandle, Emitter, Runtime};
use zeroize::Zeroize;
use crate::crypto::encrypt_with_key;
use crate::state::{close_processing_gate, open_processing_gate, PENDING_EVENTS};
use vector_core::db::at_rest::{self, MigrationProgress};

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
        // Back on the welcome screen after a reload: refuse clearnet if Tor is remembered,
        // before the screen gets to start it.
        #[cfg(feature = "tor")]
        if vector_core::tor::prelogin_preference() {
            vector_core::tor::arm_prelogin_carry(true);
            vector_core::tor::set_tor_enabled_pref(true);
        }
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
pub async fn disable_encryption<R: Runtime>(handle: AppHandle<R>, credential: Option<String>) -> Result<(), String> {
    let session = vector_core::db::current_session();
    let key = prove_user(credential).await?;
    // Mark migration in flight so reset_session() refuses to fire mid-tx.
    let _guard = MigrationGuard::try_enter()?;
    if !session.is_live() {
        return Err("Account changed, nothing was modified".to_string());
    }

    // Close the processing gate — events are queued until we reopen
    close_processing_gate();

    // The vault is cleared inside the migration, before writers resume.
    let result = at_rest::disable(&key, &progress_emitter(&handle));
    drop(key);

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

/// Enable encryption - bulk encrypt all plaintext content
///
/// This command:
/// 1. Derives the key from the credential under a fresh salt (slow Argon2 step)
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
    // Checked before the slow derivation too, so a doomed attempt costs nothing.
    if vector_core::state::is_encryption_enabled_fast() {
        return Err("Encryption is already enabled".to_string());
    }
    let kdf = crate::crypto::Kdf::fresh();
    let key = zeroize::Zeroizing::new(crate::crypto::derive_key(credential, &kdf).await);
    enable_encryption_with_key(handle, key, kdf, security_type, biometric_wrap).await
}

/// [`enable_encryption`] with the key already derived. A biometric enable passes the key its wrap
/// holds: deriving again would salt a different one.
pub(crate) async fn enable_encryption_with_key<R: Runtime>(
    handle: AppHandle<R>,
    key: zeroize::Zeroizing<[u8; 32]>,
    kdf: crate::crypto::Kdf,
    security_type: String,
    biometric_wrap: Option<String>,
) -> Result<(), String> {
    let _guard = MigrationGuard::try_enter()?;
    // Already-enabled guard: re-running would seal an encrypted store a second time.
    if vector_core::state::is_encryption_enabled_fast() {
        return Err("Encryption is already enabled".to_string());
    }

    // Keyless account: the canary (its only wrong-PIN detector at boot) rides
    // the migration transaction below, encrypted under the new key directly.
    let nip55_canary = if vector_core::signer_kind() == vector_core::SignerKind::Nip55 {
        Some(encrypt_with_key(crate::commands::account::NIP55_PIN_CANARY, &key))
    } else {
        None
    };

    close_processing_gate();
    // The migration installs the key and flips the flag only once its commit has landed.
    let result = at_rest::enable(
        &key, &kdf, &security_type, biometric_wrap.as_deref(), nip55_canary.as_deref(), &progress_emitter(&handle),
    );

    // ALWAYS reopen gate and drain queued events, regardless of success/failure (audit C2)
    drain_pending_events(&handle).await;

    result?;
    let _ = handle.emit("encryption_migration_complete", ());
    Ok(())
}

// ============================================================================
// Helper Functions
// ============================================================================

fn progress_emitter<R: Runtime>(handle: &AppHandle<R>) -> impl Fn(MigrationProgress) + '_ {
    move |p| {
        let _ = handle.emit("encryption_migration_progress", p);
    }
}

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
// Credential Verification (no secrets cross IPC)
// ============================================================================

/// Verify a credential (PIN/password) without returning any key material.
#[command]
pub async fn verify_credential(credential: String) -> Result<(), String> {
    let credential = zeroize::Zeroizing::new(credential);
    at_rest::prove_credential(&credential).await.map(drop)
}

/// The account's key from the user's own proof: the typed credential, or the OS prompt on a
/// biometric account. Never the vault, so a call from the page alone proves nothing.
pub(crate) async fn prove_user(credential: Option<String>) -> Result<zeroize::Zeroizing<[u8; 32]>, String> {
    if !vector_core::state::is_encryption_enabled_fast() {
        return Err("Local Encryption is not enabled".to_string());
    }
    #[cfg(target_os = "android")]
    if vector_core::db::get_sql_setting("security_type".to_string()).ok().flatten().as_deref() == Some("biometric") {
        return crate::commands::biometric::prove_with_prompt().await;
    }
    let credential = zeroize::Zeroizing::new(
        credential.filter(|c| !c.is_empty()).ok_or(at_rest::CREDENTIAL_REQUIRED)?,
    );
    at_rest::prove_credential(&credential).await
}

// ============================================================================
// Re-Keying (Change PIN/Password)
// ============================================================================

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
    new_kdf: crate::crypto::Kdf,
    security_type: &str,
    biometric_wrap: Option<String>,
    session: std::sync::Arc<vector_core::db::Session>,
) -> Result<(), String> {
    let _guard = MigrationGuard::try_enter()?;
    // Re-validated INSIDE the guard: `reset_session` refuses while a migration
    // is in flight, so from here the account cannot change under us. Before the
    // guard, a swap could have landed while we derived the key — re-keying then
    // would rewrite the swapped-in account's store with this key.
    if !session.is_live() {
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
    if !at_rest::key_matches_account(&old_key) {
        old_key.zeroize();
        new_key.zeroize();
        return Err("Session key does not match this account".to_string());
    }

    close_processing_gate();
    let result = at_rest::rekey(
        &old_key, &new_key, &new_kdf, security_type, biometric_wrap.as_deref(), &progress_emitter(&handle),
    );
    old_key.zeroize();
    // The vault moved to the new key inside the migration, before the drain below.
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

/// Move a legacy-salted account onto its own salt right after a verified unlock. Best-effort: on
/// any failure the account stays as it was, and the next unlock tries again.
pub(crate) async fn upgrade_after_unlock(password: &str, key: &[u8; 32]) {
    let Ok(_guard) = MigrationGuard::try_enter() else { return };
    match vector_core::db::at_rest::upgrade_after_unlock(password, key).await {
        Ok(true) => println!("[Encryption] Account moved onto its own key derivation salt"),
        Ok(false) => {}
        Err(e) => eprintln!("[Encryption] Salt upgrade deferred: {e}"),
    }
}

/// Switch an encrypted account to a typed credential (PIN or password),
/// dropping any OS-held wrap. Used to leave biometric mode; the current
/// credential (or the OS prompt) is proven first.
#[command]
pub async fn switch_to_credential<R: Runtime>(
    handle: AppHandle<R>,
    credential: String,
    security_type: String,
    current_credential: Option<String>,
) -> Result<(), String> {
    if credential.trim().is_empty() {
        return Err("Credential must not be empty".to_string());
    }
    if security_type != "pin" && security_type != "password" {
        return Err("Invalid security type".to_string());
    }
    let session = vector_core::db::current_session();
    let _switch = crate::commands::biometric::SwitchGuard::try_enter()?;
    drop(prove_user(current_credential).await?);
    let kdf = crate::crypto::Kdf::fresh();
    let new_key = crate::crypto::derive_key(credential, &kdf).await;
    rekey_from_vault(handle, new_key, kdf, &security_type, None, session).await
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

    // 1. Derive the old key and prove it opens this store
    let old_credential = zeroize::Zeroizing::new(old_credential);
    let old_key = at_rest::prove_credential(&old_credential).await?;

    // 2. Derive new key
    let new_kdf = crate::crypto::Kdf::fresh();
    let new_key = zeroize::Zeroizing::new(crate::crypto::derive_key(new_credential, &new_kdf).await);

    // 3. Close processing gate
    close_processing_gate();

    // 4. Perform transactional re-key (all-or-nothing via SQLite transaction)
    let result = at_rest::rekey(&old_key, &new_key, &new_kdf, &security_type, None, &progress_emitter(&handle));

    // 5. The migration moved the vault to the new key before releasing writers,
    // so the queued events below seal under the key the database now holds.

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
