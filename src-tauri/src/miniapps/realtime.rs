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
use log::{debug, error, info, trace, warn};
use parking_lot::Mutex;
use std::collections::HashMap;
use tauri::ipc::Channel;
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

        // Create gossip with max message size of 128 KB
        let gossip = Gossip::builder()
            .max_message_size(MAX_MESSAGE_SIZE)
            .spawn(endpoint.clone())
            .await?;

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
    ) -> Result<Option<oneshot::Receiver<()>>> {
        let mut channels = self.channels.write().await;

        if channels.contains_key(&topic) {
            return Ok(None);
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

        let public_key = self.public_key;
        let subscribe_loop = tokio::spawn(async move {
            if let Err(e) = run_subscribe_loop(gossip_receiver, topic, event_channel, join_tx, public_key).await {
                warn!("Subscribe loop failed: {e}");
            }
        });

        channels.insert(topic, ChannelState::new(subscribe_loop, gossip_sender));

        Ok(Some(join_rx))
    }

    /// Add a peer to an existing channel
    pub async fn add_peer(&self, topic: TopicId, peer: NodeAddr) -> Result<()> {
        if self.channels.read().await.contains_key(&topic) {
            self.endpoint.add_node_addr(peer.clone())?;
            self.gossip.subscribe(topic, vec![peer.node_id])?;
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

    /// Get and increment sequence number for a topic
    fn get_and_incr_seq(&self, topic: &TopicId) -> i32 {
        let mut seq_nums = self.sequence_numbers.lock();
        let entry = seq_nums.entry(*topic).or_default();
        *entry = entry.wrapping_add(1);
        *entry
    }
}

/// State for a single gossip channel
#[derive(Debug)]
pub(crate) struct ChannelState {
    /// Handle to the subscribe loop task
    subscribe_loop: JoinHandle<()>,
    /// Sender for broadcasting messages
    sender: iroh_gossip::net::GossipSender,
}

impl ChannelState {
    fn new(subscribe_loop: JoinHandle<()>, sender: iroh_gossip::net::GossipSender) -> Self {
        Self {
            subscribe_loop,
            sender,
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

/// Run the subscribe loop for a gossip topic
async fn run_subscribe_loop(
    mut receiver: iroh_gossip::net::GossipReceiver,
    topic: TopicId,
    event_channel: Channel<RealtimeEvent>,
    join_tx: oneshot::Sender<()>,
    our_public_key: PublicKey,
) -> Result<()> {
    let mut join_tx = Some(join_tx);

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
                                continue;
                            }
                        }
                    }

                    // Send data to frontend
                    if let Err(e) = event_channel.send(RealtimeEvent::Data(data)) {
                        warn!("Failed to send realtime data to frontend: {e}");
                    }
                }
                GossipEvent::Joined(peers) => {
                    for peer in peers {
                        let peer_id = BASE32_NOPAD.encode(peer.as_bytes());
                        debug!("Peer joined topic {:?}: {}", topic, peer_id);
                        let _ = event_channel.send(RealtimeEvent::PeerJoined(peer_id));
                    }
                }
                GossipEvent::NeighborUp(peer) => {
                    let peer_id = BASE32_NOPAD.encode(peer.as_bytes());
                    debug!("Neighbor up for topic {:?}: {}", topic, peer_id);
                    
                    // Signal that we're connected when first neighbor comes up
                    if let Some(tx) = join_tx.take() {
                        let _ = tx.send(());
                        let _ = event_channel.send(RealtimeEvent::Connected);
                    }
                }
                GossipEvent::NeighborDown(peer) => {
                    let peer_id = BASE32_NOPAD.encode(peer.as_bytes());
                    debug!("Neighbor down for topic {:?}: {}", topic, peer_id);
                    let _ = event_channel.send(RealtimeEvent::PeerLeft(peer_id));
                }
            },
            Ok(Event::Lagged) => {
                warn!("Gossip lagged for topic {:?}", topic);
            }
            Err(e) => {
                error!("Gossip error for topic {:?}: {e}", topic);
            }
        }
    }

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
    let addr: NodeAddr = serde_json::from_str(&json)?;
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