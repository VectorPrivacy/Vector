//! The pack creator (`PackCreator.svelte`, `CreatorOverlays.svelte`, `pack-creator.js`): the
//! in-panel editor for one of our own packs, and the overlays the picker raises over itself
//! for it (confirm, progress, naming, error). Uploads and the publish wait for the exit, so an
//! abandoned edit costs no network.

use std::collections::HashMap;
use std::sync::Arc;

use futures::channel::oneshot;
use gpui::{AnyElement, Context, Entity, ExternalPaths, FontWeight, IntoElement, ParentElement, RenderImage, ScrollHandle, SharedString, Styled, Window, div, linear_color_stop, linear_gradient, prelude::*, px, svg};
use crate::pic::img;
use vector_core::image_crop::CropRect;

use super::*;
use crate::backend::packs::{IMPORT_EXTS, MAX_EMOJI_BYTES};
use crate::composer::{CancelInput, InputChanged, Look, SendText, TextInput};

const NAME_MAX: usize = 26;
const SHORTCODE_MAX: usize = 22;
/// The grid's cell: six across the 337px column, 4px apart.
const CELL_SIDE: f32 = (337. - 4. * 5.) / 6.;
const DANGER: u32 = 0xff2ea9;

/// A crop the creator needs drawn: the picture, its natural size, and where the answer goes.
pub struct CropFor(
    pub Arc<RenderImage>,
    pub (u32, u32),
    pub std::cell::RefCell<Option<oneshot::Sender<Option<CropRect>>>>,
);
/// A creator cell's context menu, at the pointer.
pub struct CellMenu(pub usize, pub gpui::Point<gpui::Pixels>);
/// A save that failed after the picker closed: show it again, with why.
pub struct ShowAgain;

#[derive(Clone, Copy, PartialEq)]
enum Busy {
    Pending,
    Uploading,
    Deleting,
}

struct Entry {
    id: u64,
    shortcode: String,
    /// Where it lives once published.
    url: Option<String>,
    /// Picked and not yet uploaded.
    bytes: Option<Arc<Vec<u8>>>,
    preview: Option<Arc<RenderImage>>,
    /// The published picture's local source.
    src: Option<String>,
    broken: Option<String>,
}

pub struct Creator {
    /// The pack being edited: its id and `d` identifier.
    editing: Option<(String, String)>,
    name: Entity<TextInput>,
    logo_url: Option<String>,
    logo_src: Option<String>,
    logo_bytes: Option<Arc<Vec<u8>>>,
    logo_preview: Option<Arc<RenderImage>>,
    logo_dead: bool,
    emojis: Vec<Entry>,
    next: u64,
    /// Published files a save should delete: removed cells and a replaced logo.
    pending_deletes: Vec<String>,
    saving: bool,
    dirty: bool,
    busy: HashMap<u64, Busy>,
    hovered: Option<u64>,
    dragover: bool,
    max: usize,
    scroll: ScrollHandle,
}

struct Naming {
    preview: Option<Arc<RenderImage>>,
    src: Option<String>,
    input: Entity<TextInput>,
    edit: bool,
    batch: Option<(usize, usize)>,
    error: Option<String>,
    /// The entry being named keeps its own shortcode past the collision check.
    own: Option<u64>,
    done: Option<oneshot::Sender<Option<String>>>,
}

struct Confirm {
    title: &'static str,
    detail: &'static str,
    ok: &'static str,
    done: Option<oneshot::Sender<bool>>,
}

pub struct ErrorBox {
    pretitle: String,
    title: String,
    detail: String,
    button: String,
}

#[derive(Default)]
pub struct Overlays {
    confirm: Option<Confirm>,
    progress: Option<(String, String)>,
    naming: Option<Naming>,
    error: Option<ErrorBox>,
}

fn sanitize(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .take(SHORTCODE_MAX)
        .collect()
}

fn decode_preview(bytes: &[u8]) -> Option<Arc<RenderImage>> {
    let ext =
        vector_core::crypto::extension_from_mime(vector_core::crypto::mime_from_magic_bytes(bytes));
    crate::media::decode(bytes, &ext, (160, 160)).map(|(i, _, _)| Arc::new(i))
}

impl EmojiPicker {
    fn own_pack_count(&self) -> usize {
        self.packs.len()
    }

    fn max_equipped(&self) -> usize {
        if self.live() {
            vector_core::emoji_packs::effective_max_equipped_packs()
        } else {
            10
        }
    }

    fn live(&self) -> bool {
        self.media.as_ref().is_some_and(|(_, l)| *l)
    }

    /// `openEmojiPackCreator`: a new pack, or one of ours by id.
    pub fn open_creator(
        &mut self,
        id: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let pack = id
            .as_ref()
            .and_then(|id| self.packs.iter().find(|p| p.id == *id && p.own));
        if pack.is_none() && self.own_pack_count() >= self.max_equipped() {
            let max = self.max_equipped();
            self.show_error(
                "Pack Slots Full!",
                &format!("Vector supports up to {max} equipped packs. Remove one to add another."),
                "Remove a Pack First.",
                "GOT IT",
                cx,
            );
            return;
        }
        let look = Look {
            family: INTER,
            weight: FontWeight::SEMIBOLD,
            size: px(15.),
            line_height: px(19.),
            color: c(0xffffff),
            placeholder: white_a(0.4),
            ..Default::default()
        };
        let name = cx.new(|cx| {
            TextInput::new("Pack name", cx)
                .with_look(look)
                .max_chars(NAME_MAX)
                .keep_on_submit()
        });
        cx.subscribe(&name, |this, _, _: &InputChanged, cx| {
            if let Some(cr) = this.creator.as_mut() {
                cr.dirty = true;
            }
            cx.notify();
        })
        .detach();
        let mut cr = Creator {
            editing: None,
            name: name.clone(),
            logo_url: None,
            logo_src: None,
            logo_bytes: None,
            logo_preview: None,
            logo_dead: false,
            emojis: Vec::new(),
            next: 0,
            pending_deletes: Vec::new(),
            saving: false,
            dirty: false,
            busy: HashMap::new(),
            hovered: None,
            dragover: false,
            max: if self.live() {
                vector_core::emoji_packs::effective_max_emojis_per_pack()
            } else {
                30
            },
            scroll: ScrollHandle::new(),
        };
        if let Some(p) = pack {
            cr.editing = Some((p.id.clone(), p.identifier.clone()));
            name.update(cx, |n, cx| n.set_text(&p.title, cx));
            cr.logo_url = p.logo_url.clone();
            cr.logo_src = p.logo.clone();
            for e in &p.emojis {
                let id = cr.next;
                cr.next += 1;
                cr.emojis.push(Entry {
                    id,
                    shortcode: e.code.split('~').next().unwrap_or(&e.code).to_string(),
                    url: Some(e.url.clone()),
                    bytes: None,
                    preview: None,
                    src: Some(e.src.clone()),
                    broken: None,
                });
            }
        }
        let verify = self.live() && cr.editing.is_some();
        let urls: Vec<String> = cr
            .emojis
            .iter()
            .filter_map(|e| e.url.clone())
            .chain(cr.logo_url.clone())
            .collect();
        self.creator = Some(cr);
        window.focus(&name.read(cx).focus_handle(cx), cx);
        if verify {
            let rx = crate::backend::run(crate::backend::packs::gone(urls));
            cx.spawn(async move |this, cx| {
                let Ok(gone) = rx.await else { return };
                this.update(cx, |this, cx| {
                    let Some(cr) = this.creator.as_mut() else {
                        return;
                    };
                    for e in cr
                        .emojis
                        .iter_mut()
                        .filter(|e| e.url.as_ref().is_some_and(|u| gone.contains(u)))
                    {
                        e.broken = Some(gone_message().into());
                    }
                    cr.logo_dead = cr.logo_url.as_ref().is_some_and(|u| gone.contains(u));
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
        cx.notify();
    }

    /// `closeEmojiPackCreator`: save what changed, then back to the sections at the pack.
    pub fn close_creator(&mut self, cx: &mut Context<Self>) -> gpui::Task<bool> {
        let Some(cr) = self.creator.as_ref() else {
            return gpui::Task::ready(true);
        };
        if cr.saving {
            return gpui::Task::ready(false);
        }
        let dirty = cr.dirty;
        let editing = cr.editing.as_ref().map(|(id, _)| id.clone());
        let save = dirty.then(|| self.save(cx));
        cx.spawn(async move |this, cx| {
            let saved = match save {
                Some(t) => t.await,
                None => Ok(editing),
            };
            let Ok(landing) = saved else { return false };
            this.update(cx, |this, cx| {
                this.creator = None;
                if let Some(i) = landing.and_then(|id| this.packs.iter().position(|p| p.id == id)) {
                    this.collapsed.remove(&Section::Pack(i));
                    this.rebuild();
                    this.jump(Section::Pack(i), cx);
                }
                cx.notify();
            })
            .ok();
            true
        })
    }

    /// The picker went away: a dirty editor saves on its own, and a failure brings the panel back.
    pub fn leave_creator(&mut self, cx: &mut Context<Self>) {
        if self.creator.is_none() {
            return;
        }
        let task = self.close_creator(cx);
        cx.spawn(async move |this, cx| {
            if !task.await {
                this.update(cx, |this, cx| {
                    if this.overlays.error.is_some() {
                        cx.emit(ShowAgain);
                    }
                })
                .ok();
            }
        })
        .detach();
    }

    pub fn show_error(
        &mut self,
        pretitle: &str,
        detail: &str,
        title: &str,
        button: &str,
        cx: &mut Context<Self>,
    ) {
        self.overlays.error = Some(ErrorBox {
            pretitle: pretitle.into(),
            title: if title.is_empty() {
                "Please Try Again.".into()
            } else {
                title.into()
            },
            detail: detail.into(),
            button: if button.is_empty() {
                "TRY AGAIN".into()
            } else {
                button.into()
            },
        });
        cx.notify();
    }

    fn confirm(
        &mut self,
        title: &'static str,
        detail: &'static str,
        ok: &'static str,
        cx: &mut Context<Self>,
    ) -> oneshot::Receiver<bool> {
        let (tx, rx) = oneshot::channel();
        if let Some(mut old) = self.overlays.confirm.take()
            && let Some(done) = old.done.take()
        {
            let _ = done.send(false);
        }
        self.overlays.confirm = Some(Confirm {
            title,
            detail,
            ok,
            done: Some(tx),
        });
        cx.notify();
        rx
    }

    fn confirm_finish(&mut self, ok: bool, cx: &mut Context<Self>) {
        if let Some(mut cf) = self.overlays.confirm.take()
            && let Some(done) = cf.done.take()
        {
            let _ = done.send(ok);
        }
        cx.notify();
    }

    /// The naming overlay; answers the new shortcode, or nothing on Skip or Cancel.
    fn ask_name(
        &mut self,
        entry: u64,
        batch: Option<(usize, usize)>,
        edit: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> oneshot::Receiver<Option<String>> {
        let (tx, rx) = oneshot::channel();
        if let Some(mut old) = self.overlays.naming.take()
            && let Some(done) = old.done.take()
        {
            let _ = done.send(None);
        }
        let Some(e) = self
            .creator
            .as_ref()
            .and_then(|cr| cr.emojis.iter().find(|e| e.id == entry))
        else {
            let _ = tx.send(None);
            return rx;
        };
        let look = Look {
            family: INTER,
            weight: FontWeight::MEDIUM,
            size: px(14.),
            line_height: px(17.),
            color: c(0xffffff),
            placeholder: white_a(0.38),
            ..Default::default()
        };
        let initial = e.shortcode.clone();
        let input = cx.new(|cx| {
            let mut t = TextInput::new("shortcode", cx)
                .with_look(look)
                .centered()
                .max_chars(SHORTCODE_MAX)
                .keep_on_submit()
                .submit_empty();
            t.set_text(&initial, cx);
            t
        });
        cx.subscribe(&input, |this, _, _: &SendText, cx| this.naming_commit(cx))
            .detach();
        cx.subscribe(&input, |this, _, _: &CancelInput, cx| {
            this.naming_finish(None, cx)
        })
        .detach();
        cx.subscribe(&input, |this, _, _: &InputChanged, cx| {
            if let Some(n) = this.overlays.naming.as_mut() {
                n.error = None;
            }
            cx.notify();
        })
        .detach();
        window.focus(&input.read(cx).focus_handle(cx), cx);
        self.overlays.naming = Some(Naming {
            preview: e.preview.clone(),
            src: e.src.clone(),
            input,
            edit,
            batch,
            error: None,
            own: Some(entry),
            done: Some(tx),
        });
        cx.notify();
        rx
    }

    fn naming_commit(&mut self, cx: &mut Context<Self>) {
        let Some(n) = self.overlays.naming.as_mut() else {
            return;
        };
        let sc = sanitize(n.input.read(cx).text());
        if sc.is_empty() {
            n.error = Some("Letters, numbers, and underscores only.".into());
            cx.notify();
            return;
        }
        let own = n.own;
        let taken = self.creator.as_ref().is_some_and(|cr| {
            cr.emojis
                .iter()
                .any(|e| Some(e.id) != own && sanitize(&e.shortcode) == sc)
        });
        if taken {
            if let Some(n) = self.overlays.naming.as_mut() {
                n.error = Some(format!(":{sc}: is already used in this pack."));
            }
            self.show_error(
                "Oops! Name Already Taken!",
                &format!(
                    ":{sc}: is already used by another emoji in this pack. Pick a different name."
                ),
                "Duplicate Shortcode.",
                "GOT IT",
                cx,
            );
            return;
        }
        self.naming_finish(Some(sc), cx);
    }

    fn naming_finish(&mut self, value: Option<String>, cx: &mut Context<Self>) {
        if let Some(mut n) = self.overlays.naming.take()
            && let Some(done) = n.done.take()
        {
            let _ = done.send(value);
        }
        cx.notify();
    }

    fn pick_emojis(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .creator
            .as_ref()
            .is_none_or(|cr| cr.saving || cr.emojis.len() >= cr.max)
        {
            return;
        }
        let rx = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: None,
        });
        let window = window.window_handle();
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else {
                return;
            };
            window
                .update(cx, |_, window, cx| {
                    this.update(cx, |this, cx| this.add_paths(paths, window, cx))
                })
                .ok();
        })
        .detach();
    }

    /// Ask the app to draw a square crop; `None` when cancelled.
    async fn crop(
        this: &gpui::WeakEntity<Self>,
        bytes: &[u8],
        cx: &mut gpui::AsyncApp,
    ) -> Option<CropRect> {
        let b = bytes.to_vec();
        let decoded = cx
            .background_executor()
            .spawn(async move {
                let ext = vector_core::crypto::extension_from_mime(
                    vector_core::crypto::mime_from_magic_bytes(&b),
                );
                crate::media::decode(&b, &ext, (2048, 2048))
            })
            .await?;
        let (image, w, h) = decoded;
        let (tx, rx) = oneshot::channel();
        this.update(cx, |_, cx| {
            cx.emit(CropFor(
                Arc::new(image),
                (w, h),
                std::cell::RefCell::new(Some(tx)),
            ))
        })
        .ok()?;
        rx.await.ok().flatten()
    }

    /// `_pcAddFiles`: each file checked, squared if it must be, then named.
    pub fn add_paths(
        &mut self,
        paths: Vec<std::path::PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let window = window.window_handle();
        cx.spawn(async move |this, cx| {
            let (mut format, mut size, mut crop_size, mut failed) = (false, false, false, 0usize);
            let mut added = Vec::new();
            for path in paths {
                let full = this.update(cx, |this, _| this.creator.as_ref().is_none_or(|cr| cr.emojis.len() >= cr.max)).unwrap_or(true);
                if full {
                    break;
                }
                let ext = path.extension().and_then(|e| e.to_str()).unwrap_or_default().to_ascii_lowercase();
                if !IMPORT_EXTS.contains(&ext.as_str()) {
                    format = true;
                    continue;
                }
                let p = path.clone();
                let Ok(bytes) = cx.background_executor().spawn(async move { crate::backend::packs::import(&p) }).await else {
                    failed += 1;
                    continue;
                };
                if bytes.len() > MAX_EMOJI_BYTES {
                    size = true;
                    continue;
                }
                let Some((w, h)) = crate::backend::packs::dims(&bytes) else {
                    format = true;
                    continue;
                };
                let bytes = if w != h {
                    let Some(rect) = Self::crop(&this, &bytes, cx).await else { continue };
                    match cx.background_executor().spawn(async move { crate::crop::crop_emoji(bytes, rect, MAX_EMOJI_BYTES) }).await {
                        Ok(out) if out.len() > MAX_EMOJI_BYTES => {
                            crop_size = true;
                            continue;
                        }
                        Ok(out) => out,
                        Err(e) => {
                            this.update(cx, |this, cx| this.show_error("Oops! Couldn’t Crop That!", &e, "", "", cx)).ok();
                            continue;
                        }
                    }
                } else {
                    bytes
                };
                let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("emoji").to_string();
                let b = bytes.clone();
                let preview = cx.background_executor().spawn(async move { decode_preview(&b) }).await;
                let id = this
                    .update(cx, |this, cx| {
                        let cr = this.creator.as_mut()?;
                        let id = cr.next;
                        cr.next += 1;
                        let shortcode = cr.shortcode_from(&stem);
                        cr.emojis.push(Entry { id, shortcode, url: None, bytes: Some(Arc::new(bytes)), preview, src: None, broken: None });
                        cr.dirty = true;
                        cx.notify();
                        Some(id)
                    })
                    .ok()
                    .flatten();
                added.extend(id);
            }
            this.update(cx, |this, cx| {
                if size {
                    this.show_error("Oops! File Size Exceeded!", "File Size must be under 256Kb.", "", "", cx);
                } else if crop_size {
                    this.show_error(
                        "Oops! Too Big After Cropping!",
                        "Cropping this to a square re-encoded it over the 256 KB limit. Try an already-square version, or shrink it a little before adding.",
                        "Crop Pushed It Over.",
                        "GOT IT",
                        cx,
                    );
                } else if format {
                    this.show_error("Oops! Unsupported Format!", "Use PNG, JPG, GIF, or WebP.", "", "", cx);
                } else if failed > 0 {
                    let what = if failed == 1 { "That image couldn’t be imported.".to_string() } else { format!("{failed} images couldn’t be imported.") };
                    this.show_error("Oops! Couldn’t Read That!", &format!("{what} Try PNG, JPG, GIF, or WebP."), "Unsupported Image.", "GOT IT", cx);
                }
            })
            .ok();
            let total = added.len();
            for (i, id) in added.into_iter().enumerate() {
                let rx = window
                    .update(cx, |_, window, cx| {
                        this.update(cx, |this, cx| {
                            if this.creator.as_ref().is_none_or(|cr| !cr.emojis.iter().any(|e| e.id == id)) {
                                return None;
                            }
                            Some(this.ask_name(id, Some((i + 1, total)), false, window, cx))
                        })
                        .ok()
                        .flatten()
                    })
                    .ok()
                    .flatten();
                let Some(rx) = rx else { continue };
                if let Ok(Some(name)) = rx.await {
                    this.update(cx, |this, cx| {
                        if let Some(e) = this.creator.as_mut().and_then(|cr| cr.emojis.iter_mut().find(|e| e.id == id)) {
                            e.shortcode = name;
                        }
                        if let Some(cr) = this.creator.as_mut() {
                            cr.dirty = true;
                        }
                        cx.notify();
                    })
                    .ok();
                }
            }
        })
        .detach();
    }

    fn pick_logo(&mut self, cx: &mut Context<Self>) {
        if self.creator.as_ref().is_none_or(|cr| cr.saving) {
            return;
        }
        let rx = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: None,
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else { return };
            let Some(path) = paths.into_iter().next() else { return };
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or_default().to_ascii_lowercase();
            let error = |this: &gpui::WeakEntity<Self>, cx: &mut gpui::AsyncApp, pre: &str, detail: &str| {
                this.update(cx, |this, cx| this.show_error(pre, detail, "", "", cx)).ok();
            };
            if !IMPORT_EXTS.contains(&ext.as_str()) {
                return error(&this, cx, "Oops! Unsupported Format!", "Use PNG, JPG, GIF, or WebP.");
            }
            let p = path.clone();
            let Ok(bytes) = cx.background_executor().spawn(async move { crate::backend::packs::import(&p) }).await else {
                return error(&this, cx, "Oops! Unsupported Format!", "Use PNG, JPG, GIF, or WebP.");
            };
            if bytes.len() > MAX_EMOJI_BYTES {
                return error(&this, cx, "Oops! File Size Exceeded!", "File Size must be under 256Kb.");
            }
            let Some((w, h)) = crate::backend::packs::dims(&bytes) else {
                return error(&this, cx, "Oops! Unsupported Format!", "Use PNG, JPG, GIF, or WebP.");
            };
            let bytes = if w != h {
                let Some(rect) = Self::crop(&this, &bytes, cx).await else { return };
                match cx.background_executor().spawn(async move { crate::crop::crop_emoji(bytes, rect, MAX_EMOJI_BYTES) }).await {
                    Ok(out) if out.len() <= MAX_EMOJI_BYTES => out,
                    Ok(_) => {
                        this.update(cx, |this, cx| {
                            this.show_error(
                                "Oops! Too Big After Cropping!",
                                "Cropping this to a square re-encoded it over the 256 KB limit. Try an already-square version, or shrink it a little before adding.",
                                "Crop Pushed It Over.",
                                "GOT IT",
                                cx,
                            )
                        })
                        .ok();
                        return;
                    }
                    Err(e) => return error(&this, cx, "Oops! Couldn’t Crop That!", &e),
                }
            } else {
                bytes
            };
            let b = bytes.clone();
            let preview = cx.background_executor().spawn(async move { decode_preview(&b) }).await;
            this.update(cx, |this, cx| {
                let Some(cr) = this.creator.as_mut() else { return };
                if let Some(old) = cr.logo_url.take() {
                    cr.pending_deletes.push(old);
                }
                cr.logo_src = None;
                cr.logo_dead = false;
                cr.logo_bytes = Some(Arc::new(bytes));
                cr.logo_preview = preview;
                cr.dirty = true;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub fn creator_remove(&mut self, idx: usize, cx: &mut Context<Self>) {
        let Some(cr) = self.creator.as_mut().filter(|cr| !cr.saving) else {
            return;
        };
        if idx >= cr.emojis.len() {
            return;
        }
        let e = cr.emojis.remove(idx);
        if let (Some(url), None) = (e.url, e.bytes) {
            cr.pending_deletes.push(url);
        }
        cr.hovered = None;
        cr.dirty = true;
        cx.notify();
    }

    pub fn creator_rename(&mut self, idx: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(e) = self.creator.as_ref().and_then(|cr| cr.emojis.get(idx)) else {
            return;
        };
        if let Some(reason) = e.broken.clone() {
            self.broken_error(&reason, cx);
            return;
        }
        let (id, old) = (e.id, e.shortcode.clone());
        let rx = self.ask_name(id, None, true, window, cx);
        cx.spawn(async move |this, cx| {
            let Ok(Some(name)) = rx.await else { return };
            if name == old {
                return;
            }
            this.update(cx, |this, cx| {
                let Some(cr) = this.creator.as_mut() else {
                    return;
                };
                if let Some(e) = cr.emojis.iter_mut().find(|e| e.id == id) {
                    e.shortcode = name;
                    cr.dirty = true;
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn broken_error(&mut self, reason: &str, cx: &mut Context<Self>) {
        if reason.to_lowercase().contains("too large") {
            self.show_error(
                "Oops! Emoji Too Large!",
                "This emoji is over the 1 MB size limit, so it cannot be used. Compress it under 1 MB, then delete this one and upload the smaller version.",
                "Compress & Re-upload.",
                "GOT IT",
                cx,
            );
        } else {
            self.show_error(
                "Oops! Emoji Unavailable!",
                "This emoji has been deleted by its host. Re-uploading will fix it: delete this emoji, then upload it again.",
                "Delete It & Re-upload.",
                "GOT IT",
                cx,
            );
        }
    }

    /// Move the cell `from` to where `to` stands.
    fn creator_move(&mut self, from: u64, to: u64, cx: &mut Context<Self>) {
        let Some(cr) = self.creator.as_mut().filter(|cr| !cr.saving) else {
            return;
        };
        let (Some(a), Some(b)) = (
            cr.emojis.iter().position(|e| e.id == from),
            cr.emojis.iter().position(|e| e.id == to),
        ) else {
            return;
        };
        if a == b {
            return;
        }
        let e = cr.emojis.remove(a);
        cr.emojis.insert(b, e);
        cr.dirty = true;
        cx.notify();
    }

    /// `_pcSave`: upload what's local, publish, then clean up what the pack no longer holds.
    /// Answers the pack to land on, or `Err` with the editor kept open and the reason shown.
    fn save(&mut self, cx: &mut Context<Self>) -> gpui::Task<Result<Option<String>, ()>> {
        let live = self.live();
        let Some(cr) = self.creator.as_mut() else {
            return gpui::Task::ready(Ok(None));
        };
        let name: String = cr
            .name
            .read(cx)
            .text()
            .trim()
            .chars()
            .take(NAME_MAX)
            .collect();
        if cr.emojis.is_empty() {
            if cr.editing.is_none() && name.is_empty() && cr.logo_bytes.is_none() {
                cr.dirty = false;
                return gpui::Task::ready(Ok(None));
            }
            self.show_error("Oops! Add an Emoji First!", "A pack needs at least one emoji. Add one to save it, or delete it to discard the draft.", "", "", cx);
            return gpui::Task::ready(Err(()));
        }
        if name.is_empty() {
            self.show_error(
                "Oops! Name Your Pack First!",
                "Give it a name to save it, or delete it to discard the draft.",
                "",
                "",
                cx,
            );
            return gpui::Task::ready(Err(()));
        }
        let mut seen = std::collections::HashSet::new();
        let jobs: Vec<(u64, String, Option<String>, Option<Arc<Vec<u8>>>)> = cr
            .emojis
            .iter()
            .filter_map(|e| {
                let sc = sanitize(&e.shortcode);
                (!sc.is_empty() && seen.insert(sc.clone()))
                    .then(|| (e.id, sc, e.url.clone(), e.bytes.clone()))
            })
            .collect();
        if jobs.is_empty() {
            self.show_error(
                "Oops! Your Emojis Need Names!",
                "Give each emoji a name, then save.",
                "",
                "",
                cx,
            );
            return gpui::Task::ready(Err(()));
        }
        let Some(cr) = self.creator.as_mut() else {
            return gpui::Task::ready(Ok(None));
        };
        let editing = cr.editing.clone();
        let identifier = editing
            .as_ref()
            .filter(|(id, _)| self.packs.iter().any(|p| p.id == *id && p.own))
            .map(|(_, d)| d.clone());
        let editing_id = editing.map(|(id, _)| id);
        if !live {
            cr.dirty = false;
            return gpui::Task::ready(Ok(editing_id));
        }
        cr.saving = true;
        for (id, _, _, bytes) in &jobs {
            if bytes.is_some() {
                cr.busy.insert(*id, Busy::Pending);
            }
        }
        let logo = (cr.logo_url.clone(), cr.logo_bytes.clone());
        let others: std::collections::HashSet<String> = self
            .packs
            .iter()
            .filter(|p| Some(&p.id) != editing_id.as_ref())
            .flat_map(|p| {
                p.emojis
                    .iter()
                    .map(|e| e.url.clone())
                    .chain(p.logo_url.clone())
            })
            .collect();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result: Result<Option<String>, String> = async {
                let mut logo_url = logo.0.unwrap_or_default();
                if let Some(bytes) = logo.1 {
                    logo_url = upload_retrying(bytes, true).await?;
                    let u = logo_url.clone();
                    this.update(cx, |this, _| {
                        if let Some(cr) = this.creator.as_mut() {
                            cr.logo_url = Some(u);
                            cr.logo_bytes = None;
                        }
                    })
                    .ok();
                }
                let mut emojis = Vec::new();
                for (id, sc, url, bytes) in jobs {
                    let url = match bytes {
                        Some(b) => {
                            this.update(cx, |this, cx| {
                                if let Some(cr) = this.creator.as_mut() {
                                    cr.busy.insert(id, Busy::Uploading);
                                }
                                cx.notify();
                            })
                            .ok();
                            let url = upload_retrying(b, false).await?;
                            let u = url.clone();
                            this.update(cx, |this, cx| {
                                if let Some(cr) = this.creator.as_mut() {
                                    cr.busy.remove(&id);
                                    if let Some(e) = cr.emojis.iter_mut().find(|e| e.id == id) {
                                        e.url = Some(u);
                                        e.bytes = None;
                                    }
                                }
                                cx.notify();
                            })
                            .ok();
                            url
                        }
                        None => url.unwrap_or_default(),
                    };
                    if !url.is_empty() {
                        emojis.push((sc, url));
                    }
                }
                let kept: std::collections::HashSet<String> = emojis.iter().map(|(_, u)| u.clone()).chain(Some(logo_url.clone())).collect();
                let pack = crate::backend::run(crate::backend::packs::publish(identifier, name, logo_url, emojis)).await.map_err(|_| "Cancelled".to_string())??;
                let queue: Vec<String> = this
                    .update(cx, |this, _| this.creator.as_mut().map(|cr| std::mem::take(&mut cr.pending_deletes)).unwrap_or_default())
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|u| !kept.contains(u) && !others.contains(u))
                    .collect();
                for url in queue {
                    let _ = crate::backend::run(crate::backend::packs::delete_blob(url));
                }
                Ok(Some(pack.id))
            }
            .await;
            this.update(cx, |this, cx| {
                if let Some(cr) = this.creator.as_mut() {
                    cr.saving = false;
                    cr.busy.clear();
                    if result.is_ok() {
                        cr.dirty = false;
                    }
                }
                if let Err(e) = &result {
                    this.show_error(
                        "Couldn’t Save Pack",
                        &format!("Your emojis are safe, nothing was lost. An upload didn’t finish (usually a passing network glitch). Tap Done to retry.\n\n({e})"),
                        "Your Progress Is Safe.",
                        "GOT IT",
                        cx,
                    );
                }
                cx.notify();
            })
            .ok();
            result.map_err(|_| ())
        })
    }

    /// `_pcDelete`: discard a draft, or delete a published pack and its files.
    fn creator_delete(&mut self, cx: &mut Context<Self>) {
        let Some(cr) = self.creator.as_ref().filter(|cr| !cr.saving) else {
            return;
        };
        let editing = cr.editing.clone();
        let live = self.live();
        let rx = if editing.is_none() {
            self.confirm(
                "Discard This Pack?",
                "This pack was never saved. The emojis you added will be lost.",
                "DISCARD",
                cx,
            )
        } else {
            self.confirm(
                "Delete This Pack?",
                "This action permanently removes the emoji files from your media servers and the pack from Nostr. This action cannot be undone.",
                "DELETE",
                cx,
            )
        };
        cx.spawn(async move |this, cx| {
            if !rx.await.unwrap_or(false) {
                return;
            }
            let Ok((cells, logo, extras)) = this.update(cx, |this, cx| {
                let others: std::collections::HashSet<String> = this
                    .packs
                    .iter()
                    .filter(|p| Some(&p.id) != editing.as_ref().map(|(id, _)| id))
                    .flat_map(|p| {
                        p.emojis
                            .iter()
                            .map(|e| e.url.clone())
                            .chain(p.logo_url.clone())
                    })
                    .collect();
                let Some(cr) = this.creator.as_mut() else {
                    return (Vec::new(), None, Vec::new());
                };
                let cells: Vec<(u64, String)> = cr
                    .emojis
                    .iter()
                    .filter_map(|e| e.url.clone().map(|u| (e.id, u)))
                    .filter(|(_, u)| !others.contains(u))
                    .collect();
                let logo = cr.logo_url.clone().filter(|u| !others.contains(u));
                let extras: Vec<String> = std::mem::take(&mut cr.pending_deletes)
                    .into_iter()
                    .filter(|u| !others.contains(u))
                    .collect();
                if editing.is_some() && live {
                    cr.saving = true;
                    let total = cells.len() + usize::from(logo.is_some()) + extras.len();
                    let detail = if total > 0 {
                        format!(
                            "Removing {total} file{} from media servers…",
                            if total == 1 { "" } else { "s" }
                        )
                    } else {
                        "Removing from Nostr…".into()
                    };
                    this.overlays.progress = Some(("Deleting Pack".into(), detail));
                    cx.notify();
                }
                (cells, logo, extras)
            }) else {
                return;
            };
            if !live {
                this.update(cx, |this, cx| {
                    this.creator = None;
                    cx.notify();
                })
                .ok();
                return;
            }
            let Some((id, _)) = editing else {
                for url in cells.into_iter().map(|(_, u)| u).chain(extras).chain(logo) {
                    let _ = crate::backend::run(crate::backend::packs::delete_blob(url));
                }
                this.update(cx, |this, cx| {
                    this.creator = None;
                    cx.notify();
                })
                .ok();
                return;
            };
            let total = cells.len() + usize::from(logo.is_some()) + extras.len();
            let mut done = 0;
            let progress =
                |this: &gpui::WeakEntity<Self>, cx: &mut gpui::AsyncApp, detail: String| {
                    this.update(cx, |this, cx| {
                        if let Some(p) = this.overlays.progress.as_mut() {
                            p.1 = detail;
                        }
                        cx.notify();
                    })
                    .ok();
                };
            for (cell, url) in cells {
                done += 1;
                progress(
                    &this,
                    cx,
                    format!("Deleting emoji {done} of {total} from media servers…"),
                );
                this.update(cx, |this, _| {
                    if let Some(cr) = this.creator.as_mut() {
                        cr.busy.insert(cell, Busy::Deleting);
                    }
                })
                .ok();
                let _ = crate::backend::run(crate::backend::packs::delete_blob(url)).await;
                this.update(cx, |this, _| {
                    if let Some(cr) = this.creator.as_mut() {
                        cr.busy.remove(&cell);
                    }
                })
                .ok();
            }
            if let Some(url) = logo {
                done += 1;
                progress(
                    &this,
                    cx,
                    format!("Deleting pack icon ({done} of {total})…"),
                );
                let _ = crate::backend::run(crate::backend::packs::delete_blob(url)).await;
            }
            if !extras.is_empty() {
                let n = extras.len();
                progress(
                    &this,
                    cx,
                    format!(
                        "Cleaning up {n} orphan file{}…",
                        if n == 1 { "" } else { "s" }
                    ),
                );
                for url in extras {
                    let _ = crate::backend::run(crate::backend::packs::delete_blob(url)).await;
                }
            }
            progress(&this, cx, "Removing pack from Nostr…".into());
            let res = crate::backend::run(crate::backend::packs::delete(id))
                .await
                .unwrap_or_else(|_| Err("Cancelled".into()));
            this.update(cx, |this, cx| {
                this.overlays.progress = None;
                if let Some(cr) = this.creator.as_mut() {
                    cr.saving = false;
                    cr.busy.clear();
                }
                match res {
                    Ok(()) => this.creator = None,
                    Err(e) => log::warn!("pack delete failed: {e}"),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The creator in the grid's place: head, cells, dropzone.
    pub fn creator_body(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let media = self.media.clone();
        let Some(cr) = self.creator.as_ref() else {
            return div().into_any_element();
        };
        let emoji_src = |src: &Option<String>, icon: bool| -> Option<crate::media::Custom> {
            src.as_ref()
                .and_then(|s| media.as_ref().map(|(m, _)| m.borrow_mut().custom(s, icon)))
        };
        let at_cap = cr.emojis.len() >= cr.max;
        let saving = cr.saving;

        let logo_face: AnyElement = if let Some(p) = &cr.logo_preview {
            img(gpui::ImageSource::Render(p.clone()))
                .size_full()
                .map(|i| gpui::StyledImage::object_fit(i, gpui::ObjectFit::Cover))
                .into_any_element()
        } else if cr.logo_url.is_some() && !cr.logo_dead {
            match emoji_src(&cr.logo_src, true) {
                Some(crate::media::Custom::Ready(i)) => img(gpui::ImageSource::Render(i))
                    .size_full()
                    .map(|i| gpui::StyledImage::object_fit(i, gpui::ObjectFit::Cover))
                    .into_any_element(),
                _ => div().into_any_element(),
            }
        } else if cr.logo_dead {
            div()
                .text_size(px(16.))
                .text_color(c(0xf0a500))
                .child("\u{26A0}")
                .into_any_element()
        } else {
            svg()
                .path("icons/image.svg")
                .size(px(20.))
                .text_color(white_a(0.55))
                .into_any_element()
        };
        let has_logo = cr.logo_preview.is_some() || (cr.logo_url.is_some() && !cr.logo_dead);
        let logo = div()
            .id("creator-logo")
            .size(px(26.))
            .flex_shrink_0()
            .rounded(px(6.))
            .overflow_hidden()
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .when(!has_logo, |d| {
                d.border_1()
                    .border_dashed()
                    .border_color(white_a(0.18))
                    .bg(white_a(0.03))
                    .hover(|s| s.border_color(c(PRIMARY)))
            })
            .on_click(cx.listener(|this, _, _, cx| this.pick_logo(cx)))
            .child(logo_face);
        let icon_btn = |id: &'static str, icon: &'static str, size: f32, color: gpui::Hsla| {
            div()
                .id(id)
                .when_some(
                    match id {
                        "creator-done" => Some("Save Pack"),
                        "creator-delete" => Some("Remove Pack"),
                        _ => None,
                    },
                    |d, t| d.tooltip(crate::kit::tooltip(t)),
                )
                .size(px(28.))
                .flex_shrink_0()
                .rounded(px(6.))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .hover(|s| s.bg(white_a(0.06)))
                .child(svg().path(icon).size(px(size)).text_color(color))
        };
        let done: AnyElement = if saving {
            div()
                .size(px(28.))
                .ml(px(4.))
                .flex()
                .items_center()
                .justify_center()
                .child(div().size(px(14.)).child(crate::attachments::spinner(
                    14.,
                    2.5,
                    0.9,
                    0.28,
                    white_a(0.18),
                    c(PRIMARY),
                )))
                .into_any_element()
        } else {
            icon_btn("creator-done", "icons/edit.svg", 20., white_a(0.7))
                .ml(px(4.))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.close_creator(cx).detach();
                }))
                .into_any_element()
        };
        let head = div()
            .flex_shrink_0()
            .pt(px(4.))
            .pb(px(10.))
            .pr(px(6.))
            .flex()
            .items_center()
            .gap(px(6.))
            .child(logo)
            .child(div().flex_1().min_w_0().py(px(4.)).child(cr.name.clone()))
            .child(
                div()
                    .flex_shrink_0()
                    .text_size(px(11.))
                    .text_color(white_a(0.45))
                    .child(format!("({}/{})", cr.emojis.len(), cr.max)),
            )
            .child(done)
            .child(
                icon_btn("creator-delete", "icons/trash.svg", 20., c(DANGER))
                    .on_click(cx.listener(|this, _, _, cx| this.creator_delete(cx))),
            );

        let mut cells: Vec<AnyElement> = Vec::new();
        for (idx, e) in cr.emojis.iter().enumerate() {
            let id = e.id;
            let hovered = cr.hovered == Some(id) && !saving;
            let face: AnyElement = if e.broken.is_some() {
                div()
                    .text_size(px(19.))
                    .text_color(c(0xf0a500))
                    .child("\u{26A0}")
                    .into_any_element()
            } else if let Some(p) = &e.preview {
                img(gpui::ImageSource::Render(p.clone()))
                    .size(px(CELL_SIDE * 0.7))
                    .into_any_element()
            } else {
                match emoji_src(&e.src, false) {
                    Some(crate::media::Custom::Ready(i)) => img(gpui::ImageSource::Render(i))
                        .size(px(CELL_SIDE * 0.7))
                        .into_any_element(),
                    _ => div().into_any_element(),
                }
            };
            let busy = cr.busy.get(&id).copied();
            let shortcode: SharedString = format!(":{}:", e.shortcode).into();
            cells.push(
                div()
                    .id(SharedString::from(format!("creator-cell-{id}")))
                    .relative()
                    .flex_1()
                    .min_w_0()
                    .h(px(CELL_SIDE))
                    .rounded(px(8.))
                    .bg(white_a(0.03))
                    .border_1()
                    .border_color(if hovered {
                        white_a(0.18)
                    } else {
                        white_a(0.05)
                    })
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .when(saving, |d| d.opacity(0.6))
                    .tooltip(crate::kit::tooltip(shortcode))
                    .on_hover(cx.listener(move |this, on: &bool, _, cx| {
                        if let Some(cr) = this.creator.as_mut() {
                            if *on {
                                cr.hovered = Some(id);
                            } else if cr.hovered == Some(id) {
                                cr.hovered = None;
                            }
                        }
                        cx.notify();
                    }))
                    .when(!saving, |d| {
                        d.on_click(cx.listener(move |this, _, window, cx| {
                            if let Some(i) = this
                                .creator
                                .as_ref()
                                .and_then(|cr| cr.emojis.iter().position(|e| e.id == id))
                            {
                                this.creator_rename(i, window, cx);
                            }
                        }))
                        .on_mouse_down(
                            gpui::MouseButton::Right,
                            cx.listener(move |this, ev: &gpui::MouseDownEvent, _, cx| {
                                if let Some(i) = this
                                    .creator
                                    .as_ref()
                                    .and_then(|cr| cr.emojis.iter().position(|e| e.id == id))
                                {
                                    cx.emit(CellMenu(i, ev.position));
                                }
                            }),
                        )
                        .on_drag(DragCell(id, e.preview.clone()), |d, _, _, cx| {
                            cx.new(|_| d.clone())
                        })
                        .drag_over::<DragCell>(|s, _, _, _| s.border_color(c(PRIMARY)))
                        .on_drop(cx.listener(
                            move |this, d: &DragCell, _, cx| this.creator_move(d.0, id, cx),
                        ))
                    })
                    .child(face)
                    .when(hovered, |d| {
                        d.child(
                            div()
                                .id(SharedString::from(format!("creator-x-{id}")))
                                .absolute()
                                .top(px(-4.))
                                .right(px(-6.))
                                .size(px(20.))
                                .rounded_full()
                                .bg(c(0x0a0a0a))
                                .flex()
                                .items_center()
                                .justify_center()
                                .cursor_pointer()
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.creator_remove(idx, cx);
                                }))
                                .child(
                                    svg()
                                        .path("icons/x.svg")
                                        .size(px(10.))
                                        .text_color(c(DANGER)),
                                ),
                        )
                    })
                    .children(busy.map(|b| {
                        div()
                            .absolute()
                            .inset_0()
                            .rounded(px(8.))
                            .bg(if b == Busy::Deleting {
                                ca(60, 12, 20, 0.65)
                            } else {
                                ca(0, 0, 0, 0.55)
                            })
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(match b {
                                Busy::Pending => div()
                                    .size(px(22.))
                                    .rounded_full()
                                    .border(px(2.5))
                                    .border_color(white_a(0.2))
                                    .opacity(0.4)
                                    .into_any_element(),
                                Busy::Uploading => div()
                                    .size(px(22.))
                                    .child(crate::attachments::spinner(
                                        22.,
                                        2.5,
                                        1.,
                                        0.3,
                                        white_a(0.2),
                                        white_a(0.95),
                                    ))
                                    .into_any_element(),
                                Busy::Deleting => div()
                                    .size(px(22.))
                                    .child(crate::attachments::spinner(
                                        22.,
                                        2.5,
                                        1.,
                                        0.3,
                                        ca(255, 46, 169, 0.25),
                                        c(DANGER),
                                    ))
                                    .into_any_element(),
                            })
                    }))
                    .into_any_element(),
            );
        }
        let mut grid = div().pt(px(8.)).mb(px(12.)).flex().flex_col().gap(px(4.));
        let mut it = cells.into_iter().peekable();
        while it.peek().is_some() {
            let mut row = div().flex().gap(px(4.));
            let mut n = 0;
            for cell in it.by_ref().take(6) {
                row = row.child(cell);
                n += 1;
            }
            for _ in n..6 {
                row = row.child(div().flex_1().min_w_0());
            }
            grid = grid.child(row);
        }

        let dz_disabled = saving || at_cap;
        let dragover = cr.dragover;
        let dropzone = div()
            .id("creator-dropzone")
            .w_full()
            .flex_1()
            .min_h(px(78.))
            .mb(px(12.))
            .rounded(px(10.))
            .border(px(1.5))
            .border_dashed()
            .border_color(if dragover { c(PRIMARY) } else { white_a(0.18) })
            .bg(if dragover {
                ca(89, 252, 179, 0.06)
            } else {
                white_a(0.02)
            })
            .text_color(if dragover { c(PRIMARY) } else { white_a(0.7) })
            .flex()
            .items_center()
            .justify_center()
            .gap(px(4.))
            .text_size(px(13.))
            .font_weight(FontWeight::MEDIUM)
            .when(dz_disabled, |d| d.opacity(0.4))
            .when(!dz_disabled, |d| {
                d.cursor_pointer()
                    .hover(|s| {
                        s.border_color(c(PRIMARY))
                            .text_color(c(PRIMARY))
                            .bg(ca(89, 252, 179, 0.06))
                    })
                    .on_click(cx.listener(|this, _, window, cx| this.pick_emojis(window, cx)))
                    .drag_over::<ExternalPaths>(|s, _, _, _| {
                        s.border_color(c(PRIMARY))
                            .text_color(c(PRIMARY))
                            .bg(ca(89, 252, 179, 0.06))
                    })
                    .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                        cx.stop_propagation();
                        this.add_paths(paths.paths().to_vec(), window, cx);
                    }))
            })
            .child(
                svg()
                    .path("icons/plus-circle.svg")
                    .size(px(20.))
                    .text_color(if dragover { c(PRIMARY) } else { white_a(0.7) }),
            )
            .child(if at_cap {
                format!("Maximum {} reached", cr.max)
            } else {
                "Upload Emoji".into()
            });

        div()
            .id("creator-scroll")
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&cr.scroll)
            .font_family(INTER)
            .text_color(c(0xffffff))
            .child(
                div()
                    .min_h(px(264.))
                    .flex()
                    .flex_col()
                    .child(head)
                    .child(grid)
                    .child(dropzone),
            )
            .into_any_element()
    }

    /// The overlays over the whole panel, the last raised on top.
    pub fn overlays(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let media = self.media.clone();
        let mut out = Vec::new();
        let shell = |id: &'static str, border: gpui::Hsla| {
            div()
                .id(id)
                .occlude()
                .absolute()
                .inset_0()
                .rounded(px(12.))
                .overflow_hidden()
                .border_2()
                .border_color(border)
                .flex()
                .items_center()
                .justify_center()
                .p(px(24.))
                .font_family(INTER)
        };
        let glow = |color: gpui::Hsla| {
            div().absolute().inset_0().bg(linear_gradient(
                0.,
                linear_color_stop(color, 0.),
                linear_color_stop(gpui::transparent_black(), 0.38),
            ))
        };
        let btn = |id: &'static str, label: SharedString| {
            div()
                .id(id)
                .px(px(28.))
                .py(px(12.))
                .rounded(px(10.))
                .border_1()
                .text_size(px(14.))
                .font_weight(FontWeight::BOLD)
                .cursor_pointer()
                .active(|s| s.opacity(0.8))
                .child(label)
        };
        if let Some(cf) = &self.overlays.confirm {
            out.push(
                shell("pc-confirm", c(DANGER))
                    .bg(ca(8, 8, 8, 0.82))
                    .child(glow(ca(255, 46, 169, 0.22)))
                    .child(
                        div()
                            .relative()
                            .w_full()
                            .max_w(px(320.))
                            .flex()
                            .flex_col()
                            .items_center()
                            .child(img("icons/vector_warning.svg").size(px(100.)).mb(px(10.)))
                            .child(
                                div()
                                    .mb(px(6.))
                                    .text_size(px(18.))
                                    .font_weight(FontWeight::EXTRA_BOLD)
                                    .text_color(c(DANGER))
                                    .child(cf.title),
                            )
                            .child(
                                div()
                                    .mb(px(18.))
                                    .text_size(px(12.))
                                    .line_height(px(18.))
                                    .text_center()
                                    .text_color(c(0xffffff))
                                    .child(cf.detail),
                            )
                            .child(
                                div()
                                    .flex()
                                    .gap(px(8.))
                                    .child(
                                        btn("pc-confirm-cancel", "CANCEL".into())
                                            .bg(ca(3, 3, 3, 0.56))
                                            .border_color(c(DANGER))
                                            .text_color(c(0xffffff))
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.confirm_finish(false, cx)
                                            })),
                                    )
                                    .child(
                                        btn("pc-confirm-ok", cf.ok.into())
                                            .bg(c(DANGER))
                                            .border_color(c(DANGER))
                                            .text_color(c(0x000000))
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.confirm_finish(true, cx)
                                            })),
                                    ),
                            ),
                    )
                    .into_any_element(),
            );
        }
        if let Some((title, detail)) = &self.overlays.progress {
            out.push(
                shell("pc-progress", white_a(0.08))
                    .bg(ca(8, 8, 8, 0.86))
                    .child(
                        div()
                            .max_w(px(280.))
                            .flex()
                            .flex_col()
                            .items_center()
                            .child(div().size(px(56.)).mb(px(14.)).child(
                                crate::attachments::spinner(
                                    56.,
                                    5.,
                                    1.1,
                                    0.28,
                                    white_a(0.18),
                                    c(PRIMARY),
                                ),
                            ))
                            .child(
                                div()
                                    .mb(px(4.))
                                    .text_size(px(16.))
                                    .font_weight(FontWeight::BOLD)
                                    .text_color(c(0xffffff))
                                    .child(title.clone()),
                            )
                            .child(
                                div()
                                    .min_h(px(12.))
                                    .text_size(px(12.))
                                    .text_center()
                                    .text_color(white_a(0.65))
                                    .child(detail.clone()),
                            ),
                    )
                    .into_any_element(),
            );
        }
        if let Some(n) = &self.overlays.naming {
            let ready = n.preview.clone().or_else(|| {
                match n
                    .src
                    .as_ref()
                    .and_then(|s| media.as_ref().map(|(m, _)| m.borrow_mut().custom(s, false)))
                {
                    Some(crate::media::Custom::Ready(p)) => Some(p),
                    _ => None,
                }
            });
            let preview: AnyElement = match ready {
                Some(p) => img(gpui::ImageSource::Render(p))
                    .size(px(72.))
                    .into_any_element(),
                None => div().into_any_element(),
            };
            let invalid = n.error.is_some();
            out.push(
                shell("pc-naming", c(PRIMARY))
                    .bg(ca(8, 8, 8, 0.9))
                    .child(glow(ca(89, 252, 179, 0.18)))
                    .child(
                        div()
                            .relative()
                            .w_full()
                            .max_w(px(320.))
                            .flex()
                            .flex_col()
                            .items_center()
                            .children(n.batch.filter(|(_, t)| *t > 1).map(|(i, t)| {
                                div()
                                    .mb(px(8.))
                                    .text_size(px(11.))
                                    .font_weight(FontWeight::BOLD)
                                    .text_color(c(PRIMARY))
                                    .child(format!("{i} OF {t}"))
                            }))
                            .child(
                                div()
                                    .size(px(96.))
                                    .mb(px(12.))
                                    .rounded(px(14.))
                                    .bg(white_a(0.04))
                                    .border_1()
                                    .border_color(white_a(0.08))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .overflow_hidden()
                                    .child(preview),
                            )
                            .child(
                                div()
                                    .mb(px(10.))
                                    .text_size(px(16.))
                                    .font_weight(FontWeight::BOLD)
                                    .text_color(c(0xffffff))
                                    .child(if n.edit {
                                        "Rename Emoji"
                                    } else {
                                        "Name This Emoji"
                                    }),
                            )
                            .child(
                                div()
                                    .mb(px(4.))
                                    .flex()
                                    .items_center()
                                    .gap(px(4.))
                                    .child(
                                        div()
                                            .text_size(px(18.))
                                            .font_weight(FontWeight::BOLD)
                                            .text_color(white_a(0.4))
                                            .child(":"),
                                    )
                                    .child(
                                        div()
                                            .w(px(200.))
                                            .px(px(12.))
                                            .py(px(8.))
                                            .rounded(px(8.))
                                            .border_1()
                                            .border_color(if invalid {
                                                c(DANGER)
                                            } else {
                                                c(PRIMARY)
                                            })
                                            .bg(if invalid {
                                                ca(255, 46, 169, 0.12)
                                            } else {
                                                c(0x030303)
                                            })
                                            .child(n.input.clone()),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(18.))
                                            .font_weight(FontWeight::BOLD)
                                            .text_color(white_a(0.4))
                                            .child(":"),
                                    ),
                            )
                            .children(n.error.clone().map(|e| {
                                div()
                                    .mt(px(4.))
                                    .mb(px(12.))
                                    .text_size(px(11.))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(c(DANGER))
                                    .child(e)
                            }))
                            .child(
                                div()
                                    .mt(px(12.))
                                    .flex()
                                    .gap(px(8.))
                                    .child(
                                        btn(
                                            "pc-naming-skip",
                                            if n.edit { "CANCEL" } else { "SKIP" }.into(),
                                        )
                                        .bg(ca(3, 3, 3, 0.56))
                                        .border_color(white_a(0.2))
                                        .text_color(c(0xffffff))
                                        .on_click(
                                            cx.listener(|this, _, _, cx| {
                                                this.naming_finish(None, cx)
                                            }),
                                        ),
                                    )
                                    .child(
                                        btn("pc-naming-save", "SAVE".into())
                                            .bg(c(PRIMARY))
                                            .border_color(c(PRIMARY))
                                            .text_color(c(0x000000))
                                            .on_click(
                                                cx.listener(|this, _, _, cx| {
                                                    this.naming_commit(cx)
                                                }),
                                            ),
                                    ),
                            ),
                    )
                    .into_any_element(),
            );
        }
        if let Some(er) = &self.overlays.error {
            out.push(
                shell("pc-error", c(DANGER))
                    .bg(ca(8, 8, 8, 0.85))
                    .child(glow(ca(255, 46, 169, 0.32)))
                    .child(
                        div()
                            .relative()
                            .w_full()
                            .max_w(px(320.))
                            .flex()
                            .flex_col()
                            .items_center()
                            .child(img("icons/aggroboi.webp").size(px(96.)).mb(px(10.)))
                            .child(
                                div()
                                    .mt(px(4.))
                                    .mb(px(2.))
                                    .text_size(px(13.))
                                    .font_weight(FontWeight::BOLD)
                                    .text_color(c(DANGER))
                                    .child(er.pretitle.clone()),
                            )
                            .child(
                                div()
                                    .mb(px(6.))
                                    .text_size(px(26.))
                                    .line_height(px(28.6))
                                    .font_weight(FontWeight::EXTRA_BOLD)
                                    .text_color(c(0xffffff))
                                    .text_center()
                                    .child(er.title.clone()),
                            )
                            .child(
                                div()
                                    .mb(px(16.))
                                    .text_size(px(12.))
                                    .text_center()
                                    .text_color(white_a(0.65))
                                    .child(er.detail.clone()),
                            )
                            .child(
                                btn("pc-error-ok", er.button.clone().into())
                                    .bg(c(DANGER))
                                    .border_color(c(DANGER))
                                    .text_color(c(0x000000))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.overlays.error = None;
                                        cx.notify();
                                    })),
                            ),
                    )
                    .into_any_element(),
            );
        }
        out
    }
}

impl Creator {
    /// `_pcShortcodeFromFilename`: the file's stem, made unique within the pack.
    fn shortcode_from(&self, stem: &str) -> String {
        let mut sc = sanitize(stem);
        if sc.is_empty() {
            sc = "emoji".into();
        }
        if !self.emojis.iter().any(|e| e.shortcode == sc) {
            return sc;
        }
        (2..)
            .map(|i| format!("{sc}_{i}"))
            .find(|c| !self.emojis.iter().any(|e| e.shortcode == *c))
            .unwrap_or(sc)
    }
}

fn gone_message() -> &'static str {
    "This emoji is no longer available (it may have been deleted)."
}

async fn upload_retrying(bytes: Arc<Vec<u8>>, icon: bool) -> Result<String, String> {
    let mut last = String::new();
    for attempt in 0..3u64 {
        match crate::backend::run(crate::backend::packs::upload(bytes.clone(), icon)).await {
            Ok(Ok(url)) => return Ok(url),
            Ok(Err(e)) => {
                let m = e.to_lowercase();
                let permanent = [
                    "max is",
                    "is empty",
                    "account swap",
                    "no blossom",
                    "too large",
                    "too detailed",
                    "couldn't be read",
                ]
                .iter()
                .any(|p| m.contains(p));
                last = e;
                if permanent {
                    break;
                }
            }
            Err(_) => return Err("Cancelled".into()),
        }
        let wait = 400 * (attempt + 1);
        let _ = crate::backend::run(async move {
            tokio::time::sleep(std::time::Duration::from_millis(wait)).await
        })
        .await;
    }
    Err(last)
}

/// A creator cell being dragged, drawn small under the pointer.
#[derive(Clone)]
pub struct DragCell(u64, Option<Arc<RenderImage>>);

impl Render for DragCell {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size(px(CELL_SIDE * 0.6))
            .rounded(px(8.))
            .bg(ca(40, 40, 40, 0.92))
            .opacity(0.9)
            .flex()
            .items_center()
            .justify_center()
            .children(
                self.1
                    .clone()
                    .map(|p| img(gpui::ImageSource::Render(p)).size(px(CELL_SIDE * 0.42))),
            )
    }
}
