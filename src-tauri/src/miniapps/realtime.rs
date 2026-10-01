//! Mini App realtime channels: the app's side of `vector_core::xdc`.
//!
//! The node, the gossip mesh, peer signalling and the session lifecycle are
//! vector-core's (shared with Vector Web and headless clients). What lives here
//! is the window glue: one session per Mini App window, its events delivered to
//! the window (Tauri channel on desktop, mpsc to the WebView on Android),
//! buffered until the app's JS attaches, and the localhost WebSocket fast path
//! for the app's sends.
//!
//! See: https://webxdc.org/docs/spec/joinRealtimeChannel.html

use anyhow::{anyhow, Result};
use fast_thumbhash::base91_encode;
use iroh::EndpointAddr;
pub use iroh_gossip::proto::TopicId;
use std::collections::HashMap;
use std::sync::Arc;
use tauri::ipc::Channel;
use tokio::sync::{oneshot, watch};
use vector_core::xdc::{JoinOptions, XdcEvent, XdcSender, XdcSession};

/// Events sent to the app's JS.
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

/// Target for delivering realtime events (abstracts desktop vs Android)
#[derive(Clone)]
pub enum EventTarget {
    /// Desktop: Tauri IPC channel
    TauriChannel(Channel<RealtimeEvent>),
    /// Android: bounded mpsc sender (delivery task forwards to WebView via JNI)
    #[cfg_attr(not(target_os = "android"), allow(dead_code))]
    MpscSender(tokio::sync::mpsc::Sender<RealtimeEvent>),
}

/// Where a window's events go, and what arrived before its JS attached (the
/// session is joined when the window opens, before the app asks).
pub(crate) struct EventTargetState {
    target: Option<EventTarget>,
    buffer: Vec<RealtimeEvent>,
    /// Optional WebSocket sender for bi-directional WS (bypasses JNI on Android).
    /// When set, Data events are sent directly through WS instead of the normal target.
    ws_sender: Option<tokio::sync::mpsc::Sender<Vec<u8>>>,
    /// The app left: drop events rather than hold or deliver them.
    detached: bool,
}

impl EventTargetState {
    fn new(target: Option<EventTarget>) -> Self {
        Self { target, buffer: Vec::new(), ws_sender: None, detached: false }
    }

    /// Register a WS sender for bi-directional receive.
    pub fn set_ws_sender(&mut self, sender: tokio::sync::mpsc::Sender<Vec<u8>>) {
        self.ws_sender = Some(sender);
    }

    /// Send an event, buffering if no target is set yet.
    fn send(&mut self, event: RealtimeEvent) -> bool {
        if self.detached {
            return false;
        }
        if let Some(ref ws_tx) = self.ws_sender {
            if let RealtimeEvent::Data(ref b91_data) = event {
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
        for event in self.buffer.drain(..) {
            Self::deliver(&target, event);
        }
        self.target = Some(target);
        self.detached = false;
    }

    /// The app left the channel: stop delivering, and forget what was held for it.
    fn detach(&mut self) {
        self.target = None;
        self.buffer.clear();
        self.detached = true;
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

/// Realtime WebSocket senders, keyed by mini app window label.
pub(crate) type WsSenders = Arc<std::sync::RwLock<HashMap<String, tokio::sync::mpsc::Sender<Vec<u8>>>>>;

/// The app's sends from the WebSocket fast path, keyed by window label.
pub(crate) type SendHandles = Arc<std::sync::RwLock<HashMap<String, XdcSender>>>;

/// Deliver through the shared target (read lock on the hot path; write only to buffer).
fn send_event(shared_target: &SharedEventTarget, event: RealtimeEvent) -> bool {
    {
        let guard = shared_target.read().unwrap_or_else(|e| e.into_inner());
        if let Some(ref target) = guard.target {
            return EventTargetState::deliver(target, event);
        }
    }
    shared_target.write().unwrap_or_else(|e| e.into_inner()).send(event)
}

// ─── Window sessions ────────────────────────────────────────────────────────

/// One Mini App window's realtime session: joined once, whoever asks first
/// (the open's preconnect, or the app's own join), and ended with the window.
struct Slot {
    instance_id: u64,
    target: SharedEventTarget,
    /// The join's outcome, once it has one.
    ready: watch::Receiver<JoinOutcome>,
    /// `None` while the join is in flight.
    joined: Option<Joined>,
}

type JoinOutcome = Option<std::result::Result<(), String>>;

/// How long an ask waits on a join already under way: past a departure still
/// publishing from the window's previous session, and a cold node.
const JOIN_WAIT: std::time::Duration = std::time::Duration::from_secs(45);

struct Joined {
    stop: Option<oneshot::Sender<()>>,
    pump: tauri::async_runtime::JoinHandle<()>,
    mesh: Arc<vector_core::xdc::mesh::Mesh>,
}

/// What a window needs to open its session.
pub struct WindowSession<'a> {
    pub label: &'a str,
    pub instance_id: u64,
    pub chat_id: &'a str,
    pub topic: TopicId,
    /// Tell the chat we are playing; off for solo play, which has no chat.
    pub advertise: bool,
}

/// The window glue over vector-core's realtime sessions.
pub struct RealtimeManager {
    /// WebSocket server info (port), set once after WS server starts
    ws_info: std::sync::OnceLock<super::rt_ws::WsInfo>,
    /// Realtime socket tickets, one per Mini App window
    ws_tickets: super::rt_ws::WsTickets,
    /// The app's sends from the WebSocket fast path; populated when a session is joined.
    send_handles: SendHandles,
    /// Map of window_label → WS sender for bi-directional receive.
    pub(crate) ws_senders: WsSenders,
    slots: tokio::sync::Mutex<HashMap<String, Slot>>,
    /// The newest instance closed under each label: a join that starts after its
    /// window closed must not go on to advertise.
    closed: std::sync::Mutex<HashMap<String, u64>>,
}

impl RealtimeManager {
    pub fn new() -> Self {
        Self {
            ws_info: std::sync::OnceLock::new(),
            ws_tickets: Default::default(),
            send_handles: Arc::new(std::sync::RwLock::new(HashMap::new())),
            ws_senders: Arc::new(std::sync::RwLock::new(HashMap::new())),
            slots: tokio::sync::Mutex::new(HashMap::new()),
            closed: std::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Make sure the window's session exists, joining it if nothing has yet,
    /// and deliver its events to `target` when one is given (the app's JS
    /// joining). A window gets one join however many times it asks, and every
    /// ask gets that join's outcome.
    pub async fn open(&self, window: WindowSession<'_>, mut target: Option<EventTarget>) -> Result<()> {
        let label = window.label.to_string();
        enum Step {
            Wait(watch::Receiver<JoinOutcome>),
            Replace(Slot),
            Join(SharedEventTarget, watch::Sender<JoinOutcome>),
        }
        let (target, ready) = loop {
            if self.closed.lock().unwrap_or_else(|e| e.into_inner()).get(&label).is_some_and(|&c| c >= window.instance_id) {
                return Err(anyhow!("the Mini App closed before joining"));
            }
            let step = {
                let mut slots = self.slots.lock().await;
                match slots.get(&label) {
                    Some(slot) if slot.instance_id == window.instance_id => {
                        if let Some(t) = target.take() {
                            slot.target.write().unwrap_or_else(|e| e.into_inner()).set_target(t);
                        }
                        self.wire_ws_receive(&label, &slot.target);
                        Step::Wait(slot.ready.clone())
                    }
                    // A previous window under this label never closed cleanly.
                    Some(_) => Step::Replace(slots.remove(&label).expect("present")),
                    None => {
                        let shared = Arc::new(std::sync::RwLock::new(EventTargetState::new(target.take())));
                        let (tx, rx) = watch::channel(None);
                        slots.insert(label.clone(), Slot { instance_id: window.instance_id, target: shared.clone(), ready: rx, joined: None });
                        Step::Join(shared, tx)
                    }
                }
            };
            match step {
                Step::Join(shared, tx) => break (shared, tx),
                Step::Replace(stale) => self.finish(&label, stale).await,
                Step::Wait(mut rx) => {
                    let outcome = tokio::time::timeout(JOIN_WAIT, rx.wait_for(Option::is_some))
                        .await
                        .map(|r| r.map(|done| done.clone().expect("checked")));
                    match outcome {
                        Ok(Ok(done)) => return done.map_err(|e| anyhow!(e)),
                        Err(_) => return Err(anyhow!("the realtime session is still connecting")),
                        // The join stopped without an outcome: start over.
                        Ok(Err(_)) => {
                            let mut slots = self.slots.lock().await;
                            if slots.get(&label).is_some_and(|s| s.instance_id == window.instance_id && s.joined.is_none()) {
                                slots.remove(&label);
                            }
                        }
                    }
                }
            }
        };
        self.wire_ws_receive(&label, &target);
        let fail = |e: String| {
            ready.send_replace(Some(Err(e.clone())));
            anyhow!(e)
        };

        let topic_b32 = encode_topic_id(&window.topic);
        let joined = match wait_for_platform().await {
            // The app's own consent prompt gates launching a realtime app under Tor.
            Ok(()) => vector_core::xdc::join_with(window.chat_id, &topic_b32, JoinOptions { hold_unnamed: false, advertise: false, outside_tor: true }).await,
            Err(e) => Err(e),
        };
        let session = match joined {
            Ok(s) => s,
            Err(e) => {
                let mut slots = self.slots.lock().await;
                if slots.get(&label).is_some_and(|s| s.instance_id == window.instance_id && s.joined.is_none()) {
                    slots.remove(&label);
                }
                return Err(fail(e));
            }
        };

        let sender = session.sender();
        let mesh = session.mesh();
        let adopted = {
            let mut slots = self.slots.lock().await;
            match slots.get_mut(&label) {
                Some(slot) if slot.instance_id == window.instance_id && slot.joined.is_none() => {
                    let (stop_tx, stop_rx) = oneshot::channel();
                    let pump = tauri::async_runtime::spawn(pump(session, slot.target.clone(), stop_rx));
                    slot.joined = Some(Joined { stop: Some(stop_tx), pump, mesh: mesh.clone() });
                    self.send_handles.write().unwrap_or_else(|e| e.into_inner()).insert(label.clone(), sender.clone());
                    None
                }
                _ => Some(session),
            }
        };
        if let Some(session) = adopted {
            // The window closed while joining.
            session.leave().await;
            retire_unused(&mesh).await;
            return Err(fail("the Mini App closed while joining".into()));
        }
        ready.send_replace(Some(Ok(())));

        if !window.advertise {
            log_info!("[WEBXDC] Joined realtime session for {} (topic {})", label, &topic_b32[..8]);
            return Ok(());
        }
        vector_core::db::spawn_bound(async move {
            if let Err(e) = sender.advertise().await {
                log_warn!("[WEBXDC] Advertisement failed: {e}");
            }
            // A second word a few seconds on: an Android node that just came up can
            // miss peers that dialled during its relay handshake.
            #[cfg(target_os = "android")]
            {
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                let _ = sender.advertise().await;
            }
        });
        log_info!("[WEBXDC] Joined realtime session for {} (topic {})", label, &topic_b32[..8]);
        Ok(())
    }

    /// The app left the channel: its window keeps the session, but nothing
    /// more is delivered or held for it until it joins again.
    pub async fn detach(&self, label: &str) {
        if let Some(slot) = self.slots.lock().await.get(label) {
            slot.target.write().unwrap_or_else(|e| e.into_inner()).detach();
        }
    }

    /// Broadcast one frame from the app.
    pub async fn send(&self, label: &str, data: Vec<u8>) -> Result<()> {
        let sender = self.send_handles.read().unwrap_or_else(|e| e.into_inner()).get(label).cloned();
        let sender = sender.ok_or_else(|| anyhow!("Realtime channel not active"))?;
        sender.send(data).await.map_err(|e| anyhow!(e))
    }

    /// End the window's session (announcing the departure), if it is still the
    /// one `instance_id` opened.
    pub async fn close(&self, label: &str, instance_id: u64) {
        {
            let mut closed = self.closed.lock().unwrap_or_else(|e| e.into_inner());
            let newest = closed.entry(label.to_string()).or_insert(0);
            *newest = (*newest).max(instance_id);
        }
        let slot = {
            let mut slots = self.slots.lock().await;
            match slots.get(label) {
                Some(s) if s.instance_id == instance_id => slots.remove(label),
                _ => None,
            }
        };
        if let Some(slot) = slot {
            self.finish(label, slot).await;
        }
    }

    async fn finish(&self, label: &str, slot: Slot) {
        self.send_handles.write().unwrap_or_else(|e| e.into_inner()).remove(label);
        if let Some(mut joined) = slot.joined {
            if let Some(stop) = joined.stop.take() {
                let _ = stop.send(());
            }
            if tokio::time::timeout(std::time::Duration::from_secs(15), &mut joined.pump).await.is_err() {
                log_warn!("[WEBXDC] Realtime leave for {label} took over 15s");
            }
            retire_unused(&joined.mesh).await;
        }
    }

    /// End every window's session, for an account swap: vector-core leaves them
    /// all as the outgoing account while its client still exists.
    pub async fn end_all(&self) {
        vector_core::xdc::session::leave_all(std::time::Duration::from_secs(4)).await;
        let slots: Vec<(String, Slot)> = self.slots.lock().await.drain().collect();
        for (label, slot) in slots {
            self.finish(&label, slot).await;
        }
    }

    fn wire_ws_receive(&self, label: &str, target: &SharedEventTarget) {
        let map = self.ws_senders.read().unwrap_or_else(|e| e.into_inner());
        if let Some(ws_tx) = map.get(label) {
            target.write().unwrap_or_else(|e| e.into_inner()).set_ws_sender(ws_tx.clone());
        }
    }

    /// Start the WS server if not already running.
    /// Uses std::net::TcpListener for sync bind (no async runtime needed),
    /// then spawns the accept loop on tauri::async_runtime so it survives
    /// any temporary runtime (critical for Android JNI).
    pub fn ensure_ws_started(&self) {
        if self.ws_info.get().is_some() {
            return;
        }

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

        log_info!("[WEBXDC] Realtime WS server listening on 127.0.0.1:{port}");

        let _ = self.ws_info.set(super::rt_ws::WsInfo { port });
        super::scheme::set_rt_ws_port(port);

        let tickets = self.ws_tickets.clone();
        let send_handles = self.send_handles.clone();
        let ws_senders = self.ws_senders.clone();
        tauri::async_runtime::spawn(async move {
            let listener = match tokio::net::TcpListener::from_std(std_listener) {
                Ok(l) => l,
                Err(e) => {
                    log_error!("[WEBXDC] Failed to convert RT WS listener to tokio: {e}");
                    return;
                }
            };
            super::rt_ws::run_accept_loop(listener, tickets, send_handles, ws_senders).await;
        });
    }

    /// The realtime fast-path URL for one Mini App window, if the server is running.
    /// `label` must be the caller's own window, never a value the app supplied.
    pub fn ws_url_for(&self, label: &str) -> Option<String> {
        let port = self.ws_info.get()?.port;
        let mut tickets = self.ws_tickets.write().unwrap_or_else(|e| e.into_inner());
        let ticket = match tickets.iter().find(|(_, owner)| owner.as_str() == label) {
            Some((ticket, _)) => ticket.clone(),
            None => {
                let mut bytes = [0u8; 16];
                use rand::RngCore;
                rand::rngs::OsRng.fill_bytes(&mut bytes);
                let ticket = crate::util::bytes_to_hex_16(&bytes);
                tickets.insert(ticket.clone(), label.to_string());
                ticket
            }
        };
        Some(format!("ws://127.0.0.1:{port}/{ticket}"))
    }
}

impl Default for RealtimeManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Carry one session's events to its window until it ends, or until the window
/// closes (then leave, announcing the departure).
async fn pump(mut session: XdcSession, target: SharedEventTarget, mut stop: oneshot::Receiver<()>) {
    let topic = session.topic().to_string();
    let mut connected = false;
    loop {
        let event = tokio::select! {
            _ = &mut stop => break,
            event = session.recv() => event,
        };
        let Some(event) = event else { return };
        match event {
            XdcEvent::Data(frame) => {
                send_event(&target, RealtimeEvent::Data(base91_encode(&frame.payload)));
            }
            XdcEvent::PeerJoined(peer) => {
                if !connected {
                    connected = true;
                    send_event(&target, RealtimeEvent::Connected);
                }
                send_event(&target, RealtimeEvent::PeerJoined(vector_core::xdc::wire::base32_nopad_encode(&peer.node)));
                emit_neighbors(&topic, session.neighbor_count().await);
            }
            XdcEvent::PeerLeft(peer) => {
                send_event(&target, RealtimeEvent::PeerLeft(vector_core::xdc::wire::base32_nopad_encode(&peer.node)));
                emit_neighbors(&topic, session.neighbor_count().await);
            }
            XdcEvent::Lagged => {
                send_event(&target, RealtimeEvent::Lagged);
            }
        }
    }
    session.leave().await;
}

/// Tell the main window how many peers the session is connected to.
fn emit_neighbors(topic: &str, peer_count: usize) {
    vector_core::traits::emit_event_json(
        "miniapp_realtime_status",
        serde_json::json!({ "topic": topic, "peer_count": peer_count, "is_active": true }),
    );
}

/// Android gives each Mini App session a fresh node: close the mesh once
/// nothing uses it (no session, no join on its way, no call). Desktop keeps one
/// node per account.
async fn retire_unused(mesh: &Arc<vector_core::xdc::mesh::Mesh>) {
    #[cfg(target_os = "android")]
    if vector_core::xdc::mesh::retire_if_idle(mesh).await {
        log_info!("[WEBXDC] Node retired with its last Mini App session");
    }
    #[cfg(not(target_os = "android"))]
    let _ = mesh;
}

/// Hold any first use of the node until the platform can host it.
///
/// Android: iroh's resolver stack (hickory, netdev) reads system config
/// through `ndk_context`, and hickory's reader ABORTS the process when the
/// context isn't registered yet, which tao registers late in startup. The
/// service-only process never registers one, so refuse there (it has no UI for
/// a realtime session anyway).
pub(crate) async fn wait_for_platform() -> Result<(), String> {
    #[cfg(target_os = "android")]
    {
        let mut registered = crate::android::utils::context_registered();
        for _ in 0..50 {
            if registered {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            registered = crate::android::utils::context_registered();
        }
        if !registered {
            return Err("Android context never registered: refusing to start Iroh (service-only process?)".into());
        }
    }
    Ok(())
}

/// The account's node, for calls (they share it with Mini Apps).
pub(crate) async fn mesh() -> Result<Arc<vector_core::xdc::mesh::Mesh>, String> {
    wait_for_platform().await?;
    vector_core::xdc::mesh::mesh().await
}

// ─── Topic and address codecs ────────────────────────────────────────────────

/// The topic for an app whose message carries none (see `xdc::wire::fallback_topic`).
pub fn derive_topic_id(app_name: &str, chat_id: &str, message_id: &str) -> TopicId {
    TopicId::from_bytes(vector_core::xdc::wire::fallback_topic(app_name, chat_id, message_id))
}

/// Encode a topic ID to a string for storage/transmission
pub fn encode_topic_id(topic: &TopicId) -> String {
    vector_core::xdc::wire::encode_topic(topic.as_bytes())
}

/// Decode a topic ID from a string
pub fn decode_topic_id(s: &str) -> Result<TopicId> {
    vector_core::xdc::wire::decode_topic(s).map(TopicId::from_bytes).map_err(|e| anyhow!(e))
}

/// Encode an endpoint address to a string for transmission via Nostr
pub fn encode_node_addr(addr: &EndpointAddr) -> Result<String> {
    vector_core::xdc::wire::encode_node_addr(addr).map_err(|e| anyhow!(e))
}

/// Decode an endpoint address received via Nostr: relay paths only, since the
/// sender chose every address in it and dialling a direct one shows them our IP.
pub fn decode_node_addr(s: &str) -> Result<EndpointAddr> {
    vector_core::xdc::wire::decode_node_addr(s).map_err(|e| anyhow!(e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh::{SecretKey, TransportAddr};

    #[test]
    fn an_app_that_left_is_sent_nothing_from_its_absence() {
        let mut st = EventTargetState::new(None);
        st.send(RealtimeEvent::Lagged);
        st.detach();
        st.send(RealtimeEvent::Data("missed".into()));
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        st.set_target(EventTarget::MpscSender(tx));
        assert!(rx.try_recv().is_err(), "nothing held while it was away");
        st.send(RealtimeEvent::Connected);
        assert!(matches!(rx.try_recv(), Ok(RealtimeEvent::Connected)));
    }

    #[test]
    fn test_topic_id_encoding() {
        let topic = TopicId::from_bytes([3u8; 32]);
        let encoded = encode_topic_id(&topic);
        assert_eq!(decode_topic_id(&encoded).unwrap(), topic);
    }

    #[test]
    fn a_peer_cannot_get_us_to_dial_a_direct_address() {
        let id = SecretKey::from([7u8; 32]).public();
        let relay: iroh::RelayUrl = "https://relay.example./".parse().unwrap();
        let hostile: std::net::SocketAddr = "203.0.113.9:41234".parse().unwrap();
        let mut addrs = std::collections::BTreeSet::new();
        addrs.insert(TransportAddr::Relay(relay.clone()));
        addrs.insert(TransportAddr::Ip(hostile));
        let encoded = encode_node_addr(&EndpointAddr { id, addrs }).unwrap();

        let decoded = decode_node_addr(&encoded).unwrap();
        assert_eq!(decoded.id, id, "the peer is still reachable");
        assert!(decoded.addrs.iter().all(|a| matches!(a, TransportAddr::Relay(_))), "a direct address survived the decode: {:?}", decoded.addrs);
        assert!(decoded.addrs.contains(&TransportAddr::Relay(relay)), "the relay path must survive, or the peer becomes undiallable");
    }

    #[test]
    fn the_app_name_topic_matches_every_other_client() {
        assert_eq!(encode_topic_id(&derive_topic_id("Chess", "npub1chat", "msg1")), "YFLKK3KDFMLO5GJXVLY6CIMNTMMN53BKOYAJ7LXBSW4DOMFWTCXA");
    }
}
