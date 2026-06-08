//! Real-time signaling Tauri commands.
//!
//! This module handles ephemeral, real-time signals between users:
//! - Typing indicators
//! - WebXDC peer advertisement for P2P Mini Apps

use nostr_sdk::prelude::*;

use crate::{nostr_client, active_trusted_relays};

// ============================================================================
// Typing Indicators
// ============================================================================

/// Send a typing indicator to a DM recipient
#[tauri::command]
pub async fn start_typing(receiver: String) -> bool {
    // Return false on no-session — typing fires continuously from
    // keystrokes, so a panic here would crash the runtime mid-swap.
    let Some(client) = nostr_client() else { return false; };
    let Some(my_public_key) = crate::my_public_key() else { return false; };

    match PublicKey::from_bech32(receiver.as_str()) {
        Ok(pubkey) => {
            // This is a DM - use NIP-17 gift wrapping

            // Build and broadcast the Typing Indicator
            let rumor = EventBuilder::new(Kind::ApplicationSpecificData, "typing")
                .tag(Tag::public_key(pubkey))
                .tag(Tag::custom(TagKind::d(), vec!["vector"]))
                .tag(Tag::expiration(Timestamp::from_secs(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_secs()
                        + 30,
                )))
                .build(my_public_key);

            // Gift Wrap and send our Typing Indicator to receiver via our Trusted Relay
            // Note: we set a 30-second expiry so that relays can purge typing indicators quickly
            let expiry_time = Timestamp::from_secs(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs()
                    + 30,
            );
            match client
                .gift_wrap_to(
                    active_trusted_relays().await.into_iter(),
                    &pubkey,
                    rumor,
                    [Tag::expiration(expiry_time)],
                )
                .await
            {
                Ok(_) => true,
                Err(_) => false,
            }
        }
        // Non-DM (hex) targets have no ephemeral typing transport.
        Err(_) => false,
    }
}

// ============================================================================
// WebXDC Peer Discovery
// ============================================================================

/// Send a WebXDC peer advertisement to chat participants
/// This announces our Iroh node address for realtime channel peer discovery
#[tauri::command]
pub async fn send_webxdc_peer_advertisement(
    receiver: String,
    topic_id: String,
    node_addr: String,
) -> bool {
    let Some(client) = nostr_client() else { return false; };
    let Some(my_public_key) = crate::my_public_key() else { return false; };

    log_info!("Sending WebXDC peer advertisement to {} for topic {}", receiver, topic_id);
    log_debug!("Node address: {}", node_addr);

    match PublicKey::from_bech32(receiver.as_str()) {
        Ok(pubkey) => {
            // This is a DM - use NIP-17 gift wrapping

            // Build the peer advertisement rumor (no expiry — peer-left signal handles cleanup)
            let rumor = EventBuilder::new(Kind::ApplicationSpecificData, "peer-advertisement")
                .tag(Tag::public_key(pubkey))
                .tag(Tag::custom(TagKind::d(), vec!["vector-webxdc-peer"]))
                .tag(Tag::custom(TagKind::custom("webxdc-topic"), vec![topic_id]))
                .tag(Tag::custom(TagKind::custom("webxdc-node-addr"), vec![node_addr]))
                .build(my_public_key);

            // Gift Wrap and send to receiver via our Trusted Relays
            match client
                .gift_wrap_to(
                    active_trusted_relays().await.into_iter(),
                    &pubkey,
                    rumor,
                    [],
                )
                .await
            {
                Ok(_) => true,
                Err(_) => false,
            }
        }
        // Non-DM (hex) targets have no WebXDC peer transport.
        Err(_) => false,
    }
}

/// Send a "peer-left" signal so other clients know we've stopped playing.
/// Same transport as peer advertisements (NIP-17 DM gift wrap).
pub async fn send_webxdc_peer_left(
    receiver: String,
    topic_id: String,
) -> bool {
    let Some(client) = nostr_client() else { return false; };
    let Some(my_public_key) = crate::my_public_key() else { return false; };

    log_info!("Sending WebXDC peer-left to {} for topic {}", receiver, topic_id);

    match PublicKey::from_bech32(receiver.as_str()) {
        Ok(pubkey) => {
            let rumor = EventBuilder::new(Kind::ApplicationSpecificData, "peer-left")
                .tag(Tag::public_key(pubkey))
                .tag(Tag::custom(TagKind::d(), vec!["vector-webxdc-peer"]))
                .tag(Tag::custom(TagKind::custom("webxdc-topic"), vec![topic_id]))
                .build(my_public_key);

            match client
                .gift_wrap_to(
                    active_trusted_relays().await.into_iter(),
                    &pubkey,
                    rumor,
                    [],
                )
                .await
            {
                Ok(_) => true,
                Err(_) => false,
            }
        }
        // Non-DM (hex) targets have no WebXDC peer transport.
        Err(_) => false,
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
// - send_webxdc_peer_advertisement
// - notifs
