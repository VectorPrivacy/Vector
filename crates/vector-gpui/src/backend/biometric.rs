//! Unlock with the phone's fingerprint or screen lock: the vault key the Tauri client wrapped in
//! the Android Keystore, opened behind the system's own prompt. An account whose only credential
//! is that wrap has a generated PIN nobody knows, so this is its one way in.

use vector_core::db;
#[cfg(target_os = "android")]
use vector_core::state;
#[cfg(target_os = "android")]
use zeroize::Zeroizing;

/// Base64 of the wrapped vault key, present while the account is enrolled.
const WRAPPED_KEY_SETTING: &str = "biometric_wrapped_key";

/// The prompt was dismissed: the screen stays as it was.
pub const CANCELLED: &str = "BIOMETRIC_CANCELLED";
/// The wrap no longer opens this account: a new fingerprint or screen lock, or a restored store.
pub const INVALIDATED: &str = "BIOMETRIC_INVALIDATED";
/// Another sign-in owns this boot.
pub const UNAVAILABLE: &str = "BIOMETRIC_UNAVAILABLE";

pub struct Status {
    /// The phone can show the prompt (Android 11+, a screen lock set).
    pub supported: bool,
    /// This account holds a wrapped key.
    pub enrolled: bool,
    /// The system's own words for the prompt ("Use fingerprint"); empty when it has none.
    pub label: String,
}

pub async fn status() -> Status {
    let enrolled = db::get_sql_setting(WRAPPED_KEY_SETTING.into()).ok().flatten().is_some();
    #[cfg(target_os = "android")]
    {
        use crate::android::biometric as bio;
        let (avail, label) = tokio::task::spawn_blocking(|| {
            (bio::availability().unwrap_or_default(), bio::unlock_label().unwrap_or_default())
        })
        .await
        .unwrap_or_default();
        Status { supported: avail == "available", enrolled, label }
    }
    #[cfg(not(target_os = "android"))]
    Status { supported: false, enrolled, label: String::new() }
}

/// Whether the phone can show the prompt, asked on the spot.
pub fn supported() -> bool {
    #[cfg(target_os = "android")]
    {
        crate::android::biometric::availability().is_ok_and(|a| a == "available")
    }
    #[cfg(not(target_os = "android"))]
    {
        false
    }
}

/// The account unlocks only with the wrap.
pub fn is_only_credential() -> bool {
    vector_core::state::is_encryption_enabled_fast()
        && db::get_sql_setting("security_type".into()).ok().flatten().as_deref() == Some("biometric")
}

#[cfg(target_os = "android")]
fn alias(npub: &str) -> String {
    format!("vector_bio_{npub}")
}

/// The keystore key behind `npub`'s wrap goes, once nothing holds a wrap it made.
#[cfg(target_os = "android")]
pub fn forget_keystore(npub: &str) {
    crate::android::biometric::remove_key(&alias(npub));
}

/// Drops a dead wrap, unless it is the account's only credential: a keystore's passing hiccup
/// must never turn into a lost store, so that reset stays the user's call.
#[cfg(target_os = "android")]
fn forget_dead_wrap() {
    if is_only_credential() {
        return;
    }
    let _ = db::remove_setting(WRAPPED_KEY_SETTING);
    if let Ok(npub) = db::get_current_account() {
        forget_keystore(&npub);
    }
}

/// The current account's vault key, behind the prompt; not yet proven against the store.
#[cfg(target_os = "android")]
async fn unwrap_current() -> Result<Zeroizing<[u8; 32]>, String> {
    use crate::android::biometric::{self as bio, BiometricError};
    // The wrap and its keystore alias from one account, whatever swaps meanwhile.
    let alias = alias(&db::get_current_account()?);
    let wrapped = db::get_sql_setting(WRAPPED_KEY_SETTING.into())
        .ok()
        .flatten()
        .ok_or_else(|| INVALIDATED.to_string())?;
    // At launch the unlock screen can be up a moment before the activity resumes.
    for _ in 0..30 {
        if crate::android::background_sync::is_activity_in_foreground() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    // Parks on the prompt's answer.
    let unwrapped = tokio::task::spawn_blocking(move || bio::unlock_unwrap(&alias, &wrapped))
        .await
        .map_err(|e| e.to_string())?;
    unwrapped.map(Zeroizing::new).map_err(|e| match e {
        BiometricError::Cancelled => CANCELLED.into(),
        BiometricError::Invalidated => INVALIDATED.into(),
        BiometricError::Other(e) => e,
    })
}

/// Opens the vault key behind the prompt, then unlocks as a typed PIN would. `passed` runs once
/// the prompt has, before the slower sign-in.
#[cfg(target_os = "android")]
pub async fn unlock(passed: impl FnOnce() + Send + 'static) -> Result<String, String> {
    if state::nostr_client().is_some() {
        return Err(UNAVAILABLE.into());
    }
    let session = db::current_session();
    let key = match unwrap_current().await {
        Err(e) if e == INVALIDATED && session.is_live() => {
            forget_dead_wrap();
            return Err(e);
        }
        other => other?,
    };
    // A PIN typed while the sheet was up got there first.
    if !session.is_live() || state::nostr_client().is_some() {
        return Err(UNAVAILABLE.into());
    }
    passed();
    let out = super::account::unlock_with_key(key).await;
    if matches!(&out, Err(e) if e == INVALIDATED) && session.is_live() {
        forget_dead_wrap();
    }
    out
}

/// The vault key behind the prompt, proven against the store: a biometric account's answer to
/// "enter your PIN".
#[cfg(target_os = "android")]
pub async fn prove() -> Result<Zeroizing<[u8; 32]>, String> {
    let session = db::current_session();
    let key = unwrap_current().await?;
    if !session.is_live() {
        return Err(crate::t::security_account_changed().into());
    }
    db::at_rest::prove_key(&key)?;
    Ok(key)
}

/// A credential nobody will ever know, derived like a typed PIN, its key wrapped behind one
/// prompt for `npub` before anything commits: a cancel leaves the account as it was.
#[cfg(target_os = "android")]
pub async fn generate_and_wrap(
    npub: &str,
) -> Result<(String, Zeroizing<[u8; 32]>, vector_core::crypto::Kdf), String> {
    use crate::android::biometric::{self as bio, BiometricError};
    use rand::RngCore;
    let mut raw = Zeroizing::new([0u8; 32]);
    rand::rngs::OsRng.fill_bytes(&mut *raw);
    let secret = Zeroizing::new(vector_core::simd::hex::bytes_to_hex_32(&raw));
    drop(raw);
    let kdf = vector_core::crypto::Kdf::fresh();
    let key = Zeroizing::new(vector_core::crypto::derive_key(&secret, &kdf).await);
    drop(secret);
    let alias = alias(npub);
    let to_wrap = key.clone();
    let wrapped = tokio::task::spawn_blocking(move || bio::enroll_wrap(&alias, &to_wrap))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| match e {
            BiometricError::Cancelled => CANCELLED.into(),
            BiometricError::Invalidated => crate::t::security_biometric_rejected().to_string(),
            BiometricError::Other(e) => e,
        })?;
    Ok((wrapped, key, kdf))
}
