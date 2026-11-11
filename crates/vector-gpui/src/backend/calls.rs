//! The call commands, as the desktop shell's: each a thin door into core's session.

use crate::calls::session;

/// Ring `npub`; the rest arrives as `call_state` events. A refused microphone still places it, muted.
pub async fn start(npub: String) -> Result<(), String> {
    crate::mic_access::ask().await;
    vector_core::db::scoped(session::start(npub, false))
        .await
        .map(|_| ())
}

pub async fn accept() -> Result<(), String> {
    crate::mic_access::ask().await;
    vector_core::db::scoped(session::accept()).await
}

pub async fn reject() -> Result<(), String> {
    vector_core::db::scoped(session::reject()).await
}

pub async fn hangup() -> Result<(), String> {
    vector_core::db::scoped(session::hangup()).await
}

pub async fn set_muted(on: bool) -> Result<(), String> {
    vector_core::db::scoped(session::set_muted(on)).await
}

pub async fn set_volume(volume: f32) -> Result<(), String> {
    vector_core::db::scoped(session::set_volume(volume)).await
}

/// The call the session holds, for a UI that missed its events.
pub fn status() -> Option<serde_json::Value> {
    session::snapshot().and_then(|s| serde_json::to_value(s).ok())
}

/// Off Clearnet, calls connect only once the account agrees; the label names the network.
pub fn off_clearnet_label() -> Option<String> {
    use vector_core::transport::Kind;
    match vector_core::transport::preference() {
        Some(Kind::Clearnet) => None,
        Some(k) => Some(k.label().to_string()),
        None => Some("the network in use".into()),
    }
}

pub fn allow_realtime() {
    vector_core::transport::realtime::grant();
}

/// The engine's own microphone test: the call's capture chain, metered by `mic_level` events.
pub async fn mic_test_start() -> Result<(), String> {
    if session::snapshot().is_some() {
        return Err("A call is in progress".into());
    }
    crate::mic_access::require().await?;
    vector_core::db::scoped(session::mic_test_start()).await
}

pub fn mic_test_stop() {
    session::mic_test_stop();
}
