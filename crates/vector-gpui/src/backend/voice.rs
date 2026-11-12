//! Voice Channel calls, through the core's session: join, leave, who is in each call, and a
//! moderator's directives.

use nostr_sdk::prelude::*;
use serde_json::Value;
use vector_core::calls::voice;
use vector_core::db;

/// Join a voice Channel's call, leaving any other.
pub async fn join(community_id: String, channel_id: String) -> Result<Value, String> {
    crate::mic_access::ask().await;
    let state = db::scoped(async move { voice::join(&community_id, &channel_id).await }).await?;
    serde_json::to_value(state).map_err(|e| e.to_string())
}

pub async fn leave() {
    db::scoped(voice::leave()).await
}

pub fn state() -> Option<Value> {
    voice::snapshot().and_then(|s| serde_json::to_value(s).ok())
}

pub fn present(channel_id: &str) -> Value {
    serde_json::to_value(voice::in_call(channel_id)).unwrap_or_default()
}

pub fn set_muted(on: bool) -> Result<(), String> {
    voice::set_muted(on)
}

pub fn set_deafened(on: bool) -> Result<(), String> {
    voice::set_deafened(on)
}

pub fn set_volume(npub: &str, volume: f32) -> Result<(), String> {
    let pk = PublicKey::from_bech32(npub).map_err(|e| e.to_string())?;
    voice::set_peer_volume(&pk, volume)
}

/// What this account may do to `npub` here: (mute, deafen, move).
pub fn powers(community_id: &str, npub: &str) -> (bool, bool, bool) {
    let Ok(pk) = PublicKey::from_bech32(npub) else { return (false, false, false) };
    let p = voice::powers(community_id, &pk);
    (p.mute, p.deafen, p.move_)
}

pub async fn moderate(community_id: String, channel_id: String, npub: String, action: &'static str, to: Option<String>) -> Result<(), String> {
    let pk = PublicKey::from_bech32(&npub).map_err(|e| e.to_string())?;
    db::scoped(async move { voice::moderate(&community_id, &channel_id, &pk, action, to.as_deref()).await }).await
}
