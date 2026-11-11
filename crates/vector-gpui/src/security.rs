//! Settings › Security's flows: the credential prompt (`CredentialModal`), the keys card
//! (`KeysModal`) and the migration overlay (`MigrationOverlay`), each a step of an async flow.

use std::time::Duration;

use futures::StreamExt;
use futures::channel::oneshot;
use gpui::prelude::*;
use gpui::{AnyElement, AsyncApp, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight, KeyDownEvent, SharedString, WeakEntity, Window, div, px, svg};
use zeroize::Zeroizing;

use crate::backend::{self, security::MigrationProgress};
use crate::composer::{CancelInput, Look, SendText, TextInput};
use crate::kit;
use crate::theme::*;

const INCORRECT: &str = "CREDENTIAL_INCORRECT";
const REQUIRED: &str = "CREDENTIAL_REQUIRED";
const LOCKED: &str = "CREDENTIAL_LOCKED:";

pub enum Event {
    /// Local Encryption as it now stands, for the toggle to follow.
    State { enabled: bool, kind: String },
    Toast(String),
    /// Open Sign in on Another Device.
    Transfer,
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Choose,
    Pin,
    Password,
    /// The backend is checking: no controls.
    Checking,
}

struct Prompt {
    mode: Mode,
    title: SharedString,
    lead: SharedString,
    error: bool,
    confirm: SharedString,
    picked: &'static str,
    pin: Zeroizing<String>,
    reply: Option<oneshot::Sender<Option<Zeroizing<String>>>>,
    focus: bool,
}

struct Keys {
    /// Answers the warning: true to go on to the keys.
    proceed: Option<oneshot::Sender<bool>>,
    secrets: Option<(Zeroizing<String>, Vec<Zeroizing<String>>)>,
    shown_seed: bool,
    shown_nsec: bool,
    copied: Option<(&'static str, u32)>,
    copies: u32,
}

struct Migration {
    title: SharedString,
    phase: SharedString,
    pct: u32,
}

pub struct SecurityFlow {
    live: bool,
    enabled: bool,
    kind: String,
    /// A flow is running: a second toggle or Change waits for it.
    busy: bool,
    prompt: Option<Prompt>,
    keys: Option<Keys>,
    migration: Option<Migration>,
    confirm: Option<(kit::Popup, oneshot::Sender<bool>)>,
    pin_focus: FocusHandle,
    card_focus: FocusHandle,
    password: Entity<TextInput>,
}

impl EventEmitter<Event> for SecurityFlow {}

/// A credential as it reads mid-sentence: "PIN" keeps its capitals, "password" doesn't.
fn noun(kind: &str) -> &'static str {
    if kind == "password" { "password" } else { "PIN" }
}

fn label(kind: &str) -> &'static str {
    if kind == "password" { "Password" } else { "PIN" }
}

fn mode_of(kind: &str) -> Mode {
    if kind == "password" { Mode::Password } else { Mode::Pin }
}

impl SecurityFlow {
    pub fn new(live: bool, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let look = Look { masked: true, masked_size: Some(px(16.)), ..kit::field_look() };
        let password = cx.new(|cx| TextInput::new("Password", cx).with_look(look).keep_on_submit());
        cx.subscribe_in(&password, window, |f, _, _: &SendText, window, cx| f.submit_password(window, cx)).detach();
        cx.subscribe(&password, |f, _, _: &CancelInput, cx| f.answer(None, cx)).detach();
        let (enabled, kind) = if live { backend::security::status() } else { (true, "pin".into()) };
        Self {
            live,
            enabled,
            kind,
            busy: false,
            prompt: None,
            keys: None,
            migration: None,
            confirm: None,
            pin_focus: cx.focus_handle(),
            card_focus: cx.focus_handle(),
            password,
        }
    }

    /// Re-read the account's state, unless a flow is changing it. Returns whether it is on.
    pub fn sync(&mut self) -> bool {
        if self.live && !self.busy {
            (self.enabled, self.kind) = backend::security::status();
        }
        self.enabled
    }

    pub fn kind(&self) -> &str {
        &self.kind
    }

    fn state(&self, cx: &mut Context<Self>) {
        cx.emit(Event::State { enabled: self.enabled, kind: self.kind.clone() });
        cx.notify();
    }

    // ── Entry points ───────────────────────────────────────────────────────────

    pub fn toggle(&mut self, on: bool, cx: &mut Context<Self>) {
        if self.busy || on == self.enabled {
            return self.state(cx);
        }
        self.busy = true;
        cx.spawn(async move |this, cx| {
            if on { enable(&this, cx).await } else { disable(&this, cx).await }
            this.update(cx, |f, cx| {
                f.busy = false;
                f.state(cx);
            })
            .ok();
        })
        .detach();
    }

    pub fn change(&mut self, cx: &mut Context<Self>) {
        if self.busy || !self.enabled {
            return;
        }
        self.busy = true;
        cx.spawn(async move |this, cx| {
            change(&this, cx).await;
            this.update(cx, |f, cx| {
                f.busy = false;
                f.state(cx);
            })
            .ok();
        })
        .detach();
    }

    pub fn show_keys(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.busy = true;
        cx.spawn(async move |this, cx| {
            show_keys(&this, cx).await;
            this.update(cx, |f, _| f.busy = false).ok();
        })
        .detach();
    }

    // ── The prompt ─────────────────────────────────────────────────────────────

    fn open_prompt(&mut self, mode: Mode, title: SharedString, lead: SharedString, error: bool, confirm: &str, cx: &mut Context<Self>) -> oneshot::Receiver<Option<Zeroizing<String>>> {
        let (tx, rx) = oneshot::channel();
        self.prompt = Some(Prompt {
            mode,
            title,
            lead,
            error,
            confirm: SharedString::from(confirm.to_string()),
            picked: "pin",
            pin: Zeroizing::new(String::new()),
            reply: Some(tx),
            focus: true,
        });
        cx.notify();
        rx
    }

    fn checking(&mut self, title: SharedString, lead: String, cx: &mut Context<Self>) {
        self.prompt = Some(Prompt {
            mode: Mode::Checking,
            title,
            lead: lead.into(),
            error: false,
            confirm: SharedString::default(),
            picked: "pin",
            pin: Zeroizing::new(String::new()),
            reply: None,
            focus: true,
        });
        cx.notify();
    }

    fn close_prompt(&mut self, cx: &mut Context<Self>) {
        self.prompt = None;
        cx.notify();
    }

    /// Settle the prompt: a value, or None for a cancel.
    fn answer(&mut self, value: Option<Zeroizing<String>>, cx: &mut Context<Self>) {
        let Some(p) = self.prompt.as_mut() else { return };
        if p.mode == Mode::Checking {
            return;
        }
        if let Some(tx) = p.reply.take() {
            let _ = tx.send(value);
        }
        self.prompt = None;
        cx.notify();
    }

    fn submit_password(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.prompt.as_ref().is_some_and(|p| p.mode == Mode::Password) {
            return;
        }
        let value = Zeroizing::new(self.password.read(cx).text().to_string());
        if value.is_empty() {
            return;
        }
        self.password.update(cx, |p, cx| p.clear(window, cx));
        self.answer(Some(value), cx);
    }

    fn pin_key(&mut self, e: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(p) = self.prompt.as_mut().filter(|p| p.mode == Mode::Pin) else { return };
        match e.keystroke.key.as_str() {
            "backspace" => {
                p.pin.pop();
            }
            "escape" => return self.answer(None, cx),
            _ => {
                if let Some(d) = e.keystroke.key_char.as_deref().filter(|k| k.len() == 1 && k.as_bytes()[0].is_ascii_digit()) {
                    if p.pin.len() < 6 {
                        p.pin.push_str(d);
                    }
                    if p.pin.len() == 6 {
                        let pin = Zeroizing::new(std::mem::take(&mut *p.pin));
                        return self.answer(Some(pin), cx);
                    }
                }
            }
        }
        cx.notify();
    }

    // ── The keys card ──────────────────────────────────────────────────────────

    fn close_keys(&mut self, cx: &mut Context<Self>) {
        if let Some(k) = self.keys.take()
            && let Some(tx) = k.proceed
        {
            let _ = tx.send(false);
        }
        cx.notify();
    }

    fn copy_secret(&mut self, which: &'static str, cx: &mut Context<Self>) {
        let Some(k) = self.keys.as_mut() else { return };
        let Some((nsec, words)) = k.secrets.as_ref() else { return };
        let text = if which == "seed" { Zeroizing::new(words.iter().map(|w| w.as_str()).collect::<Vec<_>>().join(" ")) } else { nsec.clone() };
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(text.to_string()));
        k.copies += 1;
        let tick = k.copies;
        k.copied = Some((which, tick));
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(1600)).await;
            this.update(cx, |f, cx| {
                if let Some(k) = f.keys.as_mut()
                    && k.copied.is_some_and(|(_, t)| t == tick)
                {
                    k.copied = None;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn transfer(&mut self, cx: &mut Context<Self>) {
        self.close_keys(cx);
        cx.emit(Event::Transfer);
    }

    fn progress(&mut self, p: MigrationProgress, cx: &mut Context<Self>) {
        let Some(m) = self.migration.as_mut() else { return };
        m.pct = if p.total > 0 { ((p.completed as f64 / p.total as f64) * 100.).round() as u32 } else { 0 };
        let (done, total) = (thousands(p.completed), thousands(p.total));
        m.phase = match p.phase.as_str() {
            "decrypting" => format!("Decrypting messages... {done} / {total}"),
            "encrypting" => format!("Encrypting messages... {done} / {total}"),
            "rekeying" => format!("Re-encrypting messages... {done} / {total}"),
            "finalizing" => "Finalizing...".into(),
            other => other.to_string(),
        }
        .into();
        cx.notify();
    }
}

fn thousands(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

// ── Flows ─────────────────────────────────────────────────────────────────────

type This = WeakEntity<SecurityFlow>;

async fn ask(this: &This, cx: &mut AsyncApp, mode: Mode, title: impl Into<SharedString>, lead: impl Into<SharedString>, error: bool, confirm: &str) -> Option<Zeroizing<String>> {
    let (title, lead) = (title.into(), lead.into());
    let rx = this.update(cx, |f, cx| f.open_prompt(mode, title, lead, error, confirm, cx)).ok()?;
    rx.await.ok().flatten()
}

async fn confirm(this: &This, cx: &mut AsyncApp, popup: kit::Popup) -> bool {
    let (tx, rx) = oneshot::channel();
    if this.update(cx, |f, cx| {
        f.confirm = Some((popup, tx));
        cx.notify();
    })
    .is_err()
    {
        return false;
    }
    rx.await.unwrap_or(false)
}

async fn notice(this: &This, cx: &mut AsyncApp, title: &str, text: String) {
    let mut popup = kit::Popup::notice(title.to_string());
    popup.text = Some(text.into());
    confirm(this, cx, popup).await;
}

fn kind(this: &This, cx: &mut AsyncApp) -> String {
    this.read_with(cx, |f, _| f.kind.clone()).unwrap_or_else(|_| "pin".into())
}

/// Ask for the current PIN or password until `attempt` accepts it, holding the card while it
/// checks. `Ok(None)` on a cancel.
async fn with_current<T: Send + 'static>(
    this: &This,
    cx: &mut AsyncApp,
    title: &str,
    lead: String,
    attempt: impl Fn(Zeroizing<String>) -> oneshot::Receiver<Result<T, String>>,
) -> Result<Option<T>, String> {
    let kind = kind(this, cx);
    let mut prompt = lead;
    let mut mistake = false;
    loop {
        let Some(entered) = ask(this, cx, mode_of(&kind), title.to_string(), prompt.clone(), mistake, "Confirm").await else { return Ok(None) };
        mistake = true;
        let title = SharedString::from(title.to_string());
        this.update(cx, |f, cx| f.checking(title, format!("Checking your {}…", noun(&kind)), cx)).ok();
        let result = attempt(entered).await.unwrap_or_else(|_| Err("cancelled".into()));
        this.update(cx, |f, cx| f.close_prompt(cx)).ok();
        match result {
            Ok(v) => return Ok(Some(v)),
            Err(e) => {
                if let Some(secs) = e.strip_prefix(LOCKED).and_then(|s| s.parse::<u64>().ok()) {
                    let wait = if secs >= 60 { format!("{} min", secs.div_ceil(60)) } else { format!("{secs} s") };
                    prompt = format!("Too many wrong tries. Try again in {wait}.");
                } else if e == INCORRECT {
                    prompt = format!("Incorrect {}, try again.", noun(&kind));
                } else {
                    return Err(e);
                }
            }
        }
    }
}

/// The current credential once the backend accepts it.
async fn current(this: &This, cx: &mut AsyncApp, title: &str, lead: String) -> Option<Zeroizing<String>> {
    let live = this.read_with(cx, |f, _| f.live).unwrap_or(false);
    let result = with_current(this, cx, title, lead, |cred| {
        if !live {
            // Fixtures: any entry passes, so each step of a flow can be shown.
            let (tx, rx) = oneshot::channel();
            let _ = tx.send(Ok(cred));
            return rx;
        }
        backend::run(async move {
            backend::security::verify(cred.clone()).await?;
            Ok(cred)
        })
    })
    .await;
    match result {
        Ok(c) => c,
        Err(e) => {
            notice(this, cx, "Could not verify", e).await;
            None
        }
    }
}

/// Pick PIN or password, then enter and confirm it.
async fn new_credential(this: &This, cx: &mut AsyncApp, title: &str, message: &str) -> Option<(Zeroizing<String>, String)> {
    let kind = ask(this, cx, Mode::Choose, title.to_string(), message.to_string(), false, "Continue").await?.to_string();
    let label = label(&kind);
    let mut lead = if kind == "pin" { "Enter a 6-digit PIN.".to_string() } else { "Enter a password (4+ characters).".to_string() };
    let mut error = false;
    loop {
        let first = ask(this, cx, mode_of(&kind), format!("Create {label}"), lead.clone(), error, "Confirm").await?;
        if kind == "password" && first.chars().count() < 4 {
            lead = "Too short. Use at least 4 characters.".into();
            error = true;
            continue;
        }
        let again = ask(this, cx, mode_of(&kind), format!("Confirm {label}"), format!("Re-enter your {}.", noun(&kind)), false, "Confirm").await?;
        if *again != *first {
            lead = format!("{label}s didn't match. Try again.");
            error = true;
            continue;
        }
        return Some((first, kind));
    }
}

/// Run a migration under the overlay, its progress painted as it reports.
async fn migrate(
    this: &This,
    cx: &mut AsyncApp,
    title: &'static str,
    run: impl FnOnce(Box<dyn Fn(MigrationProgress) + Send + Sync>) -> oneshot::Receiver<Result<(), String>>,
) -> Result<(), String> {
    let (tx, mut rx) = futures::channel::mpsc::unbounded();
    this.update(cx, |f, cx| {
        f.migration = Some(Migration { title: title.into(), phase: "Preparing...".into(), pct: 0 });
        cx.notify();
    })
    .ok();
    let watch = {
        let this = this.clone();
        cx.spawn(async move |cx| {
            while let Some(p) = rx.next().await {
                this.update(cx, |f, cx| f.progress(p, cx)).ok();
            }
        })
    };
    let live = this.read_with(cx, |f, _| f.live).unwrap_or(false);
    let result = if live {
        run(Box::new(move |p| {
            let _ = tx.unbounded_send(p);
        }))
        .await
        .unwrap_or_else(|_| Err("cancelled".into()))
    } else {
        // Fixtures: a sweep part done, held long enough to be seen.
        let _ = tx.unbounded_send(MigrationProgress { total: 5678, completed: 1234, phase: "encrypting".into() });
        cx.background_executor().timer(Duration::from_secs(4)).await;
        Ok(())
    };
    drop(watch);
    this.update(cx, |f, cx| {
        f.migration = None;
        cx.notify();
    })
    .ok();
    result
}

async fn enable(this: &This, cx: &mut AsyncApp) {
    let Some((cred, kind)) = new_credential(this, cx, "Set Up Encryption", "Choose how to protect your local data. There is no recovery if you forget!").await else { return };
    let k = kind.clone();
    let result = migrate(this, cx, "Enabling Encryption", move |report| backend::run(backend::security::enable(cred, k, report))).await;
    match result {
        Ok(()) => {
            this.update(cx, |f, cx| {
                f.enabled = true;
                f.kind = kind;
                cx.emit(Event::Toast("Encryption enabled".into()));
            })
            .ok();
        }
        Err(e) => notice(this, cx, "Encryption Failed", format!("Failed to enable encryption: {e}")).await,
    }
}

async fn disable(this: &This, cx: &mut AsyncApp) {
    let mut popup = kit::Popup::notice("Disable Encryption?");
    popup.text = Some("This will decrypt all your local data and remove PIN protection.<br><br><b>Your messages and keys will be stored in plain text.</b><br><br>This is useful for faster app startup on personal devices, but reduces security if your device is lost or stolen.".into());
    popup.confirm = "Confirm".into();
    popup.cancel = Some("Cancel".into());
    if !confirm(this, cx, popup).await {
        return;
    }
    let kind = kind(this, cx);
    let Some(cred) = current(this, cx, &format!("Enter {}", label(&kind)), format!("Enter your {} to turn off Local Encryption.", noun(&kind))).await else { return };
    let result = migrate(this, cx, "Disabling Encryption", move |report| backend::run(backend::security::disable(Some(cred), report))).await;
    match result {
        Ok(()) => {
            this.update(cx, |f, cx| {
                f.enabled = false;
                f.kind = "pin".into();
                cx.emit(Event::Toast("Encryption disabled".into()));
            })
            .ok();
        }
        Err(e) => notice(this, cx, "Decryption Failed", format!("Failed to disable encryption: {e}")).await,
    }
}

async fn change(this: &This, cx: &mut AsyncApp) {
    let kind = kind(this, cx);
    let label = label(&kind);
    let Some(old) = current(this, cx, &format!("Enter Current {label}"), format!("Please enter your current {} to continue.", noun(&kind))).await else { return };
    let Some((new, new_kind)) = new_credential(this, cx, &format!("Change {label}"), "Choose a new security type and credential.").await else { return };
    let k = new_kind.clone();
    let result = migrate(this, cx, "Changing Credential", move |report| backend::run(backend::security::rekey(old, new, k, report))).await;
    match result {
        Ok(()) => {
            this.update(cx, |f, cx| {
                f.kind = new_kind;
                cx.emit(Event::Toast("Unlock method updated".into()));
            })
            .ok();
        }
        Err(e) => notice(this, cx, "Re-keying Failed", format!("Failed to change credential: {e}")).await,
    }
}

async fn show_keys(this: &This, cx: &mut AsyncApp) {
    let (tx, rx) = oneshot::channel();
    let opened = this.update(cx, |f, cx| {
        f.keys = Some(Keys { proceed: Some(tx), secrets: None, shown_seed: false, shown_nsec: false, copied: None, copies: 0 });
        cx.notify();
    });
    if opened.is_err() || !rx.await.unwrap_or(false) {
        return;
    }
    type Secrets = (Zeroizing<String>, Option<Zeroizing<String>>);
    let live = this.read_with(cx, |f, _| f.live).unwrap_or(false);
    let first: Result<Secrets, String> = if live {
        backend::run(backend::security::export(None)).await.unwrap_or_else(|_| Err("cancelled".into()))
    } else {
        Ok(fixture_keys())
    };
    let result = match first {
        Err(e) if e == REQUIRED => {
            let kind = kind(this, cx);
            with_current(this, cx, "Show Private Keys", format!("Enter your {} to show your keys.", noun(&kind)), |cred| backend::run(backend::security::export(Some(cred)))).await
        }
        other => other.map(Some),
    };
    match result {
        Ok(Some((nsec, seed))) => {
            let words = seed.map(|s| s.split_whitespace().map(|w| Zeroizing::new(w.to_string())).collect()).unwrap_or_default();
            this.update(cx, |f, cx| {
                if let Some(k) = f.keys.as_mut() {
                    k.secrets = Some((nsec, words));
                }
                cx.notify();
            })
            .ok();
        }
        Ok(None) => {
            this.update(cx, |f, cx| f.close_keys(cx)).ok();
        }
        Err(e) => {
            this.update(cx, |f, cx| f.close_keys(cx)).ok();
            notice(this, cx, "Export Failed", e).await;
        }
    }
}

/// The reference's sample keys (BIP-39's first words, a throwaway nsec).
fn fixture_keys() -> (Zeroizing<String>, Option<Zeroizing<String>>) {
    (
        Zeroizing::new("nsec1vl029mgpspedva04g90vltkh6fvh240zqtv9k0t9af8935ke9laqsnlfe5".into()),
        Some(Zeroizing::new("abandon ability able about above absent absorb abstract absurd abuse access accident".into())),
    )
}

// ── Rendering ─────────────────────────────────────────────────────────────────

/// `.vcard-overlay` holding a `.vcard`.
fn overlay(id: &'static str, priority: usize, card: gpui::Stateful<gpui::Div>) -> AnyElement {
    gpui::deferred(
        div()
            .id(id)
            .absolute()
            .inset_0()
            .p(px(16.))
            .flex()
            .items_center()
            .justify_center()
            .bg(ca(0, 0, 0, 0.88))
            .occlude()
            .child(card),
    )
    .with_priority(priority)
    .into_any_element()
}

fn card(id: &'static str) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .w(px(440.))
        .max_w_full()
        .flex()
        .flex_col()
        .gap(px(16.))
        .pt(px(20.))
        .px(px(20.))
        .pb(px(18.))
        .bg(c(0x101010))
        .border_1()
        .border_color(c(0x2c2e2f))
        .rounded(px(18.))
        .shadow(vec![gpui::BoxShadow { color: ca(0, 0, 0, 0.55), offset: gpui::point(px(0.), px(18.)), blur_radius: px(60.), spread_radius: px(0.), inset: false }])
        .font_family(INTER)
        .text_color(c(0xffffff))
        .text_size(px(16.))
        .line_height(px(24.))
}

fn head(glyph: &'static str, title: SharedString, close: Option<Box<dyn Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App)>>) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .gap(px(12.))
        .child(
            div()
                .size(px(34.))
                .flex_shrink_0()
                .rounded(px(10.))
                .bg(ca(89, 252, 179, 0.1))
                .flex()
                .items_center()
                .justify_center()
                .child(svg().path(glyph).size(px(18.)).text_color(c(ACCENT))),
        )
        .child(div().flex_1().text_size(px(18.)).line_height(px(22.)).font_weight(FontWeight::BOLD).child(title))
        .children(close.map(|close| {
            div()
                .id("vcard-x")
                .group("vcard-x")
                .self_start()
                .w(px(23.7))
                .h(px(22.))
                .flex()
                .justify_center()
                .pt(px(6.))
                .cursor_pointer()
                .on_click(close)
                .child(svg().path("icons/x-mark.svg").size(px(9.)).text_color(white_a(0.4)).group_hover("vcard-x", |d| d.text_color(c(0xffffff))))
        }))
}

fn lead(text: impl IntoElement, error: bool) -> gpui::Div {
    div().text_size(px(13.5)).line_height(px(20.25)).text_color(if error { c(0xff7a8a) } else { white_a(0.62) }).child(text)
}

/// A lead with some runs in white: `(text, white)` pairs.
fn lead_mixed(parts: &[(&str, bool)]) -> gpui::Div {
    let mut text = String::new();
    let mut runs = Vec::new();
    for (part, white) in parts {
        let start = text.len();
        text.push_str(part);
        if *white {
            runs.push((start..text.len(), gpui::HighlightStyle { color: Some(c(0xffffff)), ..Default::default() }));
        }
    }
    lead(gpui::StyledText::new(text).with_highlights(runs), false)
}

fn button(id: &'static str, text: impl Into<SharedString>, ghost: bool, enabled: bool, click: impl Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static) -> impl IntoElement {
    div()
        .id(id)
        .w_full()
        .h(px(44.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(12.))
        .bg(if ghost { c(0x1b1c1d) } else { c(ACCENT) })
        .text_color(if ghost { c(0xffffff) } else { c(0x000000) })
        .text_size(px(16.))
        .font_weight(FontWeight::SEMIBOLD)
        .when(ghost && enabled, |d| d.hover(|d| d.bg(c(0x232425))))
        .when(!enabled, |d| d.opacity(0.5))
        .when(enabled, |d| d.cursor_pointer().on_click(click))
        .child(text.into())
}

fn actions() -> gpui::Div {
    div().flex().flex_col().gap(px(8.)).mt(px(4.))
}

impl SecurityFlow {
    fn prompt_card(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let p = self.prompt.as_mut()?;
        let me = cx.entity().downgrade();
        let cancel = {
            let me = me.clone();
            move |_: &gpui::ClickEvent, _: &mut Window, cx: &mut gpui::App| {
                me.update(cx, |f, cx| f.answer(None, cx)).ok();
            }
        };
        if std::mem::take(&mut p.focus) {
            match p.mode {
                Mode::Pin => window.focus(&self.pin_focus, cx),
                Mode::Password => {
                    self.password.update(cx, |i, cx| i.clear(window, cx));
                    window.focus(&self.password.focus_handle(cx), cx);
                }
                _ => window.focus(&self.card_focus, cx),
            }
        }
        let p = self.prompt.as_ref()?;
        let mut card = card("cred-card")
            .track_focus(&self.card_focus)
            .on_key_down(cx.listener(|f, e: &KeyDownEvent, _, cx| {
                if e.keystroke.key == "escape" {
                    f.answer(None, cx);
                }
            }))
            .child(head("icons/lock.svg", p.title.clone(), (p.mode != Mode::Checking).then(|| Box::new(cancel.clone()) as Box<_>)));
        if p.mode == Mode::Checking {
            card = card.child(
                div()
                    .pt(px(28.))
                    .pb(px(22.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap(px(10.))
                    .text_size(px(14.))
                    .text_color(white_a(0.75))
                    .child(div().size(px(18.)).child(crate::attachments::spinner(18., 2., 0.8, 0.25, white_a(0.15), c(ACCENT))))
                    .child(p.lead.clone()),
            );
            window.request_animation_frame();
            return Some(overlay("cred-overlay", 14, card));
        }
        if !p.lead.is_empty() {
            card = card.child(lead(p.lead.clone(), p.error));
        }
        match p.mode {
            Mode::Choose => {
                let tile = |id: &'static str, title: &'static str, desc: &'static str| {
                    let on = p.picked == id;
                    let me = me.clone();
                    div()
                        .id(id)
                        .flex_1()
                        .flex_basis(px(0.))
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(4.))
                        .px(px(14.))
                        .py(px(12.))
                        .rounded(px(12.))
                        .border_1()
                        .border_color(if on { c(ACCENT) } else { c(0x2c2e2f) })
                        .bg(if on { ca(89, 252, 179, 0.07) } else { c(0x161718).into() })
                        .when(!on, |d| d.hover(|d| d.border_color(c(0x3a3d3e))))
                        .cursor_pointer()
                        .on_click(move |_, _, cx| {
                            me.update(cx, |f, cx| {
                                if let Some(p) = f.prompt.as_mut() {
                                    p.picked = id;
                                }
                                cx.notify();
                            })
                            .ok();
                        })
                        .child(div().text_size(px(15.)).line_height(px(19.)).font_weight(FontWeight::BOLD).child(title))
                        .child(div().text_size(px(12.)).line_height(px(16.8)).text_color(white_a(0.55)).child(desc))
                };
                card = card.child(
                    div()
                        .flex()
                        .gap(px(10.))
                        .child(tile("pin", "PIN", "A 6-digit code. Quick and convenient."))
                        .child(tile("password", "Password", "A text password. More secure, but slower to enter.")),
                );
            }
            Mode::Pin => {
                let focused = self.pin_focus.is_focused(window);
                let mut row = div().id("cred-pins").track_focus(&self.pin_focus).on_key_down(cx.listener(Self::pin_key)).flex().justify_center().gap(px(8.));
                for i in 0..6 {
                    let filled = i < p.pin.len();
                    let active = focused && i == p.pin.len().min(5);
                    row = row.child(
                        div()
                            .w(px(44.))
                            .h(px(54.))
                            .rounded(px(10.))
                            .border_1()
                            .border_color(if active { c(ACCENT) } else { c(0x2c2e2f) })
                            .bg(c(0x161718))
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_size(px(24.))
                            .font_weight(FontWeight::BOLD)
                            .when(filled, |d| d.child("\u{2022}")),
                    );
                }
                card = card.child(row);
            }
            Mode::Password => {
                let focused = self.password.focus_handle(cx).is_focused(window);
                card = card.child(
                    div()
                        .px(px(14.))
                        .py(px(12.))
                        .rounded(px(12.))
                        .border_1()
                        .border_color(if focused { c(ACCENT) } else { c(0x2c2e2f) })
                        .bg(c(0x161718))
                        .child(self.password.clone()),
                );
            }
            Mode::Checking => {}
        }
        let mut row = actions();
        match p.mode {
            Mode::Choose => {
                let me = me.clone();
                row = row.child(button("cred-go", p.confirm.clone(), false, true, move |_, _, cx| {
                    me.update(cx, |f, cx| {
                        let picked = f.prompt.as_ref().map(|p| p.picked).unwrap_or("pin");
                        f.answer(Some(Zeroizing::new(picked.to_string())), cx);
                    })
                    .ok();
                }));
            }
            Mode::Password => {
                let me = me.clone();
                let typed = !self.password.read(cx).text().is_empty();
                row = row.child(button("cred-go", p.confirm.clone(), false, typed, move |_, window, cx| {
                    me.update(cx, |f, cx| f.submit_password(window, cx)).ok();
                }));
            }
            _ => {}
        }
        row = row.child(button("cred-cancel", "Cancel", true, true, cancel));
        Some(overlay("cred-overlay", 14, card.child(row)))
    }

    fn keys_card(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let k = self.keys.as_ref()?;
        let me = cx.entity().downgrade();
        let on = move |f: fn(&mut SecurityFlow, &mut Context<SecurityFlow>)| {
            let me = me.clone();
            move |_: &gpui::ClickEvent, _: &mut Window, cx: &mut gpui::App| {
                me.update(cx, |s, cx| f(s, cx)).ok();
            }
        };
        let title = if k.secrets.is_some() { "Your Keys" } else { "Show Private Keys?" };
        let mut card = card("keys-card").child(head("icons/key.svg", title.into(), Some(Box::new(on(Self::close_keys)))));
        let Some((nsec, words)) = k.secrets.as_ref() else {
            card = card
                .child(lead_mixed(&[
                    ("To use Vector on another device, use ", false),
                    ("Sign in on Another Device", true),
                    (" instead. It's quicker, and your keys never leave this device in a form anyone else can read.", false),
                ]))
                .child(lead("Only show your keys to back them up offline or to use them in another Nostr app. Anyone who sees them can read your messages and act as you.", false))
                .child(
                    actions()
                        .child(button("keys-transfer", "Sign in on Another Device", false, true, on(Self::transfer)))
                        .child(button("keys-proceed", "Show Keys Anyway", true, true, on(|f, cx| {
                            if let Some(tx) = f.keys.as_mut().and_then(|k| k.proceed.take()) {
                                let _ = tx.send(true);
                            }
                            cx.notify();
                        }))),
                );
            return Some(overlay("keys-overlay", 12, card));
        };
        card = card.child(lead_mixed(&[
            ("Your keys are your account. Anyone who has them can read your messages and speak as you. ", false),
            ("Write them down offline and never share them.", true),
        ]));
        let section_head = |which: &'static str, title: &'static str| {
            let done = k.copied.is_some_and(|(w, _)| w == which);
            let me = cx.entity().downgrade();
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(div().text_size(px(11.)).font_weight(FontWeight::BOLD).text_color(white_a(0.45)).child(kit::spaced(title.to_uppercase(), 1.54)))
                .child(
                    div()
                        .id(which)
                        .h(px(25.))
                        .px(px(10.))
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .rounded_full()
                        .bg(c(0x1b1c1d))
                        .text_size(px(12.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(if done { c(ACCENT) } else { white_a(0.8) })
                        .hover(|d| d.bg(c(0x232425)).text_color(if done { c(ACCENT) } else { c(0xffffff) }))
                        .cursor_pointer()
                        .on_click(move |_, _, cx| {
                            me.update(cx, |f, cx| f.copy_secret(which, cx)).ok();
                        })
                        .child(svg().path(if done { "icons/check.svg" } else { "icons/copy.svg" }).size(px(13.)).text_color(if done { c(ACCENT) } else { white_a(0.8) }))
                        .child(if done { "Copied" } else { "Copy" }),
                )
        };
        let veil = |which: &'static str| {
            let me = cx.entity().downgrade();
            div()
                .id(SharedString::from(format!("veil-{which}")))
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .gap(px(8.))
                .text_size(px(13.))
                .font_weight(FontWeight::SEMIBOLD)
                .cursor_pointer()
                .on_click(move |_, _, cx| {
                    me.update(cx, |f, cx| {
                        if let Some(k) = f.keys.as_mut() {
                            if which == "seed" { k.shown_seed = true } else { k.shown_nsec = true }
                        }
                        cx.notify();
                    })
                    .ok();
                })
                .child(svg().path("icons/eye.svg").size(px(16.)).text_color(c(0xffffff)))
                .child("Tap to reveal")
        };
        // A veiled secret is drawn as smudges, never as glyphs: blur is the browser's.
        let smudge = |w: f32, h: f32| div().w(px(w)).h(px(h)).rounded(px(h / 2.)).bg(white_a(0.1));
        let boxed = || div().relative().overflow_hidden().rounded(px(12.)).border_1().border_color(c(0x232526)).bg(c(0x161718));
        if !words.is_empty() {
            let mut grid = div().flex().flex_col().gap(px(16.)).px(px(14.)).py(px(16.));
            for (r, chunk) in words.chunks(3).enumerate() {
                let mut line = div().flex().gap(px(10.));
                for (i, w) in chunk.iter().enumerate() {
                    let n = r * 3 + i + 1;
                    let cell = div().flex_1().min_w_0().h(px(25.)).flex().items_center().gap(px(6.));
                    line = line.child(if k.shown_seed {
                        cell.child(div().w(px(16.)).flex_shrink_0().flex().justify_end().text_size(px(11.)).font_weight(FontWeight::MEDIUM).text_color(white_a(0.35)).child(n.to_string()))
                            .child(div().text_size(px(14.)).font_weight(FontWeight::MEDIUM).child(SharedString::from(w.to_string())))
                    } else {
                        cell.child(div().w(px(16.)).flex().justify_end().child(smudge(7. * n.to_string().len() as f32, 8.))).child(smudge(8. * w.chars().count() as f32, 11.))
                    });
                }
                grid = grid.child(line);
            }
            card = card.child(
                div().flex().flex_col().gap(px(8.)).child(section_head("seed", "Seed Phrase")).child(boxed().child(grid).when(!k.shown_seed, |d| d.child(veil("seed")))),
            );
        }
        let nsec_body = if k.shown_nsec {
            div().px(px(14.)).py(px(12.)).font_family(crate::markdown::MONO).text_size(px(13.)).line_height(px(20.15)).child(SharedString::from(nsec.to_string()))
        } else {
            div().px(px(14.)).py(px(16.)).flex().flex_col().gap(px(9.)).child(smudge(360., 11.)).child(smudge(120., 11.))
        };
        card = card.child(
            div().flex().flex_col().gap(px(8.)).child(section_head("nsec", "Private Key")).child(boxed().child(nsec_body).when(!k.shown_nsec, |d| d.child(veil("nsec")))),
        );
        card = card.child(
            actions().child(button("keys-done", "Done", false, true, on(Self::close_keys))).child(
                div()
                    .id("keys-link")
                    .self_center()
                    .px(px(4.))
                    .py(px(6.))
                    .text_size(px(13.))
                    .line_height(px(19.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(white_a(0.55))
                    .hover(|d| d.text_color(c(0xffffff)))
                    .cursor_pointer()
                    .on_click(on(Self::transfer))
                    .child("Moving to another device? Sign in there with a code instead"),
            ),
        );
        Some(overlay("keys-overlay", 12, card))
    }

    fn migration_card(&self, window: &mut Window) -> Option<AnyElement> {
        let m = self.migration.as_ref()?;
        window.request_animation_frame();
        let glow = (crate::attachments::EPOCH.elapsed().as_secs_f32() / 2.).fract();
        let track = div()
            .relative()
            .w_full()
            .h(px(12.))
            .rounded(px(6.))
            .overflow_hidden()
            .bg(ca(30, 30, 30, 0.8))
            .child(
                div().h_full().w(gpui::relative(m.pct as f32 / 100.)).rounded(px(6.)).bg(gpui::linear_gradient(
                    90.,
                    gpui::linear_color_stop(c(0x59fcb3), 0.),
                    gpui::linear_color_stop(c(0x00d4ff), 1.),
                )),
            )
            .child(
                div().absolute().top_0().left(gpui::relative(glow * 1.4 - 0.1)).w(px(20.)).h_full().opacity(0.7).bg(gpui::linear_gradient(
                    90.,
                    gpui::linear_color_stop(white_a(0.), 0.),
                    gpui::linear_color_stop(white_a(0.75), 0.5),
                )),
            )
            .child(
                div()
                    .absolute()
                    .right(px(10.))
                    .top(px(-6.))
                    .h(px(24.))
                    .flex()
                    .items_center()
                    .font_family(crate::markdown::MONO)
                    .text_size(px(11.))
                    .font_weight(FontWeight::BOLD)
                    .text_color(c(0xffffff))
                    .child(format!("{}%", m.pct)),
            );
        let modal = div()
            .max_w(px(400.))
            .mx(px(10.))
            .p(px(40.))
            .flex()
            .flex_col()
            .items_center()
            .rounded(px(20.))
            .border_2()
            .border_color(c(0x333333))
            .bg(gpui::linear_gradient(145., gpui::linear_color_stop(c(0x1a1a1a), 0.), gpui::linear_color_stop(c(0x0d0d0d), 1.)))
            .shadow(vec![gpui::BoxShadow { color: ca(0, 0, 0, 0.8), offset: gpui::point(px(0.), px(0.)), blur_radius: px(60.), spread_radius: px(0.), inset: false }])
            .font_family(INTER)
            .text_color(c(TEXT))
            .child(div().size(px(48.)).mb(px(20.)).opacity(0.8).flex().items_center().justify_center().child(svg().path("icons/locked.svg").size(px(31.2)).text_color(c(0xffffff))))
            .child(div().mb(px(10.)).text_size(px(22.)).line_height(px(24.)).font_weight(FontWeight::SEMIBOLD).child(m.title.clone()))
            .child(div().mb(px(25.)).text_size(px(14.)).line_height(px(24.)).opacity(0.7).child(m.phase.clone()))
            .child(div().w_full().mb(px(25.)).child(track))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap(px(8.))
                    .text_size(px(13.))
                    .line_height(px(24.))
                    .opacity(0.8)
                    .child(div().size(px(16.)).flex().items_center().justify_center().child(svg().path("icons/info.svg").size(px(10.4)).text_color(c(0xf5a623)).opacity(0.5)))
                    .child("Do not close the app during this process"),
            );
        Some(
            gpui::deferred(div().id("migration-overlay").absolute().inset_0().flex().items_center().justify_center().bg(ca(0, 0, 0, 0.95)).occlude().child(modal))
                .with_priority(13)
                .into_any_element(),
        )
    }
}

impl Render for SecurityFlow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let keys = self.keys_card(cx);
        let migration = self.migration_card(window);
        let prompt = self.prompt_card(window, cx);
        let confirm = self.confirm.as_ref().map(|(p, _)| {
            let me = cx.entity().downgrade();
            kit::popup(p, move |yes, _, cx| {
                me.update(cx, |f, cx| {
                    if let Some((_, tx)) = f.confirm.take() {
                        let _ = tx.send(yes);
                    }
                    cx.notify();
                })
                .ok();
            })
        });
        // Spans the window, so each overlay's inset is the window's.
        div().absolute().inset_0().children(keys).children(migration).children(prompt).children(confirm)
    }
}
