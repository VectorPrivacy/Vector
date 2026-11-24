//! `FilePreview`: what is about to be sent, its name (click to rename), size, the image
//! options and the buttons. The picture compresses in the background while it shows, so a
//! compressed send rarely waits.

use crate::kit::Tipped as _;
use crate::t;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures::FutureExt;
use futures::future::Shared;
use gpui::prelude::*;
use gpui::{AnimationExt, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight, KeyDownEvent, MouseButton, RenderImage, SharedString, Task, Window, div, linear_color_stop, linear_gradient, px, svg};
use crate::pic::img;

use crate::composer::{CancelInput, InputChanged, SendText, TextInput};
use crate::message::types::CachedCompressedImage;
use crate::theme::{INTER, c, white_a};

pub const IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "jxl", "tiff", "tif", "ico"];
/// Pictures shown before sending but sent as they are: the send pipeline cannot read them.
#[cfg(feature = "video")]
const SHOWN_ONLY: &[&str] = &["heic", "heif", "heics", "heifs"];
#[cfg(not(feature = "video"))]
const SHOWN_ONLY: &[&str] = &[];
const MIN_COMPRESS_SIZE: u64 = 25 * 1024;

pub type Compressed = Shared<Task<Option<CachedCompressedImage>>>;

/// What the preview asks the app to send.
pub struct Send {
    pub path: PathBuf,
    pub receiver: String,
    pub reply: String,
    /// The name it travels under; empty keeps the file's own.
    pub name: String,
    pub compress: bool,
    pub keep_metadata: bool,
    pub compressed: Option<Compressed>,
}

pub enum Event {
    Send(Send),
    /// The archive couldn't be made.
    Failed(String),
    /// Dismissed: the file it showed.
    Close(PathBuf),
    /// The Mini App goes to the App Store instead.
    Publish(PathBuf),
    /// A staged file's edits are kept; it waits in the composer.
    Done,
    /// A staged file taken out of the composer.
    Remove,
    /// The picture was cropped: it now reads from `path()`, held in memory.
    Cropped,
}

impl EventEmitter<Event> for FilePreview {}

pub struct FilePreview {
    focus: FocusHandle,
    path: PathBuf,
    receiver: String,
    reply: String,
    stem: String,
    edited: bool,
    ext: String,
    size: u64,
    /// The picture and its natural size; None draws the file's icon.
    image: Option<(Arc<RenderImage>, u32, u32)>,
    icon: &'static str,
    spoiler: bool,
    blur: Option<Arc<RenderImage>>,
    compress_offered: bool,
    compress: bool,
    /// A picked video plays here through the app's clips, under its own key.
    media: Option<std::rc::Rc<std::cell::RefCell<crate::media::Media>>>,
    /// Send pressed mid-encode: the name it sends under once the encode lands.
    held: Option<String>,
    compress_info: SharedString,
    compressed: Option<Compressed>,
    cancel: Arc<AtomicBool>,
    metadata_offered: bool,
    keep_metadata: bool,
    rename: Option<Entity<TextInput>>,
    zip: Option<Zip>,
    /// Offers Publish: a Mini App in the trusted publisher's hands.
    pub publish: bool,
    /// Ask the media servers whether they'd take a finished zip (a live session only).
    pub check_servers: bool,
    /// No enabled server would take the zip: it can't be sent.
    too_large: bool,
    /// Waits in the composer with others: this edits it, and the composer sends them all.
    staged: bool,
    crop: Option<Entity<crate::cropper::Cropper>>,
    /// A crop is being made: the picture shows dimmed until it lands.
    cropping: bool,
}

/// Pictures the cropper can cut and re-encode as they are (animations frame by frame).
const CROPPABLE: &[&str] = &["png", "jpg", "jpeg", "gif", "webp"];

/// A folder or a selection being zipped, then what the archive holds.
enum Zip {
    Running(Arc<std::sync::atomic::AtomicU64>),
    Done {
        tree: Node,
        total: u32,
        listed: usize,
        open: std::collections::HashSet<String>,
        label: String,
    },
}

#[derive(Default)]
struct Node {
    name: String,
    /// The folder's path inside the archive.
    path: String,
    dirs: Vec<Node>,
    files: Vec<(String, u64)>,
}

impl Node {
    fn child(&mut self, name: &str) -> &mut Node {
        let i = match self.dirs.iter().position(|d| d.name == name) {
            Some(i) => i,
            None => {
                let path = if self.path.is_empty() {
                    name.to_string()
                } else {
                    format!("{}/{name}", self.path)
                };
                self.dirs.push(Node {
                    name: name.to_string(),
                    path,
                    ..Default::default()
                });
                self.dirs.len() - 1
            }
        };
        &mut self.dirs[i]
    }

    /// `buildFileTree`, sorted as `renderTreeNode` sorts.
    fn build(entries: &[crate::backend::archive::Entry]) -> Node {
        let mut root = Node::default();
        for e in entries {
            let clean = e.path.trim_end_matches('/');
            let mut parts: Vec<&str> = clean.split('/').collect();
            let file = if e.is_dir { None } else { parts.pop() };
            let mut node = &mut root;
            for part in parts {
                node = node.child(part);
            }
            if let Some(f) = file {
                node.files.push((f.to_string(), e.size));
            }
        }
        root.sort();
        root
    }

    fn sort(&mut self) {
        self.dirs.sort_by(|a, b| a.name.cmp(&b.name));
        self.files.sort_by(|a, b| a.0.cmp(&b.0));
        for d in &mut self.dirs {
            d.sort();
        }
    }
}

/// `getFileIcon`: the glyph a file without a picture shows.
pub fn file_icon(ext: &str) -> &'static str {
    match ext {
        e if IMAGE_EXTS.contains(&e) || SHOWN_ONLY.contains(&e) || e == "svg" => "icons/image.svg",
        "mp4" | "webm" | "mov" | "avi" | "mkv" | "m4v" => "icons/film.svg",
        "mp3" | "wav" | "ogg" | "flac" | "m4a" | "aac" => "icons/volume-max.svg",
        "zip" | "rar" | "7z" | "tar" | "gz" | "bz2" | "xz" | "zst" | "tgz" | "tbz2" => {
            "icons/folder.svg"
        }
        _ => "icons/file.svg",
    }
}

/// Characters no filesystem takes in a name.
fn clean_stem(s: &str) -> String {
    s.chars()
        .filter(|c| {
            !matches!(
                c,
                '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\0'
            )
        })
        .take(64)
        .collect()
}

impl FilePreview {
    pub fn with_media(mut self, media: std::rc::Rc<std::cell::RefCell<crate::media::Media>>) -> Self {
        self.media = Some(media);
        self
    }

    fn clip_key(&self) -> String {
        format!("fp:{}", self.path.display())
    }

    /// `.file-preview-video`: the clip, muted, sized as a picture would be.
    fn clip_view(&self, room: f32, max_h: f32, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let media = self.media.as_ref()?;
        if !crate::video::EXTENSIONS.contains(&self.ext.as_str()) {
            return None;
        }
        let key = self.clip_key();
        let spec = crate::audio::AudioInputSpec::File(self.path.clone());
        let mut m = media.borrow_mut();
        if m.videos.failed(&key) {
            return None;
        }
        m.videos.want_probe(&key, &spec);
        let (mine, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut m.videos.queue)
            .into_iter()
            .partition(|crate::video::Work::Probe(k, ..)| *k == key);
        m.videos.queue = rest;
        for work in mine {
            let (rx, media) = (crate::video::run(work), media.clone());
            cx.spawn(async move |this, cx| {
                let Ok(crate::video::WorkDone::Probed(key, info)) = rx.await else { return };
                let mut m = media.borrow_mut();
                let ok = info.is_some();
                m.videos.probed(key.clone(), info);
                if ok && !m.videos.clip(&key).is_some_and(|c| c.muted) {
                    m.videos.act(&key, None, crate::video::Act::Mute);
                }
                drop(m);
                this.update(cx, |_, cx| cx.notify()).ok();
            })
            .detach();
        }
        let clip = m.videos.clip(&key)?;
        let (w, h) = clip.info.as_ref().map_or((16., 9.), |i| crate::video::fit(i.w, i.h));
        let s = (room / w).min(max_h / h).min(1.);
        let (on_media, me, k, path) = (media.clone(), cx.entity().downgrade(), key.clone(), self.path.clone());
        Some(crate::video::view(
            &key,
            clip,
            false,
            Some((w * s, h * s)),
            (8., 8.),
            std::rc::Rc::new(move |a, _, cx| {
                on_media.borrow_mut().videos.act(&k, Some(crate::audio::AudioInputSpec::File(path.clone())), a);
                me.update(cx, |_, cx| cx.notify()).ok();
            }),
        ))
    }

    /// A file staged in the composer: compressing starts now, its picture loads when shown.
    pub fn staged(path: PathBuf, receiver: String, cx: &mut Context<Self>) -> Self {
        Self::build(path, receiver, String::new(), true, cx)
    }

    fn build(path: PathBuf, receiver: String, reply: String, staged: bool, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file").to_string();
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();
        let mut stem = match name.rsplit_once('.') {
            Some((stem, e)) if !ext.is_empty() && e.eq_ignore_ascii_case(&ext) => stem.to_string(),
            _ => name.clone(),
        };
        let spoiler = stem.to_uppercase().starts_with("SPOILER_");
        if spoiler {
            stem = stem[8..].to_string();
        }
        let size = crate::backend::pasted::len(&path).unwrap_or(0);
        let is_image = IMAGE_EXTS.contains(&ext.as_str());
        let video = crate::message::offers_video_compression(&ext, size);
        let compress_offered = video || (is_image && size > MIN_COMPRESS_SIZE);
        let cancel = Arc::new(AtomicBool::new(false));

        let compressed = compress_offered.then(|| {
            let (file, flag) = (path.clone(), cancel.clone());
            let task = cx.background_executor().spawn(async move {
                crate::shared::cancel::scoped(flag, || crate::message::precompress(&file))
                    .inspect_err(|e| log::warn!("precompress {}: {e}", file.display()))
                    .ok()
            });
            let shared = task.shared();
            let watch = shared.clone();
            if video {
                let file = path.to_string_lossy().into_owned();
                cx.spawn(async move |this, cx| loop {
                    cx.background_executor().timer(std::time::Duration::from_millis(200)).await;
                    let done = crate::message::video_compression::progress(&file);
                    let Ok(going) = this.update(cx, |p, cx| {
                        if p.compressed.as_ref().is_some_and(|c| c.peek().is_some()) {
                            return false;
                        }
                        if let Some(f) = done {
                            p.compress_info = t::preview_compressing_percent((f * 100.) as u32);
                            cx.notify();
                        }
                        true
                    }) else {
                        return;
                    };
                    if !going {
                        return;
                    }
                })
                .detach();
            }
            cx.spawn(async move |this, cx| {
                let done = watch.await;
                this.update(cx, |p, cx| {
                    if let Some(name) = p.held.take() {
                        p.dispatch(name, p.compress, cx);
                    }
                    p.compress_info = match &done {
                        Some(c) if crate::message::savings_percent(c) > 0 => t::preview_compress_saving(
                            crate::message::savings_percent(c),
                            crate::media::format_bytes(c.compressed_size),
                        ),
                        Some(_) => t::preview_compress_no_savings(),
                        None => t::preview_compress_failed(),
                    };
                    cx.notify();
                })
                .ok();
            })
            .detach();
            shared
        });

        if is_image || SHOWN_ONLY.contains(&ext.as_str()) {
            if !staged {
                Self::load_image(path.clone(), ext.clone(), cx);
            }
            if ext != "gif" {
                let file = path.clone();
                cx.spawn(async move |this, cx| {
                    let has = cx
                        .background_executor()
                        .spawn(async move { crate::backend::messaging::has_metadata(&file) })
                        .await;
                    this.update(cx, |p, cx| {
                        p.metadata_offered = has;
                        cx.notify();
                    })
                    .ok();
                })
                .detach();
            }
        }

        Self {
            focus,
            icon: file_icon(&ext),
            path,
            receiver,
            reply,
            stem,
            edited: false,
            ext,
            size,
            image: None,
            spoiler,
            blur: None,
            compress_offered,
            compress: true,
            held: None,
            media: None,
            compress_info: t::preview_compressing(),
            compressed,
            cancel,
            metadata_offered: false,
            keep_metadata: false,
            rename: None,
            zip: None,
            publish: false,
            check_servers: false,
            too_large: false,
            staged,
            crop: None,
            cropping: false,
        }
    }

    fn can_crop(&self) -> bool {
        self.image.is_some() && self.zip.is_none() && CROPPABLE.contains(&self.ext.as_str())
    }

    fn start_crop(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((image, w, h)) = self.image.clone().filter(|_| self.can_crop() && !self.cropping) else { return };
        let cropper = cx.new(|cx| crate::cropper::Cropper::new(image, (w, h), t::crop_file_title(), t::crop_file_hint(), false, window, cx).free());
        cx.subscribe_in(&cropper, window, |p, _, answer: &crate::cropper::Answer, window, cx| {
            p.crop = None;
            window.focus(&p.focus, cx);
            if let crate::cropper::Answer::Crop(rect) = answer {
                p.apply_crop(rect.clone(), window, cx);
            }
            cx.notify();
        })
        .detach();
        self.crop = Some(cropper);
        cx.notify();
    }

    /// The cut is made off the main thread on the file's own bytes; the result is held in
    /// memory only, under the same name.
    fn apply_crop(&mut self, rect: vector_core::image_crop::CropRect, window: &mut Window, cx: &mut Context<Self>) {
        self.cropping = true;
        let file = self.path.clone();
        let task = cx.background_executor().spawn(async move {
            let bytes = crate::backend::pasted::read(&file).ok()?;
            crate::crop::crop_drawn(bytes.to_vec(), rect)
                .inspect_err(|e| log::warn!("crop: {e}"))
                .ok()
        });
        cx.spawn_in(window, async move |this, cx| {
            let out = task.await;
            this.update_in(cx, |p, window, cx| {
                p.cropping = false;
                match out {
                    Some(bytes) => p.replace_with(bytes, window, cx),
                    None => cx.notify(),
                }
            })
            .ok();
        })
        .detach();
    }

    fn replace_with(&mut self, bytes: Vec<u8>, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.path.file_name().and_then(|n| n.to_str()).unwrap_or("image").to_string();
        let path = crate::backend::pasted::hold_named(bytes, &name);
        self.stop_compressing();
        self.forget_clip();
        let mut next = Self::build(path, self.receiver.clone(), self.reply.clone(), self.staged, cx);
        next.stem = std::mem::take(&mut self.stem);
        next.edited = self.edited;
        next.spoiler = self.spoiler;
        next.compress = self.compress;
        next.media = self.media.take();
        next.publish = self.publish;
        next.check_servers = self.check_servers;
        crate::backend::archive::forget(&self.path);
        *self = next;
        Self::load_image(self.path.clone(), self.ext.clone(), cx);
        window.focus(&self.focus, cx);
        cx.emit(Event::Cropped);
        cx.notify();
    }

    fn load_image(file: PathBuf, e: String, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let decoded = cx
                .background_executor()
                .spawn(async move {
                    crate::backend::pasted::read(&file)
                        .ok()
                        .and_then(|b| crate::media::decode(&b, &e, (1600, 1600)))
                })
                .await;
            this.update(cx, |p, cx| {
                if let Some((image, w, h)) = decoded {
                    p.image = Some((Arc::new(image), w, h));
                    if p.spoiler {
                        p.make_blur(cx);
                    }
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// A staged file opened for editing: its picture loads, and keys come here.
    pub fn open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.image.is_none() && (IMAGE_EXTS.contains(&self.ext.as_str()) || SHOWN_ONLY.contains(&self.ext.as_str())) {
            Self::load_image(self.path.clone(), self.ext.clone(), cx);
        }
        window.focus(&self.focus, cx);
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    pub fn ext(&self) -> &str {
        &self.ext
    }

    pub fn spoiler(&self) -> bool {
        self.spoiler
    }

    /// The name it shows under: the edited stem and its extension.
    pub fn shown_name(&self) -> String {
        if self.ext.is_empty() { self.stem.clone() } else { format!("{}.{}", self.stem, self.ext) }
    }

    /// The name it travels under; empty keeps the file's own.
    fn travel_name(&self) -> String {
        let ext = if self.ext.is_empty() {
            String::new()
        } else {
            format!(".{}", self.ext)
        };
        if self.spoiler {
            let stem = if self.stem.is_empty() {
                "image"
            } else {
                &self.stem
            };
            let stem = if stem.to_uppercase().starts_with("SPOILER_") {
                stem.to_string()
            } else {
                format!("SPOILER_{stem}") // i18n-exempt: filename convention
            };
            format!("{stem}{ext}")
        } else if self.edited {
            format!("{}{ext}", self.stem)
        } else {
            String::new()
        }
    }

    /// The staged file as it sends.
    pub fn outgoing(&self) -> crate::backend::outbox::Outgoing {
        let compress = self.compress_offered && self.compress;
        let keep_metadata = self.metadata_offered && self.keep_metadata;
        crate::backend::outbox::Outgoing {
            path: self.path.clone(),
            name: self.travel_name(),
            compress,
            keep_metadata,
            precompressed: None,
        }
    }

    /// The background compression a send waits on, when the choices still use it.
    pub fn compressed_for_send(&self) -> Option<Compressed> {
        let o = self.outgoing();
        self.compressed.clone().filter(|_| o.compress && !o.keep_metadata)
    }

    /// Taken out of the composer, or sent: its encode stops and its clip goes.
    pub fn discard(&mut self) {
        if !self.outgoing().compress {
            self.stop_compressing();
        }
        self.forget_clip();
    }

    /// Edits kept; what only the open preview needed goes.
    fn done(&mut self, cx: &mut Context<Self>) {
        if self.rename.is_some() {
            self.rename = None;
        }
        self.forget_clip();
        self.image = None;
        self.blur = None;
        cx.emit(Event::Done);
    }

    /// A folder, or several items, zipped into one archive that sends under its name.
    pub fn zipped(
        paths: Vec<PathBuf>,
        receiver: String,
        reply: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let percent = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        let (flag, pct) = (cancel.clone(), percent.clone());
        let stem = crate::backend::archive::archive_name(&paths);
        let work = cx
            .background_executor()
            .spawn(async move { crate::backend::archive::zip_paths(&paths, &flag, &pct) });
        cx.spawn(async move |this, cx| {
            let mut work = std::pin::pin!(work.fuse());
            loop {
                let timer = cx.background_executor().timer(Duration::from_millis(100));
                futures::select_biased! {
                    done = work.as_mut() => {
                        this.update(cx, |p, cx| p.zipped_done(done, cx)).ok();
                        return;
                    }
                    _ = timer.fuse() => {
                        if this.update(cx, |_, cx| cx.notify()).is_err() {
                            return;
                        }
                    }
                }
            }
        })
        .detach();
        Self {
            staged: false,
            crop: None,
            cropping: false,
            focus,
            path: PathBuf::new(),
            receiver,
            reply,
            stem,
            edited: true,
            ext: "zip".into(),
            size: 0,
            image: None,
            icon: "icons/folder.svg",
            spoiler: false,
            blur: None,
            compress_offered: false,
            compress: false,
            held: None,
            media: None,
            compress_info: SharedString::default(),
            compressed: None,
            cancel,
            metadata_offered: false,
            keep_metadata: false,
            rename: None,
            zip: Some(Zip::Running(percent)),
            publish: false,
            check_servers: false,
            too_large: false,
        }
    }

    fn zipped_done(
        &mut self,
        done: Result<crate::backend::archive::Zipped, String>,
        cx: &mut Context<Self>,
    ) {
        match done {
            Ok(z) => {
                let size = crate::media::format_bytes(z.size);
                let label = if z.dir_count > 0 {
                    t::preview_zip_files_folders(z.file_count, z.dir_count, size).to_string()
                } else {
                    t::preview_zip_files(z.file_count, size).to_string()
                };
                let tree = Node::build(&z.entries);
                let open = tree.dirs.iter().map(|d| d.path.clone()).collect();
                self.path = z.path;
                self.size = z.size;
                if self.check_servers {
                    // `checkUploadBlocked`, scoped to the .zip: falls open if the ask fails.
                    let size = z.size;
                    let rx = crate::backend::run(async move {
                        let servers = vector_core::state::get_blossom_servers();
                        vector_core::blossom_capabilities::upload_verdict(&servers, "application/zip", true, size).likely
                    });
                    cx.spawn(async move |this, cx| {
                        if let Ok(false) = rx.await {
                            this.update(cx, |p, cx| {
                                p.too_large = true;
                                if let Some(Zip::Done { label, .. }) = p.zip.as_mut() {
                                    *label = t::preview_zip_too_large(crate::media::format_bytes(size)).to_string();
                                }
                                cx.notify();
                            })
                            .ok();
                        }
                    })
                    .detach();
                }
                self.zip = Some(Zip::Done {
                    tree,
                    total: z.file_count + z.dir_count,
                    listed: z.entries.len(),
                    open,
                    label,
                });
            }
            Err(e) => {
                if !self.cancel.load(Ordering::Relaxed) {
                    cx.emit(Event::Failed(e));
                }
            }
        }
        cx.notify();
    }

    /// The thumbhash blur a spoiler shows in the picture's place.
    fn make_blur(&mut self, cx: &mut Context<Self>) {
        if self.blur.is_some() {
            return;
        }
        let Some((image, _, _)) = self.image.clone() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let blur = cx
                .background_executor()
                .spawn(async move {
                    let size = image.size(0);
                    let (w, h) = (size.width.0 as u32, size.height.0 as u32);
                    // Decoded frames are BGRA, as the GPU takes them.
                    let mut bytes = image.as_bytes(0)?.to_vec();
                    crate::media::swap_red_blue(&mut bytes);
                    let rgba = image::RgbaImage::from_raw(w, h, bytes)?;
                    let hash = crate::util::generate_thumbhash_from_image(
                        &image::DynamicImage::ImageRgba8(rgba),
                    )?;
                    crate::media::decode_thumbhash(&hash)
                })
                .await;
            this.update(cx, |p, cx| {
                p.blur = blur.map(Arc::new);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn start_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let look = crate::composer::Look {
            family: INTER,
            weight: FontWeight::MEDIUM,
            size: px(18.),
            line_height: px(24.),
            color: c(0xf7f4f4),
            ..crate::kit::field_look()
        };
        let stem = self.stem.clone();
        let input = cx.new(|cx| {
            let mut i = TextInput::new("", cx)
                .with_look(look)
                .centered()
                .keep_on_submit();
            i.set_text(&stem, cx);
            i.select_everything(cx);
            i
        });
        cx.subscribe(&input, |p, input, _: &InputChanged, cx| {
            let text = input.read(cx).text().to_string();
            let clean = clean_stem(&text);
            if clean != text {
                input.update(cx, |i, cx| i.set_text(&clean, cx));
            }
            let _ = p;
        })
        .detach();
        cx.subscribe_in(&input, window, |p, _, _: &SendText, window, cx| {
            p.end_rename(true, window, cx)
        })
        .detach();
        cx.subscribe_in(&input, window, |p, _, _: &CancelInput, window, cx| {
            p.end_rename(false, window, cx)
        })
        .detach();
        window.focus(&input.focus_handle(cx), cx);
        self.rename = Some(input);
        cx.notify();
    }

    fn end_rename(&mut self, save: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(input) = self.rename.take() else {
            return;
        };
        let name = input.read(cx).text().trim().to_string();
        if save && !name.is_empty() {
            self.stem = name;
            self.edited = true;
        }
        window.focus(&self.focus, cx);
        cx.notify();
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        if self.staged {
            return self.done(cx);
        }
        self.stop_compressing();
        self.forget_clip();
        cx.emit(Event::Close(self.path.clone()));
    }

    fn forget_clip(&self) {
        if let Some(m) = &self.media {
            m.borrow_mut().videos.forget(&self.clip_key());
        }
    }

    fn stop_compressing(&self) {
        self.cancel.store(true, Ordering::Relaxed);
        if self.compressed.as_ref().is_some_and(|c| c.peek().is_none()) {
            crate::message::video_compression::cancel(&self.path.to_string_lossy());
        }
    }

    fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.zip, Some(Zip::Running(_))) || self.too_large {
            return;
        }
        if self.rename.is_some() {
            self.end_rename(true, window, cx);
        }
        if self.staged {
            return self.done(cx);
        }
        let name = self.travel_name();
        let compress = self.compress_offered && self.compress;
        if !compress {
            self.stop_compressing();
        }
        // A video encode can run long and the chat shows nothing until it lands.
        let encoding = compress && !IMAGE_EXTS.contains(&self.ext.as_str()) && self.compressed.as_ref().is_some_and(|c| c.peek().is_none());
        if encoding {
            self.held = Some(name);
            cx.notify();
            return;
        }
        self.dispatch(name, compress, cx);
    }

    fn dispatch(&mut self, name: String, compress: bool, cx: &mut Context<Self>) {
        self.forget_clip();
        cx.emit(Event::Send(Send {
            path: self.path.clone(),
            receiver: self.receiver.clone(),
            reply: self.reply.clone(),
            name,
            compress,
            keep_metadata: self.metadata_offered && self.keep_metadata,
            compressed: self.compressed.clone(),
        }));
    }
}

/// `.neon-toggle`: a 50x24 switch.
pub fn neon_toggle(on: bool) -> gpui::Div {
    div()
        .relative()
        .flex_shrink_0()
        .w(px(50.))
        .h(px(24.))
        .rounded(px(12.))
        .map(|d| {
            if on {
                d.bg(linear_gradient(
                    90.,
                    linear_color_stop(c(crate::theme::TOGGLE_FROM), 0.),
                    linear_color_stop(c(crate::theme::TOGGLE_TO), 1.),
                ))
            } else {
                d.bg(c(0x1a1a1a))
            }
        })
        .child(
            div()
                .absolute()
                .top(px(2.))
                .left(px(if on { 28. } else { 2. }))
                .size(px(20.))
                .rounded_full()
                .bg(c(if on { 0xffffff } else { 0x333333 })),
        )
}

fn option(
    id: &'static str,
    name: SharedString,
    label: gpui::AnyElement,
    sublabel: impl Into<SharedString>,
    on: bool,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .role(gpui::Role::Switch)
        .aria_toggled(on.into())
        .aria_label(name)
        .w_full()
        .flex()
        .items_center()
        .justify_between()
        .gap(px(12.))
        .px(px(14.))
        .py(px(9.))
        .bg(white_a(0.04))
        .hover(|s| s.bg(white_a(0.07)))
        .rounded(px(12.))
        .cursor_pointer()
        .child(
            div()
                .min_w_0()
                .flex()
                .flex_wrap()
                .items_baseline()
                .gap_x(px(8.))
                .child(
                    div()
                        .text_size(px(13.5))
                        .line_height(px(20.))
                        .text_color(white_a(0.9))
                        .child(label),
                )
                .child(
                    div()
                        .text_size(px(12.5))
                        .line_height(px(20.))
                        .text_color(white_a(0.45))
                        .child(sublabel.into()),
                ),
        )
        .child(neon_toggle(on))
}

impl Render for FilePreview {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (vw, vh) = (f32::from(window.viewport_size().width), f32::from(window.viewport_size().height));
        let card_w = 520f32.min(vw - 32.);
        let inner = card_w - 32.;
        let max_h = vh * 0.55;
        let content = match &self.zip {
            Some(Zip::Running(pct)) => {
                let p = pct.load(Ordering::Relaxed);
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(12.))
                    .child(
                        div()
                            .size(px(120.))
                            .mb(px(8.))
                            .rounded(px(16.))
                            .bg(c(crate::theme::ACCENT).opacity(0.1))
                            .border_1()
                            .border_color(c(crate::theme::ACCENT).opacity(0.2))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(crate::attachments::ring(48., Some(p as f32))),
                    )
                    .child(
                        div()
                            .text_size(px(13.))
                            .text_color(white_a(0.6))
                            .child(if p > 0 {
                                t::preview_compressing_percent(p)
                            } else {
                                t::preview_compressing()
                            }),
                    )
                    .into_any_element()
            }
            Some(Zip::Done {
                tree,
                total,
                listed,
                open,
                ..
            }) => {
                let mut rows = Vec::new();
                tree_rows(tree, 0, open, &mut rows, cx);
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(12.))
                    .child(
                        div()
                            .size(px(120.))
                            .mb(px(8.))
                            .rounded(px(16.))
                            .bg(c(crate::theme::ACCENT).opacity(0.1))
                            .border_1()
                            .border_color(c(crate::theme::ACCENT).opacity(0.2))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
                                svg()
                                    .path("icons/folder.svg")
                                    .size(px(64.))
                                    .text_color(c(crate::theme::ACCENT)),
                            ),
                    )
                    .child(
                        div()
                            .id("zip-list")
                            .w_full()
                            .max_h(px(max_h * 0.75))
                            .overflow_y_scroll()
                            .rounded(px(8.))
                            .bg(white_a(0.03))
                            .pt(px(8.))
                            .pr(px(8.))
                            .pb(px(8.))
                            .pl(px(4.))
                            .children(rows)
                            .when(*total as usize > *listed, |d| {
                                d.child(
                                    div()
                                        .pt(px(8.))
                                        .pb(px(4.))
                                        .text_center()
                                        .text_size(px(12.))
                                        .text_color(white_a(0.4))
                                        .child(t::preview_zip_more(*total as usize - listed)),
                                )
                            }),
                    )
                    .into_any_element()
            }
            None => match &self.image {
                Some((image, w, h)) => {
                    let (w, h) = (*w as f32, *h as f32);
                    let s = (inner / w).min(max_h / h).min(2.);
                    let (dw, dh) = ((w * s).round(), (h * s).round());
                    let shown = if self.spoiler {
                        self.blur.clone().unwrap_or(image.clone())
                    } else {
                        image.clone()
                    };
                    let tool = |id: &'static str, icon: &'static str| {
                        div()
                            .id(id)
                            .size(px(34.))
                            .rounded_full()
                            .bg(gpui::black().opacity(0.6))
                            .border_1()
                            .border_color(white_a(0.12))
                            .hover(|s| s.bg(gpui::black().opacity(0.8)))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .child(svg().path(icon).size(px(17.)).text_color(c(0xffffff)))
                    };
                    div()
                        .relative()
                        .w(px(dw))
                        .h(px(dh))
                        .rounded(px(12.))
                        .overflow_hidden()
                        .child(img(shown).size_full().object_fit(gpui::ObjectFit::Fill))
                        .when(self.cropping, |d| d.child(div().absolute().inset_0().bg(gpui::black().opacity(0.5))))
                        .when(self.spoiler && dw >= 80. && dh >= 60., |d| {
                            d.child(
                                div()
                                    .absolute()
                                    .inset_0()
                                    .flex()
                                    .flex_col()
                                    .items_center()
                                    .justify_center()
                                    .gap(px(4.))
                                    .child(
                                        svg()
                                            .path("icons/eye-off.svg")
                                            .size(px(22.))
                                            .text_color(c(0xffffff)),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(13.))
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(c(0xffffff))
                                            .child(t::preview_spoiler()),
                                    ),
                            )
                        })
                        .child(
                            div()
                                .absolute()
                                .bottom(px(10.))
                                .right(px(10.))
                                .flex()
                                .gap(px(8.))
                                .when(self.can_crop(), |d| {
                                    d.child(
                                        tool("crop-open", "ui/crop.svg")
                                            .tip(t::preview_crop())
                                            .on_click(cx.listener(|p, _, window, cx| p.start_crop(window, cx))),
                                    )
                                })
                                .child(
                                    tool("spoiler-toggle", if self.spoiler { "icons/eye-off.svg" } else { "icons/eye.svg" })
                                        .tip(if self.spoiler { t::preview_spoiler_remove() } else { t::preview_spoiler_mark() })
                                        .on_click(cx.listener(|p, _, _, cx| {
                                            p.spoiler = !p.spoiler;
                                            if p.spoiler {
                                                p.make_blur(cx);
                                            }
                                            cx.notify();
                                        })),
                                ),
                        )
                        .into_any_element()
                }
                None if IMAGE_EXTS.contains(&self.ext.as_str()) || SHOWN_ONLY.contains(&self.ext.as_str()) => {
                    div().h(px(100.)).into_any_element()
                }
                None if let Some(v) = self.clip_view(inner, max_h, cx) => v,
                None => div()
                    .size(px(120.))
                    .mb(px(8.))
                    .rounded(px(16.))
                    .bg(c(crate::theme::ACCENT).opacity(0.1))
                    .border_1()
                    .border_color(c(crate::theme::ACCENT).opacity(0.2))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(svg().path(self.icon).size(px(64.)).text_color(c(crate::theme::ACCENT)))
                    .into_any_element(),
            },
        };

        let name = match &self.rename {
            Some(input) => div()
                .flex_1()
                .min_w_0()
                .px(px(8.))
                .py(px(2.))
                .rounded(px(8.))
                .bg(white_a(0.06))
                .border_1()
                .border_color(white_a(0.25))
                .child(input.clone())
                .into_any_element(),
            None => div()
                .id("file-preview-name")
                .min_w_0()
                .flex_shrink(1.)
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .text_size(px(16.))
                .line_height(px(22.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(white_a(0.92))
                .hover(|s| s.text_color(c(0xffffff)))
                .cursor_text()
                .on_click(cx.listener(|p, _, window, cx| p.start_rename(window, cx)))
                .child(self.stem.clone())
                .into_any_element(),
        };
        let saved = self
            .compressed
            .as_ref()
            .and_then(|c| c.peek().cloned().flatten())
            .filter(|c| self.compress && crate::message::savings_percent(c) > 0)
            .map(|c| c.compressed_size);
        let size = match &self.zip {
            Some(Zip::Running(_)) => t::preview_compressing().to_string(),
            Some(Zip::Done { label, .. }) => label.clone(),
            None => crate::media::format_bytes(self.size),
        };
        let info = div()
            .w_full()
            .flex()
            .items_center()
            .gap(px(8.))
            .child(name)
            .when(!self.ext.is_empty(), |d| {
                d.child(
                    div()
                        .flex_shrink_0()
                        .px(px(6.))
                        .rounded(px(5.))
                        .bg(white_a(0.08))
                        .text_size(px(11.))
                        .line_height(px(20.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(white_a(0.6))
                        .child(crate::kit::spaced(self.ext.to_uppercase(), 0.3)),
                )
            })
            .child(div().flex_1())
            .child(
                div()
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .text_size(px(13.))
                    .line_height(px(20.))
                    .child(div().text_color(white_a(if saved.is_some() { 0.4 } else { 0.6 })).child(size))
                    .when_some(saved, |d, bytes| {
                        d.child(div().text_color(white_a(0.4)).child("→")) // i18n-exempt: an arrow
                            .child(div().text_color(white_a(0.85)).child(crate::media::format_bytes(bytes)))
                    }),
            );

        let options = div()
            .w_full()
            .flex()
            .flex_col()
            .gap(px(8.))
            .when(self.compress_offered, |d| {
                let name = if IMAGE_EXTS.contains(&self.ext.as_str()) { t::preview_compress_image() } else { t::preview_compress_video() };
                d.child(
                    option(
                        "fp-compress",
                        name.clone(),
                        name.into_any_element(),
                        self.compress_info.clone(),
                        self.compress,
                    )
                    .on_click(cx.listener(|p, _, _, cx| {
                        p.compress = !p.compress;
                        cx.notify();
                    })),
                )
            })
            .when(self.metadata_offered, |d| {
                let label = div()
                    .flex()
                    .items_center()
                    .child(t::preview_keep_metadata())
                    .when(self.keep_metadata, |d| {
                        d.child(img("icons/warning_pink.png").ml(px(4.)).size(px(14.)))
                    })
                    .into_any_element();
                d.child(
                    option(
                        "fp-metadata",
                        t::preview_keep_metadata(),
                        label,
                        t::preview_keep_metadata_hint(),
                        self.keep_metadata,
                    )
                    .on_click(cx.listener(|p, _, _, cx| {
                        p.keep_metadata = !p.keep_metadata;
                        cx.notify();
                    })),
                )
            });

        let zipping = matches!(self.zip, Some(Zip::Running(_)));
        let me = cx.entity().downgrade();
        let act = move |f: fn(&mut Self, &mut Window, &mut Context<Self>)| {
            let me = me.clone();
            move |_: &gpui::ClickEvent, window: &mut Window, cx: &mut gpui::App| {
                me.update(cx, |p, cx| f(p, window, cx)).ok();
            }
        };
        let publish = self.publish && self.ext.eq_ignore_ascii_case("xdc");
        let buttons = div()
            .w_full()
            .flex()
            .gap(px(8.))
            .children(publish.then(|| {
                div().flex_1().child(crate::security::button("fp-publish", t::preview_publish(), true, true, act(|p, _, cx| cx.emit(Event::Publish(p.path.clone())))))
            }))
            .child(div().flex_1().child(if self.staged {
                crate::security::button("fp-remove", t::preview_remove(), true, true, act(|p, _, cx| {
                    p.forget_clip();
                    cx.emit(Event::Remove)
                }))
                .into_any_element()
            } else {
                crate::security::button("fp-cancel", t::dialog_cancel(), true, true, act(|p, _, cx| p.close(cx))).into_any_element()
            }))
            .child(div().flex_1().child(if self.staged {
                crate::security::button("fp-done", t::preview_done(), false, true, act(|p, _, cx| p.done(cx))).into_any_element()
            } else {
                let busy = zipping || self.too_large || self.held.is_some();
                let label = if zipping { t::preview_compressing() } else if self.too_large { t::preview_too_large() } else if self.held.is_some() { t::preview_sending_when_ready() } else { t::preview_send() };
                crate::security::button("fp-send", label, false, !busy, act(|p, window, cx| p.send(window, cx))).into_any_element()
            }));

        let card = crate::security::card("file-preview-container")
            .w(px(card_w))
            .max_h(px(vh - 32.))
            .p(px(16.))
            .gap(px(14.))
            .items_center()
            .overflow_hidden()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(content)
            .child(info)
            .child(options)
            .child(buttons);

        div()
            .id("file-preview-overlay")
            .track_focus(&self.focus)
            .absolute()
            .inset_0()
            .font_family(INTER)
            .bg(gpui::black().opacity(0.7))
            .backdrop_blur_once(px(6.))
            .flex()
            .items_center()
            .justify_center()
            .on_key_down(cx.listener(|p, e: &KeyDownEvent, window, cx| {
                if p.rename.is_some() || p.crop.is_some() {
                    return;
                }
                match e.keystroke.key.as_str() {
                    "escape" => p.close(cx),
                    "enter" => p.send(window, cx),
                    _ => return,
                }
                cx.stop_propagation();
            }))
            .on_mouse_down(MouseButton::Left, cx.listener(|p, _, _, cx| p.close(cx)))
            .child(card)
            .when_some(self.crop.clone(), |d, crop| {
                d.child(div().absolute().inset_0().on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()).child(crop))
            })
            .child(crate::kit::pump())
            .with_animation(
                "fp-fade",
                gpui::Animation::new(Duration::from_millis(200)),
                |d, t| d.opacity(t),
            )
    }
}

/// `renderTreeNode`: folders first, each toggling open, then files with their sizes.
fn tree_rows(
    node: &Node,
    depth: usize,
    open: &std::collections::HashSet<String>,
    rows: &mut Vec<gpui::AnyElement>,
    cx: &mut Context<FilePreview>,
) {
    if depth > 50 {
        rows.push(
            div()
                .pt(px(8.))
                .pb(px(4.))
                .text_center()
                .text_size(px(12.))
                .text_color(white_a(0.4))
                .child(t::preview_zip_deep())
                .into_any_element(),
        );
        return;
    }
    for d in &node.dirs {
        let has = !d.dirs.is_empty() || !d.files.is_empty();
        let is_open = open.contains(&d.path);
        let path = d.path.clone();
        rows.push(
            div()
                .id(SharedString::from(format!("zip-dir-{}", d.path)))
                .ml(px(depth as f32 * 16.))
                .flex()
                .items_center()
                .gap(px(6.))
                .px(px(4.))
                .py(px(3.))
                .rounded(px(4.))
                .text_size(px(12.))
                .text_color(white_a(0.8))
                .when(has, |r| {
                    r.cursor_pointer()
                        .hover(|s| s.bg(white_a(0.05)))
                        .on_click(cx.listener(move |p, _, _, cx| {
                            if let Some(Zip::Done { open, .. }) = p.zip.as_mut()
                                && !open.remove(&path)
                            {
                                open.insert(path.clone());
                            }
                            cx.notify();
                        }))
                })
                .child(
                    svg()
                        .path("icons/chevron-down.svg")
                        .size(px(12.))
                        .flex_shrink_0()
                        .text_color(white_a(0.35))
                        .with_transformation(crate::kit::turn(
                            format!("zip-chevron-{}", d.path),
                            !is_open,
                            -std::f32::consts::FRAC_PI_2,
                            0.15,
                            crate::kit::css_ease(),
                        )),
                )
                .child(
                    svg()
                        .path("icons/folder.svg")
                        .size(px(14.))
                        .flex_shrink_0()
                        .text_color(white_a(0.5)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .child(d.name.clone()),
                )
                .into_any_element(),
        );
        if is_open {
            tree_rows(d, depth + 1, open, rows, cx);
        }
    }
    for (name, size) in &node.files {
        rows.push(
            div()
                .pl(px(depth as f32 * 16. + 22. + 4.))
                .pr(px(4.))
                .py(px(3.))
                .flex()
                .items_center()
                .gap(px(6.))
                .text_size(px(12.))
                .text_color(white_a(0.7))
                .child(
                    svg()
                        .path("icons/file.svg")
                        .size(px(14.))
                        .flex_shrink_0()
                        .text_color(white_a(0.5)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .child(name.clone()),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .text_size(px(11.))
                        .text_color(white_a(0.4))
                        .child(crate::media::format_bytes(*size)),
                )
                .into_any_element(),
        );
    }
}
