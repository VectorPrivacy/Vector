//! Biometric unlock commands (Android).
//!
//! Local Encryption's 32-byte vault key, wrapped by an auth-gated AES-GCM key
//! in the Android Keystore. A passed biometric prompt (or device credential)
//! unwraps it and primes the vault exactly like a correct PIN entry, minus the
//! Argon2id wait.
//!
//! OS-backed and Vector-backed encryption are MUTUALLY EXCLUSIVE modes, never
//! layered: `security_type` is either `biometric` or `pin`/`password`. One
//! credential means one login path, which is what makes the unlock screen
//! race-free. Switching modes re-keys the store from the live vault key
//! (`rekey_from_vault`), so plaintext never touches disk.
//!
//! Ordering invariant: the unwrapped key is VERIFIED against this account's
//! stored material (pkey ciphertext / NIP-55 canary) with raw-key decrypts
//! BEFORE it ever touches the vault. A primed-then-verified vault would let a
//! concurrent at-rest write land under a wrong key (split-key corruption).

use tauri::{command, AppHandle, Runtime};
#[cfg(target_os = "android")]
use tauri::Emitter;

/// Settings row holding base64(iv || ciphertext) of the wrapped vault key.
/// Row presence = enrolled. Deliberately plaintext: its confidentiality comes
/// from the TEE key + user auth, not from the vault key it wraps.
pub(crate) const WRAPPED_KEY_SETTING: &str = "biometric_wrapped_key";

/// Per-account keystore alias — a swap must never unwrap another account's key.
#[cfg(target_os = "android")]
fn keystore_alias(npub: &str) -> String {
    format!("vector_bio_{}", npub)
}

/// Clear the enrollment: settings row + keystore key. Idempotent, best-effort.
/// Touches the CURRENT account's row and keystore alias — the enrollment is the
/// one thing here still resolved late rather than through the session.
pub(crate) fn clear_biometric_enrollment() {
    let _ = vector_core::db::remove_setting(WRAPPED_KEY_SETTING);
    #[cfg(target_os = "android")]
    if let Ok(npub) = crate::account_manager::get_current_account() {
        crate::android::biometric::remove_key(&keystore_alias(&npub));
    }
}

/// Verify a candidate vault key against this account's stored material using
/// RAW-KEY decrypts only (never the vault — it must not be primed yet).
/// `Ok(true)` = provably correct; `Ok(false)` = provably wrong;
/// `Ok(true)` is also returned when there is nothing to verify against
/// (plaintext pkey / no canary), matching the PIN path's behavior.
#[cfg(target_os = "android")]
fn verify_candidate_key(key: &[u8; 32]) -> bool {
    use crate::crypto::decrypt_with_key;
    let signer_type = vector_core::db::get_signer_type().unwrap_or_else(|_| "local".to_string());
    if signer_type == "nip55" {
        match vector_core::db::get_sql_setting("nip55_pin_check".to_string()) {
            Ok(Some(stored)) => matches!(
                decrypt_with_key(&stored, key),
                Ok(plain) if plain == crate::commands::account::NIP55_PIN_CANARY
            ),
            _ => true,
        }
    } else {
        match vector_core::db::get_pkey() {
            Ok(Some(stored)) if !stored.starts_with("nsec1") => matches!(
                decrypt_with_key(&stored, key),
                Ok(plain) if plain.starts_with("nsec1")
            ),
            _ => true,
        }
    }
}

/// Serialises an unlock-method change across BOTH of its phases (OS prompt +
/// wrap, then the re-key commit). `MigrationGuard` only covers the commit, and
/// enrolling REPLACES the keystore key before the prompt — so two overlapping
/// switches can leave a committed wrap whose key a loser's cleanup deleted.
/// Refuse, never queue.
static SWITCH_IN_FLIGHT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub(crate) struct SwitchGuard;
impl SwitchGuard {
    pub(crate) fn try_enter() -> Result<Self, String> {
        std::sync::atomic::AtomicBool::compare_exchange(
            &SWITCH_IN_FLIGHT,
            false,
            true,
            std::sync::atomic::Ordering::AcqRel,
            std::sync::atomic::Ordering::Acquire,
        )
        .map_err(|_| "An unlock-method change is already in progress".to_string())?;
        Ok(Self)
    }
}
impl Drop for SwitchGuard {
    fn drop(&mut self) {
        SWITCH_IN_FLIGHT.store(false, std::sync::atomic::Ordering::Release);
    }
}

/// Whether the CURRENT account's wrap is its sole credential.
#[cfg(target_os = "android")]
fn is_biometric_only() -> bool {
    vector_core::db::get_sql_setting("security_type".to_string())
        .ok()
        .flatten()
        .as_deref()
        == Some("biometric")
        && vector_core::state::is_encryption_enabled_fast()
}

#[derive(serde::Serialize)]
pub struct BiometricStatus {
    /// Hardware + OS can do it (Android 11+, sensor or device credential ready).
    pub supported: bool,
    /// This account holds a wrapped vault key (biometric unlock is set up).
    pub enrolled: bool,
    /// The OS's localized wording for the unlock affordance ("Use fingerprint",
    /// "Use screen lock", ...). Empty = frontend uses its default copy.
    pub label: String,
}

/// Availability + enrollment for the CURRENT account. Compile-time false off
/// Android. Callable from the unlock screen (account is selected, DB open).
/// Never cache the result across a session reload — enrollment is per-account.
#[command]
pub async fn biometric_status<R: Runtime>(_handle: AppHandle<R>) -> Result<BiometricStatus, String> {
    let enrolled = vector_core::db::get_sql_setting(WRAPPED_KEY_SETTING.to_string())
        .ok()
        .flatten()
        .is_some();
    #[cfg(target_os = "android")]
    {
        let (avail, label) = tokio::task::spawn_blocking(|| {
            (
                crate::android::biometric::availability().unwrap_or_else(|_| "unsupported".to_string()),
                crate::android::biometric::unlock_label().unwrap_or_default(),
            )
        })
        .await
        .map_err(|e| format!("join error: {:?}", e))?;
        Ok(BiometricStatus { supported: avail == "available", enrolled, label })
    }
    #[cfg(not(target_os = "android"))]
    {
        Ok(BiometricStatus { supported: false, enrolled, label: String::new() })
    }
}

/// Biometric-ONLY credential: a generated 256-bit value nobody ever knows,
/// run through the SAME machinery as a typed PIN (Argon2id, canary, at-rest
/// migrations all unchanged), with its derived key wrapped behind one prompt
/// BEFORE anything is committed — a cancel aborts with nothing changed, and
/// the store is never locked to a credential that has no wrap.
///
/// Recovery model (surfaced to the user at setup): removing the device's
/// screen lock invalidates the hardware key and this device's store resets —
/// sign in with your keys, resync from relays. Cryptographically STRONGER at
/// rest than any PIN (256-bit random vs a 6-digit search space).
#[cfg(target_os = "android")]
async fn generate_and_wrap(
    npub: &str,
) -> Result<(zeroize::Zeroizing<String>, String, [u8; 32]), String> {
    use rand::RngCore;
    use zeroize::{Zeroize, Zeroizing};

    if crate::commands::encryption::MIGRATION_IN_PROGRESS.load(std::sync::atomic::Ordering::Acquire) {
        return Err("An encryption migration is in progress, try again in a moment".to_string());
    }

    let mut raw = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut raw);
    let secret = Zeroizing::new(crate::util::bytes_to_hex_32(&raw));
    raw.zeroize();

    // Derive the vault key NOW and wrap it FIRST, so the wrap exists before
    // any store gets locked to this credential. The caller gets the key back
    // rather than re-deriving it.
    let key = crate::crypto::hash_pass((*secret).clone()).await;

    let session = vector_core::state::SessionGuard::capture();
    let alias = keystore_alias(npub);

    let wrapped = tokio::task::spawn_blocking(move || {
        let mut k = key;
        let res = crate::android::biometric::enroll_wrap(&alias, &k);
        k.zeroize();
        res
    })
    .await
    .map_err(|e| format!("join error: {:?}", e))?
    .map_err(|e| match e {
        crate::android::biometric::BiometricError::Cancelled => "BIOMETRIC_CANCELLED".to_string(),
        crate::android::biometric::BiometricError::Invalidated => {
            "Biometric hardware rejected the key, try again".to_string()
        }
        crate::android::biometric::BiometricError::Other(msg) => msg,
    })?;

    if !session.is_valid() {
        return Err("Session changed during setup".to_string());
    }
    Ok((secret, wrapped, key))
}

/// New-account onboarding: encrypt with a generated credential unlocked
/// solely by biometrics/device credential.
#[command]
pub async fn setup_encryption_biometric<R: Runtime>(handle: AppHandle<R>) -> Result<(), String> {
    #[cfg(target_os = "android")]
    {
        let _switch = SwitchGuard::try_enter()?;
        // The alias npub must match the account setup_encryption COMMITS to:
        // during add-profile the pending account is the target while CURRENT
        // still points at the previous profile, so pending wins here.
        let npub = crate::account_manager::get_pending_account()
            .ok()
            .flatten()
            .map(Ok)
            .unwrap_or_else(|| crate::account_manager::get_current_account())?;
        let (secret, wrapped, _key) = generate_and_wrap(&npub).await?;
        // The wrap rides INSIDE the setup commit transaction: the row is the
        // account's sole credential and must land atomically with the store
        // being locked to it.
        let res = crate::commands::account::setup_encryption(
            handle,
            (*secret).clone(),
            "biometric".to_string(),
            Some(wrapped),
        )
        .await;
        if res.is_err() {
            crate::android::biometric::remove_key(&keystore_alias(&npub));
        }
        res
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = handle;
        Err("Biometric unlock is Android-only".to_string())
    }
}

/// Settings path: turn Local Encryption ON in biometric-only mode (full
/// at-rest migration under the generated credential).
#[command]
pub async fn enable_encryption_biometric<R: Runtime>(handle: AppHandle<R>) -> Result<(), String> {
    #[cfg(target_os = "android")]
    {
        let _switch = SwitchGuard::try_enter()?;
        if vector_core::state::is_encryption_enabled_fast() {
            return Err("Encryption is already enabled".to_string());
        }
        // Settings runs on a committed session: current-only, never pending
        // (a stale pending from an aborted add-profile must not hijack this).
        let npub = crate::account_manager::get_current_account()?;
        let (secret, wrapped, _key) = generate_and_wrap(&npub).await?;
        let res = crate::commands::encryption::enable_encryption(
            handle,
            (*secret).clone(),
            "biometric".to_string(),
            Some(wrapped),
        )
        .await;
        if res.is_err() {
            crate::android::biometric::remove_key(&keystore_alias(&npub));
        }
        res
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = handle;
        Err("Biometric unlock is Android-only".to_string())
    }
}

/// Switch an already-encrypted account from a typed credential to
/// biometrics-only. Generates a credential nobody will ever know, wraps it
/// behind the OS prompt FIRST (a cancel changes nothing), then re-keys the
/// store from the live vault key to it — plaintext never touching disk, and
/// the wrap landing in the same transaction as the new `security_type`.
#[command]
pub async fn switch_to_biometric<R: Runtime>(handle: AppHandle<R>) -> Result<(), String> {
    #[cfg(target_os = "android")]
    {
        if !vector_core::state::is_encryption_enabled_fast() {
            return Err("Enable Local Encryption first".to_string());
        }
        if is_biometric_only() {
            return Err("This account already unlocks with biometrics".to_string());
        }
        let _switch = SwitchGuard::try_enter()?;
        let session = vector_core::state::SessionGuard::capture();
        let npub = crate::account_manager::get_current_account()?;
        // The derived key comes back from the wrap step: deriving it twice is
        // two full Argon2id runs AND an unwritten assumption that both agree
        // (which a per-account salt would silently break).
        let (_secret, wrapped, new_key) = generate_and_wrap(&npub).await?;
        let res = crate::commands::encryption::rekey_from_vault(
            handle, new_key, "biometric", Some(wrapped), session,
        )
        .await;
        if res.is_err() {
            // The store still uses the old key; the alias is an orphan.
            crate::android::biometric::remove_key(&keystore_alias(&npub));
        }
        res
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = handle;
        Err("Biometric unlock is Android-only".to_string())
    }
}

/// Unlock with biometrics: unwrap the vault key behind the OS prompt, VERIFY
/// it, prime the vault, and run the exact same login the PIN screen runs.
/// Returns the npub like `login_from_stored_key`.
///
/// Sentinel errors the frontend branches on:
/// - `BIOMETRIC_CANCELLED` — user dismissed the prompt; stay on the PIN pad.
/// - `BIOMETRIC_INVALIDATED` — enrollment is dead (biometric/lockscreen change
///   invalidated the keystore key, a restored DB has no matching key, or the
///   wrap predates a rekey). Already self-cleared; unlock with PIN, re-enable
///   in Settings.
/// - `BIOMETRIC_UNAVAILABLE` — a session precondition means this boot needs
///   the PIN path (rare self-heal states). Silent PIN fallback, no prompt shown.
#[command]
pub async fn biometric_login<R: Runtime>(handle: AppHandle<R>) -> Result<String, String> {
    #[cfg(target_os = "android")]
    {
        // Preconditions FIRST — `login_from_stored_key` has early branches
        // (stale bg-sync client, identity mismatch) that call reset_session,
        // which clears the vault this flow primes. Never spend a prompt on a
        // path that resets it.
        // A session that already exists (or is mid-boot from a credential the
        // user typed while the sheet was up) owns this boot. Delegating would
        // route into login_from_stored_key's live-client branch, which calls
        // reset_session and demolishes that session. Stand down instead.
        if crate::nostr_client().is_some() || crate::commands::account::full_session_initialized() {
            return Err("BIOMETRIC_UNAVAILABLE".to_string());
        }
        if let Some(in_memory_pk) = crate::my_public_key() {
            use nostr_sdk::prelude::ToBech32;
            let in_mem = in_memory_pk.to_bech32().ok();
            let marker = vector_core::db::read_active_account_file().ok().flatten()
                .or_else(|| crate::account_manager::get_current_account().ok());
            if let (Some(a), Some(b)) = (in_mem, marker) {
                if a != b {
                    return Err("BIOMETRIC_UNAVAILABLE".to_string());
                }
            }
        }

        // Snapshot session + account ONCE; the row read and the keystore alias
        // must both come from the same snapshot or a swap mid-flow could pair
        // account A's blob with account B's alias.
        let session = vector_core::state::SessionGuard::capture();
        let npub = crate::account_manager::get_current_account()?;
        let alias = keystore_alias(&npub);
        let wrapped = vector_core::db::get_sql_setting(WRAPPED_KEY_SETTING.to_string())
            .ok()
            .flatten()
            .ok_or("BIOMETRIC_NOT_ENROLLED")?;
        if !session.is_valid() {
            return Err("BIOMETRIC_UNAVAILABLE".to_string());
        }

        let unwrap_res = tokio::task::spawn_blocking(move || {
            crate::android::biometric::unlock_unwrap(&alias, &wrapped)
        })
        .await
        .map_err(|e| format!("join error: {:?}", e))?;

        let mut key = match unwrap_res {
            Ok(k) => k,
            Err(crate::android::biometric::BiometricError::Cancelled) => {
                return Err("BIOMETRIC_CANCELLED".to_string());
            }
            Err(crate::android::biometric::BiometricError::Invalidated) => {
                // Self-heal ONLY when this is still the same account (a swap
                // mid-prompt must not clear the swapped-in account's rows) AND
                // a typed credential exists to fall back on. For biometric-ONLY
                // accounts the wrap is the sole credential: OEM keystores throw
                // transient init failures, and one false positive here would
                // convert a retryable hiccup into permanent store loss. Leave
                // the enrollment intact; the recovery screen makes any wipe an
                // explicit user decision.
                if session.is_valid() && !is_biometric_only() {
                    clear_biometric_enrollment();
                }
                return Err("BIOMETRIC_INVALIDATED".to_string());
            }
            Err(crate::android::biometric::BiometricError::Other(msg)) => return Err(msg),
        };

        if !session.is_valid()
            || crate::nostr_client().is_some()
            || crate::commands::account::full_session_initialized()
        {
            // A credential login landed while the sheet was up — it owns the
            // session; priming the vault behind it would race its state.
            use zeroize::Zeroize;
            key.zeroize();
            return Err("BIOMETRIC_UNAVAILABLE".to_string());
        }

        // VERIFY with the raw bytes BEFORE the vault sees them. A wrap that
        // survived a rekey (crash between commit and cleanup) fails here and
        // self-heals; the vault is never primed with a wrong key.
        if !verify_candidate_key(&key) {
            use zeroize::Zeroize;
            key.zeroize();
            if session.is_valid() && !is_biometric_only() {
                clear_biometric_enrollment();
            }
            return Err("BIOMETRIC_INVALIDATED".to_string());
        }

        // Final gate before the vault: the verify reads above straddle DB I/O.
        if !session.is_valid() {
            use zeroize::Zeroize;
            key.zeroize();
            return Err("BIOMETRIC_UNAVAILABLE".to_string());
        }
        crate::ENCRYPTION_KEY.set(key, &[&crate::MY_SECRET_KEY]);
        {
            // [u8;32] is Copy — the vault got its own copy; wipe the local.
            use zeroize::Zeroize;
            key.zeroize();
        }

        // The prompt passed and the key checks out — let the UI flip to its
        // "Decrypting…" state while the real login (relays, sync) runs.
        let _ = handle.emit("biometric_unlocked", ());

        crate::commands::account::login_from_stored_key(None).await
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = handle;
        Err("Biometric unlock is Android-only".to_string())
    }
}
