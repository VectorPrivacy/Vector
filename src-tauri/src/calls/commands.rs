//! The call commands. Each one is a thin door into the session state machine.

use super::session::{self, CallState};
use super::settings::{self, AudioSettings};

#[cfg(target_os = "android")]
fn ensure_microphone() -> Result<(), String> {
    use crate::android::permissions::{check_audio_permission, request_audio_permission_blocking};
    if check_audio_permission()? {
        return Ok(());
    }
    if request_audio_permission_blocking()? {
        Ok(())
    } else {
        Err("Microphone permission denied".to_string())
    }
}

#[cfg(not(target_os = "android"))]
fn ensure_microphone() -> Result<(), String> {
    Ok(())
}

/// Ring `npub`. Returns the ringing state; the rest arrives as `call_state` events.
#[tauri::command]
pub async fn call_start(npub: String) -> Result<CallState, String> {
    ensure_microphone()?;
    session::start(npub).await
}

#[tauri::command]
pub async fn call_accept() -> Result<(), String> {
    ensure_microphone()?;
    session::accept().await
}

#[tauri::command]
pub async fn call_reject() -> Result<(), String> {
    session::reject().await
}

#[tauri::command]
pub async fn call_hangup() -> Result<(), String> {
    session::hangup().await
}

#[tauri::command]
pub async fn call_set_muted(muted: bool) -> Result<(), String> {
    session::set_muted(muted).await
}

#[tauri::command]
pub async fn call_set_volume(volume: f32) -> Result<(), String> {
    session::set_volume(volume).await
}

/// The voice processing switches, as saved for this account.
#[tauri::command]
pub async fn call_audio_settings_get() -> AudioSettings {
    settings::load()
}

/// Applies mid-call and saves.
#[tauri::command]
pub async fn call_audio_settings_set(settings: AudioSettings) -> Result<(), String> {
    settings::set(settings)
}

/// Runs the microphone through the call's processing and streams `mic_level`
/// events until stopped. Refused during a call: the call's own meter serves then.
#[tauri::command]
pub async fn call_mic_test_start() -> Result<(), String> {
    if session::snapshot().is_some() {
        return Err("A call is in progress".into());
    }
    ensure_microphone()?;
    session::mic_test_start().await
}

#[tauri::command]
pub async fn call_mic_test_stop() {
    session::mic_test_stop();
}

/// The call in progress, if any: what a reloaded webview asks first.
#[tauri::command]
pub async fn call_status() -> Option<CallState> {
    session::snapshot()
}
