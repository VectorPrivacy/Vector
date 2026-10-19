//! A shared screen in a voice Channel's call (CORD-07 §6, §8). The webview captures and encodes
//! it once; each frame goes to every member who asked to watch, one QUIC stream per frame, as in
//! a 1:1 call. A member only receives a screen it is watching, so a call of several costs each
//! sharer one upload per viewer, not per member.

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
use crate::calls::rate::{Rung, SCREEN_LADDER, SCREEN_RELAY_CAP};
use crate::calls::wire::{VideoCodec, VideoHeader, VideoKind, MAX_VIDEO_FRAME};
use crate::rt::time::timeout;

/// A unidirectional stream's first byte: what it carries.
const STREAM_FRAME: u8 = 1;
const STREAM_CONTROL: u8 = 2;
const MAX_CONTROL: usize = 1024;

/// Frames to one viewer still unacknowledged before that viewer skips to the next keyframe.
const PEER_IN_FLIGHT: usize = 6;
const STALE_MIN: Duration = Duration::from_millis(400);
const STALE_KEY_MIN: Duration = Duration::from_secs(2);
const INBOUND_FRAME_TIMEOUT: Duration = Duration::from_secs(2);
/// Keyframes are forced, and asked of a sharer, this often at most.
const KEY_GAP: Duration = Duration::from_secs(1);
/// Where a share starts, and the top a call reached only directly may climb to.
const SHARE_START: usize = 2;
const SHARE_DIRECT_CAP: usize = 5;
const CLIMB_AFTER_SECS: u32 = 8;

/// What a viewer and a sharer say to each other.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "t", rename_all = "snake_case")]
enum Ctl {
    /// Send me your screen, or stop.
    Watch { on: bool },
    /// My decoder lost the chain.
    Key,
    #[serde(other)]
    Unknown,
}

/// How one viewer is keeping up.
pub(super) struct Out {
    in_flight: AtomicUsize,
    need_key: AtomicBool,
}

#[derive(Default)]
pub(super) struct Video {
    to_web: Mutex<Option<mpsc::Sender<Bytes>>>,
    sharing: AtomicBool,
    /// Who asked for our screen.
    watchers: Mutex<HashMap<[u8; 32], Arc<Out>>>,
    /// Whose screens we asked for, by member: a sharer who reconnects is asked again.
    /// And those whose next frame we take must be a keyframe.
    watching: Mutex<HashSet<PublicKey>>,
    resync: Mutex<HashSet<PublicKey>>,
    codec: Mutex<Option<VideoCodec>>,
    rung: AtomicUsize,
    stale: AtomicU64,
    clean_secs: AtomicU32,
    last_key_forced: Mutex<Option<Instant>>,
    last_key_asked: Mutex<HashMap<PublicKey, Instant>>,
}

impl Video {
    pub(super) fn sharing(&self) -> bool {
        self.sharing.load(Ordering::SeqCst)
    }

    #[cfg(test)]
    pub(super) fn add_watcher(&self, id: [u8; 32]) {
        self.watchers.lock().unwrap().insert(id, Arc::new(Out { in_flight: AtomicUsize::new(0), need_key: AtomicBool::new(false) }));
    }

    #[cfg(test)]
    pub(super) fn set_sharing_flag(&self, on: bool) {
        self.sharing.store(on, Ordering::SeqCst);
    }

    pub(super) fn viewers(&self) -> u32 {
        if !self.sharing() {
            return 0;
        }
        self.watchers.lock().unwrap_or_else(|e| e.into_inner()).len() as u32
    }

    fn tell(&self, msg: &ToLink) {
        let tx = self.to_web.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(tx) = tx {
            let _ = tx.try_send(Bytes::from(control(msg)));
        }
    }

    fn tell_rung(&self) {
        let r: Rung = SCREEN_LADDER[self.rung.load(Ordering::SeqCst).min(SCREEN_LADDER.len() - 1)];
        self.tell(&ToLink::Rate { kind: VideoKind::Screen, kbps: r.kbps, width: r.width, height: r.height, fps: r.fps });
    }

    /// One keyframe for every viewer, a second apart at most.
    fn force_key(&self) {
        {
            let mut last = self.last_key_forced.lock().unwrap_or_else(|e| e.into_inner());
            if last.is_some_and(|t| t.elapsed() < KEY_GAP) {
                return;
            }
            *last = Some(Instant::now());
        }
        self.tell(&ToLink::Keyframe { kind: VideoKind::Screen });
    }
}

/// Take the webview's video link for this call and keep the share's rate; the tasks end with it.
pub(super) fn start(v: &Arc<Voice>) -> Vec<crate::rt::JoinHandle<()>> {
    let Ok(platform) = crate::calls::platform::get() else { return Vec::new() };
    let (tx, rx) = mpsc::channel::<LinkConn>(1);
    platform.set_video_taker(Some(tx));
    vec![crate::db::spawn_bound(link_loop(v.clone(), rx)), crate::db::spawn_bound(rate_loop(v.clone()))]
}

/// Let go of the link; dropping its sender closes the socket, which stops the capture.
pub(super) fn stop(v: &Voice) {
    if let Ok(platform) = crate::calls::platform::get() {
        platform.set_video_taker(None);
    }
    v.video.to_web.lock().unwrap_or_else(|e| e.into_inner()).take();
    v.video.sharing.store(false, Ordering::SeqCst);
}

/// A peer joined the mesh: read what they send us on streams, and if we watch their screen,
/// ask this connection for it.
pub(super) fn on_link(v: &Arc<Voice>, id: [u8; 32], author: PublicKey, conn: Connection) {
    if v.video.watching.lock().unwrap_or_else(|e| e.into_inner()).contains(&author) {
        v.video.resync.lock().unwrap_or_else(|e| e.into_inner()).insert(author);
        send_ctl(conn.clone(), Ctl::Watch { on: true });
    }
    crate::db::spawn_bound(inbound(v.clone(), id, author, conn));
}

/// A peer's connection went: nothing goes to it now.
pub(super) fn on_unlink(v: &Voice, id: &[u8; 32]) {
    v.video.watchers.lock().unwrap_or_else(|e| e.into_inner()).remove(id);
}

/// Start or stop sending our screen. Presence says so, so every member can see who shares.
pub(super) fn set_sharing(v: &Arc<Voice>, on: bool) -> Result<(), String> {
    if on && v.video.to_web.lock().unwrap_or_else(|e| e.into_inner()).is_none() {
        return Err("Video isn't ready in this call yet".into());
    }
    if v.video.sharing.swap(on, Ordering::SeqCst) == on {
        return Ok(());
    }
    if on {
        v.video.rung.store(SHARE_START, Ordering::SeqCst);
        v.video.clean_secs.store(0, Ordering::SeqCst);
        v.video.tell_rung();
        *v.video.last_key_forced.lock().unwrap_or_else(|e| e.into_inner()) = None;
        v.video.force_key();
    }
    for out in v.video.watchers.lock().unwrap_or_else(|e| e.into_inner()).values() {
        out.need_key.store(true, Ordering::SeqCst);
    }
    Ok(())
}

/// Ask a sharer for their screen, or stop. Only one who may share video here is watched; one
/// not connected yet is asked once they are.
pub(super) fn watch(v: &Arc<Voice>, author: &PublicKey, on: bool) -> Result<(), String> {
    if on {
        let community = super::held(&v.community)?;
        if !super::may_video(&community, &v.channel, author) {
            return Err("They can't share video here".into());
        }
        v.video.watching.lock().unwrap_or_else(|e| e.into_inner()).insert(*author);
        v.video.resync.lock().unwrap_or_else(|e| e.into_inner()).insert(*author);
    } else {
        v.video.watching.lock().unwrap_or_else(|e| e.into_inner()).remove(author);
    }
    if let Some(conn) = conn_of(v, author) {
        send_ctl(conn, Ctl::Watch { on });
    }
    Ok(())
}

/// Drop watched screens whose sharer may no longer send video here.
pub(super) fn revalidate(v: &Voice, may_video: impl Fn(&PublicKey) -> bool) {
    v.video.watching.lock().unwrap_or_else(|e| e.into_inner()).retain(|author| may_video(author));
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

/// Ask a sharer for a keyframe, a second apart at most.
fn ask_key(v: &Voice, author: &PublicKey) {
    {
        let mut asked = v.video.last_key_asked.lock().unwrap_or_else(|e| e.into_inner());
        if asked.get(author).is_some_and(|t| t.elapsed() < KEY_GAP) {
            return;
        }
        asked.insert(*author, Instant::now());
    }
    if let Some(conn) = conn_of(v, author) {
        send_ctl(conn, Ctl::Key);
    }
}

/// Webview sockets, one at a time: our frames out to every viewer, its decoders' requests back.
async fn link_loop(v: Arc<Voice>, mut taker: mpsc::Receiver<LinkConn>) {
    while let Some(mut link) = taker.recv().await {
        *v.video.to_web.lock().unwrap_or_else(|e| e.into_inner()) = Some(link.to_web.clone());
        // Fresh decoders on the other side start from keyframes.
        let watching: Vec<PublicKey> = v.video.watching.lock().unwrap_or_else(|e| e.into_inner()).iter().copied().collect();
        for author in watching {
            v.video.resync.lock().unwrap_or_else(|e| e.into_inner()).insert(author);
            ask_key(&v, &author);
        }
        let codec = *v.video.codec.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(codec) = codec {
            v.video.tell(&ToLink::Codec { codec });
        }
        if v.video.sharing() {
            v.video.tell_rung();
        }
        while let Some(msg) = link.from_web.recv().await {
            match parse(&msg) {
                Some(LinkMsg::Frame { header, .. }) if v.video.sharing() => fan_out(&v, msg.slice(1..), header).await,
                Some(LinkMsg::Control(FromLink::Caps { encode, .. })) => {
                    let codec = if encode.iter().any(|c| c == "h264") { Some(VideoCodec::H264) } else if encode.iter().any(|c| c == "vp8") { Some(VideoCodec::Vp8) } else { None };
                    *v.video.codec.lock().unwrap_or_else(|e| e.into_inner()) = codec;
                    if let Some(codec) = codec {
                        v.video.tell(&ToLink::Codec { codec });
                    }
                }
                Some(LinkMsg::Control(FromLink::LostFrom { peer })) => {
                    let Ok(author) = PublicKey::from_hex(&peer) else { continue };
                    v.video.resync.lock().unwrap_or_else(|e| e.into_inner()).insert(author);
                    ask_key(&v, &author);
                }
                _ => {}
            }
        }
        v.video.to_web.lock().unwrap_or_else(|e| e.into_inner()).take();
        // The page went away mid-share: what it no longer captures can't be watched.
        if v.video.sharing.swap(false, Ordering::SeqCst) {
            super::reannounce(&v);
            super::changed(&v);
        }
    }
}

/// One encoded frame to everyone watching. Streams to a viewer open in frame order, so their
/// reader sees a gap only where a frame really went missing.
async fn fan_out(v: &Arc<Voice>, frame: Bytes, header: VideoHeader) {
    for (conn, out) in targets(v) {
        ship(v, conn, out, frame.clone(), header).await;
    }
}

/// Each viewer's connection. One lock at a time: the session's state reads them the other way round.
pub(super) fn targets(v: &Voice) -> Vec<(Connection, Arc<Out>)> {
    let watchers: Vec<([u8; 32], Arc<Out>)> = v.video.watchers.lock().unwrap_or_else(|e| e.into_inner()).iter().map(|(id, out)| (*id, out.clone())).collect();
    let links = v.links.lock().unwrap_or_else(|e| e.into_inner());
    watchers.into_iter().filter_map(|(id, out)| links.get(&id).map(|l| (l.conn.clone(), out))).collect()
}

async fn ship(v: &Arc<Voice>, conn: Connection, out: Arc<Out>, frame: Bytes, header: VideoHeader) {
    let key = header.is_key();
    if out.need_key.load(Ordering::SeqCst) {
        if !key {
            v.video.force_key();
            return;
        }
        out.need_key.store(false, Ordering::SeqCst);
    }
    // A viewer this far behind skips to the next keyframe rather than queue more.
    if out.in_flight.load(Ordering::SeqCst) >= PEER_IN_FLIGHT {
        out.need_key.store(true, Ordering::SeqCst);
        v.video.stale.fetch_add(1, Ordering::SeqCst);
        v.video.force_key();
        return;
    }
    let rtt = {
        let paths = conn.paths();
        paths.iter().find(|p| p.is_selected()).map_or(Duration::ZERO, |p| p.rtt())
    };
    let stale = if key { STALE_KEY_MIN.max(rtt * 4) } else { STALE_MIN.max(rtt * 3) };
    out.in_flight.fetch_add(1, Ordering::SeqCst);
    let lost = |v: &Voice, out: &Out| {
        out.need_key.store(true, Ordering::SeqCst);
        v.video.stale.fetch_add(1, Ordering::SeqCst);
        v.video.force_key();
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

/// What one peer sends us on streams: their screen's frames, and what they ask of ours.
async fn inbound(v: Arc<Voice>, id: [u8; 32], author: PublicKey, conn: Connection) {
    let tag = author.to_bytes();
    while let Ok(mut s) = conn.accept_uni().await {
        let data = match timeout(INBOUND_FRAME_TIMEOUT, s.read_to_end(1 + MAX_VIDEO_FRAME)).await {
            Ok(Ok(data)) => data,
            _ => {
                if v.video.watching.lock().unwrap_or_else(|e| e.into_inner()).contains(&author) {
                    v.video.resync.lock().unwrap_or_else(|e| e.into_inner()).insert(author);
                    ask_key(&v, &author);
                }
                continue;
            }
        };
        match data.first() {
            Some(&STREAM_FRAME) => deliver(&v, &author, &tag, &data[1..]),
            Some(&STREAM_CONTROL) if data.len() <= 1 + MAX_CONTROL => match serde_json::from_slice::<Ctl>(&data[1..]) {
                Ok(Ctl::Watch { on: true }) => {
                    let mut watchers = v.video.watchers.lock().unwrap_or_else(|e| e.into_inner());
                    watchers.entry(id).or_insert_with(|| Arc::new(Out { in_flight: AtomicUsize::new(0), need_key: AtomicBool::new(true) }));
                    drop(watchers);
                    v.video.force_key();
                    super::changed(&v);
                }
                Ok(Ctl::Watch { on: false }) => {
                    v.video.watchers.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
                    super::changed(&v);
                }
                Ok(Ctl::Key) => {
                    if let Some(out) = v.video.watchers.lock().unwrap_or_else(|e| e.into_inner()).get(&id) {
                        out.need_key.store(true, Ordering::SeqCst);
                    }
                    v.video.force_key();
                }
                _ => {}
            },
            _ => {}
        }
    }
}

/// A sharer's frame to the webview, tagged with whose it is, if we're watching them.
fn deliver(v: &Voice, author: &PublicKey, tag: &[u8; 32], frame: &[u8]) {
    let Some(header) = VideoHeader::parse(frame) else { return };
    if !v.video.watching.lock().unwrap_or_else(|e| e.into_inner()).contains(author) {
        return;
    }
    {
        let mut resync = v.video.resync.lock().unwrap_or_else(|e| e.into_inner());
        if resync.contains(author) {
            if !header.is_key() {
                drop(resync);
                ask_key(v, author);
                return;
            }
            resync.remove(author);
        }
    }
    let Some(tx) = v.video.to_web.lock().unwrap_or_else(|e| e.into_inner()).clone() else { return };
    let mut msg = BytesMut::with_capacity(1 + 32 + frame.len());
    msg.extend_from_slice(&[KIND_PEER_FRAME]);
    msg.extend_from_slice(tag);
    msg.extend_from_slice(frame);
    // A webview this far behind starts again from a keyframe rather than lag.
    if tx.try_send(msg.freeze()).is_err() {
        v.video.resync.lock().unwrap_or_else(|e| e.into_inner()).insert(*author);
        ask_key(v, author);
    }
}

/// Once a second while sharing: a viewer falling behind steps the share down; clean seconds
/// step it back up, as far as the slowest kind of path allows.
async fn rate_loop(v: Arc<Voice>) {
    loop {
        crate::rt::time::sleep(Duration::from_secs(1)).await;
        if !v.video.sharing() {
            v.video.stale.store(0, Ordering::SeqCst);
            continue;
        }
        let cap = {
            let ids: Vec<[u8; 32]> = v.video.watchers.lock().unwrap_or_else(|e| e.into_inner()).keys().copied().collect();
            let links = v.links.lock().unwrap_or_else(|e| e.into_inner());
            let relayed = ids.iter().filter_map(|id| links.get(id)).any(|l| !l.conn.paths().iter().any(|p| p.is_selected() && p.is_ip()));
            if relayed { SCREEN_RELAY_CAP } else { SHARE_DIRECT_CAP }
        };
        let rung = v.video.rung.load(Ordering::SeqCst);
        let next = if v.video.stale.swap(0, Ordering::SeqCst) > 0 {
            v.video.clean_secs.store(0, Ordering::SeqCst);
            rung.saturating_sub(1)
        } else if v.video.clean_secs.fetch_add(1, Ordering::SeqCst) + 1 >= CLIMB_AFTER_SECS {
            v.video.clean_secs.store(0, Ordering::SeqCst);
            (rung + 1).min(cap)
        } else {
            rung.min(cap)
        };
        if next != rung {
            v.video.rung.store(next, Ordering::SeqCst);
            v.video.tell_rung();
        }
    }
}
