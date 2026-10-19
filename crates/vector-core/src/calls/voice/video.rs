//! Cameras and shared screens in a voice Channel's call (CORD-07 §6, §8). The webview captures and
//! encodes each picture once; each frame goes to every member who asked for that picture, one QUIC
//! stream per frame, as in a 1:1 call. A member only receives what it shows, so a call of several
//! costs a sender one upload per viewer, not per member.
//!
//! One viewer never holds the rest back: a frame is never queued behind a viewer's stream credit,
//! a viewer that keeps falling behind stops steering the shared rate and the keyframe cadence, and
//! catches up from the next keyframe.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::{Bytes, BytesMut};
use futures_util::FutureExt;
use iroh::endpoint::{Connection, RecvStream, VarInt};
use nostr_sdk::prelude::PublicKey;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use web_time::Instant;

use super::Voice;
use crate::calls::link::{control, parse, FromLink, LinkMsg, ToLink, KIND_PEER_FRAME};
use crate::calls::platform::LinkConn;
use crate::calls::rate::{Rung, CAMERA_LADDER, SCREEN_LADDER, SCREEN_RELAY_CAP};
use crate::calls::wire::{Tracks, VideoCodec, VideoHeader, VideoKind, MAX_VIDEO_FRAME, VIDEO_HEADER_LEN};
use crate::rt::time::timeout;

/// A unidirectional stream's first byte: what it carries.
const STREAM_FRAME: u8 = 1;
const STREAM_CONTROL: u8 = 2;
const MAX_CONTROL: usize = 1024;

const KINDS: [VideoKind; 2] = [VideoKind::Camera, VideoKind::Screen];
/// A viewer's backlog: one acknowledgement's worth of frames at the sending rate, plus this slack;
/// beyond it the viewer skips to the next keyframe.
const IN_FLIGHT_SLACK: usize = 3;
const IN_FLIGHT_MIN: usize = 4;
const ACK_SLACK_MS: u64 = 60;
const STALE_MIN: Duration = Duration::from_millis(400);
const STALE_KEY_MIN: Duration = Duration::from_secs(2);
const INBOUND_FRAME_TIMEOUT: Duration = Duration::from_secs(2);
/// How long a frame waits for a busy webview before that picture restarts from a keyframe.
const TO_WEB_WAIT: Duration = Duration::from_millis(250);
/// Keyframes are forced, and asked of a sender, this often at most; one viewer's own requests are
/// honoured less often, and a viewer still waiting gets one at least this often regardless.
const KEY_GAP: Duration = Duration::from_secs(1);
const VIEWER_KEY_GAP: Duration = Duration::from_secs(3);
/// Seconds in a row with losses before a viewer counts as behind.
const BEHIND_AFTER_SECS: u32 = 3;
/// A watched picture its sender says is on, with no frame this long, is asked for again.
const REWATCH_AFTER: Duration = Duration::from_secs(4);
/// Where each picture starts, and how far it may climb through relays and when every viewer is
/// reached directly.
const SCREEN_START: usize = 2;
const SCREEN_DIRECT_CAP: usize = 5;
const CAMERA_START: usize = 2;
const CAMERA_RELAY_CAP: usize = 3;
const CAMERA_DIRECT_CAP: usize = 5;
const CLIMB_AFTER_SECS: u32 = 8;

/// What a viewer and a sender say to each other.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "t", rename_all = "snake_case")]
enum Ctl {
    /// Send me these of your pictures from now on; neither means stop.
    Watch {
        #[serde(default)]
        camera: bool,
        #[serde(default)]
        screen: bool,
    },
    /// My decoder for that picture lost the chain.
    Key { kind: VideoKind },
    /// My decoder can't take this codec: send another.
    Unsupported { codec: VideoCodec },
    #[serde(other)]
    Unknown,
}

/// One viewer: what it asked for, and how it is keeping up.
pub(super) struct Out {
    want: Mutex<Tracks>,
    in_flight: AtomicUsize,
    need_key: [AtomicBool; 2],
    /// Losses this second, and seconds in a row that had some.
    stale: AtomicU32,
    bad_secs: AtomicU32,
    /// Too far behind to steer the rate or force keyframes; it catches up on the next keyframe.
    behind: AtomicBool,
    last_key_request: Mutex<Option<Instant>>,
}

impl Out {
    fn new() -> Self {
        Self {
            want: Mutex::new(Tracks::default()),
            in_flight: AtomicUsize::new(0),
            need_key: [AtomicBool::new(true), AtomicBool::new(true)],
            stale: AtomicU32::new(0),
            bad_secs: AtomicU32::new(0),
            behind: AtomicBool::new(false),
            last_key_request: Mutex::new(None),
        }
    }

    fn wants(&self, kind: VideoKind) -> bool {
        self.want.lock().unwrap_or_else(|e| e.into_inner()).has(kind)
    }
}

#[derive(Default)]
pub(super) struct Video {
    to_web: Mutex<Option<mpsc::Sender<Bytes>>>,
    /// What we send.
    sending: Mutex<Tracks>,
    /// Who asked for which of our pictures.
    watchers: Mutex<HashMap<[u8; 32], Arc<Out>>>,
    /// Each link's controls, sent in order on it.
    controls: Mutex<HashMap<[u8; 32], mpsc::UnboundedSender<Ctl>>>,
    /// Which pictures we asked for, by member: one who reconnects is asked again.
    watching: Mutex<HashMap<PublicKey, Tracks>>,
    watch_since: Mutex<HashMap<PublicKey, Instant>>,
    /// Pictures whose next frame we take must be a keyframe, and when each last showed a frame.
    resync: Mutex<HashSet<(PublicKey, usize)>>,
    last_frame: Mutex<HashMap<(PublicKey, usize), Instant>>,
    last_rewatch: Mutex<HashMap<PublicKey, Instant>>,
    codec: Mutex<Option<VideoCodec>>,
    /// What our webview encodes, and the codecs a viewer has refused.
    encode: Mutex<Vec<String>>,
    refused: Mutex<Vec<VideoCodec>>,
    rung: [AtomicUsize; 2],
    /// A rate or codec the webview hasn't taken yet: its queue was full.
    rate_unsent: [AtomicBool; 2],
    codec_unsent: AtomicBool,
    clean_secs: AtomicU32,
    /// The viewer count moved: the next tick repaints, however often a peer flips it.
    state_dirty: AtomicBool,
    last_key_forced: [Mutex<Option<Instant>>; 2],
    last_key_asked: Mutex<HashMap<(PublicKey, usize), Instant>>,
}

fn ladder(kind: VideoKind) -> &'static [Rung] {
    match kind {
        VideoKind::Camera => &CAMERA_LADDER,
        VideoKind::Screen => &SCREEN_LADDER,
    }
}

/// What a second's losses do to a picture's rung: `stale` of the `active` viewers (those not
/// behind) lost frames. Down when a third of them did; up after a run of clean seconds.
#[derive(Debug, PartialEq, Eq)]
enum Step {
    Down,
    Up,
    Hold,
}

fn step(stale: usize, active: usize, clean_secs: u32) -> Step {
    if active == 0 {
        Step::Hold
    } else if stale > 0 && stale >= active.div_ceil(3) {
        Step::Down
    } else if stale == 0 && clean_secs >= CLIMB_AFTER_SECS {
        Step::Up
    } else {
        Step::Hold
    }
}

impl Video {
    pub(super) fn sending(&self) -> Tracks {
        *self.sending.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// How many watch our screen while we share it.
    pub(super) fn viewers(&self) -> u32 {
        if !self.sending().screen {
            return 0;
        }
        self.outs().iter().filter(|o| o.wants(VideoKind::Screen)).count() as u32
    }

    fn outs(&self) -> Vec<Arc<Out>> {
        self.watchers.lock().unwrap_or_else(|e| e.into_inner()).values().cloned().collect()
    }

    #[cfg(test)]
    pub(super) fn add_watcher(&self, id: [u8; 32]) {
        let out = Out::new();
        *out.want.lock().unwrap() = Tracks { camera: true, screen: true };
        self.watchers.lock().unwrap().insert(id, Arc::new(out));
    }

    #[cfg(test)]
    pub(super) fn set_sending_for_test(&self, tracks: Tracks) {
        *self.sending.lock().unwrap() = tracks;
    }

    /// Whether the webview took it; a full queue drops it.
    fn tell(&self, msg: &ToLink) -> bool {
        let tx = self.to_web.lock().unwrap_or_else(|e| e.into_inner()).clone();
        tx.is_some_and(|tx| tx.try_send(Bytes::from(control(msg))).is_ok())
    }

    fn rung(&self, kind: VideoKind) -> Rung {
        let l = ladder(kind);
        l[self.rung[kind.index()].load(Ordering::SeqCst).min(l.len() - 1)]
    }

    fn tell_rung(&self, kind: VideoKind) {
        let r = self.rung(kind);
        let told = self.tell(&ToLink::Rate { kind, kbps: r.kbps, width: r.width, height: r.height, fps: r.fps });
        self.rate_unsent[kind.index()].store(!told, Ordering::SeqCst);
    }

    fn tell_codec(&self) {
        let codec = *self.codec.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(codec) = codec {
            self.codec_unsent.store(!self.tell(&ToLink::Codec { codec }), Ordering::SeqCst);
        }
    }

    /// One keyframe of that picture for every viewer, a second apart at most.
    fn force_key(&self, kind: VideoKind) {
        {
            let mut last = self.last_key_forced[kind.index()].lock().unwrap_or_else(|e| e.into_inner());
            if last.is_some_and(|t| t.elapsed() < KEY_GAP) {
                return;
            }
            *last = Some(Instant::now());
        }
        self.tell(&ToLink::Keyframe { kind });
    }

    /// Frames per second across what we send.
    fn sending_fps(&self) -> u64 {
        let s = self.sending();
        KINDS.iter().filter(|k| s.has(**k)).map(|k| self.rung(*k).fps as u64).sum::<u64>().max(1)
    }

    /// A viewer lost a frame of that picture: it waits for a keyframe, which only a viewer not
    /// already behind may force.
    fn lost(&self, out: &Out, kind: VideoKind) {
        out.need_key[kind.index()].store(true, Ordering::SeqCst);
        out.stale.fetch_add(1, Ordering::SeqCst);
        if !out.behind.load(Ordering::SeqCst) {
            self.force_key(kind);
        }
    }
}

/// Take the webview's video link for this call and keep the pictures' rates; the tasks end with it.
pub(super) fn start(v: &Arc<Voice>) -> Vec<crate::rt::JoinHandle<()>> {
    let Ok(platform) = crate::calls::platform::get() else { return Vec::new() };
    let (tx, rx) = mpsc::channel::<LinkConn>(1);
    platform.set_video_taker(Some(tx));
    vec![crate::db::spawn_bound(link_loop(v.clone(), rx)), crate::db::spawn_bound(rate_loop(v.clone()))]
}

/// Let go of the link; dropping its sender closes the socket, which stops the captures.
pub(super) fn stop(v: &Voice) {
    if let Ok(platform) = crate::calls::platform::get() {
        platform.set_video_taker(None);
    }
    v.video.to_web.lock().unwrap_or_else(|e| e.into_inner()).take();
    *v.video.sending.lock().unwrap_or_else(|e| e.into_inner()) = Tracks::default();
    v.video.controls.lock().unwrap_or_else(|e| e.into_inner()).clear();
}

/// A peer joined the mesh: its controls go out in order on this connection, we read what it sends
/// on streams, and if we watch it, this connection is asked for it.
pub(super) fn on_link(v: &Arc<Voice>, id: [u8; 32], author: PublicKey, conn: Connection) {
    let (tx, rx) = mpsc::unbounded_channel::<Ctl>();
    v.video.controls.lock().unwrap_or_else(|e| e.into_inner()).insert(id, tx);
    crate::db::spawn_bound(send_controls(conn.clone(), rx));
    let want = v.video.watching.lock().unwrap_or_else(|e| e.into_inner()).get(&author).copied();
    if let Some(want) = want.filter(|w| w.any()) {
        mark_resync(v, &author, want);
        send_ctl(v, &id, Ctl::Watch { camera: want.camera, screen: want.screen });
    }
    crate::db::spawn_bound(inbound(v.clone(), id, author, conn));
}

/// A peer's connection went: nothing goes to it now.
pub(super) fn on_unlink(v: &Voice, id: &[u8; 32]) {
    let gone = v.video.watchers.lock().unwrap_or_else(|e| e.into_inner()).remove(id);
    v.video.controls.lock().unwrap_or_else(|e| e.into_inner()).remove(id);
    if gone.is_some_and(|o| o.wants(VideoKind::Screen)) {
        v.video.state_dirty.store(true, Ordering::SeqCst);
    }
}

/// Start or stop sending a picture. Presence says so, so every member can see it.
pub(super) fn set_sending(v: &Arc<Voice>, kind: VideoKind, on: bool) -> Result<(), String> {
    if on && v.video.to_web.lock().unwrap_or_else(|e| e.into_inner()).is_none() {
        return Err("Video isn't ready in this call yet".into());
    }
    {
        let mut sending = v.video.sending.lock().unwrap_or_else(|e| e.into_inner());
        if sending.has(kind) == on {
            return Ok(());
        }
        *sending = sending.with(kind, on);
    }
    if on {
        let start = match kind {
            VideoKind::Camera => CAMERA_START,
            VideoKind::Screen => SCREEN_START,
        };
        v.video.rung[kind.index()].store(start, Ordering::SeqCst);
        v.video.clean_secs.store(0, Ordering::SeqCst);
        v.video.tell_rung(kind);
        *v.video.last_key_forced[kind.index()].lock().unwrap_or_else(|e| e.into_inner()) = None;
        v.video.force_key(kind);
    }
    for out in v.video.outs() {
        out.need_key[kind.index()].store(true, Ordering::SeqCst);
    }
    Ok(())
}

/// Ask a member for their pictures, or stop. Only one who may send video here is watched; one not
/// connected yet is asked once they are.
pub(super) fn watch(v: &Arc<Voice>, author: &PublicKey, want: Tracks) -> Result<(), String> {
    if want.any() {
        let community = super::held(&v.community)?;
        if !super::may_video(&community, &v.channel, author) {
            return Err("They can't share video here".into());
        }
    }
    let before = {
        let mut watching = v.video.watching.lock().unwrap_or_else(|e| e.into_inner());
        if want.any() { watching.insert(*author, want) } else { watching.remove(author) }
    }
    .unwrap_or_default();
    if before == want {
        return Ok(());
    }
    v.video.watch_since.lock().unwrap_or_else(|e| e.into_inner()).insert(*author, Instant::now());
    mark_resync(v, author, Tracks { camera: want.camera && !before.camera, screen: want.screen && !before.screen });
    if let Some(id) = endpoint_of(v, author) {
        send_ctl(v, &id, Ctl::Watch { camera: want.camera, screen: want.screen });
    }
    Ok(())
}

/// Drop watched pictures whose sender may no longer send video here.
pub(super) fn revalidate(v: &Voice, may_video: impl Fn(&PublicKey) -> bool) {
    v.video.watching.lock().unwrap_or_else(|e| e.into_inner()).retain(|author, _| may_video(author));
}

fn endpoint_of(v: &Voice, author: &PublicKey) -> Option<[u8; 32]> {
    v.links.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|(_, l)| l.author == *author).map(|(id, _)| *id)
}

fn mark_resync(v: &Voice, author: &PublicKey, kinds: Tracks) {
    let mut resync = v.video.resync.lock().unwrap_or_else(|e| e.into_inner());
    for kind in KINDS.into_iter().filter(|k| kinds.has(*k)) {
        resync.insert((*author, kind.index()));
    }
}

fn send_ctl(v: &Voice, id: &[u8; 32], ctl: Ctl) {
    if let Some(tx) = v.video.controls.lock().unwrap_or_else(|e| e.into_inner()).get(id) {
        let _ = tx.send(ctl);
    }
}

/// One link's controls, one stream each, in the order they were asked: a quick unwatch and watch
/// must not land the other way round.
async fn send_controls(conn: Connection, mut rx: mpsc::UnboundedReceiver<Ctl>) {
    while let Some(ctl) = rx.recv().await {
        let Ok(body) = serde_json::to_vec(&ctl) else { continue };
        let sent = async {
            let mut s = conn.open_uni().await.map_err(|e| e.to_string())?;
            let mut msg = Vec::with_capacity(1 + body.len());
            msg.push(STREAM_CONTROL);
            msg.extend_from_slice(&body);
            s.write_all(&msg).await.map_err(|e| e.to_string())?;
            s.finish().map_err(|e| e.to_string())
        };
        match timeout(Duration::from_secs(5), sent).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                crate::log_debug!("[voice] video control not sent: {e}");
                if conn.close_reason().is_some() {
                    return;
                }
            }
            Err(_) => crate::log_debug!("[voice] video control timed out"),
        }
    }
}

/// Ask a sender for a keyframe of one picture, a second apart at most.
fn ask_key(v: &Voice, author: &PublicKey, kind: VideoKind) {
    {
        let mut asked = v.video.last_key_asked.lock().unwrap_or_else(|e| e.into_inner());
        let key = (*author, kind.index());
        if asked.get(&key).is_some_and(|t| t.elapsed() < KEY_GAP) {
            return;
        }
        asked.insert(key, Instant::now());
    }
    if let Some(id) = endpoint_of(v, author) {
        send_ctl(v, &id, Ctl::Key { kind });
    }
}

/// Webview sockets, one at a time: our frames out to every viewer, its decoders' requests back.
async fn link_loop(v: Arc<Voice>, mut taker: mpsc::Receiver<LinkConn>) {
    while let Some(mut link) = taker.recv().await {
        *v.video.to_web.lock().unwrap_or_else(|e| e.into_inner()) = Some(link.to_web.clone());
        // Fresh decoders on the other side start from keyframes.
        let watching: Vec<(PublicKey, Tracks)> = v.video.watching.lock().unwrap_or_else(|e| e.into_inner()).iter().map(|(a, t)| (*a, *t)).collect();
        for (author, want) in watching {
            mark_resync(&v, &author, want);
            for kind in KINDS.into_iter().filter(|k| want.has(*k)) {
                ask_key(&v, &author, kind);
            }
        }
        v.video.tell_codec();
        let sending = v.video.sending();
        for kind in KINDS.into_iter().filter(|k| sending.has(*k)) {
            v.video.tell_rung(kind);
        }
        while let Some(msg) = link.from_web.recv().await {
            match parse(&msg) {
                Some(LinkMsg::Frame { header, .. }) => {
                    let kind = VideoKind::from_flags(header.flags);
                    if v.video.sending().has(kind) {
                        fan_out(&v, kind, msg.slice(1..), header);
                    }
                }
                Some(LinkMsg::Control(FromLink::Caps { encode, .. })) => {
                    *v.video.encode.lock().unwrap_or_else(|e| e.into_inner()) = encode;
                    *v.video.codec.lock().unwrap_or_else(|e| e.into_inner()) = choose_codec(&v.video);
                    v.video.tell_codec();
                }
                // Our decoder can't take a sender's codec: they are asked for another.
                Some(LinkMsg::Control(FromLink::Unsupported { codec, peer: Some(peer) })) => {
                    let Ok(author) = PublicKey::from_hex(&peer) else { continue };
                    if let Some(id) = endpoint_of(&v, &author) {
                        send_ctl(&v, &id, Ctl::Unsupported { codec });
                    }
                }
                Some(LinkMsg::Control(FromLink::LostFrom { peer, kind })) => {
                    let Ok(author) = PublicKey::from_hex(&peer) else { continue };
                    v.video.resync.lock().unwrap_or_else(|e| e.into_inner()).insert((author, kind.index()));
                    ask_key(&v, &author, kind);
                }
                _ => {}
            }
        }
        v.video.to_web.lock().unwrap_or_else(|e| e.into_inner()).take();
        // The page went away mid-call: what it no longer captures can't be watched.
        let was = std::mem::take(&mut *v.video.sending.lock().unwrap_or_else(|e| e.into_inner()));
        if was.any() {
            super::reannounce(&v);
            super::changed(&v);
        }
    }
}

/// One encoded frame to everyone watching that picture. Streams to a viewer open in frame order, so
/// their reader sees a gap only where a frame really went missing.
fn fan_out(v: &Arc<Voice>, kind: VideoKind, frame: Bytes, header: VideoHeader) {
    let mut msg = BytesMut::with_capacity(1 + frame.len());
    msg.extend_from_slice(&[STREAM_FRAME]);
    msg.extend_from_slice(&frame);
    let msg = msg.freeze();
    for (conn, out) in targets(v, kind) {
        ship(v, conn, out, kind, msg.clone(), header);
    }
}

/// The viewers of one picture, and their connections. One lock at a time: the session's state
/// reads them the other way round.
pub(super) fn targets(v: &Voice, kind: VideoKind) -> Vec<(Connection, Arc<Out>)> {
    let watchers: Vec<([u8; 32], Arc<Out>)> = v.video.watchers.lock().unwrap_or_else(|e| e.into_inner()).iter().map(|(id, out)| (*id, out.clone())).collect();
    let watchers: Vec<([u8; 32], Arc<Out>)> = watchers.into_iter().filter(|(_, out)| out.wants(kind)).collect();
    let links = v.links.lock().unwrap_or_else(|e| e.into_inner());
    watchers.into_iter().filter_map(|(id, out)| links.get(&id).map(|l| (l.conn.clone(), out))).collect()
}

/// One frame to one viewer, never waiting on them: a viewer out of stream credit or this far behind
/// skips to the next keyframe.
fn ship(v: &Arc<Voice>, conn: Connection, out: Arc<Out>, kind: VideoKind, msg: Bytes, header: VideoHeader) {
    let key = header.is_key();
    let i = kind.index();
    if out.need_key[i].load(Ordering::SeqCst) {
        if !key {
            if !out.behind.load(Ordering::SeqCst) {
                v.video.force_key(kind);
            }
            return;
        }
        out.need_key[i].store(false, Ordering::SeqCst);
    }
    let rtt = {
        let paths = conn.paths();
        paths.iter().find(|p| p.is_selected()).map_or(Duration::ZERO, |p| p.rtt())
    };
    let cap = (((rtt.as_millis() as u64 + ACK_SLACK_MS) * v.video.sending_fps() / 1000) as usize + IN_FLIGHT_SLACK).max(IN_FLIGHT_MIN);
    if out.in_flight.load(Ordering::SeqCst) >= cap {
        v.video.lost(&out, kind);
        return;
    }
    let Some(Ok(mut s)) = conn.open_uni().now_or_never() else {
        v.video.lost(&out, kind);
        return;
    };
    let stale = if key { STALE_KEY_MIN.max(rtt * 4) } else { STALE_MIN.max(rtt * 3) };
    out.in_flight.fetch_add(1, Ordering::SeqCst);
    let _ = s.set_priority(if key { 1 } else { 0 });
    let v = v.clone();
    crate::db::spawn_bound(async move {
        let sent = async {
            if timeout(stale, s.write_chunk(msg)).await.is_err() {
                let _ = s.reset(VarInt::from_u32(1));
                return false;
            }
            if s.finish().is_err() {
                return false;
            }
            if timeout(stale, s.stopped()).await.is_err() {
                let _ = s.reset(VarInt::from_u32(1));
                return false;
            }
            true
        }
        .await;
        out.in_flight.fetch_sub(1, Ordering::SeqCst);
        if !sent {
            v.video.lost(&out, kind);
        }
    });
}

/// What one stream from a peer carried.
enum Inbound {
    Frame(VideoKind, Vec<u8>),
    /// A frame of a picture we don't watch: the stream is stopped unread.
    Unwanted,
    Control(Vec<u8>),
    Other,
}

/// Read one stream, header first, so a frame we don't want is never read in full and a broken one
/// still names its picture. The error says which picture lost a frame, when that much arrived.
async fn read_stream(v: &Voice, author: &PublicKey, s: &mut RecvStream) -> Result<Inbound, Option<VideoKind>> {
    let mut first = [0u8; 1];
    s.read_exact(&mut first).await.map_err(|_| None)?;
    match first[0] {
        STREAM_FRAME => {
            let mut head = [0u8; VIDEO_HEADER_LEN];
            s.read_exact(&mut head).await.map_err(|_| None)?;
            let header = VideoHeader::parse(&head).ok_or(None)?;
            let kind = VideoKind::from_flags(header.flags);
            let wanted = v.video.watching.lock().unwrap_or_else(|e| e.into_inner()).get(author).is_some_and(|w| w.has(kind));
            if !wanted {
                let _ = s.stop(VarInt::from_u32(0));
                return Ok(Inbound::Unwanted);
            }
            let body = s.read_to_end(MAX_VIDEO_FRAME).await.map_err(|_| Some(kind))?;
            let mut frame = Vec::with_capacity(VIDEO_HEADER_LEN + body.len());
            frame.extend_from_slice(&head);
            frame.extend_from_slice(&body);
            Ok(Inbound::Frame(kind, frame))
        }
        STREAM_CONTROL => s.read_to_end(MAX_CONTROL).await.map(Inbound::Control).map_err(|_| None),
        _ => {
            let _ = s.stop(VarInt::from_u32(0));
            Ok(Inbound::Other)
        }
    }
}

/// What one peer sends us on streams: their pictures' frames, and what they ask of ours.
async fn inbound(v: Arc<Voice>, id: [u8; 32], author: PublicKey, conn: Connection) {
    let tag = author.to_bytes();
    let conn_id = conn.stable_id();
    while let Ok(mut s) = conn.accept_uni().await {
        match timeout(INBOUND_FRAME_TIMEOUT, read_stream(&v, &author, &mut s)).await {
            Ok(Ok(Inbound::Frame(kind, frame))) => deliver(&v, &author, &tag, kind, frame).await,
            Ok(Ok(Inbound::Unwanted)) => rewatch(&v, &author),
            Ok(Ok(Inbound::Control(data))) => match serde_json::from_slice::<Ctl>(&data) {
                Ok(Ctl::Watch { camera, screen }) => on_watch(&v, id, conn_id, Tracks { camera, screen }),
                Ok(Ctl::Key { kind }) => on_key(&v, id, kind),
                Ok(Ctl::Unsupported { codec }) => on_unsupported(&v, codec),
                _ => {}
            },
            Ok(Ok(Inbound::Other)) => {}
            Ok(Err(kind)) => {
                let want = v.video.watching.lock().unwrap_or_else(|e| e.into_inner()).get(&author).copied().unwrap_or_default();
                let lost = kind.map_or(want, |k| Tracks::default().with(k, want.has(k)));
                mark_resync(&v, &author, lost);
                for kind in KINDS.into_iter().filter(|k| lost.has(*k)) {
                    ask_key(&v, &author, kind);
                }
            }
            Err(_) => {
                let _ = s.stop(VarInt::from_u32(0));
                let want = v.video.watching.lock().unwrap_or_else(|e| e.into_inner()).get(&author).copied().unwrap_or_default();
                mark_resync(&v, &author, want);
                for kind in KINDS.into_iter().filter(|k| want.has(*k)) {
                    ask_key(&v, &author, kind);
                }
            }
        }
    }
}

/// A viewer asked for pictures of ours. The same ask again changes nothing.
fn on_watch(v: &Arc<Voice>, id: [u8; 32], conn_id: usize, want: Tracks) {
    let (out, before) = {
        let mut watchers = v.video.watchers.lock().unwrap_or_else(|e| e.into_inner());
        if !want.any() {
            let gone = watchers.remove(&id);
            drop(watchers);
            if gone.is_some_and(|o| o.wants(VideoKind::Screen)) {
                v.video.state_dirty.store(true, Ordering::SeqCst);
            }
            return;
        }
        let out = watchers.entry(id).or_insert_with(|| Arc::new(Out::new())).clone();
        drop(watchers);
        let before = std::mem::replace(&mut *out.want.lock().unwrap_or_else(|e| e.into_inner()), want);
        (out, before)
    };
    if before == want {
        return;
    }
    for kind in KINDS.into_iter().filter(|k| want.has(*k) && !before.has(*k)) {
        out.need_key[kind.index()].store(true, Ordering::SeqCst);
        v.video.force_key(kind);
    }
    if before.screen != want.screen {
        v.video.state_dirty.store(true, Ordering::SeqCst);
    }
    // The connection it asked on may have closed while this was read.
    let live = v.links.lock().unwrap_or_else(|e| e.into_inner()).get(&id).is_some_and(|l| l.conn.stable_id() == conn_id);
    if !live {
        v.video.watchers.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
    }
}

/// The codec to send: the best our encoder has that no viewer refused, H.264 before VP8.
fn choose_codec(video: &Video) -> Option<VideoCodec> {
    let encode = video.encode.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let refused = video.refused.lock().unwrap_or_else(|e| e.into_inner()).clone();
    [(VideoCodec::H264, "h264"), (VideoCodec::Vp8, "vp8")]
        .into_iter()
        .find(|(c, name)| !refused.contains(c) && encode.iter().any(|e| e == name))
        .map(|(c, _)| c)
}

/// A viewer can't decode what we send: every viewer gets another codec from the next keyframe, if
/// our encoder has one; VP8 decodes everywhere.
fn on_unsupported(v: &Voice, codec: VideoCodec) {
    {
        let mut refused = v.video.refused.lock().unwrap_or_else(|e| e.into_inner());
        if refused.contains(&codec) {
            return;
        }
        refused.push(codec);
    }
    let next = choose_codec(&v.video);
    let mut current = v.video.codec.lock().unwrap_or_else(|e| e.into_inner());
    if *current != Some(codec) || next.is_none() {
        return;
    }
    crate::log_info!("[voice] a viewer can't decode {codec:?}: sending {next:?}");
    *current = next;
    drop(current);
    v.video.tell_codec();
    for out in v.video.outs() {
        for kind in KINDS {
            out.need_key[kind.index()].store(true, Ordering::SeqCst);
        }
    }
    for kind in KINDS {
        *v.video.last_key_forced[kind.index()].lock().unwrap_or_else(|e| e.into_inner()) = None;
        v.video.force_key(kind);
    }
}

/// A viewer's decoder lost a picture: honoured every few seconds at most for each viewer, and only
/// for a picture they watch; between, it just waits for the next keyframe.
fn on_key(v: &Voice, id: [u8; 32], kind: VideoKind) {
    let out = v.video.watchers.lock().unwrap_or_else(|e| e.into_inner()).get(&id).cloned();
    let Some(out) = out.filter(|o| o.wants(kind)) else { return };
    out.need_key[kind.index()].store(true, Ordering::SeqCst);
    let mut last = out.last_key_request.lock().unwrap_or_else(|e| e.into_inner());
    if last.is_some_and(|t| t.elapsed() < VIEWER_KEY_GAP) {
        return;
    }
    *last = Some(Instant::now());
    drop(last);
    v.video.force_key(kind);
}

/// A sender is sending us what we don't watch, or not what we do: tell them again what we want,
/// every few seconds at most.
fn rewatch(v: &Voice, author: &PublicKey) {
    {
        let mut last = v.video.last_rewatch.lock().unwrap_or_else(|e| e.into_inner());
        if last.get(author).is_some_and(|t| t.elapsed() < REWATCH_AFTER) {
            return;
        }
        last.insert(*author, Instant::now());
    }
    let want = v.video.watching.lock().unwrap_or_else(|e| e.into_inner()).get(author).copied().unwrap_or_default();
    if let Some(id) = endpoint_of(v, author) {
        send_ctl(v, &id, Ctl::Watch { camera: want.camera, screen: want.screen });
    }
}

/// A sender's frame to the webview, tagged with whose it is.
async fn deliver(v: &Voice, author: &PublicKey, tag: &[u8; 32], kind: VideoKind, frame: Vec<u8>) {
    let Some(header) = VideoHeader::parse(&frame) else { return };
    let key = (*author, kind.index());
    {
        let mut resync = v.video.resync.lock().unwrap_or_else(|e| e.into_inner());
        if resync.contains(&key) {
            if !header.is_key() {
                drop(resync);
                ask_key(v, author, kind);
                return;
            }
            resync.remove(&key);
        }
    }
    v.video.last_frame.lock().unwrap_or_else(|e| e.into_inner()).insert(key, Instant::now());
    let Some(tx) = v.video.to_web.lock().unwrap_or_else(|e| e.into_inner()).clone() else { return };
    let mut msg = BytesMut::with_capacity(1 + 32 + frame.len());
    msg.extend_from_slice(&[KIND_PEER_FRAME]);
    msg.extend_from_slice(tag);
    msg.extend_from_slice(&frame);
    // A webview this far behind starts that picture again from a keyframe rather than lag.
    if !matches!(timeout(TO_WEB_WAIT, tx.send(msg.freeze())).await, Ok(Ok(()))) {
        v.video.resync.lock().unwrap_or_else(|e| e.into_inner()).insert(key);
        ask_key(v, author, kind);
    }
}

/// Once a second: each viewer's health, the pictures' rungs, what the webview missed, and watches
/// that went unanswered.
async fn rate_loop(v: Arc<Voice>) {
    let mut tick: u64 = 0;
    loop {
        crate::rt::time::sleep(Duration::from_secs(1)).await;
        tick += 1;
        if v.video.state_dirty.swap(false, Ordering::SeqCst) {
            super::changed(&v);
        }
        if v.video.codec_unsent.load(Ordering::SeqCst) {
            v.video.tell_codec();
        }
        heal_watches(&v);
        // Each viewer's losses this second, and whether they have fallen behind.
        let health: Vec<(Arc<Out>, bool)> = v
            .video
            .outs()
            .into_iter()
            .map(|out| {
                let lossy = out.stale.swap(0, Ordering::SeqCst) > 0;
                let bad = if lossy {
                    out.bad_secs.fetch_add(1, Ordering::SeqCst) + 1
                } else {
                    out.bad_secs.store(0, Ordering::SeqCst);
                    0
                };
                out.behind.store(bad >= BEHIND_AFTER_SECS, Ordering::SeqCst);
                (out, lossy)
            })
            .collect();
        let sending = v.video.sending();
        if !sending.any() {
            continue;
        }
        let relayed = {
            let ids: Vec<[u8; 32]> = v.video.watchers.lock().unwrap_or_else(|e| e.into_inner()).keys().copied().collect();
            let links = v.links.lock().unwrap_or_else(|e| e.into_inner());
            ids.iter().filter_map(|id| links.get(id)).any(|l| !l.conn.paths().iter().any(|p| p.is_selected() && p.is_ip()))
        };
        let active: Vec<&(Arc<Out>, bool)> = health.iter().filter(|(o, _)| !o.behind.load(Ordering::SeqCst)).collect();
        let lossy_any = active.iter().any(|(_, lossy)| *lossy);
        let clean = if lossy_any { 0 } else { v.video.clean_secs.fetch_add(1, Ordering::SeqCst) + 1 };
        let mut moved = false;
        for kind in KINDS.into_iter().filter(|k| sending.has(*k)) {
            let watched = active.iter().filter(|(o, _)| o.wants(kind)).count();
            let lossy = active.iter().filter(|(o, lossy)| *lossy && o.wants(kind)).count();
            let cap = match (kind, relayed) {
                (VideoKind::Screen, true) => SCREEN_RELAY_CAP,
                (VideoKind::Screen, false) => SCREEN_DIRECT_CAP,
                (VideoKind::Camera, true) => CAMERA_RELAY_CAP,
                (VideoKind::Camera, false) => CAMERA_DIRECT_CAP,
            };
            let rung = v.video.rung[kind.index()].load(Ordering::SeqCst);
            let next = match step(lossy, watched, clean) {
                Step::Down => rung.saturating_sub(1),
                Step::Up => (rung + 1).min(cap),
                Step::Hold => rung.min(cap),
            };
            if next != rung {
                v.video.rung[kind.index()].store(next, Ordering::SeqCst);
                v.video.tell_rung(kind);
                moved = true;
            } else if v.video.rate_unsent[kind.index()].load(Ordering::SeqCst) {
                v.video.tell_rung(kind);
            }
            // A viewer still waiting on a keyframe gets one this often, behind or not.
            if tick.is_multiple_of(VIEWER_KEY_GAP.as_secs()) && health.iter().any(|(o, _)| o.wants(kind) && o.need_key[kind.index()].load(Ordering::SeqCst)) {
                v.video.force_key(kind);
            }
        }
        if moved || lossy_any {
            v.video.clean_secs.store(0, Ordering::SeqCst);
        }
    }
}

/// A watched picture its sender says is on that hasn't shown a frame for a while: our watch may
/// have been lost on the way, so it is sent again.
fn heal_watches(v: &Voice) {
    let watching: Vec<(PublicKey, Tracks)> = v.video.watching.lock().unwrap_or_else(|e| e.into_inner()).iter().map(|(a, t)| (*a, *t)).collect();
    if watching.is_empty() {
        return;
    }
    let present = crate::community::v2::voice::present(&v.channel_hex, super::now_ms());
    for (author, want) in watching {
        let Some(p) = present.iter().find(|p| p.author == author) else { continue };
        let since = v.video.watch_since.lock().unwrap_or_else(|e| e.into_inner()).get(&author).copied();
        let quiet = |kind: VideoKind| {
            let last = v.video.last_frame.lock().unwrap_or_else(|e| e.into_inner()).get(&(author, kind.index())).copied();
            last.or(since).is_none_or(|t| t.elapsed() >= REWATCH_AFTER)
        };
        let missing = (want.screen && p.screen && quiet(VideoKind::Screen)) || (want.camera && p.camera && quiet(VideoKind::Camera));
        if missing {
            rewatch(v, &author);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_control_messages_read_as_cord_07_writes_them() {
        let watch: Ctl = serde_json::from_str(r#"{"t":"watch","camera":true}"#).unwrap();
        assert_eq!(watch, Ctl::Watch { camera: true, screen: false }, "an absent picture is not wanted");
        assert_eq!(serde_json::from_str::<Ctl>(r#"{"t":"watch"}"#).unwrap(), Ctl::Watch { camera: false, screen: false }, "neither is stop");
        assert_eq!(serde_json::from_str::<Ctl>(r#"{"t":"key","kind":"screen"}"#).unwrap(), Ctl::Key { kind: VideoKind::Screen });
        assert_eq!(serde_json::from_str::<Ctl>(r#"{"t":"hologram","on":true}"#).unwrap(), Ctl::Unknown, "a newer peer's control is skipped");
        assert_eq!(serde_json::to_string(&Ctl::Watch { camera: false, screen: true }).unwrap(), r#"{"t":"watch","camera":false,"screen":true}"#);
        assert_eq!(serde_json::from_str::<Ctl>(r#"{"t":"unsupported","codec":"h264"}"#).unwrap(), Ctl::Unsupported { codec: VideoCodec::H264 });
    }

    #[test]
    fn a_codec_a_viewer_refused_is_never_chosen_again() {
        let video = Video::default();
        *video.encode.lock().unwrap() = vec!["h264".into(), "vp8".into()];
        assert_eq!(choose_codec(&video), Some(VideoCodec::H264), "H.264 first");
        video.refused.lock().unwrap().push(VideoCodec::H264);
        assert_eq!(choose_codec(&video), Some(VideoCodec::Vp8), "VP8 once a viewer can't take H.264");
        *video.encode.lock().unwrap() = vec!["h264".into()];
        assert_eq!(choose_codec(&video), None, "nothing left to offer");
    }

    #[test]
    fn one_struggling_viewer_of_many_does_not_drag_the_picture_down() {
        assert_eq!(step(1, 1, 0), Step::Down, "the only viewer losing frames steps it down");
        assert_eq!(step(1, 6, 0), Step::Hold, "one of six does not");
        assert_eq!(step(2, 6, 0), Step::Down, "a third of them does");
        assert_eq!(step(0, 6, CLIMB_AFTER_SECS), Step::Up, "clean seconds climb");
        assert_eq!(step(0, 6, CLIMB_AFTER_SECS - 1), Step::Hold);
        assert_eq!(step(0, 0, CLIMB_AFTER_SECS), Step::Hold, "nobody watching: no climb");
    }
}
