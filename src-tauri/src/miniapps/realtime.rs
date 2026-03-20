//! Realtime peer channels for Mini Apps using Iroh Gossip
//!
//! This module provides P2P realtime communication for WebXDC apps using Iroh's
//! gossip protocol for reliable message delivery, with an optional WebSocket
//! fast-path for zero-overhead binary sends from the Mini App JS.
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
pub use iroh_gossip::proto::TopicId;
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

/// Trailer length: 4 bytes seq + 32 bytes pubkey
const TRAILER_LEN: usize = 4 + PUBLIC_KEY_LENGTH;

// ─── SendHandle ─────────────────────────────────────────────────────────────

/// Handle for the WS fast-path: wraps the gossip sender + seq counter
/// so the WS server can send without going through Tauri invoke.
pub(crate) struct SendHandle {
    pub sender: GossipSender,
    pub seq: Arc<AtomicI32>,
    pub public_key_bytes: [u8; PUBLIC_KEY_LENGTH],
}

impl std::fmt::Debug for SendHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SendHandle").finish()
    }
}

// ─── IrohState ──────────────────────────────────────────────────────────────

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

    /// Fast-path send handles (owned by RealtimeManager, shared via Arc).
    pub(crate) send_handles: Arc<std::sync::RwLock<HashMap<String, SendHandle>>>,
}

impl IrohState {
    /// Initialize a new Iroh state with endpoint and gossip.
    /// `send_handles` is owned by RealtimeManager and passed in so the WS server
    /// can start before IrohState exists (critical for Android JNI timing).
    pub async fn new(_relay_url: Option<String>, send_handles: Arc<std::sync::RwLock<HashMap<String, SendHandle>>>) -> Result<Self> {
        log_info!("Initializing Iroh peer channels (gossip)");

        // Generate 32 random bytes and construct SecretKey from them
        // (avoids rand_core version mismatch between our rand 0.8 and iroh's rand_core 0.9)
        let mut key_bytes = [0u8; 32];
        rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut key_bytes);
        let secret_key = SecretKey::from(key_bytes);
        let public_key = secret_key.public();

        // Build a QUIC transport config tuned for realtime gaming/streaming.
        // CRITICAL: These values are hard-won from 20+ hours of debugging.
        let transport_config = iroh::endpoint::QuicTransportConfig::builder()
            .keep_alive_interval(std::time::Duration::from_secs(15))
            .max_idle_timeout(Some(std::time::Duration::from_secs(120).try_into()?))
            .stream_receive_window(VarInt::from_u32(512 * 1024))       // 512 KB per stream
            .receive_window(VarInt::from_u32(2 * 1024 * 1024))         // 2 MB aggregate
            .send_window(1_572_864)                                     // 1.5 MB burst sends
            .max_concurrent_bidi_streams(VarInt::from_u32(256))
            .max_concurrent_uni_streams(VarInt::from_u32(256))
            .initial_rtt(std::time::Duration::from_millis(100))
            // BBR congestion control — better throughput and latency than NewReno
            // under relay conditions (bufferbloat, variable RTT)
            .congestion_controller_factory(Arc::new(noq_proto::congestion::BbrConfig::default()))
            // Rule 2: Disable observed address reports (prevents QUIC learning direct IPs
            // during handshake → routes data to unreachable path → one-way data loss)
            .send_observed_address_reports(false)
            .receive_observed_address_reports(false)
            .build();

        // Rule 1: Relay-only preset — NO N0 pkarr/DNS address discovery.
        // N0 preset publishes IPs via pkarr DNS → path migration → connection death.
        use iroh::endpoint::presets::Preset;
        struct RelayOnly;
        impl Preset for RelayOnly {
            fn apply(self, builder: iroh::endpoint::Builder) -> iroh::endpoint::Builder {
                builder.relay_mode(RelayMode::Default)
            }
        }

        let endpoint = Endpoint::builder(RelayOnly)
            .secret_key(secret_key)
            .alpns(vec![GOSSIP_ALPN.to_vec()])
            .transport_config(transport_config)
            .bind()
            .await?;

        // Rule 3: Wait for relay so get_node_addr() includes relay URL in advertisements.
        // Without this, advertisements have no relay URL and peers can't connect.
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            endpoint.online(),
        ).await.ok();
        for _ in 0..20 {
            if endpoint.addr().addrs.iter().any(|a| matches!(a, TransportAddr::Relay(_))) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        log_info!("[WEBXDC] Endpoint bound, relay ready");

        // Create gossip with max message size of 128 KB
        let gossip = Gossip::builder()
            .max_message_size(MAX_MESSAGE_SIZE)
            .spawn(endpoint.clone());

        // Accept loop: handle incoming gossip connections
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

        log_info!("[WEBXDC] Accept loop started, gossip ready");

        let public_key_bytes = *public_key.as_bytes();

        Ok(Self {
            endpoint,
            gossip,
            channels: RwLock::new(HashMap::new()),
            public_key,
            public_key_bytes,
            send_handles,
        })
    }

    /// Notify the endpoint that the network has changed
    pub async fn network_change(&self) {
        self.endpoint.network_change().await
    }

    /// Get our endpoint address for peer discovery.
    /// Rule 4: Only relay URLs — direct IPs cause path migration issues.
    pub fn get_node_addr(&self) -> EndpointAddr {
        let addr = self.endpoint.addr();
        let relay_only: std::collections::BTreeSet<_> = addr.addrs.into_iter()
            .filter(|ta| matches!(ta, TransportAddr::Relay(_)))
            .collect();
        EndpointAddr { id: addr.id, addrs: relay_only }
    }

    /// Join a gossip topic and start the subscriber loop
    pub async fn join_channel(
        &self,
        topic: TopicId,
        peers: Vec<EndpointAddr>,
        event_target: Option<EventTarget>,
        app_handle: Option<AppHandle>,
        label: String,
        ws_event_targets: Option<Arc<std::sync::RwLock<HashMap<String, tokio::sync::mpsc::Sender<Vec<u8>>>>>>,
    ) -> Result<(bool, Option<oneshot::Receiver<()>>)> {
        let mut channels = self.channels.write().await;

        // If channel already exists, we're re-joining (e.g., user closed and reopened the game)
        // Update the shared event target so the subscribe loop uses the new frontend channel
        if let Some(channel_state) = channels.get(&topic) {
            log_info!("IROH_REALTIME: Re-joining existing gossip topic {:?}, updating event target", topic);
            if let Some(target) = event_target {
                let mut state = channel_state.event_target.write().unwrap_or_else(|e| { log_error!("[WEBXDC] RwLock poisoned — recovering"); e.into_inner() });
                state.set_target(target); // Flushes buffered events from preconnect phase
            }
            // Wire up WS sender for bi-directional receive (if WS is connected)
            if let Some(ref senders) = ws_event_targets {
                let map = senders.read().unwrap_or_else(|e| e.into_inner());
                if let Some(ws_tx) = map.get(&label) {
                    let mut state = channel_state.event_target.write().unwrap_or_else(|e| e.into_inner());
                    state.set_ws_sender(ws_tx.clone());
                    log_info!("[WEBXDC] RT WS bi-directional enabled for: {label}");
                }
            }
            return Ok((true, None));
        }

        let peer_ids: Vec<PublicKey> = peers.iter().map(|p| p.id).collect();

        log_info!(
            "IROH_REALTIME: Joining gossip topic {:?} with {} peers",
            topic,
            peer_ids.len()
        );

        // DON'T manually connect + handle_connection here — that creates
        // connections BEFORE the topic subscription exists, causing a race
        // where messages arrive for an unregistered topic and get lost.
        // Instead, connect AFTER subscribing, so the gossip actor has the
        // topic registered when the connection delivers messages.

        let (join_tx, join_rx) = oneshot::channel();

        let gossip_topic = self
            .gossip
            .subscribe_with_opts(topic, JoinOptions::with_bootstrap(peer_ids))
            .await?;

        // NOW connect — topic subscription is registered, safe to receive
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
        let (gossip_sender, gossip_receiver) = gossip_topic.split();

        // Create shared event target for the subscribe loop (buffers events if target is None)
        let shared_event_target: SharedEventTarget = Arc::new(std::sync::RwLock::new(EventTargetState::new(event_target)));
        let shared_target_clone = shared_event_target.clone();

        // Wire up WS sender for bi-directional receive (if WS is connected)
        if let Some(ref senders) = ws_event_targets {
            let map = senders.read().unwrap_or_else(|e| e.into_inner());
            if let Some(ws_tx) = map.get(&label) {
                let mut state = shared_event_target.write().unwrap_or_else(|e| e.into_inner());
                state.set_ws_sender(ws_tx.clone());
                log_info!("[WEBXDC] RT WS bi-directional enabled for: {label}");
            }
        }

        // Create shared peer count
        let shared_peer_count: SharedPeerCount = Arc::new(AtomicUsize::new(0));
        let peer_count_clone = shared_peer_count.clone();

        let our_key_bytes = self.public_key_bytes;
        let topic_encoded = encode_topic_id(&topic);
        let sender_for_loop = gossip_sender.clone();
        let subscribe_loop = tokio::spawn(async move {
            if let Err(e) = run_subscribe_loop(gossip_receiver, sender_for_loop, topic, shared_target_clone, join_tx, our_key_bytes, peer_count_clone, app_handle, topic_encoded).await {
                log_warn!("Subscribe loop failed: {e}");
            }
        });

        let seq = Arc::new(AtomicI32::new(0));

        channels.insert(topic, ChannelState::new(subscribe_loop, gossip_sender.clone(), shared_event_target, shared_peer_count, seq.clone()));

        // Drop channels lock before populating send_handles
        drop(channels);

        // Populate fast-path send handle for the WS server
        self.send_handles.write().unwrap_or_else(|e| { log_error!("[WEBXDC] RwLock poisoned — recovering"); e.into_inner() }).insert(label, SendHandle {
            sender: gossip_sender,
            seq,
            public_key_bytes: self.public_key_bytes,
        });

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

    /// Single attempt to add a peer (no retries)
    pub(crate) async fn try_add_peer(&self, topic: &TopicId, peer: &EndpointAddr) -> Result<()> {
        // Rule 4: Build relay-only address (strip direct IPs to prevent path migration)
        let mut peer_addr = EndpointAddr {
            id: peer.id,
            addrs: peer.addrs.iter()
                .filter(|a| matches!(a, TransportAddr::Relay(_)))
                .cloned()
                .collect(),
        };

        // Rule 5: If peer has no relay URL, inject ours (both use same N0 relay infrastructure)
        if peer_addr.addrs.is_empty() {
            let our_addr = self.endpoint.addr();
            for ta in &our_addr.addrs {
                if let TransportAddr::Relay(url) = ta {
                    log_info!("[WEBXDC] add_peer: Peer {} has no relay URL, injecting ours: {}", peer.id, url);
                    peer_addr.addrs.insert(TransportAddr::Relay(url.clone()));
                    break;
                }
            }
        }

        log_trace!("[WEBXDC] add_peer: Connecting to peer {}", peer_addr.id);

        // Connect and hand to gossip, then join_peers.
        // Topic subscription already exists (channel is in the map),
        // so the connection won't race with topic registration.
        let conn = self.endpoint.connect(peer_addr, GOSSIP_ALPN).await?;
        self.gossip.handle_connection(conn).await?;

        let channels = self.channels.read().await;
        if let Some(channel_state) = channels.get(topic) {
            channel_state.sender.join_peers(vec![peer.id]).await?;
            log_info!("[WEBXDC] add_peer: Successfully joined peer {} to topic", peer.id);
        } else {
            return Err(anyhow!("Channel not found for topic"));
        }
        Ok(())
    }

    /// Send data to a gossip topic (used by invoke fallback)
    pub async fn send_data(&self, topic: TopicId, mut data: Vec<u8>) -> Result<()> {
        let sender = {
            let channels = self.channels.read().await;
            let state = channels
                .get(&topic)
                .ok_or_else(|| anyhow!("Channel not found for topic"))?;

            // Pre-allocate for trailer: 4-byte seq + 32-byte public key
            data.reserve(TRAILER_LEN);
            let seq_num = state.seq.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
            data.extend_from_slice(&seq_num.to_le_bytes());
            data.extend_from_slice(&self.public_key_bytes);

            state.sender.clone()
        };

        sender.broadcast(data.into()).await?;
        Ok(())
    }

    /// Zero-overhead send via the fast-path cache (for WS server).
    /// Adds gossip trailer and broadcasts via spawned task.
    pub fn fast_send(&self, label: &str, data: Vec<u8>) {
        let handles = self.send_handles.read().unwrap_or_else(|e| {
            log_error!("[WEBXDC] send_handles RwLock poisoned — recovering");
            e.into_inner()
        });
        let Some(handle) = handles.get(label) else { return };

        // Add trailer: seq + pubkey
        let mut msg = data;
        msg.reserve(TRAILER_LEN);
        let seq_num = handle.seq.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
        msg.extend_from_slice(&seq_num.to_le_bytes());
        msg.extend_from_slice(&handle.public_key_bytes);

        // Fire-and-forget broadcast via a spawned task (broadcast is async)
        let sender = handle.sender.clone();
        tokio::spawn(async move {
            let _ = sender.broadcast(msg.into()).await;
        });
    }

    /// Leave a realtime channel.
    /// CRITICAL: All GossipSender/GossipReceiver clones MUST be dropped before
    /// a new subscription to the same topic can work. Gossip only cleans up a topic
    /// when ALL sender+receiver halves are dropped. If any clone survives, a subsequent
    /// subscribe_with_opts to the same topic creates a broken duplicate subscription.
    pub async fn leave_channel(&self, topic: TopicId, label: &str) -> Result<()> {
        // 1. Remove fast-path SendHandle (drops its GossipSender clone)
        self.send_handles.write().unwrap_or_else(|e| { log_error!("[WEBXDC] RwLock poisoned — recovering"); e.into_inner() }).remove(label);

        // Remove WS sender for this label (ws_senders is on RealtimeManager,
        // but we're on IrohState — caller handles this separately)

        if let Some(channel) = self.channels.write().await.remove(&topic) {
            // 2. Drop the ChannelState's sender explicitly (don't wait for implicit drop)
            drop(channel.sender);

            // 3. Abort subscribe loop (drops GossipReceiver + sender_for_loop clone)
            channel.subscribe_loop.abort();
            let _ = channel.subscribe_loop.await;

            // 4. Yield to let the gossip actor process the quit
            tokio::task::yield_now().await;

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

    /// Clear the event target for a topic (prevents log errors after window close)
    pub async fn clear_event_target(&self, topic: &TopicId) {
        let channels = self.channels.read().await;
        if let Some(channel_state) = channels.get(topic) {
            let mut state = channel_state.event_target.write().unwrap_or_else(|e| { log_error!("[WEBXDC] RwLock poisoned — recovering"); e.into_inner() });
            state.target = None;
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

/// Shared event target + message buffer. When the target is None (preconnect created
/// the channel before JS attached a listener), incoming events are buffered. When the
/// target is set (joinRealtimeChannel), the buffer is flushed so no data is lost.
pub(crate) struct EventTargetState {
    target: Option<EventTarget>,
    buffer: Vec<RealtimeEvent>,
    /// Optional WebSocket sender for bi-directional WS (bypasses JNI on Android).
    /// When set, Data events are sent directly through WS instead of the normal target.
    ws_sender: Option<tokio::sync::mpsc::Sender<Vec<u8>>>,
}

impl EventTargetState {
    fn new(target: Option<EventTarget>) -> Self {
        Self { target, buffer: Vec::new(), ws_sender: None }
    }

    /// Register a WS sender for bi-directional receive. Data events bypass
    /// the normal target (JNI on Android) and go straight through WebSocket.
    pub fn set_ws_sender(&mut self, sender: tokio::sync::mpsc::Sender<Vec<u8>>) {
        self.ws_sender = Some(sender);
    }

    pub fn clear_ws_sender(&mut self) {
        self.ws_sender = None;
    }

    /// Send an event, buffering if no target is set yet.
    /// Data events are routed through WebSocket when available (bypasses JNI on Android).
    fn send(&mut self, event: RealtimeEvent) -> bool {
        // If WS sender is available and this is a Data event, send via WS directly.
        // This bypasses the JNI/evaluateJavascript path that gets starved by WASM.
        if let Some(ref ws_tx) = self.ws_sender {
            if let RealtimeEvent::Data(ref b91_data) = event {
                // Send raw base91 string as binary WS frame
                let _ = ws_tx.try_send(b91_data.as_bytes().to_vec());
                return true;
            }
        }

        if let Some(ref target) = self.target {
            Self::deliver(target, event)
        } else {
            if self.buffer.len() < 256 {
                self.buffer.push(event);
            }
            false
        }
    }

    /// Set the target and flush all buffered events
    fn set_target(&mut self, target: EventTarget) {
        // Flush buffer
        for event in self.buffer.drain(..) {
            Self::deliver(&target, event);
        }
        self.target = Some(target);
    }

    fn deliver(target: &EventTarget, event: RealtimeEvent) -> bool {
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
    }
}

pub(crate) type SharedEventTarget = Arc<std::sync::RwLock<EventTargetState>>;

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
    seq: Arc<AtomicI32>,
}

impl std::fmt::Debug for ChannelState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChannelState")
            .field("peer_count", &self.peer_count.load(Ordering::Relaxed))
            .field("seq", &self.seq.load(Ordering::Relaxed))
            .finish()
    }
}

impl ChannelState {
    fn new(subscribe_loop: JoinHandle<()>, sender: GossipSender, event_target: SharedEventTarget, peer_count: SharedPeerCount, seq: Arc<AtomicI32>) -> Self {
        Self { subscribe_loop, sender, event_target, peer_count, seq }
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

/// Helper to send an event through the shared event target (sync — no async overhead).
/// Hot path uses read() lock (concurrent). Only upgrades to write() when buffering.
fn send_event(shared_target: &SharedEventTarget, event: RealtimeEvent) -> bool {
    // Fast path: try read lock first (concurrent, no contention)
    {
        let guard = shared_target.read().unwrap_or_else(|e| {
            log_error!("[WEBXDC] SharedEventTarget RwLock poisoned — recovering");
            e.into_inner()
        });
        if let Some(ref target) = guard.target {
            return EventTargetState::deliver(target, event);
        }
    }
    // Slow path: no target yet, take write lock to buffer
    let mut guard = shared_target.write().unwrap_or_else(|e| {
        log_error!("[WEBXDC] SharedEventTarget RwLock poisoned — recovering");
        e.into_inner()
    });
    guard.send(event)
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
    sender: GossipSender,
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

    while let Some(event) = receiver.next().await {
        match event {
            Ok(Event::Received(msg)) => {
                let content = &msg.content;
                if content.len() < TRAILER_LEN {
                    log_warn!("[WEBXDC] Dropping malformed message ({} bytes < {} trailer)", content.len(), TRAILER_LEN);
                    continue;
                }
                let payload_len = content.len() - TRAILER_LEN;
                let sender_key = &content[payload_len + 4..];
                if sender_key == our_key_bytes {
                    continue;
                }
                send_event(&shared_event_target, RealtimeEvent::Data(base91_encode(&content[..payload_len])));
            }
            Ok(Event::NeighborUp(peer_id)) => {
                // Explicitly join this peer to our topic subscription.
                // Fixes one-way data flow: when a peer connects via the Router,
                // gossip has the connection but may not associate it with our topic.
                if let Err(e) = sender.join_peers(vec![peer_id]).await {
                    log_warn!("[WEBXDC] Failed to join_peers on NeighborUp: {e}");
                }

                let new_count = peer_count.fetch_add(1, Ordering::Relaxed) + 1;
                emit_realtime_status(&app_handle, &topic_encoded, new_count, true);

                if let Some(tx) = join_tx.take() {
                    let _ = tx.send(());
                    send_event(&shared_event_target, RealtimeEvent::Connected);
                }
                let peer_str = base32_nopad_encode(peer_id.as_bytes());
                send_event(&shared_event_target, RealtimeEvent::PeerJoined(peer_str));
            }
            Ok(Event::NeighborDown(peer_id)) => {
                let peer_str = base32_nopad_encode(peer_id.as_bytes());

                let _ = peer_count.fetch_update(
                    Ordering::Relaxed, Ordering::Relaxed,
                    |count| if count > 0 { Some(count - 1) } else { None },
                );
                let new_count = peer_count.load(Ordering::Relaxed);
                emit_realtime_status(&app_handle, &topic_encoded, new_count, true);
                send_event(&shared_event_target, RealtimeEvent::PeerLeft(peer_str));

                // Auto-reconnect: re-advertise via Nostr after a short delay
                if let Some(ref app) = app_handle {
                    let app = app.clone();
                    let te = topic_encoded.clone();
                    let topic_copy = topic;
                    tokio::spawn(async move {
                        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                        let state = app.state::<crate::miniapps::state::MiniAppsState>();
                        if !state.has_realtime_channel_for_topic(&topic_copy).await { return; }
                        if let Ok(iroh) = state.realtime.get_or_init().await {
                            let node_addr = iroh.get_node_addr();
                            if let Ok(addr_encoded) = encode_node_addr(&node_addr) {
                                if let Some(chat_id) = state.get_chat_id_for_topic(&topic_copy).await {
                                    log_info!("[WEBXDC] NeighborDown: re-advertising for reconnection (topic: {})", te);
                                    crate::commands::realtime::send_webxdc_peer_advertisement(chat_id, te, addr_encoded).await;
                                }
                            }
                        }
                    });
                }
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

// ─── RealtimeManager ────────────────────────────────────────────────────────

/// Global Iroh state manager
///
/// Uses `OnceCell` for lock-free reads after initialization —
/// `get_or_init()` is a single atomic load on the hot path.
pub struct RealtimeManager {
    /// Iroh state (initialized once, then read-only via atomic load)
    iroh: tokio::sync::OnceCell<IrohState>,
    /// Custom relay URL (if any)
    relay_url: Option<String>,
    /// WebSocket server info (port + token), set once after WS server starts
    ws_info: std::sync::OnceLock<super::rt_ws::WsInfo>,
    /// Fast-path send handles — owned here so the WS server can start
    /// before IrohState exists (critical for Android JNI timing).
    send_handles: Arc<std::sync::RwLock<HashMap<String, SendHandle>>>,
    /// Map of window_label → WS sender for bi-directional receive.
    /// WS handler registers sender on connect, join_channel wires it into the event target.
    pub(crate) ws_senders: Arc<std::sync::RwLock<HashMap<String, tokio::sync::mpsc::Sender<Vec<u8>>>>>,
}

impl RealtimeManager {
    pub fn new(relay_url: Option<String>) -> Self {
        Self {
            iroh: tokio::sync::OnceCell::new(),
            relay_url,
            ws_info: std::sync::OnceLock::new(),
            send_handles: Arc::new(std::sync::RwLock::new(HashMap::new())),
            ws_senders: Arc::new(std::sync::RwLock::new(HashMap::new())),
        }
    }

    /// Get or initialize the Iroh state.
    /// After first call, this is a single atomic load (~5ns).
    /// Also starts the realtime WebSocket server on first init.
    pub async fn get_or_init(&self) -> Result<&IrohState> {
        let sh = self.send_handles.clone();
        let iroh = self.iroh
            .get_or_try_init(|| IrohState::new(self.relay_url.clone(), sh))
            .await?;

        // Start WS server if not already running
        self.ensure_ws_started();

        Ok(iroh)
    }

    /// Start the WS server if not already running.
    /// Uses std::net::TcpListener for sync bind (no async runtime needed),
    /// then spawns the accept loop on tauri::async_runtime so it survives
    /// any temporary runtime (critical for Android JNI).
    pub fn ensure_ws_started(&self) {
        if self.ws_info.get().is_some() {
            return;
        }

        // Sync bind — no runtime needed
        let std_listener = match std::net::TcpListener::bind("127.0.0.1:0") {
            Ok(l) => l,
            Err(e) => {
                log_warn!("[WEBXDC] Failed to bind RT WS server: {e}");
                return;
            }
        };
        let port = match std_listener.local_addr() {
            Ok(a) => a.port(),
            Err(e) => {
                log_warn!("[WEBXDC] Failed to get RT WS local addr: {e}");
                return;
            }
        };
        std_listener.set_nonblocking(true).ok();

        // Random 32-char hex token (128-bit security)
        let token = {
            let mut bytes = [0u8; 16];
            use rand::RngCore;
            rand::rngs::OsRng.fill_bytes(&mut bytes);
            crate::simd::hex::bytes_to_hex_16(&bytes)
        };

        log_info!("[WEBXDC] Realtime WS server listening on 127.0.0.1:{port}");

        let _ = self.ws_info.set(super::rt_ws::WsInfo { port, token: token.clone() });

        // Spawn accept loop on the MAIN Tauri runtime (survives JNI temp runtime)
        let send_handles = self.send_handles.clone();
        let ws_senders = self.ws_senders.clone();
        tauri::async_runtime::spawn(async move {
            // Convert std listener to tokio listener on the main runtime
            let listener = match tokio::net::TcpListener::from_std(std_listener) {
                Ok(l) => l,
                Err(e) => {
                    log_error!("[WEBXDC] Failed to convert RT WS listener to tokio: {e}");
                    return;
                }
            };
            super::rt_ws::run_accept_loop(listener, token, send_handles, ws_senders).await;
        });
    }

    /// Get the WebSocket URL for the realtime fast path, if the server is running.
    pub fn ws_url(&self) -> Option<String> {
        self.ws_info.get().map(|info| {
            format!("ws://127.0.0.1:{}/{}", info.port, info.token)
        })
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

// ─── Topic utilities ─────────────────────────────────────────────────────────

/// Generate a random topic ID (for testing/fallback only)
#[allow(dead_code)]
pub fn generate_topic_id() -> TopicId {
    let mut bytes = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut bytes);
    TopicId::from_bytes(bytes)
}

/// Derive a deterministic topic ID from file hash, chat context, and message ID
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

/// Check if an IP address is a LAN/private address
fn is_lan_addr(ip: &std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => v4.is_private() || v4.is_loopback() || v4.is_link_local(),
        std::net::IpAddr::V6(v6) => {
            v6.is_loopback() || {
                let seg0 = v6.segments()[0];
                (seg0 & 0xffc0) == 0xfe80 || (seg0 & 0xfe00) == 0xfc00
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
        assert!(is_lan_addr(&"192.168.1.1".parse::<IpAddr>().unwrap()));
        assert!(is_lan_addr(&"10.0.0.1".parse::<IpAddr>().unwrap()));
        assert!(is_lan_addr(&"172.16.0.1".parse::<IpAddr>().unwrap()));
        assert!(is_lan_addr(&"127.0.0.1".parse::<IpAddr>().unwrap()));
        assert!(!is_lan_addr(&"8.8.8.8".parse::<IpAddr>().unwrap()));
        assert!(!is_lan_addr(&"1.1.1.1".parse::<IpAddr>().unwrap()));
        assert!(is_lan_addr(&"::1".parse::<IpAddr>().unwrap()));
        assert!(is_lan_addr(&"fe80::1".parse::<IpAddr>().unwrap()));
        assert!(!is_lan_addr(&"2001:db8::1".parse::<IpAddr>().unwrap()));
    }
}
