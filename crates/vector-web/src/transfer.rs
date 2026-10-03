//! Device transfer on Vector Web: the same protocol and gate as the desktop commands.

use std::future::Future;
use std::pin::Pin;

use serde_json::{json, Value};
use vector_core::transfer::service;
use vector_core::transfer::session::{ApproveError, Bundle, Role};
use zeroize::Zeroizing;

use crate::commands::Args;

const NOT_WAITING: &str = "The transfer is no longer waiting for approval";

pub fn dispatch<'a>(cmd: &'a str, a: &'a Args) -> Pin<Box<dyn Future<Output = Option<Result<Value, String>>> + 'a>> {
    Box::pin(async move { Some(match cmd {
        "transfer_start" => start(a).await,
        "transfer_check_number" => check_number(a).await,
        "transfer_approve" => approve(a).await,
        "transfer_deny" => {
            service::deny();
            Ok(Value::Null)
        }
        "transfer_cancel" => {
            service::cancel();
            Ok(Value::Null)
        }
        "transfer_finish" => finish().await,
        _ => return None,
    }) })
}

async fn start(a: &Args) -> Result<Value, String> {
    let send = a.get("send").and_then(Value::as_bool).unwrap_or(false);
    let identity = if send {
        if vector_core::is_keyless() {
            return Err("This account's key lives in your signer, so there's nothing to send from here. Sign in with your signer on the other device.".into());
        }
        Some(vector_core::my_public_key().ok_or("Sign in first")?)
    } else {
        None
    };
    let role = if send { Role::Sender } else { Role::Receiver };
    let started = service::start(role, a.opt_str("code").as_deref(), identity).await?;
    Ok(json!({ "id": started.id, "code": started.code, "qr": started.qr }))
}

async fn check_number(a: &Args) -> Result<Value, String> {
    service::check_number(&a.str("number")?).await.map(|_| Value::Null).map_err(|e| match e {
        ApproveError::WrongNumber { tries_left } => format!("TRANSFER_WRONG_NUMBER:{tries_left}"),
        _ => NOT_WAITING.to_string(),
    })
}

async fn approve(a: &Args) -> Result<Value, String> {
    if vector_core::is_keyless() || !service::sender_is_current() {
        return Err(NOT_WAITING.into());
    }
    let session = vector_core::db::current_session();
    let key = if vector_core::state::is_encryption_enabled_fast() {
        Some(crate::encryption::prove_user(a.opt_str("credential")).await?)
    } else {
        None
    };
    let (nsec, seed) = vector_core::db::at_rest::open_identity_secrets(key.as_deref())?;
    if !session.is_live() {
        return Err("The account changed, so nothing was sent".into());
    }
    let bundle = Bundle::from_stored(&nsec, seed.as_deref().map(|s| s.as_str()));
    service::approve(bundle).await.map(|_| Value::Null).map_err(|e| match e {
        ApproveError::NotYours => "This device's stored key doesn't match the account, so nothing was sent".to_string(),
        _ => NOT_WAITING.to_string(),
    })
}

/// Sign in with the account that arrived. It stays held until this succeeds, so a failed sign-in
/// can be tried again.
async fn finish() -> Result<Value, String> {
    let bundle = service::received().ok_or("Nothing has arrived to sign in with")?;
    let keys = nostr_sdk::prelude::Keys::parse(&bundle.nsec).map_err(|_| "The account that arrived is unreadable".to_string())?;
    let seed = bundle.seed.clone().map(Zeroizing::new);
    drop(bundle);
    // A create or import abandoned on this screen leaves its keys and client staged.
    crate::signers::cancel_session().await?;
    let result = crate::account::login_with_keys(keys, seed).await?;
    service::forget_received();
    Ok(result)
}
