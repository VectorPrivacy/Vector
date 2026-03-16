//! Realtime peer channels for Mini Apps using raw QUIC connections via Iroh.
//!
//! Each peer gets independent QUIC connections with per-peer send queues —
//! one slow peer cannot block others (no head-of-line blocking).
//!
//! See: https://webxdc.org/docs/spec/joinRealtimeChannel.html

#![allow(dead_code)] // API functions that will be used as the feature matures

use anyhow::{anyhow, bail, Context as _, Result};
use fast_thumbhash::base91_encode;
use iroh::endpoint::VarInt;
use iroh::{EndpointAddr, Endpoint, PublicKey, RelayMode, SecretKey, TransportAddr};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
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

// ─── Constants ──────────────────────────────────────────────────────────────

/// Custom ALPN protocol for Vector realtime P2P channels.
const VECTOR_RT_ALPN: &[u8] = b"vector-rt/1";

/// Maximum message size for realtime data (128 KB as per WebXDC spec)
const MAX_MESSAGE_SIZE: usize = 128 * 1024;

/// Bounded global send queue capacity — large enough for bursts, bounded to cap memory.
/// At 128 KB max payload, worst case is 256 * 128 KB = 32 MB buffered.
const SEND_QUEUE_CAPACITY: usize = 256;

/// Per-peer send queue capacity. Smaller than the global queue because
/// a single slow peer should not buffer unboundedly.
/// At 128 KB max, worst case is 64 * 128 KB = 8 MB per peer.
const PEER_SEND_QUEUE_CAPACITY: usize = 64;

/// The length of an ed25519 PublicKey, in bytes.
pub(crate) const PUBLIC_KEY_LENGTH: usize = 32;

// ─── TopicId ────────────────────────────────────────────────────────────────

/// Topic identifier for realtime channels (32-byte hash).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TopicId([u8; 32]);

impl TopicId {
    pub fn from_bytes(bytes: [u8; 32]) -> Self { Self(bytes) }
    pub fn as_bytes(&self) -> &[u8; 32] { &self.0 }
}

// ─── SendHandle ─────────────────────────────────────────────────────────────

/// Sync-accessible handle for the zero-overhead send fast path.
/// Cached per window label so the WS server can skip all async lookups.
pub(crate) struct SendHandle {
    pub send_tx: tokio::sync::mpsc::Sender<Vec<u8>>,
    pub drops: Arc<AtomicU64>,
}

impl std::fmt::Debug for SendHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SendHandle")
            .field("drops", &self.drops.load(Ordering::Relaxed))
            .finish()
    }
}

// ─── PeerConnection ─────────────────────────────────────────────────────────

/// A single peer's QUIC connection and associated tasks.
struct PeerConnection {
    /// The QUIC connection to this peer.
    conn: iroh::endpoint::Connection,
    /// Background task reading incoming uni streams from this peer.
    read_task: JoinHandle<()>,
    /// Background task writing to uni streams for this peer.
    write_task: JoinHandle<()>,
    /// Per-peer send queue (drainer fans out here; non-blocking try_send).
    send_tx: tokio::sync::mpsc::Sender<Vec<u8>>,
}

// ─── IrohState ──────────────────────────────────────────────────────────────

/// Store Iroh peer channels state
pub struct IrohState {
    /// Iroh QUIC endpoint for P2P connections
    pub(crate) endpoint: Endpoint,

    /// Active realtime channels (Arc for sharing with accept loop, drainer, and peer tasks)
    pub(crate) channels: Arc<RwLock<HashMap<TopicId, ChannelState>>>,

    /// Our public key (attached to messages for deduplication)
    pub(crate) public_key: PublicKey,

    /// Cached public key bytes (avoids repeated .as_bytes() calls in hot path)
    pub(crate) public_key_bytes: [u8; PUBLIC_KEY_LENGTH],

    /// Fast-path send handles keyed by window label (sync access via std RwLock).
    /// Wrapped in Arc so the WS server can share the same map.
    /// Populated on join_channel, removed on leave_channel.
    pub(crate) send_handles: Arc<std::sync::RwLock<HashMap<String, SendHandle>>>,
}

impl std::fmt::Debug for IrohState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IrohState")
            .field("public_key", &self.public_key)
            .finish()
    }
}

impl IrohState {
    /// Initialize a new Iroh state with QUIC endpoint (no gossip)
    pub async fn new(_relay_url: Option<String>) -> Result<Self> {
        log_info!("Initializing Iroh peer channels (raw QUIC)");

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

        // Build the endpoint with our custom ALPN
        let endpoint = Endpoint::builder()
            .secret_key(secret_key)
            .alpns(vec![VECTOR_RT_ALPN.to_vec()])
            .relay_mode(RelayMode::Default)
            .transport_config(transport_config)
            .bind()
            .await?;

        // Relay connects automatically in the background (no manual wait needed)
        log_info!("[WEBXDC] Endpoint bound, relay connection will establish in background");

        let public_key_bytes = *public_key.as_bytes();
        let channels: Arc<RwLock<HashMap<TopicId, ChannelState>>> = Arc::new(RwLock::new(HashMap::new()));

        // Start the accept loop to handle incoming peer connections
        let accept_endpoint = endpoint.clone();
        let accept_channels = channels.clone();
        let accept_our_key = public_key_bytes;
        tokio::spawn(async move {
            log_info!("[WEBXDC] Starting connection accept loop");
            loop {
                match accept_endpoint.accept().await {
                    Some(incoming) => {
                        let channels = accept_channels.clone();
                        tokio::spawn(async move {
                            match incoming.await {
                                Ok(conn) => {
                                    if conn.alpn() != VECTOR_RT_ALPN {
                                        return;
                                    }
                                    if let Err(e) = handle_incoming_peer(conn, channels, accept_our_key).await {
                                        log_warn!("[WEBXDC] Failed to handle incoming peer: {e}");
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

        Ok(Self {
            endpoint,
            channels,
            public_key,
            public_key_bytes,
            send_handles: Arc::new(std::sync::RwLock::new(HashMap::new())),
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

    /// Join a realtime channel and start per-peer tasks.
    /// `label` is the window label (e.g. "miniapp:abc123") used for fast-path send caching.
    pub async fn join_channel(
        &self,
        topic: TopicId,
        peers: Vec<EndpointAddr>,
        event_target: EventTarget,
        app_handle: Option<AppHandle>,
        label: String,
    ) -> Result<(bool, Option<oneshot::Receiver<()>>)> {
        let mut channels = self.channels.write().await;

        // If channel already exists, we're re-joining (e.g., user closed and reopened the game)
        // Update the shared event target so the reader tasks use the new frontend channel
        if let Some(channel_state) = channels.get(&topic) {
            log_info!("IROH_REALTIME: Re-joining existing topic {:?}, updating event target", topic);
            let mut shared_target = channel_state.event_target.write().unwrap_or_else(|e| e.into_inner());
            *shared_target = Some(event_target);
            return Ok((true, None));
        }

        log_info!(
            "IROH_REALTIME: Joining topic {:?} with {} peers",
            topic,
            peers.len()
        );

        // Create shared state
        let shared_event_target: SharedEventTarget = Arc::new(std::sync::RwLock::new(Some(event_target)));
        let shared_peer_count: SharedPeerCount = Arc::new(AtomicUsize::new(0));

        // Bounded send queue: JS invoke returns instantly, drainer fans out in background
        let (send_tx, send_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(SEND_QUEUE_CAPACITY);
        let drops = Arc::new(AtomicU64::new(0));

        // Spawn the fan-out drainer task
        let drainer_channels = self.channels.clone();
        let drainer_topic = topic;
        let drainer_drops = drops.clone();
        let drainer_handle = tokio::spawn(async move {
            run_send_drainer(send_rx, drainer_channels, drainer_topic, drainer_drops).await;
        });

        channels.insert(topic, ChannelState {
            peers: HashMap::new(),
            drainer_handle,
            send_tx: send_tx.clone(),
            event_target: shared_event_target.clone(),
            peer_count: shared_peer_count.clone(),
            drops: drops.clone(),
        });

        // Drop channels lock before connecting to peers (connections may take time)
        drop(channels);

        // Populate fast-path send handle for the WS server (zero-overhead sends)
        self.send_handles.write().unwrap_or_else(|e| e.into_inner()).insert(label, SendHandle {
            send_tx,
            drops,
        });

        // Connect to initial peers concurrently
        let (join_tx, join_rx) = oneshot::channel();
        let join_tx = Arc::new(std::sync::Mutex::new(Some(join_tx)));

        for peer_addr in peers {
            if !peer_addr.addrs.is_empty() {
                let ep = self.endpoint.clone();
                let channels_ref = self.channels.clone();
                let et = shared_event_target.clone();
                let pc = shared_peer_count.clone();
                let our_key = self.public_key_bytes;
                let join_tx_clone = join_tx.clone();
                let app_handle_clone = app_handle.clone();
                let topic_encoded = encode_topic_id(&topic);

                tokio::spawn(async move {
                    match connect_to_peer(&ep, peer_addr, topic, et.clone(), pc.clone(), our_key, channels_ref).await {
                        Ok(_) => {
                            // Signal connected on first peer
                            if let Ok(mut guard) = join_tx_clone.lock() {
                                if let Some(tx) = guard.take() {
                                    let _ = tx.send(());
                                    send_event(&et, RealtimeEvent::Connected);
                                }
                            }
                            let new_count = pc.load(Ordering::Relaxed);
                            emit_realtime_status(&app_handle_clone, &topic_encoded, new_count, true);
                        }
                        Err(e) => log_warn!("[WEBXDC] Failed to connect to peer: {e}"),
                    }
                });
            }
        }

        Ok((false, Some(join_rx)))
    }

    /// Add a peer to an existing channel
    pub async fn add_peer(&self, topic: TopicId, peer: EndpointAddr) -> Result<()> {
        self.add_peer_with_retry(topic, peer, 3).await
    }

    /// Add a peer to a topic with retry logic
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

        let (event_target, peer_count) = {
            let channels = self.channels.read().await;
            let channel = channels.get(topic)
                .ok_or_else(|| anyhow!("Channel not found for topic"))?;
            (channel.event_target.clone(), channel.peer_count.clone())
        };

        connect_to_peer(
            &self.endpoint, peer.clone(), *topic,
            event_target, peer_count,
            self.public_key_bytes, self.channels.clone(),
        ).await?;

        log_info!("[WEBXDC] add_peer: Successfully connected peer {} to topic", peer.id);
        Ok(())
    }

    /// Zero-overhead send via the fast-path cache (entirely synchronous).
    /// Called from the WS server and invoke fallback — no async, no JSON, no encoding.
    pub fn fast_send(&self, label: &str, data: Vec<u8>) {
        let handles = self.send_handles.read().unwrap_or_else(|e| e.into_inner());
        let Some(handle) = handles.get(label) else { return };

        // Non-blocking enqueue — drops packet on overload
        if let Err(tokio::sync::mpsc::error::TrySendError::Full(_)) = handle.send_tx.try_send(data) {
            handle.drops.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Leave a realtime channel
    pub async fn leave_channel(&self, topic: TopicId, label: &str) -> Result<()> {
        // Remove fast-path send handle
        self.send_handles.write().unwrap_or_else(|e| e.into_inner()).remove(label);

        if let Some(channel) = self.channels.write().await.remove(&topic) {
            // Abort drainer
            channel.drainer_handle.abort();
            let _ = channel.drainer_handle.await;

            // Close all peer connections and abort their tasks
            for (_key, peer) in channel.peers {
                peer.conn.close(0u32.into(), b"leaving");
                peer.read_task.abort();
                peer.write_task.abort();
            }

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
    /// Prevents the reader tasks from logging errors on every received message.
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

// ─── EventTarget / RealtimeEvent ────────────────────────────────────────────

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
/// on every received message in the reader hot path.
pub(crate) type SharedEventTarget = Arc<std::sync::RwLock<Option<EventTarget>>>;

/// Shared peer count that can be updated by reader tasks
pub(crate) type SharedPeerCount = Arc<AtomicUsize>;

// ─── ChannelState ───────────────────────────────────────────────────────────

/// State for a single realtime channel (one per TopicId)
pub(crate) struct ChannelState {
    /// Per-peer connections, keyed by remote PublicKey
    peers: HashMap<PublicKey, PeerConnection>,
    /// Handle to the fan-out drainer task
    drainer_handle: JoinHandle<()>,
    /// Bounded send queue — `try_send()` returns instantly, drainer fans out in background
    send_tx: tokio::sync::mpsc::Sender<Vec<u8>>,
    /// Shared event target (can be updated on re-join)
    event_target: SharedEventTarget,
    /// Current number of connected peers
    peer_count: SharedPeerCount,
    /// Dropped packet counter (overload detection, shared with SendHandle)
    drops: Arc<AtomicU64>,
}

impl std::fmt::Debug for ChannelState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChannelState")
            .field("peers", &self.peers.len())
            .field("drainer_handle", &"JoinHandle<()>")
            .field("event_target", &"SharedEventTarget")
            .field("peer_count", &self.peer_count.load(Ordering::Relaxed))
            .field("drops", &self.drops.load(Ordering::Relaxed))
            .finish()
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
    /// Messages were lost (app should request resync)
    Lagged,
}

// ─── Event helpers ──────────────────────────────────────────────────────────

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

// ─── Peer connection management ─────────────────────────────────────────────

/// Handle an incoming peer connection from the accept loop.
/// The peer opens a bi stream and sends a 32-byte topic ID.
async fn handle_incoming_peer(
    conn: iroh::endpoint::Connection,
    channels: Arc<RwLock<HashMap<TopicId, ChannelState>>>,
    our_key_bytes: [u8; PUBLIC_KEY_LENGTH],
) -> Result<()> {
    let peer_key = conn.remote_id();

    // Read topic ID from the control bi stream (with timeout to prevent stalling)
    let (_ctrl_send, mut ctrl_recv) = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        conn.accept_bi(),
    ).await
        .map_err(|_| anyhow!("Timeout waiting for topic handshake"))?
        .map_err(|e| anyhow!("Failed to accept bi stream: {e}"))?;

    let mut topic_bytes = [0u8; 32];
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        ctrl_recv.read_exact(&mut topic_bytes),
    ).await
        .map_err(|_| anyhow!("Timeout reading topic ID"))?
        .map_err(|e| anyhow!("Failed to read topic ID: {e}"))?;

    let topic = TopicId::from_bytes(topic_bytes);
    log_info!("[WEBXDC] Incoming peer {} for topic {:?}", peer_key, topic);

    let mut channels_guard = channels.write().await;
    let Some(channel) = channels_guard.get_mut(&topic) else {
        conn.close(0u32.into(), b"unknown topic");
        bail!("Incoming peer for unknown topic {:?}", topic);
    };

    // Tie-breaker for simultaneous connections: if we already have a connection
    // to this peer, the side with the higher public key wins (keeps their outgoing).
    // We're the acceptor here (incoming connection), so we should only accept if
    // the remote peer has the higher key (they're the rightful initiator).
    if channel.peers.contains_key(&peer_key) {
        if *peer_key.as_bytes() > our_key_bytes {
            // Remote has higher key — they're the initiator, accept their connection
            log_info!("[WEBXDC] Tie-breaker: accepting incoming from {} (higher key wins)", peer_key);
            let old = channel.peers.remove(&peer_key).unwrap();
            old.conn.close(0u32.into(), b"tie-break");
            old.read_task.abort();
            old.write_task.abort();
            // Decrement peer count since we'll re-increment below
            let _ = channel.peer_count.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |c| {
                if c > 0 { Some(c - 1) } else { None }
            });
        } else {
            // We have higher key — we're the initiator, keep our outgoing connection
            log_info!("[WEBXDC] Tie-breaker: rejecting incoming from {} (we have higher key)", peer_key);
            conn.close(0u32.into(), b"tie-break");
            return Ok(());
        }
    }

    // Spawn read/write tasks for this peer
    let peer_conn = spawn_peer_tasks(
        conn,
        peer_key,
        topic,
        channel.event_target.clone(),
        channel.peer_count.clone(),
        channels.clone(),
    );

    // Increment peer count and emit events
    let new_count = channel.peer_count.fetch_add(1, Ordering::Relaxed) + 1;
    log_info!("[WEBXDC] Peer {} joined topic {:?} (now {} peers)", peer_key, topic, new_count);
    let peer_str = base32_nopad_encode(peer_key.as_bytes());
    send_event(&channel.event_target, RealtimeEvent::PeerJoined(peer_str));

    channel.peers.insert(peer_key, peer_conn);

    Ok(())
}

/// Connect to a peer: establish QUIC connection, send topic ID, spawn tasks.
///
/// Tie-breaker: only the side with the higher public key initiates connections.
/// The lower-key side skips `connect_to_peer` and waits for the incoming connection
/// via the accept loop. This prevents simultaneous connections that cause
/// stream orphaning with persistent uni streams.
async fn connect_to_peer(
    endpoint: &Endpoint,
    addr: EndpointAddr,
    topic: TopicId,
    event_target: SharedEventTarget,
    peer_count: SharedPeerCount,
    our_key_bytes: [u8; PUBLIC_KEY_LENGTH],
    channels: Arc<RwLock<HashMap<TopicId, ChannelState>>>,
) -> Result<()> {
    // Tie-breaker: only initiate if we have the higher public key
    let peer_id_bytes = addr.id.as_bytes();
    if our_key_bytes < *peer_id_bytes {
        log_info!("[WEBXDC] Skipping outgoing connection to {} (they have higher key, they'll initiate)", addr.id);
        return Ok(());
    }

    let conn = endpoint.connect(addr.clone(), VECTOR_RT_ALPN).await
        .map_err(|e| anyhow!("Failed to connect to peer: {e}"))?;
    let peer_key = conn.remote_id();

    // Open control bi stream and send topic ID (topic handshake)
    let (mut ctrl_send, _ctrl_recv) = conn.open_bi().await
        .map_err(|e| anyhow!("Failed to open bi stream: {e}"))?;
    ctrl_send.write_all(topic.as_bytes()).await
        .map_err(|e| anyhow!("Failed to send topic ID: {e}"))?;

    let peer_conn = spawn_peer_tasks(
        conn, peer_key, topic,
        event_target.clone(), peer_count.clone(),
        channels.clone(),
    );

    // Add to channel
    let mut channels_guard = channels.write().await;
    if let Some(channel) = channels_guard.get_mut(&topic) {
        // Should not have an existing connection (we're the initiator), but handle gracefully
        if let Some(old) = channel.peers.remove(&peer_key) {
            old.conn.close(0u32.into(), b"replaced");
            old.read_task.abort();
            old.write_task.abort();
        }

        let new_count = channel.peer_count.fetch_add(1, Ordering::Relaxed) + 1;
        log_info!("[WEBXDC] Connected to peer {} for topic {:?} (now {} peers)", peer_key, topic, new_count);
        let peer_str = base32_nopad_encode(peer_key.as_bytes());
        send_event(&channel.event_target, RealtimeEvent::PeerJoined(peer_str));

        channel.peers.insert(peer_key, peer_conn);
    } else {
        bail!("Channel removed before peer connection completed");
    }

    Ok(())
}

/// Spawn read and write tasks for a peer connection.
fn spawn_peer_tasks(
    conn: iroh::endpoint::Connection,
    peer_key: PublicKey,
    topic: TopicId,
    event_target: SharedEventTarget,
    peer_count: SharedPeerCount,
    channels: Arc<RwLock<HashMap<TopicId, ChannelState>>>,
) -> PeerConnection {
    let (send_tx, send_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(PEER_SEND_QUEUE_CAPACITY);

    let read_conn = conn.clone();
    let read_target = event_target;
    let read_peer_count = peer_count;
    let read_channels = channels;
    let read_task = tokio::spawn(async move {
        run_peer_reader(read_conn, peer_key, topic, read_target, read_peer_count, read_channels).await;
    });

    let write_conn = conn.clone();
    let write_task = tokio::spawn(async move {
        run_peer_writer(write_conn, send_rx).await;
    });

    PeerConnection {
        conn,
        read_task,
        write_task,
        send_tx,
    }
}

// ─── Per-peer reader/writer tasks ───────────────────────────────────────────

/// Read varint-length-prefixed messages from a persistent uni stream and deliver to the event target.
/// No trailer needed — with raw QUIC, sender identity comes from the connection
/// itself and QUIC guarantees no duplicates.
async fn run_peer_reader(
    conn: iroh::endpoint::Connection,
    peer_key: PublicKey,
    topic: TopicId,
    shared_event_target: SharedEventTarget,
    peer_count: SharedPeerCount,
    channels: Arc<RwLock<HashMap<TopicId, ChannelState>>>,
) {
    // Accept the persistent uni stream from this peer's writer
    let mut recv = match conn.accept_uni().await {
        Ok(r) => r,
        Err(_) => {
            cleanup_peer(peer_key, &peer_count, &shared_event_target, &channels, &topic).await;
            return;
        }
    };

    let mut b0 = [0u8; 1];
    loop {
        // Read varint length prefix:
        //   0xxxxxxx          = 1 byte  (0–127)
        //   10xxxxxx xxxxxxxx = 2 bytes (128–16,383)
        //   11xxxxxx xxxxxxxx xxxxxxxx = 3 bytes (16,384–4,194,303)
        match recv.read_exact(&mut b0).await {
            Ok(()) => {}
            Err(_) => break, // Stream/connection closed
        }
        let msg_len = if b0[0] & 0x80 == 0 {
            b0[0] as usize
        } else {
            let mut extra = [0u8; 1];
            if recv.read_exact(&mut extra).await.is_err() { break; }
            if b0[0] & 0xC0 == 0x80 {
                ((b0[0] as usize & 0x3F) << 8) | extra[0] as usize
            } else {
                let mut extra2 = [0u8; 1];
                if recv.read_exact(&mut extra2).await.is_err() { break; }
                ((b0[0] as usize & 0x3F) << 16) | ((extra[0] as usize) << 8) | extra2[0] as usize
            }
        };
        if msg_len == 0 {
            continue; // Stream announcement packet — skip
        }
        if msg_len > MAX_MESSAGE_SIZE {
            log_warn!("[WEBXDC] Peer {} sent oversized message: {} bytes", peer_key, msg_len);
            break;
        }

        // Read the message body
        let mut content = vec![0u8; msg_len];
        match recv.read_exact(&mut content).await {
            Ok(()) => {}
            Err(e) => {
                log_warn!("[WEBXDC] Failed to read message from peer {}: {e}", peer_key);
                break;
            }
        }

        // Deliver raw payload — no trailer to strip, no self-check needed
        send_event(&shared_event_target, RealtimeEvent::Data(base91_encode(&content)));
    }

    cleanup_peer(peer_key, &peer_count, &shared_event_target, &channels, &topic).await;
}

/// Clean up after a peer disconnects.
async fn cleanup_peer(
    peer_key: PublicKey,
    peer_count: &SharedPeerCount,
    shared_event_target: &SharedEventTarget,
    channels: &Arc<RwLock<HashMap<TopicId, ChannelState>>>,
    topic: &TopicId,
) {
    log_info!("[WEBXDC] Peer disconnected: {}", peer_key);
    let peer_str = base32_nopad_encode(peer_key.as_bytes());

    // Decrement peer count (saturating to avoid underflow)
    let _ = peer_count.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |c| {
        if c > 0 { Some(c - 1) } else { None }
    });

    send_event(shared_event_target, RealtimeEvent::PeerLeft(peer_str));

    // Remove from channel peers map
    let mut channels_guard = channels.write().await;
    if let Some(channel) = channels_guard.get_mut(topic) {
        if let Some(peer) = channel.peers.remove(&peer_key) {
            peer.write_task.abort();
        }
    }
}

/// Write varint-length-prefixed messages to a persistent uni stream.
/// Opens one stream and reuses it for all messages — eliminates per-message
/// stream handshake overhead (critical for relay-routed connections).
async fn run_peer_writer(
    conn: iroh::endpoint::Connection,
    mut rx: tokio::sync::mpsc::Receiver<Vec<u8>>,
) {
    // Open the persistent send stream
    let mut send = match conn.open_uni().await {
        Ok(s) => s,
        Err(e) => {
            log_warn!("[WEBXDC] Failed to open send stream: {e}");
            return;
        }
    };

    // Write a zero-length announcement to make the stream visible to the remote reader.
    // QUIC doesn't notify the peer that a stream exists until data is written.
    if let Err(e) = send.write_all(&[0u8]).await {
        log_warn!("[WEBXDC] Failed to announce stream: {e}");
        return;
    }

    while let Some(data) = rx.recv().await {
        // Varint length prefix: 1 byte (0–127), 2 bytes (128–16,383), 3 bytes (16,384+)
        let n = data.len();
        let (hdr, hdr_len) = if n < 128 {
            ([n as u8, 0, 0], 1)
        } else if n < 16384 {
            ([0x80 | (n >> 8) as u8, n as u8, 0], 2)
        } else {
            ([0xC0 | (n >> 16) as u8, (n >> 8) as u8, n as u8], 3)
        };
        if let Err(e) = send.write_all(&hdr[..hdr_len]).await {
            log_warn!("[WEBXDC] Peer write failed (length): {e}");
            break;
        }
        if let Err(e) = send.write_all(&data).await {
            log_warn!("[WEBXDC] Peer write failed (payload): {e}");
            break;
        }
    }

    // Gracefully close the stream
    let _ = send.finish();
}

// ─── Drainer ────────────────────────────────────────────────────────────────

/// Drainer task: reads from the global send queue and fans out to all per-peer
/// send queues. Each peer has its own independent queue so one slow peer
/// cannot block others — the architectural solution to head-of-line blocking.
async fn run_send_drainer(
    mut rx: tokio::sync::mpsc::Receiver<Vec<u8>>,
    channels: Arc<RwLock<HashMap<TopicId, ChannelState>>>,
    topic: TopicId,
    drops: Arc<AtomicU64>,
) {
    let mut drop_check = tokio::time::interval(std::time::Duration::from_secs(5));
    drop_check.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut batch = Vec::with_capacity(64);

    loop {
        // Wait for at least one message (or drop-check tick)
        tokio::select! {
            msg = rx.recv() => {
                match msg {
                    Some(data) => batch.push(data),
                    None => break,
                }
            }
            _ = drop_check.tick() => {
                let dropped = drops.swap(0, Ordering::Relaxed);
                if dropped > 0 {
                    log_warn!("[WEBXDC] Realtime overload: {dropped} packets dropped in last 5s");
                }
                continue;
            }
        }

        // Drain all immediately-available messages (non-blocking)
        while batch.len() < 64 {
            match rx.try_recv() {
                Ok(data) => batch.push(data),
                Err(_) => break,
            }
        }

        // Fan-out: send each message to every peer's individual queue (non-blocking)
        let channels_guard = channels.read().await;
        if let Some(channel) = channels_guard.get(&topic) {
            for data in batch.drain(..) {
                for (_peer_key, peer_conn) in &channel.peers {
                    // Non-blocking: if this peer's queue is full, they're slow — skip them
                    let _ = peer_conn.send_tx.try_send(data.clone());
                }
            }
        } else {
            batch.clear();
        }
    }
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
    /// WebSocket server info (port + token), set once after Iroh init
    ws_info: std::sync::OnceLock<super::rt_ws::WsInfo>,
}

impl RealtimeManager {
    pub fn new(relay_url: Option<String>) -> Self {
        Self {
            iroh: tokio::sync::OnceCell::new(),
            relay_url,
            ws_info: std::sync::OnceLock::new(),
        }
    }

    /// Get or initialize the Iroh state.
    /// After first call, this is a single atomic load (~5ns).
    /// Also starts the realtime WebSocket server on first init.
    pub async fn get_or_init(&self) -> Result<&IrohState> {
        let iroh = self.iroh
            .get_or_try_init(|| IrohState::new(self.relay_url.clone()))
            .await?;

        // Start the WS server once (after IrohState is available)
        if self.ws_info.get().is_none() {
            match super::rt_ws::start(
                iroh.send_handles.clone(),
            ).await {
                Ok(info) => {
                    let _ = self.ws_info.set(info);
                }
                Err(e) => {
                    log_warn!("[WEBXDC] Failed to start RT WS server: {e}");
                }
            }
        }

        Ok(iroh)
    }

    /// Get the WebSocket URL for the realtime fast path, if the server is running.
    /// Returns `ws://127.0.0.1:{port}/{token}`.
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

// ─── Topic ID helpers ───────────────────────────────────────────────────────

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
