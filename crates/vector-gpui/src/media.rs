//! What the timeline draws attachments from: thumbhash blurs, pictures decoded once at the size
//! they draw, and each download's progress. Decoding runs off the UI thread; a picture larger
//! than its box is resampled to the box's device pixels, so the GPU never holds a full photo.

use std::collections::{HashMap, HashSet};
use std::io::Cursor;
use std::path::PathBuf;
use std::sync::Arc;

use gpui::{App, RenderImage, WeakEntity};
use image::{
    AnimationDecoder, DynamicImage, Frame, ImageDecoder, ImageReader, imageops::FilterType,
};

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
    Ready {
        image: Arc<RenderImage>,
        w: u32,
        h: u32,
    },
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
    /// When the stage began, and the stage's own progress, when it reports one.
    pub stage_at: Option<std::time::Instant>,
    pub stage_pct: Option<f32>,
    /// When an upload's last byte left: the publish that follows has a full ring and no rate.
    pub sent_at: Option<std::time::Instant>,
}

/// Decoded pictures held past which the longest undrawn are let go before their idle time.
const DECODED_BUDGET: usize = 256 << 20;
/// Drawn this recently: kept whatever the budget says.
const RECENT: std::time::Duration = std::time::Duration::from_secs(5);
/// How long a stage or a publish holds the screen before it earns words: the common, quick
/// transfer goes by without a flash of text.
const GLANCE: std::time::Duration = std::time::Duration::from_secs(1);

/// `STAGE_WORDS`.
fn stage_word(stage: &str) -> &str {
    match stage {
        "encrypting" => "Encrypting",
        "starting" => "Starting upload",
        "decrypting" => "Decrypting",
        "verifying" => "Verifying",
        "saving" => "Saving",
        other => other,
    }
}

impl Transfer {
    /// The last byte has left: nothing is left to cancel.
    pub fn publishing(&self) -> bool {
        self.pct.is_some_and(|p| p >= 100.)
    }

    /// `UploadOverlay`'s line: "Sending" for a publish that drags on, else the stage once it
    /// has held a moment (`transferStageText`), else nothing while the ring speaks for itself.
    pub fn upload_note(&self) -> String {
        if self.sent_at.is_some_and(|t| t.elapsed() >= GLANCE) {
            return "Sending".into();
        }
        let Some(stage) = self.stage.as_deref().filter(|_| self.stage_at.is_some_and(|t| t.elapsed() >= GLANCE)) else {
            return String::new();
        };
        let word = stage_word(stage);
        match self.stage_pct.filter(|&p| p < 100.) {
            Some(p) => format!("{word} {}%", p as u32),
            None => word.to_string(),
        }
    }

    /// `transferStageText`, else the rate.
    pub fn note(&self) -> String {
        if let Some(s) = &self.stage {
            return stage_word(s).to_string();
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

/// A picture a message links to.
#[derive(Clone)]
pub enum Linked {
    /// Fetching; the percent once its length is known.
    Loading(Option<f32>),
    Ready {
        image: Arc<RenderImage>,
        w: u32,
        h: u32,
    },
    /// Not a picture, or not reachable: the link stays a link.
    Failed,
}

/// A custom emoji's longest side, in device pixels: its largest display at 3x.
const EMOJI_MAX: u32 = 160;

/// The longest side a linked picture is decoded to, in device pixels.
const LINKED_MAX: (u32, u32) = (2048, 2048);

struct Want {
    key: String,
    hash: Option<[u8; 32]>,
    path: Option<PathBuf>,
    ext: String,
    max: (u32, u32),
}

#[derive(Default)]
pub struct Media {
    /// Audio attachments and the engine sources behind them.
    pub audio: crate::audio::Audio,
    pub videos: crate::video::Videos,
    pub models: crate::model3d::stage::Models,
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
    /// Downloads stopped part-way with their bytes kept on disk, by attachment id: (bytes, total).
    pub paused: HashMap<String, (u64, Option<u64>)>,
    wants: Vec<Want>,
    /// Downloads the renderer asks the app to start: (msg id, attachment id).
    pub auto_queue: Vec<(String, String)>,
    linked: HashMap<String, Linked>,
    linked_wants: Vec<String>,
    /// The messages each linked picture shows in, and the pictures done loading (or failed)
    /// since the app last looked: their rows change height, so the timeline measures them again.
    linked_by: HashMap<String, HashSet<String>>,
    pub linked_settled: Vec<String>,
    /// Messages whose link card was asked for, and those asks waiting to go: (chat, message).
    pub meta_asked: HashSet<String>,
    pub meta_queue: Vec<(String, String)>,
    /// What fetches on sight: on, up to a size.
    pub auto_download: AutoDownload,
    /// Nostr cards, by the references messages carry.
    pub embeds: crate::embeds::Cache,
    /// Emoji packs shared in messages.
    pub packs: crate::packs::Cache,
    /// "Display Web Previews" turned off: no link or Nostr cards.
    pub web_previews_off: bool,
    /// "Display Image Types": a picture's file type in its corner.
    pub image_types: bool,
    /// Custom emoji and pack icons on disk, by URL, and those asked for since the last pump.
    pub custom: HashMap<String, Custom>,
    custom_wants: Vec<(String, bool)>,
    /// Mini App packages' cards by attachment id: None while read, or unreadable.
    pub xdc: HashMap<String, Option<crate::miniapp::Info>>,
    pub xdc_queue: Vec<(String, Source)>,
    /// Mini Apps shared as links, by message and link, and the cache probes waiting to go.
    pub xdc_urls: HashMap<String, crate::attachments::XdcUrl>,
    pub xdc_url_queue: Vec<(String, String)>,
    /// Embedded videos' local copies by URL, the fetches waiting to start, and those to play
    /// once ready.
    pub embed_videos: HashMap<String, crate::embeds::VideoState>,
    pub embed_video_queue: Vec<crate::embeds::Media>,
    pub embed_autoplay: HashSet<String>,
    /// First-frame openings for videos with no poster that won't download on their own: None
    /// while fetching or when there is none.
    pub embed_previews: HashMap<String, Option<crate::audio::AudioInputSpec>>,
    pub embed_preview_queue: Vec<crate::embeds::Media>,
    /// Realtime sessions, by topic (or `att:<id>` for an app without one).
    pub xdc_live: HashMap<String, XdcLive>,
    /// When each decoded picture was last drawn, so one off screen for long is let go.
    used: HashMap<(u8, String), std::time::Instant>,
}

/// A Mini App session as its card reports it.
#[derive(Clone, Default)]
pub struct XdcLive {
    pub active: bool,
    pub peers: Vec<String>,
}

/// A custom emoji as a message draws it.
#[derive(Clone)]
pub enum Custom {
    Ready(Arc<RenderImage>),
    Pending,
    /// Not loadable, and why: the shortcode stays as text.
    Missing(String),
}

#[derive(Clone, Copy)]
pub struct AutoDownload {
    pub on: bool,
    pub limit: u64,
}

impl Default for AutoDownload {
    fn default() -> Self {
        Self {
            on: true,
            limit: 10_485_760,
        }
    }
}

impl Media {
    /// "Resume download · N%" for a download stopped part-way.
    pub fn resume_note(&self, id: &str, size: u64) -> Option<String> {
        let &(done, total) = self.paused.get(id)?;
        let total = total.or((size > 0).then_some(size)).filter(|&t| t > 0);
        Some(match total {
            Some(t) => format!("Resume download · {}%", (done.saturating_mul(100) / t).min(99)),
            None => format!("Resume download · {}", format_bytes(done)),
        })
    }

    /// A thumbhash's blur, stretched bilinearly to `px` device pixels on the CPU: the GPU's
    /// sampler would bleed the atlas around a ~32px tile into its edges.
    pub fn thumb(&mut self, hash: &str, px: (u32, u32)) -> Option<Arc<RenderImage>> {
        if hash.is_empty() {
            return None;
        }
        self.used.insert((0, format!("{hash}\u{0}{}\u{0}{}", px.0, px.1)), std::time::Instant::now());
        self.thumbs
            .entry((hash.to_string(), px.0, px.1))
            .or_insert_with(|| decode_thumbhash(hash, px).map(Arc::new))
            .clone()
    }

    /// An attachment's picture; the first ask queues its decode at `max` device pixels.
    pub fn image(&mut self, key: &str, source: Source, ext: &str, max: (u32, u32)) -> Slot {
        self.used.insert((1, key.to_string()), std::time::Instant::now());
        if let Some(s) = self.images.get(key) {
            return s.clone();
        }
        self.images.insert(key.to_string(), Slot::Loading);
        let (hash, path) = match source {
            Source::Held(h) => (Some(h), None),
            Source::File(p) => (None, Some(p)),
        };
        self.wants.push(Want {
            key: key.to_string(),
            hash,
            path,
            ext: ext.to_ascii_lowercase(),
            max,
        });
        Slot::Loading
    }
}

impl Media {
    /// A linked picture shown in message `msg`; its row is measured again once it loads.
    pub fn linked_in(&mut self, url: &str, msg: &str) -> Linked {
        let l = self.linked(url);
        if matches!(l, Linked::Loading(_)) {
            self.linked_by.entry(url.to_string()).or_default().insert(msg.to_string());
        }
        l
    }

    /// The messages waiting on pictures that have settled since the last ask.
    pub fn take_settled_rows(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        for url in std::mem::take(&mut self.linked_settled) {
            out.extend(self.linked_by.remove(&url).into_iter().flatten());
        }
        out
    }

    /// A linked picture; the first ask fetches it.
    pub fn linked(&mut self, url: &str) -> Linked {
        self.used.insert((2, url.to_string()), std::time::Instant::now());
        if let Some(l) = self.linked.get(url) {
            return l.clone();
        }
        self.linked.insert(url.to_string(), Linked::Loading(None));
        self.linked_wants.push(url.to_string());
        Linked::Loading(None)
    }

    /// Each decoded cache: entries, decoded bytes, frames, and the largest entry's bytes.
    #[cfg(feature = "devctl")]
    pub fn dev_caches(&self) -> serde_json::Value {
        fn tally<'a>(images: impl Iterator<Item = &'a Arc<RenderImage>>) -> serde_json::Value {
            let (mut n, mut bytes, mut frames, mut largest) = (0usize, 0usize, 0usize, 0usize);
            for image in images {
                let b = image_bytes(image);
                n += 1;
                bytes += b;
                frames += image.frame_count();
                largest = largest.max(b);
            }
            serde_json::json!({ "entries": n, "mb": bytes as f64 / 1048576., "frames": frames, "largest_mb": largest as f64 / 1048576. })
        }
        serde_json::json!({
            "thumbs": tally(self.thumbs.values().flatten()),
            "images": tally(self.images.values().filter_map(|s| match s { Slot::Ready { image, .. } => Some(image), _ => None })),
            "linked": tally(self.linked.values().filter_map(|l| match l { Linked::Ready { image, .. } => Some(image), _ => None })),
            "custom": tally(self.custom.values().filter_map(|c| match c { Custom::Ready(image) => Some(image), _ => None })),
            "video_clips": self.videos.dev_clips(),
        })
    }

    /// Decoded pictures not drawn for `idle`, taken out of the caches for their atlas
    /// textures to be dropped; one drawn again later is decoded again. Past
    /// [`DECODED_BUDGET`], the longest undrawn go too, sparing what was just on screen.
    pub fn sweep(&mut self, idle: std::time::Duration) -> Vec<Arc<RenderImage>> {
        let now = std::time::Instant::now();
        let mut stale: Vec<((u8, String), std::time::Instant)> = self
            .used
            .iter()
            .filter(|(_, at)| now.duration_since(**at) > RECENT)
            .map(|(k, at)| (k.clone(), *at))
            .collect();
        stale.sort_unstable_by_key(|(_, at)| *at);
        let mut held = self.decoded_bytes();
        let mut out = Vec::new();
        for (key, at) in stale {
            if now.duration_since(at) <= idle && held <= DECODED_BUDGET {
                break;
            }
            let Some(image) = self.take(&key) else { continue };
            self.used.remove(&key);
            if let Some(image) = image {
                held = held.saturating_sub(image_bytes(&image));
                out.push(image);
            }
        }
        out
    }

    fn decoded_bytes(&self) -> usize {
        let thumbs = self.thumbs.values().flatten();
        let images = self.images.values().filter_map(|s| match s {
            Slot::Ready { image, .. } => Some(image),
            _ => None,
        });
        let linked = self.linked.values().filter_map(|l| match l {
            Linked::Ready { image, .. } => Some(image),
            _ => None,
        });
        let custom = self.custom.values().filter_map(|c| match c {
            Custom::Ready(image) => Some(image),
            _ => None,
        });
        thumbs.chain(images).chain(linked).chain(custom).map(|i| image_bytes(i)).sum()
    }

    /// The cached picture under a `used` key, removed; `None` while it is still loading.
    fn take(&mut self, key: &(u8, String)) -> Option<Option<Arc<RenderImage>>> {
        Some(match key.0 {
            0 => {
                let mut parts = key.1.split('\u{0}');
                let (Some(h), Some(w), Some(t)) = (parts.next(), parts.next(), parts.next()) else { return Some(None) };
                let (Ok(w), Ok(t)) = (w.parse(), t.parse()) else { return Some(None) };
                self.thumbs.remove(&(h.to_string(), w, t)).flatten()
            }
            1 => match self.images.get(&key.1)? {
                Slot::Ready { .. } => match self.images.remove(&key.1) {
                    Some(Slot::Ready { image, .. }) => Some(image),
                    _ => None,
                },
                _ => return None,
            },
            2 => match self.linked.get(&key.1)? {
                Linked::Ready { .. } => match self.linked.remove(&key.1) {
                    Some(Linked::Ready { image, .. }) => Some(image),
                    _ => None,
                },
                _ => return None,
            },
            _ => match self.custom.get(&key.1)? {
                Custom::Ready(_) => match self.custom.remove(&key.1) {
                    Some(Custom::Ready(image)) => Some(image),
                    _ => None,
                },
                _ => return None,
            },
        })
    }

    /// A custom emoji (`icon`: a pack's logo); the first ask fetches it.
    pub fn custom(&mut self, url: &str, icon: bool) -> Custom {
        self.used.insert((3, url.to_string()), std::time::Instant::now());
        if let Some(c) = self.custom.get(url) {
            return c.clone();
        }
        self.custom.insert(url.to_string(), Custom::Pending);
        self.custom_wants.push((url.to_string(), icon));
        Custom::Pending
    }

    pub fn linked_progress(&mut self, url: &str, pct: f32) {
        if let Some(Linked::Loading(p)) = self.linked.get_mut(url) {
            *p = Some(pct);
        }
    }
}

/// Start the decodes the last render asked for; the app repaints as each lands.
pub fn pump<V: 'static>(
    media: &std::rc::Rc<std::cell::RefCell<Media>>,
    app: WeakEntity<V>,
    live: bool,
    cx: &mut App,
) {
    let wants = std::mem::take(&mut media.borrow_mut().wants);
    for w in wants {
        let media = media.clone();
        let app = app.clone();
        cx.spawn(async move |cx| {
            let bytes = if live {
                let path = w
                    .path
                    .as_ref()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default();
                crate::backend::run(crate::backend::attachments::bytes(w.hash, path))
                    .await
                    .ok()
                    .flatten()
            } else {
                let path = w.path.clone();
                cx.background_executor()
                    .spawn(async move { path.and_then(|p| crate::pic::read(p).ok()) })
                    .await
            };
            let (ext, max) = (w.ext.clone(), w.max);
            let decoded = match bytes {
                Some(b) => {
                    cx.background_executor()
                        .spawn(async move { decode(&b, &ext, max) })
                        .await
                }
                None => None,
            };
            let slot = match decoded {
                Some((image, w, h)) => Slot::Ready {
                    image: Arc::new(image),
                    w,
                    h,
                },
                None => Slot::Failed,
            };
            media.borrow_mut().images.insert(w.key, slot);
            app.update(cx, |_, cx| cx.notify()).ok();
        })
        .detach();
    }
    let custom = std::mem::take(&mut media.borrow_mut().custom_wants);
    for (url, icon) in custom {
        let media = media.clone();
        let app = app.clone();
        let rx =
            live.then(|| crate::backend::run(crate::backend::images::emoji(url.clone(), icon)));
        cx.spawn(async move |cx| {
            let res = match rx {
                Some(rx) => rx.await.unwrap_or_else(|_| Err("Cancelled".into())),
                None if std::path::Path::new(&url).is_file() => Ok(url.clone()),
                None => Err("Not found".into()),
            };
            let decoded = match res {
                Ok(path) => {
                    let bytes = cx
                        .background_executor()
                        .spawn(async move { crate::pic::read(&path).map(|b| (b, path)) })
                        .await;
                    match bytes {
                        Ok((b, path)) => {
                            let ext = std::path::Path::new(&path)
                                .extension()
                                .and_then(|e| e.to_str())
                                .unwrap_or("png")
                                .to_ascii_lowercase();
                            cx.background_executor()
                                .spawn(async move { decode(&b, &ext, (EMOJI_MAX, EMOJI_MAX)) })
                                .await
                                .ok_or_else(|| "Invalid image".to_string())
                        }
                        Err(e) => Err(e.to_string()),
                    }
                }
                Err(e) => Err(e),
            };
            let state = match decoded {
                Ok((image, ..)) => Custom::Ready(Arc::new(image)),
                Err(e) => Custom::Missing(e),
            };
            media.borrow_mut().custom.insert(url, state);
            app.update(cx, |_, cx| cx.notify()).ok();
        })
        .detach();
    }
    let linked = std::mem::take(&mut media.borrow_mut().linked_wants);
    for url in linked {
        let media = media.clone();
        let app = app.clone();
        // A fixture's pictures are files beside it.
        let local = (!live && !url.starts_with("https://")).then(|| std::path::PathBuf::from(&url));
        let rx = local
            .is_none()
            .then(|| crate::backend::run(crate::backend::inline::image(url.clone())));
        cx.spawn(async move |cx| {
            let bytes = match (rx, local) {
                (Some(rx), _) => rx.await.ok().flatten(),
                (None, Some(path)) => {
                    cx.background_executor()
                        .spawn(async move { crate::pic::read(path).ok() })
                        .await
                }
                _ => None,
            };
            let decoded = match bytes {
                Some(b) => {
                    let ext = crate::backend::inline::sniff(&b).unwrap_or("png");
                    cx.background_executor()
                        .spawn(async move { decode(&b, ext, LINKED_MAX) })
                        .await
                }
                None => None,
            };
            let slot = match decoded {
                Some((image, w, h)) => Linked::Ready {
                    image: Arc::new(image),
                    w,
                    h,
                },
                None => Linked::Failed,
            };
            {
                let mut m = media.borrow_mut();
                m.linked.insert(url.clone(), slot);
                m.linked_settled.push(url);
            }
            app.update(cx, |_, cx| cx.notify()).ok();
        })
        .detach();
    }
}

pub fn to_bgra(mut buf: image::RgbaImage) -> image::RgbaImage {
    for px in buf.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    buf
}

pub fn decode_thumbhash(hash: &str, px: (u32, u32)) -> Option<RenderImage> {
    let bytes = fast_thumbhash::base91_decode(hash).ok()?;
    let (w, h, rgba) = fast_thumbhash::thumb_hash_to_rgba(&bytes).ok()?;
    let buf = image::RgbaImage::from_raw(w as u32, h as u32, rgba)?;
    let buf = image::imageops::resize(
        &buf,
        px.0.clamp(1, 4096),
        px.1.clamp(1, 4096),
        FilterType::Triangle,
    );
    Some(RenderImage::new(vec![Frame::new(to_bgra(buf))]))
}

/// An animation's frames (GIF, animated WebP or APNG, sniffed from the bytes, not the name),
/// each scaled to `to` when that is smaller. `None` for anything that doesn't animate.
fn gif_frames(bytes: &[u8], to: impl Fn(u32, u32) -> (u32, u32)) -> Option<(Vec<Frame>, u32, u32)> {
    if bytes.starts_with(b"GIF8") {
        let dec = image::codecs::gif::GifDecoder::new(Cursor::new(bytes)).ok()?;
        let (w, h) = dec.dimensions();
        // Each frame's Graphic Control Extension: a count close enough to size the budget by.
        let count = bytes.windows(3).filter(|w| *w == [0x21, 0xF9, 0x04]).count();
        return frames_of(dec.into_frames(), w, h, count, &to);
    }
    if bytes.len() > 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        // libwebp composites the frames: the image crate's compositor keeps parts of earlier
        // frames a later one should have cleared, which show as trails.
        let dec = crate::shared::animated_webp::Decoder::new(bytes).ok()?;
        if dec.frame_count() <= 1 {
            return None;
        }
        let (w, h) = dec.dimensions();
        let count = dec.frame_count() as usize;
        let frames = dec.map(|f| {
            f.map(|(rgba, ms)| Frame::from_parts(rgba, 0, 0, image::Delay::from_numer_denom_ms(ms, 1)))
                .map_err(|e| image::ImageError::IoError(std::io::Error::other(e)))
        });
        return frames_of(image::Frames::new(Box::new(frames)), w, h, count, &to);
    }
    // An APNG declares itself with `acTL` before its first `IDAT`.
    if bytes.starts_with(b"\x89PNG") {
        let head = &bytes[..bytes.len().min(64 * 1024)];
        let idat = head.windows(4).position(|w| w == b"IDAT").unwrap_or(head.len());
        if !head[..idat].windows(4).any(|w| w == b"acTL") {
            return None;
        }
        let (frames, w, h) = apng_frames(bytes)?;
        let count = frames.len();
        return frames_of(image::Frames::new(Box::new(frames.into_iter().map(Ok))), w, h, count, &to);
    }
    None
}

/// An APNG's frames composited on its canvas by the spec: each region blended (source or over)
/// at its offset, then disposed (kept, cleared, or put back as it was). The image crate's own
/// compositing gets "dispose to previous" wrong, leaving earlier frames behind.
fn apng_frames(bytes: &[u8]) -> Option<(Vec<Frame>, u32, u32)> {
    use png::{BlendOp, DisposeOp};
    let mut dec = png::Decoder::new(Cursor::new(bytes));
    dec.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = dec.read_info().ok()?;
    let (w, h) = (reader.info().width, reader.info().height);
    let count = reader.info().animation_control?.num_frames as usize;
    // The default image is a frame only when its own fcTL comes before it.
    let default_shown = reader.info().frame_control.is_some();
    let mut canvas = image::RgbaImage::new(w, h);
    let mut buf = vec![0u8; reader.output_buffer_size()?];
    let mut frames = Vec::with_capacity(count);
    let mut first = true;
    loop {
        let Ok(out) = reader.next_frame(&mut buf) else { break };
        if first && !default_shown {
            first = false;
            continue;
        }
        let Some(fc) = reader.info().frame_control else { break };
        let (color, _) = reader.output_color_type();
        let rgba = to_rgba(&buf[..out.buffer_size()], color, out.width, out.height)?;
        let (fx, fy) = (fc.x_offset, fc.y_offset);
        if fx + out.width > w || fy + out.height > h {
            return None;
        }
        let dispose = if first && fc.dispose_op == DisposeOp::Previous { DisposeOp::Background } else { fc.dispose_op };
        first = false;
        let saved = (dispose == DisposeOp::Previous).then(|| image::imageops::crop_imm(&canvas, fx, fy, out.width, out.height).to_image());
        for (x, y, src) in rgba.enumerate_pixels() {
            let dst = canvas.get_pixel_mut(fx + x, fy + y);
            *dst = match fc.blend_op {
                BlendOp::Source => *src,
                BlendOp::Over => over(*src, *dst),
            };
        }
        let den = if fc.delay_den == 0 { 100 } else { fc.delay_den as u32 };
        let delay = image::Delay::from_numer_denom_ms(fc.delay_num as u32 * 1000, den);
        frames.push(Frame::from_parts(canvas.clone(), 0, 0, delay));
        match dispose {
            DisposeOp::None => {}
            DisposeOp::Background => {
                for y in fy..fy + out.height {
                    for x in fx..fx + out.width {
                        canvas.put_pixel(x, y, image::Rgba([0, 0, 0, 0]));
                    }
                }
            }
            DisposeOp::Previous => {
                if let Some(saved) = saved {
                    image::imageops::replace(&mut canvas, &saved, fx as i64, fy as i64);
                }
            }
        }
        if frames.len() >= count {
            break;
        }
    }
    (!frames.is_empty()).then_some((frames, w, h))
}

/// A decoded PNG frame (8 bits a channel, palettes expanded) as RGBA.
fn to_rgba(data: &[u8], color: png::ColorType, w: u32, h: u32) -> Option<image::RgbaImage> {
    let px: Vec<u8> = match color {
        png::ColorType::Rgba => data.to_vec(),
        png::ColorType::Rgb => data.chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
        png::ColorType::GrayscaleAlpha => data.chunks_exact(2).flat_map(|p| [p[0], p[0], p[0], p[1]]).collect(),
        png::ColorType::Grayscale => data.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        png::ColorType::Indexed => return None,
    };
    image::RgbaImage::from_raw(w, h, px)
}

/// `src` over `dst`, straight alpha.
fn over(src: image::Rgba<u8>, dst: image::Rgba<u8>) -> image::Rgba<u8> {
    let sa = src[3] as u32;
    if sa == 255 {
        return src;
    }
    if sa == 0 {
        return dst;
    }
    let da = dst[3] as u32 * (255 - sa) / 255;
    let a = sa + da;
    let mix = |s: u8, d: u8| ((s as u32 * sa + d as u32 * da) / a.max(1)) as u8;
    image::Rgba([mix(src[0], dst[0]), mix(src[1], dst[1]), mix(src[2], dst[2]), a as u8])
}

/// The size an animation of `count` frames decodes at to fit [`ANIMATION_BUDGET`] with every
/// frame kept: shrunk as far as needed, but never below [`MIN_SHRINK`] of `size`. Past that,
/// frames are dropped instead.
fn fit_budget(size: (u32, u32), count: usize) -> (u32, u32) {
    let full = size.0 as f64 * size.1 as f64 * 4. * count.max(1) as f64;
    if full <= ANIMATION_BUDGET as f64 {
        return size;
    }
    // A little under the budget, so rounding never tips it over and halves the frames.
    let s = (ANIMATION_BUDGET as f64 * 0.98 / full).sqrt().max(MIN_SHRINK);
    (((size.0 as f64 * s) as u32).max(1), ((size.1 as f64 * s) as u32).max(1))
}

/// The smallest an animation is shrunk to keep all its frames: smaller still would soften it
/// more than dropping every other frame costs.
const MIN_SHRINK: f64 = 0.75;

/// Decoded bytes one animation may hold. Every frame is kept whole in memory and again on
/// the GPU, so a long animation keeps every other frame (twice as long each) until it fits.
const ANIMATION_BUDGET: usize = 24 << 20;

fn frames_of(
    frames: image::Frames<'_>,
    w: u32,
    h: u32,
    count: usize,
    to: &impl Fn(u32, u32) -> (u32, u32),
) -> Option<(Vec<Frame>, u32, u32)> {
    let (tw, th) = fit_budget(to(w, h), count);
    let scale = tw < w || th < h;
    let ms = |d: image::Delay| {
        let (n, d) = d.numer_denom_ms();
        // Browsers play 10ms or less as 100ms.
        match n / d.max(1) {
            0..=10 => 100,
            v => v,
        }
    };
    let mut kept: Vec<(image::RgbaImage, u32)> = Vec::new();
    let mut bytes = 0usize;
    let mut stride = 1usize;
    for (i, f) in frames.enumerate() {
        // A damaged frame ends the animation there, as browsers play what decodes.
        let Ok(f) = f else { break };
        let delay = ms(f.delay());
        if i % stride != 0 {
            if let Some(last) = kept.last_mut() {
                last.1 += delay;
            }
            continue;
        }
        let buf = f.into_buffer();
        let buf = if scale {
            gpui::resize_premultiplied(buf, tw.max(1), th.max(1))
        } else {
            buf
        };
        bytes += buf.len();
        kept.push((buf, delay));
        if bytes > ANIMATION_BUDGET && kept.len() > 1 {
            let mut halved = Vec::with_capacity(kept.len() / 2 + 1);
            let mut it = kept.into_iter();
            while let Some((buf, mut delay)) = it.next() {
                if let Some((_, d)) = it.next() {
                    delay += d;
                }
                halved.push((buf, delay));
            }
            kept = halved;
            bytes = kept.iter().map(|(b, _)| b.len()).sum();
            stride *= 2;
        }
    }
    let frames = kept
        .into_iter()
        .map(|(buf, delay)| Frame::from_parts(to_bgra(buf), 0, 0, image::Delay::from_numer_denom_ms(delay, 1)))
        .collect();
    Some((frames, w, h))
}

/// A GIF for a square tile it covers: no side drawn larger than `side` needs.
pub fn decode_gif_cover(bytes: &[u8], side: u32) -> Option<RenderImage> {
    let (frames, _, _) = gif_frames(bytes, |w, h| {
        let s = (side as f32 / w.min(h).max(1) as f32).min(1.);
        ((w as f32 * s).round() as u32, (h as f32 * s).round() as u32)
    })?;
    (!frames.is_empty()).then(|| RenderImage::new(frames))
}

/// A picture as frames no larger than `max`, with its natural size.
pub fn decode(bytes: &[u8], ext: &str, max: (u32, u32)) -> Option<(RenderImage, u32, u32)> {
    let _trim = crate::trim::OnDrop;
    if ext != "svg"
        && let Some((frames, w, h)) = gif_frames(bytes, |w, h| {
            let s = (max.0 as f32 / w.max(1) as f32)
                .min(max.1 as f32 / h.max(1) as f32)
                .min(1.);
            ((w as f32 * s).round() as u32, (h as f32 * s).round() as u32)
        })
        && frames.len() > 1
    {
        return Some((RenderImage::new(frames), w, h));
    }
    if ext != "svg" && image::guess_format(bytes).ok() == Some(image::ImageFormat::Jpeg) {
        let mut target = None;
        let scaled = gpui::decode_jpeg_scaled(bytes, |w, h| {
            if w <= max.0 && h <= max.1 {
                return (w, h);
            }
            // As `DynamicImage::resize` sizes it, so the result matches the full decode's.
            let r = (max.0.max(1) as f64 / w as f64).min(max.1.max(1) as f64 / h as f64);
            let t = (((w as f64 * r).round() as u32).max(1), ((h as f64 * r).round() as u32).max(1));
            target = Some(t);
            t
        });
        if let (Some((img, w, h)), Some((tw, th))) = (scaled, target) {
            let img = DynamicImage::ImageRgba8(img).resize_exact(tw, th, FilterType::CatmullRom);
            return Some((RenderImage::new(vec![Frame::new(to_bgra(img.into_rgba8()))]), w, h));
        }
    }
    let img = if ext == "svg" {
        let png = vector_core::svg::rasterize_png(bytes, 1024).ok()?;
        image::load_from_memory(&png).ok()?
    } else {
        let mut dec = ImageReader::new(Cursor::new(bytes))
            .with_guessed_format()
            .ok()?
            .into_decoder()
            .ok()?;
        let orientation = dec.orientation().ok();
        let mut img = DynamicImage::from_decoder(dec).ok()?;
        if let Some(o) = orientation {
            img.apply_orientation(o);
        }
        img
    };
    let (w, h) = (img.width(), img.height());
    let img = if w > max.0 || h > max.1 {
        img.resize(max.0.max(1), max.1.max(1), FilterType::CatmullRom)
    } else {
        img
    };
    Some((
        RenderImage::new(vec![Frame::new(to_bgra(img.into_rgba8()))]),
        w,
        h,
    ))
}

/// `formatBytes`: binary units, two decimals with trailing zeros dropped.
pub fn format_bytes(n: u64) -> String {
    format_bytes_with(n as f64, 2, false)
}

/// A rate as the transfer line shows it: padded, so it doesn't jitter.
fn format_bytes_rate(bps: f64) -> String {
    format_bytes_with(bps, if bps >= 1048576. { 2 } else { 0 }, true)
}

pub fn format_bytes_with(n: f64, decimals: usize, pad: bool) -> String {
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
    let s = if !pad && s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s
    };
    format!("{s} {}", UNITS[i])
}

thread_local! {
    static SHARED: std::rc::Rc<std::cell::RefCell<Media>> = std::rc::Rc::default();
}

/// The window's one media cache, for renderers that aren't handed it.
pub fn shared() -> std::rc::Rc<std::cell::RefCell<Media>> {
    SHARED.with(|m| m.clone())
}

#[cfg(test)]
mod tests {
    use image::codecs::gif::GifEncoder;
    use image::{Delay, Frame, RgbaImage};

    #[test]
    fn past_the_budget_the_longest_undrawn_go_and_the_recent_stay() {
        use std::time::{Duration, Instant};
        let mut media = super::Media::default();
        // 64 MB apiece: eight is twice the budget.
        let picture = || std::sync::Arc::new(gpui::RenderImage::new(vec![Frame::new(RgbaImage::new(4096, 4096))]));
        let now = Instant::now();
        for i in 0..8u64 {
            let url = format!("e{i}");
            media.custom.insert(url.clone(), super::Custom::Ready(picture()));
            media.used.insert((3, url), now - Duration::from_secs(60 - i * 7));
        }
        media.custom.insert("loading".into(), super::Custom::Pending);
        media.used.insert((3, "loading".into()), now - Duration::from_secs(600));
        let gone = media.sweep(Duration::from_secs(90));
        assert_eq!(gone.len(), 4);
        let kept: Vec<_> = (0..8).filter(|i| media.custom.contains_key(&format!("e{i}"))).collect();
        assert_eq!(kept, [4, 5, 6, 7]);
        assert!(media.decoded_bytes() <= super::DECODED_BUDGET);
        assert!(matches!(media.custom.get("loading"), Some(super::Custom::Pending)));

        // Drawn moments ago: kept over the budget.
        for i in 8..16u64 {
            let url = format!("e{i}");
            media.custom.insert(url.clone(), super::Custom::Ready(picture()));
            media.used.insert((3, url), Instant::now());
        }
        assert!(media.sweep(Duration::from_secs(90)).len() == 4);
        assert!((8..16).all(|i| media.custom.contains_key(&format!("e{i}"))));
    }

    #[test]
    fn an_animation_is_sniffed_whatever_its_name() {
        let mut gif = Vec::new();
        {
            let mut enc = GifEncoder::new(&mut gif);
            for v in [0u8, 255] {
                let buf = RgbaImage::from_pixel(4, 4, image::Rgba([v, v, v, 255]));
                enc.encode_frame(Frame::from_parts(buf, 0, 0, Delay::from_numer_denom_ms(100, 1))).unwrap();
            }
        }
        let (img, w, h) = super::decode(&gif, "", (64, 64)).unwrap();
        assert_eq!((w, h), (4, 4));
        assert_eq!(img.frame_count(), 2);
        let mut png = Vec::new();
        RgbaImage::new(4, 4).write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
        assert_eq!(super::decode(&png, "png", (64, 64)).unwrap().0.frame_count(), 1);
    }
}

#[cfg(test)]
mod apng_tests {
    use super::*;

    /// A 4×1 APNG: red everywhere, then green over the left half disposed back to what was
    /// there, then blue over the right half.
    fn sample() -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, 4, 1);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            enc.set_animated(3, 0).unwrap();
            let mut w = enc.write_header().unwrap();
            w.write_image_data(&[255, 0, 0, 255].repeat(4)).unwrap();
            w.set_frame_dimension(2, 1).unwrap();
            w.set_frame_position(0, 0).unwrap();
            w.set_dispose_op(png::DisposeOp::Previous).unwrap();
            w.write_image_data(&[0, 255, 0, 255].repeat(2)).unwrap();
            w.set_frame_position(2, 0).unwrap();
            w.set_dispose_op(png::DisposeOp::None).unwrap();
            w.write_image_data(&[0, 0, 255, 255].repeat(2)).unwrap();
            w.finish().unwrap();
        }
        out
    }

    fn row(f: &Frame) -> Vec<char> {
        f.buffer()
            .pixels()
            .map(|p| match (p[0], p[1], p[2]) {
                (255, 0, 0) => 'R',
                (0, 255, 0) => 'G',
                (0, 0, 255) => 'B',
                _ => '?',
            })
            .collect()
    }

    #[test]
    fn dispose_to_previous_puts_the_region_back() {
        let (frames, w, h) = apng_frames(&sample()).unwrap();
        assert_eq!((w, h, frames.len()), (4, 1, 3));
        assert_eq!(row(&frames[0]), "RRRR".chars().collect::<Vec<_>>());
        assert_eq!(row(&frames[1]), "GGRR".chars().collect::<Vec<_>>());
        assert_eq!(row(&frames[2]), "RRBB".chars().collect::<Vec<_>>());
    }
}

#[cfg(test)]
mod budget_tests {
    use super::*;

    #[test]
    fn an_over_budget_animation_shrinks_before_it_drops_frames() {
        // 137 frames at 360×213 come to 42 MB: shrunk to fit, every frame kept.
        let (w, h) = fit_budget((360, 213), 137);
        assert!(w as usize * h as usize * 4 * 137 <= ANIMATION_BUDGET);
        assert!(w as f64 >= 360. * MIN_SHRINK && h as f64 >= 213. * MIN_SHRINK);
        // One that fits is left alone.
        assert_eq!(fit_budget((128, 128), 30), (128, 128));
        // One far over shrinks no further than the floor; dropping frames does the rest.
        assert_eq!(fit_budget((1000, 1000), 500), (750, 750));
    }
}

#[cfg(test)]
mod jpeg_tests {
    use super::*;
    use image::codecs::jpeg::JpegEncoder;
    use image::{ImageEncoder, RgbImage, RgbaImage};

    /// A smooth photo-like JPEG, with an EXIF orientation when given.
    fn jpeg(w: u32, h: u32, orientation: Option<u8>) -> Vec<u8> {
        let img = RgbImage::from_fn(w, h, |x, y| {
            image::Rgb([(x * 255 / w) as u8, (y * 255 / h) as u8, ((x + y) * 127 / (w + h)) as u8])
        });
        let mut out = Vec::new();
        let mut enc = JpegEncoder::new_with_quality(&mut out, 90);
        if let Some(o) = orientation {
            let exif = [
                b'M', b'M', 0, 42, 0, 0, 0, 8, 0, 1, 0x01, 0x12, 0, 3, 0, 0, 0, 1, 0, o, 0, 0, 0, 0, 0, 0,
            ];
            enc.set_exif_metadata(exif.to_vec()).unwrap();
        }
        enc.write_image(img.as_raw(), w, h, image::ExtendedColorType::Rgb8).unwrap();
        out
    }

    /// What `decode` gave before it decoded JPEGs scaled: the full picture, then resized.
    fn full(bytes: &[u8], max: (u32, u32)) -> (RgbaImage, u32, u32) {
        let mut dec = ImageReader::new(Cursor::new(bytes)).with_guessed_format().unwrap().into_decoder().unwrap();
        let o = dec.orientation().unwrap();
        let mut img = DynamicImage::from_decoder(dec).unwrap();
        img.apply_orientation(o);
        let (w, h) = (img.width(), img.height());
        (img.resize(max.0, max.1, FilterType::CatmullRom).into_rgba8(), w, h)
    }

    fn mean_diff(a: &[u8], b: &[u8]) -> f64 {
        a.iter().zip(b).map(|(x, y)| (*x as i32 - *y as i32).unsigned_abs() as f64).sum::<f64>() / a.len() as f64
    }

    #[test]
    fn a_jpeg_decodes_scaled_to_the_same_size_and_look() {
        for (w, h, o, max) in [(1600, 1200, None, (400, 400)), (1600, 1200, Some(6), (300, 500)), (999, 333, Some(3), (120, 90))] {
            let bytes = jpeg(w, h, o);
            let (want, fw, fh) = full(&bytes, max);
            let (got, gw, gh) = decode(&bytes, "jpg", max).unwrap();
            assert_eq!((gw, gh), (fw, fh), "natural size");
            let size = got.size(0);
            assert_eq!((size.width.0 as u32, size.height.0 as u32), want.dimensions(), "{w}x{h} {o:?}");
            let mut want = want.into_raw();
            for p in want.chunks_exact_mut(4) {
                p.swap(0, 2);
            }
            let d = mean_diff(got.as_bytes(0).unwrap(), &want);
            assert!(d < 3., "{w}x{h} {o:?}: mean difference {d}");
        }
    }

    #[test]
    fn a_fitted_jpeg_comes_out_at_the_fitted_size() {
        let bytes = jpeg(1600, 1200, Some(6));
        // Decoded at a quarter, never the full 1600 by 1200, and turned upright.
        let (img, w, h) = gpui::decode_jpeg_scaled(&bytes, |_, _| (300, 400)).unwrap();
        assert_eq!((img.dimensions(), w, h), ((300, 400), 1200, 1600));
        // Upright it's 1200 by 1600.
        let f = gpui::jpeg_fitted(&bytes, 100, false).unwrap();
        assert_eq!(f.buffer().dimensions(), (75, 100));
        let f = gpui::jpeg_fitted(&bytes, 64, true).unwrap();
        assert_eq!(f.buffer().dimensions(), (64, 64));
        // Small enough already: the plain decode stands.
        assert!(gpui::jpeg_fitted(&jpeg(80, 60, None), 100, false).is_none());
    }
}

/// Decoded size of every frame of `image`.
fn image_bytes(image: &RenderImage) -> usize {
    (0..image.frame_count())
        .map(|i| {
            let s = image.size(i);
            s.width.0.max(0) as usize * s.height.0.max(0) as usize * 4
        })
        .sum()
}
