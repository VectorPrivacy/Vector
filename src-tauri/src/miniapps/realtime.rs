//! Realtime peer channels for Mini Apps using Iroh
//!
//! This module provides P2P realtime communication for WebXDC apps using Iroh,
//! matching DeltaChat's implementation for cross-compatibility.
//!
//! See: https://webxdc.org/docs/spec/joinRealtimeChannel.html

#![allow(dead_code)] // API functions that will be used as the feature matures

use anyhow::{anyhow, bail, Context as _, Result};
use fast_thumbhash::base91_encode;
use futures_util::StreamExt;
use iroh::endpoint::VarInt;
use iroh::{EndpointAddr, Endpoint, PublicKey, RelayMode, SecretKey, TransportAddr};
use iroh_gossip::api::{Event, GossipReceiver, GossipSender, JoinOptions};
use iroh_gossip::net::{Gossip, GOSSIP_ALPN};
use iroh_gossip::proto::TopicId;
use std::collections::HashMap;
use std::sync::atomic::{AtomicI32, AtomicUsize, Ordering};
use std::sync::Arc;
use tauri::ipc::Channel;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::{oneshot, RwLock};
use tokio::task::JoinHandle;

/// BASE32 no-pad encoding (RFC 4648), replacing the `data-encoding` crate.
fn base32_nopad_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut out = String::with_capacity((bytes.len() * 8 + 4) / 5);
    let mut buf: u64 = 0;
    let mut bits: u32 = 0;
    for &b in bytes {
        buf = (buf << 8) | b as u64;
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(ALPHABET[((buf >> bits) & 0x1F) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(ALPHABET[((buf << (5 - bits)) & 0x1F) as usize] as char);
    }
    out
}

/// BASE32 no-pad decoding (RFC 4648), replacing the `data-encoding` crate.
fn base32_nopad_decode(encoded: &[u8]) -> std::result::Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(encoded.len() * 5 / 8);
    let mut buf: u64 = 0;
    let mut bits: u32 = 0;
    for &c in encoded {
        let val = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a',
            b'2'..=b'7' => c - b'2' + 26,
            _ => return Err(format!("Invalid base32 character: {}", c as char)),
        };
        buf = (buf << 5) | val as u64;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    Ok(out)
}

/// Maximum message size for realtime data (128 KB as per WebXDC spec)
const MAX_MESSAGE_SIZE: usize = 128 * 1024;

/// The length of an ed25519 PublicKey, in bytes.
const PUBLIC_KEY_LENGTH: usize = 32;

/// Store Iroh peer channels state
#[derive(Debug)]
pub struct IrohState {
    /// Iroh endpoint for peer channels
    pub(crate) endpoint: Endpoint,

    /// Gossip protocol handler
    pub(crate) gossip: Gossip,

    /// Active realtime channels
    pub(crate) channels: RwLock<HashMap<TopicId, ChannelState>>,

    /// Our public key (attached to messages for deduplication)
    pub(crate) public_key: PublicKey,

    /// Cached public key bytes (avoids repeated .as_bytes() calls in hot path)
    pub(crate) public_key_bytes: [u8; PUBLIC_KEY_LENGTH],
}

impl IrohState {
    /// Initialize a new Iroh state with endpoint and gossip
    pub async fn new(_relay_url: Option<String>) -> Result<Self> {
        log_info!("Initializing Iroh peer channels");
        
        // Generate 32 random bytes and construct SecretKey from them
        // (avoids rand_core version mismatch between our rand 0.8 and iroh's rand_core 0.9)
        let mut key_bytes = [0u8; 32];
        rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut key_bytes);
        let secret_key = SecretKey::from(key_bytes);
        let public_key = secret_key.public();

        // Build a QUIC transport config tuned for realtime gaming/streaming.
        // Handles extreme RTT scenarios (500ms–5000ms) for globe-spanning connections.
        let transport_config = iroh::endpoint::QuicTransportConfig::builder()
            .keep_alive_interval(std::time::Duration::from_secs(15))
            .max_idle_timeout(Some(std::time::Duration::from_secs(120).try_into()?))
            .stream_receive_window(VarInt::from_u32(512 * 1024))       // 512 KB per stream
            .receive_window(VarInt::from_u32(2 * 1024 * 1024))         // 2 MB aggregate
            .send_window(1_572_864)                                     // 1.5 MB burst sends
            .max_concurrent_bidi_streams(VarInt::from_u32(256))
            .max_concurrent_uni_streams(VarInt::from_u32(256))
            .initial_rtt(std::time::Duration::from_millis(100))
            // QUIC multipath: simultaneous WiFi + cellular + relay paths
            .max_concurrent_multipath_paths(3)
            .default_path_keep_alive_interval(std::time::Duration::from_secs(10))
            .default_path_max_idle_timeout(std::time::Duration::from_secs(120))
            // BBR congestion control for high BDP intercontinental links
            .congestion_controller_factory(Arc::new(
                iroh_quinn_proto::congestion::BbrConfig::default()
            ))
            .build();

        // Build the endpoint with tuned transport
        let endpoint = Endpoint::builder()
            .secret_key(secret_key)
            .alpns(vec![GOSSIP_ALPN.to_vec()])
            .relay_mode(RelayMode::Default)
            .transport_config(transport_config)
            .bind()
            .await?;

        // Relay connects automatically in the background (no manual wait needed)
        log_info!("[WEBXDC] Endpoint bound, relay connection will establish in background");

        // Create gossip with max message size of 128 KB
        let gossip = Gossip::builder()
            .max_message_size(MAX_MESSAGE_SIZE)
            .spawn(endpoint.clone());

        // Start the accept loop to handle incoming connections
        // The gossip protocol doesn't accept connections itself - we need to do it
        let accept_endpoint = endpoint.clone();
        let accept_gossip = gossip.clone();
        tokio::spawn(async move {
            log_info!("[WEBXDC] Starting connection accept loop");
            loop {
                match accept_endpoint.accept().await {
                    Some(incoming) => {
                        let gossip = accept_gossip.clone();
                        tokio::spawn(async move {
                            match incoming.await {
                                Ok(conn) => {
                                    if conn.alpn() == GOSSIP_ALPN {
                                        if let Err(e) = gossip.handle_connection(conn).await {
                                            log_error!("[WEBXDC] Failed to handle gossip connection: {}", e);
                                        }
                                    }
                                }
                                Err(e) => {
                                    log_error!("[WEBXDC] Failed to accept incoming connection: {}", e);
                                }
                            }
                        });
                    }
                    None => {
                        log_info!("[WEBXDC] Accept loop ended - endpoint closed");
                        break;
                    }
                }
            }
        });

        let public_key_bytes = *public_key.as_bytes();

        Ok(Self {
            endpoint,
            gossip,
            channels: RwLock::new(HashMap::new()),
            public_key,
            public_key_bytes,
        })
    }

    /// Notify the endpoint that the network has changed
    pub async fn network_change(&self) {
        self.endpoint.network_change().await
    }

    /// Get our endpoint address for peer discovery.
    /// Includes LAN/private addresses for direct same-network P2P (~1ms latency)
    /// but strips public IPs to preserve privacy. Remote peers fall back to relay.
    pub fn get_node_addr(&self) -> EndpointAddr {
        let addr = self.endpoint.addr();
        // Filter: keep relay URLs + only LAN IPs (strip public IPs for privacy)
        let filtered_addrs = addr.addrs.into_iter()
            .filter(|ta| match ta {
                TransportAddr::Relay(_) => true,
                TransportAddr::Ip(sa) => is_lan_addr(&sa.ip()),
                _ => false, // Unknown transport types — strip for safety
            })
            .collect();
        EndpointAddr { id: addr.id, addrs: filtered_addrs }
    }

    /// Join a gossip topic and start the subscriber loop
    pub async fn join_channel(
        &self,
        topic: TopicId,
        peers: Vec<EndpointAddr>,
        event_target: EventTarget,
        app_handle: Option<AppHandle>,
    ) -> Result<(bool, Option<oneshot::Receiver<()>>)> {
        let mut channels = self.channels.write().await;

        // If channel already exists, we're re-joining (e.g., user closed and reopened the game)
        // Update the shared event target so the subscribe loop uses the new frontend channel
        if let Some(channel_state) = channels.get(&topic) {
            log_info!("IROH_REALTIME: Re-joining existing gossip topic {:?}, updating event target", topic);
            let mut shared_target = channel_state.event_target.write().unwrap_or_else(|e| e.into_inner());
            *shared_target = Some(event_target);
            return Ok((true, None));
        }

        let peer_ids: Vec<PublicKey> = peers.iter().map(|p| p.id).collect();

        log_info!(
            "IROH_REALTIME: Joining gossip topic {:?} with {} peers",
            topic,
            peer_ids.len()
        );

        // Connect to peers so gossip can discover them
        for peer_addr in &peers {
            if !peer_addr.addrs.is_empty() {
                let addr = peer_addr.clone();
                let ep = self.endpoint.clone();
                let g = self.gossip.clone();
                tokio::spawn(async move {
                    match ep.connect(addr, GOSSIP_ALPN).await {
                        Ok(conn) => {
                            if let Err(e) = g.handle_connection(conn).await {
                                log_warn!("[WEBXDC] Failed to handle peer connection: {e}");
                            }
                        }
                        Err(e) => log_warn!("[WEBXDC] Failed to connect to peer: {e}"),
                    }
                });
            }
        }

        let (join_tx, join_rx) = oneshot::channel();

        let gossip_topic = self
            .gossip
            .subscribe_with_opts(topic, JoinOptions::with_bootstrap(peer_ids))
            .await?;
        let (gossip_sender, gossip_receiver) = gossip_topic.split();

        // Create shared event target for the subscribe loop
        let shared_event_target: SharedEventTarget = Arc::new(std::sync::RwLock::new(Some(event_target)));
        let shared_target_clone = shared_event_target.clone();
        
        // Create shared peer count
        let shared_peer_count: SharedPeerCount = Arc::new(AtomicUsize::new(0));
        let peer_count_clone = shared_peer_count.clone();

        let our_key_bytes = self.public_key_bytes;
        let topic_encoded = encode_topic_id(&topic);
        let subscribe_loop = tokio::spawn(async move {
            if let Err(e) = run_subscribe_loop(gossip_receiver, topic, shared_target_clone, join_tx, our_key_bytes, peer_count_clone, app_handle, topic_encoded).await {
                log_warn!("Subscribe loop failed: {e}");
            }
        });

        channels.insert(topic, ChannelState::new(subscribe_loop, gossip_sender, shared_event_target, shared_peer_count));

        Ok((false, Some(join_rx)))
    }

    /// Add a peer to an existing channel
    pub async fn add_peer(&self, topic: TopicId, peer: EndpointAddr) -> Result<()> {
        self.add_peer_with_retry(topic, peer, 3).await
    }

    /// Add a peer to a gossip topic with retry logic
    /// Retries with exponential backoff: 1s, 2s, 4s
    async fn add_peer_with_retry(&self, topic: TopicId, peer: EndpointAddr, max_retries: u32) -> Result<()> {
        let mut last_error = None;

        for attempt in 0..max_retries {
            if attempt > 0 {
                // Exponential backoff: 1s, 2s, 4s...
                let delay = std::time::Duration::from_secs(1 << (attempt - 1));
                log_info!("[WEBXDC] add_peer: Retry {} for peer {} after {:?}", attempt, peer.id, delay);
                tokio::time::sleep(delay).await;
            }

            match self.try_add_peer(&topic, &peer).await {
                Ok(()) => return Ok(()),
                Err(e) => {
                    log_warn!("[WEBXDC] add_peer: Attempt {} failed for peer {}: {}", attempt + 1, peer.id, e);
                    last_error = Some(e);
                }
            }
        }

        Err(last_error.unwrap_or_else(|| anyhow!("Failed to add peer after {} retries", max_retries)))
    }

    /// Single attempt to add a peer (internal helper)
    async fn try_add_peer(&self, topic: &TopicId, peer: &EndpointAddr) -> Result<()> {
        log_trace!("[WEBXDC] add_peer: Connecting to peer {}, addrs={}",
            peer.id,
            peer.addrs.len());

        // Connect to the peer and hand the connection to gossip
        let conn = self.endpoint.connect(peer.clone(), GOSSIP_ALPN).await?;
        self.gossip.handle_connection(conn).await?;

        // Verify the connection via remote_info
        if let Some(_info) = self.endpoint.remote_info(peer.id).await {
            log_trace!("[WEBXDC] add_peer: Remote info confirmed for peer {}", peer.id);
        } else {
            log_trace!("[WEBXDC] add_peer: WARNING - Could not get remote info for peer {}", peer.id);
        }

        // Then, use the existing channel's sender to join the peer
        let channels = self.channels.read().await;
        if let Some(channel_state) = channels.get(topic) {
            log_trace!("[WEBXDC] add_peer: Joining peer {} via existing channel sender", peer.id);
            channel_state.sender.join_peers(vec![peer.id]).await?;
            log_info!("[WEBXDC] add_peer: Successfully joined peer {} to topic", peer.id);
        } else {
            return Err(anyhow!("Channel not found for topic"));
        }
        Ok(())
    }

    /// Send data to a gossip topic
    pub async fn send_data(&self, topic: TopicId, mut data: Vec<u8>) -> Result<()> {
        // Clone sender and read seq under the lock, then release before broadcast.
        // This prevents holding the read lock during potentially slow network I/O
        // (backpressure under 4K video streaming, gaming, voice/video loads).
        let sender = {
            let channels = self.channels.read().await;
            let state = channels
                .get(&topic)
                .ok_or_else(|| anyhow!("Channel not found for topic"))?;

            // Pre-allocate for trailer: 4-byte seq + 32-byte public key
            data.reserve(4 + PUBLIC_KEY_LENGTH);
            let seq_num = state.seq.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
            data.extend_from_slice(&seq_num.to_le_bytes());
            data.extend_from_slice(&self.public_key_bytes);

            state.sender.clone()
        };

        sender.broadcast(data.into()).await?;

        log_trace!("Sent realtime data to topic {:?}", topic);

        Ok(())
    }

    /// Leave a realtime channel
    pub async fn leave_channel(&self, topic: TopicId) -> Result<()> {
        if let Some(channel) = self.channels.write().await.remove(&topic) {
            // Abort the subscribe loop (this drops the receiver)
            channel.subscribe_loop.abort();
            let _ = channel.subscribe_loop.await;
            log_info!("Left realtime channel {:?}", topic);
        }
        Ok(())
    }

    /// Get the current peer count for a topic
    pub async fn get_peer_count(&self, topic: &TopicId) -> usize {
        let channels = self.channels.read().await;
        if let Some(channel_state) = channels.get(topic) {
            channel_state.peer_count.load(Ordering::Relaxed)
        } else {
            0
        }
    }

    /// Clear the event target for a topic (e.g., when the window is destroyed but
    /// the Iroh channel is intentionally kept alive for peer count tracking).
    /// Prevents the subscribe loop from logging errors on every received message.
    pub async fn clear_event_target(&self, topic: &TopicId) {
        let channels = self.channels.read().await;
        if let Some(channel_state) = channels.get(topic) {
            let mut target = channel_state.event_target.write().unwrap_or_else(|e| e.into_inner());
            *target = None;
        }
    }

    /// Check if a channel exists for a topic
    pub async fn has_channel(&self, topic: &TopicId) -> bool {
        self.channels.read().await.contains_key(topic)
    }

}

/// Target for delivering realtime events (abstracts desktop vs Android)
#[derive(Clone)]
pub enum EventTarget {
    /// Desktop: Tauri IPC channel
    TauriChannel(Channel<RealtimeEvent>),
    /// Android: bounded mpsc sender (delivery task forwards to WebView via JNI)
    MpscSender(tokio::sync::mpsc::Sender<RealtimeEvent>),
}

/// Shared event target that can be updated when a user re-joins.
/// Uses std::sync::RwLock (not tokio) because the lock is held for <1μs
/// (just dispatching one event) and this avoids async runtime overhead
/// on every received message in the subscribe loop hot path.
pub(crate) type SharedEventTarget = Arc<std::sync::RwLock<Option<EventTarget>>>;

/// Shared peer count that can be updated by the subscribe loop
pub(crate) type SharedPeerCount = Arc<AtomicUsize>;

/// State for a single gossip channel
pub(crate) struct ChannelState {
    /// Handle to the subscribe loop task
    subscribe_loop: JoinHandle<()>,
    /// Sender for broadcasting messages
    sender: GossipSender,
    /// Shared event target (can be updated on re-join)
    event_target: SharedEventTarget,
    /// Current number of connected peers
    peer_count: SharedPeerCount,
    /// Sequence number for deduplication (lock-free)
    seq: AtomicI32,
}

impl std::fmt::Debug for ChannelState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChannelState")
            .field("subscribe_loop", &"JoinHandle<()>")
            .field("sender", &"GossipSender")
            .field("event_target", &"SharedEventTarget")
            .field("peer_count", &self.peer_count.load(Ordering::Relaxed))
            .field("seq", &self.seq.load(Ordering::Relaxed))
            .finish()
    }
}

impl ChannelState {
    fn new(subscribe_loop: JoinHandle<()>, sender: GossipSender, event_target: SharedEventTarget, peer_count: SharedPeerCount) -> Self {
        Self {
            subscribe_loop,
            sender,
            event_target,
            peer_count,
            seq: AtomicI32::new(0),
        }
    }
}

/// Events sent to the frontend via Tauri channel
#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase", tag = "event", content = "data")]
pub enum RealtimeEvent {
    /// Received data from a peer (base91-encoded for minimal IPC overhead)
    Data(String),
    /// Channel became operational (connected to peers)
    Connected,
    /// A peer joined the channel
    PeerJoined(String),
    /// A peer left the channel
    PeerLeft(String),
    /// Gossip stream lagged — some messages were lost (app should request resync)
    Lagged,
}

/// Helper to send an event through the shared event target (sync — no async overhead)
fn send_event(shared_target: &SharedEventTarget, event: RealtimeEvent) -> bool {
    let guard = shared_target.read().unwrap_or_else(|e| e.into_inner());
    if let Some(ref target) = *guard {
        match target {
            EventTarget::TauriChannel(channel) => {
                if let Err(e) = channel.send(event) {
                    log_error!("[WEBXDC] Failed to send event to frontend: {e}");
                    return false;
                }
            }
            EventTarget::MpscSender(sender) => {
                use tokio::sync::mpsc::error::TrySendError;
                match sender.try_send(event) {
                    Ok(()) => {}
                    Err(TrySendError::Full(_)) => {
                        log_warn!("[WEBXDC] Event delivery backpressure, dropping message");
                    }
                    Err(TrySendError::Closed(_)) => {
                        log_error!("[WEBXDC] Event delivery channel closed");
                        return false;
                    }
                }
            }
        }
        true
    } else {
        false
    }
}

/// Emit realtime status update to the main window
fn emit_realtime_status(app_handle: &Option<AppHandle>, topic_encoded: &str, peer_count: usize, is_active: bool) {
    if let Some(app) = app_handle {
        if let Some(main_window) = app.get_webview_window("main") {
            let _ = main_window.emit("miniapp_realtime_status", serde_json::json!({
                "topic": topic_encoded,
                "peer_count": peer_count,
                "is_active": is_active,
            }));
        }
    }
}

/// Run the subscribe loop for a gossip topic
async fn run_subscribe_loop(
    mut receiver: GossipReceiver,
    topic: TopicId,
    shared_event_target: SharedEventTarget,
    join_tx: oneshot::Sender<()>,
    our_key_bytes: [u8; PUBLIC_KEY_LENGTH],
    peer_count: SharedPeerCount,
    app_handle: Option<AppHandle>,
    topic_encoded: String,
) -> Result<()> {
    let mut join_tx = Some(join_tx);
    log_info!("[WEBXDC] Subscribe loop started for topic {:?}", topic);

    const TRAILER_LEN: usize = 4 + PUBLIC_KEY_LENGTH; // seq(4) + pubkey(32)

    while let Some(event) = receiver.next().await {
        match event {
            Ok(Event::Received(msg)) => {
                let content = &msg.content;

                // Extract trailer (seq + pubkey) via zero-copy slicing
                if content.len() >= TRAILER_LEN {
                    let payload_len = content.len() - TRAILER_LEN;
                    let sender_key = &content[payload_len + 4..];

                    // Skip messages from ourselves (32-byte memcmp, no PublicKey construction)
                    if sender_key == our_key_bytes {
                        continue;
                    }

                    // Only encode the payload portion (excludes 36-byte trailer)
                    send_event(&shared_event_target, RealtimeEvent::Data(base91_encode(&content[..payload_len])));
                } else {
                    // Malformed message (no trailer) — forward as-is
                    send_event(&shared_event_target, RealtimeEvent::Data(base91_encode(content)));
                }
            }
            Ok(Event::NeighborUp(peer_id)) => {
                // Increment peer count
                let new_count = peer_count.fetch_add(1, Ordering::Relaxed) + 1;

                // Emit status update to main window
                emit_realtime_status(&app_handle, &topic_encoded, new_count, true);

                // Signal that we're connected when first neighbor comes up
                if let Some(tx) = join_tx.take() {
                    let _ = tx.send(());
                    send_event(&shared_event_target, RealtimeEvent::Connected);
                }
                let peer_str = base32_nopad_encode(peer_id.as_bytes());
                send_event(&shared_event_target, RealtimeEvent::PeerJoined(peer_str));
            }
            Ok(Event::NeighborDown(peer_id)) => {
                let peer_str = base32_nopad_encode(peer_id.as_bytes());

                // Atomically decrement peer count (saturating to avoid underflow)
                let _ = peer_count.fetch_update(
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                    |count| if count > 0 { Some(count - 1) } else { None },
                );
                let new_count = peer_count.load(Ordering::Relaxed);

                // Emit status update to main window
                emit_realtime_status(&app_handle, &topic_encoded, new_count, true);

                send_event(&shared_event_target, RealtimeEvent::PeerLeft(peer_str));
            }
            Ok(Event::Lagged) => {
                log_warn!("[WEBXDC] Gossip lagged for topic {:?}", topic);
                send_event(&shared_event_target, RealtimeEvent::Lagged);
            }
            Err(e) => {
                log_error!("[WEBXDC] Gossip error for topic {:?}: {e}", topic);
            }
        }
    }

    log_info!("[WEBXDC] Subscribe loop ended for topic {:?}", topic);
    Ok(())
}

/// Global Iroh state manager
///
/// Uses `OnceCell` for lock-free reads after initialization —
/// `get_or_init()` is a single atomic load on the hot path.
pub struct RealtimeManager {
    /// Iroh state (initialized once, then read-only via atomic load)
    iroh: tokio::sync::OnceCell<IrohState>,
    /// Custom relay URL (if any)
    relay_url: Option<String>,
}

impl RealtimeManager {
    pub fn new(relay_url: Option<String>) -> Self {
        Self {
            iroh: tokio::sync::OnceCell::new(),
            relay_url,
        }
    }

    /// Get or initialize the Iroh state.
    /// After first call, this is a single atomic load (~5ns).
    pub async fn get_or_init(&self) -> Result<&IrohState> {
        self.iroh
            .get_or_try_init(|| IrohState::new(self.relay_url.clone()))
            .await
    }

    /// Shutdown Iroh if initialized
    pub async fn shutdown(&self) -> Result<()> {
        if let Some(iroh) = self.iroh.get() {
            iroh.endpoint.close().await;
        }
        Ok(())
    }
}

impl Default for RealtimeManager {
    fn default() -> Self {
        Self::new(None)
    }
}

/// Generate a new random topic ID for a Mini App
/// Generate a random topic ID (for testing/fallback only)
#[allow(dead_code)]
pub fn generate_topic_id() -> TopicId {
    let mut bytes = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut bytes);
    TopicId::from_bytes(bytes)
}

/// Derive a deterministic topic ID from file hash, chat context, and message ID
/// This ensures all participants viewing the same Mini App message
/// will derive the same topic ID without needing to transmit it.
/// Including message_id ensures reposts create isolated instances.
pub fn derive_topic_id(file_hash: &str, chat_id: &str, message_id: &str) -> TopicId {
    use sha2::{Sha256, Digest};
    
    let mut hasher = Sha256::new();
    hasher.update(b"webxdc-realtime-v1:");
    hasher.update(file_hash.as_bytes());
    hasher.update(b":");
    hasher.update(chat_id.as_bytes());
    hasher.update(b":");
    hasher.update(message_id.as_bytes());
    
    let result = hasher.finalize();
    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(&result);
    TopicId::from_bytes(bytes)
}

/// Encode a topic ID to a string for storage/transmission
pub fn encode_topic_id(topic: &TopicId) -> String {
    base32_nopad_encode(topic.as_bytes())
}

/// Decode a topic ID from a string
pub fn decode_topic_id(s: &str) -> Result<TopicId> {
    let bytes = base32_nopad_decode(s.as_bytes())
        .map_err(|e| anyhow!(e))
        .context("Invalid topic ID encoding")?;
    if bytes.len() != 32 {
        bail!("Invalid topic ID length");
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    Ok(TopicId::from_bytes(arr))
}

/// Encode an endpoint address to a string for transmission via Nostr
pub fn encode_node_addr(addr: &EndpointAddr) -> Result<String> {
    let json = serde_json::to_string(addr)?;
    Ok(base32_nopad_encode(json.as_bytes()))
}

/// Decode an endpoint address from a string received via Nostr
pub fn decode_node_addr(s: &str) -> Result<EndpointAddr> {
    let bytes = base32_nopad_decode(s.as_bytes())
        .map_err(|e| anyhow!(e))
        .context("Invalid node address encoding")?;
    let json = String::from_utf8(bytes)?;
    let addr: EndpointAddr = serde_json::from_str(&json)?;
    Ok(addr)
}

/// Check if an IP address is a LAN/private address (safe to share without leaking public IP).
///
/// Includes:
/// - IPv4: 10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16 (RFC 1918 private)
/// - IPv4: 169.254.0.0/16 (link-local), 127.0.0.0/8 (loopback)
/// - IPv6: ::1 (loopback), fe80::/10 (link-local), fc00::/7 (ULA)
fn is_lan_addr(ip: &std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => v4.is_private() || v4.is_loopback() || v4.is_link_local(),
        std::net::IpAddr::V6(v6) => {
            v6.is_loopback() || {
                let seg0 = v6.segments()[0];
                // fe80::/10 — link-local
                (seg0 & 0xffc0) == 0xfe80 ||
                // fc00::/7 — unique local address (ULA)
                (seg0 & 0xfe00) == 0xfc00
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_topic_id_encoding() {
        let topic = generate_topic_id();
        let encoded = encode_topic_id(&topic);
        let decoded = decode_topic_id(&encoded).unwrap();
        assert_eq!(topic, decoded);
    }

    #[test]
    fn test_is_lan_addr() {
        use std::net::IpAddr;

        // IPv4 private ranges — should pass
        assert!(is_lan_addr(&"192.168.1.1".parse::<IpAddr>().unwrap()));
        assert!(is_lan_addr(&"10.0.0.1".parse::<IpAddr>().unwrap()));
        assert!(is_lan_addr(&"172.16.0.1".parse::<IpAddr>().unwrap()));
        assert!(is_lan_addr(&"172.31.255.255".parse::<IpAddr>().unwrap()));
        assert!(is_lan_addr(&"127.0.0.1".parse::<IpAddr>().unwrap()));
        assert!(is_lan_addr(&"169.254.1.1".parse::<IpAddr>().unwrap()));

        // IPv4 public — should fail
        assert!(!is_lan_addr(&"8.8.8.8".parse::<IpAddr>().unwrap()));
        assert!(!is_lan_addr(&"1.1.1.1".parse::<IpAddr>().unwrap()));
        assert!(!is_lan_addr(&"203.0.113.1".parse::<IpAddr>().unwrap()));

        // IPv6 private — should pass
        assert!(is_lan_addr(&"::1".parse::<IpAddr>().unwrap()));
        assert!(is_lan_addr(&"fe80::1".parse::<IpAddr>().unwrap()));
        assert!(is_lan_addr(&"fd12:3456:789a::1".parse::<IpAddr>().unwrap()));

        // IPv6 public — should fail
        assert!(!is_lan_addr(&"2001:db8::1".parse::<IpAddr>().unwrap()));
        assert!(!is_lan_addr(&"2607:f8b0:4004:800::200e".parse::<IpAddr>().unwrap()));
    }
}