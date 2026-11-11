//! Device transfer: sign this account in on another device, or this device in from another. The
//! protocol is core's; this adds the proof that opens the key, and the sign-in at the far end.

use vector_core::transfer::service;
use vector_core::transfer::session::{ApproveError, Bundle, Role};
use zeroize::Zeroizing;

const NOT_WAITING: &str = "The transfer is no longer waiting for approval";

pub struct Started {
    pub id: u64,
    pub code: Option<String>,
    pub qr: Option<String>,
}

/// `send` when this device holds the account; `code` to join the other device's code.
pub async fn start(send: bool, code: Option<String>) -> Result<Started, String> {
    let identity = if send {
        if vector_core::is_keyless() {
            return Err("This account's key lives in your signer app, so there's nothing to send from here. Sign in with your signer on the other device.".into());
        }
        Some(vector_core::state::my_public_key().ok_or("Sign in first")?)
    } else {
        None
    };
    let role = if send { Role::Sender } else { Role::Receiver };
    let s = service::start(role, code.as_deref(), identity).await?;
    Ok(Started { id: s.id, code: s.code, qr: s.qr })
}

/// Sender: the number from the new device's screen. `Err(Some(n))` when wrong, `n` tries left.
pub async fn check_number(number: String) -> Result<(), Result<u32, String>> {
    service::check_number(&number).await.map_err(|e| match e {
        ApproveError::WrongNumber { tries_left } => Ok(tries_left as u32),
        _ => Err(NOT_WAITING.to_string()),
    })
}

/// Sender: hand the account over, its key opened under the user's own proof.
pub async fn approve(credential: Option<Zeroizing<String>>) -> Result<(), String> {
    if vector_core::is_keyless() || !service::sender_is_current() {
        return Err(NOT_WAITING.into());
    }
    let session = vector_core::db::current_session();
    let key = if vector_core::state::is_encryption_enabled_fast() {
        let credential = credential.filter(|c| !c.is_empty()).ok_or(vector_core::db::at_rest::CREDENTIAL_REQUIRED)?;
        Some(vector_core::db::at_rest::prove_credential(&credential).await?)
    } else {
        None
    };
    let (nsec, seed) = vector_core::db::at_rest::open_identity_secrets(key.as_deref())?;
    if !session.is_live() {
        return Err("The account changed, so nothing was sent".into());
    }
    let bundle = Bundle::from_stored(&nsec, seed.as_deref().map(|s| s.as_str()));
    service::approve(bundle).await.map_err(|e| match e {
        ApproveError::NotYours => "This device's stored key doesn't match the account, so nothing was sent".to_string(),
        _ => NOT_WAITING.to_string(),
    })
}

pub fn deny() {
    service::deny();
}

pub fn cancel() {
    service::cancel();
}

/// Receiver: sign in with the account that arrived. It stays held until this succeeds, so a
/// failed sign-in can be tried again.
pub async fn finish(reset: bool) -> Result<super::account::Imported, String> {
    let bundle = service::received().ok_or("Nothing has arrived to sign in with")?;
    let nsec = bundle.nsec.clone();
    let seed = bundle.seed.clone().map(Zeroizing::new);
    drop(bundle);
    if reset {
        super::account::reset().await;
    }
    let imported = super::account::import(nsec).await?;
    if matches!(imported, super::account::Imported::Staged(_)) {
        *vector_core::state::MNEMONIC_SEED.lock().unwrap() = seed.map(|s| s.to_string());
        super::account::connect().await;
    }
    service::forget_received();
    Ok(imported)
}
