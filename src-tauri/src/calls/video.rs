//! The video track of a call: frames from the webview go out one QUIC stream each,
//! frames from the peer's streams go back to the webview, and the two latches keep
//! a lost or reset frame from ever painting a corrupt picture.

use super::link::{self, LinkConn};
use super::transport::{Control, VideoCodec, VideoHeader, VideoKind, MAX_VIDEO_FRAME, VIDEO_HEADER_LEN};
use bytes::{Bytes, BytesMut};
use iroh::endpoint::{Connection, SendStream, VarInt};
use serde::Serialize;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use vector_core::calls::link::{control, parse, FromLink, LinkMsg, ToLink, KIND_FRAME};

/// Frames on the wire at once before captures are skipped.
const IN_FLIGHT: usize = 3;
/// A frame the peer has not read by then is reset: it is a picture of the past.
const STALE: Duration = Duration::from_millis(400);
/// Keyframes cost tens of kilobytes; requests coalesce to this many.
const KEYFRAME_MIN_GAP: Duration = Duration::from_secs(1);

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
    /// The encoder's own numbers, as the webview reports them.
    pub enc_fps: AtomicU32,
    pub enc_kbps: AtomicU32,
    pub enc_width: AtomicU32,
    pub enc_height: AtomicU32,
}

/// The stats line's view of the track.
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
    pub fps: u32,
    pub kbps: u32,
    pub width: u32,
    pub height: u32,
}

impl VideoStats {
    pub fn snapshot(&self) -> VideoSnapshot {
        let l = |a: &AtomicU64| a.load(Ordering::Relaxed);
        let s = |a: &AtomicU32| a.load(Ordering::Relaxed);
        VideoSnapshot {
            sent: l(&self.sent),
            bytes_out: l(&self.bytes_out),
            skipped: l(&self.skipped),
            stale: l(&self.stale),
            received: l(&self.received),
            bytes_in: l(&self.bytes_in),
            dropped: l(&self.dropped),
            key_requests: l(&self.key_requests),
            fps: s(&self.enc_fps),
            kbps: s(&self.enc_kbps),
            width: s(&self.enc_width),
            height: s(&self.enc_height),
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
}

struct Shared {
    stats: Arc<VideoStats>,
    to_web: Mutex<Option<mpsc::Sender<Bytes>>>,
    in_flight: AtomicUsize,
    /// The next outgoing frame must be a keyframe: a delta was reset or skipped after encoding.
    need_key_out: AtomicBool,
    /// Drop incoming deltas until a keyframe: the chain broke, or the webview just reconnected.
    need_key_in: AtomicBool,
    last_key_request: Mutex<Option<Instant>>,
    codec: Mutex<Option<VideoCodec>>,
    peer_kind: Mutex<VideoKind>,
}

impl Shared {
    fn tell_web(&self, msg: &ToLink) {
        let tx = self.to_web.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(tx) = tx {
            let _ = tx.try_send(Bytes::from(control(msg)));
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
        let shared = Arc::new(Shared {
            stats: Arc::clone(&stats),
            to_web: Mutex::new(None),
            in_flight: AtomicUsize::new(0),
            need_key_out: AtomicBool::new(false),
            need_key_in: AtomicBool::new(true),
            last_key_request: Mutex::new(None),
            codec: Mutex::new(None),
            peer_kind: Mutex::new(VideoKind::Off),
        });
        let hooks = Arc::new(hooks);
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
            vector_core::db::spawn_bound(async move { inbound(conn, shared, hooks).await })
        };
        Self { shared, tasks: vec![out, inn], stats }
    }

    /// The codec the peer decodes; told to the webview whenever it connects.
    pub fn set_codec(&self, codec: VideoCodec) {
        *self.shared.codec.lock().unwrap_or_else(|e| e.into_inner()) = Some(codec);
        self.shared.tell_web(&ToLink::Codec { codec });
    }

    pub fn set_peer_kind(&self, kind: VideoKind) {
        *self.shared.peer_kind.lock().unwrap_or_else(|e| e.into_inner()) = kind;
        if kind == VideoKind::Off {
            // Whatever comes next starts a fresh chain.
            self.shared.need_key_in.store(true, Ordering::Relaxed);
        }
        self.shared.tell_web(&ToLink::Peer { kind });
    }

    /// The peer cannot see us (or can again).
    pub fn set_paused(&self, on: bool) {
        self.shared.tell_web(&ToLink::Pause { on });
    }

    /// The peer asked for a keyframe.
    pub fn force_keyframe(&self) {
        self.shared.need_key_out.store(true, Ordering::Relaxed);
        self.shared.tell_web(&ToLink::Keyframe);
    }
}

impl Drop for VideoTrack {
    fn drop(&mut self) {
        link::set_taker(None);
        for t in self.tasks.drain(..) {
            t.abort();
        }
        // Dropping the sender closes the webview's socket, which stops its camera.
        self.shared.to_web.lock().unwrap_or_else(|e| e.into_inner()).take();
    }
}

/// Sockets from the webview, one at a time; each one's frames go out on the connection.
async fn outbound(conn: Connection, shared: Arc<Shared>, hooks: Arc<Hooks>, mut taker: mpsc::Receiver<LinkConn>) {
    while let Some(mut link) = taker.recv().await {
        *shared.to_web.lock().unwrap_or_else(|e| e.into_inner()) = Some(link.to_web.clone());
        // A fresh decoder on the other side of the socket needs a keyframe to start on.
        shared.need_key_in.store(true, Ordering::Relaxed);
        request_keyframe(&shared, &hooks).await;
        let codec = *shared.codec.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(codec) = codec {
            shared.tell_web(&ToLink::Codec { codec });
        }
        let kind = *shared.peer_kind.lock().unwrap_or_else(|e| e.into_inner());
        shared.tell_web(&ToLink::Peer { kind });

        while let Some(msg) = link.from_web.recv().await {
            match parse(&msg) {
                Some(LinkMsg::Frame { header, .. }) => {
                    ship(&conn, &shared, msg.slice(1..), header);
                }
                Some(LinkMsg::Control(c)) => match c {
                    FromLink::Caps { encode, decode } => (hooks.on_caps)(encode, decode),
                    FromLink::Stats { fps, kbps, width, height } => {
                        shared.stats.enc_fps.store(fps, Ordering::Relaxed);
                        shared.stats.enc_kbps.store(kbps, Ordering::Relaxed);
                        shared.stats.enc_width.store(width, Ordering::Relaxed);
                        shared.stats.enc_height.store(height, Ordering::Relaxed);
                    }
                    FromLink::Lost => {
                        shared.need_key_in.store(true, Ordering::Relaxed);
                        request_keyframe(&shared, &hooks).await;
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
    if shared.need_key_out.load(Ordering::Relaxed) {
        if header.is_key() {
            shared.need_key_out.store(false, Ordering::Relaxed);
        } else {
            shared.stats.skipped.fetch_add(1, Ordering::Relaxed);
            return;
        }
    }
    if shared.in_flight.load(Ordering::Relaxed) >= IN_FLIGHT {
        shared.stats.skipped.fetch_add(1, Ordering::Relaxed);
        // The delta chain is intact only if the encoder never encoded what we drop, so
        // the next capture is skipped instead and this encoded frame forces a keyframe.
        shared.need_key_out.store(true, Ordering::Relaxed);
        shared.tell_web(&ToLink::Skip);
        shared.tell_web(&ToLink::Keyframe);
        return;
    }
    shared.in_flight.fetch_add(1, Ordering::Relaxed);
    let conn = conn.clone();
    let shared = Arc::clone(shared);
    let len = frame.len() as u64;
    vector_core::db::spawn_bound(async move {
        let key = header.is_key();
        let result = async {
            let mut s = tokio::time::timeout(STALE, conn.open_uni()).await.map_err(|_| "open")?.map_err(|_| "open")?;
            let _ = s.set_priority(if key { 1 } else { 0 });
            if tokio::time::timeout(STALE, s.write_chunk(frame)).await.is_err() {
                let _ = s.reset(VarInt::from_u32(1));
                return Err("write");
            }
            if s.finish().is_err() {
                return Err("finish");
            }
            if tokio::time::timeout(STALE, s.stopped()).await.is_err() {
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
                shared.need_key_out.store(true, Ordering::Relaxed);
                shared.tell_web(&ToLink::Keyframe);
            }
        }
    });
}

async fn request_keyframe(shared: &Shared, hooks: &Hooks) {
    {
        let mut last = shared.last_key_request.lock().unwrap_or_else(|e| e.into_inner());
        if last.is_some_and(|t| t.elapsed() < KEYFRAME_MIN_GAP) {
            return;
        }
        *last = Some(Instant::now());
    }
    shared.stats.key_requests.fetch_add(1, Ordering::Relaxed);
    let mut send = hooks.control.lock().await;
    let _ = tokio::time::timeout(Duration::from_secs(1), super::transport::write_control(&mut *send, &Control::KeyframeRequest)).await;
}

/// The peer's streams, in order, each one a frame for the webview.
async fn inbound(conn: Connection, shared: Arc<Shared>, hooks: Arc<Hooks>) {
    let mut next_seq: Option<u32> = None;
    loop {
        let Ok(mut stream) = conn.accept_uni().await else { break };
        let mut buf = BytesMut::with_capacity(VIDEO_HEADER_LEN + 1 + 64 * 1024);
        buf.extend_from_slice(&[KIND_FRAME]);
        let mut whole = true;
        loop {
            match stream.read_chunk(MAX_VIDEO_FRAME).await {
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
        let header = if whole { VideoHeader::parse(&buf[1..]) } else { None };
        let Some(header) = header else {
            // A reset or oversized frame: the chain is broken until the next keyframe.
            shared.stats.dropped.fetch_add(1, Ordering::Relaxed);
            shared.need_key_in.store(true, Ordering::Relaxed);
            request_keyframe(&shared, &hooks).await;
            continue;
        };
        if let Some(expected) = next_seq {
            if header.seq != expected && !header.is_key() {
                shared.need_key_in.store(true, Ordering::Relaxed);
            }
        }
        next_seq = Some(header.seq.wrapping_add(1));
        if shared.need_key_in.load(Ordering::Relaxed) {
            if header.is_key() {
                shared.need_key_in.store(false, Ordering::Relaxed);
            } else {
                shared.stats.dropped.fetch_add(1, Ordering::Relaxed);
                request_keyframe(&shared, &hooks).await;
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
            shared.need_key_in.store(true, Ordering::Relaxed);
        }
    }
}
