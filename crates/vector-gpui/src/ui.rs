//! The widescreen shell: window chrome, rail, chat list and the open chat. Every size and
//! offset is the Tauri layout's own (measured from `reference/` renders); the comment on a
//! block names the CSS selector it reproduces.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    AnyElement, Context, Entity, Focusable, FontWeight, IntoElement, ListAlignment, ListState, MouseButton,
    ParentElement, Render, SharedString, Styled, Window, WindowControlArea, div, img, linear_color_stop,
    linear_gradient, list, prelude::*, px, svg, uniform_list,
};
use vector_core::chat::SerializableChat;
use vector_core::types::Message;

use futures::StreamExt;
use futures::channel::mpsc::UnboundedReceiver;
use serde_json::Value;
use vector_core::profile::SlimProfile;

use crate::backend::{self, UiEvent, account, messaging, sync};
use crate::composer::{CancelInput, SendText, TextInput};
use crate::kit;
use crate::login::{LoggedIn, LoginView};
use crate::data::{self, Row, Store};
use crate::theme::*;
use crate::inline::Target;
use crate::timeline;

const CHROME_H: f32 = 32.;
const RAIL_W: f32 = 201.;
const LIST_W: f32 = 239.;
const ROW_PITCH: f32 = 54.;

type Shared = Rc<RefCell<Store>>;

enum Screen {
    /// Finding the account on disk.
    Booting,
    Login(Entity<LoginView>),
    Main,
}

pub struct VectorApp {
    screen: Screen,
    /// Backed by a live account (else fixtures, for parity renders).
    live: bool,
    store: Shared,
    open: Option<SharedString>,
    timeline: ListState,
    /// The open chat's rows, rebuilt when it changes rather than per frame.
    rows: Rc<Vec<Row>>,
    read_anchor: Option<String>,
    input: Entity<TextInput>,
    syncing: bool,
    /// Spoilers revealed, by message.
    revealed: Rc<RefCell<HashMap<String, HashSet<u32>>>>,
    started: std::time::Instant,
    /// What the composer is doing beyond a fresh message.
    mode: Mode,
    /// The row carrying the hover toolbar.
    hovered: Option<String>,
    /// The message whose reaction menu is open.
    picker: Option<String>,
    popup: Option<(kit::Popup, PopupAct)>,
    fail_reasons: Rc<RefCell<HashMap<String, String>>>,
    media: Rc<RefCell<crate::media::Media>>,
    emoji_picker: Entity<crate::picker::EmojiPicker>,
    /// The emoji picker is showing (for `picker`'s message, else the composer).
    picker_open: bool,
    /// The picker serves the status dialog's field.
    picker_status: bool,
    picker_closed: std::time::Instant,
    /// Bumped by each hover, so a stale leave doesn't hide the toolbar.
    hover_gen: u64,
    viewer: Option<Entity<crate::viewer::Viewer>>,
    /// The attachment panel is open, and since when (its items fade in from then).
    attach: Option<std::time::Instant>,
    file_preview: Option<Entity<crate::filepreview::FilePreview>>,
    /// The avatar cropper, and the picked file it cuts.
    cropper: Option<(Entity<crate::cropper::Cropper>, std::path::PathBuf)>,
    /// The expanded profile, shown in place of the chat.
    profile: Option<crate::profile::Screen>,
    toast: Option<(SharedString, std::time::Instant)>,
    /// The Status dialog's field, while it is open.
    status: Option<Entity<TextInput>>,
    /// The QR overlay's code, and what holds focus while it shows.
    qr: Option<(std::sync::Arc<gpui::RenderImage>, gpui::FocusHandle)>,
    switcher: Option<crate::switcher::Switcher>,
    /// Add Profile is showing the login screen over this account: the account Back returns to,
    /// and whether the old session was already torn down for the new one.
    adding: Option<(String, bool)>,
}

#[derive(Clone, PartialEq)]
enum Mode {
    Normal,
    Reply { id: String, name: String, snippet: String },
    Edit { id: String },
}

/// What a popup's confirm does.
#[derive(Clone)]
enum PopupAct {
    Notice,
    Delete { id: String, failed: bool },
    Block { npub: String },
    Nickname { npub: String },
    RemoveAccount { npub: String },
}

impl VectorApp {
    fn base(store: Store, live: bool, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| TextInput::new("Enter message...", cx).multiline(px(126.)).twemoji());
        cx.subscribe(&input, |app, _, SendText(text), cx| app.send(text.clone(), cx)).detach();
        cx.subscribe_in(&input, window, |app, _, pasted: &crate::composer::Pasted, window, cx| app.on_paste(pasted, window, cx)).detach();
        cx.subscribe(&input, |app, _, _: &CancelInput, cx| app.cancel_mode(cx)).detach();
        let emoji_picker = cx.new(crate::picker::EmojiPicker::new);
        cx.subscribe_in(&emoji_picker, window, |app, _, crate::picker::Picked(emoji, shift), window, cx| app.picked(emoji.clone(), *shift, window, cx)).detach();
        cx.subscribe_in(&emoji_picker, window, |app, _, _: &crate::picker::Dismissed, window, cx| app.close_picker(window, cx)).detach();
        // A GIF alone in the draft sends at once; beside text it waits for the user's Enter.
        cx.subscribe_in(&emoji_picker, window, |app, _, crate::picker::PickedGif(url), window, cx| {
            let was_empty = app.input.read(cx).text().trim().is_empty();
            app.input.update(cx, |input, cx| input.insert_spaced(url, window, cx));
            app.close_picker(window, cx);
            if was_empty {
                app.input.update(cx, |input, cx| input.submit(window, cx));
            }
        })
        .detach();
        cx.subscribe_in(&emoji_picker, window, |app, _, crate::picker::PickedText(face, shift, recents), window, cx| {
            if app.live {
                let recents = recents.clone();
                let _ = backend::run(async move { backend::actions::set_kaomoji_recents(recents) });
            }
            let field = if app.picker_status { app.status.clone() } else { None }.unwrap_or_else(|| app.input.clone());
            field.update(cx, |input, cx| input.insert_spaced(face, window, cx));
            if !*shift {
                app.close_picker(window, cx);
            }
        })
        .detach();
        Self {
            screen: Screen::Main,
            live,
            store: Rc::new(RefCell::new(store)),
            open: None,
            timeline: ListState::new(0, ListAlignment::Top, px(600.)),
            rows: Rc::default(),
            read_anchor: None,
            input,
            syncing: false,
            revealed: Rc::default(),
            started: std::time::Instant::now(),
            mode: Mode::Normal,
            hovered: None,
            picker: None,
            popup: None,
            fail_reasons: Rc::default(),
            media: Rc::default(),
            emoji_picker,
            picker_open: false,
            picker_status: false,
            picker_closed: std::time::Instant::now(),
            hover_gen: 0,
            viewer: None,
            cropper: None,
            attach: None,
            file_preview: None,
            profile: None,
            toast: None,
            status: None,
            qr: None,
            switcher: None,
            adding: None,
        }
    }

    /// Fixture data, offline: what the parity renders compare.
    pub fn fixtures(store: Store, open: Option<String>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut app = Self::base(store, false, window, cx);
        if let Some(id) = open {
            app.open_chat(id.into(), window, cx);
        }
        if let Ok(who) = std::env::var("VECTOR_PROFILE") {
            let npub = if who == "me" { app.store.borrow().my_npub.clone() } else { who };
            app.open_profile(npub, cx);
        }
        if let Ok(mode) = std::env::var("VECTOR_SWITCHER") {
            app.on_profile(crate::profile::Act::Switcher, window, cx);
            if mode == "edit" {
                app.on_profile(crate::profile::Act::SwitcherEdit, window, cx);
            }
        }
        if std::env::var("VECTOR_STATUS").is_ok() {
            app.open_status(window, cx);
        }
        if let Ok(files) = std::env::var("VECTOR_SEND") {
            app.open_paths_preview(files.split(':').map(Into::into).collect(), window, cx);
        }
        if let Ok(file) = std::env::var("VECTOR_CROP")
            && app.profile.is_some()
            && { app.on_profile(crate::profile::Act::Edit, window, cx); true }
            && let Some((image, w, h)) = std::fs::read(&file).ok().and_then(|b| crate::media::decode(&b, "png", (2048, 2048)))
        {
            let cropper = cx.new(|cx| {
                crate::cropper::Cropper::new(Arc::new(image), (w, h), "Crop Profile Picture", "Drag to move, or drag a corner to resize.", true, window, cx)
            });
            cx.subscribe_in(&cropper, window, |this, _, answer: &crate::cropper::Answer, window, cx| this.on_crop(answer, window, cx)).detach();
            app.cropper = Some((cropper, file.into()));
        }
        app
    }

    /// A live account: login or unlock, then the app as its events arrive.
    pub fn live(mut events: UnboundedReceiver<UiEvent>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut app = Self::base(Store::live(), true, window, cx);
        app.screen = Screen::Booting;
        cx.spawn_in(window, async move |this, cx| {
            while let Some(ev) = events.next().await {
                if this.update_in(cx, |this, window, cx| this.on_event(ev, window, cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
        app.probe(window, cx);
        app
    }

    /// Find the account on disk and open it, or ask how to.
    fn probe(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let probe = backend::run(async { account::boot() });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(info) = probe.await else { return };
            this.update_in(cx, |this, window, cx| match (&info.account, info.encrypted) {
                (Some(_), false) => this.unlock_plain(window, cx),
                (Some(_), true) => this.show_login(Some(info.security_type.as_str()), window, cx),
                (None, _) => this.show_login(None, window, cx),
            })
            .ok();
        })
        .detach();
    }

    /// Start over on whichever account boot now finds, as a fresh launch would.
    fn reboot(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut fresh = Self::base(Store::live(), true, window, cx);
        fresh.screen = Screen::Booting;
        *self = fresh;
        self.probe(window, cx);
        cx.notify();
    }

    fn show_login(&mut self, unlock: Option<&str>, window: &mut Window, cx: &mut Context<Self>) {
        let view = cx.new(|cx| LoginView::new(unlock, window, cx));
        self.attach_login(view, cx, window);
    }

    fn attach_login(&mut self, view: Entity<LoginView>, cx: &mut Context<Self>, window: &mut Window) {
        cx.subscribe_in(&view, window, |this, _, e: &LoggedIn, window, cx| {
            // A profile added over another account starts on a clean slate.
            if this.adding.take().is_some() {
                let mut fresh = Self::base(Store::live(), true, window, cx);
                fresh.screen = Screen::Booting;
                *this = fresh;
            }
            this.logged_in(e.npub.clone(), window, cx)
        })
        .detach();
        cx.subscribe_in(&view, window, |this, _, e: &crate::login::AddProfile, window, cx| this.on_add_profile(e, window, cx)).detach();
        self.screen = Screen::Login(view);
        cx.notify();
    }

    /// An account kept without encryption opens straight away.
    fn unlock_plain(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rx = backend::run(async {
            let npub = account::unlock(None).await?;
            account::connect().await;
            Ok::<_, String>(npub)
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = rx.await.unwrap_or_else(|_| Err("Backend stopped".into()));
            this.update_in(cx, |this, window, cx| match result {
                Ok(npub) => this.logged_in(npub, window, cx),
                Err(e) => {
                    log::error!("unlock failed: {e}");
                    this.show_login(None, window, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    fn logged_in(&mut self, npub: String, _window: &mut Window, cx: &mut Context<Self>) {
        self.store.borrow_mut().my_npub = npub;
        self.screen = Screen::Main;
        self.syncing = true;
        let _ = backend::run(async {
            sync::fetch_messages(true).await;
            sync::notifs();
        });
        cx.notify();
    }

    fn refresh_unread(&mut self, cx: &mut Context<Self>) {
        let rx = backend::run(messaging::unread_counts());
        cx.spawn(async move |this, cx| {
            let Ok(counts) = rx.await else { return };
            this.update(cx, |this, cx| {
                this.store.borrow_mut().unread = Some(counts);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn on_event(&mut self, ev: UiEvent, window: &mut Window, cx: &mut Context<Self>) {
        match ev {
            UiEvent::Booted(boot) => {
                {
                    let mut store = self.store.borrow_mut();
                    store.my_npub = boot.npub;
                    store.profiles = boot.profiles.into_iter().map(|p| (p.id.clone(), p)).collect();
                    store.chats = boot.chats;
                    store.resort();
                }
                self.refresh_unread(cx);
            }
            UiEvent::Core(name, payload) => self.on_core_event(&name, payload, window, cx),
        }
        cx.notify();
    }

    fn on_core_event(&mut self, name: &str, payload: Value, _window: &mut Window, cx: &mut Context<Self>) {
        let chat_id = payload.get("chat_id").and_then(Value::as_str).unwrap_or_default().to_string();
        match name {
            "message_removed" => {
                let id = payload.get("id").and_then(Value::as_str).unwrap_or_default().to_string();
                if let Some(chat) = self.store.borrow_mut().chat_mut(&chat_id) {
                    chat.messages.retain(|m| m.id != id);
                }
                if matches!(&self.mode, Mode::Reply { id: r, .. } | Mode::Edit { id: r } if *r == id) {
                    self.mode = Mode::Normal;
                }
                self.store.borrow_mut().resort();
                if self.open.as_deref() == Some(chat_id.as_str()) {
                    self.refresh_rows(false);
                }
            }
            "message_new" | "message_update" => {
                if let (Some(reason), Some(id)) = (
                    payload.get("reason").and_then(Value::as_str),
                    payload.pointer("/message/id").and_then(Value::as_str),
                ) {
                    self.fail_reasons.borrow_mut().insert(id.to_string(), reason.to_string());
                }
                let Some(msg) = payload.get("message").cloned().and_then(|m| serde_json::from_value::<Message>(m).ok()) else { return };
                let known = if name == "message_new" {
                    self.store.borrow_mut().insert_message(&chat_id, msg)
                } else {
                    let old = payload.get("old_id").and_then(Value::as_str).unwrap_or(&msg.id).to_string();
                    self.store.borrow_mut().replace_message(&chat_id, &old, msg);
                    true
                };
                if !known {
                    let id = chat_id.clone();
                    let rx = backend::run(messaging::chat_snapshot(id));
                    cx.spawn(async move |this, cx| {
                        let Ok(Some(chat)) = rx.await else { return };
                        this.update(cx, |this, cx| {
                            let mut store = this.store.borrow_mut();
                            if store.chat(&chat.id).is_none() {
                                store.chats.push(chat);
                                store.resort();
                            }
                            cx.notify();
                        })
                        .ok();
                    })
                    .detach();
                }
                if self.open.as_deref() == Some(chat_id.as_str()) {
                    self.refresh_rows(true);
                }
            }
            "profile_update" => {
                if let Ok(p) = serde_json::from_value::<SlimProfile>(payload) {
                    self.store.borrow_mut().profiles.insert(p.id.clone(), p);
                }
            }
            "chat_mark_read" => {
                let last = payload.get("last_read").and_then(Value::as_str).unwrap_or_default().to_string();
                if let Some(chat) = self.store.borrow_mut().chat_mut(&chat_id) {
                    chat.last_read = last;
                }
            }
            "attachment_download_progress" => {
                let Some(id) = payload.get("id").and_then(Value::as_str) else { return };
                let mut media = self.media.borrow_mut();
                let t = media.transfers.entry(id.to_string()).or_default();
                if let Some(p) = payload.get("progress").and_then(Value::as_f64).filter(|&p| p >= 0.) {
                    t.pct = Some(p as f32);
                    t.stage = None;
                }
                t.bps = payload.get("bytesPerSec").and_then(Value::as_f64);
            }
            "inline_image_progress" => {
                let (Some(url), Some(p)) = (payload.get("url").and_then(Value::as_str), payload.get("progress").and_then(Value::as_f64)) else { return };
                if p >= 0. {
                    self.media.borrow_mut().linked_progress(url, p as f32);
                }
            }
            "attachment_upload_progress" => {
                let Some(id) = payload.get("id").and_then(Value::as_str) else { return };
                let p = payload.get("progress").and_then(Value::as_f64).unwrap_or(0.);
                let mut media = self.media.borrow_mut();
                let t = media.transfers.entry(id.to_string()).or_default();
                t.pct = Some(p as f32);
                t.stage = None;
            }
            "attachment_upload_stage" => {
                let Some(id) = payload.get("id").and_then(Value::as_str) else { return };
                let stage = payload.get("stage").and_then(Value::as_str).map(str::to_string);
                self.media.borrow_mut().transfers.entry(id.to_string()).or_default().stage = stage;
            }
            "attachment_download_stage" => {
                let Some(id) = payload.get("id").and_then(Value::as_str) else { return };
                let stage = payload.get("stage").and_then(Value::as_str).map(str::to_string);
                self.media.borrow_mut().transfers.entry(id.to_string()).or_default().stage = stage;
            }
            "attachment_download_result" => {
                let id = payload.get("id").and_then(Value::as_str).unwrap_or_default().to_string();
                let matched = payload.get("old_id").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| id.clone());
                let success = payload.get("success").and_then(Value::as_bool).unwrap_or(false);
                let cancelled = payload.get("cancelled").and_then(Value::as_bool).unwrap_or(false);
                let mut media = self.media.borrow_mut();
                for k in [&id, &matched] {
                    media.transfers.remove(k);
                    media.started.remove(k);
                }
                if !success && !cancelled {
                    let reason = payload.get("result").and_then(Value::as_str).unwrap_or_default().to_string();
                    media.failed.insert(matched, reason);
                } else {
                    media.failed.remove(&matched);
                }
            }
            "unread_counts_stale" => self.refresh_unread(cx),
            "sync_progress" => self.syncing = true,
            "sync_finished" => {
                self.syncing = false;
                self.refresh_unread(cx);
            }
            _ => {}
        }
    }

    fn rebuild_rows(&mut self) -> usize {
        let store = self.store.borrow();
        let rows = self
            .open
            .as_ref()
            .and_then(|id| store.chat(id))
            .map(|c| data::timeline(c, store.now_ms(), self.read_anchor.as_deref()))
            .unwrap_or_default();
        self.rows = Rc::new(rows);
        self.rows.len()
    }

    /// The open chat's rows changed: re-laid, following the end when `follow`.
    fn refresh_rows(&mut self, follow: bool) {
        let at_end = self.timeline.logical_scroll_top().item_ix + 12 >= self.timeline.item_count();
        let old = self.timeline.item_count();
        let new = self.rebuild_rows();
        self.timeline.splice(0..old, new);
        if follow && at_end {
            self.timeline.scroll_to_end();
        }
    }

    fn open_chat(&mut self, id: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        self.profile = None;
        self.read_anchor = self.store.borrow().chat(&id).map(|c| c.last_read.clone());
        self.open = Some(id.clone());
        let count = self.rebuild_rows();
        self.timeline = timeline_state(count);
        window.focus(&self.input.read(cx).focus_handle(cx), cx);
        if !self.live {
            return;
        }
        // History beyond the boot's last message comes from the database.
        let chat_id = id.to_string();
        messaging::set_active_chat(Some(chat_id.clone()));
        let rx = backend::run(async move {
            let views = messaging::message_views(chat_id.clone(), 200, 0).await;
            messaging::mark_as_read(chat_id, None).await;
            views
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(mut views)) = rx.await else { return };
            views.reverse();
            this.update(cx, |this, cx| {
                if this.open.as_ref() != Some(&id) {
                    return;
                }
                if let Some(chat) = this.store.borrow_mut().chat_mut(&id) {
                    // Anything newer that landed meanwhile stays.
                    let newest = views.last().map(|m| m.at).unwrap_or(0);
                    let later: Vec<Message> = chat.messages.drain(..).filter(|m| m.at > newest && !views.iter().any(|v| v.id == m.id)).collect();
                    chat.messages = views;
                    chat.messages.extend(later);
                }
                let count = this.rebuild_rows();
                this.timeline = timeline_state(count);
                this.timeline.scroll_to_end();
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// A click on a message's text.
    fn on_text(&mut self, msg_id: String, target: Target, cx: &mut Context<Self>) {
        match target {
            Target::Spoiler(id) => {
                self.revealed.borrow_mut().entry(msg_id).or_default().insert(id);
                cx.notify();
            }
            Target::Link(url) => cx.open_url(&url),
            Target::Code(text) => cx.write_to_clipboard(gpui::ClipboardItem::new_string(text.to_string())),
            Target::Mention(_) => {}
        }
    }

    fn message(&self, id: &str) -> Option<Message> {
        let store = self.store.borrow();
        store.chat(self.open.as_deref()?)?.messages.iter().find(|m| m.id == id).cloned()
    }

    fn on_act(&mut self, id: String, act: timeline::Act, window: &mut Window, cx: &mut Context<Self>) {
        use timeline::Act;
        match act {
            Act::Text(t) => self.on_text(id, t, cx),
            Act::Hover(true) => {
                self.hover_gen += 1;
                if self.hovered.as_deref() != Some(id.as_str()) {
                    self.hovered = Some(id);
                    cx.notify();
                }
            }
            Act::Hover(false) => {
                if self.hovered.as_deref() == Some(id.as_str()) && self.picker.as_deref() != Some(id.as_str()) {
                    // The toolbar overhangs the row: give the pointer a moment to reach it.
                    let gen_id = id.clone();
                    let generation = self.hover_gen;
                    cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(std::time::Duration::from_millis(100)).await;
                        this.update(cx, |this, cx| {
                            if this.hovered.as_deref() == Some(gen_id.as_str()) && this.hover_gen == generation {
                                this.hovered = None;
                                cx.notify();
                            }
                        })
                        .ok();
                    })
                    .detach();
                }
            }
            Act::Download(att) => self.start_download(id, att, false, cx),
            Act::CancelUpload(pending) => {
                if self.live {
                    messaging::cancel_upload(&pending);
                }
            }
            Act::CancelDownload(att) => {
                if self.live {
                    backend::attachments::cancel(&att);
                } else {
                    self.media.borrow_mut().started.remove(&att);
                }
                cx.notify();
            }
            Act::RevealMedia(att) => {
                self.media.borrow_mut().revealed.insert(att);
                cx.notify();
            }
            Act::OpenFile(att) => self.open_file(id, att, cx),
            Act::View(open) => {
                let live = self.live;
                let viewer = cx.new(|cx| crate::viewer::Viewer::new(open, live, window, cx));
                cx.subscribe_in(&viewer, window, |app, _, _: &crate::viewer::Closed, window, cx| {
                    app.viewer = None;
                    window.focus(&app.input.focus_handle(cx), cx);
                    cx.notify();
                })
                .detach();
                self.viewer = Some(viewer);
                cx.notify();
            }
            Act::React => self.toggle_picker(Some(id), window, cx),
            Act::ReactWith(emoji) => {
                let Some(chat) = self.open.clone() else { return };
                if self.live {
                    let rx = backend::run(backend::actions::react(chat.to_string(), id, emoji));
                    self.report(rx, "Reaction Failed", cx);
                } else {
                    let me = self.store.borrow().my_npub.clone();
                    if let Some(m) = self.store.borrow_mut().chat_mut(&chat).and_then(|c| c.messages.iter_mut().find(|m| m.id == id)) {
                        m.reactions.push(vector_core::types::Reaction { id: format!("local-{}", m.reactions.len()), reference_id: id.clone(), author_id: me, emoji, emoji_url: None });
                    }
                }
                cx.notify();
            }
            Act::Reply => {
                let Some(m) = self.message(&id) else { return };
                let author = if m.mine { self.store.borrow().my_npub.clone() } else { m.npub.clone().unwrap_or_else(|| self.open.as_deref().unwrap_or_default().to_string()) };
                let name = self.store.borrow().name(&author);
                let snippet = if !m.content.is_empty() {
                    m.content.clone()
                } else if let Some(a) = m.attachments.first() {
                    data::file_type(&a.extension).1.to_string()
                } else {
                    String::new()
                };
                self.mode = Mode::Reply { id, name, snippet };
                window.focus(&self.input.read(cx).focus_handle(cx), cx);
                cx.notify();
            }
            Act::Edit => {
                let Some(m) = self.message(&id) else { return };
                self.mode = Mode::Edit { id };
                self.input.update(cx, |input, cx| input.set_text(&m.content, cx));
                window.focus(&self.input.read(cx).focus_handle(cx), cx);
                cx.notify();
            }
            Act::Retry => {
                let Some(chat) = self.open.clone() else { return };
                if self.live {
                    let rx = backend::run(backend::actions::retry(chat.to_string(), id));
                    cx.spawn(async move |this, cx| {
                        if let Ok(Err(e)) = rx.await {
                            this.update(cx, |this, cx| this.notice("Retry Failed", e, cx)).ok();
                        }
                    })
                    .detach();
                }
            }
            Act::Delete => {
                let failed = self.message(&id).is_some_and(|m| m.mine && m.failed);
                let mut p = kit::Popup::notice("Delete this message?");
                p.text = Some("Are you sure you want to delete this message?".into());
                p.confirm = "Confirm".into();
                p.cancel = Some("Cancel".into());
                self.popup = Some((p, PopupAct::Delete { id, failed }));
                cx.notify();
            }
        }
    }

    /// A backend result whose failure the user should see.
    fn report(&self, rx: futures::channel::oneshot::Receiver<Result<(), String>>, title: &'static str, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            if let Ok(Err(e)) = rx.await {
                this.update(cx, |this, cx| this.notice(title, e, cx)).ok();
            }
        })
        .detach();
    }

    /// Open the emoji picker for a reaction to `target`, or for the composer; again closes it.
    fn toggle_picker(&mut self, target: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        // The press that dismissed it by landing outside is the same one that would reopen it.
        if self.picker_open || self.picker_closed.elapsed() < std::time::Duration::from_millis(200) {
            self.close_picker(window, cx);
            return;
        }
        self.picker_open = true;
        self.picker = target.clone();
        let picker = self.emoji_picker.clone();
        let reaction = target.is_some();
        picker.update(cx, |p, cx| {
            p.reset(reaction, std::collections::HashMap::new(), window, cx);
            p.prefetch_gifs(cx);
        });
        if self.live {
            let rx = backend::run(async { (backend::actions::emoji_usage(), backend::actions::kaomoji_recents()) });
            cx.spawn(async move |_, cx| {
                let Ok((used, kaomoji)) = rx.await else { return };
                picker.update(cx, |p, cx| {
                    p.set_usage(used, cx);
                    p.set_kaomoji_recents(kaomoji, cx);
                });
            })
            .detach();
        }
        cx.notify();
    }

    /// The status dialog's emoji button: the picker without GIFs, the dialog yielding the bottom.
    fn toggle_status_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.picker_open {
            self.close_picker(window, cx);
            return;
        }
        self.toggle_picker(None, window, cx);
        self.picker_status = true;
        self.emoji_picker.update(cx, |p, cx| {
            p.no_gifs = true;
            cx.notify();
        });
    }

    fn close_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.picker_open {
            return;
        }
        self.picker_open = false;
        self.picker = None;
        self.picker_closed = std::time::Instant::now();
        let field = if std::mem::take(&mut self.picker_status) { self.status.clone() } else { None };
        window.focus(&field.unwrap_or_else(|| self.input.clone()).read(cx).focus_handle(cx), cx);
        cx.notify();
    }

    fn picked(&mut self, emoji: String, shift: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.emoji_picker.update(cx, |p, _| p.bump(&emoji));
        if self.live {
            let e = emoji.clone();
            let _ = backend::run(async move { backend::actions::bump_emoji(&e) });
        }
        if self.picker_status {
            if let Some(field) = self.status.clone() {
                field.update(cx, |input, cx| input.insert_spaced(&emoji, window, cx));
            }
            if !shift {
                self.close_picker(window, cx);
            }
            return;
        }
        match self.picker.clone() {
            Some(id) => {
                let me = self.store.borrow().my_npub.clone();
                let already = self.message(&id).is_some_and(|m| m.reactions.iter().any(|r| r.author_id == me && r.emoji == emoji));
                if !already {
                    self.on_act(id, timeline::Act::ReactWith(emoji), window, cx);
                }
            }
            None => self.input.update(cx, |input, cx| input.insert_spaced(&emoji, window, cx)),
        }
        if !shift {
            self.close_picker(window, cx);
        }
    }

    /// Fetch an attachment; its events paint the progress and the result.
    pub fn start_download(&mut self, msg_id: String, att_id: String, auto: bool, cx: &mut Context<Self>) {
        let Some(chat) = self.open.clone() else { return };
        {
            let mut media = self.media.borrow_mut();
            if !media.started.insert(att_id.clone()) {
                return;
            }
            media.failed.remove(&att_id);
        }
        cx.notify();
        if !self.live {
            return;
        }
        let rx = backend::run(backend::attachments::download(chat.to_string(), msg_id, att_id.clone(), auto));
        cx.spawn(async move |this, cx| {
            let _ = rx.await;
            this.update(cx, |this, cx| {
                this.media.borrow_mut().started.remove(&att_id);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// A file on this device: shown in its folder, or a held one saved where the user picks.
    fn open_file(&mut self, msg_id: String, att_id: String, cx: &mut Context<Self>) {
        let Some(att) = self.message(&msg_id).and_then(|m| m.attachments.into_iter().find(|a| a.id == att_id)) else { return };
        if !att.resident && !att.path.is_empty() {
            let path = std::path::PathBuf::from(&att.path);
            let path = if path.is_absolute() { path } else { self.store.borrow().media_root.join(path) };
            cx.reveal_path(&path);
            return;
        }
        if !self.live {
            return;
        }
        let Some(hash) = vector_core::simd::hex::hex_to_bytes_32_checked(&att.id) else { return };
        let dir = dirs::download_dir().unwrap_or_else(std::env::temp_dir);
        let name = if att.name.is_empty() { format!("{}.{}", &att.id[..16.min(att.id.len())], att.extension) } else { att.name.clone() };
        let pick = cx.prompt_for_new_path(&dir, Some(&name));
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(dest))) = pick.await else { return };
            let rx = backend::run(async move {
                let bytes = backend::attachments::bytes(Some(hash), String::new()).await.ok_or("This file isn't on this device any more")?;
                tokio::fs::write(&dest, bytes).await.map_err(|e| e.to_string())
            });
            if let Ok(Err(e)) = rx.await {
                this.update(cx, |this, cx| this.notice("Save Failed", e.to_string(), cx)).ok();
            }
        })
        .detach();
    }

    fn notice(&mut self, title: &str, text: String, cx: &mut Context<Self>) {
        let mut p = kit::Popup::notice(title.to_string());
        p.text = Some(text.into());
        self.popup = Some((p, PopupAct::Notice));
        cx.notify();
    }

    fn answer_popup(&mut self, yes: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some((popup, act)) = self.popup.take() else { return };
        cx.notify();
        if !yes {
            return;
        }
        match act {
            PopupAct::Notice => {}
            PopupAct::Block { npub } => self.set_blocked(npub, true, cx),
            PopupAct::RemoveAccount { npub } => self.remove_account(npub, window, cx),
            PopupAct::Nickname { npub } => {
                if let Some(input) = popup.input {
                    let nick = input.read(cx).text().trim().to_string();
                    self.set_nickname(npub, nick, cx);
                }
            }
            PopupAct::Delete { id, failed } => {
                if !self.live {
                    let open = self.open.clone();
                    if let Some(chat) = open.and_then(|o| self.store.borrow_mut().chat_mut(&o).map(|c| c.messages.retain(|m| m.id != id))) {
                        let _ = chat;
                    }
                    self.refresh_rows(false);
                    return;
                }
                let rx = if failed { backend::run(backend::actions::delete_failed(id)) } else { backend::run(backend::actions::delete(id)) };
                self.report(rx, "Delete Failed", cx);
            }
        }
    }

    fn cancel_mode(&mut self, cx: &mut Context<Self>) {
        if matches!(self.mode, Mode::Edit { .. }) {
            self.input.update(cx, |input, cx| input.set_text("", cx));
        }
        self.mode = Mode::Normal;
        self.picker = None;
        cx.notify();
    }

    fn send(&mut self, text: String, cx: &mut Context<Self>) {
        let Some(id) = self.open.clone() else { return };
        let mode = std::mem::replace(&mut self.mode, Mode::Normal);
        if let Mode::Edit { id: msg_id } = &mode {
            if self.live {
                let rx = backend::run(backend::actions::edit(id.to_string(), msg_id.clone(), text));
                self.report(rx, "Edit Failed", cx);
            } else if let Some(m) = self.store.borrow_mut().chat_mut(&id).and_then(|c| c.messages.iter_mut().find(|m| m.id == *msg_id)) {
                m.content = text;
                m.edited = true;
            }
            self.refresh_rows(false);
            cx.notify();
            return;
        }
        let reply = match &mode {
            Mode::Reply { id, .. } => id.clone(),
            _ => String::new(),
        };
        if self.live {
            let receiver = id.to_string();
            let used = crate::emoji::in_text(&text);
            if !used.is_empty() {
                let _ = backend::run(async move { backend::actions::bump_emoji_batch(used) });
            }
            let rx = backend::run(messaging::send(receiver, text, reply));
            cx.spawn(async move |this, cx| {
                if let Ok(Err(e)) = rx.await {
                    log::warn!("send failed: {e}");
                    this.update(cx, |_, cx| cx.notify()).ok();
                }
            })
            .detach();
            return;
        }
        // Fixtures: a local echo.
        {
            let mut store = self.store.borrow_mut();
            let now = store.now_ms();
            let me = store.my_npub.clone();
            let Some(chat) = store.chat_mut(&id) else { return };
            let msg = Message { id: format!("local-{now}-{}", chat.messages.len()), content: text, replied_to: reply, at: now, mine: true, npub: Some(me), ..Default::default() };
            chat.last_read = msg.id.clone();
            chat.messages.push(msg);
            store.resort();
        }
        self.refresh_rows(false);
        self.timeline.scroll_to_end();
        cx.notify();
    }
}

impl Render for VectorApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let root = div()
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(c(APP_BG))
            .font_family(INTER)
            .text_color(c(TEXT))
            .text_size(px(16.))
            .line_height(px(24.));
        match &self.screen {
            Screen::Booting => return root.child(div().absolute().inset_0().child(chrome(window, true))),
            Screen::Login(view) => {
                return root.child(div().absolute().inset_0().child(view.clone())).child(div().absolute().top_0().left_0().right_0().child(chrome(window, true)));
            }
            Screen::Main => {}
        }
        let store = self.store.clone();
        let store = store.borrow();
        let popup = self.popup.as_ref().map(|(p, _)| {
            let app = cx.entity().downgrade();
            kit::popup(p, move |yes, window, cx| {
                app.update(cx, |app, cx| app.answer_popup(yes, window, cx)).ok();
            })
        });
        let root = root.on_drop(cx.listener(|app, paths: &gpui::ExternalPaths, window, cx| {
            app.open_paths_preview(paths.paths().to_vec(), window, cx);
        }));
        root.children(popup).child(chrome(window, false)).child(
            div()
                .flex()
                .flex_row()
                .flex_1()
                .min_h_0()
                .child(rail(&store, self.open.as_ref(), {
                    let app = cx.entity().downgrade();
                    move |_: &gpui::ClickEvent, _: &mut Window, cx: &mut gpui::App| {
                        app.update(cx, |app, cx| {
                            let me = app.store.borrow().my_npub.clone();
                            app.open_profile(me, cx);
                        })
                        .ok();
                    }
                }))
                .child(chat_list(&self.store, &store, self.open.clone(), cx))
                .child(match (&self.profile, self.open.clone()) {
                    (Some(p), _) => {
                        let app = cx.entity().downgrade();
                        let on: crate::profile::OnAct = Rc::new(move |act, window, cx| {
                            app.update(cx, |app, cx| app.on_profile(act, window, cx)).ok();
                        });
                        crate::profile::pane(&store, p, &on, cx)
                    }
                    (None, Some(id)) => self.chat_pane(&store, id, cx).into_any_element(),
                    (None, None) => empty_pane().into_any_element(),
                }),
        )
        .children(self.switcher.as_ref().filter(|_| self.profile.is_some()).map(|s| {
            let app = cx.entity().downgrade();
            let on: crate::switcher::OnAct = Rc::new(move |act, window, cx| {
                app.update(cx, |app, cx| app.on_switcher(act, window, cx)).ok();
            });
            crate::switcher::view(s, CHROME_H + 60., RAIL_W + LIST_W, f32::from(window.viewport_size().height), &on)
        }))
        .children(self.status_view(&store, window, cx))
        .children(self.qr_view(cx))
        .children(self.toast_view(window))
        .children(self.viewer.clone().map(|v| gpui::deferred(div().absolute().inset_0().child(v)).with_priority(10)))
        .children(self.file_preview.clone().map(|v| gpui::deferred(div().absolute().inset_0().child(v)).with_priority(9)))
        .children(self.cropper.as_ref().map(|(v, _)| gpui::deferred(div().absolute().inset_0().child(v.clone())).with_priority(11)))
    }
}

// ── #window-chrome ──────────────────────────────────────────────────────────────────────

/// The title strip; over the login screens it is only its controls, on the same art.
fn chrome(window: &Window, login: bool) -> impl IntoElement {
    let maximized = window.is_maximized();
    let ctl = |id: &'static str, icon: &'static str, area: WindowControlArea, hover: u32| {
        div()
            .id(id)
            .w(px(37.))
            .h_full()
            .flex()
            .items_center()
            .justify_center()
            .text_color(white_a(0.7))
            .hover(move |s| s.bg(c(hover)).text_color(c(0xffffff)))
            .window_control_area(area)
            .child(svg().path(icon).size(px(10.)).text_color(white_a(0.7)))
    };
    div()
        .h(px(CHROME_H))
        .flex_shrink_0()
        .flex()
        .items_center()
        .when(!login, |d| d.bg(c(CHROME_BG)).border_b_1().border_color(c(DIVIDER)))
        .child(
            div()
                .id("chrome-drag")
                .flex_1()
                .h_full()
                .window_control_area(WindowControlArea::Drag)
                .on_mouse_down(MouseButton::Left, |e, window, _| {
                    if e.click_count == 2 {
                        window.titlebar_double_click();
                    } else {
                        window.start_window_move();
                    }
                })
                .when(!login, |d| d.child(
                    div()
                        .relative()
                        .child(img("ui/logo-mark.svg").absolute().left(px(12.)).top(px(5.5)).w(px(12.)).h(px(20.)))
                        .child(img("ui/logo-typeface.svg").absolute().left(px(32.)).top(px(9.5)).w(px(92.)).h(px(12.))),
                )),
        )
        .child(
            div()
                .flex()
                .h_full()
                .child(
                    div()
                        .w(px(37.))
                        .mr(px(8.))
                        .h_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(svg().path("ui/chrome-help.svg").size(px(16.)).text_color(c(CHROME_HELP))),
                )
                .child(ctl("chrome-min", "ui/chrome-min.svg", WindowControlArea::Min, HOVER))
                .child(ctl(
                    "chrome-max",
                    if maximized { "ui/chrome-restore.svg" } else { "ui/chrome-max.svg" },
                    WindowControlArea::Max,
                    HOVER,
                ))
                .child(ctl("chrome-close", "ui/chrome-close.svg", WindowControlArea::Close, 0xe81123)),
        )
}

// ── #navbar (widescreen rail) ────────────────────────────────────────────────────────────

pub(crate) fn avatar(store: &Store, npub: &str, size: f32) -> AnyElement {
    match store.avatar_path(npub) {
        Some(path) => img(path).size(px(size)).rounded_full().flex_shrink_0().into_any_element(),
        None => img("icons/user-placeholder.svg").size(px(size)).rounded_full().flex_shrink_0().into_any_element(),
    }
}

fn rail_label(text: &'static str) -> impl IntoElement {
    // .ws-rail-group-label
    div()
        .h(px(24.))
        .px(px(16.))
        .font_family(RUBIK)
        .text_size(px(15.))
        .text_color(white_a(0.38))
        .child(text)
}

fn rail(store: &Store, open: Option<&SharedString>, on_me: impl Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static) -> impl IntoElement {
    let unread: Vec<&SerializableChat> = store
        .chats
        .iter()
        .filter(|c| store.unread_count(c) > 0 && open.is_none_or(|o| *o != c.id))
        .collect();
    let any_unread = store.chats.iter().any(|c| store.unread_count(c) > 0);

    // .ws-rail-messages.is-home
    let messages = div().pt(px(16.)).px(px(8.)).child(
        div()
            .h(px(40.))
            .rounded(px(6.))
            .bg(c(RAIL_TILE))
            .flex()
            .items_center()
            .justify_center()
            .gap(px(11.))
            .child(svg().path("ui/rail-mail.svg").w(px(25.)).h(px(20.5)).text_color(c(0xffffff)))
            .child(
                div()
                    .relative()
                    .font_family(RUBIK)
                    .font_weight(FontWeight::MEDIUM)
                    .text_size(px(14.))
                    .text_color(c(0xffffff))
                    .child("Messages")
                    .when(any_unread, |d| {
                        d.child(div().absolute().left(px(67.4)).top(px(-1.)).size(px(8.)).rounded_full().bg(c(PRIMARY)))
                    }),
            ),
    );

    // #ws-rail-dms
    let dms = (!unread.is_empty()).then(|| {
        div().mt(px(10.)).child(rail_label("New Messages")).children(unread.iter().map(|chat| {
            div()
                .id(SharedString::from(format!("rail-{}", chat.id)))
                .mx(px(8.))
                .h(px(31.))
                .rounded(px(10.))
                .flex()
                .items_center()
                .px(px(8.))
                .gap(px(10.))
                .child(avatar(store, &chat.id, 26.))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .font_family(RUBIK)
                        .text_size(px(14.))
                        .text_color(c(0xffffff))
                        .child(store.name(&chat.id)),
                )
                .child(div().size(px(7.)).mr(px(5.)).rounded_full().bg(white_a(0.32)))
        }))
    });

    // #ws-rail-spaces
    let spaces = div()
        .mt(px(10.))
        .pt(px(6.))
        .child(div().mx(px(16.)).h(px(1.)).bg(c(DIVIDER)))
        .child(div().pt(px(16.)).child(rail_label("Communities")))
        .child(
            div()
                .mx(px(8.))
                .h(px(31.))
                .rounded(px(10.))
                .flex()
                .items_center()
                .px(px(8.))
                .gap(px(10.))
                .child(
                    div()
                        .size(px(26.))
                        .rounded(px(4.9))
                        .border_1()
                        .border_dashed()
                        .border_color(white_a(0.22))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(svg().path("ui/plus-thin.svg").size(px(14.)).text_color(c(PRIMARY))),
                )
                .child(
                    div()
                        .font_family(RUBIK)
                        .text_size(px(14.))
                        .text_color(white_a(0.55))
                        .child("Create Community"),
                ),
        );

    let me = store.me();
    let my_name = me.map(|p| store.name(&p.id)).unwrap_or_default();
    let status = me.map(|p| p.status.title.clone()).filter(|t| !t.is_empty());

    div()
        .w(px(RAIL_W))
        .flex_shrink_0()
        .h_full()
        .flex()
        .flex_col()
        .bg(c(RAIL_BG))
        .border_r_1()
        .border_color(c(DIVIDER))
        .child(
            // #ws-rail-shortcuts
            div()
                .flex_1()
                .min_h_0()
                .border_b_1()
                .border_color(c(DIVIDER))
                .child(messages)
                .children(dms)
                .child(spaces),
        )
        .child(
            // #ws-rail-collapse
            div()
                .mt(px(10.))
                .mb(px(2.))
                .mx(px(8.))
                .h(px(40.))
                .rounded(px(6.))
                .opacity(0.4)
                .flex()
                .items_center()
                .child(
                    div().ml(px(14.)).size(px(28.)).flex().items_center().justify_center().child(
                        svg().path("icons/chevron-double-left.svg").size(px(28.)).text_color(c(0xffffff)),
                    ),
                )
                .child(
                    div()
                        .ml(px(11.))
                        .font_weight(FontWeight::MEDIUM)
                        .text_size(px(14.))
                        .text_color(c(TEXT))
                        .child("Collapse"),
                ),
        )
        .child(
            // #ws-rail-account .account
            div().p(px(8.)).pt(px(8.)).child(
                div()
                    .id("ws-rail-account")
                    .cursor_pointer()
                    .on_click(on_me)
                    .h(px(54.))
                    .rounded(px(12.))
                    .bg(c(RAIL_TILE))
                    .flex()
                    .items_center()
                    .px(px(9.))
                    .gap(px(10.))
                    .child(avatar(store, &store.my_npub, 38.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .truncate()
                                    .font_weight(FontWeight::BOLD)
                                    .text_size(px(14.))
                                    .line_height(px(19.))
                                    .text_color(c(0xffffff))
                                    .child(my_name),
                            )
                            .child(
                                div()
                                    .mt(px(1.5))
                                    .truncate()
                                    .text_size(px(12.))
                                    .line_height(px(16.))
                                    .text_color(white_a(0.4))
                                    .child(status.unwrap_or_else(|| "Set a Status".into())),
                            ),
                    ),
            ),
        )
}

// ── #chats ──────────────────────────────────────────────────────────────────────────────

fn chat_list(shared: &Shared, store: &Store, open: Option<SharedString>, cx: &mut Context<VectorApp>) -> impl IntoElement {
    let tab = |label: &'static str, active: bool| {
        div()
            .flex_1()
            .h(px(40.))
            .flex()
            .items_center()
            .justify_center()
            .font_family(RUBIK)
            .text_size(px(14.))
            .font_weight(if active { FontWeight::BOLD } else { FontWeight::MEDIUM })
            .text_color(if active { c(0xffffff) } else { white_a(0.38) })
            .when(active, |d| d.border_b_2().border_color(c(PRIMARY)))
            .child(label)
    };
    let count = store.chats.len();
    let store = shared.clone();
    let view = cx.entity().downgrade();
    div()
        .w(px(LIST_W))
        .flex_shrink_0()
        .h_full()
        .flex()
        .flex_col()
        .bg(c(LIST_BG))
        .border_r_1()
        .border_color(c(DIVIDER))
        .child(
            // .chatlist-head
            div()
                .mt(px(22.))
                .ml(px(15.))
                .mr(px(12.))
                .h(px(36.))
                .flex()
                .items_center()
                .justify_between()
                .child(div().font_weight(FontWeight::SEMIBOLD).text_color(c(0xffffff)).child("Chats"))
                .child(
                    div()
                        .size(px(32.))
                        .rounded(px(8.))
                        .border_1()
                        .border_color(white_a(0.12))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(svg().path("ui/plus-thin.svg").size(px(16.)).text_color(c(ICON_MUTED))),
                ),
        )
        .child(
            // .cc-tabs
            div()
                .mt(px(10.))
                .mx(px(11.))
                .flex()
                .border_b_1()
                .border_color(white_a(0.2))
                .child(tab("Messages", true))
                .child(tab("Requests", false)),
        )
        .child(
            div().flex_1().min_h_0().pt(px(6.)).child(
                uniform_list("chatlist", count, move |range, _, _| {
                    let store = store.borrow();
                    range
                        .map(|i| {
                            let chat = &store.chats[i];
                            let active = open.as_ref().is_some_and(|o| *o == chat.id);
                            let id: SharedString = chat.id.clone().into();
                            let view = view.clone();
                            chat_row(&store, chat, active).on_click(move |_, window, cx| {
                                let id = id.clone();
                                view.update(cx, |app, cx| {
                                    app.open_chat(id, window, cx);
                                    cx.notify();
                                })
                                .ok();
                            })
                        })
                        .collect()
                })
                .size_full(),
            ),
        )
}

/// `.chatlist-contact` (ChatlistRow.svelte).
/// The chat list's last-message line (`chatPreviewText`).
fn preview_line(chat: &SerializableChat, color: gpui::Hsla) -> AnyElement {
    let look = crate::inline::Look::new(RUBIK, 13., 18., color);
    let Some(m) = chat.messages.last() else {
        return crate::inline::inline(Vec::new(), look).into_any_element();
    };
    let prefix = if m.mine { "You: " } else { "" };
    let atoms = if m.pending {
        vec![crate::inline::Atom::Text { text: "Sending...".into(), look: look.clone(), kind: crate::inline::Kind::Plain }]
    } else if m.content.is_empty() && !m.attachments.is_empty() {
        let text = format!("{prefix}Sent a {}", data::file_type(&m.attachments[0].extension).1);
        vec![crate::inline::Atom::Text { text: text.into(), look: look.clone(), kind: crate::inline::Kind::Plain }]
    } else {
        let mut v = Vec::new();
        if !prefix.is_empty() {
            v.push(crate::inline::Atom::Text { text: prefix.into(), look: look.clone(), kind: crate::inline::Kind::Plain });
        }
        v.extend(crate::preview::atoms(&m.content, usize::MAX, &look));
        v
    };
    crate::inline::inline(atoms, look).single_line().into_any_element()
}

fn chat_row(store: &Store, chat: &SerializableChat, active: bool) -> gpui::Stateful<gpui::Div> {
    let unread = store.unread_count(chat) > 0 && !active;
    let (name_color, name_weight) = match (unread, active) {
        (true, _) => (c(0xffffff), FontWeight::BOLD),
        (false, true) => (c(0xffffff), FontWeight::NORMAL),
        _ => (white_a(0.75), FontWeight::NORMAL),
    };
    let preview_color = if active {
        white_a(0.85)
    } else if unread {
        c(PREVIEW_UNREAD)
    } else {
        ca(160, 160, 160, 0.5)
    };
    let last = chat.messages.last().map(|m| m.at).unwrap_or(0);
    div()
        .id(SharedString::from(format!("row-{}", chat.id)))
        .h(px(ROW_PITCH))
        .w(px(230.))
        .pb(px(4.))
        .child(
            div()
                .relative()
                .h(px(50.))
                .flex()
                .items_center()
                .pl(px(12.))
                .when(active, |d| {
                    d.child(div().absolute().left(px(-1.)).top(px(5.)).w(px(3.)).h(px(40.)).rounded_r(px(3.)).bg(c(PRIMARY)))
                })
                .child(avatar(store, &chat.id, 40.))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .h(px(40.))
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .h(px(20.))
                                .pl(px(10.))
                                .pr(px(15.))
                                .truncate()
                                .font_family(RUBIK)
                                .text_size(px(15.))
                                .line_height(px(20.))
                                .font_weight(name_weight)
                                .text_color(name_color)
                                .child(store.name(&chat.id)),
                        )
                        .child(
                            div()
                                .mt(px(-4.))
                                .h(px(24.))
                                .pr(px(10.))
                                .flex()
                                .items_center()
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .ml(px(10.))
                                        .mr(px(8.))
                                        .overflow_hidden()
                                        .child(preview_line(chat, preview_color)),
                                )
                                .child(
                                    div()
                                        .flex_shrink_0()
                                        .text_size(px(11.))
                                        .text_color(c(LIST_TIME))
                                        .child(data::time_ago(store.now_ms(), last)),
                                ),
                        ),
                ),
        )
}

// ── .chat ───────────────────────────────────────────────────────────────────────────────

fn empty_pane() -> impl IntoElement {
    div()
        .flex_1()
        .h_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .child(img("ui/logo-mark.svg").w(px(48.)).h(px(80.)).opacity(0.25))
        .child(div().mt(px(12.)).text_size(px(14.)).text_color(white_a(0.35)).child("Pick a conversation to get started"))
}

impl VectorApp {
    fn chat_pane(&self, store: &Store, id: SharedString, cx: &mut Context<Self>) -> impl IntoElement {
        let header = div()
            .h(px(61.))
            .flex_shrink_0()
            .relative()
            .bg(c(CHAT_HEADER_BG))
            .border_b_1()
            .border_color(c(DIVIDER))
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .id("chat-header-info")
                    .cursor_pointer()
                    .on_click({
                        let id = id.to_string();
                        cx.listener(move |app, _, _, cx| app.open_profile(id.clone(), cx))
                    })
                    .mt(px(5.))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(avatar(store, &id, 22.))
                    .child(
                        div()
                            .font_family(RUBIK)
                            .font_weight(FontWeight::BOLD)
                            .text_size(px(18.72))
                            .text_color(c(TEXT))
                            .child(store.name(&id)),
                    ),
            )
            .child(
                div()
                    .absolute()
                    .right(px(10.))
                    .top(px(10.))
                    .flex()
                    .child(
                        div()
                            .size(px(40.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(svg().path("ui/call.svg").size(px(22.)).text_color(c(ICON_MUTED))),
                    )
                    .child(
                        div()
                            .size(px(40.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(svg().path("ui/menu-dots.svg").size(px(28.)).text_color(c(ICON_MUTED))),
                    ),
            );

        let rows_store = self.store.clone();
        let rows_id = id.clone();
        let rows = self.rows.clone();
        let revealed = self.revealed.clone();
        let clock = self.started.elapsed().as_secs_f32();
        let app = cx.entity().downgrade();
        let app_media = app.clone();
        let on_act: timeline::OnAct = Rc::new(move |msg_id, act, window, cx| {
            let msg_id = msg_id.to_string();
            app.update(cx, |app, cx| app.on_act(msg_id, act, window, cx)).ok();
        });
        let hovered = self.hovered.clone();
        let picker = self.picker.clone();
        let replying = match &self.mode {
            Mode::Reply { id, .. } => Some(id.clone()),
            _ => None,
        };
        let fail_reasons = self.fail_reasons.clone();
        let media = self.media.clone();
        let live = self.live;
        let messages = list(self.timeline.clone(), move |i, window, cx| {
            let rows_store = rows_store.borrow();
            let (Some(chat), Some(row)) = (rows_store.chat(&rows_id), rows.get(i)) else {
                return div().into_any_element();
            };
            let revealed = revealed.borrow();
            let fail_reasons = fail_reasons.borrow();
            let ctx = timeline::Ctx {
                store: &rows_store,
                chat,
                revealed: &revealed,
                clock,
                on_act: on_act.clone(),
                hovered: hovered.as_deref(),
                replying: replying.as_deref(),
                picker: picker.as_deref(),
                fail_reasons: &fail_reasons,
                media: &media,
                scale: window.scale_factor(),
            };
            // A divider's 10px margins stand in for the row's own above it.
            let after_message = i > 0 && matches!(rows[i - 1], Row::Message { .. });
            let el = timeline_row(&ctx, row, after_message);
            // `.chat-rows > *:last-child`: clear of the typing strip.
            let el = if i + 1 == rows.len() { div().pb(px(33.)).child(el).into_any_element() } else { el };
            crate::media::pump(&media, app_media.clone(), live, cx);
            let metas = std::mem::take(&mut media.borrow_mut().meta_queue);
            if live {
                for (chat, msg) in metas {
                    let _ = backend::run(backend::unfurl::fetch_msg_metadata(chat, msg));
                }
            }
            let autos = std::mem::take(&mut media.borrow_mut().auto_queue);
            if !autos.is_empty() {
                let app = app_media.clone();
                cx.defer(move |cx| {
                    app.update(cx, |app, cx| {
                        for (msg, att) in autos {
                            app.start_download(msg, att, true, cx);
                        }
                    })
                    .ok();
                });
            }
            el
        })
        .size_full();

        div()
            .flex_1()
            .h_full()
            .min_w_0()
            .flex()
            .flex_col()
            .bg(c(CHAT_BG))
            .child(header)
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .bg(c(MESSAGES_BG))
                    .pt(px(12.))
                    .child(messages)
                    // `.fadeout-bottom-msgs`: rows dim into the composer.
                    .child(
                        div()
                            .absolute()
                            .bottom_0()
                            .left_0()
                            .right_0()
                            .h(px(40.))
                            .bg(gpui::linear_gradient(
                                180.,
                                gpui::linear_color_stop(ca(3, 3, 3, 0.), 0.8),
                                gpui::linear_color_stop(ca(3, 3, 3, 0.5), 1.),
                            )),
                    ),
            )
            .child(composer(self.input.clone(), &self.mode, (self.picker_open && !self.picker_status).then(|| (self.emoji_picker.clone(), self.picker.is_none())), self.attach, cx))
    }
}

/// A short chat sits at the top, as the Tauri list does; a long one opens at its end and
/// follows new messages while scrolled there.
fn timeline_state(count: usize) -> ListState {
    let state = ListState::new(count, ListAlignment::Top, px(600.));
    state.set_follow_mode(gpui::FollowMode::Tail);
    state
}

fn timeline_row(ctx: &timeline::Ctx, row: &Row, after_message: bool) -> AnyElement {
    match row {
        Row::Date { label, time } => divider(
            div()
                .flex()
                .text_size(px(12.))
                .text_color(c(TEXT))
                .child(div().font_weight(FontWeight::BOLD).child(label.clone()))
                .child(format!(", {time}")),
            white_a(0.1),
            0.7,
        ),
        Row::Unread => divider(
            // letter-spacing: 0.08em, which GPUI text has no property for.
            div()
                .flex()
                .gap(px(0.88))
                .font_weight(FontWeight::BOLD)
                .text_size(px(11.))
                .text_color(c(ACCENT))
                .children("NEW".chars().map(|ch| div().child(ch.to_string()))),
            ca(89, 252, 179, 0.6),
            1.,
        ),
        Row::Message { index, first } => {
            let gap = if !after_message { 0. } else if *first { 14. } else { 1. };
            timeline::message_row(ctx, *index, *first, gap)
        }
    }
}

/// `.msg-inline-timestamp`: a centred label with a hairline either side.
fn divider(label: impl IntoElement, line: gpui::Hsla, opacity: f32) -> AnyElement {
    let rule = move || div().flex_1().h(px(1.)).bg(line);
    div()
        .px(px(10.))
        .py(px(10.))
        .child(
            div()
                .h(px(24.))
                .flex()
                .items_center()
                .gap(px(12.))
                .font_family(RUBIK)
                .opacity(opacity)
                .child(rule())
                .child(label)
                .child(rule()),
        )
        .into_any_element()
}

/// `#chat-box.input-box`: the gradient frame around the composer, with the reply bar
/// tucked behind its top while replying, and edit's cancel in place of attach.
fn composer(
    input: Entity<TextInput>,
    mode: &Mode,
    picker: Option<(Entity<crate::picker::EmojiPicker>, bool)>,
    attach: Option<std::time::Instant>,
    cx: &mut Context<VectorApp>,
) -> impl IntoElement {
    let icon = |path: &'static str| {
        div()
            .w(px(38.))
            .h(px(45.))
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_center()
            .child(svg().path(path).size(px(23.)).text_color(c(ICON_MUTED)))
    };
    let cancel = |id: &'static str, size: f32, cx: &mut Context<VectorApp>| {
        div()
            .id(id)
            .cursor_pointer()
            .flex()
            .items_center()
            .justify_center()
            .on_click(cx.listener(|app, _, _, cx| app.cancel_mode(cx)))
            .child(svg().path("icons/cancel.svg").size(px(size)).text_color(c(timeline::DANGER)))
    };
    let replying = matches!(mode, Mode::Reply { .. });
    let editing = matches!(mode, Mode::Edit { .. });
    let mut boxed = div()
        .relative()
        .flex_shrink_0()
        .px(px(8.))
        .pt(px(if replying { 39. } else { 5. }))
        .pb(px(5.))
        .bg(linear_gradient(
            90.,
            linear_color_stop(c(ACCENT), 0.),
            linear_color_stop(c(ACCENT_DEEP), 1.),
        ));
    if let Mode::Reply { name, snippet, .. } = mode {
        let look = crate::inline::Look::new(INTER, 13., 18., white_a(0.45));
        let atoms = crate::preview::reply_atoms(snippet, &look);
        boxed = boxed.child(
            div()
                .absolute()
                .top(px(5.))
                .left(px(8.))
                .right(px(8.))
                .h(px(46.))
                .pl(px(12.))
                .pr(px(44.))
                .pb(px(12.))
                .rounded_t(px(10.))
                .bg(c(0x0b1510))
                .flex()
                .items_center()
                .gap(px(8.))
                .child(div().w(px(3.)).h(px(16.)).rounded(px(2.)).bg(c(ACCENT)).flex_shrink_0())
                .child(
                    div()
                        .flex()
                        .flex_shrink_0()
                        .max_w(gpui::relative(0.5))
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_size(px(13.5))
                        .text_color(white_a(0.85))
                        .child("Replying to\u{a0}")
                        .child(div().font_weight(FontWeight::SEMIBOLD).text_color(c(ACCENT)).child(name.clone())),
                )
                .child(div().flex_1().min_w_0().overflow_hidden().child(crate::inline::inline(atoms, look).single_line()))
                .child(div().absolute().right(px(10.5)).top(px(5.)).size(px(24.)).p(px(3.)).child(cancel("reply-cancel", 18., cx))),
        );
    }
    boxed.child(
        // The buttons keep to the last line as the text grows upward.
        div()
            .min_h(px(45.))
            .rounded(px(10.))
            .bg(c(APP_BG))
            .flex()
            .items_end()
            .child(if editing { cancel("edit-cancel", 23., cx).w(px(38.)).h(px(45.)).into_any_element() } else { attach_button(attach.is_some(), cx).into_any_element() })
            .child(div().flex_1().min_w_0().py(px(12.)).pr(px(10.)).child(input))
            .child(
                icon(if picker.as_ref().is_some_and(|p| p.1) { "icons/wink-face.svg" } else { "icons/smile-face.svg" })
                    .id("composer-emoji")
                    .cursor_pointer()
                    .on_click(cx.listener(|app, _, window, cx| app.toggle_picker(None, window, cx))),
            )
            .child(icon("icons/mic-on.svg")),
    )
    .when_some(attach.filter(|_| !editing), |d, opened| d.child(attachment_panel(opened, cx)))
    // `.emoji-picker`: 10px above the composer, 16px from the right edge.
    .when_some(picker, |d, (p, _)| {
        d.child(gpui::deferred(div().absolute().bottom(gpui::relative(1.)).right(px(16.)).pb(px(10.)).child(p)).with_priority(3))
    })
}

/// `#chat-input-file`: the plus, turned to a cross while the panel is open.
fn attach_button(open: bool, cx: &mut Context<VectorApp>) -> impl IntoElement {
    use gpui::AnimationExt as _;
    let turn = move |t: f32| {
        // cubic-bezier(0.34, 1.56, 0.64, 1): an overshoot, eased back.
        let k = 1. - (1. - t).powi(3) + 0.35 * (std::f32::consts::PI * t).sin() * (1. - t);
        if open { k } else { 1. - k }
    };
    div()
        .id("chat-input-file")
        .w(px(38.))
        .h(px(45.))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .on_click(cx.listener(|app, _, _, cx| app.toggle_attach(cx)))
        .child(
            svg()
                .path("icons/plus.svg")
                .size(px(23.))
                .text_color(c(ICON_MUTED))
                .with_animation(if open { "attach-open" } else { "attach-close" }, gpui::Animation::new(std::time::Duration::from_millis(400)), move |s, t| {
                    s.with_transformation(gpui::Transformation::rotate(gpui::radians(turn(t) * std::f32::consts::FRAC_PI_4)))
                }),
        )
}

/// `.attachment-panel`: File, Folder, Commands and Mini Apps, risen from behind the composer.
fn attachment_panel(opened: std::time::Instant, cx: &mut Context<VectorApp>) -> impl IntoElement {
    const H: f32 = 107.;
    let t = (opened.elapsed().as_secs_f32() / 0.3).min(1.);
    // cubic-bezier(0.4, 0, 0.2, 1)
    let e = 1. - (1. - t).powi(3);
    let item = |i: usize, id: &'static str, icon: &'static str, size: f32, label: &'static str| {
        let it = ((opened.elapsed().as_secs_f32() - i as f32 * 0.08) / 0.3).clamp(0., 1.);
        div()
            .id(id)
            .relative()
            .left(px(-10. * (1. - it)))
            .opacity(it)
            .flex()
            .flex_col()
            .items_center()
            .gap(px(4.))
            .cursor_pointer()
            .group(id)
            .child(
                div()
                    .size(px(58.))
                    .rounded(px(12.))
                    .bg(white_a(0.1))
                    .border_1()
                    .border_color(gpui::transparent_black())
                    .group_hover(id, |s| s.bg(white_a(0.18)).border_color(c(PRIMARY)))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(svg().path(icon).size(px(size)).text_color(white_a(0.7))),
            )
            .child(div().text_size(px(11.)).line_height(px(11.)).font_weight(FontWeight::MEDIUM).text_color(white_a(0.7)).whitespace_nowrap().child(label))
    };
    if t < 1. || opened.elapsed().as_secs_f32() < 0.6 {
        let me = cx.entity().downgrade();
        cx.defer(move |cx| {
            me.update(cx, |_, cx| cx.notify()).ok();
        });
    }
    div()
        .absolute()
        .left(px(8.))
        .right(px(8.))
        .bottom(gpui::relative(1.))
        .h(px(H + 10.))
        .overflow_hidden()
        .child(
            div()
                .id("attachment-panel")
                .absolute()
                .left_0()
                .right_0()
                .top(px((H + 10.) * (1. - e)))
                .h(px(H))
                .opacity(e)
                .p(px(16.))
                .bg(c(0x0c0c0c))
                .border_1()
                .border_color(c(DIVIDER))
                .rounded(px(12.))
                .flex()
                .gap(px(12.))
                .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(item(0, "attach-file", "icons/file.svg", 26., "File").on_click(cx.listener(|app, _, window, cx| app.pick_file(window, cx))))
                .child(item(1, "attach-folder", "icons/folder.svg", 27., "Folder").on_click(cx.listener(|app, _, window, cx| app.pick_folder(window, cx))))
                .child(item(2, "attach-commands", "ui/slash.svg", 22.5, "Commands"))
                .child(item(3, "attach-miniapps", "icons/grid-outline.svg", 22., "Mini Apps")),
        )
}

impl VectorApp {
    fn toggle_attach(&mut self, cx: &mut Context<Self>) {
        self.attach = match self.attach {
            Some(_) => None,
            None => {
                self.picker_open = false;
                Some(std::time::Instant::now())
            }
        };
        cx.notify();
    }

    /// Pick a file for the open chat and preview it before it sends.
    fn pick_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.attach = None;
        cx.notify();
        let rx = cx.prompt_for_paths(gpui::PathPromptOptions { files: true, directories: false, multiple: false, prompt: None });
        let window = window.window_handle();
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else { return };
            let Some(path) = paths.into_iter().next() else { return };
            window
                .update(cx, |_, window, cx| {
                    this.update(cx, |this, cx| this.open_file_preview(path, window, cx)).ok();
                })
                .ok();
        })
        .detach();
    }

    /// A pasted picture is written out first, so it previews and sends as any picked file does.
    fn on_paste(&mut self, pasted: &crate::composer::Pasted, window: &mut Window, cx: &mut Context<Self>) {
        let path = match pasted {
            crate::composer::Pasted::Paths(p) => return self.open_paths_preview(p.clone(), window, cx),
            crate::composer::Pasted::Image { bytes, ext } => {
                let Ok(dir) = backend::archive::work_slot() else { return };
                let path = dir.join(format!("pasted_image.{ext}"));
                if std::fs::write(&path, bytes).is_err() {
                    return;
                }
                path
            }
        };
        self.open_file_preview(path, window, cx);
    }

    /// One file previews as itself; a folder, or several items, as one zip.
    fn pick_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.attach = None;
        cx.notify();
        let rx = cx.prompt_for_paths(gpui::PathPromptOptions { files: false, directories: true, multiple: false, prompt: None });
        let window = window.window_handle();
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else { return };
            window
                .update(cx, |_, window, cx| {
                    this.update(cx, |this, cx| this.open_paths_preview(paths, window, cx)).ok();
                })
                .ok();
        })
        .detach();
    }

    fn open_paths_preview(&mut self, mut paths: Vec<std::path::PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        match paths.len() {
            0 => {}
            1 if paths[0].is_file() => self.open_file_preview(paths.remove(0), window, cx),
            _ => self.open_preview(window, cx, move |chat, reply, window, cx| crate::filepreview::FilePreview::zipped(paths, chat, reply, window, cx)),
        }
    }

    fn open_file_preview(&mut self, path: std::path::PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.open_preview(window, cx, move |chat, reply, window, cx| crate::filepreview::FilePreview::new(path, chat, reply, window, cx));
    }

    fn open_preview(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        make: impl FnOnce(String, String, &mut Window, &mut Context<crate::filepreview::FilePreview>) -> crate::filepreview::FilePreview,
    ) {
        let Some(chat) = self.open.clone() else { return };
        let reply = match &self.mode {
            Mode::Reply { id, .. } => id.clone(),
            _ => String::new(),
        };
        let preview = cx.new(|cx| make(chat.to_string(), reply, window, cx));
        cx.subscribe_in(&preview, window, |this, _, e: &crate::filepreview::Event, window, cx| {
            this.file_preview = None;
            window.focus(&this.input.focus_handle(cx), cx);
            cx.notify();
            match e {
                crate::filepreview::Event::Send(send) => this.send_file(send, cx),
                crate::filepreview::Event::Close(path) => backend::archive::forget(path),
                crate::filepreview::Event::Failed(err) => this.notice("Compression Failed", err.clone(), cx),
            }
        })
        .detach();
        self.file_preview = Some(preview);
        cx.notify();
    }

    fn send_file(&mut self, send: &crate::filepreview::Send, cx: &mut Context<Self>) {
        if matches!(self.mode, Mode::Reply { .. }) {
            self.mode = Mode::Normal;
        }
        if !self.live {
            backend::archive::forget(&send.path);
            return;
        }
        let crate::filepreview::Send { path, receiver, reply, name, compress, keep_metadata, compressed } = send;
        let (path, receiver, reply, name, compress, keep_metadata) = (path.clone(), receiver.clone(), reply.clone(), name.clone(), *compress, *keep_metadata);
        // The preview's own compression serves only the default strip-and-compress send.
        let wait = compressed.clone().filter(|_| compress && !keep_metadata);
        cx.spawn(async move |this, cx| {
            let precompressed = match wait {
                Some(w) => w.await,
                None => None,
            };
            let rx = backend::run(messaging::send_file(receiver, reply, path.clone(), name, compress, keep_metadata, precompressed));
            let done = rx.await;
            backend::archive::forget(&path);
            let err = match done {
                Ok(Ok(())) => return,
                Ok(Err(e)) => e,
                Err(e) => e.to_string(),
            };
            if err.contains("Upload cancelled") {
                return;
            }
            this.update(cx, |this, cx| this.notice("Upload Failed", err, cx)).ok();
        })
        .detach();
    }
}

// ── profile ─────────────────────────────────────────────────────────────────────────────

impl VectorApp {
    fn open_profile(&mut self, npub: String, cx: &mut Context<Self>) {
        let back = self.open.take();
        if self.live {
            backend::prefs::refresh(npub.clone());
        }
        self.profile = Some(crate::profile::Screen::new(npub, back));
        cx.notify();
    }

    fn on_profile(&mut self, act: crate::profile::Act, window: &mut Window, cx: &mut Context<Self>) {
        use crate::profile::Act;
        let Some(p) = self.profile.as_mut() else { return };
        let npub = p.npub.clone();
        match act {
            Act::Back => {
                let back = p.back.clone();
                self.profile = None;
                match back {
                    Some(id) => self.open_chat(id, window, cx),
                    None => cx.notify(),
                }
            }
            Act::Message => self.open_chat(npub.into(), window, cx),
            Act::ToggleMute => {
                if !self.live {
                    if let Some(chat) = self.store.borrow_mut().chat_mut(&npub) {
                        chat.muted = !chat.muted;
                    }
                    cx.notify();
                    return;
                }
                let rx = backend::run(backend::prefs::toggle_mute(npub.clone()));
                cx.spawn(async move |this, cx| {
                    let Ok(Some(muted)) = rx.await else { return };
                    this.update(cx, |this, cx| {
                        if let Some(chat) = this.store.borrow_mut().chat_mut(&npub) {
                            chat.muted = muted;
                        }
                        cx.notify();
                    })
                    .ok();
                })
                .detach();
            }
            Act::Share | Act::CopyNpub => {
                let (text, done) = if act == Act::Share {
                    (format!("https://vectorapp.io/profile/{npub}"), "Profile Link Copied")
                } else {
                    (npub.clone(), "Copied npub")
                };
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
                p.copied = Some((act, std::time::Instant::now()));
                self.show_toast(done, cx);
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(std::time::Duration::from_millis(2010)).await;
                    this.update(cx, |_, cx| cx.notify()).ok();
                })
                .detach();
            }
            Act::More => {
                p.more_open = !p.more_open;
                cx.notify();
            }
            Act::Nickname => {
                p.more_open = false;
                let input = cx.new(|cx| TextInput::new("Nickname", cx).with_look(kit::field_look()).keep_on_submit().submit_empty());
                cx.subscribe_in(&input, window, |app, _, _: &SendText, window, cx| app.answer_popup(true, window, cx)).detach();
                cx.subscribe_in(&input, window, |app, _, _: &CancelInput, window, cx| app.answer_popup(false, window, cx)).detach();
                window.focus(&input.focus_handle(cx), cx);
                let mut popup = kit::Popup::notice("Choose a Nickname");
                popup.icon = None;
                popup.confirm = "Confirm".into();
                popup.cancel = Some("Cancel".into());
                popup.input = Some(input);
                self.popup = Some((popup, PopupAct::Nickname { npub }));
                cx.notify();
            }
            Act::Block => {
                p.more_open = false;
                let blocked = self.store.borrow().profiles.get(&npub).is_some_and(|p| p.is_blocked);
                if blocked {
                    self.set_blocked(npub, false, cx);
                } else {
                    let mut popup = kit::Popup::notice("Block User");
                    popup.text = Some("Are you sure you want to block this user? You will no longer receive DMs from them.".into());
                    popup.confirm = "Confirm".into();
                    popup.cancel = Some("Cancel".into());
                    self.popup = Some((popup, PopupAct::Block { npub }));
                    cx.notify();
                }
            }
            Act::SetStatus => self.open_status(window, cx),
            Act::Qr => {
                let side = (288. * window.scale_factor()).round() as u32;
                if let Some(image) = crate::qr::image(&format!("https://vectorapp.io/profile/{npub}"), side) {
                    let focus = cx.focus_handle();
                    window.focus(&focus, cx);
                    self.qr = Some((image, focus));
                    cx.notify();
                }
            }
            Act::Edit => {
                let Some(me) = self.store.borrow().me().cloned() else { return };
                let look = crate::composer::Look { line_height: px(20.), color: c(TEXT), ..kit::field_look() };
                let name = cx.new(|cx| {
                    let mut i = TextInput::new("", cx).with_look(look.clone()).centered().keep_on_submit();
                    i.set_text(&me.name, cx);
                    i
                });
                let bio = cx.new(|cx| {
                    let mut i = TextInput::new("", cx).with_look(look).centered().multiline(px(100000.)).keep_on_submit();
                    i.set_text(&me.about, cx);
                    i
                });
                for input in [&name, &bio] {
                    cx.subscribe(input, |_, _, _: &crate::composer::InputChanged, cx| cx.notify()).detach();
                }
                window.focus(&name.focus_handle(cx), cx);
                p.edit = Some(crate::profile::Edit {
                    name,
                    bio,
                    removed_banner: false,
                    snap_name: me.name.trim().to_string(),
                    snap_about: me.about.trim().to_string(),
                    avatar: None,
                    banner: None,
                });
                cx.notify();
            }
            Act::RemoveBanner => {
                if let Some(e) = p.edit.as_mut() {
                    e.removed_banner = true;
                }
                cx.notify();
            }
            Act::Pick(banner) => self.pick_picture(banner, window, cx),
            Act::ExitEdit(cancel) => {
                let Some(mut e) = p.edit.take() else { return };
                cx.notify();
                if cancel {
                    return;
                }
                for (pick, banner) in [(e.avatar.take(), false), (e.banner.take(), true)] {
                    if let Some(pick) = pick {
                        self.save_picture(&npub, pick, banner, cx);
                    }
                }
                let name = e.name.read(cx).text().trim().to_string();
                let about = e.bio.read(cx).text().trim().to_string();
                let (name_changed, about_changed) = (name != e.snap_name, about != e.snap_about);
                if let Some(me) = self.store.borrow_mut().profiles.get_mut(&npub) {
                    me.name = name.clone();
                    me.about = about.clone();
                    if e.removed_banner {
                        me.banner.clear();
                        me.banner_cached.clear();
                    }
                }
                self.show_toast("Profile Saved", cx);
                if !self.live || !(name_changed || about_changed || e.removed_banner) {
                    return;
                }
                // Empty leaves a field as it is; a blank clears it.
                let name = if name_changed { name } else { String::new() };
                let banner = if e.removed_banner { " ".to_string() } else { String::new() };
                let about = if about_changed { if about.is_empty() { " ".to_string() } else { about } } else { String::new() };
                let rx = backend::run(vector_core::profile::sync::update_profile(name, String::new(), banner, about, &sync::ProfileHandler));
                cx.spawn(async move |this, cx| {
                    if let Ok(true) = rx.await {
                        return;
                    }
                    this.update(cx, |this, cx| {
                        let mut popup = kit::Popup::notice("Profile Update Failed!");
                        popup.text = Some("Failed to broadcast profile update to the network.".into());
                        this.popup = Some((popup, PopupAct::Notice));
                        cx.notify();
                    })
                    .ok();
                })
                .detach();
            }
            Act::Switcher => {
                if self.switcher.is_some() {
                    self.close_switcher(cx);
                } else {
                    self.open_switcher(cx);
                }
            }
            Act::SwitcherEdit => {
                if let Some(s) = self.switcher.as_mut() {
                    s.editing = !s.editing;
                    let editing = s.editing;
                    if let Some(p) = self.profile.as_mut() {
                        p.switcher = Some(editing);
                    }
                    cx.notify();
                }
            }
        }
    }

    fn open_switcher(&mut self, cx: &mut Context<Self>) {
        let active = self.store.borrow().my_npub.clone();
        if !self.live {
            // Fixtures: ourselves and the first two contacts by name, as the reference mock lists them.
            let store = self.store.borrow();
            let mut others: Vec<&vector_core::SlimProfile> = store.profiles.values().filter(|p| p.id != active).collect();
            others.sort_by_key(|p| p.name.to_lowercase());
            let rows = std::iter::once(store.profiles.get(&active))
                .flatten()
                .chain(others.into_iter().take(2))
                .map(|p| crate::switcher::Row {
                    npub: p.id.to_string(),
                    name: [&p.nickname, &p.display_name, &p.name].into_iter().find(|n| !n.is_empty()).map(|n| n.to_string()),
                    avatar: store.avatar_path(&p.id),
                })
                .collect();
            drop(store);
            self.show_switcher(rows, active, false, cx);
            return;
        }
        let rx = backend::run(async { (account::accounts(), account::account_cap()) });
        cx.spawn(async move |this, cx| {
            let Ok((accounts, cap)) = rx.await else { return };
            this.update(cx, |this, cx| {
                let at_cap = accounts.len() >= cap;
                let rows = accounts
                    .into_iter()
                    .map(|a| crate::switcher::Row { npub: a.npub, name: a.name, avatar: a.avatar.map(Into::into) })
                    .collect();
                this.show_switcher(rows, active, at_cap, cx);
            })
            .ok();
        })
        .detach();
    }

    fn show_switcher(&mut self, rows: Vec<crate::switcher::Row>, active: String, at_cap: bool, cx: &mut Context<Self>) {
        let Some(p) = self.profile.as_mut() else { return };
        // A refresh after a removal keeps delete mode.
        let editing = self.switcher.as_ref().is_some_and(|s| s.editing);
        p.switcher = Some(editing);
        self.switcher = Some(crate::switcher::Switcher { rows, active, editing, at_cap });
        cx.notify();
    }

    fn close_switcher(&mut self, cx: &mut Context<Self>) {
        self.switcher = None;
        if let Some(p) = self.profile.as_mut() {
            p.switcher = None;
        }
        cx.notify();
    }

    fn on_switcher(&mut self, act: crate::switcher::Act, window: &mut Window, cx: &mut Context<Self>) {
        use crate::switcher::Act;
        match act {
            Act::Close => self.close_switcher(cx),
            Act::Pick(npub) => {
                self.close_switcher(cx);
                if !self.live {
                    return;
                }
                let rx = backend::run(account::switch(npub));
                cx.spawn_in(window, async move |this, cx| {
                    let result = rx.await.unwrap_or_else(|_| Err("Backend stopped".into()));
                    this.update_in(cx, |this, window, cx| match result {
                        Ok(()) => this.reboot(window, cx),
                        Err(e) => this.notice("Switch failed", e.into(), cx),
                    })
                    .ok();
                })
                .detach();
            }
            Act::Delete(npub) => {
                let Some(s) = self.switcher.as_ref() else { return };
                let row = s.rows.iter().find(|r| r.npub == npub);
                let label = row.and_then(|r| r.name.clone()).unwrap_or_else(|| "This account".into());
                let last = s.rows.len() == 1;
                let mut text = format!(
                    "{label} will be permanently removed from this device, with the files it keeps in Vector. Make sure you have the seed phrase or nsec backed up if you want to recover it later, and save any files you want to keep."
                );
                if last {
                    text.push_str("\n\nThis is your only Vector account on this device. All downloaded attachments will also be removed. Copy any files you want to keep before continuing.");
                }
                let mut popup = kit::Popup::notice("Remove Profile?");
                popup.text = Some(text.into());
                popup.confirm = "Confirm".into();
                popup.cancel = Some("Cancel".into());
                self.popup = Some((popup, PopupAct::RemoveAccount { npub }));
                cx.notify();
            }
            Act::Add => {
                self.close_switcher(cx);
                if !self.live {
                    return;
                }
                let me = self.store.borrow().my_npub.clone();
                self.adding = Some((me, false));
                let view = cx.new(|cx| LoginView::adding(window, cx));
                self.attach_login(view, cx, window);
            }
        }
    }

    fn remove_account(&mut self, npub: String, window: &mut Window, cx: &mut Context<Self>) {
        if !self.live {
            if let Some(s) = self.switcher.as_mut() {
                s.rows.retain(|r| r.npub != npub);
            }
            return;
        }
        let rx = backend::run(account::delete(npub));
        cx.spawn_in(window, async move |this, cx| {
            let result = rx.await.unwrap_or_else(|_| Err("Backend stopped".into()));
            this.update_in(cx, |this, window, cx| match result {
                Ok(true) => this.reboot(window, cx),
                Ok(false) => this.open_switcher(cx),
                Err(e) => this.notice("Delete failed", e, cx),
            })
            .ok();
        })
        .detach();
    }

    fn on_add_profile(&mut self, e: &crate::login::AddProfile, window: &mut Window, cx: &mut Context<Self>) {
        use crate::login::AddProfile;
        let Some((back, committed)) = self.adding.as_mut() else { return };
        match e {
            AddProfile::Committed => *committed = true,
            AddProfile::Back if !*committed => {
                self.adding = None;
                self.screen = Screen::Main;
                cx.notify();
            }
            AddProfile::Back => {
                let back = back.clone();
                self.adding = None;
                let _ = account::set_active(&back);
                self.reboot(window, cx);
            }
        }
    }

    /// Pick a new banner or avatar in Edit Mode; an avatar is cropped square first.
    fn pick_picture(&mut self, banner: bool, window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(if banner { "Choose Banner Image" } else { "Choose Profile Picture" }.into()),
        });
        let window = window.window_handle();
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else { return };
            let Some(path) = paths.into_iter().next() else { return };
            let Some(ext) = backend::upload::picture_ext(&path) else {
                this.update(cx, |this, cx| this.notice("Unsupported File", "Only PNG, JPEG, GIF and WebP images can be used.".into(), cx)).ok();
                return;
            };
            let file = path.clone();
            let decoded = cx
                .background_executor()
                .spawn(async move { std::fs::read(&file).ok().and_then(|b| crate::media::decode(&b, &ext, (2048, 2048))) })
                .await;
            let Some((image, w, h)) = decoded else {
                this.update(cx, |this, cx| this.notice("Image Unreadable", "That picture couldn't be opened.".into(), cx)).ok();
                return;
            };
            let image = Arc::new(image);
            window
                .update(cx, |_, window, cx| {
                    this.update(cx, |this, cx| {
                        let Some(edit) = this.profile.as_mut().and_then(|p| p.edit.as_mut()) else { return };
                        if banner {
                            edit.banner = Some(crate::profile::Pick { path, crop: None, preview: image });
                            cx.notify();
                            return;
                        }
                        let cropper = cx.new(|cx| {
                            crate::cropper::Cropper::new(image, (w, h), "Crop Profile Picture", "Drag to move, or drag a corner to resize.", true, window, cx)
                        });
                        cx.subscribe_in(&cropper, window, |this, _, answer: &crate::cropper::Answer, window, cx| this.on_crop(answer, window, cx)).detach();
                        this.cropper = Some((cropper, path));
                        cx.notify();
                    })
                    .ok();
                })
                .ok();
        })
        .detach();
    }

    /// The avatar as it will upload, previewed in place.
    fn on_crop(&mut self, answer: &crate::cropper::Answer, _: &mut Window, cx: &mut Context<Self>) {
        let Some((_, path)) = self.cropper.take() else { return };
        cx.notify();
        let crate::cropper::Answer::Crop(crop) = answer else { return };
        let crop = crop.clone();
        cx.spawn(async move |this, cx| {
            let file = path.clone();
            let c2 = crop.clone();
            let preview = cx
                .background_executor()
                .spawn(async move {
                    let bytes = std::fs::read(&file).map_err(|e| e.to_string())?;
                    let (out, ext) = backend::upload::prepared(bytes, false, Some(c2))?;
                    crate::media::decode(&out, ext, (512, 512)).map(|(i, _, _)| i).ok_or_else(|| "Couldn't read the image".to_string())
                })
                .await;
            this.update(cx, |this, cx| match preview {
                Ok(image) => {
                    if let Some(edit) = this.profile.as_mut().and_then(|p| p.edit.as_mut()) {
                        edit.avatar = Some(crate::profile::Pick { path, crop: Some(crop), preview: Arc::new(image) });
                        cx.notify();
                    }
                }
                Err(e) => this.notice("Crop Failed", e, cx),
            })
            .ok();
        })
        .detach();
    }

    /// Upload a picked picture and publish it; an uncropped banner shows as picked meanwhile.
    fn save_picture(&mut self, npub: &str, pick: crate::profile::Pick, banner: bool, cx: &mut Context<Self>) {
        let label = if banner { "Banner" } else { "Avatar" };
        let prev = self.store.borrow().profiles.get(npub).map(|p| p.banner_cached.clone());
        if banner && let Some(me) = self.store.borrow_mut().profiles.get_mut(npub) {
            me.banner_cached = pick.path.to_string_lossy().into_owned();
        }
        if !self.live {
            return;
        }
        let rx = backend::run(backend::upload::picture(pick.path, banner, pick.crop));
        let npub = npub.to_string();
        cx.spawn(async move |this, cx| {
            let err = match rx.await {
                Ok(Ok(_)) => return,
                Ok(Err(e)) => e,
                Err(e) => e.to_string(),
            };
            this.update(cx, |this, cx| {
                if banner && let (Some(me), Some(prev)) = (this.store.borrow_mut().profiles.get_mut(&npub), prev) {
                    me.banner_cached = prev;
                }
                this.notice(&format!("{label} Upload Failed!"), err, cx);
            })
            .ok();
        })
        .detach();
    }

    fn set_blocked(&mut self, npub: String, block: bool, cx: &mut Context<Self>) {
        let done = if block { "User Blocked" } else { "User Unblocked" };
        if !self.live {
            if let Some(p) = self.store.borrow_mut().profiles.get_mut(&npub) {
                p.is_blocked = block;
            }
            self.show_toast(done, cx);
            return;
        }
        let rx = if block { backend::run(backend::prefs::block(npub.clone())) } else { backend::run(backend::prefs::unblock(npub.clone())) };
        cx.spawn(async move |this, cx| {
            let Ok(true) = rx.await else { return };
            this.update(cx, |this, cx| {
                if let Some(p) = this.store.borrow_mut().profiles.get_mut(&npub) {
                    p.is_blocked = block;
                }
                this.show_toast(done, cx);
            })
            .ok();
        })
        .detach();
    }

    fn set_nickname(&mut self, npub: String, nick: String, cx: &mut Context<Self>) {
        if nick.chars().count() >= 30 {
            let mut popup = kit::Popup::notice("Woah woah!");
            popup.text = Some(format!("A {}-character nickname seems excessive!", nick.chars().count()).into());
            self.popup = Some((popup, PopupAct::Notice));
            cx.notify();
            return;
        }
        if let Some(p) = self.store.borrow_mut().profiles.get_mut(&npub) {
            p.nickname = nick.clone();
        }
        cx.notify();
        if self.live {
            let _ = backend::run(backend::prefs::set_nickname(npub, nick));
        }
    }

    /// `showToast`: one line over a bottom gradient, for 1.5s plus 40ms a character.
    fn show_toast(&mut self, text: &str, cx: &mut Context<Self>) {
        let at = std::time::Instant::now();
        self.toast = Some((text.to_string().into(), at));
        let hold = (1500 + text.chars().count() as u64 * 40).min(6000);
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(std::time::Duration::from_millis(hold)).await;
            this.update(cx, |_, cx| cx.notify()).ok();
            cx.background_executor().timer(std::time::Duration::from_millis(320)).await;
            this.update(cx, |this, cx| {
                if this.toast.as_ref().is_some_and(|(_, t)| *t == at) {
                    this.toast = None;
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn toast_view(&self, window: &mut Window) -> Option<AnyElement> {
        let (text, at) = self.toast.as_ref()?;
        let hold = (1500 + text.chars().count() as u64 * 40).min(6000) as f32 / 1000.;
        let e = at.elapsed().as_secs_f32();
        let opacity = if e < 0.3 { e / 0.3 } else if e < hold { 1. } else { (1. - (e - hold) / 0.3).max(0.) };
        if opacity < 1. && opacity > 0. {
            window.request_animation_frame();
        }
        Some(
            gpui::deferred(
                div()
                    .absolute()
                    .inset_0()
                    .opacity(opacity)
                    .child(div().absolute().inset_0().bg(gpui::linear_gradient(
                        180.,
                        gpui::linear_color_stop(ca(0, 0, 0, 0.), 0.5),
                        gpui::linear_color_stop(ca(0, 0, 0, 0.8), 1.),
                    )))
                    .child(
                        div().absolute().left_0().right_0().bottom(px(80.)).flex().justify_center().child(
                            div()
                                .px(px(24.))
                                .py(px(12.))
                                .rounded(px(8.))
                                .border_1()
                                .border_color(c(PRIMARY))
                                .bg(ca(0, 0, 0, 0.8))
                                .text_size(px(14.))
                                .line_height(px(24.))
                                .text_color(c(0xffffff))
                                .child(text.clone()),
                        ),
                    ),
            )
            .with_priority(20)
            .into_any_element(),
        )
    }
}

// ── #status-dialog ──────────────────────────────────────────────────────────────────────

/// A status is one line of at most this many characters.
const STATUS_MAX: usize = 120;

impl VectorApp {
    fn open_status(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.store.borrow().me().map(|p| p.status.title.clone()).unwrap_or_default();
        let look = crate::composer::Look {
            family: RUBIK,
            size: px(14.5),
            line_height: px(20.),
            color: c(0xffffff),
            placeholder: white_a(0.3),
            ..Default::default()
        };
        let input = cx.new(|cx| {
            let mut i = TextInput::new("What's happening?", cx).with_look(look).twemoji().keep_on_submit().submit_empty();
            i.set_text(&current, cx);
            i
        });
        cx.subscribe(&input, |app, _, _: &SendText, cx| app.save_status(None, cx)).detach();
        cx.subscribe_in(&input, window, |app, _, _: &CancelInput, window, cx| {
            if app.picker_open && app.picker_status {
                app.close_picker(window, cx);
                return;
            }
            app.status = None;
            cx.notify();
        })
        .detach();
        cx.subscribe(&input, |_, input, _: &crate::composer::InputChanged, cx| {
            let text = input.read(cx).text().to_string();
            let clean: String = text.replace('\n', " ").chars().take(STATUS_MAX).collect();
            if clean != text {
                input.update(cx, |i, cx| i.set_text(&clean, cx));
            }
            cx.notify();
        })
        .detach();
        window.focus(&input.focus_handle(cx), cx);
        self.status = Some(input);
        cx.notify();
    }

    /// Publish the status (`Some("")` clears it), echoing it at once and rolling back on failure.
    fn save_status(&mut self, text: Option<String>, cx: &mut Context<Self>) {
        let Some(input) = self.status.take() else { return };
        if self.picker_status {
            self.picker_open = false;
            self.picker_status = false;
        }
        let status = text.unwrap_or_else(|| input.read(cx).text().trim().to_string());
        let me = self.store.borrow().my_npub.clone();
        let old = self.store.borrow_mut().profiles.get_mut(&me).map(|p| std::mem::replace(&mut p.status.title, status.clone()));
        cx.notify();
        if !self.live {
            return;
        }
        let rx = backend::run(vector_core::profile::sync::update_status(status));
        cx.spawn(async move |this, cx| {
            if let Ok(true) = rx.await {
                return;
            }
            this.update(cx, |this, cx| {
                if let (Some(old), Some(p)) = (old, this.store.borrow_mut().profiles.get_mut(&me)) {
                    p.status.title = old;
                }
                let mut popup = kit::Popup::notice("Status Update Failed!");
                popup.text = Some("Failed to broadcast status update to the network.".into());
                this.popup = Some((popup, PopupAct::Notice));
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn status_view(&self, store: &Store, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let input = self.status.clone()?;
        let panel = self.picker_open && self.picker_status;
        let raw = input.read(cx).text().to_string();
        let text = raw.trim().to_string();
        let empty = text.is_empty();
        let remaining = STATUS_MAX as i64 - raw.chars().count() as i64;
        let had = store.me().is_some_and(|p| !p.status.title.is_empty());
        let card = div()
            .id("status-dialog-card")
            .w(px(380.))
            .flex()
            .flex_col()
            .pt(px(14.))
            .px(px(16.))
            .pb(px(12.))
            .rounded(px(18.))
            .border_1()
            .border_color(c(0x2c2e2f))
            .bg(c(0x101010))
            .shadow(vec![gpui::BoxShadow { color: ca(0, 0, 0, 0.55), offset: gpui::point(px(0.), px(18.)), blur_radius: px(60.), spread_radius: px(0.), inset: false }])
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .h(px(24.))
                    .mb(px(12.))
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(div().text_size(px(11.)).font_weight(FontWeight::BOLD).text_color(white_a(0.45)).child("STATUS"))
                    .child(
                        div()
                            .id("status-dialog-x")
                            .px(px(6.))
                            .py(px(4.))
                            .text_size(px(14.))
                            .line_height(px(14.))
                            .text_color(white_a(0.4))
                            .hover(|s| s.text_color(c(0xffffff)))
                            .cursor_pointer()
                            .on_click(cx.listener(|app, _, _, cx| {
                                app.status = None;
                                cx.notify();
                            }))
                            .child("✕"),
                    ),
            )
            .child(
                div()
                    .mb(px(12.))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .min_w_0()
                    .child(div().size(px(34.)).flex_shrink_0().rounded_full().bg(c(0x1a1a1a)).child(avatar(store, &store.my_npub, 34.)))
                    .child(
                        div()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .px(px(14.))
                            .py(px(7.))
                            .rounded_full()
                            .bg(c(0x1a1a1a))
                            .border_1()
                            .border_color(c(0x262626))
                            .when(empty, |d| d.opacity(0.45))
                            .child(
                                div()
                                    .size(px(8.))
                                    .flex_shrink_0()
                                    .rounded_full()
                                    .bg(c(if empty { 0x555555 } else { ACCENT }))
                                    .when(!empty, |d| {
                                        d.shadow(vec![gpui::BoxShadow { color: ca(89, 252, 179, 0.6), offset: gpui::point(px(0.), px(0.)), blur_radius: px(6.), spread_radius: px(0.), inset: false }])
                                    }),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(px(13.5))
                                    .line_height(px(24.))
                                    .text_color(c(0xffffff))
                                    .child(crate::inline::emoji_line(if empty { "No status" } else { &text }, crate::inline::Look::new(INTER, 13.5, 24., c(0xffffff)), 16., 3.)),
                            ),
                    ),
            )
            .child(
                div()
                    .w_full()
                    .flex()
                    .items_center()
                    .p(px(2.))
                    .pl(px(14.))
                    .rounded(px(12.))
                    .bg(c(0x1a1a1a))
                    .border_1()
                    .border_color(c(0x3d4040))
                    .child(div().flex_1().min_w_0().py(px(9.)).px(px(4.)).child(input))
                    .child(
                        div()
                            .min_w(px(22.))
                            .text_right()
                            .text_size(px(11.))
                            .text_color(if remaining <= 10 { c(0xffb347) } else { white_a(0.35) })
                            .when(remaining <= 30, |d| d.child(remaining.to_string())),
                    )
                    .child(
                        div()
                            .id("status-emoji-btn")
                            .ml(px(2.))
                            .size(px(40.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .opacity(0.7)
                            .hover(|s| s.opacity(1.))
                            .cursor_pointer()
                            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(cx.listener(|app, _, window, cx| app.toggle_status_picker(window, cx)))
                            .child(svg().path(if self.picker_open && self.picker_status { "icons/wink-face.svg" } else { "icons/smile-face.svg" }).size(px(20.)).text_color(c(0xffffff))),
                    ),
            )
            .child(
                div()
                    .mt(px(12.))
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .id("status-btn-clear")
                            .px(px(2.))
                            .py(px(6.))
                            .text_size(px(12.5))
                            .text_color(white_a(0.4))
                            .hover(|s| s.text_color(c(0xffffff)))
                            .cursor_pointer()
                            .when(!had, |d| d.invisible())
                            .on_click(cx.listener(|app, _, _, cx| app.save_status(Some(String::new()), cx)))
                            .child("Clear Status"),
                    )
                    .child(
                        div()
                            .id("status-btn-save")
                            .px(px(26.))
                            .py(px(9.))
                            .rounded(px(10.))
                            .bg(c(ACCENT))
                            .text_color(c(0x06130d))
                            .text_size(px(14.))
                            .line_height(px(17.))
                            .font_weight(FontWeight::BOLD)
                            .cursor_pointer()
                            .on_click(cx.listener(|app, _, _, cx| app.save_status(None, cx)))
                            .child("Save"),
                    ),
            );
        Some(
            gpui::deferred(
                div()
                    .id("status-dialog")
                    .absolute()
                    .inset_0()
                    .flex()
                    .justify_center()
                    .map(|d| if panel { d.items_start().pt(px(24f32.max(f32::from(window.viewport_size().height) * 0.07))) } else { d.items_center() })
                    .bg(ca(0, 0, 0, 0.72))
                    .occlude()
                    .on_mouse_down(
                        gpui::MouseButton::Left,
                        cx.listener(|app, _, window, cx| {
                            // The first outside press dismisses the panel; the next, the dialog.
                            if app.picker_open && app.picker_status {
                                app.close_picker(window, cx);
                                return;
                            }
                            app.status = None;
                            cx.notify();
                        }),
                    )
                    .child(card)
                    .when(panel, |d| {
                        d.child(
                            div()
                                .absolute()
                                .left_0()
                                .right_0()
                                .bottom(px(10.))
                                .flex()
                                .justify_center()
                                .child(div().on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation()).child(self.emoji_picker.clone())),
                        )
                    }),
            )
            .with_priority(10)
            .into_any_element(),
        )
    }
}

// ── #qr-overlay ─────────────────────────────────────────────────────────────────────────

impl VectorApp {
    fn qr_view(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (image, focus) = self.qr.clone()?;
        let close = |app: &mut Self, cx: &mut Context<Self>| {
            app.qr = None;
            cx.notify();
        };
        let card = div()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(22.))
            .p(px(24.))
            .rounded(px(18.))
            .border_1()
            .border_color(c(0x2c2e2f))
            .bg(c(0x101010))
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(div().w(px(320.)).p(px(16.)).rounded(px(10.)).bg(c(0xffffff)).child(img(image).size(px(288.))))
            .child(
                div()
                    .id("qr-overlay-close")
                    .w_full()
                    .py(px(12.))
                    .rounded(px(12.))
                    .bg(c(ACCENT))
                    .text_color(c(0x000000))
                    .text_size(px(16.))
                    .line_height(px(19.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_center()
                    .cursor_pointer()
                    .on_click(cx.listener(move |app, _, _, cx| close(app, cx)))
                    .child("Close"),
            );
        Some(
            gpui::deferred(
                div()
                    .id("qr-overlay")
                    .track_focus(&focus)
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(ca(0, 0, 0, 0.88))
                    .occlude()
                    .on_key_down(cx.listener(move |app, e: &gpui::KeyDownEvent, _, cx| {
                        if e.keystroke.key == "escape" {
                            close(app, cx);
                        }
                    }))
                    .on_mouse_down(gpui::MouseButton::Left, cx.listener(move |app, _, _, cx| close(app, cx)))
                    .child(card),
            )
            .with_priority(10)
            .into_any_element(),
        )
    }
}
