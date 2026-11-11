//! `FilePreview`: what is about to be sent, its name (click to rename), size, the image
//! options and the buttons. The picture compresses in the background while it shows, so a
//! compressed send rarely waits.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures::FutureExt;
use futures::future::Shared;
use gpui::prelude::*;
use gpui::{
    AnimationExt, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight, KeyDownEvent,
    MouseButton, RenderImage, SharedString, Task, Window, div, img, linear_color_stop,
    linear_gradient, px, svg,
};

use crate::composer::{CancelInput, InputChanged, SendText, TextInput};
use crate::message::types::CachedCompressedImage;
use crate::theme::{INTER, PRIMARY, c, white_a};

pub const IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "tiff", "tif", "ico"];
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
    compress_info: SharedString,
    compressed: Option<Compressed>,
    cancel: Arc<AtomicBool>,
    metadata_offered: bool,
    keep_metadata: bool,
    rename: Option<Entity<TextInput>>,
    zip: Option<Zip>,
}

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
        e if IMAGE_EXTS.contains(&e) || e == "svg" => "icons/image.svg",
        "mp4" | "webm" | "mov" | "avi" | "mkv" => "icons/film.svg",
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
    pub fn new(
        path: PathBuf,
        receiver: String,
        reply: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file")
            .to_string();
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();
        let mut stem = if ext.is_empty() {
            name.clone()
        } else {
            name[..name.len() - ext.len() - 1].to_string()
        };
        let spoiler = stem.to_uppercase().starts_with("SPOILER_");
        if spoiler {
            stem = stem[8..].to_string();
        }
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        let is_image = IMAGE_EXTS.contains(&ext.as_str());
        let compress_offered = is_image && size > MIN_COMPRESS_SIZE;
        let cancel = Arc::new(AtomicBool::new(false));

        let compressed = compress_offered.then(|| {
            let (file, flag) = (path.clone(), cancel.clone());
            let task = cx.background_executor().spawn(async move {
                crate::shared::cancel::scoped(flag, || crate::message::precompress(&file)).ok()
            });
            let shared = task.shared();
            let watch = shared.clone();
            cx.spawn(async move |this, cx| {
                let done = watch.await;
                this.update(cx, |p, cx| {
                    p.compress_info = match &done {
                        Some(c) if crate::message::savings_percent(c) > 0 => format!(
                            "~{} ({}% smaller)",
                            crate::media::format_bytes(c.compressed_size),
                            crate::message::savings_percent(c)
                        )
                        .into(),
                        Some(_) => "No significant savings".into(),
                        None => "Couldn't compress this file".into(),
                    };
                    cx.notify();
                })
                .ok();
            })
            .detach();
            shared
        });

        if is_image {
            let file = path.clone();
            let e = ext.clone();
            cx.spawn(async move |this, cx| {
                let decoded = cx
                    .background_executor()
                    .spawn(async move {
                        std::fs::read(&file)
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
            compress_info: "Compressing...".into(),
            compressed,
            cancel,
            metadata_offered: false,
            keep_metadata: false,
            rename: None,
            zip: None,
        }
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
            compress_info: SharedString::default(),
            compressed: None,
            cancel,
            metadata_offered: false,
            keep_metadata: false,
            rename: None,
            zip: Some(Zip::Running(percent)),
        }
    }

    fn zipped_done(
        &mut self,
        done: Result<crate::backend::archive::Zipped, String>,
        cx: &mut Context<Self>,
    ) {
        match done {
            Ok(z) => {
                let plural =
                    |n: u32, one: &str| format!("{n} {one}{}", if n == 1 { "" } else { "s" });
                let mut label = format!(
                    "{} ({}",
                    crate::media::format_bytes(z.size),
                    plural(z.file_count, "file")
                );
                if z.dir_count > 0 {
                    label.push_str(&format!(", {}", plural(z.dir_count, "folder")));
                }
                label.push(')');
                let tree = Node::build(&z.entries);
                let open = tree.dirs.iter().map(|d| d.path.clone()).collect();
                self.path = z.path;
                self.size = z.size;
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
                    let rgba = image::RgbaImage::from_raw(w, h, image.as_bytes(0)?.to_vec())?;
                    let hash = crate::util::generate_thumbhash_from_image(
                        &image::DynamicImage::ImageRgba8(rgba),
                    )?;
                    crate::media::decode_thumbhash(&hash, (w.min(900), h.min(900)))
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
        self.cancel.store(true, Ordering::Relaxed);
        cx.emit(Event::Close(self.path.clone()));
    }

    fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.zip, Some(Zip::Running(_))) {
            return;
        }
        if self.rename.is_some() {
            self.end_rename(true, window, cx);
        }
        let ext = if self.ext.is_empty() {
            String::new()
        } else {
            format!(".{}", self.ext)
        };
        let name = if self.spoiler {
            let stem = if self.stem.is_empty() {
                "image"
            } else {
                &self.stem
            };
            let stem = if stem.to_uppercase().starts_with("SPOILER_") {
                stem.to_string()
            } else {
                format!("SPOILER_{stem}")
            };
            format!("{stem}{ext}")
        } else if self.edited {
            format!("{}{ext}", self.stem)
        } else {
            String::new()
        };
        let compress = self.compress_offered && self.compress;
        if !compress {
            self.cancel.store(true, Ordering::Relaxed);
        }
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
                    linear_color_stop(c(0x33db98), 0.),
                    linear_color_stop(c(0x00ff9d), 1.),
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
    label: gpui::AnyElement,
    sublabel: impl Into<SharedString>,
    on: bool,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .justify_between()
        .px(px(16.))
        .py(px(12.))
        .bg(c(0x171717))
        .border_1()
        .border_color(gpui::rgba(0x47474750))
        .rounded(px(8.))
        .cursor_pointer()
        .child(
            div()
                .child(
                    div()
                        .text_size(px(14.))
                        .line_height(px(16.))
                        .text_color(white_a(0.9))
                        .child(label),
                )
                .child(
                    div()
                        .mt(px(2.))
                        .text_size(px(12.))
                        .line_height(px(16.))
                        .text_color(white_a(0.5))
                        .child(sublabel.into()),
                ),
        )
        .child(neon_toggle(on))
}

impl Render for FilePreview {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let max_h = f32::from(window.viewport_size().height) * 0.4;
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
                            .bg(c(0x59fcb3).opacity(0.1))
                            .border_1()
                            .border_color(c(0x59fcb3).opacity(0.2))
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
                                format!("Compressing... {p}%")
                            } else {
                                "Compressing...".into()
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
                            .bg(c(0x59fcb3).opacity(0.1))
                            .border_1()
                            .border_color(c(0x59fcb3).opacity(0.2))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
                                svg()
                                    .path("icons/folder.svg")
                                    .size(px(64.))
                                    .text_color(c(0x59fcb3)),
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
                                        .child(format!("...and {} more", *total as usize - listed)),
                                )
                            }),
                    )
                    .into_any_element()
            }
            None => match &self.image {
                Some((image, w, h)) => {
                    let (w, h) = (*w as f32, *h as f32);
                    let s = (450. / w).min(max_h / h).min(1.);
                    let (dw, dh) = (w * s, h * s);
                    let shown = if self.spoiler {
                        self.blur.clone().unwrap_or(image.clone())
                    } else {
                        image.clone()
                    };
                    div()
                        .relative()
                        .w(px(dw))
                        .h(px(dh))
                        .rounded(px(8.))
                        .overflow_hidden()
                        .child(img(shown).size_full().rounded(px(8.)))
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
                                            .child("Spoiler"),
                                    ),
                            )
                        })
                        .child(
                            div()
                                .id("spoiler-toggle")
                                .absolute()
                                .bottom(px(8.))
                                .right(px(8.))
                                .size(px(32.))
                                .rounded(px(8.))
                                .bg(gpui::black().opacity(0.55))
                                .hover(|s| s.bg(gpui::black().opacity(0.75)))
                                .flex()
                                .items_center()
                                .justify_center()
                                .cursor_pointer()
                                .on_click(cx.listener(|p, _, _, cx| {
                                    p.spoiler = !p.spoiler;
                                    if p.spoiler {
                                        p.make_blur(cx);
                                    }
                                    cx.notify();
                                }))
                                .child(
                                    svg()
                                        .path(if self.spoiler {
                                            "icons/eye-off.svg"
                                        } else {
                                            "icons/eye.svg"
                                        })
                                        .size(px(18.))
                                        .text_color(c(0xffffff)),
                                ),
                        )
                        .into_any_element()
                }
                None if IMAGE_EXTS.contains(&self.ext.as_str()) => {
                    div().h(px(100.)).into_any_element()
                }
                None => div()
                    .size(px(120.))
                    .mb(px(8.))
                    .rounded(px(16.))
                    .bg(c(0x59fcb3).opacity(0.1))
                    .border_1()
                    .border_color(c(0x59fcb3).opacity(0.2))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(svg().path(self.icon).size(px(64.)).text_color(c(0x59fcb3)))
                    .into_any_element(),
            },
        };

        let name = match &self.rename {
            Some(input) => div()
                .w_full()
                .max_w(px(350.))
                .px(px(8.))
                .py(px(4.))
                .rounded(px(6.))
                .bg(white_a(0.06))
                .border_1()
                .border_color(c(PRIMARY))
                .child(input.clone())
                .into_any_element(),
            None => div()
                .id("file-preview-name")
                .text_size(px(18.))
                .line_height(px(24.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(white_a(0.9))
                .hover(|s| s.text_color(c(PRIMARY)))
                .cursor_pointer()
                .on_click(cx.listener(|p, _, window, cx| p.start_rename(window, cx)))
                .child(self.stem.clone())
                .into_any_element(),
        };
        let info = div()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .items_center()
            .child(
                div()
                    .mb(px(8.))
                    .max_w(px(400.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap(px(8.))
                    .child(name)
                    .when(!self.ext.is_empty(), |d| {
                        d.child(
                            div()
                                .flex_shrink_0()
                                .px(px(6.))
                                .py(px(2.))
                                .rounded(px(4.))
                                .bg(c(PRIMARY).opacity(0.15))
                                .text_size(px(11.))
                                .line_height(px(24.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(c(PRIMARY))
                                .child(crate::kit::spaced(format!(".{}", self.ext), 0.3)),
                        )
                    }),
            )
            .child(
                div()
                    .text_size(px(14.))
                    .line_height(px(24.))
                    .text_color(white_a(0.6))
                    .child(match &self.zip {
                        Some(Zip::Running(_)) => "Compressing...".to_string(),
                        Some(Zip::Done { label, .. }) => label.clone(),
                        None => crate::media::format_bytes(self.size),
                    }),
            );

        let options = div()
            .flex_shrink_0()
            .w_full()
            .max_w(px(300.))
            .flex()
            .flex_col()
            .gap(px(12.))
            .when(self.compress_offered, |d| {
                d.child(
                    option(
                        "fp-compress",
                        "Compress Image".into_any_element(),
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
                    .child("Keep Metadata")
                    .when(self.keep_metadata, |d| {
                        d.child(img("icons/warning.svg").ml(px(4.)).size(px(14.)))
                    })
                    .into_any_element();
                d.child(
                    option(
                        "fp-metadata",
                        label,
                        "Includes location, camera & date",
                        self.keep_metadata,
                    )
                    .on_click(cx.listener(|p, _, _, cx| {
                        p.keep_metadata = !p.keep_metadata;
                        cx.notify();
                    })),
                )
            });

        let zipping = matches!(self.zip, Some(Zip::Running(_)));
        let button = |id: &'static str, label: &'static str| {
            div()
                .id(id)
                .px(px(32.))
                .py(px(12.))
                .rounded(px(8.))
                .text_size(px(14.))
                .line_height(px(17.))
                .font_weight(FontWeight::MEDIUM)
                .cursor_pointer()
                .child(label)
        };
        let buttons = div()
            .flex_shrink_0()
            .mt_auto()
            .flex()
            .gap(px(12.))
            .child(
                button("fp-cancel", "Cancel")
                    .bg(white_a(0.1))
                    .hover(|s| s.bg(white_a(0.15)))
                    .text_color(white_a(0.9))
                    .on_click(cx.listener(|p, _, _, cx| p.close(cx))),
            )
            .child(
                button("fp-send", if zipping { "Compressing..." } else { "Send" })
                    .when(zipping, |d| d.opacity(0.5).cursor_default())
                    .bg(linear_gradient(
                        90.,
                        linear_color_stop(c(0x33db98), 0.),
                        linear_color_stop(c(0x00ff9d), 1.),
                    ))
                    .hover(|s| s.opacity(0.9))
                    .text_color(c(0x000000))
                    .on_click(cx.listener(|p, _, window, cx| p.send(window, cx))),
            );

        let vh = f32::from(window.viewport_size().height);
        let vw = f32::from(window.viewport_size().width);
        let card = div()
            .id("file-preview-container")
            .w(px(500f32.min(vw - 40.)))
            .h(px(650f32.min(vh - 40.)))
            .p(px(24.))
            .bg(c(0x0a0a0a))
            .border_1()
            .border_color(c(PRIMARY))
            .rounded(px(16.))
            .flex()
            .flex_col()
            .items_center()
            .gap(px(16.))
            .overflow_hidden()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(16.))
                    .child(
                        div()
                            .w_full()
                            .flex()
                            .flex_col()
                            .items_center()
                            .gap(px(12.))
                            .child(content),
                    )
                    .child(info)
                    .child(options),
            )
            .child(buttons);

        div()
            .id("file-preview-overlay")
            .track_focus(&self.focus)
            .absolute()
            .inset_0()
            .font_family(INTER)
            .bg(gpui::black().opacity(0.85))
            .flex()
            .items_center()
            .justify_center()
            .on_key_down(cx.listener(|p, e: &KeyDownEvent, window, cx| {
                if p.rename.is_some() {
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
                .child("Deeply nested...")
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
                        .with_transformation(gpui::Transformation::rotate(gpui::radians(
                            if is_open {
                                0.
                            } else {
                                -std::f32::consts::FRAC_PI_2
                            },
                        ))),
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
