//! Login with Remote Signer: a pasted `bunker://` link, or a `nostrconnect://` QR the signer
//! scans. The QR lives exactly as long as its single-use link.

use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt;
use gpui::{AnyElement, ClipboardItem, Context, Entity, Focusable, FontWeight, InteractiveElement, IntoElement, ParentElement, RenderImage, SharedString, StatefulInteractiveElement, Styled, Window, div, img, prelude::FluentBuilder, px};

use super::LoginView;
use crate::backend::{self, account, bunker};
use crate::composer::TextInput;
use crate::theme::*;

const LINK_LIFE: Duration = Duration::from_secs(120);

#[derive(Clone, Copy, PartialEq)]
pub(super) enum Tone {
    Plain,
    Connecting,
    Online,
    Error,
}

pub(super) struct Signer {
    pub url: Entity<TextInput>,
    pub busy: bool,
    pub status: Option<(SharedString, Tone)>,
    pub qr_open: bool,
    link: Option<String>,
    qr: Option<Arc<RenderImage>>,
    /// The link's countdown, until the signer answers.
    deadline: Option<Instant>,
    copied: bool,
    /// Bumped per link, so a dead session's updates are ignored.
    session: u64,
}

impl Signer {
    pub fn new(url: Entity<TextInput>) -> Self {
        Self { url, busy: false, status: None, qr_open: false, link: None, qr: None, deadline: None, copied: false, session: 0 }
    }

    /// The countdown owns the status line while a link is live.
    fn line(&self) -> Option<(SharedString, Tone)> {
        if let Some(left) = self.deadline.and_then(|d| d.checked_duration_since(Instant::now())) {
            let secs = left.as_secs_f32().ceil() as u64;
            return Some((format!("Waiting for signer… ({}:{:02})", secs / 60, secs % 60).into(), Tone::Connecting));
        }
        self.status.clone()
    }
}

fn tone_color(t: Tone) -> u32 {
    match t {
        Tone::Plain => 0xb2b2b2,
        Tone::Connecting => 0xffb84d,
        Tone::Online => 0x59fcb3,
        Tone::Error => 0xff2ea9,
    }
}

impl LoginView {
    pub(super) fn signer_connect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let url = self.signer.url.read(cx).text().trim().to_string();
        if self.signer.busy || url.is_empty() {
            return;
        }
        if !url.to_ascii_lowercase().starts_with("bunker://") {
            self.signer.status = Some(("Must start with bunker://".into(), Tone::Error));
            return cx.notify();
        }
        let Ok(add) = self.gate(cx) else { return };
        self.signer.busy = true;
        self.signer.status = Some(("Connecting to signer…".into(), Tone::Connecting));
        cx.notify();
        let rx = backend::run(async move {
            if let Some((kind, opts)) = add {
                account::enter_add_mode(kind, opts).await?;
            }
            let imported = bunker::connect(url).await?;
            if matches!(imported, account::Imported::Staged(_)) {
                account::connect().await;
            }
            Ok::<_, String>(imported)
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = rx.await.unwrap_or_else(|_| Err("Backend stopped".into()));
            this.update_in(cx, |this, window, cx| {
                this.signer.busy = false;
                match result {
                    Ok(imported) => {
                        this.signer.status = None;
                        this.signer.url.update(cx, |u, cx| u.clear(window, cx));
                        this.imported(imported, window, cx);
                    }
                    Err(e) => {
                        this.net_settle(cx);
                        this.signer.status = Some((e.into(), Tone::Error));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// A live link shows again as it is; otherwise a fresh session starts.
    pub(super) fn signer_open_qr(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.signer.qr_open = true;
        if self.signer.link.is_some() && self.signer.deadline.is_some_and(|d| d > Instant::now()) {
            return cx.notify();
        }
        self.signer_session(window, cx);
    }

    pub(super) fn signer_close_qr(&mut self, cx: &mut Context<Self>) {
        self.signer.qr_open = false;
        cx.notify();
    }

    fn signer_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let reauth = self.reauth.is_some();
        let add = if reauth { None } else {
            let Ok(add) = self.gate(cx) else { return };
            add
        };
        self.signer.session += 1;
        let session = self.signer.session;
        self.signer.link = None;
        self.signer.qr = None;
        self.signer.copied = false;
        self.signer.deadline = None;
        self.signer.status = Some(("Waiting for signer…".into(), Tone::Connecting));
        cx.notify();
        let (tx, mut updates) = futures::channel::mpsc::unbounded();
        let rx = backend::run(async move {
            if let Some((kind, opts)) = add {
                account::enter_add_mode(kind, opts).await?;
            }
            if reauth { bunker::reauthorize(tx).await } else { bunker::start_session(tx).await }
        });
        cx.spawn_in(window, async move |this, cx| {
            let started = rx.await.unwrap_or_else(|_| Err("Backend stopped".into()));
            let ok = this
                .update(cx, |this, cx| {
                    if this.signer.session != session {
                        return false;
                    }
                    match started {
                        Ok(link) => {
                            this.signer.qr = crate::qr::image(&link, 556);
                            this.signer.link = Some(link);
                            this.signer.deadline = Some(Instant::now() + LINK_LIFE);
                            cx.notify();
                            true
                        }
                        Err(e) => {
                            this.signer.status = Some((e.into(), Tone::Error));
                            cx.notify();
                            false
                        }
                    }
                })
                .unwrap_or(false);
            if !ok {
                return;
            }
            Self::tick(this.clone(), session, cx.clone());
            while let Some(update) = updates.next().await {
                let go_on = this
                    .update_in(cx, |this, window, cx| {
                        if this.signer.session != session {
                            return false;
                        }
                        match update {
                            bunker::Update::Awaiting => {
                                // Waiting on a human in the signer: the link no longer expires here.
                                this.signer.deadline = None;
                                this.signer.status = Some(("Check your signer app to approve…".into(), Tone::Connecting));
                            }
                            bunker::Update::Done(Ok(imported)) => {
                                this.signer.deadline = None;
                                this.signer.link = None;
                                this.signer.qr = None;
                                this.signer.qr_open = false;
                                if let Some(at_unlock) = this.reauth {
                                    this.signer.status = Some(("Signer reconnected.".into(), Tone::Online));
                                    this.reauthorized(at_unlock, window, cx);
                                    return false;
                                }
                                this.signer.status = Some(("Connected. Choosing security…".into(), Tone::Online));
                                if matches!(imported, account::Imported::Staged(_)) {
                                    backend::run(account::connect());
                                }
                                this.imported(imported, window, cx);
                            }
                            bunker::Update::Done(Err(e)) => {
                                this.signer.deadline = None;
                                this.signer.link = None;
                                this.signer.qr = None;
                                this.signer.status = Some((e.into(), Tone::Error));
                                this.net_settle(cx);
                                // Almost always the link timing out: a fresh one, if the form still shows.
                                let view = cx.entity();
                                cx.spawn_in(window, async move |_, cx| {
                                    cx.background_executor().timer(Duration::from_millis(1500)).await;
                                    view.update_in(cx, |this, window, cx| {
                                        if this.method == "signer" && this.signer.session == session {
                                            this.signer_session(window, cx);
                                        }
                                    })
                                    .ok();
                                })
                                .detach();
                            }
                        }
                        cx.notify();
                        true
                    })
                    .unwrap_or(false);
                if !go_on {
                    break;
                }
            }
        })
        .detach();
    }

    /// One repaint a second while the link counts down; at expiry the popup goes with it.
    fn tick(this: gpui::WeakEntity<Self>, session: u64, mut cx: gpui::AsyncWindowContext) {
        cx.clone()
            .spawn(async move |cx| {
                loop {
                    cx.background_executor().timer(Duration::from_secs(1)).await;
                    let live = this
                        .update(cx, |this, cx| {
                            if this.signer.session != session {
                                return false;
                            }
                            let Some(d) = this.signer.deadline else { return false };
                            if d <= Instant::now() {
                                this.signer.deadline = None;
                                this.signer.link = None;
                                this.signer.qr = None;
                                this.signer.qr_open = false;
                                this.signer.status = None;
                                cx.notify();
                                return false;
                            }
                            cx.notify();
                            true
                        })
                        .unwrap_or(false);
                    if !live {
                        break;
                    }
                }
            })
            .detach();
        let _ = &mut cx;
    }

    /// Fixtures: `VECTOR_LOGIN=signer`, `signer-qr` (a live link), `signer-error`, `signer-reauth`.
    pub fn fixture_signer(&mut self, mode: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.step = super::Step::Import;
        self.method = "signer";
        if mode == "-reauth" {
            self.reauth = Some(false);
            return self.fixture_signer("-qr", window, cx);
        }
        match mode {
            "-qr" => {
                let link = "nostrconnect://".to_string() + &"7e".repeat(32) + "?relay=wss%3A%2F%2Frelay.damus.io&secret=3f9a1c&name=Vector";
                self.signer.qr = crate::qr::image(&link, 556);
                self.signer.link = Some(link);
                self.signer.deadline = Some(Instant::now() + LINK_LIFE);
                self.signer.qr_open = true;
            }
            "-error" => self.signer.status = Some(("Must start with bunker://".into(), Tone::Error)),
            _ => window.focus(&self.signer.url.focus_handle(cx), cx),
        }
        cx.notify();
    }

    /// Leaving the signer form drops a pairing that never committed.
    pub(super) fn signer_leave(&mut self, cx: &mut Context<Self>) {
        self.signer.session += 1;
        self.signer.qr_open = false;
        self.signer.link = None;
        self.signer.qr = None;
        self.signer.deadline = None;
        self.signer.status = None;
        // A re-pair never staged anything, and the account it serves stays signed in.
        if !self.signer.busy && self.reauth.is_none() {
            backend::run(bunker::cancel());
        }
        cx.notify();
    }

    /// Re-pair this account's signer: from Settings, or at an unlock the signer refused.
    pub fn start_reauth(&mut self, at_unlock: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.popup = None;
        self.reauth_from = self.step;
        self.reauth = Some(at_unlock);
        self.step = super::Step::Import;
        self.method = "signer";
        self.signer.status = None;
        self.signer_open_qr(window, cx);
    }

    pub(super) fn end_reauth(&mut self, cx: &mut Context<Self>) {
        self.signer_leave(cx);
        match self.reauth.take() {
            Some(false) => cx.emit(super::ReauthClosed),
            _ => {
                self.step = self.reauth_from;
                self.method = "qr";
            }
        }
        cx.notify();
    }

    fn reauthorized(&mut self, at_unlock: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !at_unlock {
            self.reauth = None;
            self.method = "qr";
            cx.emit(super::ReauthClosed);
            return;
        }
        self.signer.busy = true;
        let rx = backend::run(async {
            let npub = account::resume_bunker_unlock().await?;
            account::connect().await;
            Ok::<_, String>(npub)
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = rx.await.unwrap_or_else(|_| Err("Backend stopped".into()));
            this.update_in(cx, |this, _, cx| {
                this.signer.busy = false;
                match result {
                    Ok(npub) => {
                        this.reauth = None;
                        cx.emit(super::LoggedIn { npub });
                    }
                    Err(e) => this.signer.status = Some((e.into(), Tone::Error)),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// An unlock the remote signer didn't answer: offer to re-pair it, keeping the account.
    pub fn offer_reauth(&mut self, error: &str, cx: &mut Context<Self>) -> bool {
        if !(error.contains("Remote signer unreachable") || error.to_lowercase().contains("bunker")) {
            return false;
        }
        let escaped = error.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
        let mut popup = crate::kit::Popup::notice("Signer unreachable");
        popup.text = Some(format!("Your remote signer didn't respond. If you've reset or revoked Vector's permissions in your signer app, re-pair below without losing your account data.<br><br>{escaped}").into());
        popup.confirm = "Re-authorize Signer".into();
        popup.cancel = Some("Cancel".into());
        self.popup = Some(popup);
        self.popup_reauth = true;
        cx.notify();
        true
    }

    pub(super) fn signer_block(&self, cx: &mut Context<Self>) -> gpui::Div {
        let qr_btn = div()
            .id("lg-signer-qr")
            .size(px(40.))
            .flex_none()
            .rounded(px(7.))
            .cursor_pointer()
            .on_click(cx.listener(|this, _, w, cx| this.signer_open_qr(w, cx)))
            .child(img("icons/login/qr.svg").size(px(40.)));
        let row = self
            .field(self.signer.url.clone(), false, |this, w, cx| this.signer_connect(w, cx), cx)
            .when(self.signer.busy, |d| d.opacity(0.5))
            .child(qr_btn);
        div()
            .flex()
            .flex_col()
            .items_center()
            .child(super::lg_title(
                if self.reauth.is_some() { "Reconnect Your Remote Signer" } else { "Login with Remote Signer" }.into(),
                true,
                0xffffff,
            ))
            .child(row)
            .when_some(self.signer.line().filter(|_| !self.signer.qr_open), |d, (text, tone)| {
                d.child(div().max_w(px(340.)).mt(px(12.)).text_center().text_size(px(12.)).line_height(px(24.)).text_color(c(tone_color(tone))).child(text))
            })
    }

    /// `.lg-qr-overlay`: the link as a QR, its countdown, Copy Link and Close.
    pub(super) fn signer_qr(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.signer.qr_open {
            return None;
        }
        let line = self.signer.line();
        let ready = self.signer.qr.is_some();
        let loading = match &line {
            Some((t, Tone::Error)) => t.clone(),
            _ => "Generating connection link…".into(),
        };
        let can_copy = self.signer.link.is_some() && !self.signer.busy;
        let btn = |id: &'static str, label: &'static str| {
            div()
                .id(id)
                .w(px(100.))
                .h(px(38.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(6.))
                .text_size(px(16.))
                .font_weight(FontWeight::MEDIUM)
                .cursor_pointer()
                .child(label)
        };
        let card = div()
            .id("lg-qr-card")
            .w(px(352.))
            .flex()
            .flex_col()
            .items_center()
            .pt(px(20.))
            .px(px(20.))
            .pb(px(22.))
            .border_1()
            .border_color(c(0x474747))
            .rounded(px(9.))
            .bg(ca(3, 3, 3, 0.95))
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .size(px(310.))
                    .p(px(16.))
                    .rounded(px(16.))
                    .bg(c(0xffffff))
                    .flex()
                    .items_center()
                    .justify_center()
                    .map(|d| match &self.signer.qr {
                        Some(image) => d.child(img(image.clone()).size(px(278.))),
                        None => d.child(div().px(px(24.)).text_center().text_size(px(13.)).text_color(c(0x5b6060)).child(loading)),
                    }),
            )
            .child(
                div()
                    .my(px(12.))
                    .min_h(px(24.))
                    .text_size(px(12.))
                    .line_height(px(24.))
                    .text_color(c(0xb2b2b2))
                    .child(if ready { line.map(|l| l.0).unwrap_or_default() } else { SharedString::default() }),
            )
            .child(
                div()
                    .flex()
                    .gap(px(10.))
                    .child(
                        btn("lg-qr-copy", if self.signer.copied { "Copied" } else { "Copy Link" })
                            .border_1()
                            .border_color(c(0x474747))
                            .text_color(c(0xffffff))
                            .when(!can_copy, |d| d.opacity(0.5))
                            .when(can_copy, |d| {
                                d.on_click(cx.listener(|this, _, _, cx| {
                                    if let Some(link) = this.signer.link.clone() {
                                        cx.write_to_clipboard(ClipboardItem::new_string(link));
                                        this.signer.copied = true;
                                        cx.notify();
                                        cx.spawn(async move |this, cx| {
                                            cx.background_executor().timer(Duration::from_millis(2500)).await;
                                            this.update(cx, |this, cx| {
                                                this.signer.copied = false;
                                                cx.notify();
                                            })
                                            .ok();
                                        })
                                        .detach();
                                    }
                                }))
                            }),
                    )
                    .child(
                        btn("lg-qr-close", "Close")
                            .bg(c(0x59fcb3))
                            .text_color(c(0x000000))
                            .on_click(cx.listener(|this, _, _, cx| this.signer_close_qr(cx))),
                    ),
            );
        Some(
            div()
                .id("lg-qr-overlay")
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .bg(ca(0, 0, 0, 0.6))
                .occlude()
                .on_mouse_down(gpui::MouseButton::Left, cx.listener(|this, _, _, cx| this.signer_close_qr(cx)))
                .child(card)
                .into_any_element(),
        )
    }
}
