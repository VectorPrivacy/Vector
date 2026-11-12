//! Video on a call, in process: the link the call's video track hands frames over, a worker
//! that encodes ours and decodes theirs with OpenH264, and the camera. What the UI draws is
//! the newest picture of each track, already BGRA.

mod camera;
mod worker;
pub mod yuv;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::thread::JoinHandle;
use std::time::Instant;

use bytes::Bytes;
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use gpui::RenderImage;
use tokio::sync::mpsc;
pub use vector_core::calls::wire::VideoKind;
use vector_media::calls::link::{self, LinkConn};

pub use camera::Capture;
use yuv::I420;

/// Frames queued each way before the oldest are dropped, as the webview's socket has.
const LINK_DEPTH: usize = 8;

pub enum In {
    Link(Bytes),
    LinkClosed,
    Close,
    Frame { kind: VideoKind, pic: I420, ts_us: u64 },
    Capture { kind: VideoKind, fps: u32, kbps: u32 },
    Stop(VideoKind),
    SourceLost(VideoKind),
    /// A voice call member's picture is on screen (their key, hex; the track's index), or gone.
    Want((String, usize)),
    Unwant((String, usize)),
}

/// Microseconds on one clock for every capture.
pub fn clock_us() -> u64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_micros() as u64
}

/// The UI's nudge: one wake in flight at a time, however many pictures land meanwhile.
#[derive(Clone)]
pub struct Wake {
    tx: UnboundedSender<()>,
    pending: Arc<AtomicBool>,
}

impl Wake {
    pub fn ping(&self) {
        if !self.pending.swap(true, Ordering::AcqRel) {
            let _ = self.tx.unbounded_send(());
        }
    }
}

pub struct Pic {
    pub image: Arc<RenderImage>,
    pub w: u32,
    pub h: u32,
}

impl Pic {
    fn new(w: u32, h: u32, bgra: Vec<u8>) -> Option<Self> {
        let buf = image::RgbaImage::from_raw(w, h, bgra)?;
        Some(Self { image: Arc::new(RenderImage::new(vec![image::Frame::new(buf)])), w, h })
    }
}

#[derive(Default)]
pub struct Pictures {
    pub open: bool,
    /// The peer's camera and screen in a call of two.
    pub peer: [Option<Pic>; 2],
    /// Our own, as the corner thumbnails show them.
    pub mine: [Option<Pic>; 2],
    pub dec_fps: [u32; 2],
    /// A voice call's members' pictures, by their key (hex) and track.
    pub members: HashMap<(String, usize), Pic>,
    /// Captures that ended from outside (a camera unplugged).
    pub lost: Vec<VideoKind>,
    /// Replaced pictures, for the window to let go of.
    pub retired: Vec<Arc<RenderImage>>,
}

pub struct Video {
    pics: Arc<Mutex<Pictures>>,
    wake: Wake,
    wake_rx: Option<UnboundedReceiver<()>>,
    tx: Option<Sender<In>>,
    worker: Option<JoinHandle<()>>,
    /// The call (or voice Channel) the link is open for.
    link_for: Option<String>,
    captures: [Option<Capture>; 2],
    pub starting: [bool; 2],
}

impl Default for Video {
    fn default() -> Self {
        let (tx, rx) = unbounded();
        Self {
            pics: Arc::default(),
            wake: Wake { tx, pending: Arc::default() },
            wake_rx: Some(rx),
            tx: None,
            worker: None,
            link_for: None,
            captures: [None, None],
            starting: [false; 2],
        }
    }
}

/// What this device encodes and decodes, told to the core for every offer and answer.
pub fn announce_caps() {
    vector_media::calls::session::set_video_caps(vec!["h264".into()], vec!["h264".into()]);
}

impl Video {
    pub fn take_wake(&mut self) -> Option<(UnboundedReceiver<()>, Arc<AtomicBool>)> {
        self.wake_rx.take().map(|rx| (rx, Arc::clone(&self.wake.pending)))
    }

    pub fn pics(&self) -> MutexGuard<'_, Pictures> {
        self.pics.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn link_open(&self) -> bool {
        self.worker.as_ref().is_some_and(|w| !w.is_finished()) && self.pics().open
    }

    /// The link follows `key`: open while it names a call that can take video, closed when it
    /// changes or goes. True when the link closed under us, so our pictures stopped.
    pub fn follow(&mut self, key: Option<&str>) -> bool {
        let dead = self.worker.as_ref().is_some_and(|w| w.is_finished());
        if self.link_for.as_deref() == key && !dead {
            return false;
        }
        if self.link_for.as_deref() == key && dead {
            // The core dropped the link; whatever we capture goes nowhere now.
            self.worker = None;
            self.tx = None;
            let had = self.captures.iter().any(Option::is_some);
            self.captures = [None, None];
            return had;
        }
        let had = self.captures.iter().any(Option::is_some);
        self.close();
        if let Some(k) = key
            && self.open()
        {
            self.link_for = Some(k.to_string());
        }
        had && key.is_none()
    }

    fn open(&mut self) -> bool {
        let (core_tx, mut core_rx) = mpsc::channel::<Bytes>(LINK_DEPTH);
        let (us_tx, us_rx) = mpsc::channel::<Bytes>(LINK_DEPTH);
        if !link::connect(LinkConn { to_web: core_tx, from_web: us_rx }) {
            return false;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let fwd = tx.clone();
        let spawned = std::thread::Builder::new().name("video-link".into()).spawn(move || {
            while let Some(b) = core_rx.blocking_recv() {
                if fwd.send(In::Link(b)).is_err() {
                    return;
                }
            }
            let _ = fwd.send(In::LinkClosed);
        });
        if spawned.is_err() {
            return false;
        }
        let w = worker::Worker::new(rx, us_tx, Arc::clone(&self.pics), self.wake.clone());
        let Ok(handle) = std::thread::Builder::new().name("video".into()).spawn(move || w.run()) else { return false };
        self.tx = Some(tx);
        self.worker = Some(handle);
        true
    }

    fn close(&mut self) {
        self.captures = [None, None];
        self.starting = [false; 2];
        if let Some(tx) = self.tx.take() {
            let _ = tx.send(In::Close);
        }
        self.worker = None;
        self.link_for = None;
    }

    /// Opening a camera can wait on a system prompt: the caller runs this off the UI thread.
    pub fn camera_opener(&self, kind: VideoKind) -> Option<impl FnOnce() -> Result<Capture, String> + Send + 'static> {
        let tx = self.tx.clone()?;
        Some(move || camera::open(kind, tx))
    }

    pub fn attach(&mut self, kind: VideoKind, cap: Capture) {
        let i = kind.index();
        self.starting[i] = false;
        let (fps, kbps) = if kind == VideoKind::Screen { (15, 1000) } else { (30, 800) };
        if let Some(tx) = &self.tx {
            let _ = tx.send(In::Capture { kind, fps, kbps });
        }
        self.captures[i] = Some(cap);
    }

    pub fn stop(&mut self, kind: VideoKind) {
        self.captures[kind.index()] = None;
        if let Some(tx) = &self.tx {
            let _ = tx.send(In::Stop(kind));
        }
    }

    pub fn capturing(&self, kind: VideoKind) -> bool {
        self.captures[kind.index()].is_some()
    }

    pub fn want(&self, peer: String, kind: VideoKind, on: bool) {
        if let Some(tx) = &self.tx {
            let key = (peer, kind.index());
            let _ = tx.send(if on { In::Want(key) } else { In::Unwant(key) });
        }
    }
}

impl Video {
    /// Pictures from files, for the parity renders: their camera and screen, then ours.
    pub fn show_fixture(&self, peer: [Option<image::RgbaImage>; 2], mine: [Option<image::RgbaImage>; 2]) {
        let pic = |img: image::RgbaImage| {
            let (w, h) = img.dimensions();
            let mut raw = img.into_raw();
            raw.chunks_exact_mut(4).for_each(|p| p.swap(0, 2));
            Pic::new(w, h, raw)
        };
        let mut p = self.pics();
        p.peer = peer.map(|i| i.and_then(pic));
        p.mine = mine.map(|i| i.and_then(pic));
        p.dec_fps = [30, 15];
    }
}
