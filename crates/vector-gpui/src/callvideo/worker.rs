//! One video link's life, on its own thread: our captures encoded and sent, the peer's frames
//! decoded into pictures for the UI, a stats line a second. Each track (camera, screen) has
//! its own encoder and decoder; a voice call's members each have theirs.

use std::collections::HashMap;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use openh264::OpenH264API;
use openh264::decoder::Decoder;
use openh264::encoder::{BitRate, Encoder, EncoderConfig, FrameRate, FrameType, RateControlMode, UsageType, VuiConfig};
use openh264::formats::{YUVSlices, YUVSource};
use tokio::sync::mpsc;
use vector_core::calls::link::{FromLink, KIND_CONTROL, KIND_FRAME, KIND_PEER_FRAME, ToLink};
use vector_core::calls::wire::{VIDEO_HEADER_LEN, VIDEO_KEY, VIDEO_SCREEN, VideoCodec, VideoHeader, VideoKind};

use super::yuv::{self, I420};
use super::{In, Pic, Pictures, Wake};

const PEER_TAG_LEN: usize = 32;
const MAX_FRAME: usize = 1024 * 1024;
const MAX_DECODE_PX: usize = 4096 * 2304;
/// Our own thumbnail is a corner tile: a 320-wide picture at 15 fps is all it shows.
const PREVIEW_W: usize = 320;
const PREVIEW_GAP: Duration = Duration::from_millis(66);

const KINDS: [VideoKind; 2] = [VideoKind::Camera, VideoKind::Screen];

#[derive(Clone, Copy)]
struct Spec {
    fps: u32,
    kbps: u32,
    target: (u32, u32),
    max_h: u32,
}

#[derive(Clone, Copy)]
struct Rate {
    kbps: u32,
    width: u32,
    height: u32,
    fps: u32,
}

struct Outgoing {
    kind: VideoKind,
    spec: Option<Spec>,
    enc: Option<Encoder>,
    enc_size: (usize, usize),
    seq: u32,
    skip_next: bool,
    force_key: bool,
    last_sent: Option<u64>,
    frames: u32,
    bytes: u64,
    preview_at: Option<Instant>,
    rate: Option<Rate>,
}

impl Outgoing {
    fn new(kind: VideoKind) -> Self {
        Self { kind, spec: None, enc: None, enc_size: (0, 0), seq: 0, skip_next: false, force_key: true, last_sent: None, frames: 0, bytes: 0, preview_at: None, rate: None }
    }

    /// The rung's size for a camera, turned for one held upright; at most the rung's height
    /// for a screen. Always even, never upscaled.
    fn target(&self, cw: usize, ch: usize) -> (usize, usize) {
        let Some(c) = self.spec else { return (0, 0) };
        let (mut w, mut h) = (cw, ch);
        if self.kind == VideoKind::Screen {
            if c.max_h > 0 && ch > c.max_h as usize {
                w = (cw as f32 * c.max_h as f32 / ch as f32).round() as usize;
                h = c.max_h as usize;
            }
        } else if c.target.0 > 0 && c.target.1 > 0 {
            let (tw, th) = if (ch > cw) == (c.target.1 > c.target.0) { (c.target.0, c.target.1) } else { (c.target.1, c.target.0) };
            let (tw, th) = (tw as usize, th as usize);
            if cw > tw || ch > th {
                let s = (tw as f32 / cw as f32).min(th as f32 / ch as f32);
                w = (cw as f32 * s).round() as usize;
                h = (ch as f32 * s).round() as usize;
            }
        }
        (w & !1, h & !1)
    }

    fn configure(&mut self, w: usize, h: usize) {
        self.enc = None;
        let Some(spec) = self.spec else { return };
        let screen = self.kind == VideoKind::Screen;
        let cfg = EncoderConfig::new()
            .bitrate(BitRate::from_bps(spec.kbps.max(50) * 1000))
            .max_frame_rate(FrameRate::from_hz(spec.fps.max(1) as f32))
            .rate_control_mode(RateControlMode::Bitrate)
            .usage_type(if screen { UsageType::ScreenContentRealTime } else { UsageType::CameraVideoRealTime })
            .vui(VuiConfig::bt601())
            .num_threads(if w * h > 1280 * 720 { 4 } else { 2 });
        match Encoder::with_api_config(OpenH264API::from_source(), cfg) {
            Ok(e) => {
                self.enc = Some(e);
                self.enc_size = (w, h);
                self.force_key = true;
            }
            Err(e) => log::warn!("video encoder: {e}"),
        }
    }
}

/// A peer's picture decoder, for the codec its frames name.
enum Dec {
    H264(Decoder),
    Vp8(super::vp8::Vp8),
}

impl Dec {
    fn codec(&self) -> VideoCodec {
        match self {
            Dec::H264(_) => VideoCodec::H264,
            Dec::Vp8(_) => VideoCodec::Vp8,
        }
    }
}

#[derive(Default)]
struct Incoming {
    dec: Option<Dec>,
    need_key: bool,
    frames: u32,
}

impl Incoming {
    fn reset(&mut self) {
        self.dec = None;
        self.need_key = true;
    }
}

pub struct Worker {
    rx: Receiver<In>,
    to_core: mpsc::Sender<Bytes>,
    pics: Arc<Mutex<Pictures>>,
    wake: Wake,
    codec: Option<VideoCodec>,
    paused: bool,
    out: [Outgoing; 2],
    inn: [Incoming; 2],
    members: HashMap<(String, usize), Incoming>,
    dec_errors: u32,
}

impl Worker {
    pub fn new(rx: Receiver<In>, to_core: mpsc::Sender<Bytes>, pics: Arc<Mutex<Pictures>>, wake: Wake) -> Self {
        Self {
            rx,
            to_core,
            pics,
            wake,
            codec: None,
            paused: false,
            out: [Outgoing::new(VideoKind::Camera), Outgoing::new(VideoKind::Screen)],
            inn: [Incoming { need_key: true, ..Default::default() }, Incoming { need_key: true, ..Default::default() }],
            members: HashMap::new(),
            dec_errors: 0,
        }
    }

    fn send(&self, b: Vec<u8>) -> bool {
        self.to_core.try_send(Bytes::from(b)).is_ok()
    }

    fn control(&self, msg: &FromLink) {
        let mut out = vec![KIND_CONTROL];
        if serde_json::to_writer(&mut out, msg).is_ok() {
            let _ = self.to_core.blocking_send(Bytes::from(out));
        }
    }

    pub fn run(mut self) {
        self.control(&FromLink::Caps { encode: vec!["h264".into()], decode: super::vp8::decode_caps() });
        self.pics().open = true;
        self.wake.ping();
        let mut next_stats = Instant::now() + Duration::from_secs(1);
        'outer: loop {
            let wait = next_stats.saturating_duration_since(Instant::now());
            let first = match self.rx.recv_timeout(wait) {
                Ok(m) => Some(m),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => break,
            };
            // Of the captures queued meanwhile only the newest of each track is encoded: a
            // backlog means the machine is behind, and old frames would never send in time.
            let mut frames: [Option<(I420, u64)>; 2] = [None, None];
            let mut msgs = first.into_iter().collect::<Vec<_>>();
            while let Ok(m) = self.rx.try_recv() {
                msgs.push(m);
            }
            for m in msgs {
                match m {
                    In::Frame { kind, pic, ts_us } => frames[kind.index()] = Some((pic, ts_us)),
                    In::Link(b) => self.on_link(&b),
                    In::LinkClosed | In::Close => break 'outer,
                    In::Capture { kind, fps, kbps } => self.start_capture(kind, fps, kbps),
                    In::Stop(kind) => {
                        let o = &mut self.out[kind.index()];
                        o.spec = None;
                        o.enc = None;
                        o.rate = None;
                        self.pics().mine[kind.index()] = None;
                        self.wake.ping();
                    }
                    In::SourceLost(kind) => {
                        self.pics().lost.push(kind);
                        self.wake.ping();
                    }
                    In::Want(peer) => {
                        self.members.insert(peer.clone(), Incoming { need_key: true, ..Default::default() });
                        self.control(&FromLink::LostFrom { peer: peer.0, kind: KINDS[peer.1] });
                    }
                    In::Unwant(key) => {
                        self.members.remove(&key);
                    }
                }
            }
            for (i, f) in frames.into_iter().enumerate() {
                if let Some((pic, ts)) = f {
                    self.on_captured(i, pic, ts);
                }
            }
            if Instant::now() >= next_stats {
                next_stats = Instant::now() + Duration::from_secs(1);
                self.stats();
            }
        }
        let mut guard = self.pics();
        let p = &mut *guard;
        p.open = false;
        for slot in p.peer.iter_mut().chain(p.mine.iter_mut()) {
            if let Some(old) = slot.take() {
                p.retired.push(old.image);
            }
        }
        p.retired.extend(p.members.drain().map(|(_, v)| v.image));
        drop(guard);
        self.wake.ping();
    }

    fn pics(&self) -> std::sync::MutexGuard<'_, Pictures> {
        self.pics.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn start_capture(&mut self, kind: VideoKind, fps: u32, kbps: u32) {
        let o = &mut self.out[kind.index()];
        o.spec = Some(Spec { fps, kbps, target: (0, 0), max_h: 0 });
        o.force_key = true;
        o.seq = 0;
        o.last_sent = None;
        o.enc = None;
        if let Some(r) = o.rate {
            self.set_rate(kind, r);
        }
    }

    fn set_rate(&mut self, kind: VideoKind, r: Rate) {
        let o = &mut self.out[kind.index()];
        o.rate = Some(r);
        let Some(spec) = o.spec.as_mut() else { return };
        let changed = spec.kbps != r.kbps || spec.fps != r.fps;
        spec.kbps = r.kbps;
        spec.fps = r.fps;
        if kind == VideoKind::Screen {
            spec.max_h = r.height;
            super::screen::FPS.store(r.fps, std::sync::atomic::Ordering::Relaxed);
        } else {
            spec.target = (r.width, r.height);
        }
        if changed && o.enc.is_some() {
            let (w, h) = o.enc_size;
            o.configure(w, h);
        }
    }

    fn on_link(&mut self, msg: &[u8]) {
        match msg.first() {
            Some(&KIND_FRAME) => {
                let frame = &msg[1..];
                if frame.len() < VIDEO_HEADER_LEN {
                    return;
                }
                let i = if frame[12] & VIDEO_SCREEN != 0 { 1 } else { 0 };
                self.decode(None, i, frame);
            }
            Some(&KIND_PEER_FRAME) => {
                let body = &msg[1..];
                if body.len() < PEER_TAG_LEN + VIDEO_HEADER_LEN {
                    return;
                }
                let i = if body[PEER_TAG_LEN + 12] & VIDEO_SCREEN != 0 { 1 } else { 0 };
                let Ok(tag) = <&[u8; 32]>::try_from(&body[..PEER_TAG_LEN]) else { return };
                let hex = vector_core::simd::hex::bytes_to_hex_32(tag);
                // Nobody looks at this picture: decoding it would be wasted.
                if !self.members.contains_key(&(hex.clone(), i)) {
                    return;
                }
                self.decode(Some(hex), i, &body[PEER_TAG_LEN..]);
            }
            Some(&KIND_CONTROL) => {
                let Ok(c) = serde_json::from_slice::<ToLink>(&msg[1..]) else { return };
                match c {
                    ToLink::Rate { kind, kbps, width, height, fps } => self.set_rate(kind, Rate { kbps, width, height, fps }),
                    ToLink::Skip { kind } => self.out[kind.index()].skip_next = true,
                    ToLink::Keyframe { kind } => self.out[kind.index()].force_key = true,
                    ToLink::Peer { tracks } => {
                        let mut p = self.pics.lock().unwrap_or_else(|e| e.into_inner());
                        for k in KINDS {
                            if !tracks.has(k) {
                                self.inn[k.index()].reset();
                                if let Some(old) = p.peer[k.index()].take() {
                                    p.retired.push(old.image);
                                }
                            }
                        }
                        drop(p);
                        self.wake.ping();
                    }
                    ToLink::Pause { on } => self.paused = on,
                    ToLink::Codec { codec } => {
                        if self.codec != Some(codec) {
                            self.codec = Some(codec);
                            for o in &mut self.out {
                                o.enc = None;
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn on_captured(&mut self, i: usize, pic: I420, ts_us: u64) {
        let paused = self.paused;
        let codec = self.codec;
        let o = &mut self.out[i];
        let Some(spec) = o.spec else { return };
        // Our own thumbnail shows even while the peer looks away.
        if o.preview_at.is_none_or(|t| t.elapsed() >= PREVIEW_GAP) {
            o.preview_at = Some(Instant::now());
            let pw = PREVIEW_W.min(pic.w);
            let ph = ((pic.h * pw) / pic.w.max(1)) & !1;
            let small = pic.scale(pw, ph.max(2));
            let (y, u, v) = small.planes();
            let bgra = yuv::to_bgra(y, u, v, (small.w, small.w / 2), small.w, small.h);
            if let Some(p) = Pic::new(small.w as u32, small.h as u32, bgra) {
                let mut g = self.pics.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(old) = g.mine[i].replace(p) {
                    g.retired.push(old.image);
                }
                drop(g);
                self.wake.ping();
            }
        }
        if paused || o.skip_next || codec != Some(VideoCodec::H264) {
            o.skip_next = false;
            return;
        }
        let min_gap = (1e6 / spec.fps.max(1) as f64 * 0.9) as u64;
        if o.last_sent.is_some_and(|l| ts_us.saturating_sub(l) < min_gap) {
            return;
        }
        o.last_sent = Some(ts_us);
        let (w, h) = o.target(pic.w, pic.h);
        if w == 0 || h == 0 {
            return;
        }
        let pic = if (w, h) == (pic.w, pic.h) { pic } else { pic.scale(w, h) };
        if o.enc.is_none() || o.enc_size != (w, h) {
            o.configure(w, h);
        }
        let force = std::mem::take(&mut o.force_key);
        let Some(enc) = o.enc.as_mut() else { return };
        if force {
            enc.force_intra_frame();
        }
        let (py, pu, pv) = pic.planes();
        let src = YUVSlices::new((py, pu, pv), (pic.w, pic.h), (pic.w, pic.w / 2, pic.w / 2));
        let Ok(bits) = enc.encode_at(&src, openh264::Timestamp::from_millis(ts_us / 1000)) else {
            o.force_key = true;
            return;
        };
        let key = matches!(bits.frame_type(), FrameType::IDR);
        if matches!(bits.frame_type(), FrameType::Skip | FrameType::Invalid) {
            return;
        }
        let mut buf = vec![0u8; 1 + VIDEO_HEADER_LEN];
        buf[0] = KIND_FRAME;
        bits.write_vec(&mut buf);
        let len = buf.len() - 1 - VIDEO_HEADER_LEN;
        if len == 0 || buf.len() > MAX_FRAME {
            return;
        }
        let flags = if key { VIDEO_KEY } else { 0 } | if o.kind == VideoKind::Screen { VIDEO_SCREEN } else { 0 };
        VideoHeader { seq: o.seq, ts_us, flags, codec: VideoCodec::H264 }.write(&mut buf[1..1 + VIDEO_HEADER_LEN]);
        o.seq = o.seq.wrapping_add(1);
        o.frames += 1;
        o.bytes += len as u64;
        if !self.send(buf) {
            // A frame the peer never gets breaks its chain: start the next one afresh.
            self.out[i].force_key = true;
        }
    }

    fn lost(&self, member: &Option<String>, i: usize) {
        match member {
            Some(peer) => self.control(&FromLink::LostFrom { peer: peer.clone(), kind: KINDS[i] }),
            None => self.control(&FromLink::Lost { kind: KINDS[i] }),
        }
    }

    fn decode(&mut self, member: Option<String>, i: usize, frame: &[u8]) {
        let Some(h) = VideoHeader::parse(frame) else { return };
        let key = h.flags & VIDEO_KEY != 0;
        let takes = match h.codec {
            VideoCodec::H264 => true,
            VideoCodec::Vp8 => super::vp8::available(),
        };
        if !takes {
            // Only what our caps name; a peer that sends another anyway is told so once.
            if key {
                self.control(&FromLink::Unsupported { codec: h.codec, peer: member.clone() });
            }
            return;
        }
        let slot = match &member {
            Some(m) => match self.members.get_mut(&(m.clone(), i)) {
                Some(s) => s,
                None => return,
            },
            None => &mut self.inn[i],
        };
        if slot.dec.as_ref().is_some_and(|d| d.codec() != h.codec) {
            slot.reset();
        }
        if slot.dec.is_none() {
            if !key {
                slot.need_key = true;
                return;
            }
            let made = match h.codec {
                VideoCodec::Vp8 => super::vp8::Vp8::new().map(Dec::Vp8),
                _ => Decoder::new().map(Dec::H264).map_err(|e| e.to_string()),
            };
            match made {
                Ok(d) => slot.dec = Some(d),
                Err(e) => {
                    log::warn!("video decoder: {e}");
                    return;
                }
            }
        }
        if slot.need_key && !key {
            // Waiting on a keyframe nobody asked for is waiting on luck; the core
            // rate-limits the request.
            self.lost(&member, i);
            return;
        }
        slot.need_key = false;
        let picture = match slot.dec.as_mut().expect("decoder just made") {
            Dec::H264(dec) => match dec.decode(&frame[VIDEO_HEADER_LEN..]) {
                Ok(Some(yuv)) => {
                    let (w, h) = yuv.dimensions();
                    if w * h > MAX_DECODE_PX || w == 0 || h == 0 {
                        None
                    } else {
                        let (ys, us, _) = yuv.strides();
                        Some(Ok(Pic::new(w as u32, h as u32, yuv::to_bgra(yuv.y(), yuv.u(), yuv.v(), (ys, us), w, h))))
                    }
                }
                Ok(None) => return,
                Err(e) => Some(Err(e.to_string())),
            },
            Dec::Vp8(dec) => match dec.decode(&frame[VIDEO_HEADER_LEN..], MAX_DECODE_PX) {
                Ok(Some((w, h, bgra))) => Some(Ok(Pic::new(w, h, bgra))),
                Ok(None) => return,
                Err(e) => Some(Err(e)),
            },
        };
        match picture {
            Some(Ok(Some(pic))) => {
                slot.frames += 1;
                let mut p = self.pics.lock().unwrap_or_else(|e| e.into_inner());
                let old = match &member {
                    Some(m) => p.members.insert((m.clone(), i), pic),
                    None => p.peer[i].replace(pic),
                };
                if let Some(old) = old {
                    p.retired.push(old.image);
                }
                drop(p);
                self.wake.ping();
            }
            Some(Err(e)) => {
                slot.reset();
                self.dec_errors += 1;
                if self.dec_errors >= 8 {
                    log::warn!("video decode: {e}");
                    self.control(&FromLink::Unsupported { codec: h.codec, peer: member.clone() });
                    self.dec_errors = 0;
                } else {
                    self.lost(&member, i);
                }
            }
            _ => {
                slot.reset();
                self.lost(&member, i);
            }
        }
    }

    fn stats(&mut self) {
        let mut fps = [0; 2];
        for (i, o) in self.out.iter_mut().enumerate() {
            if o.spec.is_some() {
                let (w, h) = o.enc_size;
                let msg = FromLink::Stats { kind: KINDS[i], fps: o.frames, kbps: (o.bytes * 8 / 1000) as u32, width: w as u32, height: h as u32 };
                let mut out = vec![KIND_CONTROL];
                if serde_json::to_writer(&mut out, &msg).is_ok() {
                    let _ = self.to_core.try_send(Bytes::from(out));
                }
            }
            o.frames = 0;
            o.bytes = 0;
            fps[i] = std::mem::take(&mut self.inn[i].frames);
        }
        let mut p = self.pics();
        if p.dec_fps != fps {
            p.dec_fps = fps;
            drop(p);
            self.wake.ping();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gradient(w: usize, h: usize, t: usize) -> I420 {
        let rgb: Vec<u8> = (0..w * h).flat_map(|i| [((i % w + t) % 256) as u8, ((i / w) % 256) as u8, 90]).collect();
        I420::from_rgb(&rgb, w, h, w * 3, 3, false).unwrap()
    }

    #[test]
    fn our_frames_decode_back_through_the_link() {
        let (to_core, mut core_rx) = mpsc::channel::<Bytes>(64);
        let (_tx, rx) = std::sync::mpsc::channel();
        let (wtx, _wrx) = futures::channel::mpsc::unbounded();
        let pics = Arc::new(Mutex::new(Pictures::default()));
        let mut w = Worker::new(rx, to_core, Arc::clone(&pics), Wake { tx: wtx, pending: Arc::default() });
        w.codec = Some(VideoCodec::H264);
        w.start_capture(VideoKind::Camera, 30, 800);
        w.set_rate(VideoKind::Camera, Rate { kbps: 800, width: 640, height: 360, fps: 30 });
        for t in 0..10 {
            w.on_captured(0, gradient(1280, 720, t * 4), t as u64 * 40_000);
        }
        let mut sent = Vec::new();
        while let Ok(b) = core_rx.try_recv() {
            sent.push(b);
        }
        assert!(sent.len() >= 8, "{} frames", sent.len());
        let first = VideoHeader::parse(&sent[0][1..]).unwrap();
        assert!(first.flags & VIDEO_KEY != 0, "the first frame is a keyframe");
        assert_eq!(&sent[0][1 + VIDEO_HEADER_LEN..1 + VIDEO_HEADER_LEN + 4], &[0, 0, 0, 1], "Annex B");
        for b in &sent {
            w.on_link(b);
        }
        let p = pics.lock().unwrap();
        let pic = p.peer[0].as_ref().expect("decoded");
        assert_eq!((pic.w, pic.h), (640, 360));
    }
}
