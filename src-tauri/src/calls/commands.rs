//! The call commands. Each one is a thin door into the session state machine.

use super::session::{self, CallState};
use super::settings::{self, AudioSettings};
use super::transport::VideoKind;

/// Ring `npub`. Returns the ringing state; the rest arrives as `call_state` events.
/// `video` only tells the other side what kind of call this is; no camera starts here.
#[tauri::command]
pub async fn call_start(npub: String, video: Option<bool>) -> Result<CallState, String> {
    // Asked now, not at launch. A refusal still places the call: it goes on muted.
    crate::mic_access::ask().await;
    session::start(npub, video.unwrap_or(false)).await
}

/// The loopback socket the webview sends its encoded video through.
#[tauri::command]
pub async fn call_video_link() -> Result<String, String> {
    super::link::url()
}

/// Start or stop sending the camera or the screen; the other one is unaffected.
#[tauri::command]
pub async fn call_video_set(kind: VideoKind, on: bool) -> Result<(), String> {
    session::set_video(kind, on).await
}

/// The shared screen's sound goes with it, or stops. `native` asks the platform to
/// capture it itself, for a picker that gave none.
#[tauri::command]
pub async fn call_share_audio(on: bool, native: Option<bool>) -> Result<(), String> {
    session::set_share_audio(on, native.unwrap_or(false)).await
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
    crate::mic_access::ask().await;
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

/// Whether calls and Mini Apps may connect directly (each peer then learns this device's IP).
#[tauri::command]
pub async fn realtime_direct_get() -> bool {
    vector_core::db::scoped(async { vector_core::transport::realtime::direct() }).await
}

/// Takes effect once nothing uses the live node: a call in progress keeps the one it started on.
#[tauri::command]
pub async fn realtime_direct_set(on: bool) -> Result<(), String> {
    vector_core::db::scoped(vector_core::transport::realtime::set_direct(on)).await
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
    crate::mic_access::require().await?;
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

/// Join a voice Channel's call, leaving any other.
#[tauri::command]
pub async fn voice_join(community_id: String, channel_id: String) -> Result<vector_core::calls::voice::VoiceState, String> {
    vector_core::db::scoped(vector_core::calls::voice::join(&community_id, &channel_id)).await
}

#[tauri::command]
pub async fn voice_leave() {
    vector_core::db::scoped(vector_core::calls::voice::leave()).await
}

#[tauri::command]
pub async fn voice_state() -> Option<vector_core::calls::voice::VoiceState> {
    vector_core::db::scoped(async { vector_core::calls::voice::snapshot() }).await
}

#[tauri::command]
pub async fn voice_set_muted(on: bool) -> Result<(), String> {
    vector_core::db::scoped(async move { vector_core::calls::voice::set_muted(on) }).await
}

#[tauri::command]
pub async fn voice_set_deafened(on: bool) -> Result<(), String> {
    vector_core::db::scoped(async move { vector_core::calls::voice::set_deafened(on) }).await
}

/// Send our camera or screen to the voice call, or stop.
#[tauri::command]
pub async fn voice_video(kind: VideoKind, on: bool) -> Result<(), String> {
    vector_core::db::scoped(async move { vector_core::calls::voice::set_video(kind, on) }).await
}

/// Which of a member's pictures this device shows in the voice call.
#[tauri::command]
pub async fn voice_watch(npub: String, screen: bool, camera: bool) -> Result<(), String> {
    let member = nostr_sdk::prelude::PublicKey::parse(&npub).map_err(|e| e.to_string())?;
    let want = vector_core::calls::wire::Tracks { camera, screen };
    vector_core::db::scoped(async move { vector_core::calls::voice::watch(&member, want) }).await
}

/// How loud a member plays in this account's calls, 0 to 2; remembered.
#[tauri::command]
pub async fn voice_set_volume(npub: String, volume: f32) -> Result<(), String> {
    let pk = nostr_sdk::prelude::PublicKey::parse(&npub).map_err(|e| e.to_string())?;
    vector_core::db::scoped(async move { vector_core::calls::voice::set_peer_volume(&pk, volume) }).await
}

/// Mute, deafen, move or disconnect a member in a voice channel's call, as a moderator.
#[tauri::command]
pub async fn voice_moderate(community_id: String, channel_id: String, npub: String, action: String, to: Option<String>) -> Result<(), String> {
    let target = nostr_sdk::prelude::PublicKey::parse(&npub).map_err(|e| e.to_string())?;
    vector_core::db::scoped(async move { vector_core::calls::voice::moderate(&community_id, &channel_id, &target, &action, to.as_deref()).await }).await
}

/// What this account may do to a member in a voice call: { mute, deafen, move }.
#[tauri::command]
pub async fn voice_powers(community_id: String, npub: String) -> vector_core::calls::voice::Powers {
    let Ok(target) = nostr_sdk::prelude::PublicKey::parse(&npub) else { return Default::default() };
    vector_core::db::scoped(async move { vector_core::calls::voice::powers(&community_id, &target) }).await
}

/// Who is in a voice Channel's call now, whether or not we are, earliest first.
#[tauri::command]
pub async fn voice_present(channel_id: String) -> Vec<vector_core::calls::voice::InCall> {
    vector_core::db::scoped(async move { vector_core::calls::voice::in_call(&channel_id) }).await
}
