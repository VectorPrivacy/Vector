//! The welcome screen's network choice (`loginNet` in js/auth.js): before sign-in it drives the
//! install-wide choice the next account inherits; while adding a profile it only records the
//! choice, which the commit applies once the live account is gone.

use std::rc::Rc;

use gpui::prelude::*;
use gpui::{AnimationExt as _, AnyElement, App, Context, Entity, Focusable, FontWeight, SharedString, Window, div, px, svg};
use serde_json::Value;

use super::LoginView;
use crate::backend::{self, transport as net};
use crate::composer::{CancelInput, Look, SendText, TextInput};
use crate::kit::{self, Choice};
use crate::theme::*;

const NETS: &[Choice] = &[
    Choice { value: "clearnet", label: "Standard" },
    Choice { value: "tor", label: "Tor" },
    Choice { value: "i2p", label: "I2P" },
];

fn label(kind: &str) -> &str {
    NETS.iter().find(|c| c.value == kind).map_or(kind, |c| c.label)
}

fn json<T: serde::Serialize>(v: T) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

pub(super) struct LoginNet {
    pub shown: bool,
    kinds: Vec<&'static str>,
    pub kind: &'static str,
    /// Connecting: its own line says so.
    pub busy: bool,
    /// The pick isn't connected: signing in would only fail later.
    pub hold: bool,
    failed: String,
    failed_code: String,
    pub info: Option<&'static str>,
    select_open: bool,
    sam_port: u16,
    sam_user: String,
    sam_password: String,
    sam_error: String,
    sam_open: bool,
    port: Option<Entity<TextInput>>,
    user: Option<Entity<TextInput>>,
    password: Option<Entity<TextInput>>,
    /// Only the latest pick may paint.
    seq: u64,
    /// A pick's command is running: its own outcome paints, not the views it passes through.
    inflight: bool,
    /// The next I2P apply drops the SAM credentials given before.
    clear_auth: bool,
    /// The fixtures' view, standing in for the backend.
    pub fixture: Option<Value>,
}

impl Default for LoginNet {
    fn default() -> Self {
        Self {
            shown: false,
            kinds: Vec::new(),
            kind: "",
            busy: false,
            hold: false,
            failed: String::new(),
            failed_code: String::new(),
            info: None,
            select_open: false,
            sam_port: 7656,
            sam_user: String::new(),
            sam_password: String::new(),
            sam_error: String::new(),
            sam_open: false,
            port: None,
            user: None,
            password: None,
            seq: 0,
            inflight: false,
            clear_auth: false,
            fixture: None,
        }
    }
}

fn intern(kind: &str) -> &'static str {
    NETS.iter().find(|c| c.value == kind).map_or("", |c| c.value)
}

fn there(v: &Value, kind: &str) -> bool {
    s(v, "kind") == kind && (kind == "clearnet" || v.get("ready").and_then(Value::as_bool) == Some(true))
}

impl LoginView {
    /// Browsing Add Profile: the choice is only recorded until the commit.
    fn net_recording(&self) -> bool {
        self.adding == Some(false)
    }

    fn net_state(&self) -> Option<Value> {
        match &self.net.fixture {
            Some(v) => Some(v.clone()),
            None => self.live.then(|| json(net::transport_get_state())),
        }
    }

    /// Start appeared: offer the networks this build has, and line the transport up with the pick.
    pub(super) fn net_refresh(&mut self, cx: &mut Context<Self>) {
        let Some(v) = self.net_state() else { return };
        let kinds: Vec<&'static str> = v.get("supported").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(intern).filter(|k| !k.is_empty()).collect()).unwrap_or_default();
        let prelogin = s(&v, "prelogin").to_string();
        // A remembered choice that is unreadable, or that this build lacks, picks nothing: never direct by default.
        let unusable = self.adding.is_none() && (prelogin == "unknown" || !kinds.contains(&prelogin.as_str()));
        if kinds.len() < 2 && !unusable {
            self.net.shown = false;
            self.net.hold = false;
            self.net.busy = false;
            self.net.failed.clear();
            cx.notify();
            return;
        }
        if self.live {
            if let Some(p) = json(net::i2p_get_config()).get("sam_port").and_then(Value::as_u64) {
                self.net.sam_port = p as u16;
            }
        }
        self.net.kinds = kinds.clone();
        self.net.shown = true;
        if let Some(committed) = self.adding {
            if self.net.kind.is_empty() {
                self.net.kind = if kinds.contains(&s(&v, "kind")) { intern(s(&v, "kind")) } else { "clearnet" };
            }
            if !committed {
                self.net.busy = false;
                self.net.hold = false;
                self.net.failed.clear();
                cx.notify();
                return;
            }
            let kind = self.net.kind;
            self.net_apply(kind, there(&v, kind), cx);
            return;
        }
        if unusable {
            self.net.kind = "";
            self.net.busy = false;
            self.net.hold = true;
            self.net.failed = if prelogin == "unknown" { "Choose how Vector connects.".into() } else { format!("This build doesn't include {}.", label(&prelogin)) };
            self.net.failed_code.clear();
            cx.notify();
            return;
        }
        let kind = intern(&prelogin);
        self.net.kind = kind;
        self.net_apply(kind, there(&v, kind), cx);
    }

    /// A pick. Leaving a network is always allowed, even mid-connect: a censored network must not trap the user.
    fn net_pick(&mut self, kind: &'static str, cx: &mut Context<Self>) {
        self.net.select_open = false;
        if self.net_recording() {
            self.net.kind = kind;
            self.net.failed.clear();
            cx.notify();
            return;
        }
        if kind == self.net.kind && self.net.failed.is_empty() {
            cx.notify();
            return;
        }
        self.net_apply(kind, false, cx);
    }

    /// The I2P options a pick carries: the port, and SAM credentials when entered (in memory only,
    /// saved with the account that signs in).
    fn net_opts(&mut self) -> Option<Value> {
        if self.net.kind != "i2p" {
            return None;
        }
        let clear = std::mem::take(&mut self.net.clear_auth);
        let mut o = serde_json::json!({ "sam_port": self.net.sam_port });
        if clear {
            o["sam_user"] = Value::Null;
            o["sam_password"] = Value::Null;
        } else if !self.net.sam_user.is_empty() && !self.net.sam_password.is_empty() {
            o["sam_user"] = self.net.sam_user.clone().into();
            o["sam_password"] = self.net.sam_password.clone().into();
        }
        Some(o)
    }

    /// `quiet`: nothing visible changes (already there), so skip the transition.
    fn net_apply(&mut self, kind: &'static str, quiet: bool, cx: &mut Context<Self>) {
        self.net.seq += 1;
        let seq = self.net.seq;
        self.net.kind = kind;
        self.net.failed.clear();
        if !quiet {
            self.net.busy = kind != "clearnet";
            self.net.hold = true;
        }
        cx.notify();
        if !self.live {
            self.net_paint(self.net.fixture.clone(), String::new(), cx);
            return;
        }
        self.net.inflight = true;
        let opts = self.net_opts();
        let remember = self.adding.is_none();
        let rx = backend::run(net::transport_set_prelogin(kind.into(), Some(remember), opts));
        cx.spawn(async move |this, cx| {
            let err = match rx.await {
                Ok(Ok(_)) => String::new(),
                Ok(Err(e)) => e,
                Err(_) => "Backend stopped".into(),
            };
            this.update(cx, |this, cx| {
                if seq != this.net.seq {
                    return;
                }
                this.net.inflight = false;
                let v = this.net_state();
                this.net_paint(v, err, cx);
            })
            .ok();
        })
        .detach();
    }

    /// Every transport view repaints the line while the login screen is up.
    pub fn on_transport_view(&mut self, view: Value, cx: &mut Context<Self>) {
        self.net_paint(Some(view), String::new(), cx);
    }

    /// The line under the pick, from a view: I2P keeps connecting after its command returns.
    fn net_paint(&mut self, v: Option<Value>, err: String, cx: &mut Context<Self>) {
        let n = &mut self.net;
        if !n.shown || n.kind.is_empty() || n.inflight || self.adding == Some(false) {
            return;
        }
        if n.kind == "clearnet" {
            n.busy = false;
            n.hold = false;
            n.failed = err;
            n.failed_code.clear();
            cx.notify();
            return;
        }
        let v = v.unwrap_or(Value::Null);
        let mine = s(&v, "kind") == n.kind;
        let up = mine && v.get("ready").and_then(Value::as_bool) == Some(true);
        let starting = mine && s(&v, "phase") == "starting" && err.is_empty();
        let reason = v.get("reason").map(|r| s(r, "text")).filter(|t| mine && !t.is_empty());
        let code = v.get("reason").map(|r| s(r, "code")).unwrap_or("");
        n.failed_code = if up || starting || !err.is_empty() || !mine { String::new() } else { code.to_string() };
        n.failed = if up || starting {
            String::new()
        } else if !err.is_empty() {
            err
        } else {
            reason.map_or_else(|| format!("{} couldn't connect.", label(n.kind)), str::to_string)
        };
        n.busy = starting;
        n.hold = !up;
        cx.notify();
    }

    /// I2P keeps trying on its own, so Retry only skips its wait; anything else starts over.
    fn net_retry(&mut self, cx: &mut Context<Self>) {
        let in_i2p = self.net_state().is_some_and(|v| s(&v, "kind") == "i2p");
        if self.live && self.net.kind == "i2p" && in_i2p && !self.net.inflight {
            self.net.busy = true;
            self.net.failed.clear();
            cx.notify();
            let rx = backend::run(net::transport_retry(None));
            cx.spawn(async move |this, cx| {
                let res = rx.await.unwrap_or_else(|_| Err("Backend stopped".into()));
                this.update(cx, |this, cx| match res {
                    Ok(v) => this.net_paint(Some(json(v)), String::new(), cx),
                    Err(e) => {
                        this.net.busy = false;
                        this.net.failed = e;
                        this.net.failed_code.clear();
                        cx.notify();
                    }
                })
                .ok();
            })
            .detach();
            return;
        }
        if !self.net.kind.is_empty() {
            let kind = self.net.kind;
            self.net_apply(kind, false, cx);
        }
    }

    /// Re-apply a running I2P pick after its options changed (Add Profile applies at its commit).
    fn net_reapply_i2p(&mut self, clear: bool, cx: &mut Context<Self>) {
        if self.net.kind != "i2p" || self.net_recording() {
            return;
        }
        if clear {
            self.net.clear_auth = true;
        }
        self.net_apply("i2p", false, cx);
    }

    fn set_sam_port(&mut self, text: &str, cx: &mut Context<Self>) {
        let Some(port) = text.parse::<u16>().ok().filter(|p| *p >= 1) else {
            self.net.sam_error = "Enter a port from 1 to 65535.".into();
            cx.notify();
            return;
        };
        self.net.sam_error.clear();
        if port == self.net.sam_port {
            cx.notify();
            return;
        }
        self.net.sam_port = port;
        self.net_reapply_i2p(false, cx);
    }

    /// SAM credentials for a router that asks for them: both, or neither to remove them.
    fn set_sam_auth(&mut self, user: &str, password: &str, cx: &mut Context<Self>) -> bool {
        let u = user.trim();
        if u.is_empty() != password.is_empty() {
            self.net.sam_error = "Enter both a username and a password.".into();
            cx.notify();
            return false;
        }
        if u.chars().chain(password.chars()).any(|ch| ch.is_whitespace() || ch == '"' || ch == '=') {
            self.net.sam_error = "Use letters, numbers and symbols, without spaces or quotes.".into();
            cx.notify();
            return false;
        }
        let had = !self.net.sam_user.is_empty() && !self.net.sam_password.is_empty();
        self.net.sam_user = u.to_string();
        self.net.sam_password = password.to_string();
        self.net.sam_error.clear();
        if !u.is_empty() || had {
            self.net_reapply_i2p(u.is_empty(), cx);
        }
        cx.notify();
        true
    }

    /// From the failed line: the I2P tab, with the port field (or the SAM password form) ready.
    fn net_change_port(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.net_info(Some("i2p"), window, cx);
        if self.net.failed_code.starts_with("sam_auth") {
            self.open_sam_form(window, cx);
        } else if let Some(port) = &self.net.port {
            window.focus(&port.focus_handle(cx), cx);
        }
    }

    /// Sign-in waits for the chosen network: the transport refuses everything until it connects.
    pub(super) fn net_ready(&self) -> Result<(), String> {
        let n = &self.net;
        if !n.shown || !n.hold || n.kind == "clearnet" {
            return Ok(());
        }
        if n.kind.is_empty() {
            return Err(if n.failed.is_empty() { "Choose how Vector connects.".into() } else { n.failed.clone() });
        }
        Err(format!("{} isn't connected yet. Try again, or pick another network.", label(n.kind)))
    }

    /// What Add Profile's commit starts the new account on.
    pub(super) fn net_choice(&mut self) -> (Option<String>, Option<Value>) {
        let kind = (self.net.shown && !self.net.kind.is_empty()).then(|| self.net.kind.to_string());
        let opts = self.net_opts();
        if kind.as_deref().is_some_and(|k| k != "clearnet") {
            self.net.busy = true;
            self.net.hold = true;
            self.net.failed.clear();
        }
        (kind, opts)
    }

    /// After the commit's network settled: show where it stands.
    pub(super) fn net_settle(&mut self, cx: &mut Context<Self>) {
        let v = self.net_state();
        self.net_paint(v, String::new(), cx);
    }

    /// The explainer: a tab opens it (or switches it), None closes it.
    fn net_info(&mut self, tab: Option<&'static str>, window: &mut Window, cx: &mut Context<Self>) {
        if tab.is_none() {
            if let Some(port) = &self.net.port {
                let text = port.read(cx).text().to_string();
                if text != self.net.sam_port.to_string() && !text.is_empty() {
                    self.set_sam_port(&text, cx);
                }
            }
        }
        self.net.info = tab;
        if tab == Some("i2p") && self.net.port.is_none() {
            let port = cx.new(|cx| {
                let mut i = TextInput::new("", cx).with_look(field_look(true, false)).centered().max_chars(5).keep_on_submit();
                i.set_text(&self.net.sam_port.to_string(), cx);
                i
            });
            cx.subscribe_in(&port, window, |this, input, _: &SendText, _, cx| {
                let text = input.read(cx).text().to_string();
                this.set_sam_port(&text, cx);
            })
            .detach();
            cx.observe(&port, |this, input, cx| {
                let text = input.read(cx).text().to_string();
                let digits: String = text.chars().filter(char::is_ascii_digit).collect();
                if digits != text {
                    input.update(cx, |i, cx| i.set_text(&digits, cx));
                    return;
                }
                if !this.net.sam_error.is_empty() {
                    this.net.sam_error.clear();
                    cx.notify();
                }
            })
            .detach();
            self.net.port = Some(port);
        } else if let (Some("i2p"), Some(port)) = (tab, &self.net.port) {
            let p = self.net.sam_port.to_string();
            port.update(cx, |i, cx| i.set_text(&p, cx));
        }
        cx.notify();
    }

    fn open_sam_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.net.sam_open = true;
        let make = |placeholder: &str, masked: bool, cx: &mut Context<Self>| {
            let placeholder = placeholder.to_string();
            cx.new(|cx| TextInput::new(placeholder, cx).with_look(field_look(false, masked)).keep_on_submit())
        };
        let user = make("", false, cx);
        let password = make("", true, cx);
        for input in [&user, &password] {
            cx.observe(input, |this, _, cx| {
                if !this.net.sam_error.is_empty() {
                    this.net.sam_error.clear();
                    cx.notify();
                }
            })
            .detach();
        }
        cx.subscribe_in(&password, window, |this, _, _: &SendText, _, cx| this.save_sam(cx)).detach();
        cx.subscribe_in(&password, window, |this, _, _: &CancelInput, _, cx| this.close_sam_form(cx)).detach();
        window.focus(&user.focus_handle(cx), cx);
        self.net.user = Some(user);
        self.net.password = Some(password);
        cx.notify();
    }

    fn close_sam_form(&mut self, cx: &mut Context<Self>) {
        self.net.sam_open = false;
        self.net.user = None;
        self.net.password = None;
        self.net.sam_error.clear();
        cx.notify();
    }

    fn save_sam(&mut self, cx: &mut Context<Self>) {
        let (Some(u), Some(p)) = (&self.net.user, &self.net.password) else { return };
        let (u, p) = (u.read(cx).text().to_string(), p.read(cx).text().to_string());
        if u.is_empty() || p.is_empty() {
            return;
        }
        if self.set_sam_auth(&u, &p, cx) {
            self.close_sam_form(cx);
        }
    }

    // ── Painting ────────────────────────────────────────────────────────────────────────

    fn tabs(&self) -> Vec<&'static str> {
        self.net.kinds.iter().copied().filter(|k| *k == "tor" || *k == "i2p").collect()
    }

    /// `.lg-net`: Network, its explainer, and the pick.
    pub(super) fn net_row(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.net.shown {
            return None;
        }
        let entity = cx.entity();
        let tabs = self.tabs();
        let first = if tabs.contains(&self.net.kind) { self.net.kind } else { tabs.first().copied().unwrap_or("tor") };
        let toggle = {
            let entity = entity.clone();
            move |_: &mut Window, cx: &mut App| {
                entity.update(cx, |this, cx| {
                    this.net.select_open = !this.net.select_open;
                    cx.notify();
                })
            }
        };
        let pick = {
            let entity = entity.clone();
            move |v: &'static str, _: &mut Window, cx: &mut App| entity.update(cx, |this, cx| this.net_pick(v, cx))
        };
        let choices: Vec<Choice> = self.net.kinds.iter().filter_map(|k| NETS.iter().find(|c| c.value == *k).map(|c| Choice { value: c.value, label: c.label })).collect();
        let select = div().opacity(if self.net.busy { 0.6 } else { 1. }).child(kit::select("lg-net-select", 130., &choices, self.net.kind, self.net.select_open, toggle, pick));
        Some(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap(px(12.))
                .w(px(250.))
                .mt(px(24.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .text_size(px(16.))
                        .line_height(px(19.))
                        .child("Network")
                        .when(!tabs.is_empty(), |d| {
                            d.child(
                                div()
                                    .id("lg-net-info")
                                    .cursor_pointer()
                                    .on_click(move |_, w, cx| entity.update(cx, |this, cx| this.net_info(Some(first), w, cx)))
                                    .child(svg().path("icons/info.svg").size(px(18.)).text_color(c(0x333333)).hover(|d| d.text_color(c(ICON_MUTED)))),
                            )
                        }),
                )
                .child(select)
                .into_any_element(),
        )
    }

    /// `.lg-net-slot.reserve`: Start keeps the line's room so the screen never shifts.
    pub(super) fn net_line(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.net.shown {
            return None;
        }
        let n = &self.net;
        let mut slot = div().min_h(px(21.)).flex().flex_col().items_center();
        if n.busy {
            slot = slot.child(
                div()
                    .mt(px(9.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap(px(4.))
                    .text_size(px(10.))
                    .line_height(px(12.))
                    .text_color(c(ICON_MUTED))
                    .child(
                        svg()
                            .path("icons/refresh.svg")
                            .size(px(10.))
                            .mr(px(2.))
                            .text_color(c(0x59fcb3))
                            .with_animation("lg-net-spin", gpui::Animation::new(std::time::Duration::from_secs(1)).repeat(), |s, t| {
                                s.with_transformation(gpui::Transformation::rotate(gpui::percentage(t)))
                            }),
                    )
                    .child(div().text_color(c(0x59fcb3)).child(format!("{} connecting…", label(n.kind))))
                    .child("Do not close app."),
            );
        } else if !n.failed.is_empty() {
            let danger = |a: f32| ca(0xff, 0x2e, 0xa9, a);
            let btn = |id: &'static str, text: &'static str| {
                div()
                    .id(id)
                    .px(px(10.))
                    .py(px(4.))
                    .rounded_full()
                    .bg(white_a(0.1))
                    .hover(|d| d.bg(white_a(0.18)))
                    .text_size(px(10.5))
                    .line_height(px(13.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(c(0xffffff))
                    .whitespace_nowrap()
                    .text_center()
                    .cursor_pointer()
                    .child(text)
            };
            let mut actions = div().flex_none().flex().flex_col().gap(px(4.));
            if !n.kind.is_empty() {
                actions = actions.child(btn("lg-net-retry", "Retry").on_click(cx.listener(|this, _, _, cx| this.net_retry(cx))));
            }
            if n.kind == "i2p" && (n.failed_code == "router_unreachable" || n.failed_code.starts_with("sam_")) {
                let text = if n.failed_code.starts_with("sam_auth") { "Add SAM Password" } else { "Change Port" };
                actions = actions.child(btn("lg-net-port", text).on_click(cx.listener(|this, _, w, cx| this.net_change_port(w, cx))));
            }
            slot = slot.child(
                div()
                    .mt(px(9.))
                    .w(px(250.))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .pt(px(9.))
                    .pb(px(9.))
                    .pl(px(11.))
                    .pr(px(10.))
                    .border_1()
                    .border_color(danger(0.3))
                    .rounded(px(10.))
                    .bg(danger(0.08))
                    .text_size(px(11.))
                    .line_height(px(14.))
                    .text_color(white_a(0.82))
                    .child(svg().path("icons/info.svg").flex_none().size(px(15.)).text_color(danger(1.)))
                    .child(div().flex_1().min_w_0().child(SharedString::from(n.failed.clone())))
                    .when(!n.kind.is_empty(), |d| d.child(actions)),
            );
        }
        Some(slot.into_any_element())
    }

    /// The I2P tab's router setup: the SAM port, and the credentials for a router that asks.
    fn sam_router(&self, cx: &mut Context<Self>) -> AnyElement {
        let n = &self.net;
        let field = |name: &'static str, input: Option<&Entity<TextInput>>, w: f32, min_label: Option<f32>| {
            div()
                .flex()
                .items_center()
                .gap(px(10.))
                .text_size(px(13.))
                .line_height(px(16.))
                .text_color(c(ICON_MUTED))
                .child(div().when_some(min_label, |d, m| d.min_w(px(m)).text_left()).child(name))
                .child(
                    div()
                        .w(px(w))
                        .h(px(30.))
                        .flex()
                        .items_center()
                        .when(min_label.is_some(), |d| d.px(px(8.)))
                        .border_1()
                        .border_color(c(0x333333))
                        .rounded(px(6.))
                        .bg(c(0x050505))
                        .children(input.cloned()),
                )
        };
        let link = |id: &'static str, text: &'static str| {
            div().id(id).text_color(c(0x5fd4ea)).cursor_pointer().hover(|d| d.underline()).child(text)
        };
        let has_auth = !n.sam_user.is_empty() && !n.sam_password.is_empty();
        let mut col = div().flex().flex_col().items_center().text_size(px(13.)).line_height(px(16.)).text_color(c(ICON_MUTED)).child(
            div()
                .mt(px(14.))
                .flex()
                .flex_wrap()
                .items_center()
                .justify_center()
                .gap_x(px(16.))
                .gap_y(px(6.))
                .child(field("SAM port", n.port.as_ref(), 76., None))
                .when(!n.sam_open && !has_auth, |d| {
                    d.child(link("lg-sam-add", "Add SAM Password").on_click(cx.listener(|this, _, w, cx| this.open_sam_form(w, cx))))
                }),
        );
        if has_auth && !n.sam_open {
            col = col.child(
                div()
                    .mt(px(10.))
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .child(SharedString::from(format!("Signs in as {}.", n.sam_user)))
                    .child(link("lg-sam-remove", "Remove").on_click(cx.listener(|this, _, _, cx| {
                        this.set_sam_auth("", "", cx);
                    }))),
            );
        } else if n.sam_open {
            let ready = n.user.as_ref().zip(n.password.as_ref()).is_some_and(|(u, p)| !u.read(cx).text().is_empty() && !p.read(cx).text().is_empty());
            col = col
                .child(div().mt(px(18.)).child(field("Username", n.user.as_ref(), 160., Some(72.))))
                .child(div().mt(px(18.)).child(field("Password", n.password.as_ref(), 160., Some(72.))))
                .child(
                    div()
                        .mt(px(10.))
                        .flex()
                        .items_center()
                        .gap(px(12.))
                        .child(link("lg-sam-cancel", "Cancel").on_click(cx.listener(|this, _, _, cx| this.close_sam_form(cx))))
                        .child(
                            link("lg-sam-use", "Use")
                                .when(!ready, |d| d.opacity(0.4).cursor_default())
                                .on_click(cx.listener(|this, _, _, cx| this.save_sam(cx))),
                        ),
                );
        }
        col.when(!n.sam_error.is_empty(), |d| d.child(div().mt(px(8.)).text_size(px(12.5)).text_color(c(0xff8c75)).child(SharedString::from(n.sam_error.clone()))))
            .into_any_element()
    }

    /// `.lg-net-modal`: the explainer, a tab per network, I2P's with its router setup.
    pub(super) fn net_modal(&self, narrow: bool, cx: &mut Context<Self>) -> Option<AnyElement> {
        let kind = self.net.info?;
        let (a, b) = (cx.entity(), cx.entity());
        let tabs = self.tabs();
        let extras = crate::netinfo::Extras {
            on_tab: Some(Rc::new(move |k, w, cx| b.update(cx, |this, cx| this.net_info(Some(k), w, cx)))),
            tabs,
            router: (kind == "i2p").then(|| self.sam_router(cx)),
            narrow,
        };
        Some(crate::netinfo::modal(
            kind,
            false,
            extras,
            move |w, cx| a.update(cx, |this, cx| this.net_info(None, w, cx)),
            |key, _, cx| {
                if let Some((_, url)) = crate::netinfo::LINKS.iter().find(|(k, _)| *k == key) {
                    cx.open_url(url);
                }
            },
        ))
    }
}

fn field_look(mono: bool, masked: bool) -> Look {
    Look {
        family: if mono { crate::markdown::MONO } else { RUBIK },
        weight: FontWeight::NORMAL,
        size: px(13.),
        line_height: px(18.),
        color: c(0xffffff),
        placeholder: white_a(0.25),
        masked,
        ..Default::default()
    }
}
