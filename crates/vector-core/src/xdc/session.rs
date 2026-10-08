//! A joined realtime session: one Mini App instance's gossip topic, the
//! players on it, and the Nostr signalling that lets them find each other.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::{mpsc, watch};

use super::mesh::{self, Mesh, MeshEvent};
use super::signal::{self, Signal};
use super::wire::{self, NODE_KEY_LEN};

/// Frames and events queued per session before the oldest are dropped as `Lagged`.
const QUEUE: usize = 256;
/// Nodes a session will attribute; past this, newcomers stay anonymous.
const ROSTER_CAP: usize = 256;
/// Advertisements dialled at join, newest first.
const BOOTSTRAP_CAP: usize = 32;
const READVERTISE_DELAY: Duration = Duration::from_secs(2);
const READVERTISE_MIN_GAP: Duration = Duration::from_secs(10);
const LEAVE_WAIT: Duration = Duration::from_secs(15);
/// How long a node's frames wait for an advertisement to name their sender.
/// The advertisement travels over Nostr and the first frames straight over the
/// mesh, so they usually arrive first.
pub const ATTRIBUTION_WAIT: Duration = Duration::from_secs(10);
/// Frames held per unnamed node, unnamed nodes held at once, and bytes held in all.
const HELD_FRAMES: usize = 64;
const HELD_NODES: usize = 32;
const HELD_BYTES: usize = 4 * 1024 * 1024;

/// A peer on the mesh. `npub` is the account whose advertisement first named
/// this node in this chat; `None` until one has, or once two accounts have
/// claimed the same node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XdcPeer {
    pub node: [u8; NODE_KEY_LEN],
    pub npub: Option<String>,
}

/// One frame an app (or another bot) sent on the channel.
#[derive(Debug, Clone)]
pub struct XdcFrame {
    pub payload: Vec<u8>,
    pub from: XdcPeer,
    /// The frame came straight from `from.node` over that node's own
    /// authenticated connection. Otherwise it was relayed, and the sender named
    /// in its trailer is the sender's claim.
    pub direct: bool,
}

impl XdcFrame {
    /// The payload as UTF-8 text, if it is.
    pub fn text(&self) -> Option<&str> {
        std::str::from_utf8(&self.payload).ok()
    }

    /// The payload parsed as JSON, the encoding nearly every app uses
    /// (`channel.send(new TextEncoder().encode(JSON.stringify(msg)))`).
    pub fn json<T: serde::de::DeserializeOwned>(&self) -> Option<T> {
        serde_json::from_slice(&self.payload).ok()
    }

    /// The sender's npub, when you can act on it: the frame came straight from
    /// the sender's device, and only one account in this chat has claimed that
    /// device. Use it for anything someone could fake (a move, a vote).
    ///
    /// `None` when another player passed the frame on, or its sender isn't
    /// known. A device counts as the first account in the chat to announce it.
    pub fn verified_sender(&self) -> Option<&str> {
        self.direct.then_some(self.from.npub.as_deref()).flatten()
    }
}

#[derive(Debug, Clone)]
pub enum XdcEvent {
    Data(XdcFrame),
    /// A node connected to us on this topic.
    PeerJoined(XdcPeer),
    /// A node dropped off, or a player announced they left.
    PeerLeft(XdcPeer),
    /// Frames were dropped: the reader fell behind, or more arrived from an
    /// unnamed sender than the session holds while it waits for their name.
    Lagged,
}

/// Who a node speaks for. The first advertisement binds it; a second account
/// naming the same node makes it anonymous rather than letting the claim move.
#[derive(Clone)]
enum Binding {
    Npub(String),
    Contested,
}

/// What every holder of a session (the handle, its pump, the signal router) shares.
struct Shared {
    topic: [u8; 32],
    /// Canonical base32: one spelling, whatever the caller passed.
    topic_b32: String,
    chat_id: String,
    /// The account that joined. Background work runs as it, never as whoever is
    /// logged in by then.
    session: Arc<crate::db::Session>,
    /// The account's mesh the topic is subscribed on.
    mesh: Arc<Mesh>,
    roster: Mutex<HashMap<[u8; NODE_KEY_LEN], Binding>>,
    /// Nodes reported joined and not yet left, so each join pairs with one leave
    /// whether gossip or a Nostr departure reports it first.
    present: Mutex<HashSet<[u8; NODE_KEY_LEN]>>,
    /// The reader's channel, for events that arrive over Nostr rather than the
    /// mesh. Taken on leave, so the reader sees the session end.
    events: Mutex<Option<mpsc::Sender<XdcEvent>>>,
    /// Set when leaving starts; the session takes no new peers or sends after.
    closed: AtomicBool,
    /// Turns true once leaving has finished, departure signal included.
    done: watch::Sender<bool>,
    readvertise: Mutex<ReadvertiseState>,
    /// Signalled when an advertisement names a node, so its held frames go out.
    claimed: tokio::sync::Notify,
    /// See [`JoinOptions::hold_unnamed`].
    hold_unnamed: bool,
    /// We have advertised on the topic: only then is a departure owed, or a
    /// re-advertisement wanted.
    advertised: AtomicBool,
}

#[derive(Default)]
struct ReadvertiseState {
    pending: bool,
    last: Option<crate::rt::time::Instant>,
}

impl Shared {
    fn npub(&self, node: &[u8; NODE_KEY_LEN]) -> Option<String> {
        match self.roster.lock().unwrap().get(node) {
            Some(Binding::Npub(n)) => Some(n.clone()),
            _ => None,
        }
    }

    fn peer(&self, node: [u8; NODE_KEY_LEN]) -> XdcPeer {
        XdcPeer { node, npub: self.npub(&node) }
    }

    fn bind(&self, node: [u8; NODE_KEY_LEN], npub: &str) {
        let mut roster = self.roster.lock().unwrap();
        match roster.get(&node) {
            None if roster.len() >= ROSTER_CAP => {}
            None => {
                roster.insert(node, Binding::Npub(npub.to_string()));
            }
            Some(Binding::Npub(n)) if n != npub => {
                crate::log_warn!("[xdc] node {} claimed by two accounts; leaving it unattributed", mesh::short(&node));
                roster.insert(node, Binding::Contested);
            }
            Some(_) => {}
        }
    }

    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Relaxed)
    }

    /// Whether an advertisement may still name this node: none has, and the
    /// roster has room for it.
    fn awaits_claim(&self, node: &[u8; NODE_KEY_LEN]) -> bool {
        let roster = self.roster.lock().unwrap();
        !roster.contains_key(node) && roster.len() < ROSTER_CAP
    }
}

/// The account's joined sessions, by topic. Lives on the session: a swapped-out
/// account's games are not this account's.
#[derive(Default)]
struct Joined {
    map: Mutex<HashMap<[u8; 32], Arc<Shared>>>,
    /// Set when the account's sessions are torn down for a swap. A join still in
    /// flight then must not register (it would escape the teardown and keep a node
    /// the next account inherits).
    sealed: AtomicBool,
}

fn joined_of(session: &Arc<crate::db::Session>) -> Arc<Joined> {
    session.scoped::<Joined, _>()
}

fn joined() -> Arc<Joined> {
    joined_of(&crate::db::current_session())
}

/// Register a joined session, unless the account's sessions have been sealed for
/// a swap. Under the lock the teardown seals with: either the teardown sees this
/// session, or this sees the seal.
fn register(registry: &Joined, shared: &Arc<Shared>) -> bool {
    let mut map = registry.map.lock().unwrap();
    if registry.sealed.load(Ordering::SeqCst) {
        return false;
    }
    map.insert(shared.topic, shared.clone());
    true
}

fn live_session(topic: &[u8; 32]) -> Option<Arc<Shared>> {
    joined().map.lock().unwrap().get(topic).filter(|s| !s.is_closed()).cloned()
}

/// Whether the live account is in any realtime session.
pub fn any_live() -> bool {
    joined_of(&crate::db::live_session()).map.lock().unwrap().values().any(|s| !s.is_closed())
}

/// Whether this account is in the session on `topic` (base32), and not leaving it.
pub fn is_joined(topic: &str) -> bool {
    wire::decode_topic(topic).map(|t| live_session(&t).is_some()).unwrap_or(false)
}

/// Let sessions run outside the chosen network (Tor, I2P). Iroh connects directly, so session
/// peers and the Iroh relay see this machine's IP; the app asks the user the same question.
pub fn allow_outside_transport(allow: bool) {
    crate::transport::realtime::allow_always(allow);
}

/// [`allow_outside_transport`], under its original name.
pub fn allow_outside_tor(allow: bool) {
    allow_outside_transport(allow);
}

/// Run `fut` as the account that joined, in its own task.
fn spawn_as<F>(session: &Arc<crate::db::Session>, fut: F)
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    crate::db::spawn_bound(crate::db::with_session(session.clone(), fut));
}

/// A live realtime session. Dropping it leaves.
pub struct XdcSession {
    shared: Arc<Shared>,
    rx: mpsc::Receiver<XdcEvent>,
    left: bool,
}

/// How a session is joined.
#[derive(Debug, Clone)]
pub struct JoinOptions {
    /// Hold a node's frames until an advertisement names its sender (see
    /// [`XdcFrame::verified_sender`]). An app window has no use for the sender,
    /// so it takes frames as they come.
    pub hold_unnamed: bool,
    /// Advertise as part of joining. Off for a caller that advertises later
    /// ([`XdcSession::advertise`]) or never (an app with nobody to play with).
    /// A session that never advertised leaves without a departure signal.
    pub advertise: bool,
    /// The caller vouches that the user accepted a session outside the chosen network (Tor,
    /// I2P). Otherwise the account's consent, or the process-wide
    /// [`allow_outside_transport`], decides.
    pub outside_tor: bool,
}

impl Default for JoinOptions {
    fn default() -> Self {
        JoinOptions { hold_unnamed: true, advertise: true, outside_tor: false }
    }
}

/// Join the session on `topic` (the `webxdc-topic` an app's message carries)
/// in `chat_id` (a DM npub or Community channel id): dial everyone already
/// advertised there, then advertise ourselves so the rest can dial us.
pub async fn join(chat_id: &str, topic: &str) -> Result<XdcSession, String> {
    join_with(chat_id, topic, JoinOptions::default()).await
}

/// [`join`] with options.
pub async fn join_with(chat_id: &str, topic: &str, options: JoinOptions) -> Result<XdcSession, String> {
    // Everything below writes and signs as the account that called, even if
    // another logs in while this awaits.
    crate::db::scoped(join_inner(chat_id.to_string(), topic.to_string(), options)).await
}

async fn join_inner(chat_id: String, topic: String, options: JoinOptions) -> Result<XdcSession, String> {
    let session = crate::db::current_session();
    let registry = joined_of(&session);
    let switching = || Err::<XdcSession, String>("the account is switching".into());
    if registry.sealed.load(Ordering::SeqCst) {
        return switching();
    }
    let topic_bytes = wire::decode_topic(&topic)?;
    let topic_b32 = wire::encode_topic(&topic_bytes);
    if !options.outside_tor {
        crate::transport::realtime::check()?;
    }
    // A task still bound to an account that has since swapped out must not start one.
    if !session.is_live() {
        return Err("account changed before the session could start".into());
    }
    let me = crate::state::my_public_key()
        .and_then(|pk| nostr_sdk::prelude::ToBech32::to_bech32(&pk).ok())
        .ok_or("Not logged in")?;

    // A session still leaving this topic finishes first, so its departure
    // signal can't land after (and erase) our new advertisement.
    let prior = registry.map.lock().unwrap().get(&topic_bytes).cloned();
    if let Some(prior) = prior {
        if !prior.is_closed() {
            return Err("already in this session".into());
        }
        let mut done = prior.done.subscribe();
        let _ = crate::rt::time::timeout(LEAVE_WAIT, done.wait_for(|d| *d)).await;
    }

    if registry.sealed.load(Ordering::SeqCst) {
        return switching();
    }
    let mut unregistered = Unregistered { registry: registry.clone(), owner: session.id(), armed: true };
    // Leased until the topic is subscribed, so a retire can't close it in between.
    let lease = mesh::lease_vouched().await?;
    let mesh = lease.mesh().clone();
    let addr = wire::encode_node_addr(&mesh.node_addr())?;
    let (tx, rx) = mpsc::channel(QUEUE);
    let shared = Arc::new(Shared {
        topic: topic_bytes,
        topic_b32: topic_b32.clone(),
        chat_id: chat_id.clone(),
        session: session.clone(),
        mesh: mesh.clone(),
        roster: Mutex::new(HashMap::new()),
        present: Mutex::new(HashSet::new()),
        events: Mutex::new(Some(tx.clone())),
        closed: AtomicBool::new(false),
        done: watch::channel(false).0,
        readvertise: Mutex::new(ReadvertiseState::default()),
        claimed: tokio::sync::Notify::new(),
        hold_unnamed: options.hold_unnamed,
        advertised: AtomicBool::new(false),
    });

    // Only this chat's advertisements: a signal persisted from any other chat
    // (a stranger's DM, a channel that banned its sender) is no player here.
    let mut bootstrap = Vec::new();
    for ad in crate::db::miniapps::get_active_peer_advertisements_in(&topic_b32, &chat_id, &me, BOOTSTRAP_CAP).unwrap_or_default() {
        if let Ok(peer) = wire::decode_node_addr(&ad.node_addr_encoded) {
            shared.bind(*peer.id.as_bytes(), &ad.npub);
            bootstrap.push(peer);
        }
    }
    crate::log_info!("[xdc] joining {} in {} with {} known peer(s)", &topic_b32[..8], short_chat(&chat_id), bootstrap.len());

    let (mesh_tx, mesh_rx) = mpsc::channel(QUEUE);
    mesh.subscribe(topic_bytes, bootstrap, mesh_tx).await?;
    drop(lease);
    if !register(&registry, &shared) {
        mesh.unsubscribe(&topic_bytes).await;
        return Err("the account switched while joining".into());
    }
    unregistered.armed = false;
    spawn_as(&session, pump(shared.clone(), mesh_rx, tx));
    // The handle exists before the advertisement goes out, so a join dropped
    // while publishing still leaves.
    let joined = XdcSession { shared, rx, left: false };
    if options.advertise {
        if let Err(e) = advertise(&joined.shared, &addr).await {
            crate::log_warn!("[xdc] advertisement failed, peers must dial us from an older one: {e}");
        }
    }
    Ok(joined)
}

/// Held by a join until its session is registered. A join of a sealed account
/// that stops before then (backed out, failed or cancelled) may have bound the
/// mesh after the teardown retired it, so it retires it again.
struct Unregistered {
    registry: Arc<Joined>,
    owner: u64,
    armed: bool,
}

impl Drop for Unregistered {
    fn drop(&mut self) {
        if self.armed && self.registry.sealed.load(Ordering::SeqCst) {
            mesh::retire(self.owner);
        }
    }
}

/// Translate mesh events for the reader, and re-advertise when a neighbor drops
/// so a peer that reconnects with a fresh node can find us again. A node's
/// events wait (see [`Held`]) until an advertisement names who sent them.
async fn pump(shared: Arc<Shared>, mut mesh_rx: mpsc::Receiver<MeshEvent>, tx: mpsc::Sender<XdcEvent>) {
    let mut held = Held::default();
    loop {
        let wake = held.deadline();
        let ev = tokio::select! {
            ev = mesh_rx.recv() => match ev {
                Some(ev) => Some(ev),
                None => break,
            },
            _ = shared.claimed.notified() => None,
            _ = crate::rt::time::sleep_until(wake.unwrap_or_else(crate::rt::time::Instant::now)), if wake.is_some() => None,
        };
        for out in held.release(&shared, crate::rt::time::Instant::now()) {
            if tx.send(out).await.is_err() {
                return;
            }
        }
        let Some(ev) = ev else { continue };
        let out = match ev {
            MeshEvent::Data { payload, sender, direct } => match held.hold_frame(&shared, sender, payload, direct) {
                Some(payload) => XdcEvent::Data(XdcFrame { payload, from: shared.peer(sender), direct }),
                None => continue,
            },
            MeshEvent::NeighborUp(node) => {
                if held.hold_arrival(&shared, node) || !shared.present.lock().unwrap().insert(node) {
                    continue;
                }
                XdcEvent::PeerJoined(shared.peer(node))
            }
            MeshEvent::NeighborDown(node) => {
                readvertise_later(shared.clone());
                held.forget_arrival(&node);
                if !shared.present.lock().unwrap().remove(&node) {
                    continue;
                }
                XdcEvent::PeerLeft(shared.peer(node))
            }
            MeshEvent::Lagged => XdcEvent::Lagged,
        };
        if tx.send(out).await.is_err() {
            break;
        }
    }
}

/// Events from nodes no advertisement has named yet. They are held until one
/// does, then delivered in order with their sender, so a frame is never handed
/// over anonymous when its sender was about to be provable. A node still
/// unnamed after [`ATTRIBUTION_WAIT`] is delivered anonymous from then on.
#[derive(Default)]
struct Held {
    nodes: Vec<HeldNode>,
    waited: HashSet<[u8; NODE_KEY_LEN]>,
    bytes: usize,
}

struct HeldNode {
    node: [u8; NODE_KEY_LEN],
    since: crate::rt::time::Instant,
    arrived: bool,
    frames: Vec<(Vec<u8>, bool)>,
    dropped: bool,
}

impl Held {
    /// The node's hold, opened only for `opens` events: a node connected to us,
    /// or a frame straight from it. A relayed frame names its sender unproven,
    /// and anyone could fill every slot with made-up ones.
    fn entry(&mut self, shared: &Shared, node: [u8; NODE_KEY_LEN], opens: bool) -> Option<&mut HeldNode> {
        if let Some(i) = self.nodes.iter().position(|h| h.node == node) {
            return Some(&mut self.nodes[i]);
        }
        if !shared.hold_unnamed || !opens || self.waited.contains(&node) || self.nodes.len() >= HELD_NODES || !shared.awaits_claim(&node) {
            return None;
        }
        self.nodes.push(HeldNode { node, since: crate::rt::time::Instant::now(), arrived: false, frames: Vec::new(), dropped: false });
        self.nodes.last_mut()
    }

    /// Hold a frame, or hand its payload back to deliver now.
    fn hold_frame(&mut self, shared: &Shared, node: [u8; NODE_KEY_LEN], payload: Vec<u8>, direct: bool) -> Option<Vec<u8>> {
        let room = HELD_BYTES.saturating_sub(self.bytes);
        let Some(h) = self.entry(shared, node, direct) else { return Some(payload) };
        if h.frames.len() < HELD_FRAMES && payload.len() <= room {
            let len = payload.len();
            h.frames.push((payload, direct));
            self.bytes += len;
        } else {
            h.dropped = true;
        }
        None
    }

    fn hold_arrival(&mut self, shared: &Shared, node: [u8; NODE_KEY_LEN]) -> bool {
        self.entry(shared, node, true).map(|h| h.arrived = true).is_some()
    }

    fn forget_arrival(&mut self, node: &[u8; NODE_KEY_LEN]) {
        if let Some(h) = self.nodes.iter_mut().find(|h| h.node == *node) {
            h.arrived = false;
        }
    }

    fn deadline(&self) -> Option<crate::rt::time::Instant> {
        self.nodes.iter().map(|h| h.since + ATTRIBUTION_WAIT).min()
    }

    /// Everything now named, or out of time, in the order it arrived.
    fn release(&mut self, shared: &Shared, now: crate::rt::time::Instant) -> Vec<XdcEvent> {
        let mut out = Vec::new();
        let mut kept = Vec::new();
        for h in std::mem::take(&mut self.nodes) {
            let named = !shared.awaits_claim(&h.node);
            if !named && now < h.since + ATTRIBUTION_WAIT {
                kept.push(h);
                continue;
            }
            if !named {
                if self.waited.len() >= ROSTER_CAP {
                    self.waited.clear();
                }
                self.waited.insert(h.node);
            }
            self.bytes -= h.frames.iter().map(|(p, _)| p.len()).sum::<usize>();
            let from = shared.peer(h.node);
            if h.arrived && shared.present.lock().unwrap().insert(h.node) {
                out.push(XdcEvent::PeerJoined(from.clone()));
            }
            out.extend(h.frames.into_iter().map(|(payload, direct)| XdcEvent::Data(XdcFrame { payload, from: from.clone(), direct })));
            if h.dropped {
                out.push(XdcEvent::Lagged);
            }
        }
        self.nodes = kept;
        out
    }
}

/// One pending re-advertisement per session, at most one per `READVERTISE_MIN_GAP`:
/// a peer flapping its connection must not set our publish rate.
fn readvertise_later(shared: Arc<Shared>) {
    if !shared.advertised.load(Ordering::SeqCst) {
        return;
    }
    let delay = {
        let mut st = shared.readvertise.lock().unwrap();
        if st.pending {
            return;
        }
        st.pending = true;
        let since = st.last.map(|t| t.elapsed()).unwrap_or(READVERTISE_MIN_GAP);
        READVERTISE_DELAY.max(READVERTISE_MIN_GAP.saturating_sub(since))
    };
    let session = shared.session.clone();
    spawn_as(&session, async move {
        crate::rt::time::sleep(delay).await;
        {
            let mut st = shared.readvertise.lock().unwrap();
            st.pending = false;
            st.last = Some(crate::rt::time::Instant::now());
        }
        if shared.is_closed() {
            return;
        }
        if let Ok(addr) = wire::encode_node_addr(&shared.mesh.node_addr()) {
            let _ = advertise(&shared, &addr).await;
        }
    });
}

impl XdcSession {
    /// The session's topic, base32, as the app's message carries it.
    pub fn topic(&self) -> &str {
        &self.shared.topic_b32
    }

    /// The DM npub or Community channel id the session is played in.
    pub fn chat_id(&self) -> &str {
        &self.shared.chat_id
    }

    /// Broadcast a frame to everyone on the channel, at most 128 000 bytes.
    /// Like the app's own `send`, delivery is best-effort and never echoes back.
    pub async fn send(&self, payload: impl Into<Vec<u8>>) -> Result<(), String> {
        self.sender().send(payload).await
    }

    /// The next event, or `None` once the session has ended.
    pub async fn recv(&mut self) -> Option<XdcEvent> {
        self.rx.recv().await
    }

    /// Who a node speaks for, by the advertisement that first named it here.
    pub fn npub_of(&self, node: &[u8; NODE_KEY_LEN]) -> Option<String> {
        self.shared.npub(node)
    }

    /// The mesh the session runs on (see [`mesh::retire_if_idle`]).
    pub fn mesh(&self) -> Arc<Mesh> {
        self.shared.mesh.clone()
    }

    /// How many nodes are directly connected to us on this topic.
    pub async fn neighbor_count(&self) -> usize {
        self.shared.mesh.neighbor_count(&self.shared.topic).await
    }

    /// A handle that sends on this session from another task; it stops working
    /// once the session is left.
    pub fn sender(&self) -> XdcSender {
        XdcSender { shared: self.shared.clone() }
    }

    /// Resolves once this session has fully left, departure signal included,
    /// however it ends (dropped, left, or torn down with its account).
    pub fn finished(&self) -> impl std::future::Future<Output = ()> + Send + 'static {
        let mut done = self.shared.done.subscribe();
        async move {
            let _ = done.wait_for(|d| *d).await;
        }
    }

    /// Start leaving without waiting: [`recv`](Self::recv) returns `None` from
    /// then on, and [`finished`](Self::finished) resolves once it is done.
    pub fn end(&mut self) {
        if self.left || !crate::rt::can_spawn() {
            return;
        }
        self.left = true;
        let shared = self.shared.clone();
        spawn_as(&shared.session.clone(), leave(shared, true));
    }

    /// Advertise our node on this session's topic again, for a session joined
    /// without advertising ([`JoinOptions::advertise`]) or to be found anew.
    pub async fn advertise(&self) -> Result<(), String> {
        let addr = wire::encode_node_addr(&self.shared.mesh.node_addr())?;
        crate::db::with_session(self.shared.session.clone(), advertise(&self.shared, &addr)).await
    }

    /// Leave: drop the subscription and tell the chat we went.
    pub async fn leave(mut self) {
        self.left = true;
        let shared = self.shared.clone();
        crate::db::with_session(shared.session.clone(), leave(shared, true)).await;
    }
}

/// Marks a session finished when dropped: unsubscribed, deregistered, and
/// `done` set for anyone waiting on it. Held across every await of a leave, so
/// a leave cancelled half-way (an account swap's deadline) still never strands
/// a waiter or leaves the topic subscribed.
struct Finish {
    shared: Arc<Shared>,
    /// The topic may still be subscribed.
    subscribed: bool,
}

impl Finish {
    fn complete(shared: &Arc<Shared>) {
        shared.events.lock().unwrap_or_else(|e| e.into_inner()).take();
        {
            let reg = joined_of(&shared.session);
            let mut map = reg.map.lock().unwrap_or_else(|e| e.into_inner());
            if map.get(&shared.topic).is_some_and(|s| Arc::ptr_eq(s, shared)) {
                map.remove(&shared.topic);
            }
        }
        shared.done.send_replace(true);
    }
}

impl Drop for Finish {
    fn drop(&mut self) {
        if !self.subscribed || !crate::rt::can_spawn() {
            return Finish::complete(&self.shared);
        }
        // Cut off before the unsubscribe finished: finish it (or wait out the one
        // under way) first, and only then release waiters.
        let shared = self.shared.clone();
        spawn_as(&shared.session.clone(), async move {
            shared.mesh.unsubscribe(&shared.topic).await;
            Finish::complete(&shared);
        });
    }
}

/// Start leaving: `None` if another caller already has (wait on `done` instead).
async fn begin_leave(shared: &Arc<Shared>) -> Option<Finish> {
    if shared.closed.swap(true, Ordering::Relaxed) {
        return None;
    }
    let mut finish = Finish { shared: shared.clone(), subscribed: true };
    shared.events.lock().unwrap().take();
    shared.mesh.unsubscribe(&shared.topic).await;
    finish.subscribed = false;
    Some(finish)
}

/// Publish the departure, unless the account is no longer the one logged in:
/// its signer is gone, and another account's would link the two.
async fn announce_left(shared: &Shared) {
    if shared.session.is_live() && shared.advertised.load(Ordering::SeqCst) {
        let _ = signal::send(&shared.chat_id, &shared.topic_b32, None).await;
        last_left_of(&shared.session).lock().unwrap().insert(shared.topic, unix_now());
    }
}

/// When this account last announced leaving each topic (unix seconds).
struct LastLeft;

fn last_left_of(session: &Arc<crate::db::Session>) -> Arc<Mutex<HashMap<[u8; 32], u64>>> {
    session.scoped::<LastLeft, _>()
}

fn unix_now() -> u64 {
    web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Advertise our node on the session's topic. Dated after our own last
/// departure from it: a departure wins a same-second tie, so an advertisement
/// in that second would read as stale and a quick reopen would stay invisible.
async fn advertise(shared: &Shared, addr: &str) -> Result<(), String> {
    let left = last_left_of(&shared.session).lock().unwrap().get(&shared.topic).copied();
    if let Some(left) = left {
        let now = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).unwrap_or_default();
        if now.as_secs() <= left {
            let next = Duration::from_secs(left + 1).saturating_sub(now);
            crate::rt::time::sleep(next + Duration::from_millis(50)).await;
        }
    }
    // A departure must stay the last word. The send dates its event before
    // its first await, so nothing can leave between this check and that.
    if shared.is_closed() {
        return Err("the session has ended".into());
    }
    shared.advertised.store(true, Ordering::SeqCst);
    signal::send(&shared.chat_id, &shared.topic_b32, Some(addr)).await
}

async fn leave(shared: Arc<Shared>, announce: bool) {
    let Some(finish) = begin_leave(&shared).await else {
        let mut done = shared.done.subscribe();
        let _ = done.wait_for(|d| *d).await;
        return;
    };
    if announce {
        announce_left(&shared).await;
    }
    drop(finish);
    crate::log_info!("[xdc] left {}", &shared.topic_b32[..8]);
}

/// Leave every session of the account now logged in, for an account swap: it
/// seals the account's registry for good, so no join can slip in behind the
/// teardown. Everything local happens next and unconditionally, the account's
/// mesh is closed (the next account binds a fresh node key), and the departure
/// signals go last, together, within `deadline`. Abandoned part-way, the
/// account refuses new sessions until it next logs in.
pub async fn leave_all(deadline: Duration) {
    leave_every(deadline, true).await;
}

/// [`leave_all`] for a network switch: the account stays open, so later joins (with consent
/// for the new network) still work. The mesh closes once nothing uses it.
pub async fn leave_all_for_switch(deadline: Duration) {
    leave_every(deadline, false).await;
    mesh::retire_live_if_idle().await;
}

async fn leave_every(deadline: Duration, seal: bool) {
    let session = crate::db::current_session();
    let registry = joined_of(&session);
    let all: Vec<Arc<Shared>> = {
        let map = registry.map.lock().unwrap();
        if seal {
            registry.sealed.store(true, Ordering::SeqCst);
        }
        map.values().cloned().collect()
    };
    let mut leaving = Vec::new();
    let mut already = Vec::new();
    for shared in all {
        match begin_leave(&shared).await {
            Some(finish) => leaving.push((shared, finish)),
            None => already.push(shared),
        }
    }
    if seal {
        mesh::retire(session.id());
    }
    let announcements = leaving.into_iter().map(|(shared, finish)| async move {
        crate::db::with_session(shared.session.clone(), announce_left(&shared)).await;
        drop(finish);
    });
    // Sessions that were already leaving get the same deadline to finish their own.
    let pending = already.into_iter().map(|shared| async move {
        let mut done = shared.done.subscribe();
        let _ = done.wait_for(|d| *d).await;
    });
    let _ = crate::rt::time::timeout(
        deadline,
        futures_util::future::join(futures_util::future::join_all(announcements), futures_util::future::join_all(pending)),
    )
    .await;
}

/// Sends on a session from anywhere; see [`XdcSession::sender`].
#[derive(Clone)]
pub struct XdcSender {
    shared: Arc<Shared>,
}

impl XdcSender {
    pub async fn send(&self, payload: impl Into<Vec<u8>>) -> Result<(), String> {
        if self.shared.is_closed() {
            return Err("the session has ended".into());
        }
        self.shared.mesh.broadcast(&self.shared.topic, payload.into()).await
    }

    pub fn topic(&self) -> &str {
        &self.shared.topic_b32
    }

    /// Advertise our node on the session's topic (see [`XdcSession::advertise`]).
    pub async fn advertise(&self) -> Result<(), String> {
        if self.shared.is_closed() {
            return Err("the session has ended".into());
        }
        let addr = wire::encode_node_addr(&self.shared.mesh.node_addr())?;
        crate::db::with_session(self.shared.session.clone(), advertise(&self.shared, &addr)).await
    }
}

impl Drop for XdcSession {
    fn drop(&mut self) {
        // A runtime already shutting down has no one left to tell.
        if !self.left && crate::rt::can_spawn() {
            let shared = self.shared.clone();
            spawn_as(&shared.session.clone(), leave(shared, true));
        }
    }
}

/// Apply an ingested signal to a live session this account is in: learn the
/// sender's node, dial an advertised peer, report a departure. Returns false
/// when no live session is on the topic.
pub async fn apply_signal(sig: &Signal) -> bool {
    let Ok(topic) = wire::decode_topic(&sig.topic) else { return false };
    let Some(shared) = live_session(&topic) else { return false };
    // A signal for our topic from another chat is not a player of this session.
    // Our own npub is: another device of this account plays as us.
    if shared.chat_id != sig.chat_id || !sig.current {
        return true;
    }
    match &sig.node_addr {
        Some(addr) => {
            let node = *addr.id.as_bytes();
            shared.bind(node, &sig.npub);
            shared.claimed.notify_one();
            if node == shared.mesh.node_key() {
                return true;
            }
            // Dialled in the background: signals arrive inside message processing,
            // which one unreachable peer must not hold up.
            let (shared, addr) = (shared.clone(), addr.clone());
            spawn_as(&shared.session.clone(), async move {
                match shared.mesh.add_peer(&topic, addr).await {
                    // A player who reopens the app on a node gossip never saw leave
                    // gets no fresh NeighborUp; the reachable advertisement is the arrival.
                    Ok(()) => {
                        let tx = shared.events.lock().unwrap().clone();
                        if let Some(tx) = tx {
                            if shared.present.lock().unwrap().insert(node) && tx.send(XdcEvent::PeerJoined(shared.peer(node))).await.is_err() {
                                shared.present.lock().unwrap().remove(&node);
                            }
                        }
                    }
                    Err(e) => crate::log_debug!("[xdc] could not reach advertised peer {}: {e}", mesh::short(&node)),
                }
            });
        }
        // Gossip keeps a neighbor whose app closed while its node stays up, so the
        // departure signal is often the only word that a player left.
        None => {
            let gone: Vec<[u8; NODE_KEY_LEN]> = {
                let mut present = shared.present.lock().unwrap();
                let mine: Vec<_> = present.iter().copied().filter(|n| shared.npub(n).as_deref() == Some(sig.npub.as_str())).collect();
                for n in &mine {
                    present.remove(n);
                }
                mine
            };
            let tx = shared.events.lock().unwrap().clone();
            if let Some(tx) = tx {
                for node in gone {
                    let _ = tx.send(XdcEvent::PeerLeft(shared.peer(node))).await;
                }
            }
        }
    }
    true
}

fn short_chat(chat_id: &str) -> &str {
    chat_id.get(..12).unwrap_or(chat_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn unregistered(chat: &str, topic: [u8; 32]) -> (Arc<Shared>, mpsc::Receiver<XdcEvent>) {
        let (tx, rx) = mpsc::channel(QUEUE);
        let shared = Arc::new(Shared {
            topic,
            topic_b32: wire::encode_topic(&topic),
            chat_id: chat.to_string(),
            session: crate::db::current_session(),
            mesh: Arc::new(Mesh::offline().await),
            roster: Mutex::new(HashMap::new()),
            present: Mutex::new(HashSet::new()),
            events: Mutex::new(Some(tx)),
            closed: AtomicBool::new(false),
            done: watch::channel(false).0,
            readvertise: Mutex::new(ReadvertiseState::default()),
            claimed: tokio::sync::Notify::new(),
            hold_unnamed: true,
            advertised: AtomicBool::new(false),
        });
        (shared, rx)
    }

    async fn shared_for(chat: &str, topic: [u8; 32]) -> (Arc<Shared>, mpsc::Receiver<XdcEvent>) {
        let (shared, rx) = unregistered(chat, topic).await;
        joined().map.lock().unwrap().insert(topic, shared.clone());
        (shared, rx)
    }

    fn sig(chat: &str, npub: &str, topic: &[u8; 32], node: Option<[u8; 32]>, current: bool) -> Signal {
        Signal {
            chat_id: chat.to_string(),
            npub: npub.to_string(),
            topic: wire::encode_topic(topic),
            node_addr: node.map(|n| iroh::EndpointAddr::new(iroh::PublicKey::from_bytes(&n).unwrap())),
            created_at: 1,
            current,
        }
    }

    fn node(seed: u8) -> [u8; 32] {
        *iroh::SecretKey::from([seed; 32]).public().as_bytes()
    }

    #[tokio::test]
    async fn the_first_account_to_name_a_node_keeps_it_and_a_rival_claim_unattributes_it() {
        // Bound to one session: parallel tests swap the process-wide one.
        crate::db::with_session(crate::db::current_session(), async {
            let topic = [41u8; 32];
            let (shared, _rx) = shared_for("npub1chat", topic).await;
            let a = node(1);
            apply_signal(&sig("npub1chat", "npub1alice", &topic, Some(a), true)).await;
            assert_eq!(shared.npub(&a).as_deref(), Some("npub1alice"));
            apply_signal(&sig("npub1chat", "npub1alice", &topic, Some(a), true)).await;
            assert_eq!(shared.npub(&a).as_deref(), Some("npub1alice"), "re-advertising is not a rival");

            apply_signal(&sig("npub1chat", "npub1mallory", &topic, Some(a), true)).await;
            assert_eq!(shared.npub(&a), None, "a contested node is nobody's");
            apply_signal(&sig("npub1chat", "npub1alice", &topic, Some(a), true)).await;
            assert_eq!(shared.npub(&a), None, "contest is permanent for the session");
            let frame = XdcFrame { payload: vec![], from: shared.peer(a), direct: true };
            assert_eq!(frame.verified_sender(), None);
            joined().map.lock().unwrap().remove(&topic);
        })
        .await;
    }

    #[tokio::test]
    async fn signals_from_another_chat_or_superseded_ones_change_nothing() {
        // Bound to one session: parallel tests swap the process-wide one.
        crate::db::with_session(crate::db::current_session(), async {
            let topic = [42u8; 32];
            let (shared, _rx) = shared_for("npub1chat", topic).await;
            let a = node(2);
            assert!(apply_signal(&sig("npub1stranger", "npub1stranger", &topic, Some(a), true)).await);
            assert!(apply_signal(&sig("npub1chat", "npub1alice", &topic, Some(a), false)).await);
            assert_eq!(shared.npub(&a), None);
            assert!(!apply_signal(&sig("npub1chat", "npub1alice", &[43u8; 32], Some(a), true)).await, "no session on that topic");
            joined().map.lock().unwrap().remove(&topic);
        })
        .await;
    }

    #[tokio::test]
    async fn a_departure_leaves_only_its_own_nodes_and_pairs_with_the_arrival() {
        // Bound to one session: parallel tests swap the process-wide one.
        crate::db::with_session(crate::db::current_session(), async {
            let topic = [44u8; 32];
            let (shared, mut rx) = shared_for("npub1chat", topic).await;
            let (a, b) = (node(3), node(4));
            shared.bind(a, "npub1alice");
            shared.bind(b, "npub1bob");
            shared.present.lock().unwrap().extend([a, b]);

            apply_signal(&sig("npub1chat", "npub1mallory", &topic, None, true)).await;
            assert!(rx.try_recv().is_err(), "a stranger's departure removes nobody");

            apply_signal(&sig("npub1chat", "npub1alice", &topic, None, true)).await;
            match rx.try_recv() {
                Ok(XdcEvent::PeerLeft(p)) => assert_eq!((p.node, p.npub.as_deref()), (a, Some("npub1alice"))),
                other => panic!("expected alice to leave, got {other:?}"),
            }
            apply_signal(&sig("npub1chat", "npub1alice", &topic, None, true)).await;
            assert!(rx.try_recv().is_err(), "one leave per arrival");
            assert!(shared.present.lock().unwrap().contains(&b));
            joined().map.lock().unwrap().remove(&topic);
        })
        .await;
    }

    #[tokio::test]
    async fn a_closing_session_is_not_joined_and_takes_no_signals() {
        // Bound to one session: parallel tests swap the process-wide one.
        crate::db::with_session(crate::db::current_session(), async {
            let topic = [45u8; 32];
            let (shared, _rx) = shared_for("npub1chat", topic).await;
            assert!(is_joined(&wire::encode_topic(&topic)));
            shared.closed.store(true, Ordering::Relaxed);
            assert!(!is_joined(&wire::encode_topic(&topic)));
            assert!(!apply_signal(&sig("npub1chat", "npub1alice", &topic, Some(node(5)), true)).await);
            joined().map.lock().unwrap().remove(&topic);
        })
        .await;
    }

    #[tokio::test]
    async fn a_leave_cut_off_half_way_still_finishes() {
        crate::db::with_session(crate::db::current_session(), async {
            let topic = [46u8; 32];
            let (shared, mut rx) = shared_for("npub1chat", topic).await;
            let mut done = shared.done.subscribe();
            let cut = async {
                let _finish = Finish { shared: shared.clone(), subscribed: false };
                std::future::pending::<()>().await;
            };
            assert!(tokio::time::timeout(Duration::from_millis(20), cut).await.is_err());
            assert!(*done.borrow_and_update(), "waiters are released");
            assert!(!joined().map.lock().unwrap().contains_key(&topic), "deregistered");
            // (The pump's own sender, absent here, ends with the unsubscribe.)
            assert!(rx.recv().await.is_none(), "the reader sees the end");
        })
        .await;
    }

    #[tokio::test]
    async fn a_leave_cut_off_mid_unsubscribe_releases_waiters_only_once_the_topic_is_gone() {
        crate::db::with_session(crate::db::current_session(), async {
            let topic = [48u8; 32];
            let (shared, _rx) = shared_for("npub1chat", topic).await;
            let (mesh_tx, mut mesh_rx) = mpsc::channel(4);
            shared.mesh.subscribe(topic, Vec::new(), mesh_tx).await.unwrap();
            let mut done = shared.done.subscribe();
            // One poll: the leave starts unsubscribing, then is dropped.
            assert!(futures_util::FutureExt::now_or_never(begin_leave(&shared)).is_none());
            assert!(!*done.borrow(), "nobody released before the topic is dropped");
            done.wait_for(|d| *d).await.unwrap();
            assert!(
                matches!(mesh_rx.try_recv(), Err(mpsc::error::TryRecvError::Disconnected)),
                "the old subscription's handles are gone by the time waiters are released"
            );
            assert!(!joined().map.lock().unwrap().contains_key(&topic));
        })
        .await;
    }

    #[tokio::test]
    async fn a_join_after_the_teardown_sealed_the_account_backs_out() {
        crate::db::with_session(crate::db::current_session(), async {
            let registry = joined();
            registry.sealed.store(true, Ordering::SeqCst);
            let topic = wire::encode_topic(&[47u8; 32]);
            let r = join_inner("npub1chat".into(), topic.clone(), JoinOptions::default()).await;
            registry.sealed.store(false, Ordering::SeqCst);
            // Refused before anything is bound: the later back-out says otherwise.
            assert_eq!(r.err().as_deref(), Some("the account is switching"));
        })
        .await;
    }

    #[tokio::test]
    async fn a_network_switch_ends_every_session_but_keeps_later_joins_open() {
        let _serial = mesh::SLOT_TESTS.lock().await;
        crate::db::with_session(crate::db::current_session(), async {
            let owner = crate::db::current_session().id();
            let node = mesh::install_for_test(owner, Mesh::offline().await).await;
            let topic = [45u8; 32];
            let (shared, _rx) = shared_for("npub1chat", topic).await;
            leave_all_for_switch(Duration::from_millis(200)).await;
            assert!(shared.is_closed(), "the session ended");
            assert!(!joined().sealed.load(Ordering::SeqCst), "a switch is not an account swap");
            assert_eq!(mesh::slot_owner().await, None, "the idle node closed with its last session");
            drop(node);
            let r = join_inner("npub1chat".into(), wire::encode_topic(&[44u8; 32]), JoinOptions::default()).await;
            assert_ne!(r.err().as_deref(), Some("the account is switching"));
        })
        .await;
    }

    #[tokio::test]
    async fn a_join_that_finishes_after_the_seal_is_not_registered() {
        crate::db::with_session(crate::db::current_session(), async {
            let topic = [49u8; 32];
            let (shared, _rx) = unregistered("npub1chat", topic).await;
            let registry = Joined::default();
            registry.sealed.store(true, Ordering::SeqCst);
            assert!(!register(&registry, &shared));
            assert!(registry.map.lock().unwrap().is_empty(), "a sealed account gains no session");
            let open = Joined::default();
            assert!(register(&open, &shared));
            assert!(open.map.lock().unwrap().contains_key(&topic));
        })
        .await;
    }

    #[tokio::test]
    async fn a_sealed_join_that_stops_before_registering_retires_the_mesh_it_bound() {
        let _serial = mesh::SLOT_TESTS.lock().await;
        let owner = u64::MAX - 47;
        mesh::install_for_test(owner, Mesh::offline().await).await;
        let registry = Arc::new(Joined::default());
        drop(Unregistered { registry: registry.clone(), owner, armed: true });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(mesh::slot_owner().await, Some(owner), "an account still open keeps its mesh");
        registry.sealed.store(true, Ordering::SeqCst);
        drop(Unregistered { registry: registry.clone(), owner, armed: false });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(mesh::slot_owner().await, Some(owner), "a registered join keeps it");
        drop(Unregistered { registry, owner, armed: true });
        for _ in 0..100 {
            if mesh::slot_owner().await != Some(owner) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the sealed account's mesh was not retired");
    }

    fn piped(shared: &Arc<Shared>) -> (mpsc::Sender<MeshEvent>, mpsc::Receiver<XdcEvent>) {
        let (mesh_tx, mesh_rx) = mpsc::channel(QUEUE);
        let (tx, rx) = mpsc::channel(QUEUE);
        crate::db::spawn_bound(pump(shared.clone(), mesh_rx, tx));
        (mesh_tx, rx)
    }

    fn data(node: [u8; 32], text: &str) -> MeshEvent {
        MeshEvent::Data { payload: text.as_bytes().to_vec(), sender: node, direct: true }
    }

    async fn next(rx: &mut mpsc::Receiver<XdcEvent>) -> Option<XdcEvent> {
        tokio::time::timeout(Duration::from_millis(300), rx.recv()).await.ok().flatten()
    }

    fn said(ev: Option<XdcEvent>) -> (Option<String>, Option<String>) {
        match ev {
            Some(XdcEvent::Data(f)) => (f.text().map(str::to_string), f.verified_sender().map(str::to_string)),
            other => panic!("expected a frame, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_frame_waits_for_an_advertisement_to_name_its_sender() {
        crate::db::with_session(crate::db::current_session(), async {
            let topic = [50u8; 32];
            let (shared, _events) = shared_for("npub1chat", topic).await;
            let (alice, bob) = (node(6), node(7));
            shared.bind(bob, "npub1bob");
            let (mesh, mut rx) = piped(&shared);
            mesh.send(MeshEvent::NeighborUp(alice)).await.unwrap();
            mesh.send(data(alice, "first")).await.unwrap();
            mesh.send(data(alice, "second")).await.unwrap();
            mesh.send(data(bob, "hi")).await.unwrap();
            assert_eq!(said(next(&mut rx).await), (Some("hi".into()), Some("npub1bob".into())), "a named sender isn't held");
            assert!(next(&mut rx).await.is_none(), "alice's events wait for her advertisement");

            // Her advertisement lands (dialling her fake node then fails in the background).
            let ad = sig("npub1chat", "npub1alice", &topic, Some(alice), true);
            crate::db::spawn_bound(async move {
                apply_signal(&ad).await;
            });
            match next(&mut rx).await {
                Some(XdcEvent::PeerJoined(p)) => assert_eq!(p.npub.as_deref(), Some("npub1alice")),
                other => panic!("expected alice to arrive first, got {other:?}"),
            }
            assert_eq!(said(next(&mut rx).await), (Some("first".into()), Some("npub1alice".into())));
            assert_eq!(said(next(&mut rx).await), (Some("second".into()), Some("npub1alice".into())));
            mesh.send(data(alice, "third")).await.unwrap();
            assert_eq!(said(next(&mut rx).await), (Some("third".into()), Some("npub1alice".into())), "named from then on");
            joined().map.lock().unwrap().remove(&topic);
        })
        .await;
    }

    #[tokio::test]
    async fn a_sender_nobody_names_is_delivered_anonymous_once_the_wait_is_over() {
        crate::db::with_session(crate::db::current_session(), async {
            let (shared, _events) = unregistered("npub1chat", [51u8; 32]).await;
            let carol = node(8);
            let (mesh, mut rx) = piped(&shared);
            tokio::time::pause();
            mesh.send(data(carol, "x")).await.unwrap();
            tokio::time::advance(ATTRIBUTION_WAIT + Duration::from_secs(1)).await;
            assert_eq!(said(rx.recv().await), (Some("x".into()), None));
            mesh.send(data(carol, "y")).await.unwrap();
            let y = tokio::time::timeout(Duration::from_millis(1), rx.recv()).await.expect("no second wait for the same node");
            assert_eq!(said(y), (Some("y".into()), None));
        })
        .await;
    }

    #[tokio::test]
    async fn the_hold_is_bounded_and_opened_only_by_a_node_itself() {
        crate::db::with_session(crate::db::current_session(), async {
            let (shared, _events) = unregistered("npub1chat", [52u8; 32]).await;
            let (mesh, mut rx) = piped(&shared);
            // A relayed frame can't open a hold: its named sender is anyone's claim.
            mesh.send(MeshEvent::Data { payload: b"relayed".to_vec(), sender: node(20), direct: false }).await.unwrap();
            assert_eq!(said(next(&mut rx).await).0.as_deref(), Some("relayed"));
            // Every slot taken, the next unnamed node is delivered at once.
            for i in 0..HELD_NODES as u8 {
                mesh.send(data(node(100 + i), "held")).await.unwrap();
            }
            mesh.send(data(node(99), "overflow")).await.unwrap();
            assert_eq!(said(next(&mut rx).await).0.as_deref(), Some("overflow"));
            assert!(next(&mut rx).await.is_none(), "the rest wait");
        })
        .await;
    }

    #[tokio::test]
    async fn a_held_node_keeps_its_first_frames_then_reports_the_rest_lost() {
        crate::db::with_session(crate::db::current_session(), async {
            let (shared, _events) = unregistered("npub1chat", [53u8; 32]).await;
            let dave = node(21);
            let (mesh, mut rx) = piped(&shared);
            for i in 0..HELD_FRAMES + 3 {
                mesh.send(data(dave, &i.to_string())).await.unwrap();
            }
            assert!(next(&mut rx).await.is_none());
            shared.bind(dave, "npub1dave");
            shared.claimed.notify_one();
            for i in 0..HELD_FRAMES {
                assert_eq!(said(next(&mut rx).await), (Some(i.to_string()), Some("npub1dave".into())));
            }
            assert!(matches!(next(&mut rx).await, Some(XdcEvent::Lagged)));
        })
        .await;
    }

    #[tokio::test]
    async fn held_bytes_are_capped_across_the_session() {
        crate::db::with_session(crate::db::current_session(), async {
            let (shared, _events) = unregistered("npub1chat", [56u8; 32]).await;
            let gus = node(24);
            let (mesh, mut rx) = piped(&shared);
            mesh.send(MeshEvent::Data { payload: vec![1u8; HELD_BYTES / 2], sender: gus, direct: true }).await.unwrap();
            mesh.send(MeshEvent::Data { payload: vec![2u8; HELD_BYTES / 2 + 1], sender: gus, direct: true }).await.unwrap();
            assert!(next(&mut rx).await.is_none());
            shared.bind(gus, "npub1gus");
            shared.claimed.notify_one();
            match next(&mut rx).await {
                Some(XdcEvent::Data(f)) => assert_eq!(f.payload.len(), HELD_BYTES / 2),
                other => panic!("expected the frame that fit, got {other:?}"),
            }
            assert!(matches!(next(&mut rx).await, Some(XdcEvent::Lagged)), "the one that didn't is reported");
        })
        .await;
    }

    #[tokio::test]
    async fn a_node_that_leaves_while_held_neither_arrives_nor_leaves() {
        crate::db::with_session(crate::db::current_session(), async {
            let (shared, _events) = unregistered("npub1chat", [54u8; 32]).await;
            let erin = node(22);
            let (mesh, mut rx) = piped(&shared);
            mesh.send(MeshEvent::NeighborUp(erin)).await.unwrap();
            mesh.send(data(erin, "bye")).await.unwrap();
            mesh.send(MeshEvent::NeighborDown(erin)).await.unwrap();
            assert!(next(&mut rx).await.is_none());
            shared.bind(erin, "npub1erin");
            shared.claimed.notify_one();
            assert_eq!(said(next(&mut rx).await), (Some("bye".into()), Some("npub1erin".into())), "its frames still count");
            assert!(next(&mut rx).await.is_none(), "no arrival for a node already gone, so no leave either");
        })
        .await;
    }

    #[tokio::test]
    async fn a_contested_node_is_released_anonymous() {
        crate::db::with_session(crate::db::current_session(), async {
            let (shared, _events) = unregistered("npub1chat", [55u8; 32]).await;
            let frank = node(23);
            let (mesh, mut rx) = piped(&shared);
            mesh.send(data(frank, "mine")).await.unwrap();
            assert!(next(&mut rx).await.is_none());
            shared.bind(frank, "npub1frank");
            shared.bind(frank, "npub1mallory");
            shared.claimed.notify_one();
            assert_eq!(said(next(&mut rx).await), (Some("mine".into()), None));
        })
        .await;
    }

    #[tokio::test]
    async fn an_app_window_gets_frames_from_an_unnamed_sender_at_once() {
        crate::db::with_session(crate::db::current_session(), async {
            let (shared, _events) = unregistered("npub1chat", [57u8; 32]).await;
            let shared = Arc::new(Shared { hold_unnamed: false, ..Arc::try_unwrap(shared).ok().expect("sole owner") });
            let (mesh, mut rx) = piped(&shared);
            mesh.send(data(node(25), "now")).await.unwrap();
            assert_eq!(said(next(&mut rx).await), (Some("now".into()), None));
        })
        .await;
    }

    #[tokio::test]
    async fn an_advertisement_is_dated_after_our_last_departure() {
        crate::db::with_session(crate::db::current_session(), async {
            let (shared, _events) = unregistered("npub1chat", [58u8; 32]).await;
            let left = unix_now() + 1;
            last_left_of(&shared.session).lock().unwrap().insert(shared.topic, left);
            // The send itself fails here (no client); what matters is when it is made.
            let _ = advertise(&shared, "addr").await;
            assert!(unix_now() > left, "a same-second departure would win the tie");
        })
        .await;
    }

    #[tokio::test]
    async fn only_a_session_that_advertised_announces_its_departure() {
        // Departures go out only from the live session; hold off the tests that swap it.
        let _swaps = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        crate::db::with_session(crate::db::current_session(), async {
            let (shared, _events) = unregistered("npub1chat", [59u8; 32]).await;
            let departed = || last_left_of(&shared.session).lock().unwrap().contains_key(&shared.topic);
            announce_left(&shared).await;
            assert!(!departed(), "nothing of ours to retract");
            let _ = advertise(&shared, "addr").await;
            announce_left(&shared).await;
            assert!(departed());
        })
        .await;
    }

    #[tokio::test]
    async fn only_a_session_that_advertised_re_advertises() {
        let (shared, _events) = unregistered("npub1chat", [61u8; 32]).await;
        readvertise_later(shared.clone());
        assert!(!shared.readvertise.lock().unwrap().pending, "nobody was told we are here");
        shared.advertised.store(true, Ordering::SeqCst);
        readvertise_later(shared.clone());
        assert!(shared.readvertise.lock().unwrap().pending);
    }

    #[tokio::test]
    async fn a_session_that_has_left_never_advertises_again() {
        crate::db::with_session(crate::db::current_session(), async {
            let (shared, _events) = unregistered("npub1chat", [60u8; 32]).await;
            shared.closed.store(true, Ordering::SeqCst);
            assert_eq!(advertise(&shared, "addr").await, Err("the session has ended".into()));
        })
        .await;
    }

    #[tokio::test]
    async fn the_roster_stops_attributing_past_its_cap() {
        let (shared, _rx) = unregistered("", [0; 32]).await;
        for i in 0..(ROSTER_CAP as u32 + 10) {
            let mut n = [0u8; 32];
            n[..4].copy_from_slice(&i.to_le_bytes());
            shared.bind(n, "npub1x");
        }
        assert_eq!(shared.roster.lock().unwrap().len(), ROSTER_CAP);
    }
}
