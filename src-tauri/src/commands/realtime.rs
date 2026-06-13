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
        // A hex target is a Community channel — publish an ephemeral typing signal over Concord.
        Err(_) => crate::commands::community::send_community_typing(receiver.as_str()).await,
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
    // The receiver/chat_id was captured by the caller under the CURRENT account; a swap
    // mid-await would sign + publish the NEW account's identity into the OLD account's chat.
    let session = vector_core::state::SessionGuard::capture();
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

            let relays = active_trusted_relays().await;
            if !session.is_valid() {
                return false;
            }
            // Gift Wrap and send to receiver via our Trusted Relays
            match client.gift_wrap_to(relays.into_iter(), &pubkey, rumor, []).await {
                Ok(_) => true,
                Err(_) => false,
            }
        }
        // Non-bech32 target = a Community channel id → the Concord carrier (kind 3310).
        Err(_) => send_community_webxdc_signal(&receiver, &topic_id, Some(&node_addr), &session).await,
    }
}

/// Send a "peer-left" signal so other clients know we've stopped playing.
/// Same transports as peer advertisements (NIP-17 DM gift wrap / Concord 3310).
pub async fn send_webxdc_peer_left(
    receiver: String,
    topic_id: String,
) -> bool {
    // Same swap exposure as the advertisement — see send_webxdc_peer_advertisement.
    let session = vector_core::state::SessionGuard::capture();
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

            let relays = active_trusted_relays().await;
            if !session.is_valid() {
                return false;
            }
            match client.gift_wrap_to(relays.into_iter(), &pubkey, rumor, []).await {
                Ok(_) => true,
                Err(_) => false,
            }
        }
        // Non-bech32 target = a Community channel id → the Concord carrier (kind 3310).
        Err(_) => send_community_webxdc_signal(&receiver, &topic_id, None, &session).await,
    }
}

/// Publish a WebXDC peer signal into a Community channel (kind 3310, sealed under the channel
/// epoch key). `node_addr` Some = advertisement, None = peer-left. Best-effort bool to match
/// the DM twin — a missed ad only delays discovery until the auto re-advertise.
async fn send_community_webxdc_signal(
    channel_id: &str,
    topic_id: &str,
    node_addr: Option<&str>,
    session: &vector_core::state::SessionGuard,
) -> bool {
    use vector_core::community::{service, transport::LiveTransport};
    let (community, channel) = match crate::commands::community::resolve_community_channel(channel_id) {
        Ok(rc) => rc,
        Err(e) => {
            log_warn!("[WEBXDC] No Community channel for peer signal target {}: {}", channel_id, e);
            return false;
        }
    };
    // Re-check after the DB resolve: a swap here would publish the NEW account's identity
    // (both accounts can be members of the same community) under the OLD caller's intent.
    if !session.is_valid() {
        return false;
    }
    let transport = LiveTransport::with_timeout(std::time::Duration::from_secs(12));
    match service::publish_webxdc_signal(&transport, &community, &channel, topic_id, node_addr).await {
        Ok(()) => true,
        Err(e) => {
            log_warn!("[WEBXDC] Community peer signal publish failed: {}", e);
            false
        }
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
