//! Real-time signaling Tauri commands.
//!
//! This module handles ephemeral, real-time signals between users: typing
//! indicators, and the live subscriptions. (Mini App peer signals are
//! vector-core's: `vector_core::xdc::signal`.)

use nostr_sdk::prelude::*;


// ============================================================================
// Typing Indicators
// ============================================================================

/// Send a typing indicator to a DM recipient
#[tauri::command]
pub async fn start_typing(receiver: String) -> bool {
    // Return false on no-session — typing fires continuously from
    // keystrokes, so a panic here would crash the runtime mid-swap.
    if crate::my_public_key().is_none() {
        return false;
    }

    match PublicKey::from_bech32(receiver.as_str()) {
        // A DM — vector-core owns the NIP-17 typing pipeline (30s expiry).
        Ok(_) => vector_core::VectorCore.send_typing(&receiver).await.is_ok(),
        // A hex target is a Community channel — publish an ephemeral typing signal over Concord.
        Err(_) => crate::commands::community::send_community_typing(receiver.as_str()).await,
    }
}

// ============================================================================
// Live Subscriptions
// ============================================================================

/// Start live subscriptions for real-time events (GiftWraps + Community messages).
/// Called once after login to begin receiving notifications.
#[tauri::command]
pub async fn notifs() -> Result<bool, String> {
    crate::services::start_subscriptions().await
}

// Handler list for this module (for reference):
// - start_typing
// - notifs
