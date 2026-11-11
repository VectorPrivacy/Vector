//! What the timeline draws attachments from: thumbhash blurs, pictures decoded once at the size
//! they draw, and each download's progress. Decoding runs off the UI thread; a picture larger
//! than its box is resampled to the box's device pixels, so the GPU never holds a full photo.

use std::collections::{HashMap, HashSet};
use std::io::Cursor;
use std::path::PathBuf;
use std::sync::Arc;

use gpui::{App, RenderImage, WeakEntity};
use image::{AnimationDecoder, DynamicImage, Frame, ImageDecoder, ImageReader, imageops::FilterType};

/// A bubble's picture box: `min(450/w, 350/h, 1)`.
pub const MAX_W: f32 = 450.;
pub const MAX_H: f32 = 350.;

pub fn fit(w: u32, h: u32) -> (f32, f32) {
    let (w, h) = (w.max(1) as f32, h.max(1) as f32);
    let s = (MAX_W / w).min(MAX_H / h).min(1.);
    (w * s, h * s)
}

#[derive(Clone)]
pub enum Slot {
    Loading,
    Ready { image: Arc<RenderImage>, w: u32, h: u32 },
    Failed,
}

/// A transfer in flight, as its events last reported it.
#[derive(Clone, Default)]
pub struct Transfer {
    /// Percent, once a length is known.
    pub pct: Option<f32>,
    pub bps: Option<f64>,
    /// What it is doing while no bytes move ("decrypting", "saving").
    pub stage: Option<String>,
}

impl Transfer {
    /// `transferStageText`, else the rate.
    pub fn note(&self) -> String {
        if let Some(s) = &self.stage {
            return match s.as_str() {
                "verifying" => "Verifying".into(),
                "decrypting" => "Decrypting".into(),
                "saving" => "Saving".into(),
                other => other.to_string(),
            };
        }
        match self.bps.filter(|&b| b > 0.) {
            Some(b) => format!("{}/s", format_bytes_rate(b)),
            None => String::new(),
        }
    }
}

pub enum Source {
    Held([u8; 32]),
    File(PathBuf),
}

struct Want {
    key: String,
    hash: Option<[u8; 32]>,
    path: Option<PathBuf>,
    ext: String,
    max: (u32, u32),
}

#[derive(Default)]
pub struct Media {
    thumbs: HashMap<(String, u32, u32), Option<Arc<RenderImage>>>,
    images: HashMap<String, Slot>,
    /// Spoiler pictures tapped open, by attachment id.
    pub revealed: HashSet<String>,
    /// Downloads running, by attachment id.
    pub transfers: HashMap<String, Transfer>,
    /// Downloads asked for from this client and not answered yet.
    pub started: HashSet<String>,
    /// Why a download failed, by attachment id.
    pub failed: HashMap<String, String>,
    /// Attachments whose auto-download already fired.
    pub auto_fired: HashSet<String>,
    wants: Vec<Want>,
    /// Downloads the renderer asks the app to start: (msg id, attachment id).
    pub auto_queue: Vec<(String, String)>,
}

impl Media {
    /// A thumbhash's blur, stretched bilinearly to `px` device pixels on the CPU: the GPU's
    /// sampler would bleed the atlas around a ~32px tile into its edges.
    pub fn thumb(&mut self, hash: &str, px: (u32, u32)) -> Option<Arc<RenderImage>> {
        if hash.is_empty() {
            return None;
        }
        self.thumbs.entry((hash.to_string(), px.0, px.1)).or_insert_with(|| decode_thumbhash(hash, px).map(Arc::new)).clone()
    }

    /// An attachment's picture; the first ask queues its decode at `max` device pixels.
    pub fn image(&mut self, key: &str, source: Source, ext: &str, max: (u32, u32)) -> Slot {
        if let Some(s) = self.images.get(key) {
            return s.clone();
        }
        self.images.insert(key.to_string(), Slot::Loading);
        let (hash, path) = match source {
            Source::Held(h) => (Some(h), None),
            Source::File(p) => (None, Some(p)),
        };
        self.wants.push(Want { key: key.to_string(), hash, path, ext: ext.to_ascii_lowercase(), max });
        Slot::Loading
    }
}

/// Start the decodes the last render asked for; the app repaints as each lands.
pub fn pump<V: 'static>(media: &std::rc::Rc<std::cell::RefCell<Media>>, app: WeakEntity<V>, live: bool, cx: &mut App) {
    let wants = std::mem::take(&mut media.borrow_mut().wants);
    for w in wants {
        let media = media.clone();
        let app = app.clone();
        cx.spawn(async move |cx| {
            let bytes = if live {
                let path = w.path.as_ref().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
                crate::backend::run(crate::backend::attachments::bytes(w.hash, path)).await.ok().flatten()
            } else {
                let path = w.path.clone();
                cx.background_executor().spawn(async move { path.and_then(|p| std::fs::read(p).ok()) }).await
            };
            let (ext, max) = (w.ext.clone(), w.max);
            let decoded = match bytes {
                Some(b) => cx.background_executor().spawn(async move { decode(&b, &ext, max) }).await,
                None => None,
            };
            let slot = match decoded {
                Some((image, w, h)) => Slot::Ready { image: Arc::new(image), w, h },
                None => Slot::Failed,
            };
            media.borrow_mut().images.insert(w.key, slot);
            app.update(cx, |_, cx| cx.notify()).ok();
        })
        .detach();
    }
}

fn to_bgra(mut buf: image::RgbaImage) -> image::RgbaImage {
    for px in buf.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    buf
}

fn decode_thumbhash(hash: &str, px: (u32, u32)) -> Option<RenderImage> {
    let bytes = fast_thumbhash::base91_decode(hash).ok()?;
    let (w, h, rgba) = fast_thumbhash::thumb_hash_to_rgba(&bytes).ok()?;
    let buf = image::RgbaImage::from_raw(w as u32, h as u32, rgba)?;
    let buf = image::imageops::resize(&buf, px.0.clamp(1, 4096), px.1.clamp(1, 4096), FilterType::Triangle);
    Some(RenderImage::new(vec![Frame::new(to_bgra(buf))]))
}

/// A picture as frames no larger than `max`, with its natural size.
pub fn decode(bytes: &[u8], ext: &str, max: (u32, u32)) -> Option<(RenderImage, u32, u32)> {
    if ext == "gif"
        && let Ok(dec) = image::codecs::gif::GifDecoder::new(Cursor::new(bytes))
    {
        let (w, h) = dec.dimensions();
        let frames = dec.into_frames().collect_frames().ok()?;
        if frames.len() > 1 {
            let frames: Vec<Frame> = frames
                .into_iter()
                .map(|f| {
                    let delay = f.delay();
                    let (left, top) = (f.left(), f.top());
                    Frame::from_parts(to_bgra(f.into_buffer()), left, top, delay)
                })
                .collect();
            return Some((RenderImage::new(frames), w, h));
        }
    }
    let img = if ext == "svg" {
        let png = vector_core::svg::rasterize_png(bytes, 1024).ok()?;
        image::load_from_memory(&png).ok()?
    } else {
        let mut dec = ImageReader::new(Cursor::new(bytes)).with_guessed_format().ok()?.into_decoder().ok()?;
        let orientation = dec.orientation().ok();
        let mut img = DynamicImage::from_decoder(dec).ok()?;
        if let Some(o) = orientation {
            img.apply_orientation(o);
        }
        img
    };
    let (w, h) = (img.width(), img.height());
    let img = if w > max.0 || h > max.1 { img.resize(max.0.max(1), max.1.max(1), FilterType::CatmullRom) } else { img };
    Some((RenderImage::new(vec![Frame::new(to_bgra(img.into_rgba8()))]), w, h))
}

/// `formatBytes`: binary units, two decimals with trailing zeros dropped.
pub fn format_bytes(n: u64) -> String {
    format_bytes_with(n as f64, 2, false)
}

/// A rate as the transfer line shows it: padded, so it doesn't jitter.
fn format_bytes_rate(bps: f64) -> String {
    format_bytes_with(bps, if bps >= 1048576. { 2 } else { 0 }, true)
}

fn format_bytes_with(n: f64, decimals: usize, pad: bool) -> String {
    const UNITS: [&str; 5] = ["Bytes", "KB", "MB", "GB", "TB"];
    if n == 0. {
        return "0 Bytes".into();
    }
    let (mut v, mut i) = (n, 0);
    while v >= 1024. && i < UNITS.len() - 1 {
        v /= 1024.;
        i += 1;
    }
    let s = format!("{v:.decimals$}");
    let s = if !pad && s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_string() } else { s };
    format!("{s} {}", UNITS[i])
}
