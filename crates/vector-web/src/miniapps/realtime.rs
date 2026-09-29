//! Realtime channels for mini apps over Iroh gossip, relay-only.
//!
//! Wire-compatible with desktop (`src-tauri/src/miniapps/realtime.rs`): the same
//! gossip ALPN, topic bytes, relay-only address encoding and message trailer, so
//! a browser and a desktop peer share one channel. A browser can't open direct
//! paths anyway; every peer only ever sees the relay.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use futures_util::StreamExt;
use iroh::endpoint::VarInt;
use iroh::{Endpoint, EndpointAddr, PublicKey, RelayMode, SecretKey, TransportAddr};
use iroh_gossip::api::{Event, GossipReceiver, GossipSender, JoinOptions};
use iroh_gossip::net::{Gossip, GOSSIP_ALPN};
pub use iroh_gossip::proto::TopicId;
use serde_json::json;
use vector_core::rt::JoinHandle;

use crate::emitter;

const MAX_MESSAGE_SIZE: usize = 128 * 1024;
const PUBLIC_KEY_LENGTH: usize = 32;
/// Every gossip message ends in a 4-byte sequence number and the sender's node key.
const TRAILER_LEN: usize = 4 + PUBLIC_KEY_LENGTH;
/// Events held for a channel no app window has attached to yet.
const BUFFER_LIMIT: usize = 256;

pub struct Iroh {
    endpoint: Endpoint,
    gossip: Gossip,
    key_bytes: [u8; PUBLIC_KEY_LENGTH],
    channels: tokio::sync::RwLock<HashMap<TopicId, Channel>>,
}

struct Channel {
    sender: GossipSender,
    subscribe_loop: JoinHandle<()>,
    target: Arc<Mutex<Target>>,
    seq: AtomicI32,
}

/// Where a channel's events go: the app window's label once it joins, a buffer until then.
#[derive(Default)]
struct Target {
    label: Option<String>,
    buffer: Vec<Delivery>,
}

enum Delivery {
    Data(Vec<u8>),
    Signal(&'static str, Option<String>),
}

impl Target {
    fn send(&mut self, d: Delivery) {
        match &self.label {
            Some(label) => deliver(label, d),
            None if self.buffer.len() < BUFFER_LIMIT => self.buffer.push(d),
            None => {}
        }
    }

    fn attach(&mut self, label: String) {
        for d in self.buffer.drain(..) {
            deliver(&label, d);
        }
        self.label = Some(label);
    }
}

fn deliver(label: &str, d: Delivery) {
    match d {
        Delivery::Data(bytes) => emitter::emit_bytes("miniapp_rt", &json!({ "label": label, "event": "data" }), &bytes),
        Delivery::Signal(event, data) => emitter::emit("miniapp_rt", &json!({ "label": label, "event": event, "data": data })),
    }
}

static IROH: tokio::sync::Mutex<Option<Arc<Iroh>>> = tokio::sync::Mutex::const_new(None);

/// The process's endpoint, bound on first use. Its node key is fresh each run.
pub async fn iroh() -> Result<Arc<Iroh>, String> {
    let mut slot = IROH.lock().await;
    if let Some(i) = slot.as_ref() {
        return Ok(i.clone());
    }
    let i = Arc::new(Iroh::bind().await.map_err(|e| format!("Realtime unavailable: {e}"))?);
    *slot = Some(i.clone());
    Ok(i)
}

/// The endpoint if one is already bound; teardown never starts one.
pub async fn try_iroh() -> Option<Arc<Iroh>> {
    IROH.lock().await.clone()
}

impl Iroh {
    async fn bind() -> anyhow::Result<Self> {
        let mut key = [0u8; 32];
        rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut key);
        let secret_key = SecretKey::from(key);
        let key_bytes = *secret_key.public().as_bytes();

        // Desktop's tuning, so both ends of a channel behave alike.
        let transport = iroh::endpoint::QuicTransportConfig::builder()
            .keep_alive_interval(std::time::Duration::from_secs(15))
            .max_idle_timeout(Some(std::time::Duration::from_secs(120).try_into()?))
            .stream_receive_window(VarInt::from_u32(512 * 1024))
            .receive_window(VarInt::from_u32(2 * 1024 * 1024))
            .send_window(1_572_864)
            .max_concurrent_bidi_streams(VarInt::from_u32(256))
            .max_concurrent_uni_streams(VarInt::from_u32(256))
            .initial_rtt(std::time::Duration::from_millis(100))
            .congestion_controller_factory(Arc::new(noq_proto::congestion::CubicConfig::default()))
            .send_observed_address_reports(false)
            .receive_observed_address_reports(false)
            .build();

        // Relay-only: no address discovery publishing, nothing but the relay advertised.
        struct RelayOnly;
        impl iroh::endpoint::presets::Preset for RelayOnly {
            fn apply(self, builder: iroh::endpoint::Builder) -> iroh::endpoint::Builder {
                builder
                    .relay_mode(RelayMode::Default)
                    .crypto_provider(Arc::new(rustls::crypto::ring::default_provider()))
            }
        }

        let endpoint = Endpoint::builder(RelayOnly)
            .secret_key(secret_key)
            .alpns(vec![GOSSIP_ALPN.to_vec()])
            .transport_config(transport)
            .bind()
            .await?;

        // An advertisement without our relay is undiallable.
        let _ = vector_core::rt::time::timeout(std::time::Duration::from_secs(8), endpoint.online()).await;
        for _ in 0..20 {
            if endpoint.addr().addrs.iter().any(|a| matches!(a, TransportAddr::Relay(_))) {
                break;
            }
            vector_core::rt::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        vector_core::log_info!("[WEBXDC] endpoint {} via {}", short_id(&endpoint.id()), relay_urls(&endpoint.addr()));

        let gossip = Gossip::builder().max_message_size(MAX_MESSAGE_SIZE).spawn(endpoint.clone());

        let (ep, g) = (endpoint.clone(), gossip.clone());
        // spawn-detached: the endpoint's accept loop, alive as long as the endpoint.
        vector_core::rt::spawn(async move {
            while let Some(incoming) = ep.accept().await {
                let g = g.clone();
                // spawn-detached: one inbound connection's handshake.
                vector_core::rt::spawn(async move {
                    match incoming.await {
                        Ok(conn) if conn.alpn() == GOSSIP_ALPN => {
                            if let Err(e) = g.handle_connection(conn).await {
                                vector_core::log_warn!("[WEBXDC] gossip connection failed: {e}");
                            }
                        }
                        Ok(_) => {}
                        Err(e) => vector_core::log_warn!("[WEBXDC] accept failed: {e}"),
                    }
                });
            }
        });

        Ok(Self { endpoint, gossip, key_bytes, channels: tokio::sync::RwLock::new(HashMap::new()) })
    }

    pub fn node_addr(&self) -> EndpointAddr {
        relay_only(self.endpoint.addr())
    }

    pub async fn has_channel(&self, topic: &TopicId) -> bool {
        self.channels.read().await.contains_key(topic)
    }

    /// Subscribe to a topic, or point an existing subscription at `label`. Events
    /// buffer until a label is attached.
    pub async fn join(&self, topic: TopicId, peers: Vec<EndpointAddr>, label: Option<String>, topic_encoded: String) -> anyhow::Result<()> {
        let mut channels = self.channels.write().await;
        if let Some(ch) = channels.get(&topic) {
            if let Some(label) = label {
                ch.target.lock().unwrap().attach(label);
            }
            return Ok(());
        }

        // Subscribe before dialling, so the topic exists when their messages land.
        let ids: Vec<PublicKey> = peers.iter().map(|p| p.id).collect();
        let sub = self.gossip.subscribe_with_opts(topic, JoinOptions::with_bootstrap(ids)).await?;
        for peer in peers {
            self.dial(peer);
        }
        let (sender, receiver) = sub.split();

        let target = Arc::new(Mutex::new(Target::default()));
        if let Some(label) = label {
            target.lock().unwrap().attach(label);
        }
        let peer_count = Arc::new(AtomicUsize::new(0));
        let subscribe_loop = {
            let (sender, target, key) = (sender.clone(), target.clone(), self.key_bytes);
            // spawn-detached: carries gossip frames for a topic; holds no account state.
            vector_core::rt::spawn(subscribe_loop(receiver, sender, target, peer_count, key, topic_encoded))
        };
        channels.insert(topic, Channel { sender, subscribe_loop, target, seq: AtomicI32::new(0) });
        Ok(())
    }

    fn dial(&self, peer: EndpointAddr) {
        let peer = relay_only(peer);
        if peer.addrs.is_empty() {
            return;
        }
        let (ep, g) = (self.endpoint.clone(), self.gossip.clone());
        // spawn-detached: one outbound dial; gossip owns the connection after.
        vector_core::rt::spawn(async move {
            match ep.connect(peer, GOSSIP_ALPN).await {
                Ok(conn) => {
                    if let Err(e) = g.handle_connection(conn).await {
                        vector_core::log_warn!("[WEBXDC] peer connection failed: {e}");
                    }
                }
                Err(e) => vector_core::log_warn!("[WEBXDC] dial failed: {e}"),
            }
        });
    }

    /// Bring a newly advertised peer into a live channel, retrying 1 s, 2 s apart.
    pub async fn add_peer(&self, topic: TopicId, peer: EndpointAddr) -> anyhow::Result<()> {
        let mut last = None;
        for attempt in 0..3u32 {
            if attempt > 0 {
                vector_core::rt::time::sleep(std::time::Duration::from_secs(1 << (attempt - 1))).await;
            }
            match self.try_add_peer(&topic, &peer).await {
                Ok(()) => return Ok(()),
                Err(e) => last = Some(e),
            }
        }
        Err(last.unwrap_or_else(|| anyhow::anyhow!("peer unreachable")))
    }

    async fn try_add_peer(&self, topic: &TopicId, peer: &EndpointAddr) -> anyhow::Result<()> {
        let mut addr = relay_only(peer.clone());
        // Both ends use the default relays; a peer with none is reachable through ours.
        if addr.addrs.is_empty() {
            if let Some(relay) = self.endpoint.addr().addrs.into_iter().find(|a| matches!(a, TransportAddr::Relay(_))) {
                addr.addrs.insert(relay);
            }
        }
        let conn = self.endpoint.connect(addr, GOSSIP_ALPN).await?;
        self.gossip.handle_connection(conn).await?;
        let channels = self.channels.read().await;
        let ch = channels.get(topic).ok_or_else(|| anyhow::anyhow!("channel closed"))?;
        ch.sender.join_peers(vec![peer.id]).await?;
        Ok(())
    }

    pub async fn send(&self, topic: &TopicId, mut data: Vec<u8>) -> anyhow::Result<()> {
        let sender = {
            let channels = self.channels.read().await;
            let ch = channels.get(topic).ok_or_else(|| anyhow::anyhow!("channel closed"))?;
            let seq = ch.seq.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
            data.reserve(TRAILER_LEN);
            data.extend_from_slice(&seq.to_le_bytes());
            data.extend_from_slice(&self.key_bytes);
            ch.sender.clone()
        };
        sender.broadcast(data.into()).await?;
        Ok(())
    }

    /// Drop every handle on the topic; gossip only forgets a topic once all are gone.
    pub async fn leave(&self, topic: &TopicId) {
        if let Some(ch) = self.channels.write().await.remove(topic) {
            drop(ch.sender);
            ch.subscribe_loop.abort();
            vector_core::rt::yield_now().await;
        }
    }
}

async fn subscribe_loop(
    mut receiver: GossipReceiver,
    sender: GossipSender,
    target: Arc<Mutex<Target>>,
    peer_count: Arc<AtomicUsize>,
    our_key: [u8; PUBLIC_KEY_LENGTH],
    topic_encoded: String,
) {
    let mut connected = false;
    while let Some(event) = receiver.next().await {
        match event {
            Ok(Event::Received(msg)) => {
                let content = &msg.content;
                if content.len() < TRAILER_LEN {
                    continue;
                }
                let payload = content.len() - TRAILER_LEN;
                if content[payload + 4..] == our_key {
                    continue;
                }
                target.lock().unwrap().send(Delivery::Data(content[..payload].to_vec()));
            }
            Ok(Event::NeighborUp(peer)) => {
                // A peer that reached us through the accept loop still needs joining to the topic.
                let _ = sender.join_peers(vec![peer]).await;
                let n = peer_count.fetch_add(1, Ordering::Relaxed) + 1;
                super::emit_status(&topic_encoded, n, true);
                let mut t = target.lock().unwrap();
                if !connected {
                    connected = true;
                    t.send(Delivery::Signal("connected", None));
                }
                t.send(Delivery::Signal("peerJoined", Some(base32_nopad_encode(peer.as_bytes()))));
            }
            Ok(Event::NeighborDown(peer)) => {
                let _ = peer_count.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |c| c.checked_sub(1));
                super::emit_status(&topic_encoded, peer_count.load(Ordering::Relaxed), true);
                target.lock().unwrap().send(Delivery::Signal("peerLeft", Some(base32_nopad_encode(peer.as_bytes()))));
                super::readvertise_later(topic_encoded.clone());
            }
            Ok(Event::Lagged) => target.lock().unwrap().send(Delivery::Signal("lagged", None)),
            Err(e) => vector_core::log_warn!("[WEBXDC] gossip error: {e}"),
        }
    }
}

// ─── Encodings shared with desktop ──────────────────────────────────────────

pub fn base32_nopad_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut out = String::with_capacity((bytes.len() * 8).div_ceil(5));
    let (mut buf, mut bits) = (0u64, 0u32);
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

fn base32_nopad_decode(encoded: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(encoded.len() * 5 / 8);
    let (mut buf, mut bits) = (0u64, 0u32);
    for &c in encoded {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a',
            b'2'..=b'7' => c - b'2' + 26,
            _ => return Err(format!("invalid base32 character {}", c as char)),
        };
        buf = (buf << 5) | v as u64;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    Ok(out)
}

pub fn encode_topic_id(topic: &TopicId) -> String {
    base32_nopad_encode(topic.as_bytes())
}

pub fn decode_topic_id(s: &str) -> Result<TopicId, String> {
    let bytes = base32_nopad_decode(s.as_bytes())?;
    let arr: [u8; 32] = bytes.try_into().map_err(|_| "invalid topic length".to_string())?;
    Ok(TopicId::from_bytes(arr))
}

/// Desktop's fallback topic for an app shared without one.
pub fn derive_topic_id(name: &str, chat_id: &str, message_id: &str) -> TopicId {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(b"webxdc-realtime-v1:");
    h.update(name.as_bytes());
    h.update(b":");
    h.update(chat_id.as_bytes());
    h.update(b":");
    h.update(message_id.as_bytes());
    TopicId::from_bytes(h.finalize().into())
}

pub fn encode_node_addr(addr: &EndpointAddr) -> Result<String, String> {
    let json = serde_json::to_string(addr).map_err(|e| e.to_string())?;
    Ok(base32_nopad_encode(json.as_bytes()))
}

/// Peer-supplied, so reduced to relay paths here: dialling an address a peer
/// nominates would otherwise be theirs to choose.
pub fn decode_node_addr(s: &str) -> Result<EndpointAddr, String> {
    let bytes = base32_nopad_decode(s.as_bytes())?;
    let addr: EndpointAddr = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    Ok(relay_only(addr))
}

pub fn relay_only(addr: EndpointAddr) -> EndpointAddr {
    EndpointAddr { id: addr.id, addrs: addr.addrs.into_iter().filter(|a| matches!(a, TransportAddr::Relay(_))).collect() }
}

fn short_id(id: &PublicKey) -> String {
    id.to_string().chars().take(16).collect()
}

fn relay_urls(addr: &EndpointAddr) -> String {
    let urls: Vec<String> = addr
        .addrs
        .iter()
        .filter_map(|a| match a {
            TransportAddr::Relay(u) => Some(u.to_string()),
            _ => None,
        })
        .collect();
    if urls.is_empty() { "none".into() } else { urls.join(", ") }
}
