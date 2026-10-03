//! Device transfer: sign this account in on another device, or this device in from another.
//! The protocol lives in vector-core; these commands add the proof that opens the key.

use tauri::{command, AppHandle, Runtime};
use vector_core::transfer::service;
use vector_core::transfer::session::{ApproveError, Bundle, Role};
use zeroize::Zeroizing;

use crate::commands::account::LoginResult;

#[derive(serde::Serialize)]
pub struct TransferStarted {
    id: u64,
    code: Option<String>,
    qr: Option<String>,
}

const NOT_WAITING: &str = "The transfer is no longer waiting for approval";

/// Start a transfer. `send` when this device holds the account; `code` to join the other
/// device's code rather than show one here.
#[command]
pub async fn transfer_start(send: bool, code: Option<String>) -> Result<TransferStarted, String> {
    let identity = if send {
        if vector_core::is_keyless() {
            return Err("This account's key lives in your signer app, so there's nothing to send from here. Sign in with your signer on the other device.".into());
        }
        Some(crate::my_public_key().ok_or("Sign in first")?)
    } else {
        None
    };
    let role = if send { Role::Sender } else { Role::Receiver };
    let started = service::start(role, code.as_deref(), identity).await?;
    Ok(TransferStarted { id: started.id, code: started.code, qr: started.qr })
}

/// Sender: the number from the new device's screen.
#[command]
pub async fn transfer_check_number(number: String) -> Result<(), String> {
    service::check_number(&number).await.map_err(|e| match e {
        ApproveError::WrongNumber { tries_left } => format!("TRANSFER_WRONG_NUMBER:{tries_left}"),
        _ => NOT_WAITING.to_string(),
    })
}

/// Sender: hand the account over. The key opens under the user's own proof, as for Export.
#[command]
pub async fn transfer_approve(credential: Option<String>) -> Result<(), String> {
    if vector_core::is_keyless() || !service::sender_is_current() {
        return Err(NOT_WAITING.into());
    }
    let session = vector_core::db::current_session();
    let key = if vector_core::state::is_encryption_enabled_fast() {
        Some(crate::commands::encryption::prove_user(credential).await?)
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

#[command]
pub fn transfer_deny() {
    service::deny();
}

#[command]
pub fn transfer_cancel() {
    service::cancel();
}

/// Receiver: sign in with the account that arrived. It stays held until this succeeds, so a
/// failed sign-in can be tried again.
#[command]
pub async fn transfer_finish<R: Runtime>(handle: AppHandle<R>) -> Result<LoginResult, String> {
    let bundle = service::received().ok_or("Nothing has arrived to sign in with")?;
    let keys = nostr_sdk::prelude::Keys::parse(&bundle.nsec).map_err(|_| "The account that arrived is unreadable".to_string())?;
    let seed = bundle.seed.clone().map(Zeroizing::new);
    drop(bundle);
    // A create or import abandoned on this screen leaves its keys and client staged.
    crate::commands::account::clear_pending_bunker_session().await;
    let result = crate::commands::account::login_with_keys(handle, keys, seed).await?;
    service::forget_received();
    Ok(result)
}
