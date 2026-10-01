//! Realtime channels for mini apps: vector-core's sessions (`vector_core::xdc`),
//! delivered to the app's frame. A browser node is relay-only; the wire is every
//! client's.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use futures_util::future::Either;
use serde_json::json;
use tokio::sync::oneshot;
use vector_core::xdc::{JoinOptions, XdcEvent, XdcSender, XdcSession};

use crate::emitter;

/// Events held for a window whose app has not joined yet.
const BUFFER_LIMIT: usize = 256;
/// How long a window's own join waits on the one its open started.
const JOIN_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// Where a session's events go: the app window's label once it joins, a buffer until then.
#[derive(Default)]
struct Target {
    label: Option<String>,
    buffer: Vec<Delivery>,
    /// The app left: drop events rather than hold them.
    detached: bool,
}

enum Delivery {
    Data(Vec<u8>),
    Signal(&'static str, Option<String>),
}

impl Target {
    fn send(&mut self, d: Delivery) {
        match &self.label {
            Some(label) => deliver(label, d),
            None if !self.detached && self.buffer.len() < BUFFER_LIMIT => self.buffer.push(d),
            None => {}
        }
    }

    fn attach(&mut self, label: String) {
        for d in self.buffer.drain(..) {
            deliver(&label, d);
        }
        self.label = Some(label);
        self.detached = false;
    }

    fn detach(&mut self) {
        self.label = None;
        self.buffer.clear();
        self.detached = true;
    }
}

fn deliver(label: &str, d: Delivery) {
    match d {
        Delivery::Data(bytes) => emitter::emit_bytes("miniapp_rt", &json!({ "label": label, "event": "data" }), &bytes),
        Delivery::Signal(event, data) => emitter::emit("miniapp_rt", &json!({ "label": label, "event": event, "data": data })),
    }
}

/// One app window's session: joined once, whoever asks first (the open, or the
/// app's own join), and ended with the window.
struct Slot {
    id: u64,
    topic: String,
    target: Arc<Mutex<Target>>,
    /// `None` while the join is in flight.
    joined: Option<Joined>,
}

struct Joined {
    stop: oneshot::Sender<()>,
    sender: XdcSender,
    finished: std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>,
}

/// The worker serves one account: a swap reloads it.
static SLOTS: Mutex<Option<HashMap<String, Slot>>> = Mutex::new(None);

fn slots<R>(f: impl FnOnce(&mut HashMap<String, Slot>) -> R) -> R {
    f(SLOTS.lock().unwrap().get_or_insert_with(HashMap::new))
}

/// What a window needs to open its session.
pub(super) struct Window<'a> {
    pub label: &'a str,
    pub id: u64,
    pub chat_id: &'a str,
    /// Canonical base32.
    pub topic: &'a str,
    /// Tell the chat we are playing; off for solo play, which has no chat.
    pub advertise: bool,
}

enum Step {
    Done,
    Wait,
    Replace(Slot),
    Join(Arc<Mutex<Target>>),
}

/// Make sure the window's session exists, joining it if nothing has yet, and
/// deliver its events to the app when `attach` (the app's own join). A window
/// gets one join however many times it asks.
pub(super) async fn open(w: Window<'_>, attach: bool) -> Result<(), String> {
    let label = w.label.to_string();
    let mut attached = false;
    let mut waited = std::time::Duration::ZERO;
    let target = loop {
        let step = slots(|m| match m.get(&label) {
            Some(slot) if slot.id == w.id => {
                if attach && !attached {
                    slot.target.lock().unwrap().attach(label.clone());
                    attached = true;
                }
                if slot.joined.is_some() { Step::Done } else { Step::Wait }
            }
            Some(_) => Step::Replace(m.remove(&label).expect("present")),
            None => {
                let mut target = Target::default();
                if attach {
                    target.label = Some(label.clone());
                }
                let target = Arc::new(Mutex::new(target));
                m.insert(label.clone(), Slot { id: w.id, topic: w.topic.to_string(), target: target.clone(), joined: None });
                Step::Join(target)
            }
        });
        match step {
            Step::Done => return Ok(()),
            Step::Replace(stale) => finish(stale).await,
            Step::Join(target) => break target,
            Step::Wait if waited >= JOIN_WAIT => return Err("the realtime session is still connecting".into()),
            Step::Wait => {
                let tick = std::time::Duration::from_millis(100);
                vector_core::rt::time::sleep(tick).await;
                waited += tick;
            }
        }
    };

    // The page asks for consent before an app that uses realtime opens.
    let options = JoinOptions { hold_unnamed: false, advertise: false, outside_tor: true };
    let session = match vector_core::xdc::join_with(w.chat_id, w.topic, options).await {
        Ok(s) => s,
        Err(e) => {
            slots(|m| {
                if m.get(&label).is_some_and(|s| s.id == w.id && s.joined.is_none()) {
                    m.remove(&label);
                }
            });
            return Err(e);
        }
    };

    let sender = session.sender();
    let adopted = slots(|m| match m.get_mut(&label) {
        Some(slot) if slot.id == w.id && slot.joined.is_none() => {
            let (stop, stop_rx) = oneshot::channel();
            let finished = Box::pin(session.finished());
            vector_core::db::spawn_bound(pump(session, target, stop_rx));
            slot.joined = Some(Joined { stop, sender: sender.clone(), finished });
            None
        }
        _ => Some(session),
    });
    if let Some(session) = adopted {
        // The window closed while joining.
        session.leave().await;
        return Err("the mini app closed while joining".into());
    }
    if w.advertise {
        vector_core::db::spawn_bound(async move {
            if let Err(e) = sender.advertise().await {
                vector_core::log_warn!("[WEBXDC] advertisement failed: {e}");
            }
        });
    }
    Ok(())
}

/// The app left the channel: its window keeps the session, but nothing more is
/// delivered or held for it until it joins again.
pub(super) fn detach(label: &str) {
    slots(|m| {
        if let Some(slot) = m.get(label) {
            slot.target.lock().unwrap().detach();
        }
    });
}

/// Broadcast one frame from the app.
pub(super) async fn send(label: &str, bytes: Vec<u8>) -> Result<(), String> {
    let sender = slots(|m| m.get(label).and_then(|s| s.joined.as_ref()).map(|j| j.sender.clone()));
    sender.ok_or("No realtime channel")?.send(bytes).await
}

/// End the window's session (announcing the departure), if it is still the one
/// `id` opened.
pub(super) async fn close(label: &str, id: u64) {
    let slot = slots(|m| match m.get(label) {
        Some(s) if s.id == id => m.remove(label),
        _ => None,
    });
    if let Some(slot) = slot {
        finish(slot).await;
    }
}

async fn finish(slot: Slot) {
    if let Some(joined) = slot.joined {
        let _ = joined.stop.send(());
        if vector_core::rt::time::timeout(std::time::Duration::from_secs(15), joined.finished).await.is_err() {
            vector_core::log_warn!("[WEBXDC] realtime leave took over 15s");
        }
    }
}

/// Whether any window has a session on `topic` (canonical base32).
pub(super) fn is_active(topic: &str) -> bool {
    slots(|m| m.values().any(|s| s.topic == topic && s.joined.is_some()))
}

/// Carry one session's events to its window until it ends, or until the window
/// closes (then leave, announcing the departure).
async fn pump(mut session: XdcSession, target: Arc<Mutex<Target>>, mut stop: oneshot::Receiver<()>) {
    let topic = session.topic().to_string();
    let mut connected = false;
    loop {
        let event = {
            let recv = std::pin::pin!(session.recv());
            match futures_util::future::select(&mut stop, recv).await {
                Either::Left(_) => break,
                Either::Right((event, _)) => event,
            }
        };
        let Some(event) = event else { return };
        let mut t = target.lock().unwrap();
        match event {
            XdcEvent::Data(frame) => t.send(Delivery::Data(frame.payload)),
            XdcEvent::PeerJoined(peer) => {
                if !connected {
                    connected = true;
                    t.send(Delivery::Signal("connected", None));
                }
                t.send(Delivery::Signal("peerJoined", Some(vector_core::xdc::wire::base32_nopad_encode(&peer.node))));
                drop(t);
                super::emit_status(&topic, true);
            }
            XdcEvent::PeerLeft(peer) => {
                t.send(Delivery::Signal("peerLeft", Some(vector_core::xdc::wire::base32_nopad_encode(&peer.node))));
                drop(t);
                super::emit_status(&topic, true);
            }
            XdcEvent::Lagged => t.send(Delivery::Signal("lagged", None)),
        }
    }
    session.leave().await;
}
