//! Local Encryption: turning it on or off, changing the credential, and showing the keys. The
//! sweep itself is core's `db::at_rest`; this holds the account still while it runs.

use std::sync::atomic::{AtomicBool, Ordering};

use vector_core::db::{self, at_rest};
use vector_core::state;
use zeroize::Zeroizing;

pub use at_rest::MigrationProgress;

static MIGRATING: AtomicBool = AtomicBool::new(false);

/// One migration at a time: a second over the same vault would split it between two keys.
struct Migrating;

impl Migrating {
    fn enter() -> Result<Self, String> {
        MIGRATING
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| crate::t::security_migration_running().to_string())?;
        Ok(Self)
    }
}

impl Drop for Migrating {
    fn drop(&mut self) {
        MIGRATING.store(false, Ordering::Release);
    }
}

/// A swap mid-sweep would pull the connection out from under its transaction.
pub fn refuse_if_migrating() -> Result<(), String> {
    if MIGRATING.load(Ordering::Acquire) {
        Err(crate::t::security_wait_for_change().into())
    } else {
        Ok(())
    }
}

/// Whether the account is encrypted, and with a `pin` or a `password`.
pub fn status() -> (bool, String) {
    let on = state::is_encryption_enabled_fast();
    let kind = if on {
        db::get_sql_setting("security_type".into()).ok().flatten()
    } else {
        None
    };
    (on, kind.unwrap_or_else(|| "pin".into()))
}

pub async fn verify(credential: Zeroizing<String>) -> Result<(), String> {
    at_rest::prove_credential(&credential).await.map(drop)
}

/// The key from the user's own proof, never the vault: the typed credential, or the phone's
/// prompt for an account that has none.
pub(super) async fn prove(credential: Option<Zeroizing<String>>) -> Result<Zeroizing<[u8; 32]>, String> {
    if !state::is_encryption_enabled_fast() {
        return Err(crate::t::security_not_enabled().into());
    }
    #[cfg(target_os = "android")]
    if super::biometric::is_only_credential() {
        return super::biometric::prove().await;
    }
    let credential = credential
        .filter(|c| !c.is_empty())
        .ok_or(at_rest::CREDENTIAL_REQUIRED)?;
    at_rest::prove_credential(&credential).await
}

/// Inbound events wait at the gate while a sweep holds the store.
fn sweep<T>(run: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    state::close_processing_gate();
    let result = run();
    state::open_processing_gate();
    result
}

pub async fn enable(
    credential: Zeroizing<String>,
    kind: String,
    progress: impl Fn(MigrationProgress),
) -> Result<(), String> {
    if state::is_encryption_enabled_fast() {
        return Err(crate::t::security_already_enabled().into());
    }
    let session = db::current_session();
    let kdf = vector_core::crypto::Kdf::fresh();
    let key = Zeroizing::new(vector_core::crypto::derive_key(&credential, &kdf).await);
    drop(credential);
    let _one = Migrating::enter()?;
    if !session.is_live() {
        return Err(crate::t::security_account_changed().into());
    }
    if state::is_encryption_enabled_fast() {
        return Err(crate::t::security_already_enabled().into());
    }
    sweep(|| at_rest::enable(&key, &kdf, &kind, None, None, &progress))
}

pub async fn disable(
    credential: Option<Zeroizing<String>>,
    progress: impl Fn(MigrationProgress),
) -> Result<(), String> {
    let session = db::current_session();
    let biometric = super::biometric::is_only_credential();
    let key = prove(credential).await?;
    let _one = Migrating::enter()?;
    if !session.is_live() {
        return Err(crate::t::security_account_changed().into());
    }
    sweep(|| at_rest::disable(&key, &progress))?;
    if biometric {
        forget_keystore();
    }
    Ok(())
}

/// The keystore key of a wrap the store no longer holds.
fn forget_keystore() {
    #[cfg(target_os = "android")]
    if let Ok(npub) = db::get_current_account() {
        super::biometric::forget_keystore(&npub);
    }
}

/// Turn encryption on under a credential only the phone's prompt opens.
#[cfg(target_os = "android")]
pub async fn enable_biometric(progress: impl Fn(MigrationProgress)) -> Result<(), String> {
    if state::is_encryption_enabled_fast() {
        return Err(crate::t::security_already_enabled().into());
    }
    let session = db::current_session();
    let npub = db::get_current_account()?;
    let (wrapped, key, kdf) = super::biometric::generate_and_wrap(&npub).await?;
    let out = (|| {
        let _one = Migrating::enter()?;
        if !session.is_live() {
            return Err(crate::t::security_account_changed().to_string());
        }
        if state::is_encryption_enabled_fast() {
            return Err(crate::t::security_already_enabled().into());
        }
        sweep(|| at_rest::enable(&key, &kdf, "biometric", Some(&wrapped), None, &progress))
    })();
    if out.is_err() {
        super::biometric::forget_keystore(&npub);
    }
    out
}

/// Move a typed credential's account onto the phone's prompt, re-sealing in one transaction.
#[cfg(target_os = "android")]
pub async fn switch_to_biometric(
    credential: Zeroizing<String>,
    progress: impl Fn(MigrationProgress),
) -> Result<(), String> {
    let session = db::current_session();
    let old_key = prove(Some(credential)).await?;
    let npub = db::get_current_account()?;
    let (wrapped, new_key, kdf) = super::biometric::generate_and_wrap(&npub).await?;
    let out = (|| {
        let _one = Migrating::enter()?;
        if !session.is_live() {
            return Err(crate::t::security_account_changed().to_string());
        }
        sweep(|| at_rest::rekey(&old_key, &new_key, &kdf, "biometric", Some(&wrapped), &progress))
    })();
    if out.is_err() {
        super::biometric::forget_keystore(&npub);
    }
    out
}

/// Re-seal everything under a new credential, all in one transaction.
pub async fn rekey(
    old: Zeroizing<String>,
    new: Zeroizing<String>,
    kind: String,
    progress: impl Fn(MigrationProgress),
) -> Result<(), String> {
    let session = db::current_session();
    let _one = Migrating::enter()?;
    let biometric = super::biometric::is_only_credential();
    let old_key = prove(Some(old)).await?;
    let kdf = vector_core::crypto::Kdf::fresh();
    let new_key = Zeroizing::new(vector_core::crypto::derive_key(&new, &kdf).await);
    drop(new);
    if !session.is_live() {
        return Err(crate::t::security_account_changed().into());
    }
    sweep(|| at_rest::rekey(&old_key, &new_key, &kdf, &kind, None, &progress))?;
    if biometric {
        forget_keystore();
    }
    Ok(())
}

/// The account's nsec and seed phrase. An encrypted account opens them with a key from the
/// user's proof, so the live session alone can't read them out.
pub async fn export(
    credential: Option<Zeroizing<String>>,
) -> Result<(Zeroizing<String>, Option<Zeroizing<String>>), String> {
    if vector_core::is_keyless() {
        return Err(crate::t::security_export_external().into());
    }
    let session = db::current_session();
    let key = if state::is_encryption_enabled_fast() {
        Some(prove(credential).await?)
    } else {
        None
    };
    let secrets = at_rest::open_identity_secrets(key.as_deref())?;
    if !session.is_live() {
        return Err(crate::t::security_export_account_changed().into());
    }
    Ok(secrets)
}
