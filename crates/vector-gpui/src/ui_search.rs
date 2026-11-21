//! Message search's lifecycle: opened from the keyboard or a chat header, the corpus read in
//! the background, each keystroke a fresh scan, and a result opened where it was said.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{AnyElement, AppContext as _, Context, Focusable as _, SharedString, Window, px};
use vector_core::search::Has;

use super::VectorApp;
use crate::backend;
use crate::composer::{CancelInput, InputChanged, Nav, TextInput};
use crate::search::{Act, Directory, Search, Strip, Suggest, Thumb};
use crate::t;

impl VectorApp {
    /// Opens search over every chat, or over the open one when `here`.
    pub(super) fn open_search(&mut self, here: bool, window: &mut Window, cx: &mut Context<Self>) {
        let dir = Directory::read(&self.store.borrow());
        let scope = here
            .then(|| self.open.clone())
            .flatten()
            .and_then(|id| dir.chats.iter().find(|c| c.id == id.as_ref()).cloned());
        if let Some(s) = self.search.as_ref() {
            let focus = s.input.focus_handle(cx);
            window.focus(&focus, cx);
            if scope.is_none() {
                return;
            }
        }
        let placeholder: SharedString = match &scope {
            Some(c) => t::search_placeholder_in(c.name.split(" · ").next().unwrap_or(&c.name).to_string()),
            None => t::search_placeholder(),
        };
        let input = cx.new(|cx| {
            let mut i = TextInput::new(placeholder, cx).with_look(crate::composer::Look {
                size: gpui::px(17.),
                ..crate::kit::field_look()
            });
            i.set_nav(true);
            if let Some(c) = &scope {
                let value = c.name.clone();
                let text = crate::search::completion(&Suggest::Value {
                    key: crate::search::Key::In,
                    value,
                    label: SharedString::default(),
                    npub: None,
                });
                i.set_text(&text, cx);
            }
            i
        });
        cx.subscribe_in(&input, window, |app, _, _: &InputChanged, _, cx| app.search_changed(cx)).detach();
        cx.subscribe_in(&input, window, |app, _, nav: &Nav, window, cx| {
            let Some(s) = app.search.as_mut() else { return };
            match nav {
                Nav::Up => s.step(false),
                Nav::Down => s.step(true),
                Nav::Pick => {
                    let sel = s.sel;
                    let act = if s.showing_suggestions() { Act::Suggest(sel) } else { Act::Pick(sel) };
                    return app.on_search(act, window, cx);
                }
                Nav::Close => return app.on_search(Act::Close, window, cx),
            }
            cx.notify();
        })
        .detach();
        cx.subscribe_in(&input, window, |app, _, _: &CancelInput, window, cx| app.on_search(Act::Close, window, cx)).detach();
        window.focus(&input.focus_handle(cx), cx);
        let reuse = self.search.as_mut().and_then(|s| s.corpus.take());
        self.search = Some(Search::new(input, dir));
        self.close_popups(cx);
        match reuse {
            Some(corpus) => self.search_loaded(corpus, cx),
            None => self.search_load(cx),
        }
        self.search_changed(cx);
    }

    fn search_load(&mut self, cx: &mut Context<Self>) {
        let (names, blocked) = {
            let store = self.store.borrow();
            (crate::search::names(&store), crate::search::blocked(&store))
        };
        if !self.live {
            let corpus = Arc::new(crate::search::fixture_corpus(&self.store.borrow()));
            return self.search_loaded(corpus, cx);
        }
        let rx = backend::run(async move {
            let records = vector_core::search::load()?;
            Ok::<_, String>(Arc::new(crate::search::build(&records, &names, &blocked)))
        });
        cx.spawn(async move |this, cx| {
            let res = rx.await;
            this.update(cx, |this, cx| match res {
                Ok(Ok(corpus)) => this.search_loaded(corpus, cx),
                Ok(Err(e)) => this.show_toast(&e, cx),
                Err(_) => {}
            })
            .ok();
        })
        .detach();
    }

    fn search_loaded(&mut self, corpus: Arc<vector_core::search::Corpus>, cx: &mut Context<Self>) {
        if let Some(s) = self.search.as_mut() {
            s.corpus = Some(corpus);
        }
        self.search_changed(cx);
    }

    fn search_changed(&mut self, cx: &mut Context<Self>) {
        let now = self.store.borrow().now_ms();
        let Some(s) = self.search.as_mut() else { return };
        let (text, caret) = {
            let i = s.input.read(cx);
            (i.text().to_string(), i.caret())
        };
        s.refresh(&text, caret, now);
        cx.notify();
    }

    pub(super) fn on_search(&mut self, act: Act, window: &mut Window, cx: &mut Context<Self>) {
        let Some(s) = self.search.as_mut() else { return };
        match act {
            Act::Close => {
                // The plaintext corpus goes with the panel.
                self.search = None;
                self.release_search_thumbs(window, cx);
                window.focus(&self.input.read(cx).focus_handle(cx), cx);
            }
            Act::Hover(i) => {
                if s.sel == i {
                    return;
                }
                s.sel = i;
            }
            Act::Sort => {
                s.oldest = !s.oldest;
                return self.search_changed(cx);
            }
            Act::Suggest(i) => {
                let Some(sg) = s.suggest.get(i).cloned() else { return };
                let range = s.suggest_at.clone();
                let input = s.input.clone();
                input.update(cx, |inp, cx| inp.replace(range, &crate::search::completion(&sg), window, cx));
                return self.search_changed(cx);
            }
            Act::Unfilter(range) => {
                let input = s.input.clone();
                input.update(cx, |inp, cx| {
                    let text = inp.text().to_string();
                    let end = text[range.end..].find(|c: char| !c.is_whitespace()).map_or(text.len(), |n| range.end + n);
                    inp.replace(range.start..end, "", window, cx);
                });
                let focus = input.focus_handle(cx);
                window.focus(&focus, cx);
                return self.search_changed(cx);
            }
            Act::View(open) => return self.open_viewer(open, window, cx),
            Act::Pick(i) => {
                let Some(hit) = s.found.hits.get(i).cloned() else { return };
                self.search = None;
                self.release_search_thumbs(window, cx);
                self.settings = None;
                self.profile = None;
                self.close_nexus(cx);
                self.new_chat = None;
                self.create_community = None;
                if self.is_slim {
                    self.slim.drawer = false;
                }
                self.open_chat(SharedString::from(hit.chat), window, cx);
                self.jump_or_say(hit.id, t::msg_jump_not_local(), cx);
            }
        }
        cx.notify();
    }

    pub(super) fn search_view(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let thumbs = self.search_thumbs(window, cx);
        let s = self.search.as_ref()?;
        let app = cx.entity().downgrade();
        let on: crate::search::OnAct = Rc::new(move |act, window, cx| {
            app.update(cx, |app, cx| app.on_search(act, window, cx)).ok();
        });
        let vp = window.viewport_size();
        let store = self.store.borrow();
        Some(crate::search::view(s, &store, &thumbs, (f32::from(vp.width), f32::from(vp.height)), self.is_slim, cx, &on))
    }

    /// Thumbnails for the media results on screen or near it: only those decode, and a message
    /// no open chat holds is read once for its attachments. Nothing downloads for search.
    fn search_thumbs(&mut self, window: &Window, cx: &mut Context<Self>) -> HashMap<usize, Strip> {
        let mut out = HashMap::new();
        let live = self.live;
        let Some(s) = self.search.as_mut() else { return out };
        if s.showing_suggestions() {
            return out;
        }
        let area = s.scroll.bounds();
        let top = area.top() - s.scroll.offset().y - px(400.);
        let bottom = area.bottom() - s.scroll.offset().y + px(400.);
        let scale = window.scale_factor();
        let store = self.store.borrow();
        let mut media = self.media.borrow_mut();
        let mut wanted = Vec::new();
        // Rows are the chats' headers and the results, as the view lays them out.
        let mut row = 0;
        for (i, hit) in s.found.hits.iter().enumerate() {
            if i == 0 || s.found.hits[i - 1].chat != hit.chat {
                row += 1;
            }
            row += 1;
            if !(hit.has.contains(Has::IMAGE) || hit.has.contains(Has::VIDEO)) {
                continue;
            }
            let near = match s.scroll.bounds_for_item(row - 1) {
                Some(b) => b.bottom() > top && b.top() < bottom,
                None => i < 12,
            };
            if !near {
                continue;
            }
            let held = store.chat(&hit.chat).and_then(|c| c.messages.iter().find(|m| m.id == hit.id)).map(|m| &m.attachments);
            let read = s.atts.get(&hit.id).and_then(Option::as_ref);
            let Some(atts) = held.or(read) else {
                if live && !s.atts.contains_key(&hit.id) {
                    s.atts.insert(hit.id.clone(), None);
                    wanted.push((hit.chat.clone(), hit.id.clone()));
                }
                continue;
            };
            let visual: Vec<_> = atts.iter().filter(|a| visual(a)).collect();
            let thumbs: Vec<Thumb> = visual.iter().take(crate::search::THUMBS).filter_map(|a| thumb(&mut media, &store.media_root, a, scale)).collect();
            if !thumbs.is_empty() {
                out.insert(i, Strip { more: visual.len() - thumbs.len(), thumbs });
            }
        }
        drop((store, media));
        for (chat, id) in wanted {
            let rx = backend::run(backend::messaging::messages_around(chat, id.clone(), 0, 0));
            cx.spawn(async move |this, cx| {
                let found = rx.await.ok().and_then(Result::ok).and_then(|ms| ms.into_iter().find(|m| m.id == id));
                this.update(cx, |this, cx| {
                    if let Some(s) = this.search.as_mut() {
                        s.atts.insert(id, Some(found.map(|m| m.attachments).unwrap_or_default()));
                        cx.notify();
                    }
                })
                .ok();
            })
            .detach();
        }
        out
    }

    /// The member pane's field changed: members filter as before, messages scan the Community.
    pub(super) fn pane_find_changed(&mut self, cx: &mut Context<Self>) {
        let now = self.store.borrow().now_ms();
        let Some(v) = self.community.as_mut() else { return };
        let (text, caret) = {
            let i = v.search.read(cx);
            (i.text().to_string(), i.caret())
        };
        if text.trim().is_empty() {
            // Read afresh on the next search, so it sees what arrived meanwhile.
            v.find.corpus = None;
            v.find.loading = false;
            v.find.refresh(&text, caret, now);
            return cx.notify();
        }
        if v.find.corpus.is_none() && !v.find.loading {
            let store = self.store.borrow();
            if let Some(com) = store.community(&v.id) {
                let people = v.members.iter().chain(v.unlisted.keys()).cloned();
                v.find.dir = Directory::community(&store, com, people);
                v.find.scope = com.channels.iter().filter(|c| c.view != "voice").map(|c| c.channel_id.clone()).collect();
            }
            drop(store);
            v.find.loading = true;
            let id = v.id.clone();
            self.pane_find_load(id, cx);
        }
        let Some(v) = self.community.as_mut() else { return };
        v.find.refresh(&text, caret, now);
        cx.notify();
    }

    fn pane_find_load(&mut self, id: String, cx: &mut Context<Self>) {
        let done = move |this: &mut Self, corpus: Arc<vector_core::search::Corpus>, cx: &mut Context<Self>| {
            if let Some(v) = this.community.as_mut().filter(|v| v.id == id && v.find.loading) {
                v.find.corpus = Some(corpus);
                v.find.loading = false;
                this.pane_find_changed(cx);
            }
        };
        if !self.live {
            let corpus = Arc::new(crate::search::fixture_corpus(&self.store.borrow()));
            return done(self, corpus, cx);
        }
        let (names, blocked) = {
            let store = self.store.borrow();
            (crate::search::names(&store), crate::search::blocked(&store))
        };
        let rx = backend::run(async move {
            let records = vector_core::search::load()?;
            Ok::<_, String>(Arc::new(crate::search::build(&records, &names, &blocked)))
        });
        cx.spawn(async move |this, cx| {
            let res = rx.await;
            this.update(cx, |this, cx| match res {
                Ok(Ok(corpus)) => done(this, corpus, cx),
                Ok(Err(e)) => {
                    if let Some(v) = this.community.as_mut() {
                        v.find.loading = false;
                    }
                    this.show_toast(&e, cx)
                }
                Err(_) => {}
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn pane_find_nav(&mut self, nav: &Nav, window: &mut Window, cx: &mut Context<Self>) {
        let Some(v) = self.community.as_mut() else { return };
        if v.search.read(cx).text().trim().is_empty() {
            return;
        }
        match nav {
            Nav::Up => v.find.step(false),
            Nav::Down => v.find.step(true),
            Nav::Pick => {
                let act = if v.find.showing_suggestions() { Act::Suggest(v.find.sel) } else { Act::Pick(v.find.sel) };
                return self.on_pane_find(act, window, cx);
            }
            Nav::Close => return self.on_pane_find(Act::Close, window, cx),
        }
        cx.notify();
    }

    pub(super) fn on_pane_find(&mut self, act: Act, window: &mut Window, cx: &mut Context<Self>) {
        let Some(v) = self.community.as_mut() else { return };
        let input = v.search.clone();
        match act {
            Act::Close => input.update(cx, |i, cx| i.set_text("", cx)),
            Act::Hover(i) => {
                if v.find.sel == i {
                    return;
                }
                v.find.sel = i;
                return cx.notify();
            }
            Act::Suggest(i) => {
                let Some(sg) = v.find.suggest.get(i).cloned() else { return };
                let range = v.find.suggest_at.clone();
                input.update(cx, |inp, cx| inp.replace(range, &crate::search::completion(&sg), window, cx));
                window.focus(&input.focus_handle(cx), cx);
            }
            Act::Pick(i) => {
                let Some(hit) = v.find.found.hits.get(i).cloned() else { return };
                if self.is_slim {
                    self.slim.details = None;
                }
                self.open_channel(hit.chat, window, cx);
                self.jump_or_say(hit.id, t::msg_jump_not_local(), cx);
                return cx.notify();
            }
            Act::View(open) => return self.open_viewer(open, window, cx),
            Act::Sort | Act::Unfilter(_) => {}
        }
        self.pane_find_changed(cx);
    }
}

/// A result's attachment as a thumbnail: a downloaded picture decoded small, a video's first
/// frame once probed, else the picture's blur. A file not downloaded stays so.
impl VectorApp {
    /// The panel's thumbnails off the GPU soon after it closes: nothing else draws them. The
    /// sweep waits for a frame without them, as one still on screen is kept.
    fn release_search_thumbs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.media.borrow_mut().let_go(THUMB_KEY);
        cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(std::time::Duration::from_millis(500)).await;
            this.update_in(cx, |this, window, cx| this.sweep_pictures(false, window, cx)).ok();
        })
        .detach();
    }
}

/// Search's own cache keys: the timeline decodes the same pictures at its own size.
const THUMB_KEY: &str = "search\u{0}";

fn visual(att: &vector_core::types::Attachment) -> bool {
    let ext = att.extension.to_ascii_lowercase();
    crate::attachments::IMAGES.contains(&ext.as_str()) || crate::video::EXTENSIONS.contains(&ext.as_str())
}

fn thumb(media: &mut crate::media::Media, root: &std::path::Path, att: &vector_core::types::Attachment, scale: f32) -> Option<Thumb> {
    use crate::media::{Slot, Source};
    let ext = att.extension.to_ascii_lowercase();
    let picture = crate::attachments::IMAGES.contains(&ext.as_str());
    let video = crate::video::EXTENSIONS.contains(&ext.as_str());
    if !picture && !video {
        return None;
    }
    let source = (att.downloaded || att.resident).then(|| match vector_core::simd::hex::hex_to_bytes_32_checked(&att.id) {
        Some(h) if att.resident || att.path.is_empty() => Source::Held(h),
        _ => {
            let p = std::path::Path::new(&att.path);
            Source::File(if p.is_absolute() { p.to_path_buf() } else { root.join(p) })
        }
    });
    let device = |w: f32, h: f32| (((w * scale).ceil() as u32).max(1), ((h * scale).ceil() as u32).max(1));
    let blur = |media: &mut crate::media::Media| {
        let meta = att.img_meta.as_ref()?;
        let (w, h) = (meta.width.max(1), meta.height.max(1));
        let image = media.thumb(&meta.thumbhash)?;
        Some(Thumb { image, w, h, open: None, video })
    };
    if video {
        if let Some(source) = source {
            let input = match source {
                Source::Held(h) => crate::audio::AudioInputSpec::Held(h),
                Source::File(p) => crate::audio::AudioInputSpec::File(p),
            };
            media.videos.want_probe(&att.id, &input);
            let info = media.videos.clip(&att.id).and_then(|c| c.info.clone());
            if let Some(i) = info
                && let Some(image) = i.poster
            {
                return Some(Thumb { image, w: i.w, h: i.h, open: None, video: true });
            }
        }
        return blur(media);
    }
    let Some(source) = source else { return blur(media) };
    let (hash, path) = match &source {
        Source::Held(h) => (Some(*h), None),
        Source::File(p) => (None, Some(p.clone())),
    };
    // Shown cropped to cover its box, so decode large enough to fill it both ways.
    let fill = match att.img_meta.as_ref().filter(|m| m.width > 0 && m.height > 0) {
        Some(m) => {
            let a = m.width as f32 / m.height as f32;
            let shown = (crate::search::THUMB_H * a).clamp(crate::search::THUMB_W.0, crate::search::THUMB_W.1);
            device((crate::search::THUMB_H * a).max(shown).min(4096.), (shown / a).max(crate::search::THUMB_H).min(4096.))
        }
        None => device(crate::search::THUMB_W.1, crate::search::THUMB_H),
    };
    match media.still(&format!("{THUMB_KEY}{}", att.id), source, &ext, fill) {
        Slot::Ready { image, w, h } => Some(Thumb {
            open: Some(crate::viewer::Open {
                image: image.clone(),
                w,
                h,
                full: Some(crate::viewer::Full { hash, path, ext, url: None }),
            }),
            image,
            w,
            h,
            video: false,
        }),
        _ => blur(media),
    }
}
