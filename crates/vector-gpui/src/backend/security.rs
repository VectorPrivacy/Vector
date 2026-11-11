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
            .map_err(|_| "A migration is already in progress".to_string())?;
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
    if MIGRATING.load(Ordering::Acquire) { Err("Wait for the encryption change to finish".into()) } else { Ok(()) }
}

/// Whether the account is encrypted, and with a `pin` or a `password`.
pub fn status() -> (bool, String) {
    let on = state::is_encryption_enabled_fast();
    let kind = if on { db::get_sql_setting("security_type".into()).ok().flatten() } else { None };
    (on, kind.unwrap_or_else(|| "pin".into()))
}

pub async fn verify(credential: Zeroizing<String>) -> Result<(), String> {
    at_rest::prove_credential(&credential).await.map(drop)
}

/// The key from the user's own proof, never the vault.
async fn prove(credential: Option<Zeroizing<String>>) -> Result<Zeroizing<[u8; 32]>, String> {
    if !state::is_encryption_enabled_fast() {
        return Err("Local Encryption is not enabled".into());
    }
    let credential = credential.filter(|c| !c.is_empty()).ok_or(at_rest::CREDENTIAL_REQUIRED)?;
    at_rest::prove_credential(&credential).await
}

/// Inbound events wait at the gate while a sweep holds the store.
fn sweep<T>(run: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    state::close_processing_gate();
    let result = run();
    state::open_processing_gate();
    result
}

pub async fn enable(credential: Zeroizing<String>, kind: String, progress: impl Fn(MigrationProgress)) -> Result<(), String> {
    if state::is_encryption_enabled_fast() {
        return Err("Encryption is already enabled".into());
    }
    let session = db::current_session();
    let kdf = vector_core::crypto::Kdf::fresh();
    let key = Zeroizing::new(vector_core::crypto::derive_key(&credential, &kdf).await);
    drop(credential);
    let _one = Migrating::enter()?;
    if !session.is_live() {
        return Err("Account changed, nothing was modified".into());
    }
    if state::is_encryption_enabled_fast() {
        return Err("Encryption is already enabled".into());
    }
    sweep(|| at_rest::enable(&key, &kdf, &kind, None, None, &progress))
}

pub async fn disable(credential: Option<Zeroizing<String>>, progress: impl Fn(MigrationProgress)) -> Result<(), String> {
    let session = db::current_session();
    let key = prove(credential).await?;
    let _one = Migrating::enter()?;
    if !session.is_live() {
        return Err("Account changed, nothing was modified".into());
    }
    sweep(|| at_rest::disable(&key, &progress))
}

/// Re-seal everything under a new credential, all in one transaction.
pub async fn rekey(old: Zeroizing<String>, new: Zeroizing<String>, kind: String, progress: impl Fn(MigrationProgress)) -> Result<(), String> {
    let session = db::current_session();
    let _one = Migrating::enter()?;
    let old_key = at_rest::prove_credential(&old).await?;
    drop(old);
    let kdf = vector_core::crypto::Kdf::fresh();
    let new_key = Zeroizing::new(vector_core::crypto::derive_key(&new, &kdf).await);
    drop(new);
    if !session.is_live() {
        return Err("Account changed, nothing was modified".into());
    }
    sweep(|| at_rest::rekey(&old_key, &new_key, &kdf, &kind, None, &progress))
}

/// The account's nsec and seed phrase. An encrypted account opens them with a key from the
/// user's proof, so the live session alone can't read them out.
pub async fn export(credential: Option<Zeroizing<String>>) -> Result<(Zeroizing<String>, Option<Zeroizing<String>>), String> {
    if vector_core::is_keyless() {
        return Err("This is an external signer account. Your identity key lives on your signer app, never on this device, so there's nothing to export here.".into());
    }
    let session = db::current_session();
    let key = if state::is_encryption_enabled_fast() { Some(prove(credential).await?) } else { None };
    let secrets = at_rest::open_identity_secrets(key.as_deref())?;
    if !session.is_live() {
        return Err("The account changed, so nothing was exported".into());
    }
    Ok(secrets)
}
