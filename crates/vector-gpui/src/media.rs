//! What the timeline draws attachments from: thumbhash blurs, pictures decoded once at the size
//! they draw, and each download's progress. Decoding runs off the UI thread; a picture larger
//! than its box is resampled to the box's device pixels, so the GPU never holds a full photo.

use std::collections::{HashMap, HashSet};
use std::io::Cursor;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{App, RenderImage, WeakEntity};
use image::{
    DynamicImage, Frame, ImageDecoder, ImageReader, imageops::FilterType,
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

/// Decoded pictures and resting clips' frames held past which the costliest off screen go at
/// once, before their idle time.
const DECODED_BUDGET: usize = 64 << 20;
/// Asked for this recently: spared, it may be on its way to the screen.
const GRACE: std::time::Duration = std::time::Duration::from_millis(1500);
/// Each half-life off screen doubles what an entry's bytes weigh against the budget.
const HALF_LIFE: f64 = 10.;
/// The size kept for the sweep's whole idle time; four times larger keeps half as long.
const IDLE_AT: usize = 1 << 20;
/// A decode whose row went this long undrawn while other rows drew (scrolled past) is skipped.
const STALE: std::time::Duration = std::time::Duration::from_secs(1);
/// How long a stage or a publish holds the screen before it earns words: the common, quick
/// transfer goes by without a flash of text.
const GLANCE: std::time::Duration = std::time::Duration::from_secs(1);

/// `STAGE_WORDS`.
fn stage_word(stage: &str) -> String {
    use crate::t;
    match stage {
        "encrypting" => t::file_stage_encrypting().into(),
        "starting" => t::file_stage_starting().into(),
        "decrypting" => t::file_stage_decrypting().into(),
        "verifying" => t::file_stage_verifying().into(),
        "saving" => t::file_stage_saving().into(),
        other => other.to_string(),
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
            return crate::t::file_stage_sending().into();
        }
        let Some(stage) = self.stage.as_deref().filter(|_| self.stage_at.is_some_and(|t| t.elapsed() >= GLANCE)) else {
            return String::new();
        };
        let word = stage_word(stage);
        match self.stage_pct.filter(|&p| p < 100.) {
            Some(p) => crate::t::file_stage_stage_percent(p as u32, &*word).into(),
            None => word,
        }
    }

    /// `transferStageText`, else the rate.
    pub fn note(&self) -> String {
        if let Some(s) = &self.stage {
            return stage_word(s);
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
/// The longest side of one drawn at text size: 40 points at 2x.
const EMOJI_SMALL: u32 = 80;
/// What marks a text-size emoji's cache key.
const SMALL: &str = "\u{0}small";

/// The box a linked picture is decoded to fit, in device pixels: its row draws it no larger than
/// a wide bubble by 400 points at 2x. The viewer reads its own full-size copy.
const LINKED_MAX: (u32, u32) = (1600, 800);

struct Want {
    key: String,
    hash: Option<[u8; 32]>,
    path: Option<PathBuf>,
    ext: String,
    max: (u32, u32),
    still: bool,
}

#[derive(Default)]
pub struct Media {
    /// Audio attachments and the engine sources behind them.
    pub audio: crate::audio::Audio,
    /// Animations too long to hold, playing from their bytes, by picture key.
    streams: HashMap<String, Stream>,
    /// Each stream's picture as the screen shows it, swapped without a redraw.
    stream_live: HashMap<String, gpui::LiveImage>,
    /// Frames a stream stepped past, for the window to let go of.
    pub stream_retired: Vec<Arc<RenderImage>>,
    pub videos: crate::video::Videos,
    pub models: crate::model3d::stage::Models,
    thumbs: HashMap<String, Option<Arc<RenderImage>>>,
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
    /// Attachments that came without a size and asked their server for it, once a session.
    pub sized: HashSet<String>,
    /// Size asks the renderer leaves for the app: (msg id, attachment id).
    pub size_queue: Vec<(String, String)>,
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
    /// Invite and Nostr cards seen resolving, so only a fresh resolve pops.
    pub settle: crate::kit::Settle,
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
    /// The message each embedded clip was last played from, which the floating player returns to.
    pub embed_clip_msgs: HashMap<String, String>,
    /// First-frame openings for videos with no poster that won't download on their own: None
    /// while fetching or when there is none.
    pub embed_previews: HashMap<String, Option<crate::audio::AudioInputSpec>>,
    pub embed_preview_queue: Vec<crate::embeds::Media>,
    /// Realtime sessions, by topic (or `att:<id>` for an app without one).
    pub xdc_live: HashMap<String, XdcLive>,
    /// When each decoded picture was last drawn, so one off screen for long is let go.
    used: HashMap<(u8, String), std::time::Instant>,
    /// A decode landed since the last sweep.
    pressed: bool,
    /// Attachments whose bytes couldn't be read or decoded, by how often they were asked again.
    retried: HashMap<String, u8>,
    /// When a row last drew an attachment's or a link's picture.
    asked: Option<std::time::Instant>,
}

/// A Mini App session as its card reports it.
#[derive(Clone, Default)]
pub struct XdcLive {
    pub active: bool,
    pub peers: Vec<String>,
    /// The latest count reported, which a neighbour update sends without names.
    pub count: usize,
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
        self.resume_progress(id, size).map(|p| crate::t::file_resume_note(p).into())
    }

    /// How much of a paused download is held: a percentage, or bytes when the size is unknown.
    pub fn resume_progress(&self, id: &str, size: u64) -> Option<String> {
        let &(done, total) = self.paused.get(id)?;
        let total = total.or((size > 0).then_some(size)).filter(|&t| t > 0);
        Some(match total {
            Some(t) => format!("{}%", (done.saturating_mul(100) / t).min(99)),
            None => format_bytes(done),
        })
    }

    /// A thumbhash's blur at its own ~32 px, for the GPU to stretch: draw it with
    /// `ObjectFit::Fill`. Its tile is edge padded, so the sampler never reads a neighbour.
    pub fn thumb(&mut self, hash: &str) -> Option<Arc<RenderImage>> {
        if hash.is_empty() {
            return None;
        }
        self.used.insert((0, hash.to_string()), std::time::Instant::now());
        self.thumbs
            .entry(hash.to_string())
            .or_insert_with(|| decode_thumbhash(hash).map(Arc::new))
            .clone()
    }

    /// An attachment's picture; the first ask queues its decode at `max` device pixels.
    pub fn image(&mut self, key: &str, source: Source, ext: &str, max: (u32, u32)) -> Slot {
        self.image_as(key, source, ext, max, false)
    }

    /// Pictures cached under `prefix` marked long unused, so the next sweep frees them.
    pub fn let_go(&mut self, prefix: &str) {
        let Some(then) = std::time::Instant::now().checked_sub(std::time::Duration::from_secs(24 * 3600)) else { return };
        for (key, at) in self.used.iter_mut() {
            if key.0 == 1 && key.1.starts_with(prefix) {
                *at = then;
            }
        }
    }

    /// Steps every stream on screen to its due frame; a stream not drawn for [`STREAM_UNSEEN`]
    /// while the window moves is let go. When the next frame is due.
    /// The live picture behind `key`, when it streams.
    pub fn live_of(&self, key: &str) -> Option<gpui::LiveImage> {
        self.stream_live.get(key).cloned()
    }

    /// Streams the presented frame shows count as seen, though nothing redrew them.
    pub fn mark_streams_seen(&mut self, window: &gpui::Window) {
        let now = std::time::Instant::now();
        for (key, s) in self.streams.iter_mut() {
            if self.stream_live.get(key).is_some_and(|l| window.live_shown(l)) {
                s.seen = now;
            }
        }
    }

    pub fn any_streams(&self) -> bool {
        !self.streams.is_empty()
    }

    pub fn tick_streams(&mut self, moving: bool) -> Option<std::time::Duration> {
        let now = std::time::Instant::now();
        let retired = &mut self.stream_retired;
        let mut next: Option<std::time::Duration> = None;
        let lives = &mut self.stream_live;
        self.streams.retain(|key, s| {
            if moving && now.saturating_duration_since(s.seen) > STREAM_UNSEEN {
                retired.push(s.current.clone());
                lives.remove(key);
                return false;
            }
            let before = s.current.clone();
            if let Some(d) = s.tick(now, moving, retired) {
                next = Some(next.map_or(d, |n| n.min(d)));
            }
            if !Arc::ptr_eq(&before, &s.current)
                && let Some(live) = lives.get(key)
            {
                *live.borrow_mut() = s.current.clone();
            }
            true
        });
        next
    }

    /// As `image`, an animation kept to its first frame.
    pub fn still(&mut self, key: &str, source: Source, ext: &str, max: (u32, u32)) -> Slot {
        self.image_as(key, source, ext, max, true)
    }

    fn image_as(&mut self, key: &str, source: Source, ext: &str, max: (u32, u32), still: bool) -> Slot {
        let now = std::time::Instant::now();
        self.asked = Some(now);
        self.used.insert((1, key.to_string()), now);
        if !still && let Some(s) = self.streams.get_mut(key) {
            s.seen = now;
            return Slot::Ready { image: s.current.clone(), w: s.w, h: s.h };
        }
        if let Some(s) = self.images.get(key) {
            if matches!(s, Slot::Loading) {
                gpui::keyed_volatile();
            }
            return s.clone();
        }
        gpui::keyed_volatile();
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
            still,
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

    /// The link pictures ready to draw, by URL.
    pub fn linked_ready(&self) -> HashSet<String> {
        self.linked.iter().filter(|(_, l)| matches!(l, Linked::Ready { .. })).map(|(u, _)| u.clone()).collect()
    }

    /// Pictures decoded ahead of the rows that show them.
    pub fn take_linked(&mut self, pics: Vec<(String, Linked)>) {
        let now = std::time::Instant::now();
        for (url, l) in pics {
            self.used.insert((2, url.clone()), now);
            self.linked.insert(url, l);
        }
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
        let now = std::time::Instant::now();
        self.asked = Some(now);
        self.used.insert((2, url.to_string()), now);
        if let Some(l) = self.linked.get(url) {
            if matches!(l, Linked::Loading(_)) {
                gpui::keyed_volatile();
            }
            return l.clone();
        }
        gpui::keyed_volatile();
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
            // The frame on screen; each stream holds up to AHEAD more in its ring.
            "streams": tally(self.streams.values().map(|s| &s.current)),
        })
    }

    /// Decoded pictures and resting clips off screen past their idle time, taken out of the
    /// caches for their atlas textures to be dropped; one drawn again later is decoded again.
    /// The larger an entry, the shorter its idle time. Past [`DECODED_BUDGET`] the costliest go
    /// too, cost being bytes doubled every [`HALF_LIFE`] off screen: a big picture scrolled past
    /// goes before a small one, and a recent one before an old one only when far larger.
    /// Nothing `shown` (in the frame on screen) or asked for within [`GRACE`] goes.
    pub fn sweep(&mut self, idle: std::time::Duration, shown: impl Fn(&RenderImage) -> bool) -> Vec<Arc<RenderImage>> {
        enum Held {
            Pic((u8, String)),
            Clip(String),
        }
        self.pressed = false;
        let now = std::time::Instant::now();
        let mut held = self.decoded_bytes() + self.videos.held();
        let mut stale: Vec<(Held, f64, usize)> = Vec::new();
        for (key, at) in &self.used {
            let age = now.saturating_duration_since(*at);
            if age > GRACE {
                let bytes = self.peek(key).map_or(0, |i| image_bytes(i));
                stale.push((Held::Pic(key.clone()), age.as_secs_f64(), bytes));
            }
        }
        for (att, at, bytes) in self.videos.at_rest() {
            let age = now.saturating_duration_since(at);
            if age > GRACE {
                stale.push((Held::Clip(att), age.as_secs_f64(), bytes));
            }
        }
        let cost = |(_, age, bytes): &(Held, f64, usize)| *bytes as f64 * (age / HALF_LIFE).exp2();
        stale.sort_unstable_by(|a, b| cost(b).total_cmp(&cost(a)));
        let mut out = Vec::new();
        for (held_by, age, bytes) in stale {
            if age <= idle_for(bytes, idle).as_secs_f64() && held <= DECODED_BUDGET {
                continue;
            }
            match held_by {
                Held::Pic(key) => {
                    if self.peek(&key).is_some_and(|i| shown(i)) {
                        continue;
                    }
                    let Some(image) = self.take(&key) else { continue };
                    self.used.remove(&key);
                    if let Some(image) = image {
                        held = held.saturating_sub(bytes);
                        out.push(image);
                    }
                }
                Held::Clip(att) => {
                    if self.videos.clip(&att).and_then(|c| c.picture()).is_some_and(|i| shown(&i)) {
                        continue;
                    }
                    self.videos.shed(&att);
                    held = held.saturating_sub(bytes);
                }
            }
        }
        out
    }

    /// A decode landed and the held bytes may be past the budget: the next frame sweeps.
    pub fn pressed(&mut self) -> bool {
        let landed = std::mem::take(&mut self.pressed) | self.videos.take_landed();
        landed && self.decoded_bytes() + self.videos.held() > DECODED_BUDGET
    }

    /// Whether rows have drawn since without `key`: scrolled past, its decode waits for the next
    /// sight. Time alone can't tell, a pane at rest drawing nothing.
    fn stale(&self, key: &(u8, String)) -> bool {
        self.used.get(key).is_none_or(|at| self.asked.is_some_and(|last| last.saturating_duration_since(*at) > STALE))
    }

    /// The cached picture under a `used` key, left in place.
    fn peek(&self, key: &(u8, String)) -> Option<&Arc<RenderImage>> {
        match key.0 {
            0 => self.thumbs.get(&key.1)?.as_ref(),
            1 => match self.images.get(&key.1)? {
                Slot::Ready { image, .. } => Some(image),
                _ => None,
            },
            2 => match self.linked.get(&key.1)? {
                Linked::Ready { image, .. } => Some(image),
                _ => None,
            },
            _ => match self.custom.get(&key.1)? {
                Custom::Ready(image) => Some(image),
                _ => None,
            },
        }
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
            0 => self.thumbs.remove(&key.1).flatten(),
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
            if matches!(c, Custom::Pending) {
                gpui::keyed_volatile();
            }
            return c.clone();
        }
        gpui::keyed_volatile();
        self.custom.insert(url.to_string(), Custom::Pending);
        self.custom_wants.push((url.to_string(), icon));
        Custom::Pending
    }

    /// A custom emoji drawn at text size (a line, a reaction, a picker cell): decoded to
    /// [`EMOJI_SMALL`], a sixteenth of the pixels, unless the full size is already held.
    pub fn custom_small(&mut self, url: &str) -> Custom {
        if let Some(c @ Custom::Ready(_)) = self.custom.get(url) {
            let c = c.clone();
            self.used.insert((3, url.to_string()), std::time::Instant::now());
            return c;
        }
        self.custom(&format!("{url}{SMALL}"), false) // i18n-exempt: cache key
    }

    /// Whether a custom emoji failed to load, at either size.
    pub fn custom_missing(&self, url: &str) -> bool {
        // i18n-exempt: cache key
        [url.to_string(), format!("{url}{SMALL}")].iter().any(|k| matches!(self.custom.get(k), Some(Custom::Missing(_))))
    }

    /// A picture fetched again: whatever its key failed to load before is forgotten.
    pub fn refetched(&mut self, key: &str) {
        if matches!(self.images.get(key), Some(Slot::Failed)) {
            self.images.remove(key);
        }
        self.retried.remove(key);
    }

    pub fn linked_progress(&mut self, url: &str, pct: f32) {
        if let Some(Linked::Loading(p)) = self.linked.get_mut(url) {
            *p = Some(pct);
        }
    }
}

/// How often a picture that failed to load is asked for again in a session.
const RETRIES: u8 = 2;

/// Pictures read and decoded at once: a fast scroll queues the rest, and those it has passed
/// by the time their turn comes are dropped unread.
const DECODE_SLOTS: usize = 4;

fn decode_slots() -> Arc<tokio::sync::Semaphore> {
    thread_local!(static SLOTS: Arc<tokio::sync::Semaphore> = Arc::new(tokio::sync::Semaphore::new(DECODE_SLOTS)));
    SLOTS.with(|s| s.clone())
}

/// Start the decodes the last render asked for; the app repaints as each lands.
fn land(m: &mut Media, key: String, decoded: Option<Picture>) {
    match decoded {
        Some(Picture::Held(image, iw, ih)) => {
            m.images.insert(key, Slot::Ready { image: Arc::new(image), w: iw, h: ih });
            m.pressed = true;
        }
        Some(Picture::Stream(stream)) => {
            m.images.remove(&key);
            m.stream_live.insert(key.clone(), Rc::new(std::cell::RefCell::new(stream.current.clone())));
            if let Some(old) = m.streams.insert(key, stream) {
                m.stream_retired.push(old.current.clone());
            }
            m.pressed = true;
        }
        // A held copy let go meanwhile: the next sight asks again, a couple of times.
        None if m.retried.get(&key).copied().unwrap_or(0) < RETRIES => {
            *m.retried.entry(key.clone()).or_default() += 1;
            m.images.remove(&key);
        }
        None => {
            m.images.insert(key, Slot::Failed);
        }
    }
}

/// `w` read and decoded off this thread.
fn read_now(w: &Want, live: bool) -> futures::channel::oneshot::Receiver<Option<Picture>> {
    let (ext, max, still) = (w.ext.clone(), w.max, w.still);
    if live {
        let (hash, path) = (w.hash, w.path.as_ref().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default());
        return crate::backend::run(async move {
            let b = crate::backend::attachments::bytes(hash, path).await?;
            tokio::task::spawn_blocking(move || decode_or_stream(Arc::new(b), &ext, max, still, max)).await.ok()?
        });
    }
    let (tx, rx) = futures::channel::oneshot::channel();
    let path = w.path.clone();
    // spawn-detached: reads and decodes a fixture file; owns no account state.
    std::thread::spawn(move || {
        let _ = tx.send(path.and_then(|p| crate::pic::read(p).ok()).and_then(|b| decode_or_stream(Arc::new(b), &ext, max, still, max)));
    });
    rx
}

pub fn pump<V: 'static>(
    media: &std::rc::Rc<std::cell::RefCell<Media>>,
    app: WeakEntity<V>,
    live: bool,
    cx: &mut App,
) {
    let mut wants = std::mem::take(&mut media.borrow_mut().wants);
    // A held frame shows the pictures it asked for: read and decoded at once, waited on together.
    if cx.frame_hold_deadline().is_some() {
        for w in wants.drain(..) {
            let mut rx = read_now(&w, live);
            let (media, app) = (media.clone(), app.clone());
            cx.wait_in_hold(move |cx, deadline| {
                if let Some(decoded) = crate::kit::recv_until(&mut rx, deadline) {
                    land(&mut media.borrow_mut(), w.key, decoded);
                    if let Some(app) = app.upgrade() {
                        cx.notify(app.entity_id());
                    }
                    return true;
                }
                cx.spawn(async move |cx| {
                    let decoded = rx.await.ok().flatten();
                    land(&mut media.borrow_mut(), w.key, decoded);
                    app.update(cx, |_, cx| cx.notify()).ok();
                })
                .detach();
                false
            });
        }
    }
    for w in wants {
        let media = media.clone();
        let app = app.clone();
        cx.spawn(async move |cx| {
            let _slot = decode_slots().acquire_owned().await;
            // Scrolled past while it queued: nothing read, the next sight asks again.
            if media.borrow().stale(&(1, w.key.clone())) {
                media.borrow_mut().images.remove(&w.key);
                app.update(cx, |_, cx| cx.notify()).ok();
                return;
            }
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
            // Scrolled past while its bytes were read: the next sight asks again.
            if media.borrow().stale(&(1, w.key.clone())) {
                media.borrow_mut().images.remove(&w.key);
                app.update(cx, |_, cx| cx.notify()).ok();
                return;
            }
            let (ext, max, still) = (w.ext.clone(), w.max, w.still);
            let decoded = match bytes {
                Some(b) => {
                    cx.background_executor()
                        .spawn(async move { decode_or_stream(Arc::new(b), &ext, max, still, max) })
                        .await
                }
                None => None,
            };
            land(&mut media.borrow_mut(), w.key, decoded);
            app.update(cx, |_, cx| cx.notify()).ok();
        })
        .detach();
    }
    let custom = std::mem::take(&mut media.borrow_mut().custom_wants);
    for (url, icon) in custom {
        let media = media.clone();
        let app = app.clone();
        let (src, max) = match url.strip_suffix(SMALL) {
            Some(src) => (src.to_string(), EMOJI_SMALL),
            None => (url.clone(), EMOJI_MAX),
        };
        let rx =
            live.then(|| crate::backend::run(crate::backend::images::emoji(src.clone(), icon)));
        cx.spawn(async move |cx| {
            let res = match rx {
                Some(rx) => rx.await.unwrap_or_else(|_| Err(crate::t::emoji_why_cancelled().into())),
                None if std::path::Path::new(&src).is_file() => Ok(src.clone()),
                None => Err(crate::t::emoji_why_not_found().into()),
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
                                .spawn(async move { decode(&b, &ext, (max, max)) })
                                .await
                                .ok_or_else(|| crate::t::emoji_why_invalid().to_string())
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
            {
                let mut m = media.borrow_mut();
                m.custom.insert(url, state);
                m.pressed = true;
            }
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
            let _slot = decode_slots().acquire_owned().await;
            if bytes.is_some() && media.borrow().stale(&(2, url.clone())) {
                media.borrow_mut().linked.remove(&url);
                app.update(cx, |_, cx| cx.notify()).ok();
                return;
            }
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
                // Decoded ahead meanwhile: that copy may already be on the GPU.
                if !matches!(m.linked.get(&url), Some(Linked::Ready { .. })) {
                    m.linked.insert(url.clone(), slot);
                }
                m.linked_settled.push(url);
                m.pressed = true;
            }
            app.update(cx, |_, cx| cx.notify()).ok();
        })
        .detach();
    }
}

/// The link card pictures and icons of the newest `messages` (newest first) that this device already
/// holds and `ready` lacks, decoded: a page lands with them drawn instead of reflowing a frame on.
pub async fn held_linked(messages: &[vector_core::types::Message], ready: &HashSet<String>) -> Vec<(String, Linked)> {
    const NEWEST: usize = 30;
    const MOST: usize = 8;
    let mut urls: Vec<String> = Vec::new();
    'scan: for m in messages.iter().take(NEWEST) {
        let Some(meta) = m.preview_metadata.as_ref() else { continue };
        for url in [&meta.og_image, &meta.favicon].into_iter().flatten() {
            if !ready.contains(url) && !urls.contains(url) {
                urls.push(url.clone());
            }
            if urls.len() == MOST {
                break 'scan;
            }
        }
    }
    let jobs = urls.into_iter().map(|url| async move {
        let bytes = crate::backend::inline::held_image(&url).await?;
        let ext = crate::backend::inline::sniff(&bytes).unwrap_or("png");
        let (image, w, h) = tokio::task::spawn_blocking(move || decode(&bytes, ext, LINKED_MAX)).await.ok()??;
        Some((url, Linked::Ready { image: Arc::new(image), w, h }))
    });
    futures::future::join_all(jobs).await.into_iter().flatten().collect()
}

pub fn to_bgra(mut buf: image::RgbaImage) -> image::RgbaImage {
    swap_red_blue(&mut buf);
    buf
}

/// RGBA <-> BGRA in place. Whole pixels as masked `u32`s, which the compiler vectorises: ~3x the
/// per-byte swap.
pub fn swap_red_blue(bytes: &mut [u8]) {
    // SAFETY: any bit pattern is a valid u32.
    let (head, words, tail) = unsafe { bytes.align_to_mut::<u32>() };
    if !head.is_empty() {
        // Pixels straddle the words; image buffers never start here.
        for px in bytes.chunks_exact_mut(4) {
            px.swap(0, 2);
        }
        return;
    }
    for w in words {
        let v = u32::from_le(*w);
        *w = ((v & 0xFF00_FF00) | ((v >> 16) & 0xFF) | ((v & 0xFF) << 16)).to_le();
    }
    for px in tail.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
}

/// A thumbhash's blur at the size it encodes (at most 32 px a side).
pub fn decode_thumbhash(hash: &str) -> Option<RenderImage> {
    let bytes = fast_thumbhash::base91_decode(hash).ok()?;
    // 64 px: smooth under the GPU's stretch, where 32 px shows bilinear creases.
    let (w, h, bgra) = crate::thumbhash::decode_bgra(&bytes, 64)?;
    let buf = image::RgbaImage::from_raw(w as u32, h as u32, bgra)?;
    Some(RenderImage::new(vec![Frame::new(buf)]))
}

/// An animation's frames (GIF, animated WebP or APNG, sniffed from the bytes, not the name),
/// each scaled to `to` when that is smaller. `None` for anything that doesn't animate.
/// The most pixels an animation's canvas may hold: a header claims its size, and a few bytes
/// can claim 65535 × 65535. Larger ones fall back to the still decode and its allocation limit.
const MAX_CANVAS: u64 = 1 << 25;

fn canvas_fits(w: u32, h: u32) -> bool {
    u64::from(w) * u64::from(h) <= MAX_CANVAS
}

fn gif_frames(bytes: &[u8], to: impl Fn(u32, u32) -> (u32, u32)) -> Option<(Vec<Frame>, u32, u32)> {
    if bytes.starts_with(b"GIF8") {
        let gif = GifCanvas::new(bytes)?;
        let (w, h) = (gif.canvas.width(), gif.canvas.height());
        return frames_of(gif, w, h, &to);
    }
    if bytes.len() > 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        // An animation's VP8X chunk names the canvas, which libwebp allocates on opening.
        if bytes.len() >= 30 && &bytes[12..16] == b"VP8X" {
            let side = |b: &[u8]| 1 + (u32::from(b[0]) | u32::from(b[1]) << 8 | u32::from(b[2]) << 16);
            if !canvas_fits(side(&bytes[24..27]), side(&bytes[27..30])) {
                return None;
            }
        }
        // libwebp composites the frames: the image crate's compositor keeps parts of earlier
        // frames a later one should have cleared, which show as trails.
        let dec = crate::shared::animated_webp::Decoder::new(bytes).ok()?;
        if dec.frame_count() <= 1 {
            return None;
        }
        let (w, h) = dec.dimensions();
        let frames = dec.map(|f| {
            f.map(|(rgba, ms)| Frame::from_parts(rgba, 0, 0, image::Delay::from_numer_denom_ms(ms, 1)))
                .map_err(|e| image::ImageError::IoError(std::io::Error::other(e)))
        });
        return frames_of(Decoded::new(image::Frames::new(Box::new(frames))), w, h, &to);
    }
    // An APNG declares itself with `acTL` before its first `IDAT`.
    if bytes.starts_with(b"\x89PNG") {
        let (w, h, _) = ApngCanvas::shape(bytes)?;
        return frames_of(ApngCanvas::new(bytes)?, w, h, &to);
    }
    None
}

/// An APNG composited on its canvas by the spec, a frame at a time: each region blended (source
/// or over) at its offset, then disposed (kept, cleared, or put back as it was). The image
/// crate's own compositing gets "dispose to previous" wrong, leaving earlier frames behind.
struct ApngCanvas<'a> {
    reader: png::Reader<Cursor<&'a [u8]>>,
    canvas: image::RgbaImage,
    /// The frame just reached, drawn over the canvas before its disposal.
    shown: image::RgbaImage,
    buf: Vec<u8>,
    /// The default image is a frame only when its own fcTL comes before it.
    default_shown: bool,
    first: bool,
    left: usize,
}

impl<'a> ApngCanvas<'a> {
    fn new(bytes: &'a [u8]) -> Option<Self> {
        let mut dec = png::Decoder::new(Cursor::new(bytes));
        dec.set_transformations(png::Transformations::normalize_to_color8());
        let reader = dec.read_info().ok()?;
        let (w, h) = (reader.info().width, reader.info().height);
        if !canvas_fits(w, h) {
            return None;
        }
        let left = reader.info().animation_control?.num_frames as usize;
        let default_shown = reader.info().frame_control.is_some();
        let buf = vec![0u8; reader.output_buffer_size()?];
        Some(Self { reader, canvas: image::RgbaImage::new(w, h), shown: image::RgbaImage::new(w, h), buf, default_shown, first: true, left })
    }

    /// The canvas size and how many frames the file says it has.
    fn shape(bytes: &[u8]) -> Option<(u32, u32, usize)> {
        let head = &bytes[..bytes.len().min(64 * 1024)];
        let idat = head.windows(4).position(|w| w == b"IDAT").unwrap_or(head.len());
        if !head[..idat].windows(4).any(|w| w == b"acTL") {
            return None;
        }
        let a = ApngCanvas::new(bytes)?;
        Some((a.canvas.width(), a.canvas.height(), a.left))
    }
}

impl FrameSource for ApngCanvas<'_> {
    fn advance(&mut self) -> Option<u32> {
        use png::{BlendOp, DisposeOp};
        if self.left == 0 {
            return None;
        }
        loop {
            let out = self.reader.next_frame(&mut self.buf).ok()?;
            if self.first && !self.default_shown {
                self.first = false;
                continue;
            }
            let fc = self.reader.info().frame_control?;
            let (color, _) = self.reader.output_color_type();
            let rgba = to_rgba(&self.buf[..out.buffer_size()], color, out.width, out.height)?;
            let (fx, fy) = (fc.x_offset, fc.y_offset);
            let (w, h) = self.canvas.dimensions();
            if fx.saturating_add(out.width) > w || fy.saturating_add(out.height) > h {
                return None;
            }
            let dispose = if self.first && fc.dispose_op == DisposeOp::Previous { DisposeOp::Background } else { fc.dispose_op };
            self.first = false;
            let saved = (dispose == DisposeOp::Previous).then(|| image::imageops::crop_imm(&self.canvas, fx, fy, out.width, out.height).to_image());
            for (x, y, src) in rgba.enumerate_pixels() {
                let dst = self.canvas.get_pixel_mut(fx + x, fy + y);
                *dst = match fc.blend_op {
                    BlendOp::Source => *src,
                    BlendOp::Over => over(*src, *dst),
                };
            }
            self.shown.copy_from_slice(&self.canvas);
            match dispose {
                DisposeOp::None => {}
                DisposeOp::Background => {
                    for y in fy..fy + out.height {
                        for x in fx..fx + out.width {
                            self.canvas.put_pixel(x, y, image::Rgba([0, 0, 0, 0]));
                        }
                    }
                }
                DisposeOp::Previous => {
                    if let Some(saved) = saved {
                        image::imageops::replace(&mut self.canvas, &saved, fx as i64, fy as i64);
                    }
                }
            }
            self.left -= 1;
            let den = if fc.delay_den == 0 { 100 } else { u32::from(fc.delay_den) };
            return Some(u32::from(fc.delay_num) * 1000 / den);
        }
    }

    fn take(&mut self, to: Option<(u32, u32)>) -> image::RgbaImage {
        match to {
            // `shown` is drawn afresh from the canvas each frame, so it may be scaled in place.
            Some((w, h)) => gpui::resize_premultiplied_from(&mut self.shown, w, h),
            None => self.shown.clone(),
        }
    }
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


/// Decoded bytes one held animation may hold at its drawn size: past it, it streams. A caller
/// that asks for every frame held keeps every other frame (twice as long each) until it fits.
const ANIMATION_BUDGET: usize = 24 << 20;

/// What a picture decodes to: every frame held, or an animation too long to hold at the size it
/// draws, played from its bytes a few frames ahead.
pub enum Picture {
    Held(RenderImage, u32, u32),
    Stream(Stream),
}

/// `decode_as`, streaming a GIF, animated WebP, APNG or HEIF sequence whose frames at `max` outgrow
/// [`ANIMATION_BUDGET`], at `stream_max`: never shrunk below the drawn size, never a frame dropped.
pub fn decode_or_stream(bytes: Arc<Vec<u8>>, ext: &str, max: (u32, u32), still: bool, stream_max: (u32, u32)) -> Option<Picture> {
    if ext != "svg"
        && !still
        && let Some((w, h, count)) = streamable(&bytes)
    {
        let to = within_max((w, h), max);
        if to.0 as usize * to.1 as usize * 4 * count > ANIMATION_BUDGET {
            if let Some(s) = Stream::start(bytes.clone(), (w, h), within_max((w, h), stream_max)) {
                return Some(Picture::Stream(s));
            }
        }
    }
    decode_as(&bytes, ext, max, still).map(|(i, w, h)| Picture::Held(i, w, h))
}

/// `(w, h)` shrunk to fit `max`, never enlarged.
fn within_max((w, h): (u32, u32), max: (u32, u32)) -> (u32, u32) {
    let s = (max.0 as f32 / w.max(1) as f32).min(max.1 as f32 / h.max(1) as f32).min(1.);
    (((w as f32 * s).round() as u32).max(1), ((h as f32 * s).round() as u32).max(1))
}

/// A GIF or animated WebP's canvas and about how many frames it has.
fn streamable(bytes: &Arc<Vec<u8>>) -> Option<(u32, u32, usize)> {
    if bytes.starts_with(b"GIF8") {
        let gif = GifCanvas::new(bytes)?;
        // Each frame's Graphic Control Extension: a count close enough to size the budget by.
        let count = bytes.windows(3).filter(|w| *w == [0x21, 0xF9, 0x04]).count();
        return (count > 1).then(|| (gif.canvas.width(), gif.canvas.height(), count));
    }
    if bytes.len() > 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        let dec = crate::shared::animated_webp::Decoder::new(bytes).ok()?;
        let (w, h) = dec.dimensions();
        let count = dec.frame_count() as usize;
        return (count > 1 && canvas_fits(w, h)).then_some((w, h, count));
    }
    if bytes.starts_with(b"\x89PNG") {
        return ApngCanvas::shape(bytes).filter(|&(_, _, n)| n > 1);
    }
    #[cfg(feature = "video")]
    if crate::heif::sniff(bytes) && crate::heif::has_track(bytes) {
        let (_, w, h, count) = crate::video::Sequence::open(Shared(bytes.clone()))?;
        return Some((w, h, count));
    }
    None
}

/// Bytes a decoder reads without a copy of its own.
struct Shared(Arc<Vec<u8>>);

impl AsRef<[u8]> for Shared {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

fn frame_source(bytes: &Arc<Vec<u8>>) -> Option<Box<dyn FrameSource + '_>> {
    if bytes.starts_with(b"GIF8") {
        return Some(Box::new(GifCanvas::new(bytes)?));
    }
    if bytes.starts_with(b"\x89PNG") {
        return Some(Box::new(ApngCanvas::new(bytes)?));
    }
    #[cfg(feature = "video")]
    if crate::heif::sniff(bytes) {
        return Some(Box::new(crate::video::Sequence::open(Shared(bytes.clone()))?.0));
    }
    let dec = crate::shared::animated_webp::Decoder::new(bytes).ok()?;
    let frames = dec.map(|f| {
        f.map(|(rgba, ms)| Frame::from_parts(rgba, 0, 0, image::Delay::from_numer_denom_ms(ms, 1)))
            .map_err(|e| image::ImageError::IoError(std::io::Error::other(e)))
    });
    Some(Box::new(Decoded::new(image::Frames::new(Box::new(frames)))))
}

/// Browsers play a delay of 10 ms or less as 100 ms.
fn browser_delay(v: u32) -> u32 {
    if v <= 10 { 100 } else { v }
}

/// Frames decoded ahead of the one shown: enough to ride out a slow frame, little to hold.
const AHEAD: usize = 2;
/// Not drawn for this long while the window moves: off screen, so it is let go, ring, decoder
/// and texture, and starts again from its bytes when it comes back.
const STREAM_UNSEEN: std::time::Duration = std::time::Duration::from_millis(1000);
/// Fallen this far behind its clock: it carries on from now rather than racing to catch up.
const STREAM_LATE: std::time::Duration = std::time::Duration::from_millis(250);

/// An animation played from its bytes: a decoder thread keeps [`AHEAD`] frames at the drawn
/// size ready, and each due frame replaces the one shown.
pub struct Stream {
    frames: std::sync::mpsc::Receiver<(Arc<RenderImage>, u32)>,
    pub current: Arc<RenderImage>,
    pub w: u32,
    pub h: u32,
    delay: u32,
    due: Option<std::time::Instant>,
    pub seen: std::time::Instant,
}

impl Stream {
    /// Started on `bytes`, its first frame decoded before it returns.
    pub fn start(bytes: Arc<Vec<u8>>, natural: (u32, u32), to: (u32, u32)) -> Option<Self> {
        let (tx, rx) = std::sync::mpsc::sync_channel(AHEAD);
        // spawn-detached: decodes frames from bytes in hand; ends when the stream is dropped.
        std::thread::Builder::new()
            .name("anim-stream".into())
            .spawn(move || stream_frames(&bytes, natural, to, &tx))
            .ok()?;
        let (current, delay) = rx.recv().ok()?;
        Some(Self { frames: rx, current, w: natural.0, h: natural.1, delay, due: None, seen: std::time::Instant::now() })
    }

    /// Steps to the frame now due, while it may move; how long until the next is due.
    pub fn tick(&mut self, now: std::time::Instant, moving: bool, retired: &mut Vec<Arc<RenderImage>>) -> Option<std::time::Duration> {
        if !moving {
            self.due = None;
            return None;
        }
        let due = *self.due.get_or_insert(now + std::time::Duration::from_millis(self.delay.into()));
        if now < due {
            return Some(due - now);
        }
        match self.frames.try_recv() {
            Ok((frame, delay)) => {
                retired.push(std::mem::replace(&mut self.current, frame));
                self.delay = delay;
                let from = if now - due > STREAM_LATE { now } else { due };
                let next = from + std::time::Duration::from_millis(delay.into());
                self.due = Some(next);
                Some(next.saturating_duration_since(now))
            }
            // The decoder is a frame behind: look again shortly.
            Err(std::sync::mpsc::TryRecvError::Empty) => Some(std::time::Duration::from_millis(4)),
            Err(std::sync::mpsc::TryRecvError::Disconnected) => None,
        }
    }
}

/// Every frame of `bytes` at `to`, over and over, until the stream lets go.
fn stream_frames(bytes: &Arc<Vec<u8>>, natural: (u32, u32), to: (u32, u32), tx: &std::sync::mpsc::SyncSender<(Arc<RenderImage>, u32)>) {
    let scale = (to != natural).then_some(to);
    loop {
        let Some(mut source) = frame_source(bytes) else { return };
        let mut any = false;
        // A damaged frame ends the pass there, as browsers play what decodes.
        while let Some(delay) = source.advance() {
            let delay = browser_delay(delay);
            let buf = source.take(scale);
            let buf = if source.bgra() { buf } else { to_bgra(buf) };
            let frame = RenderImage::new(vec![Frame::from_parts(buf, 0, 0, image::Delay::from_numer_denom_ms(delay, 1))]);
            if tx.send((Arc::new(frame), delay)).is_err() {
                return;
            }
            any = true;
        }
        if !any {
            return;
        }
    }
}

/// An animation one frame at a time: its delay first, its pixels only for a frame that is kept.
trait FrameSource {
    /// The next frame's delay in ms; None at the end, or at a frame that won't decode.
    fn advance(&mut self) -> Option<u32>;
    /// The frame `advance` reached, scaled to `to` when given.
    fn take(&mut self, to: Option<(u32, u32)>) -> image::RgbaImage;
    /// Whether `take` hands over BGRA already.
    fn bgra(&self) -> bool {
        false
    }
}

#[cfg(feature = "video")]
impl FrameSource for crate::video::Sequence {
    fn advance(&mut self) -> Option<u32> {
        crate::video::Sequence::advance(self)
    }

    fn take(&mut self, to: Option<(u32, u32)>) -> image::RgbaImage {
        crate::video::Sequence::take(self, to)
    }

    fn bgra(&self) -> bool {
        true
    }
}

/// Frames a decoder hands over whole.
struct Decoded<'a> {
    frames: image::Frames<'a>,
    at: Option<image::RgbaImage>,
}

impl<'a> Decoded<'a> {
    fn new(frames: image::Frames<'a>) -> Self {
        Self { frames, at: None }
    }
}

impl FrameSource for Decoded<'_> {
    fn advance(&mut self) -> Option<u32> {
        let f = self.frames.next()?.ok()?;
        let (n, d) = f.delay().numer_denom_ms();
        self.at = Some(f.into_buffer());
        Some(n / d.max(1))
    }

    fn take(&mut self, to: Option<(u32, u32)>) -> image::RgbaImage {
        let buf = self.at.take().unwrap_or_default();
        match to {
            Some((w, h)) => gpui::resize_premultiplied(buf, w, h),
            None => buf,
        }
    }
}

/// A GIF composited on one canvas the whole way through, as the image crate composites it, but
/// with no buffer per frame: a long GIF scrolled past decoded hundreds of megabytes of frames
/// only to keep a few.
struct GifCanvas<'a> {
    reader: gif::Decoder<Cursor<&'a [u8]>>,
    /// What the next frame draws over: every frame so far, each disposed as it asked.
    canvas: image::RgbaImage,
    /// The frame just reached, drawn over the canvas.
    shown: image::RgbaImage,
    raw: Vec<u8>,
}

impl<'a> GifCanvas<'a> {
    fn new(bytes: &'a [u8]) -> Option<Self> {
        let mut opts = gif::DecodeOptions::new();
        opts.set_color_output(gif::ColorOutput::RGBA);
        let reader = opts.read_info(Cursor::new(bytes)).ok()?;
        let (w, h) = (u32::from(reader.width()), u32::from(reader.height()));
        if !canvas_fits(w, h) {
            return None;
        }
        Some(Self { reader, canvas: image::RgbaImage::new(w, h), shown: image::RgbaImage::new(w, h), raw: Vec::new() })
    }
}

impl FrameSource for GifCanvas<'_> {
    fn advance(&mut self) -> Option<u32> {
        let f = self.reader.next_frame_info().ok()??;
        let (left, top, fw, fh) = (f.left as usize, f.top as usize, f.width as usize, f.height as usize);
        let (dispose, delay) = (f.dispose, u32::from(f.delay) * 10);
        // A frame's own header claims its size; the buffer is sized before the crate checks it.
        if !canvas_fits(fw as u32, fh as u32) {
            return None;
        }
        self.raw.resize(self.reader.buffer_size(), 0);
        self.reader.read_into_buffer(&mut self.raw).ok()?;
        let cw = self.canvas.width() as usize;
        let ch = self.canvas.height() as usize;
        self.shown.copy_from_slice(&self.canvas);
        // The part of the frame on the canvas: a frame may overhang it.
        let (xs, ys) = (fw.min(cw.saturating_sub(left)), fh.min(ch.saturating_sub(top)));
        for y in 0..if xs == 0 { 0 } else { ys } {
            let src = &self.raw[y * fw * 4..][..xs * 4];
            let at = ((top + y) * cw + left) * 4;
            let shown = &mut (*self.shown)[at..at + xs * 4];
            let canvas = &mut (*self.canvas)[at..at + xs * 4];
            for ((s, o), c) in src.chunks_exact(4).zip(shown.chunks_exact_mut(4)).zip(canvas.chunks_exact_mut(4)) {
                if s[3] != 0 {
                    o.copy_from_slice(s);
                }
                match dispose {
                    gif::DisposalMethod::Any | gif::DisposalMethod::Keep => c.copy_from_slice(o),
                    gif::DisposalMethod::Background => c.fill(0),
                    gif::DisposalMethod::Previous => {}
                }
            }
        }
        Some(delay)
    }

    fn take(&mut self, to: Option<(u32, u32)>) -> image::RgbaImage {
        match to {
            // `shown` is drawn afresh from the canvas each frame, so it may be scaled in place.
            Some((w, h)) => gpui::resize_premultiplied_from(&mut self.shown, w, h),
            None => self.shown.clone(),
        }
    }
}

fn frames_of(
    mut frames: impl FrameSource,
    w: u32,
    h: u32,
    to: &impl Fn(u32, u32) -> (u32, u32),
) -> Option<(Vec<Frame>, u32, u32)> {
    let bgra = frames.bgra();
    let (tw, th) = to(w, h);
    let scale = (tw < w || th < h).then_some((tw.max(1), th.max(1)));
    // Browsers play 10ms or less as 100ms.
    let ms = |v: u32| match v {
        0..=10 => 100,
        v => v,
    };
    let mut kept: Vec<(image::RgbaImage, u32)> = Vec::new();
    let mut bytes = 0usize;
    let mut stride = 1usize;
    let mut i = 0usize;
    // A damaged frame ends the animation there, as browsers play what decodes.
    while let Some(delay) = frames.advance() {
        let delay = ms(delay);
        let skip = i % stride != 0;
        i += 1;
        if skip {
            if let Some(last) = kept.last_mut() {
                last.1 += delay;
            }
            continue;
        }
        let buf = frames.take(scale);
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
        .map(|(buf, delay)| {
            let buf = if bgra { buf } else { to_bgra(buf) };
            Frame::from_parts(buf, 0, 0, image::Delay::from_numer_denom_ms(delay, 1))
        })
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
    decode_as(bytes, ext, max, false)
}

/// `decode`, or with `still` an animation's first frame alone.
pub fn decode_as(bytes: &[u8], ext: &str, max: (u32, u32), still: bool) -> Option<(RenderImage, u32, u32)> {
    let _trim = crate::trim::OnDrop;
    if ext != "svg"
        && crate::heif::sniff(bytes)
        && let Some(d) = heif(bytes, max, still)
    {
        return Some(d);
    }
    if ext != "svg"
        && !still
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
        let need = |w: u32, h: u32| {
            if w <= max.0 && h <= max.1 {
                return Some(((w, h), None));
            }
            // As `DynamicImage::resize` sizes it, so the result matches the full decode's.
            let r = (max.0.max(1) as f64 / w as f64).min(max.1.max(1) as f64 / h as f64);
            let t = (((w as f64 * r).round() as u32).max(1), ((h as f64 * r).round() as u32).max(1));
            Some((t, Some(t)))
        };
        if let Some((img, w, h)) = gpui::jpeg_into(bytes, need, |_, _, w, h| (0, 0, w, h), FilterType::CatmullRom, true) {
            return Some((RenderImage::new(vec![Frame::new(img)]), w, h));
        }
    }
    if bytes.starts_with(b"\x89PNG")
        && let Some((img, w, h)) = png_into(bytes, max)
    {
        return Some((RenderImage::new(vec![Frame::new(img)]), w, h));
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
        // `from_decoder` skips the allocation limit `ImageReader::decode` checks.
        image::Limits::default().reserve(dec.total_bytes()).ok()?;
        let orientation = dec.orientation().ok();
        let mut img = DynamicImage::from_decoder(dec).ok()?;
        if let Some(o) = orientation {
            img.apply_orientation(o);
        }
        img
    };
    let (w, h) = (img.width(), img.height());
    let img = if w > max.0 || h > max.1 {
        gpui::resample_within(&img, max.0.max(1), max.1.max(1), FilterType::CatmullRom)
    } else {
        img
    };
    Some((
        RenderImage::new(vec![Frame::new(to_bgra(img.into_rgba8()))]),
        w,
        h,
    ))
}

/// A still PNG decoded a row at a time into BGRA, resized as the general decode resizes it where
/// it passes `max`: no full-size copy is made. None for what the image crate decodes instead:
/// an interlaced, 16-bit or turned file, or one past its allocation limit.
fn png_into(bytes: &[u8], max: (u32, u32)) -> Option<(image::RgbaImage, u32, u32)> {
    let mut dec = png::Decoder::new(Cursor::new(bytes));
    dec.set_transformations(png::Transformations::EXPAND);
    let mut reader = dec.read_info().ok()?;
    let info = reader.info();
    let (w, h) = (info.width, info.height);
    if info.interlaced || info.bit_depth == png::BitDepth::Sixteen || u64::from(w) * u64::from(h) * 4 > 512 << 20 {
        return None;
    }
    let turned = info.exif_metadata.as_deref().and_then(image::metadata::Orientation::from_exif_chunk);
    if turned.is_some_and(|o| o != image::metadata::Orientation::NoTransforms) {
        return None;
    }
    let (tw, th) = if w > max.0 || h > max.1 {
        let (mw, mh) = (max.0.max(1), max.1.max(1));
        let r = (mw as f64 / w as f64).min(mh as f64 / h as f64);
        let fit = |len: u32| ((len as f64 * r).round() as u64).clamp(1, u32::MAX as u64) as u32;
        (fit(w), fit(h))
    } else {
        (w, h)
    };
    let mut out = vec![0u8; tw as usize * th as usize * 4];
    let line = tw as usize * 4;
    let c = match reader.output_color_type().0 {
        png::ColorType::Grayscale => 1,
        png::ColorType::GrayscaleAlpha => 2,
        png::ColorType::Rgb => 3,
        png::ColorType::Rgba => 4,
        png::ColorType::Indexed => return None,
    };
    let put = |src: &[u8], out: &mut [u8]| {
        for (p, o) in src.chunks_exact(c).zip(out.chunks_exact_mut(4)) {
            match c {
                1 => o.copy_from_slice(&[p[0], p[0], p[0], 255]),
                2 => o.copy_from_slice(&[p[0], p[0], p[0], p[1]]),
                3 => o.copy_from_slice(&[p[2], p[1], p[0], 255]),
                _ => o.copy_from_slice(&[p[2], p[1], p[0], p[3]]),
            }
        }
    };
    if (tw, th) == (w, h) {
        let mut y = 0;
        while let Some(row) = reader.next_row().ok()? {
            put(&row.data()[..w as usize * c], &mut out[y * line..][..line]);
            y += 1;
        }
        (y == h as usize).then_some(())?;
    } else {
        fn feed<const C: usize>(
            reader: &mut png::Reader<Cursor<&[u8]>>,
            (w, h, tw, th): (u32, u32, u32, u32),
            mut emit: impl FnMut(usize, &[u8]),
        ) -> Option<()> {
            let mut rows = gpui::Rows::<C>::new(w, h, tw, th, FilterType::CatmullRom)?;
            let mut y = 0;
            while let Some(row) = reader.next_row().ok()? {
                rows.push(y, row.data(), &mut emit);
                y += 1;
            }
            (y == h as usize).then_some(())
        }
        let sizes = (w, h, tw, th);
        let mut emit = |r: usize, o: &[u8]| put(o, &mut out[r * line..][..line]);
        match c {
            1 => feed::<1>(&mut reader, sizes, &mut emit),
            2 => feed::<2>(&mut reader, sizes, &mut emit),
            3 => feed::<3>(&mut reader, sizes, &mut emit),
            _ => feed::<4>(&mut reader, sizes, &mut emit),
        }?;
    }
    Some((image::RgbaImage::from_raw(tw, th, out)?, w, h))
}

/// A HEIF file: a sequence or burst plays as an animation, else its primary picture shows.
fn heif(bytes: &[u8], max: (u32, u32), still: bool) -> Option<(RenderImage, u32, u32)> {
    #[cfg(not(feature = "video"))]
    let _ = still;
    #[cfg(feature = "video")]
    if !still
        && crate::heif::has_track(bytes)
        && let Some((seq, w, h, _)) = crate::video::Sequence::open(bytes.to_vec())
        && let Some((frames, w, h)) = frames_of(seq, w, h, &|w, h| crate::heif::fit((w, h), max))
        && frames.len() > 1
    {
        return Some((RenderImage::new(frames), w, h));
    }
    let (img, w, h) = crate::heif::decode(bytes, max)?;
    Some((RenderImage::new(vec![Frame::new(img)]), w, h))
}

/// Someone else's picture, within limits: a small file can claim a huge canvas.
pub fn decode_bounded(bytes: &[u8]) -> Option<DynamicImage> {
    let mut reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format().ok()?;
    reader.limits(vector_core::crypto::bounded_image_limits());
    reader.decode().ok()
}

/// [`decode_bounded`] as RGBA no larger than `max_side`.
pub fn decode_rgba(bytes: &[u8], max_side: u32) -> Option<image::RgbaImage> {
    let img = decode_bounded(bytes)?;
    let img = if img.width() > max_side || img.height() > max_side {
        gpui::resample_within(&img, max_side, max_side, FilterType::Triangle)
    } else {
        img
    };
    Some(img.into_rgba8())
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
    vector_i18n::locale().bytes(n, decimals, pad)
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

    use std::time::{Duration, Instant};

    const IDLE: Duration = Duration::from_secs(90);

    /// A picture of `mb` megabytes, held as a custom emoji drawn `ago`.
    fn held(media: &mut super::Media, name: &str, mb: u32, ago: Duration) -> std::sync::Arc<gpui::RenderImage> {
        let side = (mb as f64 * 262144.).sqrt() as u32;
        let image = std::sync::Arc::new(gpui::RenderImage::new(vec![Frame::new(RgbaImage::new(side, side))]));
        media.custom.insert(name.into(), super::Custom::Ready(image.clone()));
        media.used.insert((3, name.into()), Instant::now() - ago);
        image
    }

    fn kept(media: &super::Media, names: &[&str]) -> Vec<String> {
        names.iter().filter(|n| media.custom.contains_key(**n)).map(|n| n.to_string()).collect()
    }

    #[test]
    fn animations_claiming_huge_canvases_decode_to_nothing() {
        let mut gif = Vec::new();
        {
            let mut enc = gif::Encoder::new(&mut gif, u16::MAX, u16::MAX, &[]).unwrap();
            for _ in 0..2 {
                enc.write_frame(&gif::Frame::from_rgba(1, 1, &mut [255, 0, 0, 255])).unwrap();
            }
        }
        assert!(super::decode(&gif, "gif", (64, 64)).is_none());
        // A small canvas whose frame claims to be huge.
        let mut gif = Vec::new();
        {
            let mut enc = gif::Encoder::new(&mut gif, 2, 2, &[]).unwrap();
            for _ in 0..2 {
                enc.write_frame(&gif::Frame::from_rgba(1, 1, &mut [255, 0, 0, 255])).unwrap();
            }
        }
        let at = gif.windows(9).position(|w| w == b"\x2c\0\0\0\0\x01\0\x01\0").unwrap();
        gif[at + 5..at + 9].fill(0xff);
        let _ = super::decode(&gif, "gif", (64, 64));
        let mut webp = b"RIFF\x16\0\0\0WEBPVP8X\x0a\0\0\0\x02\0\0\0".to_vec();
        webp.extend([0xff, 0x3f, 0, 0xff, 0x3f, 0]);
        assert!(super::decode(&webp, "webp", (64, 64)).is_none());
    }

    /// Animated pictures as other people send them: every format's own compositing code, fed
    /// valid files with random bytes changed. `PICFUZZ_ITERS` and `PICFUZZ_SEED` run more.
    #[test]
    fn no_damaged_animation_panics_the_decoder() {
        let mut gif = Vec::new();
        {
            let mut enc = gif::Encoder::new(&mut gif, 12, 10, &[]).unwrap();
            for i in 0..3u16 {
                let mut f = gif::Frame::from_rgba_speed(6, 5, &mut vec![(i * 60) as u8; 6 * 5 * 4], 30);
                f.left = i * 2;
                f.top = i;
                f.dispose = [gif::DisposalMethod::Keep, gif::DisposalMethod::Background, gif::DisposalMethod::Previous][i as usize];
                f.delay = 5;
                enc.write_frame(&f).unwrap();
            }
        }
        let mut apng = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut apng, 12, 10);
            enc.set_color(png::ColorType::Rgba);
            enc.set_animated(3, 0).unwrap();
            let mut w = enc.write_header().unwrap();
            for i in 0..3u8 {
                if i > 0 {
                    w.set_frame_dimension(6, 5).unwrap();
                    w.set_frame_position(u32::from(i) * 2, u32::from(i)).unwrap();
                    w.set_dispose_op([png::DisposeOp::None, png::DisposeOp::Background, png::DisposeOp::Previous][i as usize]).unwrap();
                    w.set_blend_op([png::BlendOp::Source, png::BlendOp::Over][i as usize % 2]).unwrap();
                }
                let (fw, fh) = if i == 0 { (12, 10) } else { (6, 5) };
                w.write_image_data(&vec![i * 80; fw * fh * 4]).unwrap();
            }
        }
        let webp = {
            let mut enc = crate::shared::animated_webp::Encoder::new(12, 10, 80.).unwrap();
            for i in 0..3u8 {
                enc.add(&vec![i * 80; 12 * 10 * 4], 50).unwrap();
            }
            enc.finish().unwrap()
        };
        let iters: usize = std::env::var("PICFUZZ_ITERS").ok().and_then(|v| v.parse().ok()).unwrap_or(200);
        let mut seed: u64 = std::env::var("PICFUZZ_SEED").ok().and_then(|v| v.parse().ok()).unwrap_or(0x2545_f491_4f6c_dd1d);
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let mut failed = Vec::new();
        for i in 0..iters {
            let mut b = [&gif, &apng, &webp][i % 3].clone();
            for _ in 0..1 + next() % 6 {
                let at = (next() % b.len() as u64) as usize;
                match next() % 5 {
                    0 => b[at] = next() as u8,
                    1 => b[at] = 0xff,
                    2 => b[at] = 0,
                    3 => b.truncate(at.max(8)),
                    _ => b[at] ^= 1 << (next() % 8),
                }
            }
            if std::panic::catch_unwind(|| super::decode(&b, "", (64, 64))).is_err() {
                failed.push(b);
                if failed.len() >= 3 {
                    break;
                }
            }
        }
        assert!(failed.is_empty(), "files that panicked: {failed:?}");
    }

    /// Still pictures as other people send them, in every format the viewer opens.
    /// `PICFUZZ_DIR` adds sample files as seeds; `PICFUZZ_OUT` keeps what failed.
    #[test]
    fn no_damaged_still_panics_the_decoder() {
        let (w, h) = (20u32, 14u32);
        let px: Vec<u8> = (0..w * h).flat_map(|i| [(i * 7) as u8, (i * 3) as u8, (i / 2) as u8, 200]).collect();
        let img = image::DynamicImage::ImageRgba8(RgbaImage::from_raw(w, h, px.clone()).unwrap());
        let encoded = |f: image::ImageFormat, img: &image::DynamicImage| {
            let mut out = std::io::Cursor::new(Vec::new());
            img.write_to(&mut out, f).unwrap();
            out.into_inner()
        };
        let rgb = image::DynamicImage::ImageRgb8(img.to_rgb8());
        let mut seeds: Vec<(Vec<u8>, String)> = vec![
            (encoded(image::ImageFormat::Png, &img), "png".into()),
            (encoded(image::ImageFormat::Jpeg, &rgb), "jpg".into()),
            (encoded(image::ImageFormat::Bmp, &img), "bmp".into()),
            (encoded(image::ImageFormat::Tiff, &img), "tiff".into()),
            (encoded(image::ImageFormat::Ico, &img), "ico".into()),
            (encoded(image::ImageFormat::WebP, &img), "webp".into()),
            (crate::jxl::lossless(&px, w, h, true).unwrap(), "jxl".into()),
            (
                br##"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="14"><defs><linearGradient id="g"><stop offset="0" stop-color="#f00"/></linearGradient></defs><rect width="20" height="14" fill="url(#g)"/><text x="1" y="9">Hi</text></svg>"##.to_vec(),
                "svg".into(),
            ),
        ];
        if let Ok(dir) = std::env::var("PICFUZZ_DIR") {
            for e in std::fs::read_dir(dir).unwrap().flatten() {
                let ext = e.path().extension().and_then(|x| x.to_str()).unwrap_or_default().to_ascii_lowercase();
                seeds.push((std::fs::read(e.path()).unwrap(), ext));
            }
        }
        let iters: usize = std::env::var("PICFUZZ_ITERS").ok().and_then(|v| v.parse().ok()).unwrap_or(160);
        let mut seed: u64 = std::env::var("PICFUZZ_SEED").ok().and_then(|v| v.parse().ok()).unwrap_or(0x1234_5678_9abc_def1);
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let mut failed = Vec::new();
        for i in 0..iters {
            let (mut b, ext) = seeds[i % seeds.len()].clone();
            for _ in 0..1 + next() % 6 {
                let at = (next() % b.len() as u64) as usize;
                match next() % 6 {
                    0 => b[at] = next() as u8,
                    1 => b[at] = 0xff,
                    2 => b[at] = 0,
                    3 => b.truncate(at.max(8)),
                    4 => b[at..].iter_mut().take(4).for_each(|x| *x = 0xff),
                    _ => b[at] ^= 1 << (next() % 8),
                }
            }
            let max = if i % 2 == 0 { (8, 8) } else { (64, 64) };
            let t = Instant::now();
            if std::panic::catch_unwind(|| super::decode(&b, &ext, max)).is_err() || t.elapsed() > Duration::from_secs(5) {
                eprintln!("case {i} ({ext}, {} bytes)", b.len());
                failed.push((ext, b));
                if failed.len() >= 3 {
                    break;
                }
            }
        }
        if let Ok(out) = std::env::var("PICFUZZ_OUT") {
            for (i, (ext, b)) in failed.iter().enumerate() {
                std::fs::write(format!("{out}/bad-{i}.{ext}"), b).unwrap();
            }
        }
        assert!(failed.is_empty(), "{} damaged pictures panicked or stalled", failed.len());
    }

    #[test]
    fn pictures_drawn_from_disk_refuse_huge_canvases() {
        let open = |format, bytes: Vec<u8>| {
            gpui::Image::from_bytes(format, bytes).to_image_data(gpui::SvgRenderer::new(std::sync::Arc::new(())))
        };
        let mut gif = Vec::new();
        {
            let mut enc = gif::Encoder::new(&mut gif, 2, 2, &[]).unwrap();
            enc.write_frame(&gif::Frame::from_rgba_speed(2, 2, &mut vec![200; 16], 30)).unwrap();
        }
        assert!(open(gpui::ImageFormat::Gif, gif.clone()).is_ok());
        gif[6..10].fill(0xff);
        assert!(open(gpui::ImageFormat::Gif, gif).is_err(), "a 65535×65535 canvas");
        let png = |w, h| {
            let mut out = std::io::Cursor::new(Vec::new());
            image::DynamicImage::ImageLuma8(image::GrayImage::new(w, h)).write_to(&mut out, image::ImageFormat::Png).unwrap();
            out.into_inner()
        };
        assert!(open(gpui::ImageFormat::Png, png(64, 64)).is_ok());
        assert!(open(gpui::ImageFormat::Png, png(16_385, 1)).is_err());
        assert!(open(gpui::ImageFormat::Png, png(9_000, 8_000)).is_err(), "72 megapixels, 288 MB as RGBA");
    }

    #[test]
    fn a_gif_composites_as_the_image_crate_does() {
        use super::FrameSource as _;
        use gif::DisposalMethod::*;
        use image::AnimationDecoder as _;
        let (w, h) = (24u16, 16u16);
        let mut bytes = Vec::new();
        {
            let mut enc = gif::Encoder::new(&mut bytes, w, h, &[]).unwrap();
            for (i, dispose) in [Keep, Background, Previous, Any, Keep, Previous, Background, Keep, Any].into_iter().enumerate() {
                let (fw, fh) = (8 + i as u16 % 3 * 4, 6 + i as u16 % 2 * 5);
                let mut rgba = vec![0u8; fw as usize * fh as usize * 4];
                for (j, px) in rgba.chunks_exact_mut(4).enumerate() {
                    if (i + j) % 5 != 0 {
                        px.copy_from_slice(&[(i * 30) as u8, (j * 7) as u8, (255 - i * 20) as u8, 255]);
                    }
                }
                let mut f = gif::Frame::from_rgba_speed(fw, fh, &mut rgba, 30);
                f.left = (i as u16 * 3) % (w - fw + 1);
                f.top = (i as u16 * 2) % (h - fh + 1);
                f.dispose = dispose;
                f.delay = i as u16;
                enc.write_frame(&f).unwrap();
            }
        }
        let expect: Vec<_> = image::codecs::gif::GifDecoder::new(std::io::Cursor::new(&bytes)).unwrap().into_frames().map(Result::unwrap).collect();
        assert_eq!(expect.len(), 9);
        let mut gif = super::GifCanvas::new(&bytes).unwrap();
        for (i, e) in expect.iter().enumerate() {
            let (n, d) = e.delay().numer_denom_ms();
            assert_eq!(gif.advance(), Some(n / d), "frame {i}");
            assert_eq!(&gif.take(None), e.buffer(), "frame {i}");
        }
        assert_eq!(gif.advance(), None);
    }

    #[test]
    fn a_big_picture_off_screen_goes_sooner_than_a_small_one() {
        let mut media = super::Media::default();
        held(&mut media, "small", 1, Duration::from_secs(80));
        held(&mut media, "mid", 4, Duration::from_secs(40));
        held(&mut media, "big", 16, Duration::from_secs(30));
        assert_eq!(media.sweep(IDLE, |_| false).len(), 1);
        assert_eq!(kept(&media, &["small", "mid", "big"]), ["small", "mid"]);
        assert_eq!(super::idle_for(1 << 20, IDLE), IDLE);
        assert_eq!(super::idle_for(4 << 20, IDLE), IDLE / 2);
        assert_eq!(super::idle_for(1 << 30, IDLE), IDLE / 6);
    }

    #[test]
    fn past_the_budget_the_costliest_go_first_and_no_more_than_needed() {
        let mut media = super::Media::default();
        let big: Vec<_> = (2..6).map(|s| format!("big{s}")).collect();
        for (i, name) in big.iter().enumerate() {
            held(&mut media, name, 32, Duration::from_secs(2 + i as u64));
        }
        let small: Vec<_> = (0..20).map(|i| format!("small{i}")).collect();
        for (i, name) in small.iter().enumerate() {
            held(&mut media, name, 1, Duration::from_secs(20 + i as u64));
        }
        assert!(media.decoded_bytes() > super::DECODED_BUDGET);
        media.custom.insert("loading".into(), super::Custom::Pending);
        media.used.insert((3, "loading".into()), Instant::now() - Duration::from_secs(600));
        assert_eq!(media.sweep(IDLE, |_| false).len(), 3);
        // A 32 MB picture off screen 3 s outweighs a 1 MB one off screen 39 s.
        assert_eq!(kept(&media, &["big2", "big3", "big4", "big5"]), ["big2"]);
        assert!(small.iter().all(|n| media.custom.contains_key(n)));
        assert!(media.decoded_bytes() <= super::DECODED_BUDGET);
        assert!(matches!(media.custom.get("loading"), Some(super::Custom::Pending)));
    }

    #[test]
    fn what_is_on_screen_or_just_asked_for_stays_over_the_budget() {
        let mut media = super::Media::default();
        let shown = held(&mut media, "shown", 64, Duration::from_secs(300));
        held(&mut media, "asked", 64, Duration::from_millis(200));
        held(&mut media, "gone", 64, Duration::from_secs(4));
        let gone = media.sweep(IDLE, |i| i.id == shown.id);
        assert_eq!(gone.len(), 1);
        assert_eq!(kept(&media, &["shown", "asked", "gone"]), ["shown", "asked"]);
        assert!(!media.pressed(), "a sweep settles the pressure it was called for");
    }

    #[test]
    fn a_text_size_emoji_reuses_a_full_one_held_and_else_asks_for_its_own() {
        let mut media = super::Media::default();
        let full = held(&mut media, "https://e/a.png", 1, Duration::ZERO);
        assert!(matches!(media.custom_small("https://e/a.png"), super::Custom::Ready(i) if i.id == full.id));
        assert!(matches!(media.custom_small("https://e/b.png"), super::Custom::Pending));
        assert_eq!(media.custom_wants, [(format!("https://e/b.png{}", super::SMALL), false)]);
        media.custom.insert(format!("https://e/b.png{}", super::SMALL), super::Custom::Missing("gone".into()));
        assert!(media.custom_missing("https://e/b.png"));
        assert!(!media.custom_missing("https://e/a.png"));
    }

    #[test]
    fn a_decode_scrolled_past_is_stale_but_one_on_a_resting_pane_is_not() {
        let mut media = super::Media::default();
        let key = (1u8, "a".to_string());
        media.used.insert(key.clone(), Instant::now() - Duration::from_secs(30));
        media.asked = Some(Instant::now() - Duration::from_secs(30));
        assert!(!media.stale(&key));
        media.asked = Some(Instant::now());
        assert!(media.stale(&key));
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
    pub(super) fn sample() -> Vec<u8> {
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
        let bytes = sample();
        let mut a = ApngCanvas::new(&bytes).unwrap();
        let (w, h) = a.canvas.dimensions();
        let mut frames = Vec::new();
        while let Some(d) = a.advance() {
            frames.push(Frame::from_parts(a.take(None), 0, 0, image::Delay::from_numer_denom_ms(d, 1)));
        }
        assert_eq!((w, h, frames.len()), (4, 1, 3));
        assert_eq!(row(&frames[0]), "RRRR".chars().collect::<Vec<_>>());
        assert_eq!(row(&frames[1]), "GGRR".chars().collect::<Vec<_>>());
        assert_eq!(row(&frames[2]), "RRBB".chars().collect::<Vec<_>>());
    }
}

#[cfg(test)]
mod budget_tests {
    use super::*;

    /// `count` frames of `w`×`h`, each `delay_cs` hundredths of a second.
    fn gif(w: u16, h: u16, count: usize, delay_cs: u16) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut enc = gif::Encoder::new(&mut out, w, h, &[0, 0, 0, 255, 255, 255]).unwrap();
            enc.set_repeat(gif::Repeat::Infinite).unwrap();
            for i in 0..count {
                let buffer: Vec<u8> = (0..w as usize * h as usize).map(|p| ((p + i) % 7 == 0) as u8).collect();
                let frame = gif::Frame { width: w, height: h, delay: delay_cs, buffer: buffer.into(), ..Default::default() };
                enc.write_frame(&frame).unwrap();
            }
        }
        out
    }

    #[test]
    fn an_animation_that_fits_is_held_whole_at_its_drawn_size() {
        let bytes = gif(200, 150, 300, 4);
        let Some(Picture::Held(image, w, h)) = decode_or_stream(Arc::new(bytes), "gif", (100, 100), false, (100, 100)) else {
            panic!("a 9 MB animation is held");
        };
        assert_eq!((w, h), (200, 150));
        assert_eq!(image.frame_count(), 300, "no frame dropped");
        assert_eq!((image.size(0).width.0, image.size(0).height.0), (100, 75), "decoded at the drawn size");
    }

    #[test]
    fn an_over_budget_animation_streams_at_its_drawn_size_and_native_delays() {
        let bytes = gif(200, 150, 300, 4);
        let Some(Picture::Stream(mut s)) = decode_or_stream(Arc::new(bytes), "gif", (400, 400), false, (400, 400)) else {
            panic!("36 MB of frames at the drawn size stream");
        };
        assert_eq!((s.w, s.h), (200, 150));
        assert_eq!((s.current.size(0).width.0, s.current.size(0).height.0), (200, 150), "never enlarged, never shrunk");
        let mut retired = Vec::new();
        let mut now = std::time::Instant::now();
        assert_eq!(s.tick(now, true, &mut retired), Some(std::time::Duration::from_millis(40)));
        for k in 1..=20 {
            now += std::time::Duration::from_millis(40);
            loop {
                match s.tick(now, true, &mut retired) {
                    Some(d) if retired.len() == k => {
                        assert_eq!(d, std::time::Duration::from_millis(40), "the GIF's own pace");
                        break;
                    }
                    // The decoder is a frame behind: it asks again shortly.
                    Some(d) => std::thread::sleep(d),
                    None => panic!("the stream ended"),
                }
            }
        }
        assert_eq!(retired.len(), 20, "one frame per 40 ms, none skipped");
        assert_eq!(s.tick(now, false, &mut retired), None, "a still window steps nothing");
    }

    #[test]
    fn an_over_budget_apng_streams_too() {
        let (w, h, count) = (200u32, 150u32, 300u32);
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, w, h);
            enc.set_color(png::ColorType::Rgba);
            enc.set_animated(count, 0).unwrap();
            enc.set_frame_delay(1, 25).unwrap();
            let mut wr = enc.write_header().unwrap();
            for i in 0..count {
                let px: Vec<u8> = (0..w * h).flat_map(|p| [((p + i) % 251) as u8, 0, 0, 255]).collect();
                wr.write_image_data(&px).unwrap();
            }
            wr.finish().unwrap();
        }
        let Some(Picture::Stream(s)) = decode_or_stream(Arc::new(out), "png", (400, 400), false, (400, 400)) else {
            panic!("36 MB of APNG frames at the drawn size stream");
        };
        assert_eq!((s.w, s.h), (200, 150));
        assert_eq!(s.delay, 40, "its own 1/25 s");
    }

    /// Steps `s` through `n` frames at their own pace, returning each frame's delay.
    fn play(s: &mut Stream, n: usize) -> Vec<u32> {
        let mut retired = Vec::new();
        let mut now = std::time::Instant::now();
        let mut delays = vec![s.delay];
        s.tick(now, true, &mut retired);
        while retired.len() < n {
            now += std::time::Duration::from_millis(s.delay.into());
            let before = retired.len();
            while retired.len() == before {
                if s.tick(now, true, &mut retired) == Some(std::time::Duration::from_millis(4)) {
                    std::thread::sleep(std::time::Duration::from_millis(4));
                }
            }
            delays.push(s.delay);
        }
        delays
    }

    #[cfg(feature = "video")]
    #[test]
    fn a_heif_sequence_streams_every_frame_at_its_rate() {
        let bytes = std::fs::read(format!("{}/reference/media/sequence.heics", env!("CARGO_MANIFEST_DIR"))).unwrap();
        let mut s = Stream::start(Arc::new(bytes), (64, 48), (64, 48)).expect("a sequence streams");
        assert_eq!((s.current.size(0).width.0, s.current.size(0).height.0), (64, 48));
        // Two passes: the end starts it over, every frame 100 ms.
        assert_eq!(play(&mut s, 12), vec![100; 13]);
    }

    #[test]
    fn a_streamed_apng_draws_what_the_held_one_does() {
        let bytes = Arc::new(apng_tests::sample());
        let mut s = Stream::start(bytes.clone(), (4, 1), (4, 1)).expect("an APNG streams");
        let mut held = ApngCanvas::new(&bytes).unwrap();
        for _ in 0..3 {
            held.advance().unwrap();
            let want = to_bgra(held.take(None));
            let mut retired = Vec::new();
            assert_eq!(s.current.as_bytes(0).unwrap(), &*want);
            let now = std::time::Instant::now() + std::time::Duration::from_secs(60);
            s.due = Some(now - std::time::Duration::from_millis(1));
            while retired.is_empty() {
                s.tick(now, true, &mut retired);
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
    }
}

#[cfg(test)]
mod jpeg_tests {
    use super::*;
    use image::codecs::jpeg::JpegEncoder;
    use image::{ImageEncoder, RgbImage, RgbaImage};

    /// A smooth photo-like JPEG, with an EXIF orientation when given.
    fn jpeg(w: u32, h: u32, orientation: Option<u8>) -> Vec<u8> {
        encoded(w, h, orientation, false)
    }

    fn encoded(w: u32, h: u32, orientation: Option<u8>, grey: bool) -> Vec<u8> {
        let img = RgbImage::from_fn(w, h, |x, y| {
            image::Rgb([(x * 255 / w) as u8, (y * 255 / h) as u8, ((x + y) * 127 / (w + h)) as u8])
        });
        let (raw, kind) = if grey {
            (DynamicImage::ImageRgb8(img).into_luma8().into_raw(), image::ExtendedColorType::L8)
        } else {
            (img.into_raw(), image::ExtendedColorType::Rgb8)
        };
        let mut out = Vec::new();
        let mut enc = JpegEncoder::new_with_quality(&mut out, 90);
        if let Some(o) = orientation {
            let exif = [
                b'M', b'M', 0, 42, 0, 0, 0, 8, 0, 1, 0x01, 0x12, 0, 3, 0, 0, 0, 1, 0, o, 0, 0, 0, 0, 0, 0,
            ];
            enc.set_exif_metadata(exif.to_vec()).unwrap();
        }
        enc.write_image(&raw, w, h, kind).unwrap();
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

    // The JPEG path before rows went straight to their upright place, as it shipped.
    fn old_decode_jpeg_scaled(
        bytes: &[u8],
        need: impl FnOnce(u32, u32) -> (u32, u32),
    ) -> Option<(image::RgbaImage, u32, u32)> {
        use image::metadata::Orientation;
        let mut dec = jpeg_decoder::Decoder::new(bytes);
        dec.read_info().ok()?;
        let info = dec.info()?;
        let (w, h) = (info.width as u32, info.height as u32);
        // The image crate's allocation limit, which this decoder lacks: a progressive file keeps
        // coefficients for every pixel the header claims.
        if u64::from(w) * u64::from(h) * 4 > 512 << 20 {
            return None;
        }
        let orientation = dec
            .exif_data()
            .and_then(Orientation::from_exif_chunk)
            .unwrap_or(Orientation::NoTransforms);
        let turned = matches!(
            orientation,
            Orientation::Rotate90 | Orientation::Rotate270 | Orientation::Rotate90FlipH | Orientation::Rotate270FlipH
        );
        let (uw, uh) = if turned { (h, w) } else { (w, h) };
        let (nw, nh) = need(uw, uh);
        let (rw, rh) = if turned { (nh, nw) } else { (nw, nh) };
        let scaled = |len: u32, s: u32| (len * s - 1) / 8 + 1;
        let s = [1u32, 2, 4, 8].into_iter().find(|&s| scaled(w, s) >= rw && scaled(h, s) >= rh)?;
        let (sw, sh) = dec.scale(scaled(w, s) as u16, scaled(h, s) as u16).ok()?;
        let (sw, sh) = (sw as u32, sh as u32);
        if sw < rw || sh < rh {
            return None;
        }
        let px = dec.decode().ok()?;
        let mut rgba = Vec::with_capacity(sw as usize * sh as usize * 4);
        match info.pixel_format {
            jpeg_decoder::PixelFormat::RGB24 => {
                for p in px.chunks_exact(3) {
                    rgba.extend_from_slice(&[p[0], p[1], p[2], 255]);
                }
            }
            jpeg_decoder::PixelFormat::L8 => {
                for &l in &px {
                    rgba.extend_from_slice(&[l, l, l, 255]);
                }
            }
            _ => return None,
        }
        drop(px);
        let mut img = image::DynamicImage::ImageRgba8(image::RgbaImage::from_raw(sw, sh, rgba)?);
        img.apply_orientation(orientation);
        Some((img.into_rgba8(), uw, uh))
    }

    fn old_jpeg_fitted(bytes: &[u8], max: u32, square: bool) -> Option<image::Frame> {
        let mut target = None;
        let (img, w, h) = old_decode_jpeg_scaled(bytes, |w, h| {
            let (cw, ch) = if square { (w.min(h), w.min(h)) } else { (w, h) };
            if (cw == w && ch == h && w.max(h) <= max) || max == 0 || cw == 0 {
                return (w, h);
            }
            let scale = (max as f64 / cw.max(ch) as f64).min(1.);
            let (nw, nh) = (
                ((cw as f64 * scale).round() as u32).max(1),
                ((ch as f64 * scale).round() as u32).max(1),
            );
            target = Some((nw, nh));
            (
                (w as f64 * nw as f64 / cw as f64).ceil() as u32,
                (h as f64 * nh as f64 / ch as f64).ceil() as u32,
            )
        })?;
        let (nw, nh) = target?;
        let (sw, sh) = img.dimensions();
        let k = sw as f64 / w as f64;
        let (cw, ch) = if square { (w.min(h), w.min(h)) } else { (w, h) };
        let (cw, ch) = (((cw as f64 * k).round() as u32).clamp(1, sw), ((ch as f64 * k).round() as u32).clamp(1, sh));
        let cut = if (cw, ch) == (sw, sh) {
            img
        } else {
            image::imageops::crop_imm(&img, (sw - cw) / 2, (sh - ch) / 2, cw, ch).to_image()
        };
        let mut buffer = if (cw, ch) == (nw, nh) { cut } else { gpui::resize_premultiplied(cut, nw, nh) };
        for p in buffer.chunks_exact_mut(4) {
            p.swap(0, 2);
        }
        Some(image::Frame::new(buffer))
    }

    #[test]
    fn rows_written_upright_match_the_copies_they_replaced() {
        for o in 1..=8u8 {
            for (w, h, grey) in [(1600, 1200, false), (333, 999, false), (97, 65, false), (333, 999, true)] {
                let bytes = encoded(w, h, Some(o), grey);
                for square in [false, true] {
                    for max in [(w.min(h) / 5).max(2), w.max(h) / 3] {
                        let f = gpui::jpeg_fitted(&bytes, max, square).map(|f| f.into_buffer());
                        let want = old_jpeg_fitted(&bytes, max, square).map(|f| f.into_buffer());
                        assert!(alike(f, want, o), "{w}x{h} orientation {o} square {square} max {max}");
                    }
                }
                let need = |w: u32, h: u32| (w / 3, h / 7);
                let got = gpui::decode_jpeg_scaled(&bytes, need);
                assert!(got == old_decode_jpeg_scaled(&bytes, need), "{w}x{h} orientation {o}");
                assert!(got.is_some());
            }
        }
    }

    // Scaled upright in one pass, or as decoded and then turned: a flip or turn may round one
    // level apart where the filter's sums run in another order.
    fn alike(a: Option<image::RgbaImage>, b: Option<image::RgbaImage>, orientation: u8) -> bool {
        match (a, b) {
            (Some(a), Some(b)) if orientation != 1 => {
                a.dimensions() == b.dimensions() && a.iter().zip(b.iter()).all(|(x, y)| x.abs_diff(*y) <= 1)
            }
            (a, b) => a == b,
        }
    }

    #[test]
    fn photos_scaled_as_they_decode_match_a_scale_after() {
        for o in 1..=8u8 {
            for (w, h, grey) in [(1600, 1200, false), (333, 999, false), (97, 65, false), (333, 999, true)] {
                let bytes = encoded(w, h, Some(o), grey);
                for max in [(w / 5, h / 5), (w / 3, h), (w * 2, h / 2), (17, 2000)] {
                    let got = decode(&bytes, "jpg", max).map(|(img, ..)| {
                        let s = img.size(0);
                        let mut px = img.as_bytes(0).unwrap().to_vec();
                        for p in px.chunks_exact_mut(4) {
                            p.swap(0, 2);
                        }
                        image::RgbaImage::from_raw(s.width.0 as u32, s.height.0 as u32, px).unwrap()
                    });
                    let mut target = None;
                    let want = old_decode_jpeg_scaled(&bytes, |w, h| {
                        let r = (max.0 as f64 / w as f64).min(max.1 as f64 / h as f64);
                        let t = (((w as f64 * r).round() as u32).max(1), ((h as f64 * r).round() as u32).max(1));
                        target = Some(t);
                        t
                    })
                    .map(|(img, ..)| {
                        let (tw, th) = target.unwrap();
                        gpui::resample(&img, tw, th, FilterType::CatmullRom)
                    });
                    assert!(alike(got, want, o), "{w}x{h} orientation {o} into {max:?}");
                }
            }
        }
    }

    fn png_of(w: u32, h: u32, colour: png::ColorType, depth: png::BitDepth, trns: bool) -> Vec<u8> {
        let mut seed = w * 31 + h;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        let channels = match colour {
            png::ColorType::Grayscale | png::ColorType::Indexed => 1,
            png::ColorType::GrayscaleAlpha => 2,
            png::ColorType::Rgb => 3,
            png::ColorType::Rgba => 4,
        };
        let bits = depth as usize;
        let row = (w as usize * channels * bits).div_ceil(8);
        // Smooth enough to read as a picture, with noise in the low bits.
        let data: Vec<u8> = (0..h as usize)
            .flat_map(|y| (0..row).map(move |x| (x * 3 + y * 5) as u8))
            .map(|v| v ^ (next() as u8 & 7))
            .collect();
        let mut out = Vec::new();
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(colour);
        enc.set_depth(depth);
        if colour == png::ColorType::Indexed {
            let n = 1usize << bits;
            enc.set_palette((0..n * 3).map(|i| (i * 37) as u8).collect::<Vec<_>>());
            if trns {
                enc.set_trns((0..n).map(|i| (i * 91) as u8).collect::<Vec<_>>());
            }
        } else if trns && colour == png::ColorType::Rgb {
            enc.set_trns(vec![0, 3, 0, 6, 0, 9]);
        }
        let mut wr = enc.write_header().unwrap();
        wr.write_image_data(&data).unwrap();
        drop(wr);
        out
    }

    #[test]
    fn pngs_decoded_a_row_at_a_time_match_the_whole_decode() {
        use png::{BitDepth as B, ColorType as T};
        let kinds = [
            (T::Rgba, B::Eight, false),
            (T::Rgb, B::Eight, false),
            (T::Rgb, B::Eight, true),
            (T::Grayscale, B::Eight, false),
            (T::Grayscale, B::Two, false),
            (T::GrayscaleAlpha, B::Eight, false),
            (T::Indexed, B::Eight, false),
            (T::Indexed, B::Four, true),
        ];
        for (colour, depth, trns) in kinds {
            for (w, h) in [(333, 199), (97, 65), (1, 40)] {
                let bytes = png_of(w, h, colour, depth, trns);
                for max in [(w, h), (w / 3 + 1, h), (w * 2, h / 4 + 1), (5000, 5000)] {
                    let got = png_into(&bytes, max).map(|(img, ..)| img.into_raw());
                    let img = image::load_from_memory(&bytes).unwrap();
                    let img = if w > max.0 || h > max.1 {
                        gpui::resample_within(&img, max.0, max.1, FilterType::CatmullRom)
                    } else {
                        img
                    };
                    let want = to_bgra(img.into_rgba8()).into_raw();
                    assert!(got.as_ref() == Some(&want), "{colour:?} {depth:?} trns {trns} {w}x{h} into {max:?}");
                }
            }
        }
        // The header's interlace byte set, its CRC redone: refused before any data is read.
        let mut interlaced = png_of(8, 8, T::Grayscale, B::Eight, false);
        interlaced[28] = 1;
        let crc = interlaced[12..29].iter().fold(!0u32, |c, &b| {
            (0..8).fold(c ^ u32::from(b), |c, _| if c & 1 == 1 { (c >> 1) ^ 0xEDB8_8320 } else { c >> 1 })
        });
        interlaced[29..33].copy_from_slice(&(!crc).to_be_bytes());
        assert!(png::Decoder::new(Cursor::new(&interlaced[..])).read_info().unwrap().info().interlaced);
        assert!(png_into(&interlaced, (4, 4)).is_none());
        assert!(png_into(&png_of(9, 9, T::Rgb, B::Sixteen, false), (4, 4)).is_none());
        let mut cut = png_of(97, 65, T::Rgb, B::Eight, false);
        cut.truncate(cut.len() / 2);
        assert!(png_into(&cut, (40, 40)).is_none());
    }

    #[test]
    fn rows_fed_one_at_a_time_match_the_whole_picture() {
        let mut seed = 0x9e37_79b9u32;
        for (w, h, nw, nh) in [(97, 65, 31, 20), (31, 20, 97, 65), (640, 480, 100, 75), (5, 300, 3, 7), (1, 1, 4, 4), (300, 2, 299, 1)] {
            let px: Vec<u8> = (0..w * h * 3)
                .map(|_| {
                    seed ^= seed << 13;
                    seed ^= seed >> 17;
                    seed ^= seed << 5;
                    seed as u8
                })
                .collect();
            let img = image::RgbImage::from_raw(w, h, px).unwrap();
            for filter in [FilterType::Triangle, FilterType::CatmullRom] {
                let want = gpui::resample(&img, nw, nh, filter).into_raw();
                let mut got = vec![0u8; want.len()];
                let mut rows = gpui::Rows::<3>::new(w, h, nw, nh, filter).unwrap();
                let line = nw as usize * 3;
                for (y, row) in img.as_raw().chunks_exact(w as usize * 3).enumerate() {
                    rows.push(y, row, |r, o| got[r * line..][..line].copy_from_slice(o));
                }
                assert!(got == want, "{w}x{h} into {nw}x{nh} {filter:?}");
            }
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
        // Small enough already: decoded whole, as it is.
        let f = gpui::jpeg_fitted(&jpeg(80, 60, None), 100, false).unwrap();
        assert_eq!(f.buffer().dimensions(), (80, 60));
    }
}

/// Decoded size of every frame of `image`.
/// How long an entry of `bytes` is kept off screen: `idle` at [`IDLE_AT`] or less, shrinking
/// with the square root of its size, to a sixth of it.
fn idle_for(bytes: usize, idle: std::time::Duration) -> std::time::Duration {
    let s = (IDLE_AT as f64 / bytes.max(1) as f64).sqrt().clamp(1. / 6., 1.);
    idle.mul_f64(s)
}

pub fn image_bytes(image: &RenderImage) -> usize {
    (0..image.frame_count())
        .map(|i| {
            let s = image.size(i);
            s.width.0.max(0) as usize * s.height.0.max(0) as usize * 4
        })
        .sum()
}

#[cfg(test)]
mod resample_tests {
    use image::imageops::{self, FilterType};
    use image::{ImageBuffer, Luma, Pixel, Rgb, Rgba};

    fn noise<P: Pixel<Subpixel = u8> + 'static>(w: u32, h: u32, seed: u64) -> ImageBuffer<P, Vec<u8>> {
        let mut s = seed | 1;
        let raw = (0..w as usize * h as usize * P::CHANNEL_COUNT as usize)
            .map(|i| {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                // Half noise, half smooth ramps: rounding ties show up on flat areas.
                if (i / 97) % 2 == 0 { s as u8 } else { (i % 256) as u8 }
            })
            .collect();
        ImageBuffer::from_raw(w, h, raw).unwrap()
    }

    fn same<P: Pixel<Subpixel = u8> + 'static>(w: u32, h: u32, nw: u32, nh: u32, f: FilterType) {
        let img = noise::<P>(w, h, (w * 31 + h * 7 + nw) as u64);
        let ours = gpui::resample(&img, nw, nh, f);
        let theirs = imageops::resize(&img, nw, nh, f);
        assert_eq!(ours.dimensions(), theirs.dimensions());
        assert!(ours.as_raw() == theirs.as_raw(), "{w}x{h} -> {nw}x{nh} {f:?} {}ch", P::CHANNEL_COUNT);
    }

    #[test]
    fn the_streamed_resize_matches_the_image_crate_to_the_bit() {
        let sizes = [
            (640, 480, 160, 120),
            (1008, 756, 600, 450),
            (333, 777, 101, 37),
            (64, 64, 63, 65),
            (7, 5, 40, 29),
            (1, 1, 9, 4),
            (500, 1, 13, 1),
            (1, 300, 1, 2),
            (32, 32, 1, 1),
        ];
        for (w, h, nw, nh) in sizes {
            for f in [FilterType::Triangle, FilterType::CatmullRom] {
                same::<Rgba<u8>>(w, h, nw, nh, f);
                same::<Rgb<u8>>(w, h, nw, nh, f);
                same::<Luma<u8>>(w, h, nw, nh, f);
            }
        }
    }
}

#[cfg(test)]
mod swap_tests {
    #[test]
    fn swap_red_blue_matches_the_byte_swap_at_every_offset_and_length() {
        let src: Vec<u8> = (0..4099u32).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect();
        for off in 0..8 {
            for len in [0, 4, 8, 12, 64, 1020, 4084] {
                let len = len.min(src.len() - off) / 4 * 4;
                let mut want = src[off..off + len].to_vec();
                for px in want.chunks_exact_mut(4) {
                    px.swap(0, 2);
                }
                let mut buf = src.clone();
                super::swap_red_blue(&mut buf[off..off + len]);
                assert_eq!(&buf[off..off + len], &want[..], "offset {off}, length {len}");
                assert_eq!(&buf[..off], &src[..off]);
                assert_eq!(&buf[off + len..], &src[off + len..]);
            }
        }
    }
}
