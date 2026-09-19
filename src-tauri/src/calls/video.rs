//! The video of a call: frames from the webview go out one QUIC stream each, frames
//! from the peer's streams go back to the webview, and per-track latches keep a lost
//! or reset frame from ever painting a corrupt picture. A side may send its camera
//! and its screen at once; every frame's flags say which one it belongs to.

use super::link::{self, LinkConn};
use super::media::MediaStats;
use super::rate::{Rung, VideoObservation, VideoRate, CAMERA_LADDER, CAMERA_RELAY_CAP, SCREEN_LADDER, SCREEN_RELAY_CAP};
use super::transport::{Control, Tracks, VideoCodec, VideoHeader, VideoKind, MAX_VIDEO_FRAME, VIDEO_HEADER_LEN};
use bytes::{Bytes, BytesMut};
use iroh::endpoint::{Connection, SendStream, VarInt};
use serde::Serialize;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use vector_core::calls::link::{control, parse, FromLink, LinkMsg, ToLink, KIND_FRAME};

/// Frames on the wire at once before captures are skipped, at a zero round trip. A
/// frame counts as on the wire until the peer has acknowledged all of it, so the
/// cap grows with the round trip: what a path holds in one RTT, and a little over.
const IN_FLIGHT_MIN: usize = 3;
/// A delta the peer has not acknowledged by then is reset: it is a picture of the
/// past. Also scaled by the round trip, and a keyframe gets longer, since resetting
/// one only buys another.
const STALE_MIN: Duration = Duration::from_millis(400);
const STALE_KEY_MIN: Duration = Duration::from_secs(2);
/// Keyframes cost tens of kilobytes; requests coalesce to this many, in both directions.
const KEYFRAME_MIN_GAP: Duration = Duration::from_secs(1);
/// A frame the peer takes longer than this to deliver is abandoned; the next keyframe restarts.
const INBOUND_FRAME_TIMEOUT: Duration = Duration::from_secs(2);
/// With the screen on as well, the camera is a thumbnail: it gets this rung at most.
const CAMERA_BESIDE_SCREEN_CAP: usize = 2;

const KINDS: [VideoKind; 2] = [VideoKind::Camera, VideoKind::Screen];

/// One encoder's own numbers, as the webview reports them.
#[derive(Default)]
pub struct TrackStats {
    pub fps: AtomicU32,
    pub kbps: AtomicU32,
    pub width: AtomicU32,
    pub height: AtomicU32,
}

#[derive(Default)]
pub struct VideoStats {
    pub sent: AtomicU64,
    pub bytes_out: AtomicU64,
    pub skipped: AtomicU64,
    pub stale: AtomicU64,
    pub received: AtomicU64,
    pub bytes_in: AtomicU64,
    pub dropped: AtomicU64,
    pub key_requests: AtomicU64,
    pub camera: TrackStats,
    pub screen: TrackStats,
}

#[derive(Serialize, Clone, Debug, Default)]
pub struct TrackSnapshot {
    pub fps: u32,
    pub kbps: u32,
    pub width: u32,
    pub height: u32,
    /// The rung the ladder is on, and how many it has; the panel's quality control.
    pub rung: u32,
    pub rungs: u32,
    pub pinned: bool,
}

/// The stats line's view of the video.
#[derive(Serialize, Clone, Debug, Default)]
pub struct VideoSnapshot {
    pub sent: u64,
    pub bytes_out: u64,
    pub skipped: u64,
    pub stale: u64,
    pub received: u64,
    pub bytes_in: u64,
    pub dropped: u64,
    pub key_requests: u64,
    pub camera: TrackSnapshot,
    pub screen: TrackSnapshot,
}

impl VideoStats {
    fn track(&self, kind: VideoKind) -> &TrackStats {
        match kind {
            VideoKind::Camera => &self.camera,
            VideoKind::Screen => &self.screen,
        }
    }
}

/// What the track needs from the session while it runs.
pub struct Hooks {
    pub call_id: String,
    /// The call's control stream, for keyframe requests.
    pub control: Arc<tokio::sync::Mutex<SendStream>>,
    /// The webview's socket went away: the session tells the peer our video is off.
    pub on_link_closed: Arc<dyn Fn(&str) + Send + Sync>,
    /// The webview reported what it can encode and decode.
    pub on_caps: Arc<dyn Fn(Vec<String>, Vec<String>) + Send + Sync>,
    /// The audio track's counters: its refused datagrams are video's first alarm.
    pub audio: Arc<MediaStats>,
    /// The peer declared decoders in its offer or answer. A peer that did not runs a
    /// build whose control reader stops at the first message it does not know, so
    /// it is never sent one.
    pub peer_video: bool,
}

/// The user's choices for one track: a held rung, a held frame rate, or neither.
#[derive(Clone, Copy, Default)]
pub struct Prefs {
    pub rung: Option<usize>,
    pub fps: Option<u32>,
}

struct Shared {
    stats: Arc<VideoStats>,
    to_web: Mutex<Option<mpsc::Sender<Bytes>>>,
    in_flight: AtomicUsize,
    /// That track's next outgoing frame must be a keyframe: a delta was reset.
    need_key_out: [AtomicBool; 2],
    /// Drop that track's incoming deltas until a keyframe: the chain broke, or the
    /// webview just reconnected.
    need_key_in: [AtomicBool; 2],
    last_key_request: [Mutex<Option<Instant>>; 2],
    /// The peer's keyframe requests are honoured this often at most.
    last_key_forced: [Mutex<Option<Instant>>; 2],
    codec: Mutex<Option<VideoCodec>>,
    peer: Mutex<Tracks>,
    /// The peer declared decoders, so it runs a build that knows the video messages.
    peer_video: AtomicBool,
    conn: Connection,
    /// One ladder per track we send; None while that track is off.
    rate: Mutex<[Option<VideoRate>; 2]>,
    prefs: Mutex<[Prefs; 2]>,
}

impl Shared {
    fn tell_web(&self, msg: &ToLink) {
        let tx = self.to_web.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(tx) = tx {
            let _ = tx.try_send(Bytes::from(control(msg)));
        }
    }

    /// Whether the selected path goes through a relay, and its round trip.
    fn path(&self) -> (bool, u32) {
        let paths = self.conn.paths();
        match paths.iter().find(|p| p.is_selected()).or_else(|| paths.iter().next()) {
            Some(p) => (p.is_relay(), p.rtt().as_millis() as u32),
            None => (true, 0),
        }
    }

    fn tell_rung(&self, kind: VideoKind, r: Rung) {
        self.tell_web(&ToLink::Rate { kind, kbps: r.kbps, width: r.width, height: r.height, fps: r.fps });
    }

    /// Frames per second across everything we send, for the in-flight cap.
    fn sending_fps(&self) -> usize {
        let rates = self.rate.lock().unwrap_or_else(|e| e.into_inner());
        rates.iter().flatten().map(|r| r.rung().fps as usize).sum::<usize>().max(1)
    }

    /// How many frames may be unacknowledged before captures are skipped: one round
    /// trip's worth at the sending rate, plus a little, never under the floor.
    fn in_flight_cap(&self, rtt_ms: u32) -> usize {
        (rtt_ms as usize * self.sending_fps() / 1000 + 2).max(IN_FLIGHT_MIN)
    }

    /// The ceiling a track gets on this path, and beside its sibling.
    fn cap_for(&self, kind: VideoKind, relay: bool, both: bool) -> usize {
        match kind {
            VideoKind::Camera => {
                let cap = if relay { CAMERA_RELAY_CAP } else { CAMERA_LADDER.len() - 1 };
                if both { cap.min(CAMERA_BESIDE_SCREEN_CAP) } else { cap }
            }
            VideoKind::Screen => {
                if relay { SCREEN_RELAY_CAP } else { SCREEN_LADDER.len() - 1 }
            }
        }
    }
}

pub struct VideoTrack {
    shared: Arc<Shared>,
    tasks: Vec<JoinHandle<()>>,
    pub stats: Arc<VideoStats>,
}

impl VideoTrack {
    pub fn start(conn: Connection, hooks: Hooks) -> Self {
        let stats = Arc::new(VideoStats::default());
        let hooks = Arc::new(hooks);
        let shared = Arc::new(Shared {
            stats: Arc::clone(&stats),
            to_web: Mutex::new(None),
            in_flight: AtomicUsize::new(0),
            need_key_out: [AtomicBool::new(false), AtomicBool::new(false)],
            need_key_in: [AtomicBool::new(true), AtomicBool::new(true)],
            last_key_request: [Mutex::new(None), Mutex::new(None)],
            last_key_forced: [Mutex::new(None), Mutex::new(None)],
            codec: Mutex::new(None),
            peer: Mutex::new(Tracks::default()),
            peer_video: AtomicBool::new(hooks.peer_video),
            conn: conn.clone(),
            rate: Mutex::new([None, None]),
            prefs: Mutex::new([Prefs::default(), Prefs::default()]),
        });
        let (taker_tx, taker_rx) = mpsc::channel::<LinkConn>(1);
        link::set_taker(Some(taker_tx));

        let out = {
            let shared = Arc::clone(&shared);
            let hooks = Arc::clone(&hooks);
            let conn = conn.clone();
            vector_core::db::spawn_bound(async move { outbound(conn, shared, hooks, taker_rx).await })
        };
        let inn = {
            let shared = Arc::clone(&shared);
            let hooks = Arc::clone(&hooks);
            let conn = conn.clone();
            vector_core::db::spawn_bound(async move { inbound(conn, shared, hooks).await })
        };
        let rate = {
            let shared = Arc::clone(&shared);
            let hooks = Arc::clone(&hooks);
            vector_core::db::spawn_bound(async move { rate_loop(conn, shared, hooks).await })
        };
        Self { shared, tasks: vec![out, inn, rate], stats }
    }

    /// What we send from now on. A track that turns on gets a fresh ladder on a rung
    /// the path allows (and the user's pin, if any), and the webview hears its size
    /// and rate before any frame; a sibling's ceiling moves when the pair changes.
    pub fn set_sending(&self, tracks: Tracks) {
        let (relay, _) = self.shared.path();
        let prefs = *self.shared.prefs.lock().unwrap_or_else(|e| e.into_inner());
        let mut rates = self.shared.rate.lock().unwrap_or_else(|e| e.into_inner());
        let both = tracks.camera && tracks.screen;
        for kind in KINDS {
            let i = kind.index();
            let cap = self.shared.cap_for(kind, relay, both);
            match (tracks.has(kind), rates[i].as_mut()) {
                (false, _) => rates[i] = None,
                (true, Some(rate)) => {
                    if let Some(r) = rate.set_cap(cap) {
                        self.shared.tell_rung(kind, r);
                    }
                }
                (true, None) => {
                    let mut rate = match kind {
                        VideoKind::Camera => VideoRate::camera(cap),
                        VideoKind::Screen => VideoRate::screen(cap),
                    };
                    rate.set_pin(prefs[i].rung);
                    rate.set_fps(prefs[i].fps);
                    self.shared.tell_rung(kind, rate.rung());
                    self.shared.need_key_out[i].store(true, Ordering::Relaxed);
                    rates[i] = Some(rate);
                }
            }
        }
    }

    /// The user's choice for a track, kept for the next time it turns on too.
    pub fn set_prefs(&self, kind: VideoKind, prefs: Prefs) {
        let i = kind.index();
        self.shared.prefs.lock().unwrap_or_else(|e| e.into_inner())[i] = prefs;
        let mut rates = self.shared.rate.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(rate) = rates[i].as_mut() {
            let moved = rate.set_pin(prefs.rung);
            let r = rate.set_fps(prefs.fps);
            let _ = moved;
            self.shared.tell_rung(kind, r);
        }
    }

    /// The codec the peer decodes; told to the webview whenever it connects.
    pub fn set_codec(&self, codec: VideoCodec) {
        *self.shared.codec.lock().unwrap_or_else(|e| e.into_inner()) = Some(codec);
        self.shared.tell_web(&ToLink::Codec { codec });
    }

    pub fn set_peer(&self, tracks: Tracks) {
        let before = std::mem::replace(&mut *self.shared.peer.lock().unwrap_or_else(|e| e.into_inner()), tracks);
        for kind in KINDS {
            // A track that went off and comes back starts a fresh chain.
            if before.has(kind) && !tracks.has(kind) {
                self.shared.need_key_in[kind.index()].store(true, Ordering::Relaxed);
            }
        }
        self.shared.tell_web(&ToLink::Peer { tracks });
    }

    /// The peer cannot see us (or can again).
    pub fn set_paused(&self, on: bool) {
        self.shared.tell_web(&ToLink::Pause { on });
    }

    /// The peer asked for a keyframe on that track. Honoured once a second: a peer
    /// asking at wire rate would otherwise turn every frame into a keyframe.
    pub fn force_keyframe(&self, kind: VideoKind) {
        let i = kind.index();
        {
            let mut last = self.shared.last_key_forced[i].lock().unwrap_or_else(|e| e.into_inner());
            if last.is_some_and(|t| t.elapsed() < KEYFRAME_MIN_GAP) {
                return;
            }
            *last = Some(Instant::now());
        }
        self.shared.need_key_out[i].store(true, Ordering::Relaxed);
        self.shared.tell_web(&ToLink::Keyframe { kind });
    }

    /// The panel's view of each track's ladder.
    pub fn snapshot(&self) -> VideoSnapshot {
        let l = |a: &AtomicU64| a.load(Ordering::Relaxed);
        let rates = self.shared.rate.lock().unwrap_or_else(|e| e.into_inner());
        let prefs = self.shared.prefs.lock().unwrap_or_else(|e| e.into_inner());
        let track = |kind: VideoKind| {
            let i = kind.index();
            let t = self.stats.track(kind);
            let (rung, rungs) = rates[i].as_ref().map_or((0, 0), |r| {
                let current = r.rung();
                let ladder: &[Rung] = if r.is_camera() { &CAMERA_LADDER } else { &SCREEN_LADDER };
                let idx = ladder.iter().position(|x| x.kbps == current.kbps).unwrap_or(0);
                (idx as u32, r.rungs() as u32)
            });
            TrackSnapshot {
                fps: t.fps.load(Ordering::Relaxed),
                kbps: t.kbps.load(Ordering::Relaxed),
                width: t.width.load(Ordering::Relaxed),
                height: t.height.load(Ordering::Relaxed),
                rung,
                rungs,
                pinned: prefs[i].rung.is_some(),
            }
        };
        VideoSnapshot {
            sent: l(&self.stats.sent),
            bytes_out: l(&self.stats.bytes_out),
            skipped: l(&self.stats.skipped),
            stale: l(&self.stats.stale),
            received: l(&self.stats.received),
            bytes_in: l(&self.stats.bytes_in),
            dropped: l(&self.stats.dropped),
            key_requests: l(&self.stats.key_requests),
            camera: track(VideoKind::Camera),
            screen: track(VideoKind::Screen),
        }
    }
}

impl Drop for VideoTrack {
    fn drop(&mut self) {
        link::set_taker(None);
        for t in self.tasks.drain(..) {
            t.abort();
        }
        // Dropping the sender closes the webview's socket, which stops its captures.
        self.shared.to_web.lock().unwrap_or_else(|e| e.into_inner()).take();
    }
}

/// Sockets from the webview, one at a time; each one's frames go out on the connection.
async fn outbound(conn: Connection, shared: Arc<Shared>, hooks: Arc<Hooks>, mut taker: mpsc::Receiver<LinkConn>) {
    while let Some(mut link) = taker.recv().await {
        *shared.to_web.lock().unwrap_or_else(|e| e.into_inner()) = Some(link.to_web.clone());
        // Fresh decoders on the other side of the socket need keyframes to start on.
        for kind in KINDS {
            shared.need_key_in[kind.index()].store(true, Ordering::Relaxed);
            request_keyframe(&shared, &hooks, kind).await;
        }
        let codec = *shared.codec.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(codec) = codec {
            shared.tell_web(&ToLink::Codec { codec });
        }
        let tracks = *shared.peer.lock().unwrap_or_else(|e| e.into_inner());
        shared.tell_web(&ToLink::Peer { tracks });
        {
            let rates = shared.rate.lock().unwrap_or_else(|e| e.into_inner());
            for kind in KINDS {
                if let Some(r) = rates[kind.index()].as_ref() {
                    shared.tell_rung(kind, r.rung());
                }
            }
        }

        while let Some(msg) = link.from_web.recv().await {
            match parse(&msg) {
                Some(LinkMsg::Frame { header, .. }) => {
                    ship(&conn, &shared, msg.slice(1..), header);
                }
                Some(LinkMsg::Control(c)) => match c {
                    FromLink::Caps { encode, decode } => (hooks.on_caps)(encode, decode),
                    FromLink::Stats { kind, fps, kbps, width, height } => {
                        let t = shared.stats.track(kind);
                        t.fps.store(fps, Ordering::Relaxed);
                        t.kbps.store(kbps, Ordering::Relaxed);
                        t.width.store(width, Ordering::Relaxed);
                        t.height.store(height, Ordering::Relaxed);
                    }
                    FromLink::Lost { kind } => {
                        shared.need_key_in[kind.index()].store(true, Ordering::Relaxed);
                        request_keyframe(&shared, &hooks, kind).await;
                    }
                    FromLink::Unsupported { codec } => {
                        if shared.peer_video.load(Ordering::Relaxed) {
                            let mut send = hooks.control.lock().await;
                            let _ = tokio::time::timeout(Duration::from_secs(1), super::transport::write_control(&mut *send, &Control::VideoUnsupported { codec })).await;
                        }
                    }
                    FromLink::Unknown => {}
                },
                None => {}
            }
        }
        shared.to_web.lock().unwrap_or_else(|e| e.into_inner()).take();
        (hooks.on_link_closed)(&hooks.call_id);
    }
}

/// One frame onto one stream, on its own task, so a slow frame never holds the next.
fn ship(conn: &Connection, shared: &Arc<Shared>, frame: Bytes, header: VideoHeader) {
    let kind = VideoKind::from_flags(header.flags);
    let i = kind.index();
    if shared.need_key_out[i].load(Ordering::Relaxed) {
        if header.is_key() {
            shared.need_key_out[i].store(false, Ordering::Relaxed);
        } else {
            shared.stats.skipped.fetch_add(1, Ordering::Relaxed);
            return;
        }
    }
    let (_, rtt_ms) = shared.path();
    let cap = shared.in_flight_cap(rtt_ms);
    // An encoded frame is never dropped: a delta the peer never sees breaks the chain.
    // Instead the webview is told to skip captures while the wire is this full, so
    // what it drops was never encoded.
    if shared.in_flight.load(Ordering::Relaxed) + 1 >= cap {
        shared.stats.skipped.fetch_add(1, Ordering::Relaxed);
        shared.tell_web(&ToLink::Skip { kind });
    }
    shared.in_flight.fetch_add(1, Ordering::Relaxed);
    let conn = conn.clone();
    let shared = Arc::clone(shared);
    let len = frame.len() as u64;
    let key = header.is_key();
    let rtt = Duration::from_millis(rtt_ms as u64);
    let stale = if key { STALE_KEY_MIN.max(rtt * 4) } else { STALE_MIN.max(rtt * 3) };
    vector_core::db::spawn_bound(async move {
        let result = async {
            let mut s = tokio::time::timeout(stale, conn.open_uni()).await.map_err(|_| "open")?.map_err(|_| "open")?;
            let _ = s.set_priority(if key { 1 } else { 0 });
            if tokio::time::timeout(stale, s.write_chunk(frame)).await.is_err() {
                let _ = s.reset(VarInt::from_u32(1));
                return Err("write");
            }
            if s.finish().is_err() {
                return Err("finish");
            }
            if tokio::time::timeout(stale, s.stopped()).await.is_err() {
                let _ = s.reset(VarInt::from_u32(1));
                return Err("stale");
            }
            Ok(())
        }
        .await;
        shared.in_flight.fetch_sub(1, Ordering::Relaxed);
        match result {
            Ok(()) => {
                shared.stats.sent.fetch_add(1, Ordering::Relaxed);
                shared.stats.bytes_out.fetch_add(len, Ordering::Relaxed);
            }
            Err(_) => {
                shared.stats.stale.fetch_add(1, Ordering::Relaxed);
                shared.need_key_out[i].store(true, Ordering::Relaxed);
                shared.tell_web(&ToLink::Keyframe { kind });
            }
        }
    });
}

/// Once a second while we send: the connection's own loss and round trip, the audio
/// track's refused datagrams and our own backlog decide each rung; the path's kind
/// decides the ceilings.
async fn rate_loop(conn: Connection, shared: Arc<Shared>, hooks: Arc<Hooks>) {
    let mut last_stale = 0u64;
    let mut was_relay: Option<bool> = None;
    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let (relay, rtt_ms) = shared.path();
        let stale = shared.stats.stale.load(Ordering::Relaxed);
        let backlog = stale > last_stale || shared.in_flight.load(Ordering::Relaxed) >= shared.in_flight_cap(rtt_ms);
        last_stale = stale;
        let net = conn.stats();
        let obs = VideoObservation {
            sent_packets: net.udp_tx.datagrams,
            lost_packets: net.lost_packets,
            rtt_ms,
            audio_send_dropped: hooks.audio.send_dropped.load(Ordering::Relaxed),
            backlog,
        };
        let path_changed = was_relay != Some(relay);
        was_relay = Some(relay);
        let mut changes: Vec<(VideoKind, Rung)> = Vec::new();
        {
            let mut rates = shared.rate.lock().unwrap_or_else(|e| e.into_inner());
            let both = rates.iter().all(|r| r.is_some());
            for kind in KINDS {
                let Some(rate) = rates[kind.index()].as_mut() else { continue };
                let capped = if path_changed { rate.set_cap(shared.cap_for(kind, relay, both)) } else { None };
                if let Some(r) = rate.observe(obs).or(capped) {
                    changes.push((kind, r));
                }
            }
        }
        for (kind, r) in changes {
            shared.tell_rung(kind, r);
        }
    }
}

async fn request_keyframe(shared: &Shared, hooks: &Hooks, kind: VideoKind) {
    if !shared.peer_video.load(Ordering::Relaxed) {
        return;
    }
    {
        let mut last = shared.last_key_request[kind.index()].lock().unwrap_or_else(|e| e.into_inner());
        if last.is_some_and(|t| t.elapsed() < KEYFRAME_MIN_GAP) {
            return;
        }
        *last = Some(Instant::now());
    }
    shared.stats.key_requests.fetch_add(1, Ordering::Relaxed);
    let mut send = hooks.control.lock().await;
    let _ = tokio::time::timeout(Duration::from_secs(1), super::transport::write_control(&mut *send, &Control::KeyframeRequest { kind })).await;
}

/// The peer's streams, in order, each one a frame for the webview.
async fn inbound(conn: Connection, shared: Arc<Shared>, hooks: Arc<Hooks>) {
    let mut next_seq: [Option<u32>; 2] = [None, None];
    loop {
        let Ok(mut stream) = conn.accept_uni().await else { break };
        let mut buf = BytesMut::with_capacity(VIDEO_HEADER_LEN + 1 + 64 * 1024);
        buf.extend_from_slice(&[KIND_FRAME]);
        let mut whole = true;
        let deadline = Instant::now() + INBOUND_FRAME_TIMEOUT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let read = match tokio::time::timeout(left, stream.read_chunk(MAX_VIDEO_FRAME)).await {
                Ok(r) => r,
                Err(_) => {
                    let _ = stream.stop(VarInt::from_u32(3));
                    whole = false;
                    break;
                }
            };
            match read {
                Ok(Some(chunk)) => {
                    if buf.len() + chunk.len() > MAX_VIDEO_FRAME + 1 {
                        let _ = stream.stop(VarInt::from_u32(2));
                        whole = false;
                        break;
                    }
                    buf.extend_from_slice(&chunk);
                }
                Ok(None) => break,
                Err(_) => {
                    whole = false;
                    break;
                }
            }
        }
        // A reset or oversized frame still says which track it was if its header
        // arrived; without one, both chains restart on their next keyframe.
        let Some(header) = VideoHeader::parse(&buf[1..]) else {
            shared.stats.dropped.fetch_add(1, Ordering::Relaxed);
            for kind in KINDS {
                shared.need_key_in[kind.index()].store(true, Ordering::Relaxed);
                request_keyframe(&shared, &hooks, kind).await;
            }
            continue;
        };
        let kind = VideoKind::from_flags(header.flags);
        let i = kind.index();
        if !whole {
            shared.stats.dropped.fetch_add(1, Ordering::Relaxed);
            shared.need_key_in[i].store(true, Ordering::Relaxed);
            request_keyframe(&shared, &hooks, kind).await;
            continue;
        }
        if let Some(expected) = next_seq[i] {
            if header.seq != expected && !header.is_key() {
                shared.need_key_in[i].store(true, Ordering::Relaxed);
            }
        }
        next_seq[i] = Some(header.seq.wrapping_add(1));
        if shared.need_key_in[i].load(Ordering::Relaxed) {
            if header.is_key() {
                shared.need_key_in[i].store(false, Ordering::Relaxed);
            } else {
                shared.stats.dropped.fetch_add(1, Ordering::Relaxed);
                request_keyframe(&shared, &hooks, kind).await;
                continue;
            }
        }
        let len = buf.len() as u64;
        let tx = shared.to_web.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let delivered = match tx {
            Some(tx) => tx.try_send(buf.freeze()).is_ok(),
            None => false,
        };
        if delivered {
            shared.stats.received.fetch_add(1, Ordering::Relaxed);
            shared.stats.bytes_in.fetch_add(len, Ordering::Relaxed);
        } else {
            // The webview is not keeping up (or is not there): resume on a keyframe.
            shared.stats.dropped.fetch_add(1, Ordering::Relaxed);
            shared.need_key_in[i].store(true, Ordering::Relaxed);
        }
    }
}
