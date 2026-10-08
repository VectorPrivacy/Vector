//! The Iroh gossip mesh realtime sessions ride: one relay-only endpoint for the
//! account using it (a fresh node key per account), one gossip subscription per
//! topic, events out through a channel.
//!
//! Wire-compatible with the app's own realtime layer: the same ALPN, transport
//! tuning, address form and frame trailer, so a headless peer and a player in
//! the app share one channel.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering};
use std::sync::Arc;

use futures_util::StreamExt;
use iroh::endpoint::VarInt;
use iroh::{Endpoint, EndpointAddr, EndpointId, RelayMode, SecretKey, TransportAddr};
use iroh_gossip::api::{Event, GossipReceiver, GossipSender, JoinOptions};
use iroh_gossip::net::{Gossip, GOSSIP_ALPN};
use iroh_gossip::proto::TopicId;
use tokio::sync::{mpsc, watch};

use super::wire::{self, NODE_KEY_LEN};

/// What a subscription reports.
#[derive(Debug, Clone)]
pub enum MeshEvent {
    /// A frame from a peer. `sender` is the node key its trailer names; `direct`
    /// is true when that same node handed it to us over its own authenticated
    /// connection, so the trailer could not have been forged in transit.
    Data { payload: Vec<u8>, sender: [u8; NODE_KEY_LEN], direct: bool },
    NeighborUp([u8; NODE_KEY_LEN]),
    NeighborDown([u8; NODE_KEY_LEN]),
    /// Frames were dropped: the reader fell behind, or gossip did.
    Lagged,
}

pub struct Mesh {
    endpoint: Endpoint,
    gossip: Gossip,
    key: [u8; NODE_KEY_LEN],
    topics: tokio::sync::RwLock<HashMap<TopicId, Topic>>,
    /// Topics whose handles are still being dropped; each turns true once gone.
    closing: Arc<std::sync::Mutex<HashMap<TopicId, watch::Receiver<bool>>>>,
    /// Joins holding the mesh before they have subscribed (see [`lease`]).
    leases: AtomicUsize,
}

/// The mesh, held by a join from the moment it is handed out until its topic is
/// subscribed: [`retire_if_idle`] sees no topic in that gap, and must not close it.
pub struct MeshLease(Arc<Mesh>);

impl MeshLease {
    pub fn mesh(&self) -> &Arc<Mesh> {
        &self.0
    }
}

impl Drop for MeshLease {
    fn drop(&mut self) {
        self.0.leases.fetch_sub(1, Ordering::SeqCst);
    }
}

struct Topic {
    sender: GossipSender,
    pump: crate::rt::JoinHandle<()>,
    seq: AtomicI32,
    neighbors: Arc<AtomicUsize>,
}

/// One dial attempt. A fresh node can spend a few seconds on its relay handshake
/// before the dial even starts.
const ADD_PEER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// The ALPNs the node accepts: gossip, and calls where this build has them.
fn alpns() -> Vec<Vec<u8>> {
    #[allow(unused_mut)]
    let mut alpns = vec![GOSSIP_ALPN.to_vec()];
    #[cfg(feature = "calls")]
    alpns.push(crate::calls::wire::CALL_ALPN.to_vec());
    alpns
}

/// The live mesh and the account (session id) it was bound for. One account's
/// node is never another's: two accounts advertising one node key are linkable.
static MESH: tokio::sync::Mutex<Option<(u64, Arc<Mesh>)>> = tokio::sync::Mutex::const_new(None);

/// The calling account's mesh, bound on first use with a fresh node key. A mesh
/// bound for any other account is closed and replaced, never shared.
pub async fn mesh() -> Result<Arc<Mesh>, String> {
    crate::transport::realtime::check()?;
    acquire(false).await
}

/// [`mesh`], leased until dropped (see [`MeshLease`]).
pub async fn lease() -> Result<MeshLease, String> {
    crate::transport::realtime::check()?;
    acquire(true).await.map(MeshLease)
}

/// [`lease`] for a caller that already holds the user's consent for this network.
pub(crate) async fn lease_vouched() -> Result<MeshLease, String> {
    acquire(true).await.map(MeshLease)
}

async fn acquire(lease: bool) -> Result<Arc<Mesh>, String> {
    let session = crate::db::current_session();
    let mut slot = MESH.lock().await;
    match claim(&mut slot, session.id(), session.is_live()) {
        Claim::Reuse(m) => {
            if lease {
                m.leases.fetch_add(1, Ordering::SeqCst);
            }
            return Ok(m);
        }
        Claim::Refuse => return Err("the account is switching".into()),
        Claim::Bind(previous) => {
            if let Some(old) = previous {
                old.close_detached();
            }
        }
    }
    let m = Arc::new(Mesh::bind().await.map_err(|e| format!("Realtime unavailable: {e}"))?);
    if lease {
        m.leases.fetch_add(1, Ordering::SeqCst);
    }
    *slot = Some((session.id(), m.clone()));
    Ok(m)
}

/// Close `expected` if it is still the live mesh and nothing uses it: no topic,
/// no join on its way to one, no call. For a client that wants a fresh node
/// for each app session. Returns whether it closed.
pub async fn retire_if_idle(expected: &Arc<Mesh>) -> bool {
    let mut slot = MESH.lock().await;
    let idle = match slot.as_ref() {
        Some((_, m)) if Arc::ptr_eq(m, expected) => {
            m.leases.load(Ordering::SeqCst) == 0 && m.topics.read().await.is_empty() && !call_active()
        }
        _ => false,
    };
    if !idle {
        return false;
    }
    if let Some((_, m)) = slot.take() {
        m.close_detached();
    }
    true
}

/// Close the live mesh if nothing uses it: a node left idle keeps its relay link, address
/// probes and DNS going outside the chosen network.
pub async fn retire_live_if_idle() -> bool {
    let live = MESH.lock().await.as_ref().map(|(_, m)| m.clone());
    match live {
        Some(m) => retire_if_idle(&m).await,
        None => false,
    }
}

/// [`retire_live_if_idle`] unless the account is on Clearnet, where one node per account stays:
/// for a call that just ended, whose node nothing else may be using.
pub async fn retire_live_off_clearnet() -> bool {
    if crate::transport::preference() == Some(crate::transport::Kind::Clearnet) {
        return false;
    }
    retire_live_if_idle().await
}

#[cfg(feature = "calls")]
fn call_active() -> bool {
    crate::calls::session::snapshot().is_some()
}

#[cfg(not(feature = "calls"))]
fn call_active() -> bool {
    false
}

/// Close the mesh bound for account `owner`, if it still holds the slot, so the
/// next account binds a fresh node key. Returns at once: the slot may be busy
/// binding, and another account's bind replaces (and closes) it anyway.
pub fn retire(owner: u64) {
    if !crate::rt::can_spawn() {
        return;
    }
    // spawn-detached: closes a node by owner id; reads no account state.
    crate::rt::spawn(async move {
        if let Some(m) = take_owned(&mut *MESH.lock().await, owner) {
            m.close_detached();
        }
    });
}

#[cfg(test)]
pub(crate) async fn install_for_test(owner: u64, mesh: Mesh) -> Arc<Mesh> {
    let mesh = Arc::new(mesh);
    *MESH.lock().await = Some((owner, mesh.clone()));
    mesh
}

/// Serializes the tests that put a mesh in the process-wide slot.
#[cfg(test)]
pub(crate) static SLOT_TESTS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[cfg(test)]
pub(crate) async fn slot_owner() -> Option<u64> {
    MESH.lock().await.as_ref().map(|(owner, _)| *owner)
}

enum Claim<T> {
    Reuse(T),
    /// Bind a fresh one; close the other account's, if any.
    Bind(Option<T>),
    Refuse,
}

/// The slot's rule: an account reuses only its own mesh, and only the live
/// account may bind one.
fn claim<T: Clone>(slot: &mut Option<(u64, T)>, owner: u64, live: bool) -> Claim<T> {
    match slot.as_ref() {
        Some((o, m)) if *o == owner => Claim::Reuse(m.clone()),
        _ if !live => Claim::Refuse,
        _ => Claim::Bind(slot.take().map(|(_, m)| m)),
    }
}

fn take_owned<T>(slot: &mut Option<(u64, T)>, owner: u64) -> Option<T> {
    if slot.as_ref().is_some_and(|(o, _)| *o == owner) {
        slot.take().map(|(_, m)| m)
    } else {
        None
    }
}

impl Mesh {
    async fn bind() -> anyhow_lite::Result<Self> {
        let mesh = Self::build(wire::relay_mode()).await?;
        // An advertisement without our relay is undiallable.
        let _ = crate::rt::time::timeout(std::time::Duration::from_secs(8), mesh.endpoint.online()).await;
        for _ in 0..20 {
            if mesh.endpoint.addr().addrs.iter().any(|a| matches!(a, TransportAddr::Relay(_))) {
                break;
            }
            crate::rt::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        crate::log_info!("[xdc] node {} via {}", short(&mesh.key), wire::relay_urls(&mesh.endpoint.addr()));
        Ok(mesh)
    }

    /// A mesh with no relay, for tests: binds offline and at once.
    #[cfg(test)]
    pub(crate) async fn offline() -> Self {
        Self::build(RelayMode::Disabled).await.expect("bind")
    }

    async fn build(relay_mode: RelayMode) -> anyhow_lite::Result<Self> {
        let mut seed = [0u8; 32];
        rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut seed);
        let secret_key = SecretKey::from(seed);
        let key = *secret_key.public().as_bytes();

        // The app's tuning, so both ends of a channel behave alike.
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
            // Observed-address reports route data onto a path the peer can't reach.
            .send_observed_address_reports(false)
            .receive_observed_address_reports(false)
            .build();

        // No address discovery service: nothing announces this node but its own
        // advertisements, which carry its relay alone. (Iroh itself still trades
        // direct addresses with connected peers.)
        struct RelayOnly(RelayMode);
        impl iroh::endpoint::presets::Preset for RelayOnly {
            fn apply(self, builder: iroh::endpoint::Builder) -> iroh::endpoint::Builder {
                builder.relay_mode(self.0).crypto_provider(Arc::new(rustls::crypto::ring::default_provider()))
            }
        }

        let endpoint = Endpoint::builder(RelayOnly(relay_mode))
            .secret_key(secret_key)
            .alpns(alpns())
            .transport_config(transport)
            .bind()
            .await?;

        let gossip = Gossip::builder().max_message_size(wire::MAX_GOSSIP_MESSAGE).spawn(endpoint.clone());

        let (ep, g) = (endpoint.clone(), gossip.clone());
        // spawn-detached: the endpoint's accept loop; it lives as long as the endpoint and holds no account state.
        crate::rt::spawn(async move {
            while let Some(incoming) = ep.accept().await {
                let g = g.clone();
                // spawn-detached: one inbound handshake, handed to gossip.
                crate::rt::spawn(async move {
                    match incoming.await {
                        Ok(conn) if conn.alpn() == GOSSIP_ALPN => {
                            if let Err(e) = g.handle_connection(conn).await {
                                crate::log_debug!("[xdc] inbound gossip connection ended: {e}");
                            }
                        }
                        #[cfg(feature = "calls")]
                        Ok(conn) if conn.alpn() == crate::calls::wire::CALL_ALPN => {
                            crate::calls::session::on_incoming(conn).await;
                        }
                        Ok(_) => {}
                        Err(e) => crate::log_debug!("[xdc] accept failed: {e}"),
                    }
                });
            }
        });

        Ok(Self {
            endpoint,
            gossip,
            key,
            topics: tokio::sync::RwLock::new(HashMap::new()),
            closing: Arc::new(std::sync::Mutex::new(HashMap::new())),
            leases: AtomicUsize::new(0),
        })
    }

    /// Stop gossip, then close the endpoint, each on its own budget so a peer
    /// that stops reading can't keep the node online. Detached, so neither a
    /// cancelled caller nor a slow peer leaves it half-closed.
    fn close_detached(&self) {
        if !crate::rt::can_spawn() {
            return;
        }
        let (endpoint, gossip) = (self.endpoint.clone(), self.gossip.clone());
        // spawn-detached: closes a node no account uses any more; holds no account state.
        crate::rt::spawn(async move {
            let _ = crate::rt::time::timeout(std::time::Duration::from_secs(2), gossip.shutdown()).await;
            let _ = crate::rt::time::timeout(std::time::Duration::from_secs(4), endpoint.close()).await;
        });
    }

    /// The endpoint itself, for protocols that share the node (calls dial on it).
    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    pub fn node_key(&self) -> [u8; NODE_KEY_LEN] {
        self.key
    }

    /// Our address as an advertisement carries it: relay only.
    pub fn node_addr(&self) -> EndpointAddr {
        wire::relay_only(self.endpoint.addr())
    }

    pub async fn neighbor_count(&self, topic: &[u8; 32]) -> usize {
        self.topics
            .read()
            .await
            .get(&TopicId::from_bytes(*topic))
            .map(|t| t.neighbors.load(Ordering::Relaxed))
            .unwrap_or(0)
    }

    /// Subscribe to a topic and dial `peers`. Events go to `sink` until
    /// [`unsubscribe`](Self::unsubscribe). One subscription per topic per process.
    pub async fn subscribe(&self, topic: [u8; 32], peers: Vec<EndpointAddr>, sink: mpsc::Sender<MeshEvent>) -> Result<(), String> {
        let id = TopicId::from_bytes(topic);
        // A topic still dropping its last handles would share gossip's state with
        // the new one: wait it out.
        let mut topics = loop {
            let topics = self.topics.write().await;
            if topics.contains_key(&id) {
                return Err("already subscribed to this topic".into());
            }
            let pending = self.closing.lock().unwrap_or_else(|e| e.into_inner()).get(&id).cloned();
            match pending {
                None => break topics,
                Some(gone) => {
                    drop(topics);
                    self.await_teardown(id, gone).await;
                }
            }
        };
        // Subscribe before dialling, so the topic exists when their frames land.
        let peers: Vec<EndpointAddr> = peers.into_iter().map(|p| self.reachable(p)).collect();
        let bootstrap: Vec<EndpointId> = peers.iter().map(|p| p.id).collect();
        let sub = self
            .gossip
            .subscribe_with_opts(id, JoinOptions::with_bootstrap(bootstrap))
            .await
            .map_err(|e| e.to_string())?;
        for peer in peers {
            self.dial(peer);
        }
        let (sender, receiver) = sub.split();
        let neighbors = Arc::new(AtomicUsize::new(0));
        // spawn-detached: carries one topic's gossip frames into a channel; no account state.
        let pump = crate::rt::spawn(pump(receiver, sender.clone(), sink, self.key, neighbors.clone()));
        // A random start: gossip dedups by content, and this node outlives a
        // session, so a rejoin counting from zero would repeat old frames byte for byte.
        let seq = AtomicI32::new(rand::random::<i32>());
        topics.insert(id, Topic { sender, pump, seq, neighbors });
        Ok(())
    }

    /// A peer's address as we will dial it ([`wire::dialable`]), through our
    /// relay when none of theirs is left.
    fn reachable(&self, peer: EndpointAddr) -> EndpointAddr {
        let mut addr = wire::dialable(peer);
        if addr.addrs.is_empty() {
            if let Some(relay) = self.endpoint.addr().addrs.into_iter().find(|a| matches!(a, TransportAddr::Relay(_))) {
                addr.addrs.insert(relay);
            }
        }
        addr
    }

    fn dial(&self, peer: EndpointAddr) {
        let peer = self.reachable(peer);
        if peer.addrs.is_empty() {
            return;
        }
        let (ep, g) = (self.endpoint.clone(), self.gossip.clone());
        // spawn-detached: one outbound dial; gossip owns the connection after.
        crate::rt::spawn(async move {
            match ep.connect(peer, GOSSIP_ALPN).await {
                Ok(conn) => {
                    if let Err(e) = g.handle_connection(conn).await {
                        crate::log_debug!("[xdc] peer connection ended: {e}");
                    }
                }
                Err(e) => crate::log_debug!("[xdc] dial failed: {e}"),
            }
        });
    }

    /// Bring a newly advertised peer into a live topic, retrying 1 s, then 2 s apart.
    pub async fn add_peer(&self, topic: &[u8; 32], peer: EndpointAddr) -> Result<(), String> {
        let mut last = String::new();
        for attempt in 0..3u32 {
            if attempt > 0 {
                crate::rt::time::sleep(std::time::Duration::from_secs(1 << (attempt - 1))).await;
            }
            // A dead peer must not hold the caller until QUIC's idle timeout.
            match crate::rt::time::timeout(ADD_PEER_TIMEOUT, self.try_add_peer(topic, &peer)).await {
                Ok(Ok(())) => return Ok(()),
                Ok(Err(e)) => last = e,
                Err(_) => last = "timed out".into(),
            }
        }
        Err(last)
    }

    async fn try_add_peer(&self, topic: &[u8; 32], peer: &EndpointAddr) -> Result<(), String> {
        let addr = self.reachable(peer.clone());
        let conn = self.endpoint.connect(addr, GOSSIP_ALPN).await.map_err(|e| e.to_string())?;
        self.gossip.handle_connection(conn).await.map_err(|e| e.to_string())?;
        let sender = {
            let topics = self.topics.read().await;
            topics.get(&TopicId::from_bytes(*topic)).ok_or("not subscribed")?.sender.clone()
        };
        sender.join_peers(vec![peer.id]).await.map_err(|e| e.to_string())
    }

    pub async fn broadcast(&self, topic: &[u8; 32], payload: Vec<u8>) -> Result<(), String> {
        if payload.len() > wire::MAX_PAYLOAD {
            return Err(format!("frame of {} bytes exceeds the {} byte limit", payload.len(), wire::MAX_PAYLOAD));
        }
        let (sender, frame) = {
            let topics = self.topics.read().await;
            let t = topics.get(&TopicId::from_bytes(*topic)).ok_or("not subscribed")?;
            let seq = t.seq.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
            (t.sender.clone(), wire::seal_frame(payload, seq, &self.key))
        };
        sender.broadcast(frame.into()).await.map_err(|e| e.to_string())
    }

    /// Drop every handle on the topic: gossip only forgets a topic once all are gone,
    /// and a surviving clone would break the next subscription to it. Returns once
    /// they are gone, including those of an unsubscribe already under way; the
    /// teardown finishes even if this call is cancelled.
    pub async fn unsubscribe(&self, topic: &[u8; 32]) {
        let id = TopicId::from_bytes(*topic);
        let gone = {
            let mut topics = self.topics.write().await;
            match topics.remove(&id) {
                Some(t) => {
                    let (done, gone) = watch::channel(false);
                    self.closing.lock().unwrap_or_else(|e| e.into_inner()).insert(id, gone.clone());
                    let closing = self.closing.clone();
                    // spawn-detached: drops one topic's gossip handles; holds no account state.
                    crate::rt::spawn(async move {
                        drop(t.sender);
                        t.pump.abort();
                        let _ = t.pump.await;
                        crate::rt::yield_now().await;
                        // Deregister before signalling: a resubscribe waits on this
                        // signal, so the entry removed is always this teardown's own.
                        closing.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
                        done.send_replace(true);
                    });
                    Some(gone)
                }
                None => self.closing.lock().unwrap_or_else(|e| e.into_inner()).get(&id).cloned(),
            }
        };
        if let Some(gone) = gone {
            self.await_teardown(id, gone).await;
        }
    }

    /// Wait for a topic's teardown. One whose task died unsignalled (its runtime
    /// shut down) is forgotten rather than waited on forever.
    async fn await_teardown(&self, id: TopicId, mut gone: watch::Receiver<bool>) {
        if gone.wait_for(|g| *g).await.is_err() {
            let mut closing = self.closing.lock().unwrap_or_else(|e| e.into_inner());
            if closing.get(&id).is_some_and(|rx| rx.same_channel(&gone)) {
                closing.remove(&id);
            }
        }
    }
}

/// The accept loop holds the endpoint open, so releasing the last handle must close it.
impl Drop for Mesh {
    fn drop(&mut self) {
        self.close_detached();
    }
}

async fn pump(
    mut receiver: GossipReceiver,
    sender: GossipSender,
    sink: mpsc::Sender<MeshEvent>,
    our_key: [u8; NODE_KEY_LEN],
    neighbors: Arc<AtomicUsize>,
) {
    let lagged = AtomicBool::new(false);
    let deliver = |ev: MeshEvent| {
        if lagged.swap(false, Ordering::Relaxed) && sink.try_send(MeshEvent::Lagged).is_err() {
            lagged.store(true, Ordering::Relaxed);
        }
        if sink.try_send(ev).is_err() {
            lagged.store(true, Ordering::Relaxed);
        }
    };
    while let Some(event) = receiver.next().await {
        if sink.is_closed() {
            break;
        }
        match event {
            Ok(Event::Received(msg)) => {
                let Some((payload, sender_key)) = wire::open_frame(&msg.content) else { continue };
                if sender_key == our_key {
                    continue;
                }
                let direct = msg.scope.is_direct() && *msg.delivered_from.as_bytes() == sender_key;
                deliver(MeshEvent::Data { payload: payload.to_vec(), sender: sender_key, direct });
            }
            Ok(Event::NeighborUp(peer)) => {
                // A peer that reached us through the accept loop still needs joining to the topic.
                let _ = sender.join_peers(vec![peer]).await;
                neighbors.fetch_add(1, Ordering::Relaxed);
                deliver(MeshEvent::NeighborUp(*peer.as_bytes()));
            }
            Ok(Event::NeighborDown(peer)) => {
                let _ = neighbors.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |c| c.checked_sub(1));
                deliver(MeshEvent::NeighborDown(*peer.as_bytes()));
            }
            Ok(Event::Lagged) => deliver(MeshEvent::Lagged),
            Err(e) => crate::log_debug!("[xdc] gossip error: {e}"),
        }
    }
}

pub(crate) fn short(key: &[u8; NODE_KEY_LEN]) -> String {
    crate::simd::hex::bytes_to_hex_string(&key[..6])
}

/// `?` over iroh's and noq's error types without pulling in anyhow.
mod anyhow_lite {
    pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_account_never_gets_another_accounts_mesh() {
        let mut slot = Some((1u64, "a"));
        assert!(matches!(claim(&mut slot, 1, true), Claim::Reuse("a")));
        assert!(matches!(claim(&mut slot, 1, false), Claim::Reuse("a")), "its own, even mid-swap");
        assert!(matches!(claim(&mut slot, 2, false), Claim::Refuse), "a swapped-out account binds nothing");
        assert_eq!(slot, Some((1, "a")));
        assert!(matches!(claim(&mut slot, 2, true), Claim::Bind(Some("a"))), "the next account replaces it");
        assert_eq!(slot, None);
        assert!(matches!(claim(&mut slot, 2, true), Claim::Bind(None)));
    }

    #[test]
    fn retiring_closes_only_the_named_accounts_mesh() {
        let mut slot = Some((2u64, "b"));
        assert_eq!(take_owned(&mut slot, 1), None);
        assert_eq!(slot, Some((2, "b")));
        assert_eq!(take_owned(&mut slot, 2), Some("b"));
        assert_eq!(slot, None);
    }

    #[tokio::test]
    async fn an_unsubscribe_cut_off_half_way_still_drops_the_topic_before_a_retry_returns() {
        let mesh = Mesh::offline().await;
        let topic = [7u8; 32];
        let (tx, mut rx) = mpsc::channel(4);
        mesh.subscribe(topic, Vec::new(), tx).await.unwrap();
        // One poll: the entry is removed, then the call is dropped mid-teardown.
        assert!(futures_util::FutureExt::now_or_never(mesh.unsubscribe(&topic)).is_none());
        mesh.unsubscribe(&topic).await;
        assert!(
            matches!(rx.try_recv(), Err(mpsc::error::TryRecvError::Disconnected)),
            "the pump's handles are gone once the retry returns"
        );
        let (tx, _rx) = mpsc::channel(4);
        mesh.subscribe(topic, Vec::new(), tx).await.expect("the topic can be joined again");
        mesh.unsubscribe(&topic).await;
    }

    #[tokio::test]
    async fn releasing_a_mesh_closes_its_node() {
        let mesh = Mesh::offline().await;
        let endpoint = mesh.endpoint.clone();
        drop(mesh);
        for _ in 0..100 {
            if endpoint.is_closed() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("the endpoint outlived its mesh");
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn a_call_off_clearnet_takes_its_idle_node_with_it() {
        let _db = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let _serial = SLOT_TESTS.lock().await;
        let before = crate::transport::preference();
        let owner = crate::db::live_session_id();
        let mesh = install_for_test(owner, Mesh::offline().await).await;
        crate::transport::set_preference(Some(crate::transport::Kind::Clearnet));
        assert!(!retire_live_off_clearnet().await, "Clearnet keeps the account's node");
        assert_eq!(slot_owner().await, Some(owner));
        crate::transport::set_preference(Some(crate::transport::Kind::Tor));
        let (tx, _rx) = mpsc::channel(4);
        mesh.subscribe([8u8; 32], Vec::new(), tx).await.unwrap();
        assert!(!retire_live_off_clearnet().await, "a Mini App session still on it keeps it");
        mesh.unsubscribe(&[8u8; 32]).await;
        assert!(retire_live_off_clearnet().await, "off Clearnet the idle node goes");
        assert_eq!(slot_owner().await, None);
        crate::transport::set_preference(before);
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn a_mesh_is_retired_only_once_nothing_uses_it() {
        // The live session's network decides whether a lease may open a node at all.
        let _db = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let _serial = SLOT_TESTS.lock().await;
        crate::db::with_session(crate::db::current_session(), async {
            let owner = crate::db::current_session().id();
            let mesh = install_for_test(owner, Mesh::offline().await).await;
            let lease = lease().await.expect("the caller's own mesh");
            assert!(Arc::ptr_eq(lease.mesh(), &mesh));
            assert!(!retire_if_idle(&mesh).await, "a join between handing out the mesh and subscribing holds it");
            drop(lease);
            let (tx, _rx) = mpsc::channel(4);
            mesh.subscribe([9u8; 32], Vec::new(), tx).await.unwrap();
            assert!(!retire_if_idle(&mesh).await, "a subscribed topic holds it");
            mesh.unsubscribe(&[9u8; 32]).await;
            assert!(retire_if_idle(&mesh).await, "idle, it closes");
            assert_eq!(slot_owner().await, None);
            let successor = install_for_test(owner, Mesh::offline().await).await;
            assert!(!retire_if_idle(&mesh).await, "a stale close must not take its successor");
            assert_eq!(slot_owner().await, Some(owner));
            assert!(retire_if_idle(&successor).await);
        })
        .await;
    }
}
