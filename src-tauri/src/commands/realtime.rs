//! Real-time signaling Tauri commands.
//!
//! This module handles ephemeral, real-time signals between users:
//! - Typing indicators
//! - WebXDC peer advertisement for P2P Mini Apps

use nostr_sdk::prelude::*;

use crate::{mls, NOSTR_CLIENT, TRUSTED_RELAYS};

// ============================================================================
// Typing Indicators
// ============================================================================

/// Send a typing indicator to a DM recipient or MLS group
#[tauri::command]
pub async fn start_typing(receiver: String) -> bool {
    let client = NOSTR_CLIENT.get().expect("Nostr client not initialized");
    let signer = client.signer().await.unwrap();
    let my_public_key = signer.get_public_key().await.unwrap();

    // Check if this is a group chat (group IDs are hex, not bech32)
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
                    TRUSTED_RELAYS.iter().copied(),
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
        Err(_) => {
            // This is a group chat - use MLS
            let group_id = receiver.clone();

            // Build the typing indicator rumor
            let rumor = EventBuilder::new(Kind::ApplicationSpecificData, "typing")
                .tag(Tag::custom(TagKind::d(), vec!["vector"]))
                .tag(Tag::expiration(Timestamp::from_secs(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_secs()
                        + 30,
                )))
                .build(my_public_key);

            // Send via MLS
            match mls::send_mls_message(&group_id, rumor, None).await {
                Ok(_) => true,
                Err(_e) => false,
            }
        }
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
    let client = NOSTR_CLIENT.get().expect("Nostr client not initialized");
    let signer = client.signer().await.unwrap();
    let my_public_key = signer.get_public_key().await.unwrap();
    let my_npub = my_public_key.to_bech32().unwrap_or_else(|_| "unknown".to_string());

    println!("[WEBXDC] Sending peer advertisement: my_npub={}, receiver={}, topic={}", my_npub, receiver, topic_id);
    log::info!("Sending WebXDC peer advertisement to {} for topic {}", receiver, topic_id);
    log::debug!("Node address: {}", node_addr);

    // Check if this is a group chat (group IDs are hex, not bech32)
    match PublicKey::from_bech32(receiver.as_str()) {
        Ok(pubkey) => {
            // This is a DM - use NIP-17 gift wrapping

            // Build the peer advertisement rumor
            let rumor = EventBuilder::new(Kind::ApplicationSpecificData, "peer-advertisement")
                .tag(Tag::public_key(pubkey))
                .tag(Tag::custom(TagKind::d(), vec!["vector-webxdc-peer"]))
                .tag(Tag::custom(TagKind::custom("webxdc-topic"), vec![topic_id]))
                .tag(Tag::custom(TagKind::custom("webxdc-node-addr"), vec![node_addr]))
                .tag(Tag::expiration(Timestamp::from_secs(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_secs()
                        + 300, // 5 minute expiry
                )))
                .build(my_public_key);

            // Gift Wrap and send to receiver via our Trusted Relays
            let expiry_time = Timestamp::from_secs(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs()
                    + 300,
            );
            match client
                .gift_wrap_to(
                    TRUSTED_RELAYS.iter().copied(),
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
        Err(_) => {
            // This is a group chat - use MLS
            let group_id = receiver.clone();

            // Build the peer advertisement rumor
            let rumor = EventBuilder::new(Kind::ApplicationSpecificData, "peer-advertisement")
                .tag(Tag::custom(TagKind::d(), vec!["vector-webxdc-peer"]))
                .tag(Tag::custom(TagKind::custom("webxdc-topic"), vec![topic_id]))
                .tag(Tag::custom(TagKind::custom("webxdc-node-addr"), vec![node_addr]))
                .tag(Tag::expiration(Timestamp::from_secs(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_secs()
                        + 300,
                )))
                .build(my_public_key);

            // Send via MLS
            match mls::send_mls_message(&group_id, rumor, None).await {
                Ok(_) => true,
                Err(_e) => false,
            }
        }
    }
}

// ============================================================================
// Live Subscriptions
// ============================================================================

/// Start live subscriptions for real-time events (GiftWraps + MLS messages).
/// Called once after login to begin receiving notifications.
#[tauri::command]
pub async fn notifs() -> Result<bool, String> {
    crate::services::start_subscriptions().await
}

// Handler list for this module (for reference):
// - start_typing
// - send_webxdc_peer_advertisement
// - notifs
