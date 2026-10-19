//! Cameras and shared screens in a voice Channel's call (CORD-07 §6, §8). The webview captures and
//! encodes each picture once; each frame goes to every member who asked for that picture, one QUIC
//! stream per frame, as in a 1:1 call. A member only receives what it shows, so a call of several
//! costs a sender one upload per viewer, not per member.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::{Bytes, BytesMut};
use iroh::endpoint::{Connection, VarInt};
use nostr_sdk::prelude::PublicKey;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use web_time::Instant;

use super::Voice;
use crate::calls::link::{control, parse, FromLink, LinkMsg, ToLink, KIND_PEER_FRAME};
use crate::calls::platform::LinkConn;
use crate::calls::rate::{Rung, CAMERA_LADDER, SCREEN_LADDER, SCREEN_RELAY_CAP};
use crate::calls::wire::{Tracks, VideoCodec, VideoHeader, VideoKind, MAX_VIDEO_FRAME};
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
/// Keyframes are forced, and asked of a sender, this often at most.
const KEY_GAP: Duration = Duration::from_secs(1);
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
    #[serde(other)]
    Unknown,
}

/// One viewer: what it asked for, and how it is keeping up.
pub(super) struct Out {
    want: Mutex<Tracks>,
    in_flight: AtomicUsize,
    need_key: [AtomicBool; 2],
}

impl Out {
    fn new(want: Tracks) -> Self {
        Self { want: Mutex::new(want), in_flight: AtomicUsize::new(0), need_key: [AtomicBool::new(true), AtomicBool::new(true)] }
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
    /// Which pictures we asked for, by member: one who reconnects is asked again.
    watching: Mutex<HashMap<PublicKey, Tracks>>,
    /// Pictures whose next frame we take must be a keyframe.
    resync: Mutex<HashSet<(PublicKey, usize)>>,
    codec: Mutex<Option<VideoCodec>>,
    rung: [AtomicUsize; 2],
    stale: AtomicU64,
    clean_secs: AtomicU32,
    last_key_forced: [Mutex<Option<Instant>>; 2],
    last_key_asked: Mutex<HashMap<(PublicKey, usize), Instant>>,
}

fn ladder(kind: VideoKind) -> &'static [Rung] {
    match kind {
        VideoKind::Camera => &CAMERA_LADDER,
        VideoKind::Screen => &SCREEN_LADDER,
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
        let outs: Vec<Arc<Out>> = self.watchers.lock().unwrap_or_else(|e| e.into_inner()).values().cloned().collect();
        outs.iter().filter(|o| o.wants(VideoKind::Screen)).count() as u32
    }

    #[cfg(test)]
    pub(super) fn add_watcher(&self, id: [u8; 32]) {
        self.watchers.lock().unwrap().insert(id, Arc::new(Out::new(Tracks { camera: true, screen: true })));
    }

    #[cfg(test)]
    pub(super) fn set_sending_for_test(&self, tracks: Tracks) {
        *self.sending.lock().unwrap() = tracks;
    }

    fn tell(&self, msg: &ToLink) {
        let tx = self.to_web.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(tx) = tx {
            let _ = tx.try_send(Bytes::from(control(msg)));
        }
    }

    fn rung(&self, kind: VideoKind) -> Rung {
        let l = ladder(kind);
        l[self.rung[kind.index()].load(Ordering::SeqCst).min(l.len() - 1)]
    }

    fn tell_rung(&self, kind: VideoKind) {
        let r = self.rung(kind);
        self.tell(&ToLink::Rate { kind, kbps: r.kbps, width: r.width, height: r.height, fps: r.fps });
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
}

/// A peer joined the mesh: read what they send us on streams, and if we watch them, ask this
/// connection for it.
pub(super) fn on_link(v: &Arc<Voice>, id: [u8; 32], author: PublicKey, conn: Connection) {
    let want = v.video.watching.lock().unwrap_or_else(|e| e.into_inner()).get(&author).copied();
    if let Some(want) = want.filter(|w| w.any()) {
        {
            let mut resync = v.video.resync.lock().unwrap_or_else(|e| e.into_inner());
            for kind in KINDS.into_iter().filter(|k| want.has(*k)) {
                resync.insert((author, kind.index()));
            }
        }
        send_ctl(conn.clone(), Ctl::Watch { camera: want.camera, screen: want.screen });
    }
    crate::db::spawn_bound(inbound(v.clone(), id, author, conn));
}

/// A peer's connection went: nothing goes to it now.
pub(super) fn on_unlink(v: &Voice, id: &[u8; 32]) {
    v.video.watchers.lock().unwrap_or_else(|e| e.into_inner()).remove(id);
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
    let outs: Vec<Arc<Out>> = v.video.watchers.lock().unwrap_or_else(|e| e.into_inner()).values().cloned().collect();
    for out in outs {
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
    {
        let mut resync = v.video.resync.lock().unwrap_or_else(|e| e.into_inner());
        for kind in KINDS.into_iter().filter(|k| want.has(*k) && !before.has(*k)) {
            resync.insert((*author, kind.index()));
        }
    }
    if let Some(conn) = conn_of(v, author) {
        send_ctl(conn, Ctl::Watch { camera: want.camera, screen: want.screen });
    }
    Ok(())
}

/// Drop watched pictures whose sender may no longer send video here.
pub(super) fn revalidate(v: &Voice, may_video: impl Fn(&PublicKey) -> bool) {
    v.video.watching.lock().unwrap_or_else(|e| e.into_inner()).retain(|author, _| may_video(author));
}

fn conn_of(v: &Voice, author: &PublicKey) -> Option<Connection> {
    v.links.lock().unwrap_or_else(|e| e.into_inner()).values().find(|l| l.author == *author).map(|l| l.conn.clone())
}

fn send_ctl(conn: Connection, ctl: Ctl) {
    let Ok(body) = serde_json::to_vec(&ctl) else { return };
    crate::db::spawn_bound(async move {
        let sent = async {
            let mut s = conn.open_uni().await.map_err(|e| e.to_string())?;
            let mut msg = Vec::with_capacity(1 + body.len());
            msg.push(STREAM_CONTROL);
            msg.extend_from_slice(&body);
            s.write_all(&msg).await.map_err(|e| e.to_string())?;
            s.finish().map_err(|e| e.to_string())
        };
        if let Err(e) = timeout(Duration::from_secs(5), sent).await.unwrap_or_else(|_| Err("timed out".into())) {
            crate::log_debug!("[voice] video control not sent: {e}");
        }
    });
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
    if let Some(conn) = conn_of(v, author) {
        send_ctl(conn, Ctl::Key { kind });
    }
}

/// Every picture we watch starts again from a keyframe.
fn resync_all(v: &Voice, author: &PublicKey, want: Tracks) {
    for kind in KINDS.into_iter().filter(|k| want.has(*k)) {
        v.video.resync.lock().unwrap_or_else(|e| e.into_inner()).insert((*author, kind.index()));
        ask_key(v, author, kind);
    }
}

/// Webview sockets, one at a time: our frames out to every viewer, its decoders' requests back.
async fn link_loop(v: Arc<Voice>, mut taker: mpsc::Receiver<LinkConn>) {
    while let Some(mut link) = taker.recv().await {
        *v.video.to_web.lock().unwrap_or_else(|e| e.into_inner()) = Some(link.to_web.clone());
        // Fresh decoders on the other side start from keyframes.
        let watching: Vec<(PublicKey, Tracks)> = v.video.watching.lock().unwrap_or_else(|e| e.into_inner()).iter().map(|(a, t)| (*a, *t)).collect();
        for (author, want) in watching {
            resync_all(&v, &author, want);
        }
        let codec = *v.video.codec.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(codec) = codec {
            v.video.tell(&ToLink::Codec { codec });
        }
        let sending = v.video.sending();
        for kind in KINDS.into_iter().filter(|k| sending.has(*k)) {
            v.video.tell_rung(kind);
        }
        while let Some(msg) = link.from_web.recv().await {
            match parse(&msg) {
                Some(LinkMsg::Frame { header, .. }) => {
                    let kind = VideoKind::from_flags(header.flags);
                    if v.video.sending().has(kind) {
                        fan_out(&v, kind, msg.slice(1..), header).await;
                    }
                }
                Some(LinkMsg::Control(FromLink::Caps { encode, .. })) => {
                    let codec = if encode.iter().any(|c| c == "h264") { Some(VideoCodec::H264) } else if encode.iter().any(|c| c == "vp8") { Some(VideoCodec::Vp8) } else { None };
                    *v.video.codec.lock().unwrap_or_else(|e| e.into_inner()) = codec;
                    if let Some(codec) = codec {
                        v.video.tell(&ToLink::Codec { codec });
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
async fn fan_out(v: &Arc<Voice>, kind: VideoKind, frame: Bytes, header: VideoHeader) {
    for (conn, out) in targets(v, kind) {
        ship(v, conn, out, kind, frame.clone(), header).await;
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

async fn ship(v: &Arc<Voice>, conn: Connection, out: Arc<Out>, kind: VideoKind, frame: Bytes, header: VideoHeader) {
    let key = header.is_key();
    let i = kind.index();
    if out.need_key[i].load(Ordering::SeqCst) {
        if !key {
            v.video.force_key(kind);
            return;
        }
        out.need_key[i].store(false, Ordering::SeqCst);
    }
    let rtt = {
        let paths = conn.paths();
        paths.iter().find(|p| p.is_selected()).map_or(Duration::ZERO, |p| p.rtt())
    };
    // A viewer this far behind skips to the next keyframe rather than queue more.
    let cap = (((rtt.as_millis() as u64 + ACK_SLACK_MS) * v.video.sending_fps() / 1000) as usize + IN_FLIGHT_SLACK).max(IN_FLIGHT_MIN);
    if out.in_flight.load(Ordering::SeqCst) >= cap {
        out.need_key[i].store(true, Ordering::SeqCst);
        v.video.stale.fetch_add(1, Ordering::SeqCst);
        v.video.force_key(kind);
        return;
    }
    let stale = if key { STALE_KEY_MIN.max(rtt * 4) } else { STALE_MIN.max(rtt * 3) };
    out.in_flight.fetch_add(1, Ordering::SeqCst);
    let lost = move |v: &Voice, out: &Out| {
        out.need_key[i].store(true, Ordering::SeqCst);
        v.video.stale.fetch_add(1, Ordering::SeqCst);
        v.video.force_key(kind);
    };
    let mut s = match timeout(stale, conn.open_uni()).await {
        Ok(Ok(s)) => s,
        _ => {
            out.in_flight.fetch_sub(1, Ordering::SeqCst);
            lost(v, &out);
            return;
        }
    };
    let _ = s.set_priority(if key { 1 } else { 0 });
    let v = v.clone();
    crate::db::spawn_bound(async move {
        let sent = async {
            let mut msg = BytesMut::with_capacity(1 + frame.len());
            msg.extend_from_slice(&[STREAM_FRAME]);
            msg.extend_from_slice(&frame);
            if timeout(stale, s.write_chunk(msg.freeze())).await.is_err() {
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
            lost(&v, &out);
        }
    });
}

/// What one peer sends us on streams: their pictures' frames, and what they ask of ours.
async fn inbound(v: Arc<Voice>, id: [u8; 32], author: PublicKey, conn: Connection) {
    let tag = author.to_bytes();
    while let Ok(mut s) = conn.accept_uni().await {
        let data = match timeout(INBOUND_FRAME_TIMEOUT, s.read_to_end(1 + MAX_VIDEO_FRAME)).await {
            Ok(Ok(data)) => data,
            _ => {
                let want = v.video.watching.lock().unwrap_or_else(|e| e.into_inner()).get(&author).copied().unwrap_or_default();
                resync_all(&v, &author, want);
                continue;
            }
        };
        match data.first() {
            Some(&STREAM_FRAME) => deliver(&v, &author, &tag, &data[1..]),
            Some(&STREAM_CONTROL) if data.len() <= 1 + MAX_CONTROL => match serde_json::from_slice::<Ctl>(&data[1..]) {
                Ok(Ctl::Watch { camera, screen }) => {
                    let want = Tracks { camera, screen };
                    let out = {
                        let mut watchers = v.video.watchers.lock().unwrap_or_else(|e| e.into_inner());
                        if want.any() {
                            Some(watchers.entry(id).or_insert_with(|| Arc::new(Out::new(Tracks::default()))).clone())
                        } else {
                            watchers.remove(&id);
                            None
                        }
                    };
                    if let Some(out) = out {
                        let before = std::mem::replace(&mut *out.want.lock().unwrap_or_else(|e| e.into_inner()), want);
                        for kind in KINDS.into_iter().filter(|k| want.has(*k) && !before.has(*k)) {
                            out.need_key[kind.index()].store(true, Ordering::SeqCst);
                            v.video.force_key(kind);
                        }
                    }
                    super::changed(&v);
                }
                Ok(Ctl::Key { kind }) => {
                    let out = v.video.watchers.lock().unwrap_or_else(|e| e.into_inner()).get(&id).cloned();
                    if let Some(out) = out {
                        out.need_key[kind.index()].store(true, Ordering::SeqCst);
                    }
                    v.video.force_key(kind);
                }
                _ => {}
            },
            _ => {}
        }
    }
}

/// A sender's frame to the webview, tagged with whose it is, if we're watching that picture.
fn deliver(v: &Voice, author: &PublicKey, tag: &[u8; 32], frame: &[u8]) {
    let Some(header) = VideoHeader::parse(frame) else { return };
    let kind = VideoKind::from_flags(header.flags);
    let wanted = v.video.watching.lock().unwrap_or_else(|e| e.into_inner()).get(author).is_some_and(|w| w.has(kind));
    if !wanted {
        return;
    }
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
    let Some(tx) = v.video.to_web.lock().unwrap_or_else(|e| e.into_inner()).clone() else { return };
    let mut msg = BytesMut::with_capacity(1 + 32 + frame.len());
    msg.extend_from_slice(&[KIND_PEER_FRAME]);
    msg.extend_from_slice(tag);
    msg.extend_from_slice(frame);
    // A webview this far behind starts that picture again from a keyframe rather than lag.
    if tx.try_send(msg.freeze()).is_err() {
        v.video.resync.lock().unwrap_or_else(|e| e.into_inner()).insert(key);
        ask_key(v, author, kind);
    }
}

/// Once a second while sending: a viewer falling behind steps our pictures down; clean seconds step
/// them back up, as far as the slowest kind of path allows.
async fn rate_loop(v: Arc<Voice>) {
    loop {
        crate::rt::time::sleep(Duration::from_secs(1)).await;
        let sending = v.video.sending();
        if !sending.any() {
            v.video.stale.store(0, Ordering::SeqCst);
            continue;
        }
        let relayed = {
            let ids: Vec<[u8; 32]> = v.video.watchers.lock().unwrap_or_else(|e| e.into_inner()).keys().copied().collect();
            let links = v.links.lock().unwrap_or_else(|e| e.into_inner());
            ids.iter().filter_map(|id| links.get(id)).any(|l| !l.conn.paths().iter().any(|p| p.is_selected() && p.is_ip()))
        };
        let stale = v.video.stale.swap(0, Ordering::SeqCst) > 0;
        let climb = !stale && v.video.clean_secs.fetch_add(1, Ordering::SeqCst) + 1 >= CLIMB_AFTER_SECS;
        if stale || climb {
            v.video.clean_secs.store(0, Ordering::SeqCst);
        }
        for kind in KINDS.into_iter().filter(|k| sending.has(*k)) {
            let cap = match (kind, relayed) {
                (VideoKind::Screen, true) => SCREEN_RELAY_CAP,
                (VideoKind::Screen, false) => SCREEN_DIRECT_CAP,
                (VideoKind::Camera, true) => CAMERA_RELAY_CAP,
                (VideoKind::Camera, false) => CAMERA_DIRECT_CAP,
            };
            let rung = v.video.rung[kind.index()].load(Ordering::SeqCst);
            let next = if stale { rung.saturating_sub(1) } else if climb { (rung + 1).min(cap) } else { rung.min(cap) };
            if next != rung {
                v.video.rung[kind.index()].store(next, Ordering::SeqCst);
                v.video.tell_rung(kind);
            }
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
    }
}
