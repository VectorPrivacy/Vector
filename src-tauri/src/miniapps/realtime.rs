//! Realtime peer channels for Mini Apps using Iroh
//!
//! This module provides P2P realtime communication for WebXDC apps using Iroh,
//! matching DeltaChat's implementation for cross-compatibility.
//!
//! See: https://webxdc.org/docs/spec/joinRealtimeChannel.html

#![allow(dead_code)] // API functions that will be used as the feature matures

use anyhow::{anyhow, bail, Context as _, Result};
use data_encoding::BASE32_NOPAD;
use futures_util::StreamExt;
use iroh::{Endpoint, NodeAddr, NodeId, PublicKey, RelayMode, SecretKey};
use iroh_gossip::net::{Event, Gossip, GossipEvent, JoinOptions, GOSSIP_ALPN};
use iroh_gossip::proto::TopicId;
use log::{info, trace, warn};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use tauri::ipc::Channel;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::{oneshot, RwLock};
use tokio::task::JoinHandle;

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

    /// Sequence numbers for gossip channels (for deduplication)
    pub(crate) sequence_numbers: Mutex<HashMap<TopicId, i32>>,

    /// Active realtime channels
    pub(crate) channels: RwLock<HashMap<TopicId, ChannelState>>,

    /// Our public key (attached to messages for deduplication)
    pub(crate) public_key: PublicKey,
}

impl IrohState {
    /// Initialize a new Iroh state with endpoint and gossip
    pub async fn new(_relay_url: Option<String>) -> Result<Self> {
        info!("Initializing Iroh peer channels");
        
        let secret_key = SecretKey::generate(rand::rngs::OsRng);
        let public_key = secret_key.public();

        // Build the endpoint with default relay mode
        let endpoint = Endpoint::builder()
            .secret_key(secret_key)
            .alpns(vec![GOSSIP_ALPN.to_vec()])
            .relay_mode(RelayMode::Default)
            .bind()
            .await?;

        // Wait for the relay connection to be established
        // This is important because we need the relay URL in our node address
        println!("[WEBXDC] Waiting for relay connection...");
        let mut relay_watcher = endpoint.home_relay();
        let relay_timeout = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            async {
                loop {
                    match relay_watcher.get() {
                        Ok(Some(url)) => {
                            println!("[WEBXDC] Connected to relay: {}", url);
                            return Some(url);
                        }
                        Ok(None) => {
                            // No relay yet, wait for update
                            if relay_watcher.updated().await.is_err() {
                                // Watcher disconnected
                                break;
                            }
                        }
                        Err(_) => {
                            // Watcher disconnected
                            break;
                        }
                    }
                }
                None
            }
        ).await;
        
        match relay_timeout {
            Ok(Some(url)) => println!("[WEBXDC] Relay connection established: {}", url),
            Ok(None) => println!("[WEBXDC] WARNING: Relay watcher closed without connecting"),
            Err(_) => println!("[WEBXDC] WARNING: Timeout waiting for relay connection"),
        }

        // Create gossip with max message size of 128 KB
        let gossip = Gossip::builder()
            .max_message_size(MAX_MESSAGE_SIZE)
            .spawn(endpoint.clone())
            .await?;

        // Start the accept loop to handle incoming connections
        // The gossip protocol doesn't accept connections itself - we need to do it
        let accept_endpoint = endpoint.clone();
        let accept_gossip = gossip.clone();
        tokio::spawn(async move {
            println!("[WEBXDC] Starting connection accept loop");
            loop {
                match accept_endpoint.accept().await {
                    Some(incoming) => {
                        let gossip = accept_gossip.clone();
                        tokio::spawn(async move {
                            match incoming.await {
                                Ok(conn) => {
                                    let alpn = conn.alpn();
                                    match alpn {
                                        Some(ref alpn_bytes) => {
                                            println!("[WEBXDC] Accepted connection with ALPN: {:?}", String::from_utf8_lossy(alpn_bytes));
                                            if alpn_bytes.as_slice() == GOSSIP_ALPN {
                                                println!("[WEBXDC] Forwarding gossip connection to gossip handler");
                                                if let Err(e) = gossip.handle_connection(conn).await {
                                                    println!("[WEBXDC] ERROR: Failed to handle gossip connection: {}", e);
                                                } else {
                                                    println!("[WEBXDC] Gossip connection handled successfully");
                                                }
                                            } else {
                                                println!("[WEBXDC] Ignoring non-gossip connection with ALPN: {:?}", String::from_utf8_lossy(alpn_bytes));
                                            }
                                        }
                                        None => {
                                            println!("[WEBXDC] Accepted connection with no ALPN, ignoring");
                                        }
                                    }
                                }
                                Err(e) => {
                                    println!("[WEBXDC] ERROR: Failed to accept incoming connection: {}", e);
                                }
                            }
                        });
                    }
                    None => {
                        println!("[WEBXDC] Accept loop ended - endpoint closed");
                        break;
                    }
                }
            }
        });

        Ok(Self {
            endpoint,
            gossip,
            sequence_numbers: Mutex::new(HashMap::new()),
            channels: RwLock::new(HashMap::new()),
            public_key,
        })
    }

    /// Notify the endpoint that the network has changed
    pub async fn network_change(&self) {
        self.endpoint.network_change().await
    }

    /// Close the Iroh endpoint
    pub async fn close(self) -> Result<()> {
        self.endpoint.close().await;
        Ok(())
    }

    /// Get our node address (without direct IP addresses for privacy)
    pub async fn get_node_addr(&self) -> Result<NodeAddr> {
        let mut addr = self.endpoint.node_addr().await?;
        println!("[WEBXDC] get_node_addr: node_id={}, relay_url={:?}, direct_addrs={}",
            addr.node_id,
            addr.relay_url(),
            addr.direct_addresses.len());
        // Remove direct addresses for privacy (only use relay)
        addr.direct_addresses = std::collections::BTreeSet::new();
        Ok(addr)
    }

    /// Join a gossip topic and start the subscriber loop
    pub async fn join_channel(
        &self,
        topic: TopicId,
        peers: Vec<NodeAddr>,
        event_channel: Channel<RealtimeEvent>,
        app_handle: Option<AppHandle>,
    ) -> Result<(bool, Option<oneshot::Receiver<()>>)> {
        let mut channels = self.channels.write().await;

        // If channel already exists, we're re-joining (e.g., user closed and reopened the game)
        // Update the shared event channel so the subscribe loop uses the new frontend channel
        if let Some(channel_state) = channels.get(&topic) {
            info!("IROH_REALTIME: Re-joining existing gossip topic {:?}, updating event channel", topic);
            let mut shared_channel = channel_state.event_channel.write().await;
            *shared_channel = Some(event_channel);
            println!("[WEBXDC] Updated shared event channel for existing topic");
            return Ok((true, None));
        }

        let node_ids: Vec<NodeId> = peers.iter().map(|p| p.node_id).collect();

        info!(
            "IROH_REALTIME: Joining gossip topic {:?} with {} peers",
            topic,
            node_ids.len()
        );

        // Add peer addresses to the endpoint
        for node_addr in &peers {
            if !node_addr.direct_addresses.is_empty() || node_addr.relay_url().is_some() {
                self.endpoint.add_node_addr(node_addr.clone())?;
            }
        }

        let (join_tx, join_rx) = oneshot::channel();

        let (gossip_sender, gossip_receiver) = self
            .gossip
            .subscribe_with_opts(topic, JoinOptions::with_bootstrap(node_ids))
            .split();

        // Create shared event channel for the subscribe loop
        let shared_event_channel: SharedEventChannel = Arc::new(RwLock::new(Some(event_channel)));
        let shared_channel_clone = shared_event_channel.clone();
        
        // Create shared peer count
        let shared_peer_count: SharedPeerCount = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let peer_count_clone = shared_peer_count.clone();

        let public_key = self.public_key;
        let topic_encoded = encode_topic_id(&topic);
        let subscribe_loop = tokio::spawn(async move {
            if let Err(e) = run_subscribe_loop(gossip_receiver, topic, shared_channel_clone, join_tx, public_key, peer_count_clone, app_handle, topic_encoded).await {
                warn!("Subscribe loop failed: {e}");
            }
        });

        channels.insert(topic, ChannelState::new(subscribe_loop, gossip_sender, shared_event_channel, shared_peer_count));

        Ok((false, Some(join_rx)))
    }

    /// Add a peer to an existing channel
    pub async fn add_peer(&self, topic: TopicId, peer: NodeAddr) -> Result<()> {
        // First, add the node address to the endpoint so we can connect to it
        println!("[WEBXDC] add_peer: Adding node addr for peer {}, relay_url={:?}, direct_addrs={}",
            peer.node_id,
            peer.relay_url(),
            peer.direct_addresses.len());
        self.endpoint.add_node_addr(peer.clone())?;
        
        // Verify the node address was added by checking if we can get connection info
        if let Some(info) = self.endpoint.remote_info(peer.node_id) {
            println!("[WEBXDC] add_peer: Remote info for peer {}: relay_url={:?}, addrs={:?}",
                peer.node_id,
                info.relay_url,
                info.addrs);
        } else {
            println!("[WEBXDC] add_peer: WARNING - Could not get remote info for peer {}", peer.node_id);
        }
        
        // Then, use the existing channel's sender to join the peer
        let channels = self.channels.read().await;
        if let Some(channel_state) = channels.get(&topic) {
            println!("[WEBXDC] add_peer: Joining peer {} via existing channel sender", peer.node_id);
            channel_state.sender.join_peers(vec![peer.node_id]).await?;
            println!("[WEBXDC] add_peer: Successfully joined peer {} to topic (join_peers returned)", peer.node_id);
        } else {
            println!("[WEBXDC] add_peer: Channel not found for topic, cannot add peer");
            return Err(anyhow!("Channel not found for topic"));
        }
        Ok(())
    }

    /// Send data to a gossip topic
    pub async fn send_data(&self, topic: TopicId, mut data: Vec<u8>) -> Result<()> {
        let mut channels = self.channels.write().await;
        let state = channels
            .get_mut(&topic)
            .ok_or_else(|| anyhow!("Channel not found for topic"))?;

        // Append sequence number and public key for deduplication
        let seq_num = self.get_and_incr_seq(&topic);
        data.extend(seq_num.to_le_bytes());
        data.extend(self.public_key.as_bytes());

        state.sender.broadcast(data.into()).await?;

        trace!("Sent realtime data to topic {:?}", topic);

        Ok(())
    }

    /// Leave a realtime channel
    pub async fn leave_channel(&self, topic: TopicId) -> Result<()> {
        if let Some(channel) = self.channels.write().await.remove(&topic) {
            // Abort the subscribe loop (this drops the receiver)
            channel.subscribe_loop.abort();
            let _ = channel.subscribe_loop.await;
            info!("Left realtime channel {:?}", topic);
        }
        Ok(())
    }

    /// Get the current peer count for a topic
    pub async fn get_peer_count(&self, topic: &TopicId) -> usize {
        let channels = self.channels.read().await;
        if let Some(channel_state) = channels.get(topic) {
            let count = channel_state.peer_count.load(std::sync::atomic::Ordering::Relaxed);
            println!("[WEBXDC] get_peer_count for topic {:?}: {} (from ChannelState)", topic, count);
            count
        } else {
            println!("[WEBXDC] get_peer_count for topic {:?}: 0 (no channel found)", topic);
            0
        }
    }

    /// Check if a channel exists for a topic
    pub async fn has_channel(&self, topic: &TopicId) -> bool {
        self.channels.read().await.contains_key(topic)
    }

    /// Get and increment sequence number for a topic
    fn get_and_incr_seq(&self, topic: &TopicId) -> i32 {
        let mut seq_nums = self.sequence_numbers.lock();
        let entry = seq_nums.entry(*topic).or_default();
        *entry = entry.wrapping_add(1);
        *entry
    }
}

/// Shared event channel that can be updated when a user re-joins
pub(crate) type SharedEventChannel = Arc<RwLock<Option<Channel<RealtimeEvent>>>>;

/// Shared peer count that can be updated by the subscribe loop
pub(crate) type SharedPeerCount = Arc<std::sync::atomic::AtomicUsize>;

/// State for a single gossip channel
pub(crate) struct ChannelState {
    /// Handle to the subscribe loop task
    subscribe_loop: JoinHandle<()>,
    /// Sender for broadcasting messages
    sender: iroh_gossip::net::GossipSender,
    /// Shared event channel (can be updated on re-join)
    event_channel: SharedEventChannel,
    /// Current number of connected peers
    peer_count: SharedPeerCount,
}

impl std::fmt::Debug for ChannelState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChannelState")
            .field("subscribe_loop", &"JoinHandle<()>")
            .field("sender", &"GossipSender")
            .field("event_channel", &"SharedEventChannel")
            .field("peer_count", &self.peer_count.load(std::sync::atomic::Ordering::Relaxed))
            .finish()
    }
}

impl ChannelState {
    fn new(subscribe_loop: JoinHandle<()>, sender: iroh_gossip::net::GossipSender, event_channel: SharedEventChannel, peer_count: SharedPeerCount) -> Self {
        Self {
            subscribe_loop,
            sender,
            event_channel,
            peer_count,
        }
    }
}

/// Events sent to the frontend via Tauri channel
#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase", tag = "event", content = "data")]
pub enum RealtimeEvent {
    /// Received data from a peer
    Data(Vec<u8>),
    /// Channel became operational (connected to peers)
    Connected,
    /// A peer joined the channel
    PeerJoined(String),
    /// A peer left the channel
    PeerLeft(String),
}

/// Helper to send an event through the shared channel
async fn send_event(shared_channel: &SharedEventChannel, event: RealtimeEvent) -> bool {
    let channel_guard = shared_channel.read().await;
    if let Some(ref channel) = *channel_guard {
        if let Err(e) = channel.send(event) {
            println!("[WEBXDC] ERROR: Failed to send event to frontend: {e}");
            return false;
        }
        true
    } else {
        println!("[WEBXDC] WARNING: No event channel available");
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
    mut receiver: iroh_gossip::net::GossipReceiver,
    topic: TopicId,
    shared_event_channel: SharedEventChannel,
    join_tx: oneshot::Sender<()>,
    our_public_key: PublicKey,
    peer_count: SharedPeerCount,
    app_handle: Option<AppHandle>,
    topic_encoded: String,
) -> Result<()> {
    let mut join_tx = Some(join_tx);
    println!("[WEBXDC] Subscribe loop started for topic {:?}", topic);

    while let Some(event) = receiver.next().await {
        match event {
            Ok(Event::Gossip(gossip_event)) => match gossip_event {
                GossipEvent::Received(msg) => {
                    let mut data = msg.content.to_vec();
                    
                    // Extract and remove the appended public key and sequence number
                    if data.len() >= PUBLIC_KEY_LENGTH + 4 {
                        let sender_key_bytes = data.split_off(data.len() - PUBLIC_KEY_LENGTH);
                        let _seq_bytes = data.split_off(data.len() - 4);
                        
                        // Skip messages from ourselves
                        if let Ok(sender_key) = PublicKey::try_from(sender_key_bytes.as_slice()) {
                            if sender_key == our_public_key {
                                println!("[WEBXDC] Skipping message from ourselves");
                                continue;
                            }
                        }
                    }

                    // Send data to frontend via shared channel
                    send_event(&shared_event_channel, RealtimeEvent::Data(data)).await;
                }
                GossipEvent::Joined(peers) => {
                    println!("[WEBXDC] Peers joined: {:?}", peers.len());
                    // Update peer count based on joined peers
                    // This is more reliable than NeighborUp for initial count
                    let joined_count = peers.len();
                    if joined_count > 0 {
                        let current = peer_count.load(std::sync::atomic::Ordering::Relaxed);
                        if joined_count > current {
                            peer_count.store(joined_count, std::sync::atomic::Ordering::Relaxed);
                            println!("[WEBXDC] Updated peer count to {} from Joined event", joined_count);
                            emit_realtime_status(&app_handle, &topic_encoded, joined_count, true);
                        }
                    }
                    for peer in peers {
                        let peer_id = BASE32_NOPAD.encode(peer.as_bytes());
                        println!("[WEBXDC] Peer joined topic: {}", peer_id);
                        send_event(&shared_event_channel, RealtimeEvent::PeerJoined(peer_id)).await;
                    }
                }
                GossipEvent::NeighborUp(peer) => {
                    let peer_id = BASE32_NOPAD.encode(peer.as_bytes());
                    println!("[WEBXDC] Neighbor UP for topic: {}", peer_id);
                    
                    // Increment peer count
                    let new_count = peer_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                    println!("[WEBXDC] Peer count now: {}", new_count);
                    
                    // Emit status update to main window
                    emit_realtime_status(&app_handle, &topic_encoded, new_count, true);
                    
                    // Signal that we're connected when first neighbor comes up
                    if let Some(tx) = join_tx.take() {
                        let _ = tx.send(());
                        send_event(&shared_event_channel, RealtimeEvent::Connected).await;
                    }
                }
                GossipEvent::NeighborDown(peer) => {
                    let peer_id = BASE32_NOPAD.encode(peer.as_bytes());
                    println!("[WEBXDC] Neighbor DOWN for topic: {}", peer_id);
                    
                    // Decrement peer count (saturating to avoid underflow)
                    let old_count = peer_count.load(std::sync::atomic::Ordering::Relaxed);
                    if old_count > 0 {
                        peer_count.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                    }
                    let new_count = peer_count.load(std::sync::atomic::Ordering::Relaxed);
                    println!("[WEBXDC] Peer count now: {}", new_count);
                    
                    // Emit status update to main window
                    emit_realtime_status(&app_handle, &topic_encoded, new_count, true);
                    
                    send_event(&shared_event_channel, RealtimeEvent::PeerLeft(peer_id)).await;
                }
            },
            Ok(Event::Lagged) => {
                println!("[WEBXDC] WARNING: Gossip lagged for topic {:?}", topic);
            }
            Err(e) => {
                println!("[WEBXDC] ERROR: Gossip error for topic {:?}: {e}", topic);
            }
        }
    }

    println!("[WEBXDC] Subscribe loop ended for topic {:?}", topic);
    Ok(())
}

/// Global Iroh state manager
pub struct RealtimeManager {
    /// Iroh state (lazily initialized)
    iroh: RwLock<Option<IrohState>>,
    /// Custom relay URL (if any)
    relay_url: Option<String>,
}

impl RealtimeManager {
    pub fn new(relay_url: Option<String>) -> Self {
        Self {
            iroh: RwLock::new(None),
            relay_url,
        }
    }

    /// Get or initialize the Iroh state
    pub async fn get_or_init(&self) -> Result<tokio::sync::RwLockReadGuard<'_, IrohState>> {
        // Check if already initialized
        {
            let guard = self.iroh.read().await;
            if guard.is_some() {
                return Ok(tokio::sync::RwLockReadGuard::map(guard, |opt| {
                    opt.as_ref().unwrap()
                }));
            }
        }

        // Initialize
        {
            let mut guard = self.iroh.write().await;
            if guard.is_none() {
                let iroh = IrohState::new(self.relay_url.clone()).await?;
                *guard = Some(iroh);
            }
        }

        // Return read guard
        let guard = self.iroh.read().await;
        Ok(tokio::sync::RwLockReadGuard::map(guard, |opt| {
            opt.as_ref().unwrap()
        }))
    }

    /// Shutdown Iroh if initialized
    pub async fn shutdown(&self) -> Result<()> {
        let mut guard = self.iroh.write().await;
        if let Some(iroh) = guard.take() {
            iroh.close().await?;
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
    BASE32_NOPAD.encode(topic.as_bytes())
}

/// Decode a topic ID from a string
pub fn decode_topic_id(s: &str) -> Result<TopicId> {
    let bytes = BASE32_NOPAD
        .decode(s.as_bytes())
        .context("Invalid topic ID encoding")?;
    if bytes.len() != 32 {
        bail!("Invalid topic ID length");
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    Ok(TopicId::from_bytes(arr))
}

/// Encode a node address to a string for transmission via Nostr
pub fn encode_node_addr(addr: &NodeAddr) -> Result<String> {
    let json = serde_json::to_string(addr)?;
    Ok(BASE32_NOPAD.encode(json.as_bytes()))
}

/// Decode a node address from a string
pub fn decode_node_addr(s: &str) -> Result<NodeAddr> {
    let bytes = BASE32_NOPAD
        .decode(s.as_bytes())
        .context("Invalid node address encoding")?;
    let json = String::from_utf8(bytes)?;
    println!("[WEBXDC] decode_node_addr: json={}", json);
    let addr: NodeAddr = serde_json::from_str(&json)?;
    println!("[WEBXDC] decode_node_addr: node_id={}, relay_url={:?}, direct_addrs={}",
        addr.node_id,
        addr.relay_url(),
        addr.direct_addresses.len());
    Ok(addr)
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
}