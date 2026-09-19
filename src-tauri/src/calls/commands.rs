//! The call commands. Each one is a thin door into the session state machine.

use super::session::{self, CallState};
use super::settings::{self, AudioSettings};
use super::transport::VideoKind;

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
/// `video` only tells the other side what kind of call this is; no camera starts here.
#[tauri::command]
pub async fn call_start(npub: String, video: Option<bool>) -> Result<CallState, String> {
    ensure_microphone()?;
    session::start(npub, video.unwrap_or(false)).await
}

/// The loopback socket the webview sends its encoded video through.
#[tauri::command]
pub async fn call_video_link() -> Result<String, String> {
    session::video_link_url()
}

/// Start or stop sending the camera or the screen; the other one is unaffected.
#[tauri::command]
pub async fn call_video_set(kind: VideoKind, on: bool) -> Result<(), String> {
    session::set_video(kind, on).await
}

/// The shared screen's sound goes with it, or stops.
#[tauri::command]
pub async fn call_share_audio(on: bool) -> Result<(), String> {
    session::set_share_audio(on).await
}

/// Listener-side volume for their screen's sound.
#[tauri::command]
pub async fn call_set_share_volume(volume: f32) -> Result<(), String> {
    session::set_share_volume(volume).await
}

/// A held quality rung and frame rate for one of our pictures; None lets the ladder decide.
#[tauri::command]
pub async fn call_video_prefs(kind: VideoKind, rung: Option<u32>, fps: Option<u32>) -> Result<(), String> {
    session::set_video_prefs(kind, rung.map(|r| r as usize), fps)
}

/// Our view of the peer's picture is hidden or shown; they may stop sending.
#[tauri::command]
pub async fn call_video_pause(on: bool) -> Result<(), String> {
    session::set_video_pause(on).await
}

/// What the webview found it can encode and decode, from its boot probe.
#[tauri::command]
pub async fn call_video_caps(encode: Vec<String>, decode: Vec<String>) {
    session::set_video_caps(encode, decode);
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
