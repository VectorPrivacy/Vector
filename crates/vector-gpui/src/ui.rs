//! The widescreen shell: window chrome, rail, chat list and the open chat. Every size and
//! offset is the Tauri layout's own (measured from `reference/` renders); the comment on a
//! block names the CSS selector it reproduces.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use gpui::AnimationExt;
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
pub(crate) const RAIL_W: f32 = 201.;
pub(crate) const LIST_W: f32 = 239.;
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
    badge_card: Option<crate::badgecard::Card>,
    new_chat: Option<crate::newchat::NewChat>,
    /// Add Profile is showing the login screen over this account: the account Back returns to,
    /// and whether the old session was already torn down for the new one.
    adding: Option<(String, bool)>,
    /// The list shows Requests rather than Messages; `tab_slid` once a pick moved the bar.
    requests_tab: bool,
    tab_slid: bool,
    /// The request senders last asked about for shared Communities.
    mutuals_asked: Vec<String>,
    /// Viktor, while the Messages list is empty.
    viktor: Option<Entity<crate::viktor::Viktor>>,
    settings: Option<crate::settings::Settings>,
    security: Entity<crate::security::SecurityFlow>,
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
    /// Block from a request's bar, which also closes the chat.
    BlockRequest { npub: String },
    Nickname { npub: String },
    RemoveAccount { npub: String },
    Unblock { npub: String },
    Logout,
    ClearStorage,
    AddServer,
    DisableRelay { url: String },
    RemoveRelay { url: String },
    RemoveServer { url: String },
    /// Delete one slice of the storage chart: its category and the extensions it counted.
    DeleteSlice { category: &'static str, exts: Vec<String> },
    /// Turn Keep Files off, deleting what it kept.
    StopKeeping,
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
        let security = cx.new(|cx| crate::security::SecurityFlow::new(live, window, cx));
        cx.subscribe_in(&security, window, |app, _, e: &crate::security::Event, window, cx| app.on_security(e, window, cx)).detach();
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
            badge_card: None,
            new_chat: None,
            adding: None,
            requests_tab: false,
            tab_slid: false,
            mutuals_asked: Vec::new(),
            viktor: None,
            settings: None,
            security,
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
        if let Ok(mode) = std::env::var("VECTOR_LOGIN") {
            app.show_login((mode != "welcome").then_some("pin"), window, cx);
        }
        if let Ok(n) = std::env::var("VECTOR_BADGE_CARD").map(|v| v.parse::<usize>().unwrap_or(1))
            && let Some(p) = app.profile.as_ref()
        {
            use crate::profile::Badge;
            let b = p.badges;
            let held: Vec<Badge> = [(b.invites > 0, Badge::Inviter), (b.fawkes, Badge::Fawkes), (b.bug > 0, Badge::BugHunter), (b.v2026, Badge::V2026)]
                .into_iter()
                .filter_map(|(on, w)| on.then_some(w))
                .collect();
            if let Some(w) = held.get(n.saturating_sub(1)) {
                app.on_profile(crate::profile::Act::Badge(*w), window, cx);
            }
        }
        // Fixtures: VECTOR_SETTINGS=<category> opens Settings there.
        if let Ok(cat) = std::env::var("VECTOR_SETTINGS") {
            let cats = crate::settings::categories(&crate::settings::Offers { voice: false, routing: true, updates: true });
            let id = cats.iter().find(|c| c.id == cat).map(|c| c.id).unwrap_or("appearance");
            app.open_settings(id, window, cx);
        }
        // Fixtures: VECTOR_XFER=approve|match opens the transfer card at that step.
        if let Ok(step) = std::env::var("VECTOR_XFER") {
            let send = step != "match";
            let payload = if send {
                serde_json::json!({ "id": 1, "stage": step })
            } else {
                serde_json::json!({ "id": 1, "stage": "match", "sas": "482913", "sender": "npub1ytm2q9jl8kcn4ajqqv2d6xtqy9pyzhml3s7d9dsg7nv8x8ufsjhsn5xzkl", "name": "jskitty" })
            };
            app.security.update(cx, |f, cx| {
                f.open_transfer(send, cx);
                f.transfer_state(&payload, cx);
            });
        }
        if let Ok(pick) = std::env::var("VECTOR_NEW_CHAT") {
            app.open_new_chat(window, cx);
            if let Ok(n) = pick.parse::<usize>()
                && n > 0
            {
                let first = {
                    let nc = app.new_chat.as_ref().unwrap();
                    let store = app.store.borrow();
                    crate::newchat::rows_for(nc, &store).get(n - 1).cloned()
                };
                if let Some(npub) = first {
                    app.on_new_chat(crate::newchat::Act::Toggle(npub), window, cx);
                }
            }
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
        self.attach_login(view.clone(), cx, window);
        if !self.live {
            let rows = self.fixture_accounts();
            let me = self.store.borrow().my_npub.clone();
            let open = std::env::var("VECTOR_LOGIN").is_ok_and(|m| m == "picker");
            view.update(cx, |v, cx| v.set_accounts(rows, Some(me), open, cx));
            return;
        }
        let rx = backend::run(async { (account::accounts(), vector_core::db::read_active_account_file().ok().flatten()) });
        cx.spawn(async move |_, cx| {
            let Ok((accounts, active)) = rx.await else { return };
            let rows = accounts.into_iter().map(|a| crate::switcher::Row { npub: a.npub, name: a.name, avatar: a.avatar.map(Into::into) }).collect();
            view.update(cx, |v, cx| v.set_accounts(rows, active, false, cx));
        })
        .detach();
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
        cx.subscribe(&view, |this, _, _: &crate::login::OpenTransfer, cx| this.security.update(cx, |f, cx| f.open_transfer(false, cx))).detach();
        cx.subscribe_in(&view, window, |this, _, crate::login::PickAccount(npub), window, cx| {
            if !this.live {
                return;
            }
            let rx = backend::run(account::switch(npub.clone()));
            cx.spawn_in(window, async move |this, cx| {
                let result = rx.await.unwrap_or_else(|_| Err("Backend stopped".into()));
                this.update_in(cx, |this, window, cx| match result {
                    Ok(()) => this.reboot(window, cx),
                    Err(e) => log::error!("account switch failed: {e}"),
                })
                .ok();
            })
            .detach();
        })
        .detach();
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
        let rx = backend::run(async { backend::storage::auto_download() });
        cx.spawn(async move |this, cx| {
            let Ok((on, limit)) = rx.await else { return };
            this.update(cx, |this, _| this.media.borrow_mut().auto_download = crate::media::AutoDownload { on, limit }).ok();
        })
        .detach();
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
        self.sync_request_mutuals(cx);
        cx.notify();
    }

    fn on_core_event(&mut self, name: &str, payload: Value, _window: &mut Window, cx: &mut Context<Self>) {
        let chat_id = payload.get("chat_id").and_then(Value::as_str).unwrap_or_default().to_string();
        match name {
            "transfer_state" => self.security.update(cx, |f, cx| f.transfer_state(&payload, cx)),
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
        self.new_chat = None;
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
            PopupAct::BlockRequest { npub } => {
                if self.open.as_deref() == Some(npub.as_str()) {
                    self.open = None;
                }
                self.set_blocked(npub, true, cx);
            }
            PopupAct::RemoveAccount { npub } => self.remove_account(npub, window, cx),
            PopupAct::Unblock { npub } => {
                if let Some(s) = self.settings.as_mut()
                    && let Some(list) = s.blocked.as_mut()
                {
                    list.retain(|u| u.npub != npub);
                }
                self.set_blocked(npub, false, cx);
            }
            PopupAct::ClearStorage => {
                if let Some(s) = self.settings.as_mut() {
                    s.clearing = true;
                }
                let rx = backend::run(backend::storage::clear_all());
                self.after_storage_clear(rx, false, cx);
            }
            PopupAct::DeleteSlice { category, exts } => {
                if let Some(s) = self.settings.as_mut() {
                    s.donut.busy = true;
                }
                let rx = backend::run(backend::storage::clear_category(category, exts));
                self.after_storage_clear(rx, true, cx);
            }
            PopupAct::AddServer => {
                let Some(input) = popup.input else { return };
                let url = input.read(cx).text().trim().to_string();
                if url.is_empty() {
                    return;
                }
                let rx = backend::run(backend::network::add_server(url));
                cx.spawn(async move |this, cx| {
                    let result = rx.await.unwrap_or_else(|_| Err("cancelled".into()));
                    this.update(cx, |this, cx| {
                        if let Err(e) = result {
                            let mut popup = kit::Popup::notice("Failed to Add Server");
                            popup.text = Some(e.into());
                            this.popup = Some((popup, PopupAct::Notice));
                        }
                        this.load_network(cx);
                        cx.notify();
                    })
                    .ok();
                })
                .detach();
            }
            PopupAct::DisableRelay { url } => self.net_change(backend::run(async move { backend::network::toggle_default_relay(url, false).await.map(|_| ()) }), true, cx),
            PopupAct::RemoveRelay { url } => self.net_change(backend::run(async move { backend::network::remove_custom_relay(url).await.map(|_| ()) }), true, cx),
            PopupAct::RemoveServer { url } => self.net_change(backend::run(async move { backend::network::remove_server(url).await.map(|_| ()) }), true, cx),
            PopupAct::StopKeeping => self.set_keep_media(false, cx),
            PopupAct::Logout => {
                let me = self.store.borrow().my_npub.clone();
                self.settings = None;
                self.remove_account(me, window, cx);
            }
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
                return root
                    .child(div().absolute().inset_0().child(view.clone()))
                    .child(div().absolute().top_0().left_0().right_0().child(chrome(window, true)))
                    .child(self.security.clone());
            }
            Screen::Main => {}
        }
        let store = self.store.clone();
        let store = store.borrow();
        let no_messages = !store.chats.iter().any(|c| store.listed(c) && !store.is_request(c));
        if no_messages != self.viktor.is_some() {
            self.viktor = no_messages.then(|| {
                static GREETED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
                let greet = !GREETED.swap(true, std::sync::atomic::Ordering::Relaxed);
                cx.new(|cx| crate::viktor::Viktor::new(146., greet, cx))
            });
        }
        let net_dialog = self.settings.as_ref().and_then(|s| s.dialog.as_ref()).map(|d| {
            let app = cx.entity().downgrade();
            let on: crate::netdialog::OnAct = std::rc::Rc::new(move |act, window, cx| {
                app.update(cx, |app, cx| app.on_net(act, window, cx)).ok();
            });
            crate::netdialog::view(d, &on, cx)
        });
        let popup = self.popup.as_ref().map(|(p, _)| {
            let app = cx.entity().downgrade();
            kit::popup(p, move |yes, window, cx| {
                app.update(cx, |app, cx| app.answer_popup(yes, window, cx)).ok();
            })
        });
        let root = root.on_drop(cx.listener(|app, paths: &gpui::ExternalPaths, window, cx| {
            app.open_paths_preview(paths.paths().to_vec(), window, cx);
        }));
        root.children(net_dialog).children(popup).child(self.security.clone()).child(chrome(window, false)).child(
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
                .when_some(self.settings.as_ref(), |row, settings| {
                    let app = cx.entity().downgrade();
                    let on: crate::settings::OnAct = Rc::new(move |act, window, cx| {
                        app.update(cx, |app, cx| app.on_settings(act, window, cx)).ok();
                    });
                    let width = f32::from(window.viewport_size().width) - RAIL_W;
                    row.child(div().flex_1().min_w_0().h_full().child(crate::settings::view(settings, width, &on, cx)))
                })
                .when(self.settings.is_none(), |row| row.child(chat_list(&self.store, &store, self.open.clone(), (self.requests_tab, self.tab_slid, no_messages), cx)))
                .when(self.settings.is_none(), |row| row.child(match (&self.profile, self.open.clone()) {
                    _ if self.new_chat.is_some() => {
                        let app = cx.entity().downgrade();
                        let on: crate::newchat::OnAct = Rc::new(move |act, window, cx| {
                            app.update(cx, |app, cx| app.on_new_chat(act, window, cx)).ok();
                        });
                        let width = f32::from(window.viewport_size().width) - RAIL_W - LIST_W;
                        div().flex_1().min_w_0().h_full().child(crate::newchat::view(self.new_chat.as_ref().unwrap(), &store, width, &on, cx)).into_any_element()
                    }
                    (Some(p), _) => {
                        let app = cx.entity().downgrade();
                        let on: crate::profile::OnAct = Rc::new(move |act, window, cx| {
                            app.update(cx, |app, cx| app.on_profile(act, window, cx)).ok();
                        });
                        crate::profile::pane(&store, p, &on, window, cx)
                    }
                    (None, Some(id)) => self.chat_pane(&store, id, cx).into_any_element(),
                    (None, None) => empty_pane(self.viktor.clone()).into_any_element(),
                })),
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
        .children(self.badge_card.as_ref().map(|card| {
            if card.opened.elapsed().as_secs_f32() < 0.45 {
                window.request_animation_frame();
            }
            let app = cx.entity().downgrade();
            let size = window.viewport_size();
            gpui::deferred(crate::badgecard::view(card, window, cx, move |ev, window, cx| {
                app.update(cx, |app, cx| {
                    use crate::badgecard::Ev;
                    match ev {
                        Ev::Close => app.badge_card = None,
                        Ev::Pointer(p) => {
                            if let Some(card) = app.badge_card.as_mut() {
                                let x = (f32::from(p.x) - f32::from(size.width) / 2.) / 179.;
                                let y = (f32::from(p.y) - f32::from(size.height) / 2.) / 230.;
                                card.tilt = gpui::point(x.clamp(-1., 1.), y.clamp(-1., 1.));
                            }
                        }
                        Ev::Leave => {
                            if let Some(card) = app.badge_card.as_mut() {
                                card.tilt = gpui::Point::default();
                            }
                        }
                    }
                    let _ = window;
                    cx.notify();
                })
                .ok();
            }))
            .with_priority(12)
        }))
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
        .filter(|c| store.listed(c) && store.unread_count(c) > 0 && open.is_none_or(|o| *o != c.id) && !store.is_request(c))
        .collect();
    let any_unread = store.chats.iter().any(|c| store.listed(c) && store.unread_count(c) > 0);

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

fn chat_list(shared: &Shared, store: &Store, open: Option<SharedString>, (requests, slid, no_messages): (bool, bool, bool), cx: &mut Context<VectorApp>) -> impl IntoElement {
    let (mut dot_messages, mut dot_requests) = (false, false);
    let mut ids: Vec<usize> = Vec::new();
    for (i, chat) in store.chats.iter().enumerate() {
        if !store.listed(chat) {
            continue;
        }
        let request = store.is_request(chat);
        if store.unread_count(chat) > 0 {
            if request {
                dot_requests = true;
            } else {
                dot_messages = true;
            }
        }
        if request == requests {
            ids.push(i);
        }
    }
    fn pick(to_requests: bool, cx: &mut Context<VectorApp>) -> impl Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static {
        cx.listener(move |app, _: &gpui::ClickEvent, _, cx| {
            app.requests_tab = to_requests;
            app.tab_slid = true;
            cx.notify();
        })
    }
    let (to_messages, to_requests) = (pick(false, cx), pick(true, cx));
    let tab = |label: &'static str, on: bool, dot: bool, click| {
        div()
            .id(label)
            .flex_1()
            .h(px(40.))
            .flex()
            .items_center()
            .justify_center()
            .font_family(RUBIK)
            .text_size(px(14.))
            .font_weight(if on { FontWeight::BOLD } else { FontWeight::MEDIUM })
            .text_color(if on { c(0xffffff) } else { white_a(0.38) })
            .when(!on, |d| {
                d.cursor_pointer().hover(|s| s.text_color(white_a(0.75))).on_click(click)
            })
            .child(
                div().relative().child(label).when(dot && !on, |d| {
                    d.child(div().absolute().top(px(-1.)).right(px(-9.)).size(px(8.)).rounded_full().bg(c(ACCENT)))
                }),
            )
    };
    // .cc-tab-bar: slides between the halves, but never on the first paint.
    let half = (LIST_W - 23.) / 2.;
    let to = if requests { half } else { 0. };
    let bar = div().absolute().bottom(px(-1.)).w(px(half)).h(px(2.)).bg(c(PRIMARY));
    let bar = if slid {
        bar.with_animation(
            SharedString::from(format!("cc-tab-bar-{requests}")),
            gpui::Animation::new(std::time::Duration::from_millis(240)).with_easing(crate::kit::bezier(0.2, 0.8, 0.2, 1.)),
            move |d, t| d.left(px(half - to + (2. * to - half) * t)),
        )
        .into_any_element()
    } else {
        bar.left(px(to)).into_any_element()
    };
    let empty = if !ids.is_empty() {
        None
    } else if requests {
        // .cc-empty
        Some(
            div()
                .mx(px(24.))
                .my(px(28.))
                .text_size(px(14.))
                .line_height(px(20.))
                .text_center()
                .text_color(white_a(0.45))
                .child("New people who message you show up here.")
                .into_any_element(),
        )
    } else {
        no_messages.then(|| get_started(cx).into_any_element())
    };
    let count = ids.len();
    let ids = Rc::new(ids);
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
                        .id("chatlist-new")
                        .size(px(32.))
                        .rounded(px(8.))
                        .border_1()
                        .border_color(white_a(0.12))
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .on_click(cx.listener(|app, _, window, cx| app.open_new_chat(window, cx)))
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
                .relative()
                .child(tab("Messages", !requests, dot_messages, to_messages))
                .child(tab("Requests", requests, dot_requests, to_requests))
                .child(bar),
        )
        .children(empty)
        .child(
            div().flex_1().min_h_0().pt(px(6.)).child(
                uniform_list("chatlist", count, move |range, _, _| {
                    let store = store.borrow();
                    range
                        .filter_map(|i| ids.get(i).and_then(|&i| store.chats.get(i)))
                        .map(|chat| {
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

/// `.chatlist-get-started`: the empty list's first step, which rides the New Chat button.
fn get_started(cx: &mut Context<VectorApp>) -> impl IntoElement {
    div().pt(px(6.)).px(px(7.)).child(
        div()
            .id("chatlist-get-started")
            .relative()
            .overflow_hidden()
            .h(px(50.))
            .flex()
            .items_center()
            .gap(px(10.))
            .rounded_full()
            .border_1()
            .border_color(ca(89, 252, 179, 0.5))
            .bg(c(0x000000))
            .cursor_pointer()
            .on_click(cx.listener(|app, _, window, cx| app.open_new_chat(window, cx)))
            .child(
                div().absolute().top_0().bottom_0().left(gpui::relative(0.35)).right_0().rounded_r_full().bg(gpui::linear_gradient(
                    90.,
                    gpui::linear_color_stop(ca(89, 252, 179, 0.), 0.),
                    gpui::linear_color_stop(ca(89, 252, 179, 0.3), 1.),
                )),
            )
            .child(
                div()
                    .ml(px(5.))
                    .size(px(38.))
                    .flex_shrink_0()
                    .rounded_full()
                    .border_1()
                    .border_color(c(0x191919))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(svg().path("icons/add-user.svg").size(px(17.)).text_color(c(ACCENT))),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .pr(px(12.))
                    .truncate()
                    .text_size(px(15.))
                    .line_height(px(18.))
                    .font_weight(FontWeight::BOLD)
                    .text_color(c(0xf7f4f4))
                    .child("Get Started"),
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

fn empty_pane(viktor: Option<Entity<crate::viktor::Viktor>>) -> AnyElement {
    if let Some(viktor) = viktor {
        return div().flex_1().h_full().flex().items_center().justify_center().child(intro(viktor)).into_any_element();
    }
    div()
        .flex_1()
        .h_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .child(img("ui/logo-mark.svg").w(px(48.)).h(px(80.)).opacity(0.25))
        .child(div().mt(px(12.)).text_size(px(14.)).text_color(white_a(0.35)).child("Pick a conversation to get started"))
        .into_any_element()
}

/// `.chatlist-intro` in the empty conversation pane: Viktor's welcome, pointing at the Hub.
fn intro(viktor: Entity<crate::viktor::Viktor>) -> impl IntoElement {
    const TEXT_P: &str = "Feel free to join the public community to learn more about Vector, discuss privacy, and make some new friends.";
    let link = 13..13 + "join the public community".len();
    let accent = gpui::HighlightStyle { color: Some(c(ACCENT)), ..Default::default() };
    let words = gpui::InteractiveText::new("intro-text", gpui::StyledText::new(TEXT_P).with_highlights([(link.clone(), accent)]))
        .on_click(vec![link], |_, _, cx| cx.open_url("https://vectorapp.io/hub"));
    div()
        .max_w(px(448.))
        .px(px(24.))
        .flex()
        .flex_col()
        .items_center()
        .gap(px(18.))
        .text_center()
        .child(viktor)
        .child(
            div()
                .flex()
                .flex_col()
                .items_center()
                .child(div().mb(px(10.)).text_size(px(24.)).line_height(px(29.)).font_weight(FontWeight::BOLD).text_color(c(0xf7f4f4)).child("Welcome to Vector!"))
                .child(div().text_size(px(15.)).line_height(px(15. * 1.55)).text_color(white_a(0.55)).child(words)),
        )
        .with_animation("intro-in", gpui::Animation::new(std::time::Duration::from_millis(400)).with_easing(kit::bezier(0.22, 1., 0.36, 1.)), |d, t| {
            d.opacity(t).mt(px(8. * (1. - t)))
        })
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
                held: rows_store.is_request(chat),
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

        let request = store.chat(&id).is_some_and(|chat| store.is_request(chat)).then(|| request_bar(id.to_string(), store.pictures_held(&id), cx));
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
                    .pt(px(if request.is_some() { 12. + 56. } else { 12. }))
                    .child(messages)
                    .children(request)
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

/// `.request-bar`: under the header of a DM from someone new, Block or Accept.
fn request_bar(npub: String, pictures: bool, cx: &mut Context<VectorApp>) -> impl IntoElement {
    let button = |id: &'static str, label: &'static str| {
        div()
            .id(id)
            .flex_shrink_0()
            .h(px(34.))
            .px(px(12.))
            .flex()
            .items_center()
            .rounded(px(9.))
            .border_1()
            .cursor_pointer()
            .text_size(px(14.))
            .font_weight(FontWeight::SEMIBOLD)
            .child(label)
    };
    let (block, accept) = (npub.clone(), npub);
    div()
        .id("request-bar")
        .occlude()
        .absolute()
        .top_0()
        .left_0()
        .right_0()
        .h(px(56.))
        .flex()
        .items_center()
        .gap(px(8.))
        .pl(px(12.))
        .pr(px(10.))
        .bg(c(0x0d0d0d))
        .border_b_1()
        .border_color(white_a(0.12))
        .child(svg().path("ui/request-shield.svg").size(px(22.)).flex_shrink_0().text_color(c(ACCENT)))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(1.))
                .child(div().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).line_height(px(17.)).text_color(c(0xffffff)).child("Message request"))
                .child(
                    div()
                        .truncate()
                        .text_size(px(13.))
                        .line_height(px(16.))
                        .text_color(white_a(0.7))
                        .child(if pictures { "Accept to load media & profile" } else { "Accept to load media" }),
                ),
        )
        .child(
            button("request-block", "Block")
                .border_color(c(timeline::DANGER).opacity(0.6))
                .text_color(c(timeline::DANGER))
                .on_click(cx.listener(move |app, _, _, cx| app.confirm_block_request(block.clone(), cx))),
        )
        .child(
            button("request-accept", "Accept")
                .bg(c(PRIMARY))
                .border_color(gpui::transparent_black())
                .text_color(c(0x000000))
                .on_click(cx.listener(move |app, _, _, cx| app.accept_request(accept.clone(), cx))),
        )
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
        self.new_chat = None;
        if self.live {
            backend::prefs::refresh(npub.clone());
        }
        self.profile = Some(crate::profile::Screen::new(npub.clone(), back));
        cx.notify();
        if !self.live {
            // Fixtures: VECTOR_BADGES=invites,fawkes,bug,v2026 (counts; 0 for none) on our own profile.
            if let Ok(spec) = std::env::var("VECTOR_BADGES")
                && npub == self.store.borrow().my_npub
            {
                let n: Vec<u32> = spec.split(',').map(|v| v.trim().parse().unwrap_or(0)).collect();
                let at = |i: usize| n.get(i).copied().unwrap_or(0);
                if let Some(p) = self.profile.as_mut() {
                    p.badges = backend::badges::Badges { invites: at(0), fawkes: at(1) > 0, bug: at(2) as u8, v2026: at(3) > 0 };
                }
            }
            return;
        }
        let mine = npub == self.store.borrow().my_npub;
        let rx = backend::run(backend::badges::of(npub.clone(), mine));
        cx.spawn(async move |this, cx| {
            let Ok(badges) = rx.await else { return };
            this.update(cx, |this, cx| {
                if let Some(p) = this.profile.as_mut().filter(|p| p.npub == npub) {
                    p.badges = badges;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
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
            Act::Badge(which) => {
                self.badge_card = Some(crate::badgecard::Card::of(which, p.badges));
                cx.notify();
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
            let rows = self.fixture_accounts();
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

    fn open_settings(&mut self, cat: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        use crate::settings::{Home, Settings};
        let search = cx.new(|cx| TextInput::new("Search", cx).with_look(crate::composer::Look { size: px(14.), line_height: px(17.), weight: FontWeight::NORMAL, placeholder: c(0xb2b2b2), ..kit::field_look() }));
        cx.subscribe_in(&search, window, |_, _, _: &crate::composer::InputChanged, _, cx| cx.notify()).detach();
        let seen = backend::device::get("settings_seen").and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok()).unwrap_or_default();
        let sql_keys: Vec<(&'static str, bool)> = crate::settings::toggles()
            .filter_map(|t| match t.home {
                Home::Sql(default) => Some((t.key, default)),
                _ => None,
            })
            .collect();
        let sql = if self.live { backend::prefs::sql_bools(&sql_keys) } else { sql_keys.iter().map(|(_, d)| *d).collect() };
        let mut on: HashMap<&'static str, bool> = sql_keys.iter().map(|(k, _)| *k).zip(sql).collect();
        on.insert("encryption", self.security.update(cx, |f, _| f.sync()));
        for t in crate::settings::toggles() {
            if let Home::Device(yes, no, default) = t.home {
                on.insert(t.key, match backend::device::get(t.key) {
                    Some(v) if v == yes => true,
                    Some(v) if v == no => false,
                    _ => default,
                });
            }
        }
        let synced = if self.live { Some(backend::prefs::synced_settings()) } else { None };
        on.insert("advanced", synced.as_ref().is_some_and(|s| s.advanced));
        on.insert("streamer_on", synced.as_ref().is_some_and(|s| s.streamer.on));
        on.insert("streamer_wallpapers", synced.as_ref().is_none_or(|s| s.streamer.hide_wallpapers));
        let me = self.store.borrow().my_npub.clone();
        on.insert("stream_consent", self.store.borrow().profiles.get(&me).is_some_and(|p| p.stream_consent));
        let theme = if self.live { backend::prefs::sql("theme") } else { None }.unwrap_or_else(|| "vector".into());
        let notif_privacy = if self.live { backend::prefs::sql("notif_content_privacy") } else { None }.unwrap_or_else(|| "full".into());
        self.profile = None;
        self.new_chat = None;
        self.switcher = None;
        self.settings = Some(Settings {
            cat,
            search,
            scroll: gpui::ScrollHandle::new(),
            offers: crate::settings::Offers { voice: false, routing: vector_core::transport::supported().len() > 1, updates: true },
            theme,
            open_select: None,
            on,
            flipped: HashSet::new(),
            streamer_notif: synced.map(|s| s.streamer.notif).unwrap_or_else(|| "none".into()),
            notif_privacy,
            storage: None,
            relays: None,
            servers: None,
            clock: None,
            dialog: None,
            sec_kind: self.security.read(cx).kind().to_string(),
            sec_advanced: false,
            donut: Default::default(),
            auto_limit: if self.live { backend::prefs::sql("max_auto_download_bytes").and_then(|v| v.parse().ok()).filter(|v: &u64| *v > 0) } else { None }.unwrap_or(10_485_760),
            clearing: false,
            notif_sound: if self.live { backend::prefs::sql("notif_sound") } else { None }.unwrap_or_else(|| "default".into()),
            sound_custom: false,
            blocked: None,
            seen: seen.into_iter().collect(),
            jumped: None,
        });
        self.load_blocked(cx);
        self.load_storage(cx);
        self.load_network(cx);
        self.mark_settings_seen(None);
        cx.notify();
    }

    fn on_net(&mut self, act: crate::netdialog::Act, _window: &mut Window, cx: &mut Context<Self>) {
        use crate::netdialog::{Act, NetDialog};
        let Some(s) = self.settings.as_mut() else { return };
        let Some(d) = s.dialog.as_mut() else { return };
        cx.notify();
        match (act, d) {
            (Act::Close, _) => s.dialog = None,
            (Act::Mode(m), NetDialog::AddRelay { mode, .. }) => *mode = m,
            (Act::Add, NetDialog::AddRelay { input, mode }) => {
                let typed = input.read(cx).text().trim().to_string();
                if typed.is_empty() {
                    let mut popup = kit::Popup::notice("Invalid URL");
                    popup.text = Some("Please enter a relay URL.".into());
                    self.popup = Some((popup, PopupAct::Notice));
                    return;
                }
                // Plain ws:// only for .i2p and .onion hosts, where the network encrypts end to end.
                let lower = typed.to_lowercase();
                let tls = lower.starts_with("wss://");
                let bare = typed.get(if tls { 6 } else if lower.starts_with("ws://") { 5 } else { 0 }..).unwrap_or("").to_string();
                let host = bare.split(['/', ':', '?', '#']).next().unwrap_or("").to_lowercase();
                let native = host.ends_with(".i2p") || host.ends_with(".onion");
                let url = format!("{}{bare}", if native && !tls { "ws://" } else { "wss://" });
                let mode = mode.to_string();
                let rx = backend::run(async move { backend::network::add_custom_relay(url, Some(mode)).await.map(|_| ()) });
                cx.spawn(async move |this, cx| {
                    let result = rx.await.unwrap_or_else(|_| Err("cancelled".into()));
                    this.update(cx, |this, cx| {
                        match result {
                            Ok(()) => {
                                if let Some(s) = this.settings.as_mut() {
                                    s.dialog = None;
                                }
                            }
                            Err(e) => {
                                let mut popup = kit::Popup::notice("Failed to Add Relay");
                                popup.text = Some(e.into());
                                this.popup = Some((popup, PopupAct::Notice));
                            }
                        }
                        this.load_network(cx);
                        cx.notify();
                    })
                    .ok();
                })
                .detach();
            }
            (Act::ToggleModeList, NetDialog::Relay { mode_open, .. }) => *mode_open = !*mode_open,
            (Act::RelayMode(m), NetDialog::Relay { row, mode_open, .. }) => {
                *mode_open = false;
                if row.is_default || row.mode == m {
                    return;
                }
                row.mode = m.to_string();
                let (url, mode) = (row.url.clone(), m.to_string());
                self.net_change(backend::run(async move { backend::network::update_relay_mode(url, mode).await.map(|_| ()) }), false, cx);
            }
            (Act::CopyLogs, NetDialog::Relay { row, logs, copied, .. }) => {
                let text = logs
                    .iter()
                    .map(|l| {
                        let at = chrono::DateTime::from_timestamp(l.timestamp as i64, 0).map(|t| t.with_timezone(&chrono::Local).format("%-I:%M:%S %p").to_string()).unwrap_or_default();
                        format!("[{at}] {}: {}", l.level.to_uppercase(), l.message)
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(format!("Relay: {}\n\n{text}", row.url)));
                *copied = true;
            }
            (Act::RelayAction, NetDialog::Relay { row, .. }) => {
                let url = row.url.clone();
                if row.is_default && row.enabled {
                    let mut popup = kit::Popup::notice("Disable Default Relay?");
                    popup.text = Some("This is a <b>default relay</b>. Disabling it may affect message delivery and sync reliability.<br><br>Are you sure you want to disable it?".into());
                    popup.confirm = "Confirm".into();
                    popup.cancel = Some("Cancel".into());
                    self.popup = Some((popup, PopupAct::DisableRelay { url }));
                } else if row.is_default {
                    self.net_change(backend::run(async move { backend::network::toggle_default_relay(url, true).await.map(|_| ()) }), true, cx);
                } else {
                    let mut popup = kit::Popup::notice("Remove Relay?");
                    popup.text = Some(format!("Are you sure you want to remove <b>{}</b>?", crate::settings::host(&url)).into());
                    popup.confirm = "Confirm".into();
                    popup.cancel = Some("Cancel".into());
                    self.popup = Some((popup, PopupAct::RemoveRelay { url }));
                }
            }
            (Act::ServerAction, NetDialog::Server { info }) => {
                let url = info.url.clone();
                if info.is_custom {
                    let mut popup = kit::Popup::notice("Remove media server?");
                    popup.text = Some(format!("<b>{url}</b> will be removed from your list. Existing uploads on that server remain accessible.").into());
                    popup.confirm = "Confirm".into();
                    popup.cancel = Some("Cancel".into());
                    self.popup = Some((popup, PopupAct::RemoveServer { url }));
                } else {
                    let enabled = !info.enabled;
                    info.enabled = enabled;
                    self.net_change(backend::run(async move { backend::network::toggle_server(url, enabled).await }), false, cx);
                }
            }
            _ => {}
        }
    }

    /// Apply a relay or server change, then reread the lists; `close` ends the dialog on success.
    fn net_change(&mut self, rx: futures::channel::oneshot::Receiver<Result<(), String>>, close: bool, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let result = rx.await.unwrap_or_else(|_| Err("cancelled".into()));
            this.update(cx, |this, cx| {
                match result {
                    Ok(()) if close => {
                        if let Some(s) = this.settings.as_mut() {
                            s.dialog = None;
                        }
                    }
                    Ok(()) => {}
                    Err(e) => {
                        let mut popup = kit::Popup::notice("Error");
                        popup.text = Some(e.into());
                        this.popup = Some((popup, PopupAct::Notice));
                    }
                }
                this.load_network(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The relay dialog's status, ping and log, every second while it is open.
    fn refresh_relay_dialog(&mut self, cx: &mut Context<Self>) {
        use crate::netdialog::NetDialog;
        let Some(NetDialog::Relay { row, .. }) = self.settings.as_ref().and_then(|s| s.dialog.as_ref()) else { return };
        if !self.live {
            return;
        }
        let url = row.url.clone();
        let rx = backend::run(async move {
            vector_core::db::scoped(async move {
                let row = backend::network::relay_rows().await.into_iter().find(|r| r.url.eq_ignore_ascii_case(&url));
                (row, backend::network::relay_metrics_of(&url), backend::network::relay_logs_of(&url))
            })
            .await
        });
        cx.spawn(async move |this, cx| {
            let Ok((fresh, m, l)) = rx.await else { return };
            let open = this
                .update(cx, |this, cx| {
                    let Some(Some(NetDialog::Relay { row, metrics, logs, .. })) = this.settings.as_mut().map(|s| s.dialog.as_mut()) else { return false };
                    if let Some(f) = fresh {
                        row.status = f.status;
                        row.enabled = f.enabled;
                    }
                    *metrics = m;
                    *logs = l;
                    cx.notify();
                    true
                })
                .unwrap_or(false);
            if open {
                cx.background_executor().timer(std::time::Duration::from_secs(1)).await;
                this.update(cx, |this, cx| this.refresh_relay_dialog(cx)).ok();
            }
        })
        .detach();
    }

    /// Read the relay and server lists and the clock, and keep them fresh while Network shows.
    fn load_network(&mut self, cx: &mut Context<Self>) {
        if !self.live {
            if let Some(s) = self.settings.as_mut() {
                let (relays, servers) = fixture_network();
                s.relays = Some(relays);
                s.servers = Some(servers);
                s.clock = Some(vector_core::clock::Status { decentralised: true, measured_offset_ms: 0, offset_ms: 0, answered: 0, agreeing: 0, checked_at: 0 });
            }
            return;
        }
        let rx = backend::run(async {
            vector_core::db::scoped(async { (backend::network::relay_rows().await, backend::network::server_rows(), vector_core::clock::status()) }).await
        });
        cx.spawn(async move |this, cx| {
            let Ok((relays, servers, clock)) = rx.await else { return };
            let again = this
                .update(cx, |this, cx| {
                    let Some(s) = this.settings.as_mut() else { return false };
                    s.relays = Some(relays);
                    s.servers = Some(servers);
                    s.clock = Some(clock);
                    cx.notify();
                    s.cat == "network"
                })
                .unwrap_or(false);
            if again {
                cx.background_executor().timer(std::time::Duration::from_secs(2)).await;
                this.update(cx, |this, cx| {
                    if this.settings.as_ref().is_some_and(|s| s.cat == "network") {
                        this.load_network(cx);
                    }
                })
                .ok();
            }
        })
        .detach();
    }

    /// Refresh the storage chart; a fresh report drops the chart's selection.
    fn load_storage(&mut self, cx: &mut Context<Self>) {
        if !self.live {
            if let Some(s) = self.settings.as_mut() {
                // Fixtures: `VECTOR_STORAGE=ext:bytes,...` stands in for a disk report.
                let dist = std::env::var("VECTOR_STORAGE").unwrap_or_default();
                s.storage = Some(dist.split(',').filter_map(|kv| kv.split_once(':')).filter_map(|(k, v)| Some((k.to_string(), v.parse().ok()?))).collect());
            }
            return;
        }
        let rx = backend::run(async { vector_core::db::scoped(async { backend::storage::distribution() }).await });
        cx.spawn(async move |this, cx| {
            let Ok(dist) = rx.await else { return };
            this.update(cx, |this, cx| {
                if let Some(s) = this.settings.as_mut() {
                    s.storage = Some(dist);
                    s.donut.hover = None;
                    s.donut.picked = None;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn confirm_storage_delete(&mut self) {
        let Some(s) = self.settings.as_ref() else { return };
        let Some(dist) = s.storage.as_ref() else { return };
        let segs = crate::settings::segments(dist);
        let Some(seg) = s.donut.picked.and_then(|i| segs.get(i)) else { return };
        let cat = &crate::settings::STORE_CATS[seg.cat];
        let size = crate::media::format_bytes(seg.size);
        let text = match cat.key {
            Some("/ai_models") => format!("This will delete {size} of downloaded AI models from this device.<br><br>Voice transcription will download a model again when needed."),
            Some(_) => format!("This will delete {size} of cached avatars, banners and images.<br><br>Vector rebuilds this cache automatically over time."),
            None => format!("This will delete {size} of {} from this device.<br><br>You can download them again from their chats later, if they are still available.", cat.noun),
        };
        let category = match cat.key {
            Some("/ai_models") => "ai",
            Some(_) => "cache",
            None => "files",
        };
        let exts = crate::settings::slice_exts(seg.cat, dist);
        let mut popup = kit::Popup::notice(cat.title);
        popup.icon = Some("icons/vector_warning.svg");
        popup.text = Some(text.into());
        popup.confirm = "Confirm".into();
        popup.cancel = Some("Cancel".into());
        self.popup = Some((popup, PopupAct::DeleteSlice { category, exts }));
    }

    fn after_storage_clear(&mut self, rx: futures::channel::oneshot::Receiver<Result<u64, String>>, slice: bool, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let result = rx.await.unwrap_or_else(|_| Err("cancelled".into()));
            this.update(cx, |this, cx| {
                if let Some(s) = this.settings.as_mut() {
                    s.clearing = false;
                    s.donut.busy = false;
                }
                match result {
                    Ok(freed) if slice => this.show_toast(&format!("Freed {}", vector_core::crypto::format_bytes(freed)), cx),
                    Ok(_) => {}
                    Err(e) => {
                        let mut popup = kit::Popup::notice(if slice { "Delete Failed" } else { "Clear Failed" });
                        popup.icon = Some("icons/vector_warning.svg");
                        popup.text = Some(if slice { format!("Could not delete: {e}") } else { format!("Could not clear storage: {e}") }.into());
                        this.popup = Some((popup, PopupAct::Notice));
                    }
                }
                this.load_storage(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Keep Files on or off; the switch goes back if the change doesn't happen.
    fn set_keep_media(&mut self, on: bool, cx: &mut Context<Self>) {
        if let Some(s) = self.settings.as_mut() {
            s.on.insert("keep_media", on);
            s.flipped.insert("keep_media");
        }
        if !self.live {
            return;
        }
        let rx = backend::run(backend::storage::set_keep_media(on));
        cx.spawn(async move |this, cx| {
            let result = rx.await.unwrap_or_else(|_| Err("cancelled".into()));
            this.update(cx, |this, cx| {
                if let Err(e) = result {
                    if let Some(s) = this.settings.as_mut() {
                        s.on.insert("keep_media", !on);
                    }
                    let mut popup = kit::Popup::notice("Keep Files");
                    popup.icon = Some("icons/vector_warning.svg");
                    popup.text = Some(e.into());
                    this.popup = Some((popup, PopupAct::Notice));
                } else if !on {
                    this.load_storage(cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Turning Keep Files off asks first when it has kept anything.
    fn toggle_keep_media(&mut self, on: bool, cx: &mut Context<Self>) {
        if on || !self.live {
            return self.set_keep_media(on, cx);
        }
        let rx = backend::run(async { vector_core::db::scoped(backend::storage::kept_summary()).await });
        cx.spawn(async move |this, cx| {
            let (files, bytes) = rx.await.unwrap_or((0, 0));
            this.update(cx, |this, cx| {
                if files == 0 {
                    return this.set_keep_media(false, cx);
                }
                let mut popup = kit::Popup::notice("Stop keeping files?");
                popup.icon = Some("icons/vector_warning.svg");
                popup.text = Some(
                    format!("{files} kept {} ({}) will be deleted from this device. They load again when you open them.", if files == 1 { "file" } else { "files" }, crate::media::format_bytes(bytes)).into(),
                );
                popup.confirm = "Confirm".into();
                popup.cancel = Some("Cancel".into());
                this.popup = Some((popup, PopupAct::StopKeeping));
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Pick, check and store a custom notification sound.
    fn pick_sound(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(gpui::PathPromptOptions { files: true, directories: false, multiple: false, prompt: None });
        let dir = backend::default_data_dir().join("cache").join("sounds");
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else { return };
            let Some(path) = paths.into_iter().next() else { return };
            let imported = cx.background_executor().spawn(async move { backend::sound::import(&path, &dir) }).await;
            this.update(cx, |this, cx| {
                cx.notify();
                let stored = match imported {
                    Ok(p) => p,
                    Err(e) => {
                        let (title, text) = match e.as_str() {
                            "FILE_TOO_LARGE" => ("File Too Large", "Notification sounds must be under 1MB. Please choose a shorter audio clip."),
                            "AUDIO_TOO_LONG" => ("Audio Too Long", "Notification sounds must be 10 seconds or less."),
                            _ => return log::warn!("custom sound: {e}"),
                        };
                        let mut popup = kit::Popup::notice(title);
                        popup.text = Some(text.into());
                        this.popup = Some((popup, PopupAct::Notice));
                        return;
                    }
                };
                let Some(s) = this.settings.as_mut() else { return };
                s.sound_custom = false;
                s.notif_sound = format!("custom:{}", stored.display());
                backend::sound::purge();
                if this.live {
                    backend::prefs::set_sql("notif_sound", s.notif_sound.clone());
                }
            })
            .ok();
        })
        .detach();
    }

    fn load_blocked(&mut self, cx: &mut Context<Self>) {
        let fill = |this: &mut Self, list: Vec<vector_core::SlimProfile>| {
            let store = this.store.borrow();
            let rows = list
                .into_iter()
                .map(|p| {
                    let name = [&p.nickname, &p.name, &p.display_name].into_iter().find(|n| !n.is_empty()).cloned().unwrap_or_default();
                    crate::settings::Blocked { avatar: store.avatar_path(&p.id), npub: p.id, name, veiled: false }
                })
                .collect();
            drop(store);
            if let Some(s) = this.settings.as_mut() {
                s.blocked = Some(rows);
            }
        };
        if !self.live {
            let list = self.store.borrow().profiles.values().filter(|p| p.is_blocked).cloned().collect();
            fill(self, list);
            return;
        }
        let rx = backend::run(vector_core::profile::sync::get_blocked_users());
        cx.spawn(async move |this, cx| {
            let Ok(list) = rx.await else { return };
            this.update(cx, |this, cx| {
                fill(this, list);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The block now being read stops wearing its NEW pill.
    fn mark_settings_seen(&mut self, anchor: Option<&'static str>) {
        let Some(s) = self.settings.as_mut() else { return };
        let cats = crate::settings::categories(&s.offers);
        let Some(cat) = cats.iter().find(|c| c.id == s.cat) else { return };
        let Some(id) = anchor.or(cat.anchors.first().map(|a| a.id)) else { return };
        if !s.fresh_ids(&cats).contains(&id) {
            return;
        }
        s.seen.insert(id.to_string());
        let list: Vec<&String> = s.seen.iter().collect();
        if let Ok(json) = serde_json::to_string(&list) {
            backend::device::set("settings_seen", json);
        }
    }

    fn on_settings(&mut self, act: crate::settings::Act, window: &mut Window, cx: &mut Context<Self>) {
        let into_network = matches!(act, crate::settings::Act::Category("network") | crate::settings::Act::Anchor("network", _)) && self.settings.as_ref().is_some_and(|s| s.cat != "network");
        self.settings_act(act, window, cx);
        if into_network {
            self.load_network(cx);
        }
    }

    fn settings_act(&mut self, act: crate::settings::Act, window: &mut Window, cx: &mut Context<Self>) {
        use crate::settings::{Act, Home};
        let Some(s) = self.settings.as_mut() else { return };
        cx.notify();
        match act {
            Act::Category(id) => {
                s.open_select = None;
                s.jumped = None;
                if s.cat != id {
                    s.cat = id;
                    s.scroll = gpui::ScrollHandle::new();
                    self.mark_settings_seen(None);
                }
            }
            Act::Anchor(cat, anchor) => {
                s.open_select = None;
                if s.cat != cat {
                    s.cat = cat;
                    s.scroll = gpui::ScrollHandle::new();
                }
                let cats = crate::settings::categories(&s.offers);
                if let Some(ix) = cats.iter().find(|c| c.id == cat).and_then(|c| c.anchors.iter().position(|a| a.id == anchor)) {
                    s.scroll.scroll_to_top_of_item(ix);
                }
                s.jumped = Some(anchor);
                self.mark_settings_seen(Some(anchor));
            }
            Act::Scrolled => s.jumped = None,
            Act::Toggle("keep_media", value) => self.toggle_keep_media(value, cx),
            Act::Toggle("encryption", value) => {
                s.on.insert("encryption", value);
                s.flipped.insert("encryption");
                self.security.update(cx, |f, cx| f.toggle(value, cx));
            }
            Act::Toggle("clock_decentralised", value) => {
                s.on.insert("clock_decentralised", value);
                s.flipped.insert("clock_decentralised");
                if self.live {
                    let rx = backend::run(async move { vector_core::db::scoped(async move { vector_core::clock::set_decentralised(value).map(|_| vector_core::clock::status()) }).await });
                    cx.spawn(async move |this, cx| {
                        let Ok(Ok(status)) = rx.await else { return };
                        this.update(cx, |this, cx| {
                            if let Some(s) = this.settings.as_mut() {
                                s.clock = Some(status);
                                cx.notify();
                            }
                        })
                        .ok();
                    })
                    .detach();
                }
            }
            Act::Toggle(key, value) => {
                let Some(t) = crate::settings::toggle_of(key) else { return };
                if key == "auto_download_enabled" {
                    self.media.borrow_mut().auto_download.on = value;
                }
                if key == "notif_global_mute" && value {
                    backend::sound::purge();
                }
                s.on.insert(key, value);
                s.flipped.insert(key);
                match t.home {
                    Home::Device(yes, no, _) => backend::device::set(key, if value { yes } else { no }),
                    Home::Sql(_) if self.live => backend::prefs::set_sql(key, value.to_string()),
                    Home::Synced if self.live => {
                        use vector_core::synced_prefs as sp;
                        let rx = backend::run(async move {
                            backend::prefs::edit_synced(|| match key {
                                "advanced" => sp::set_advanced(value),
                                "streamer_on" => sp::set_streamer_on(value),
                                _ => sp::set_streamer_wallpapers(value),
                            })
                        });
                        self.settle_toggle(key, rx, cx);
                    }
                    Home::Profile if self.live => {
                        let rx = backend::run(async move {
                            vector_core::db::scoped_result(vector_core::profile::sync::set_stream_consent(value, &backend::sync::ProfileHandler)).await
                        });
                        cx.spawn(async move |this, cx| {
                            let stuck = matches!(rx.await, Ok(Ok(true))) == value;
                            this.update(cx, |this, cx| {
                                if let Some(s) = this.settings.as_mut() {
                                    s.on.insert(key, if stuck { value } else { !value });
                                }
                                cx.notify();
                            })
                            .ok();
                        })
                        .detach();
                    }
                    _ => {}
                }
            }
            Act::Help(key) => {
                if let Some((title, text)) = crate::settings::help(key) {
                    let mut popup = kit::Popup::notice(title);
                    popup.icon = None;
                    popup.text = Some(text.into());
                    self.popup = Some((popup, PopupAct::Notice));
                }
            }
            Act::Select(id) => s.open_select = if s.open_select == Some(id) { None } else { Some(id) },
            Act::Pick(id, value) => {
                s.open_select = None;
                match id {
                    "theme" => {
                        s.theme = value.to_string();
                        if self.live {
                            backend::prefs::set_sql("theme", value.to_string());
                        }
                    }
                    "notif-privacy" => {
                        s.notif_privacy = value.to_string();
                        if self.live {
                            backend::prefs::set_sql("notif_content_privacy", value.to_string());
                        }
                    }
                    "auto-limit" => {
                        let bytes: u64 = value.parse().unwrap_or(10_485_760);
                        s.auto_limit = bytes;
                        self.media.borrow_mut().auto_download.limit = bytes;
                        if self.live {
                            backend::prefs::set_sql("max_auto_download_bytes", value.to_string());
                        }
                    }
                    "notif-sound" if value == "custom" => s.sound_custom = true,
                    "notif-sound" => {
                        s.sound_custom = false;
                        s.notif_sound = value.to_string();
                        backend::sound::purge();
                        if self.live {
                            backend::prefs::set_sql("notif_sound", value.to_string());
                        }
                    }
                    "streamer-notif" => {
                        s.streamer_notif = value.to_string();
                        if self.live {
                            let _ = backend::run(async move { backend::prefs::edit_synced(|| vector_core::synced_prefs::set_streamer_notif(value)) });
                        }
                    }
                    _ => {}
                }
            }
            Act::Unblock(npub) => {
                let name = self.store.borrow().name(&npub);
                let mut popup = kit::Popup::notice("Unblock User");
                popup.text = Some(format!("Are you sure you want to unblock {name}?").into());
                popup.confirm = "Confirm".into();
                popup.cancel = Some("Cancel".into());
                self.popup = Some((popup, PopupAct::Unblock { npub }));
            }
            Act::CopyLogs => {
                let log = backend::default_data_dir().join("vector.log");
                match std::fs::read_to_string(log).ok().filter(|l| !l.trim().is_empty()) {
                    Some(text) => {
                        let lines = text.lines().filter(|l| !l.trim().is_empty()).count();
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
                        self.show_toast(&format!("Copied {lines} log entries to clipboard"), cx);
                    }
                    None => self.show_toast("No logs to copy!", cx),
                }
            }
            Act::AddServer => {
                let input = cx.new(|cx| TextInput::new("blossom.primal.net", cx).with_look(kit::field_look()).keep_on_submit());
                cx.subscribe_in(&input, window, |app, _, _: &SendText, window, cx| app.answer_popup(true, window, cx)).detach();
                cx.subscribe_in(&input, window, |app, _, _: &CancelInput, window, cx| app.answer_popup(false, window, cx)).detach();
                window.focus(&input.focus_handle(cx), cx);
                let mut popup = kit::Popup::notice("Add Media Server");
                popup.icon = None;
                popup.text = Some("Enter the address of a Blossom-compatible server. A bare domain like <b>blossom.primal.net</b> works. Vector adds <b>https://</b> automatically.".into());
                popup.confirm = "Confirm".into();
                popup.cancel = Some("Cancel".into());
                popup.input = Some(input);
                self.popup = Some((popup, PopupAct::AddServer));
            }
            Act::AddRelay => {
                let input = cx.new(|cx| TextInput::new("relay.example.com", cx).with_look(kit::field_look()).keep_on_submit());
                cx.subscribe_in(&input, window, |app, _, _: &SendText, window, cx| app.on_net(crate::netdialog::Act::Add, window, cx)).detach();
                cx.subscribe_in(&input, window, |app, _, _: &CancelInput, window, cx| app.on_net(crate::netdialog::Act::Close, window, cx)).detach();
                cx.observe(&input, |_, _, cx| cx.notify()).detach();
                window.focus(&input.focus_handle(cx), cx);
                s.dialog = Some(crate::netdialog::NetDialog::AddRelay { input, mode: "both" });
            }
            Act::OpenRelay(url) => {
                let Some(row) = s.relays.iter().flatten().find(|r| r.url == url).cloned() else { return };
                s.dialog = Some(crate::netdialog::NetDialog::Relay { row, metrics: Default::default(), logs: Vec::new(), mode_open: false, copied: false });
                self.refresh_relay_dialog(cx);
            }
            Act::OpenServer(url) => {
                let Some(info) = s.servers.iter().flatten().find(|r| r.url == url).cloned() else { return };
                s.dialog = Some(crate::netdialog::NetDialog::Server { info });
            }
            Act::DonutHover(at) => s.donut.hover = at,
            Act::DonutPick(i) => s.donut.picked = if s.donut.picked == Some(i) { None } else { Some(i) },
            Act::DonutDelete => self.confirm_storage_delete(),
            Act::ClearStorage => {
                if !s.clearing {
                    let mut popup = kit::Popup::notice("Clear Storage?");
                    popup.icon = Some("icons/vector_warning.svg".into());
                    popup.text = Some("Deletes this account's files, plus the cache. This can't be undone.".into());
                    popup.confirm = "Confirm".into();
                    popup.cancel = Some("Cancel".into());
                    self.popup = Some((popup, PopupAct::ClearStorage));
                }
            }
            Act::PreviewSound => backend::sound::play(backend::sound::Sound::parse(&s.notif_sound)),
            Act::PickSound => self.pick_sound(cx),
            Act::ChangeCredential => self.security.update(cx, |f, cx| f.change(cx)),
            Act::ShowKeys => self.security.update(cx, |f, cx| f.show_keys(cx)),
            Act::Transfer => self.security.update(cx, |f, cx| f.open_transfer(true, cx)),
            Act::SecAdvanced => s.sec_advanced = !s.sec_advanced,
            Act::ClearSound => {
                s.sound_custom = false;
                s.notif_sound = "default".into();
                backend::sound::purge();
                if self.live {
                    backend::prefs::set_sql("notif_sound", "default".into());
                }
            }
            Act::Logout => {
                let mut popup = kit::Popup::notice("Going Incognito?");
                popup.text = Some("Logging out of Vector will fully erase the database, <b>ensure you have a backup of your keys before logging out!</b><br><br>That said, would you like to continue?".into());
                popup.confirm = "Confirm".into();
                popup.cancel = Some("Cancel".into());
                self.popup = Some((popup, PopupAct::Logout));
            }
            Act::Link(key) => cx.open_url(match key {
                "donate" => "https://vector-privacy.gitbook.io/vector-privacy/vector-messenger/more/donations",
                "gitbook" => "https://docs.vectorapp.io",
                _ => "https://vectorapp.io/privacy-policy",
            }),
            Act::ClearSearch => {
                let search = s.search.clone();
                search.update(cx, |input, cx| input.clear(window, cx));
                window.focus(&search.focus_handle(cx), cx);
            }
        }
    }

    /// A synced toggle shows what stuck once the save answers.
    fn settle_toggle(&mut self, key: &'static str, rx: futures::channel::oneshot::Receiver<Result<vector_core::synced_prefs::SyncedSettings, String>>, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let Ok(Ok(view)) = rx.await else { return };
            this.update(cx, |this, cx| {
                if let Some(s) = this.settings.as_mut() {
                    let stuck = match key {
                        "advanced" => view.advanced,
                        "streamer_on" => view.streamer.on,
                        _ => view.streamer.hide_wallpapers,
                    };
                    s.on.insert(key, stuck);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn open_new_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let filter = cx.new(|cx| {
            TextInput::new("Search (paste npub, invite link, etc.)", cx).with_look(crate::composer::Look { placeholder: white_a(0.35), ..kit::field_look() })
        });
        cx.subscribe_in(&filter, window, |app, input, _: &crate::composer::InputChanged, _, cx| {
            let text = input.read(cx).text().to_string();
            app.pasted_npub(&text, cx);
            cx.notify();
        })
        .detach();
        window.focus(&filter.focus_handle(cx), cx);
        let dm: Option<HashSet<String>> = if self.live {
            None
        } else {
            Some(self.store.borrow().chats.iter().filter(|c| matches!(c.chat_type, vector_core::ChatType::DirectMessage)).map(|c| c.id.clone()).collect())
        };
        self.profile = None;
        self.switcher = None;
        self.new_chat = Some(crate::newchat::NewChat { filter, picked: None, strangers: Vec::new(), contacts: dm });
        if self.live {
            let rx = backend::run(async { vector_core::db::events::get_dm_contact_npubs().unwrap_or_default() });
            cx.spawn(async move |this, cx| {
                let Ok(list) = rx.await else { return };
                this.update(cx, |this, cx| {
                    if let Some(n) = this.new_chat.as_mut() {
                        n.contacts = Some(list.into_iter().collect());
                        cx.notify();
                    }
                })
                .ok();
            })
            .detach();
        }
        cx.notify();
    }

    /// A pasted npub is picked: a known profile directly, anyone else as a stranger whose
    /// profile is fetched.
    fn pasted_npub(&mut self, text: &str, cx: &mut Context<Self>) {
        let Some(npub) = crate::newchat::extract_npub(text) else { return };
        let Some(n) = self.new_chat.as_mut() else { return };
        let store = self.store.borrow();
        if npub == store.my_npub {
            return;
        }
        let known = store.profiles.contains_key(&npub);
        drop(store);
        n.picked = Some(npub.clone());
        if known || n.strangers.contains(&npub) {
            return;
        }
        n.strangers.push(npub.clone());
        if self.live {
            let _ = backend::run(async move { vector_core::profile::sync::load_profile(npub, &sync::ProfileHandler).await });
        }
        cx.notify();
    }

    fn on_new_chat(&mut self, act: crate::newchat::Act, window: &mut Window, cx: &mut Context<Self>) {
        use crate::newchat::Act;
        let Some(n) = self.new_chat.as_mut() else { return };
        match act {
            Act::Toggle(npub) => {
                n.picked = if n.picked.as_ref() == Some(&npub) { None } else { Some(npub) };
                cx.notify();
            }
            Act::Start => {
                let Some(npub) = n.picked.clone() else { return };
                self.new_chat = None;
                self.store.borrow_mut().ensure_dm(&npub);
                self.open_chat(npub.into(), window, cx);
            }
            Act::Cancel => {
                self.new_chat = None;
                self.open = None;
                cx.notify();
            }
            Act::Hub => cx.open_url("https://vectorapp.io/hub"),
        }
    }

    /// Fixtures: ourselves and the first two contacts by name, as the reference mock lists them.
    fn fixture_accounts(&self) -> Vec<crate::switcher::Row> {
        let store = self.store.borrow();
        let active = &store.my_npub;
        let mut others: Vec<&vector_core::SlimProfile> = store.profiles.values().filter(|p| &p.id != active).collect();
        others.sort_by_key(|p| p.name.to_lowercase());
        std::iter::once(store.profiles.get(active))
            .flatten()
            .chain(others.into_iter().take(2))
            .map(|p| crate::switcher::Row {
                npub: p.id.to_string(),
                name: [&p.nickname, &p.display_name, &p.name].into_iter().find(|n| !n.is_empty()).map(|n| n.to_string()),
                avatar: store.avatar_path(&p.id),
            })
            .collect()
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

    fn confirm_block_request(&mut self, npub: String, cx: &mut Context<Self>) {
        let mut popup = kit::Popup::notice("Block User");
        popup.text = Some("Are you sure you want to block this user? You will no longer receive DMs from them.".into());
        popup.confirm = "Confirm".into();
        popup.cancel = Some("Cancel".into());
        self.popup = Some((popup, PopupAct::BlockRequest { npub }));
        cx.notify();
    }

    /// Accept lets what they sent load as the settings allow, and moves the chat to Messages.
    fn accept_request(&mut self, npub: String, cx: &mut Context<Self>) {
        let lift = |this: &mut Self, npub: &str, cx: &mut Context<Self>| {
            if let Some(chat) = this.store.borrow_mut().chat_mut(npub) {
                chat.metadata.insert(vector_core::chat::ACCEPTED_KEY, "true".into());
            }
            this.media.borrow_mut().meta_asked.clear();
            cx.notify();
        };
        if !self.live {
            lift(self, &npub, cx);
            return;
        }
        let rx = backend::run(backend::prefs::accept(npub.clone()));
        cx.spawn(async move |this, cx| {
            let ok = matches!(rx.await, Ok(Ok(())));
            this.update(cx, |this, cx| if ok { lift(this, &npub, cx) } else { this.show_toast("Failed to accept", cx) }).ok();
        })
        .detach();
    }

    /// Re-ask which request senders share a Community with us when the requests change.
    fn sync_request_mutuals(&mut self, cx: &mut Context<Self>) {
        if !self.live {
            return;
        }
        let asking: Vec<String> = {
            let store = self.store.borrow();
            store.chats.iter().filter(|c| store.is_request(c)).map(|c| c.id.clone()).collect()
        };
        if asking == self.mutuals_asked {
            return;
        }
        self.mutuals_asked = asking.clone();
        if asking.is_empty() {
            self.store.borrow_mut().mutuals.clear();
            return;
        }
        let rx = backend::run(backend::prefs::request_mutuals());
        cx.spawn(async move |this, cx| {
            let Ok(next) = rx.await else { return };
            this.update(cx, |this, cx| {
                if this.mutuals_asked == asking {
                    this.store.borrow_mut().mutuals = next.into_iter().collect();
                    cx.notify();
                }
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
    /// The account another device sent has arrived: the login screen signs in with it.
    fn finish_transfer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let security = self.security.clone();
        let Screen::Login(view) = &self.screen else {
            security.update(cx, |f, cx| f.transfer_finished(Err("Sign out of this account first".into()), cx));
            return;
        };
        let rx = view.update(cx, |v, cx| v.finish_transfer(window, cx));
        cx.spawn(async move |_, cx| {
            let result = rx.await.unwrap_or_else(|_| Err("Backend stopped".into()));
            security.update(cx, |f, cx| f.transfer_finished(result, cx));
        })
        .detach();
    }

    fn on_security(&mut self, e: &crate::security::Event, window: &mut Window, cx: &mut Context<Self>) {
        match e {
            crate::security::Event::State { enabled, kind } => {
                if let Some(s) = self.settings.as_mut() {
                    s.on.insert("encryption", *enabled);
                    s.sec_kind = kind.clone();
                }
                cx.notify();
            }
            crate::security::Event::Toast(text) => self.show_toast(text, cx),
            crate::security::Event::Received => self.finish_transfer(window, cx),
        }
    }

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

/// Fixtures: the reference's relay and server rows.
fn fixture_network() -> (Vec<backend::network::RelayRow>, Vec<vector_core::blossom_servers::BlossomServerInfo>) {
    use backend::network::RelayRow;
    use vector_core::blossom_stats::ServerStatus;
    let relay = |url: &str, status: &'static str, is_default: bool, enabled: bool, mode: &str| RelayRow { url: url.into(), status, is_default, enabled, mode: mode.into() };
    let relays = vec![
        relay("wss://jskitty.com/nostr", "connected", true, true, "both"),
        relay("wss://asia.vectorapp.io/nostr", "connected", true, true, "both"),
        relay("wss://nostr.computingcache.com", "connecting", true, true, "both"),
        relay("wss://relay.ditto.pub", "disabled", true, false, "both"),
        relay("wss://relay.example.org", "disconnected", false, true, "read"),
    ];
    let server = |url: &str, enabled: bool, state: &'static str, label: &'static str, tone: &'static str, latency_ms: Option<u32>| vector_core::blossom_servers::BlossomServerInfo {
        url: url.into(),
        is_default: true,
        is_custom: false,
        enabled,
        status: ServerStatus { state, label, tone, latency_ms },
    };
    let servers = vec![
        server("https://magnitude.vectorapp.io", true, "online", "Online", "connected", Some(84)),
        server("https://blossom.ditto.pub", true, "untested", "Untested", "disabled", None),
        server("https://blossom.primal.net", false, "disabled", "Disabled", "disabled", None),
    ];
    (relays, servers)
}
