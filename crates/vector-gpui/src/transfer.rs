//! Device transfer (`TransferModal`): one card for both sides. Either device shows a code (QR
//! plus text) or types the other's; the new device then shows a number and whose account is
//! coming, the signed-in one asks for that number, and the new device signs in once it arrives.

use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::prelude::*;
use gpui::{AnyElement, Context, Entity, FocusHandle, Focusable, FontWeight, KeyDownEvent, RenderImage, SharedString, Window, div, px, svg};
use crate::pic::img;
use serde_json::Value;

use crate::backend;
use crate::composer::{InputChanged, Look, SendText, TextInput};
use crate::security::{
    Event, SecurityFlow, actions, button, card, head, lead, lead_mixed, noun, overlay,
};
use crate::theme::*;

const CODE_LIFE: Duration = Duration::from_secs(5 * 60);
const CODE_LABEL: &str = "vector transfer code:";

#[derive(Clone, Copy, PartialEq)]
pub enum Stage {
    Show,
    Enter,
    Connecting,
    Match,
    Approve,
    Sending,
    Sent,
    Finishing,
    Error,
}

pub struct Xfer {
    sender: bool,
    stage: Stage,
    code: SharedString,
    qr: Option<Arc<RenderImage>>,
    expires: Instant,
    entry: Entity<TextInput>,
    number: String,
    number_focus: FocusHandle,
    sas: String,
    from: String,
    name: String,
    avatar: Option<Arc<RenderImage>>,
    busy: bool,
    unconfirmed: bool,
    finish_failed: bool,
    error: String,
    copied: Option<Instant>,
    /// Bumped by every user step, so a slow answer to an abandoned one changes nothing.
    seq: u64,
    /// The newest transfer the backend has named; events below the floor are abandoned ones.
    id: u64,
    floor: u64,
    focus: bool,
}

impl Xfer {
    fn closable(&self) -> bool {
        !self.busy && !matches!(self.stage, Stage::Sending | Stage::Finishing)
    }
}

/// Codes read "7-orbit-lemon-stage": lowercase, one dash between parts, nothing else.
fn clean_code(text: &str) -> String {
    let lower = text.trim_start().to_lowercase();
    let body = lower.strip_prefix(CODE_LABEL).unwrap_or(&lower);
    let mut out = String::with_capacity(body.len());
    let mut dash = false;
    for ch in body.chars() {
        if ch.is_ascii_alphanumeric() {
            if dash && !out.is_empty() {
                out.push('-');
            }
            dash = false;
            out.push(ch);
        } else {
            dash = true;
        }
    }
    if dash && !out.is_empty() {
        out.push('-');
    }
    out.truncate(40);
    out
}

/// A PNG data URL, as the sender's avatar arrives.
fn data_image(url: &str) -> Option<Arc<RenderImage>> {
    let b64 = url.split_once(";base64,")?.1;
    let bytes = vector_core::simd::base64::STANDARD
        .decode_to_vec(b64)
        .ok()?;
    let mut buf = image::load_from_memory(&bytes).ok()?.into_rgba8();
    for p in buf.chunks_exact_mut(4) {
        p.swap(0, 2);
    }
    Some(Arc::new(RenderImage::new(vec![image::Frame::new(buf)])))
}

fn short(npub: &str) -> String {
    if npub.len() > 18 {
        format!("{}…{}", &npub[..12], &npub[npub.len() - 6..])
    } else {
        npub.to_string()
    }
}

impl SecurityFlow {
    fn xfer_mut(&mut self) -> Option<&mut Xfer> {
        self.xfer.as_mut()
    }

    /// Open the card. `send` only on a signed-in device the user is holding: Settings.
    pub fn open_transfer(&mut self, send: bool, cx: &mut Context<Self>) {
        let look = Look {
            family: crate::markdown::MONO,
            weight: FontWeight::MEDIUM,
            ..crate::kit::field_look()
        };
        let entry = cx.new(|cx| {
            TextInput::new("7-orbit-lemon-stage", cx)
                .with_look(look)
                .keep_on_submit()
        });
        cx.subscribe(&entry, |_, input, _: &InputChanged, cx| {
            let text = input.read(cx).text().to_string();
            let clean = clean_code(&text);
            if clean != text {
                input.update(cx, |i, cx| i.set_text(&clean, cx));
            }
            cx.notify();
        })
        .detach();
        cx.subscribe(&entry, |f, _, _: &SendText, cx| f.connect(cx))
            .detach();
        self.xfer = Some(Xfer {
            sender: send,
            stage: Stage::Connecting,
            code: SharedString::default(),
            qr: None,
            expires: Instant::now(),
            entry,
            number: String::new(),
            number_focus: cx.focus_handle(),
            sas: String::new(),
            from: String::new(),
            name: String::new(),
            avatar: None,
            busy: false,
            unconfirmed: false,
            finish_failed: false,
            error: String::new(),
            copied: None,
            seq: 0,
            id: 0,
            floor: 0,
            focus: false,
        });
        self.show_code(cx);
    }

    /// A code read off the camera: straight to connecting, as if typed.
    pub fn join_scanned(&mut self, send: bool, code: &str, cx: &mut Context<Self>) {
        self.open_transfer(send, cx);
        let Some(x) = self.xfer.as_mut() else { return };
        x.stage = Stage::Enter;
        let entry = x.entry.clone();
        entry.update(cx, |i, cx| i.set_text(&clean_code(code), cx));
        self.connect(cx);
    }

    /// Leave the current transfer behind: its answers and events no longer count.
    fn abandon(&mut self) {
        if let Some(x) = self.xfer.as_mut() {
            x.seq += 1;
            x.floor = x.id + 1;
        }
        if self.live {
            let _ = backend::run(async { backend::transfer::cancel() });
        }
    }

    pub fn close_transfer(&mut self, cx: &mut Context<Self>) {
        if !self.xfer.as_ref().is_some_and(Xfer::closable) {
            return;
        }
        self.abandon();
        self.xfer = None;
        cx.notify();
    }

    fn start(
        &mut self,
        code: Option<String>,
        cx: &mut Context<Self>,
    ) -> Option<(
        u64,
        futures::channel::oneshot::Receiver<Result<backend::transfer::Started, String>>,
    )> {
        let x = self.xfer.as_ref()?;
        let (seq, send) = (x.seq, x.sender);
        let rx = if self.live {
            backend::run(backend::transfer::start(send, code))
        } else {
            // Fixtures: the reference's code, so the card can be shown.
            let (tx, rx) = futures::channel::oneshot::channel();
            let shown = code.is_none();
            let _ = tx.send(Ok(backend::transfer::Started {
                id: 1,
                code: shown.then(|| "7-orbit-lemon-stage".into()),
                qr: shown.then(|| "Vector transfer code: 7-orbit-lemon-stage".into()),
            }));
            rx
        };
        cx.notify();
        Some((seq, rx))
    }

    fn show_code(&mut self, cx: &mut Context<Self>) {
        self.abandon();
        let Some(x) = self.xfer_mut() else { return };
        x.stage = Stage::Connecting;
        x.error.clear();
        x.busy = false;
        x.number.clear();
        x.sas.clear();
        x.finish_failed = false;
        let Some((seq, rx)) = self.start(None, cx) else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let result = rx
                .await
                .unwrap_or_else(|_| Err("The transfer stopped before it began".into()));
            this.update(cx, |f, cx| {
                let Some(x) = f.xfer.as_mut().filter(|x| x.seq == seq) else {
                    return;
                };
                match result {
                    Ok(started) => {
                        x.id = started.id;
                        if x.stage == Stage::Connecting {
                            x.stage = Stage::Show;
                            x.code = started.code.unwrap_or_default().into();
                            x.qr = started.qr.as_deref().and_then(|t| crate::qr::image(t, 440));
                            x.expires = Instant::now() + CODE_LIFE;
                            f.tick_expiry(seq, cx);
                        }
                    }
                    Err(e) => {
                        x.stage = Stage::Error;
                        x.error = e;
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Repaint the countdown each second while the code shows.
    fn tick_expiry(&mut self, seq: u64, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let going = this
                    .update(cx, |f, cx| {
                        cx.notify();
                        f.xfer.as_ref().is_some_and(|x| {
                            x.seq == seq
                                && x.stage == Stage::Show
                                && Instant::now() < x.expires + Duration::from_secs(1)
                        })
                    })
                    .unwrap_or(false);
                if !going {
                    break;
                }
            }
        })
        .detach();
    }

    fn enter_code(&mut self, cx: &mut Context<Self>) {
        self.abandon();
        let Some(x) = self.xfer_mut() else { return };
        x.stage = Stage::Enter;
        x.error.clear();
        x.busy = false;
        x.focus = true;
        let entry = x.entry.clone();
        entry.update(cx, |i, cx| i.set_text("", cx));
        cx.notify();
    }

    fn connect(&mut self, cx: &mut Context<Self>) {
        let Some(x) = self
            .xfer
            .as_ref()
            .filter(|x| x.stage == Stage::Enter && !x.busy)
        else {
            return;
        };
        let code = x.entry.read(cx).text().trim().to_string();
        if code.is_empty() {
            return;
        }
        self.abandon();
        let Some(x) = self.xfer_mut() else { return };
        x.busy = true;
        x.error.clear();
        let Some((seq, rx)) = self.start(Some(code.clone()), cx) else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let result = rx
                .await
                .unwrap_or_else(|_| Err("The transfer stopped before it began".into()));
            this.update(cx, |f, cx| {
                let Some(x) = f.xfer.as_mut().filter(|x| x.seq == seq) else {
                    return;
                };
                x.busy = false;
                match result {
                    Ok(started) => {
                        x.id = started.id;
                        if x.stage == Stage::Enter {
                            x.stage = Stage::Connecting;
                        }
                    }
                    Err(e) => {
                        x.stage = Stage::Enter;
                        if code.to_lowercase().starts_with(CODE_LABEL) {
                            x.entry.update(cx, |i, cx| i.set_text("", cx));
                        }
                        x.error = e;
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn copy_code(&mut self, cx: &mut Context<Self>) {
        let Some(x) = self.xfer_mut() else { return };
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(x.code.to_string()));
        let at = Instant::now();
        x.copied = Some(at);
        cx.emit(Event::Toast("Copied Code".into()));
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(1500))
                .await;
            this.update(cx, |f, cx| {
                if let Some(x) = f.xfer.as_mut().filter(|x| x.copied == Some(at)) {
                    x.copied = None;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Try Again: a failed sign-in retries itself; anything else starts over.
    fn retry(&mut self, cx: &mut Context<Self>) {
        if self.xfer.as_ref().is_some_and(|x| x.finish_failed) {
            self.finish_received(cx);
        } else {
            self.show_code(cx);
        }
    }

    fn number_key(&mut self, e: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let pasted = (e.keystroke.modifiers.platform || e.keystroke.modifiers.control)
            && e.keystroke.key == "v";
        let clip = pasted
            .then(|| cx.read_from_clipboard().and_then(|i| i.text()))
            .flatten();
        let Some(x) = self
            .xfer
            .as_mut()
            .filter(|x| x.stage == Stage::Approve && !x.busy)
        else {
            return;
        };
        match e.keystroke.key.as_str() {
            "backspace" => {
                x.number.pop();
            }
            "enter" => return self.approve(cx),
            "escape" => return self.close_transfer(cx),
            _ => {
                let typed = clip
                    .or_else(|| e.keystroke.key_char.clone())
                    .unwrap_or_default();
                for d in typed.chars().filter(char::is_ascii_digit) {
                    if x.number.len() < 6 {
                        x.number.push(d);
                    }
                }
            }
        }
        cx.notify();
    }

    /// Sender: the number from the new device, then the user's proof, then the account goes.
    fn approve(&mut self, cx: &mut Context<Self>) {
        let live = self.live;
        let Some(x) = self
            .xfer
            .as_mut()
            .filter(|x| x.stage == Stage::Approve && !x.busy && x.number.len() == 6)
        else {
            return;
        };
        x.busy = true;
        x.error.clear();
        let (seq, number) = (x.seq, x.number.clone());
        cx.notify();
        cx.spawn(async move |this, cx| {
            let current = |f: &SecurityFlow| {
                f.xfer
                    .as_ref()
                    .is_some_and(|x| x.seq == seq && x.stage == Stage::Approve)
            };
            let checked = if live {
                backend::run(backend::transfer::check_number(number))
                    .await
                    .unwrap_or(Err(Err("The transfer has ended".into())))
            } else {
                Ok(())
            };
            if let Err(e) = checked {
                this.update(cx, |f, cx| {
                    if !current(f) {
                        return;
                    }
                    let x = f.xfer.as_mut().unwrap();
                    x.busy = false;
                    x.number.clear();
                    x.error = match e {
                        Ok(left) => format!(
                            "That isn't the number on the new device. {left} {} left.",
                            if left == 1 { "try" } else { "tries" }
                        ),
                        Err(e) => e,
                    };
                    cx.notify();
                })
                .ok();
                return;
            }
            let first = if live {
                backend::run(backend::transfer::approve(None))
                    .await
                    .unwrap_or_else(|_| Err("cancelled".into()))
            } else {
                Ok(())
            };
            let result = match first {
                Err(e)
                    if e == "CREDENTIAL_REQUIRED"
                        && this.read_with(cx, |f, _| current(f)).unwrap_or(false) =>
                {
                    let k = crate::security::kind(&this, cx);
                    crate::security::with_current(
                        &this,
                        cx,
                        "Approve the Transfer",
                        format!(
                            "Enter your {} to send this account to the new device.",
                            noun(&k)
                        ),
                        |cred| backend::run(backend::transfer::approve(Some(cred))),
                    )
                    .await
                }
                other => other.map(Some),
            };
            this.update(cx, |f, cx| {
                if !current(f) {
                    return;
                }
                let x = f.xfer.as_mut().unwrap();
                match result {
                    Ok(Some(())) => {}
                    Ok(None) => x.busy = false,
                    Err(e) => {
                        x.busy = false;
                        x.error = e;
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn deny(&mut self, cx: &mut Context<Self>) {
        if let Some(x) = self.xfer.as_mut() {
            x.seq += 1;
            x.floor = x.id + 1;
        }
        if self.live {
            let _ = backend::run(async { backend::transfer::deny() });
        }
        self.xfer = None;
        cx.notify();
    }

    /// Receiver: the account arrived; the app signs in with it.
    fn finish_received(&mut self, cx: &mut Context<Self>) {
        let Some(x) = self.xfer_mut() else { return };
        x.stage = Stage::Finishing;
        x.error.clear();
        x.finish_failed = false;
        cx.emit(Event::Received);
        cx.notify();
    }

    /// How signing in with the received account went.
    pub fn transfer_finished(&mut self, result: Result<(), String>, cx: &mut Context<Self>) {
        let Some(x) = self.xfer.as_mut().filter(|x| x.stage == Stage::Finishing) else {
            return;
        };
        match result {
            Ok(()) => self.xfer = None,
            Err(e) => {
                let gone = e.contains("Nothing has arrived");
                x.stage = Stage::Error;
                x.finish_failed = !gone;
                x.error = if gone {
                    "The account that arrived has expired. Start again.".into()
                } else {
                    format!("Couldn't finish signing in: {e}")
                };
            }
        }
        cx.notify();
    }

    /// `transfer_state` from core.
    pub fn transfer_state(&mut self, p: &Value, cx: &mut Context<Self>) {
        let Some(x) = self.xfer.as_mut() else { return };
        let id = p.get("id").and_then(Value::as_u64).unwrap_or(0);
        let stage = p.get("stage").and_then(Value::as_str).unwrap_or_default();
        let ours: &[&str] = if x.sender {
            &["approve", "sending", "sent", "error"]
        } else {
            &["match", "received", "error"]
        };
        if id < x.floor || !ours.contains(&stage) || x.stage == Stage::Finishing {
            return;
        }
        let text = |k: &str| p.get(k).and_then(Value::as_str).map(str::to_string);
        match stage {
            "match" => {
                x.stage = Stage::Match;
                x.sas = text("sas").unwrap_or_default();
                x.from = text("sender").unwrap_or_default();
                x.name = text("name").unwrap_or_default();
                x.avatar = text("avatar").as_deref().and_then(data_image);
            }
            "approve" => {
                x.stage = Stage::Approve;
                x.number.clear();
                x.error.clear();
                x.busy = false;
                x.focus = true;
            }
            "sending" => x.stage = Stage::Sending,
            "sent" => {
                x.stage = Stage::Sent;
                x.unconfirmed = p
                    .get("unconfirmed")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                x.busy = false;
            }
            "received" => {
                if let Some(s) = text("sender") {
                    x.from = s;
                }
                if let Some(n) = text("name") {
                    x.name = n;
                }
                if let Some(a) = text("avatar").as_deref().and_then(data_image) {
                    x.avatar = Some(a);
                }
                return self.finish_received(cx);
            }
            _ => {
                x.stage = Stage::Error;
                x.error = text("error").unwrap_or_else(|| "The transfer stopped.".into());
                x.busy = false;
            }
        }
        cx.notify();
    }

    // ── Rendering ─────────────────────────────────────────────────────────────

    pub(crate) fn xfer_card(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let x = self.xfer.as_mut()?;
        if std::mem::take(&mut x.focus) {
            match x.stage {
                Stage::Enter => window.focus(&x.entry.focus_handle(cx), cx),
                Stage::Approve => window.focus(&x.number_focus, cx),
                _ => {}
            }
        }
        let x = self.xfer.as_ref()?;
        let me = cx.entity().downgrade();
        let on = move |f: fn(&mut SecurityFlow, &mut Context<SecurityFlow>)| {
            let me = me.clone();
            move |_: &gpui::ClickEvent, _: &mut Window, cx: &mut gpui::App| {
                me.update(cx, |s, cx| f(s, cx)).ok();
            }
        };
        let title = match x.stage {
            Stage::Match => "Check the number",
            Stage::Approve => "Sign in on a new device?",
            Stage::Finishing => "Signing in",
            Stage::Sent if x.unconfirmed => "Sent",
            Stage::Sent => "All set",
            Stage::Error => "Transfer stopped",
            _ if x.sender => "Sign in on another device",
            _ => "Sign in with another device",
        };
        let close = x.closable().then(|| {
            Box::new(on(Self::close_transfer))
                as Box<dyn Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App)>
        });
        let mut body = card("xfer-card").child(head("icons/devices.svg", title.into(), close));
        let link = |id: &'static str,
                    text: &'static str,
                    enabled: bool,
                    f: fn(&mut SecurityFlow, &mut Context<SecurityFlow>)| {
            div()
                .id(id)
                .self_center()
                .px(px(4.))
                .py(px(6.))
                .text_size(px(13.))
                .line_height(px(16.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(white_a(0.55))
                .when(!enabled, |d| d.opacity(0.4))
                .when(enabled, |d| {
                    d.hover(|d| d.text_color(c(0xffffff)))
                        .cursor_pointer()
                        .on_click(on(f))
                })
                .child(text)
        };
        let error = |text: &str| {
            div()
                .mt(px(-4.))
                .text_size(px(12.5))
                .line_height(px(17.5))
                .text_color(c(0xff7a8a))
                .child(SharedString::from(text.to_string()))
        };
        let spinner = |text: &'static str| {
            window.request_animation_frame();
            div()
                .pt(px(28.))
                .pb(px(22.))
                .flex()
                .items_center()
                .justify_center()
                .gap(px(10.))
                .text_size(px(14.))
                .text_color(white_a(0.75))
                .child(div().size(px(18.)).child(crate::attachments::spinner(
                    18.,
                    2.,
                    0.8,
                    0.25,
                    white_a(0.15),
                    c(ACCENT),
                )))
                .child(text)
        };
        let identity = || {
            let face = match &x.avatar {
                Some(a) => img(a.clone()).object_fit(gpui::ObjectFit::Cover)
                    .size(px(44.))
                    .rounded_full()
                    .into_any_element(),
                None => img("icons/user-placeholder.svg")
                    .size(px(44.))
                    .rounded_full()
                    .into_any_element(),
            };
            div()
                .flex()
                .items_center()
                .gap(px(12.))
                .px(px(14.))
                .py(px(12.))
                .rounded(px(12.))
                .border_1()
                .border_color(c(0x232526))
                .bg(c(0x161718))
                .child(face)
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(3.))
                        .min_w_0()
                        .child(
                            div()
                                .text_size(px(16.))
                                .font_weight(FontWeight::BOLD)
                                .child(SharedString::from(if x.name.is_empty() {
                                    "Unnamed account".to_string()
                                } else {
                                    x.name.clone()
                                })),
                        )
                        .when(!x.from.is_empty(), |d| {
                            d.child(
                                div()
                                    .font_family(crate::markdown::MONO)
                                    .text_size(px(12.))
                                    .text_color(white_a(0.5))
                                    .child(short(&x.from)),
                            )
                        }),
                )
        };
        let field = |focused: bool| {
            div()
                .w_full()
                .px(px(14.))
                .py(px(12.))
                .rounded(px(12.))
                .border_1()
                .border_color(if focused { c(ACCENT) } else { c(0x2c2e2f) })
                .bg(c(0x161718))
        };
        match x.stage {
            Stage::Show => {
                let left = x
                    .expires
                    .saturating_duration_since(Instant::now())
                    .as_secs_f32()
                    .ceil() as u64;
                let clock = format!("{}:{:02}", left / 60, left % 60);
                body = body.child(lead(
                    if x.sender {
                        "On the new device, tap Login, then Sign in with another device, and scan this or type the code."
                    } else {
                        "On your signed-in device, open Settings and choose Sign in on another device, then scan this or type the code."
                    },
                    false,
                ));
                body = body.child(
                    div()
                        .self_center()
                        .size(px(248.))
                        .p(px(14.))
                        .rounded(px(14.))
                        .bg(c(0xffffff))
                        .children(x.qr.clone().map(|q| img(q).size(px(220.)))),
                );
                let copied = x.copied.is_some();
                body = body.child(
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap(px(6.))
                        .child(
                            div()
                                .flex()
                                .gap(px(8.))
                                .child(
                                    div()
                                        .id("xfer-code")
                                        .h(px(37.))
                                        .px(px(14.))
                                        .flex()
                                        .items_center()
                                        .rounded(px(10.))
                                        .border_1()
                                        .border_color(c(0x232526))
                                        .bg(c(0x161718))
                                        .hover(|d| d.border_color(c(0x3a3c3d)).bg(c(0x1c1d1e)))
                                        .cursor_pointer()
                                        .on_click(on(Self::copy_code))
                                        .font_family(crate::markdown::MONO)
                                        .text_size(px(16.))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(crate::kit::spaced(x.code.as_ref(), 0.32)),
                                )
                                .child(
                                    div()
                                        .id("xfer-copy")
                                        .tooltip(crate::kit::tooltip("Copy"))
                                        .w(px(38.))
                                        .h(px(37.))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .rounded(px(10.))
                                        .border_1()
                                        .border_color(c(0x232526))
                                        .bg(c(0x161718))
                                        .hover(|d| d.border_color(c(0x3a3c3d)).bg(c(0x1c1d1e)))
                                        .cursor_pointer()
                                        .on_click(on(Self::copy_code))
                                        .child(
                                            svg()
                                                .path(if copied {
                                                    "icons/check.svg"
                                                } else {
                                                    "icons/copy.svg"
                                                })
                                                .size(px(18.))
                                                .text_color(c(ACCENT)),
                                        ),
                                ),
                        )
                        .child(
                            div()
                                .text_size(px(12.))
                                .line_height(px(24.))
                                .text_color(if left <= 30 {
                                    c(0xff7a8a)
                                } else {
                                    white_a(0.45)
                                })
                                .child(if left > 0 {
                                    format!("Expires in {clock}")
                                } else {
                                    "Expired".into()
                                }),
                        ),
                );
                let mut a = actions();
                if left == 0 {
                    a = a.child(button(
                        "xfer-new",
                        "Get a New Code",
                        false,
                        true,
                        on(Self::show_code),
                    ));
                }
                body = body.child(a.child(link(
                    "xfer-type",
                    "Type the other device's code instead",
                    true,
                    Self::enter_code,
                )));
            }
            Stage::Enter => {
                let focused = x.entry.focus_handle(cx).is_focused(window);
                let typed = !x.entry.read(cx).text().trim().is_empty();
                body = body.child(lead(
                    "Type the code shown on your other device. It's a number and three words.",
                    false,
                ));
                let mut form = div().flex().flex_col().gap(px(12.)).child(
                    field(focused)
                        .when(x.busy, |d| d.opacity(0.6))
                        .child(x.entry.clone()),
                );
                if !x.error.is_empty() {
                    form = form.child(error(&x.error));
                }
                form = form.child(
                    actions()
                        .child(button(
                            "xfer-connect",
                            if x.busy { "Connecting…" } else { "Connect" },
                            false,
                            !x.busy && typed,
                            on(Self::connect),
                        ))
                        .child(link(
                            "xfer-show",
                            "Show a code on this device instead",
                            !x.busy,
                            Self::show_code,
                        )),
                );
                body = body.child(form);
            }
            Stage::Connecting => body = body.child(spinner("Connecting…")),
            Stage::Match => {
                body = body.child(div().flex().justify_center().child(lead(
                    "On your other device, type this number to approve.",
                    false,
                )));
                let digit = |d: char| {
                    div()
                        .w(px(42.))
                        .h(px(56.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(10.))
                        .border_1()
                        .border_color(c(0x2c2e2f))
                        .bg(c(0x161718))
                        .text_size(px(28.))
                        .font_weight(FontWeight::BOLD)
                        .text_color(c(ACCENT))
                        .child(d.to_string())
                };
                let split = x.sas.len().min(3);
                let group = |s: &str| div().flex().gap(px(6.)).children(s.chars().map(digit));
                body = body.child(
                    div()
                        .my(px(2.))
                        .flex()
                        .justify_center()
                        .gap(px(14.))
                        .child(group(&x.sas[..split]))
                        .child(group(&x.sas[split..])),
                );
                if !x.from.is_empty() {
                    body = body
                        .child(
                            div()
                                .mb(px(-8.))
                                .flex()
                                .justify_center()
                                .text_size(px(12.))
                                .text_color(white_a(0.45))
                                .child("Signing in as"),
                        )
                        .child(identity());
                }
                body = body.child(actions().child(button(
                    "xfer-cancel",
                    "Cancel",
                    true,
                    true,
                    on(Self::close_transfer),
                )));
            }
            Stage::Approve => {
                body = body.child(lead_mixed(&[
                    (
                        "You're giving full, permanent control of this account to another device. ",
                        false,
                    ),
                    (
                        "Only continue if you're setting up your own device right now.",
                        true,
                    ),
                    (
                        " Vector never asks for this to verify, unlock or join anything.",
                        false,
                    ),
                ]));
                let focused = x.number_focus.is_focused(window);
                let shown = if x.number.len() > 3 {
                    format!("{} {}", &x.number[..3], &x.number[3..])
                } else {
                    x.number.clone()
                };
                let empty = shown.is_empty();
                let mut form = div()
                    .flex()
                    .flex_col()
                    .gap(px(12.))
                    .child(
                        div()
                            .h(px(24.))
                            .flex()
                            .items_center()
                            .text_size(px(11.))
                            .font_weight(FontWeight::BOLD)
                            .text_color(white_a(0.45))
                            .child(crate::kit::spaced("NUMBER SHOWN ON THE NEW DEVICE", 0.6)),
                    )
                    .child(
                        field(focused)
                            .id("xfer-number")
                            .track_focus(&x.number_focus)
                            .on_key_down(cx.listener(Self::number_key))
                            .h(px(45.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .when(x.busy, |d| d.opacity(0.6))
                            .font_family(crate::markdown::MONO)
                            .font_weight(FontWeight::MEDIUM)
                            .text_size(px(16.))
                            .text_color(if empty { c(0x757575) } else { c(0xffffff) })
                            .child(crate::kit::spaced(
                                if empty { "000 000".to_string() } else { shown },
                                2.88,
                            )),
                    );
                if !x.error.is_empty() {
                    form = form.child(error(&x.error));
                }
                let ready = !x.busy && x.number.len() == 6;
                form = form.child(
                    actions()
                        .child(button(
                            "xfer-approve",
                            "Approve",
                            false,
                            ready,
                            on(Self::approve),
                        ))
                        .child(button("xfer-deny", "Deny", true, !x.busy, on(Self::deny))),
                );
                body = body.child(form);
            }
            Stage::Sending => body = body.child(spinner("Sending your account…")),
            Stage::Sent => {
                body = body
                    .child(lead(
                        if x.unconfirmed {
                            "Your account is on its way. Check the new device to finish signing in."
                        } else {
                            "Your new device is signing in. Your chats and communities will sync there on their own."
                        },
                        false,
                    ))
                    .child(actions().child(button("xfer-done", "Done", false, true, on(Self::close_transfer))));
            }
            Stage::Finishing => {
                if !x.from.is_empty() {
                    body = body.child(identity());
                }
                body = body.child(spinner("Signing in…"));
            }
            Stage::Error => {
                body = body
                    .child(lead(SharedString::from(x.error.clone()), false))
                    .child(
                        actions()
                            .child(button(
                                "xfer-retry",
                                "Try Again",
                                false,
                                true,
                                on(Self::retry),
                            ))
                            .child(button(
                                "xfer-close",
                                "Close",
                                true,
                                true,
                                on(Self::close_transfer),
                            )),
                    );
            }
        }
        Some(overlay("xfer-overlay", 11, body))
    }
}

#[cfg(test)]
mod tests {
    use super::clean_code;

    #[test]
    fn codes_read_as_one_dashed_lowercase_line() {
        assert_eq!(
            clean_code("Vector transfer code: 7-Orbit lemon__stage"),
            "7-orbit-lemon-stage"
        );
        assert_eq!(clean_code("  7 orbit "), "7-orbit-");
        assert_eq!(clean_code("--7"), "7");
    }
}
