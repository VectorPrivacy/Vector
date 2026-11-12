//! The screen as a picture source: what can be shared (every monitor, every window), a
//! thumbnail of each for the picker, and a capture thread for the one chosen. X11 is read
//! straight off the server; macOS and Windows go through `xcap`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use vector_core::calls::wire::VideoKind;

use super::yuv::I420;
use super::{Capture, In, clock_us};

/// The frame rate the ladder wants of the screen; the capture loop paces itself by it.
pub static FPS: AtomicU32 = AtomicU32::new(15);

const THUMB_W: u32 = 320;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Target {
    Monitor(u32),
    Window(u32),
}

pub struct Source {
    pub target: Target,
    pub name: String,
    /// A small BGRA picture of it, as it looks now.
    pub thumb: Option<(u32, u32, Vec<u8>)>,
}

/// One grab: packed 4-byte pixels, BGRX or RGBA.
struct Grab {
    w: usize,
    h: usize,
    stride: usize,
    bgr: bool,
    data: Vec<u8>,
}

impl Grab {
    fn i420(&self) -> Option<I420> {
        I420::from_rgb(&self.data, self.w, self.h, self.stride, 4, self.bgr)
    }

    fn thumb(&self) -> Option<(u32, u32, Vec<u8>)> {
        let pic = self.i420()?;
        let tw = (THUMB_W as usize).min(pic.w) & !1;
        let th = ((pic.h * tw) / pic.w.max(1)).max(2) & !1;
        let small = pic.scale(tw, th);
        let (y, u, v) = small.planes();
        Some((small.w as u32, small.h as u32, super::yuv::to_bgra(y, u, v, (small.w, small.w / 2), small.w, small.h)))
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use super::{Grab, Target};
    use x11rb::connection::Connection;
    use x11rb::protocol::randr::ConnectionExt as _;
    use x11rb::protocol::xproto::{AtomEnum, ConnectionExt as _, ImageFormat, Window};
    use x11rb::rust_connection::RustConnection;

    pub struct Session {
        conn: RustConnection,
        root: Window,
    }

    fn err(e: impl std::fmt::Display) -> String {
        e.to_string()
    }

    impl Session {
        pub fn open() -> Result<Self, String> {
            if std::env::var_os("DISPLAY").is_none() {
                return Err("Screen sharing is not available on this system".into());
            }
            let (conn, n) = x11rb::connect(None).map_err(|_| "Screen sharing is not available on this system".to_string())?;
            let root = conn.setup().roots[n].root;
            Ok(Self { conn, root })
        }

        fn atom(&self, name: &str) -> Option<u32> {
            Some(self.conn.intern_atom(false, name.as_bytes()).ok()?.reply().ok()?.atom)
        }

        fn rects(&self) -> Vec<(u32, String, (i16, i16, u16, u16))> {
            let Ok(Ok(r)) = self.conn.randr_get_monitors(self.root, true).map(|c| c.reply()) else {
                let g = self.conn.get_geometry(self.root).ok().and_then(|c| c.reply().ok());
                return g.map(|g| vec![(0, "Entire screen".into(), (0, 0, g.width, g.height))]).unwrap_or_default();
            };
            r.monitors
                .iter()
                .enumerate()
                .map(|(i, m)| {
                    let name = self
                        .conn
                        .get_atom_name(m.name)
                        .ok()
                        .and_then(|c| c.reply().ok())
                        .map(|n| String::from_utf8_lossy(&n.name).into_owned())
                        .unwrap_or_else(|| format!("Screen {}", i + 1));
                    (i as u32, name, (m.x, m.y, m.width, m.height))
                })
                .collect()
        }

        fn name_of(&self, w: Window) -> Option<String> {
            let utf8 = self.atom("UTF8_STRING")?;
            let net = self.atom("_NET_WM_NAME")?;
            let read = |prop: u32, ty: u32| {
                let r = self.conn.get_property(false, w, prop, ty, 0, 1024).ok()?.reply().ok()?;
                (!r.value.is_empty()).then(|| String::from_utf8_lossy(&r.value).into_owned())
            };
            read(net, utf8).or_else(|| read(AtomEnum::WM_NAME.into(), AtomEnum::STRING.into()))
        }

        pub fn list(&self) -> Vec<(Target, String)> {
            let mut out: Vec<(Target, String)> = self.rects().into_iter().map(|(i, name, _)| (Target::Monitor(i), name)).collect();
            if out.len() == 1 {
                out[0].1 = "Entire screen".into();
            }
            let Some(list) = self.atom("_NET_CLIENT_LIST") else { return out };
            let Ok(Ok(r)) = self.conn.get_property(false, self.root, list, AtomEnum::WINDOW, 0, 4096).map(|c| c.reply()) else { return out };
            for w in r.value32().into_iter().flatten() {
                let Some(g) = self.conn.get_geometry(w).ok().and_then(|c| c.reply().ok()) else { continue };
                if g.width < 32 || g.height < 32 {
                    continue;
                }
                if let Some(name) = self.name_of(w) {
                    out.push((Target::Window(w), name));
                }
            }
            out
        }

        pub fn grab(&self, t: Target) -> Result<Grab, String> {
            let (drawable, (x, y, w, h)) = match t {
                Target::Monitor(i) => (self.root, self.rects().into_iter().find(|r| r.0 == i).ok_or("The screen is gone")?.2),
                Target::Window(w) => {
                    let g = self.conn.get_geometry(w).map_err(err)?.reply().map_err(|_| "The window was closed".to_string())?;
                    (w, (0, 0, g.width, g.height))
                }
            };
            let img = self.conn.get_image(ImageFormat::Z_PIXMAP, drawable, x, y, w, h, !0).map_err(err)?.reply().map_err(err)?;
            let (w, h) = (w as usize, h as usize);
            if img.data.len() < w * h * 4 {
                return Err("This screen's pixel format isn't supported".into());
            }
            Ok(Grab { w, h, stride: img.data.len() / h, bgr: true, data: img.data })
        }
    }
}

#[cfg(not(target_os = "linux"))]
mod imp {
    use super::{Grab, Target};

    pub struct Session;

    fn grab_of(img: image::RgbaImage) -> Grab {
        let (w, h) = (img.width() as usize, img.height() as usize);
        Grab { w, h, stride: w * 4, bgr: false, data: img.into_raw() }
    }

    impl Session {
        pub fn open() -> Result<Self, String> {
            Ok(Self)
        }

        pub fn list(&self) -> Vec<(Target, String)> {
            let mut out = Vec::new();
            if let Ok(ms) = xcap::Monitor::all() {
                let one = ms.len() == 1;
                for (i, m) in ms.iter().enumerate() {
                    let Ok(id) = m.id() else { continue };
                    let name = if one { "Entire screen".into() } else { m.friendly_name().or_else(|_| m.name()).unwrap_or_else(|_| format!("Screen {}", i + 1)) };
                    out.push((Target::Monitor(id), name));
                }
            }
            if let Ok(ws) = xcap::Window::all() {
                for w in ws {
                    if w.is_minimized().unwrap_or(true) || w.width().unwrap_or(0) < 32 || w.height().unwrap_or(0) < 32 {
                        continue;
                    }
                    let (Ok(id), Ok(title)) = (w.id(), w.title()) else { continue };
                    if title.is_empty() {
                        continue;
                    }
                    let app = w.app_name().unwrap_or_default();
                    out.push((Target::Window(id), if app.is_empty() || app == title { title } else { format!("{title} — {app}") }));
                }
            }
            out
        }

        pub fn grab(&self, t: Target) -> Result<Grab, String> {
            match t {
                Target::Monitor(id) => {
                    let m = xcap::Monitor::all().map_err(|e| e.to_string())?.into_iter().find(|m| m.id().ok() == Some(id)).ok_or("The screen is gone")?;
                    m.capture_image().map(grab_of).map_err(|e| e.to_string())
                }
                Target::Window(id) => {
                    let w = xcap::Window::all().map_err(|e| e.to_string())?.into_iter().find(|w| w.id().ok() == Some(id)).ok_or("The window was closed")?;
                    w.capture_image().map(grab_of).map_err(|e| e.to_string())
                }
            }
        }
    }
}

/// Everything that can be shared, each with a thumbnail. Slow (a grab apiece): call it off
/// the UI thread.
pub fn sources() -> Result<Vec<Source>, String> {
    let s = imp::Session::open()?;
    Ok(s.list()
        .into_iter()
        .map(|(target, name)| Source { target, name, thumb: s.grab(target).ok().and_then(|g| g.thumb()) })
        .collect())
}

/// Shares `target` until the capture is dropped; a source that disappears ends it.
pub fn open(target: Target, to: Sender<In>) -> Result<Capture, String> {
    let first = imp::Session::open()?.grab(target)?;
    drop(first);
    let stop = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&stop);
    std::thread::Builder::new()
        .name("screen".into())
        .spawn(move || {
            let Ok(s) = imp::Session::open() else { return };
            let mut failures = 0;
            while !flag.load(Ordering::Relaxed) {
                let t0 = Instant::now();
                match s.grab(target) {
                    Ok(g) => {
                        failures = 0;
                        if let Some(pic) = g.i420()
                            && to.send(In::Frame { kind: VideoKind::Screen, pic, ts_us: clock_us() }).is_err()
                        {
                            break;
                        }
                    }
                    Err(e) => {
                        failures += 1;
                        if failures > 10 {
                            log::warn!("screen capture ended: {e}");
                            let _ = to.send(In::SourceLost(VideoKind::Screen));
                            break;
                        }
                    }
                }
                let gap = Duration::from_secs_f32(1. / FPS.load(Ordering::Relaxed).clamp(1, 60) as f32);
                std::thread::sleep(gap.saturating_sub(t0.elapsed()));
            }
        })
        .map_err(|e| e.to_string())?;
    Ok(Capture { stop })
}
