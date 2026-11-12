//! Settings > Privacy > Routing (`TransportCard`, `TorOptions`, `I2pOptions`, `I2pOutproxies`,
//! `TransportSteps`): a pill per network, the card of the one on view, then its own settings.
//! A pill only changes the view; the card's Use button switches.

use std::time::Instant;

use gpui::prelude::*;
use gpui::AnimationExt as _;
use gpui::{AnyElement, Entity, FontWeight, Hsla, SharedString, div, img, point, px, svg};
use serde_json::Value;

use crate::composer::TextInput;
use crate::markdown::MONO;
use crate::netglyph::{self, Net, State};
use crate::settings::{Act, OnAct, info, toggle, toggle_row};
use crate::theme::*;

pub enum NetAct {
    View(String),
    Use(String),
    Retry,
    Renew,
    Help(&'static str),
    Link(&'static str),
    TorEngine(bool),
    FindTor,
    SaveTorPort,
    TorBridges(bool),
    ApplyBridges,
    TorMulti(bool),
    NewCircuit,
    TestRouter,
    SaveRouter,
    AuthOpen,
    AuthCancel,
    AuthSave,
    AuthRemove,
    I2pOnly(bool),
    OpMove(usize, isize),
    OpToggle(usize, bool),
    OpRemove(usize),
    OpReset,
    OpAdd,
    OpCancel,
    OpSave,
    CopyAddress(String),
    OpenNetwork,
}

#[derive(Default, Clone, PartialEq)]
pub enum Load {
    #[default]
    Idle,
    Loading,
    Ok,
    Error(String),
}

#[derive(Default)]
pub struct Circuits {
    pub phase: Load,
    pub count: usize,
    pub hosts: usize,
    pub hops: Vec<Value>,
}

#[derive(Default)]
pub struct Engine {
    pub external: bool,
    pub port: Option<u16>,
    pub found: Option<u16>,
    pub busy: bool,
}

#[derive(Default)]
pub struct Bridges {
    pub enabled: bool,
    pub saved: String,
    pub busy: bool,
    /// A save's outcome: the text, and whether it failed.
    pub status: Option<(String, bool)>,
    /// obfs4 lines are typed and `obfs4proxy` isn't installed.
    pub obfs4_missing: bool,
    pub no_obfs4: bool,
}

pub struct Routing {
    pub view: Option<Value>,
    /// When the view arrived: `retry_in` counts from here.
    pub at: Instant,
    /// The pill on view; empty follows the network in use.
    pub viewed: String,
    /// The pill the thumb slides from.
    pub thumb_from: usize,
    /// A switch is under way.
    pub locked: bool,
    /// I2P-Only with none of the account's relays inside I2P.
    pub stranded: bool,
    pub config: Option<Value>,
    pub multi: bool,
    pub engine: Engine,
    pub bridges: Bridges,
    pub circuits: Circuits,
    pub port: Option<Entity<TextInput>>,
    pub port_dirty: bool,
    /// A router test's result or a refused save: the text, and whether it is good news.
    pub note: Option<(String, bool)>,
    pub testing: bool,
    pub saving: bool,
    /// The SAM password form, while open: username and password.
    pub auth: Option<(Entity<TextInput>, Entity<TextInput>)>,
    pub auth_error: String,
    pub auth_busy: bool,
    pub renewing: bool,
    pub exit_error: String,
    pub ops_busy: bool,
    pub ops_error: String,
    /// The Add Outproxy form, while open: name, address, port.
    pub adding: Option<[Entity<TextInput>; 3]>,
    pub tor_port: Option<Entity<TextInput>>,
    pub tor_port_dirty: bool,
    pub bridge_lines: Option<Entity<TextInput>>,
    pub bridges_dirty: bool,
}

impl Default for Routing {
    fn default() -> Self {
        Self {
            view: None,
            at: Instant::now(),
            viewed: String::new(),
            thumb_from: 0,
            locked: false,
            stranded: false,
            config: None,
            multi: true,
            engine: Engine::default(),
            bridges: Bridges::default(),
            circuits: Circuits::default(),
            port: None,
            port_dirty: false,
            note: None,
            testing: false,
            saving: false,
            auth: None,
            auth_error: String::new(),
            auth_busy: false,
            renewing: false,
            exit_error: String::new(),
            ops_busy: false,
            ops_error: String::new(),
            adding: None,
            tor_port: None,
            tor_port_dirty: false,
            bridge_lines: None,
            bridges_dirty: false,
        }
    }
}

pub const MAX_OUTPROXIES: usize = 8;

pub fn label(kind: &str) -> &str {
    match kind {
        "clearnet" => "Standard",
        "tor" => "Tor",
        "i2p" => "I2P",
        k => k,
    }
}

/// `(rgb, soft)`: each network's colour and its light tint.
fn tint(kind: &str) -> ((u8, u8, u8), Hsla) {
    match kind {
        "tor" => ((149, 48, 244), c(0xd9b3ff)),
        "i2p" => ((64, 200, 228), c(0xb3ecf7)),
        _ => ((255, 255, 255), white_a(0.88)),
    }
}

fn dot_color(kind: &str) -> Hsla {
    match kind {
        "tor" => c(0xc285ff),
        "i2p" => c(0x5fd4ea),
        _ => c(ACCENT),
    }
}

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

fn b(v: &Value, k: &str) -> bool {
    v.get(k).and_then(Value::as_bool).unwrap_or(false)
}

impl Routing {
    pub fn in_use(&self) -> &str {
        self.view.as_ref().map(|v| s(v, "kind")).filter(|k| !k.is_empty() && *k != "unknown").unwrap_or("")
    }

    pub fn viewed(&self) -> String {
        if !self.viewed.is_empty() {
            return self.viewed.clone();
        }
        match self.in_use() {
            "" => "clearnet".into(),
            k => k.into(),
        }
    }

    pub fn kinds(&self) -> Vec<String> {
        let Some(v) = &self.view else { return Vec::new() };
        let mut kinds: Vec<String> =
            v.get("supported").and_then(Value::as_array).into_iter().flatten().filter_map(|k| k.as_str().map(str::to_string)).collect();
        let used = self.in_use().to_string();
        if !used.is_empty() && !kinds.contains(&used) {
            kinds.push(used);
        }
        kinds
    }

    fn supported(&self, kind: &str) -> bool {
        self.view.as_ref().and_then(|v| v.get("supported")).and_then(Value::as_array).is_some_and(|a| a.iter().any(|k| k.as_str() == Some(kind)))
    }

    /// `transportStateClass`.
    pub fn state(&self, kind: &str) -> State {
        let Some(v) = &self.view else { return State::Disabled };
        if s(v, "kind") != kind || s(v, "phase") == "unsupported" {
            return State::Disabled;
        }
        if b(v, "ready") {
            State::Connected
        } else if s(v, "phase") == "starting" || reason_code(v) == "session_lost" {
            State::Starting
        } else {
            State::Failed
        }
    }

    /// Up, but no outproxy answers or I2P-Only reaches none of the relays: only half works.
    pub fn partial(&self) -> bool {
        let Some(v) = &self.view else { return false };
        b(v, "ready") && (steps(v).iter().any(|st| s(st, "id") == "exit" && s(st, "state") == "fail") || self.stranded)
    }

    fn tor_starting(&self) -> bool {
        self.in_use() == "tor" && self.view.as_ref().is_some_and(|v| v.get("detail").is_some_and(|d| s(d, "status") == "bootstrapping"))
    }

    fn call_active(&self) -> bool {
        self.view.as_ref().is_some_and(|v| b(v, "call_active"))
    }

    fn retry_in(&self) -> Option<u64> {
        let v = self.view.as_ref()?;
        let left = v.get("retry_in")?.as_f64()? - self.at.elapsed().as_secs_f64();
        Some(left.ceil().max(1.) as u64)
    }

    fn stuck(&self) -> bool {
        self.view.as_ref().is_some_and(start_stuck)
    }

    /// Something on screen moves on its own: the glyph, a pulsing dot, a countdown.
    pub fn animating(&self) -> bool {
        self.view.is_some()
    }
}

fn reason_code(v: &Value) -> &str {
    v.get("reason").map(|r| s(r, "code")).unwrap_or("")
}

fn reason_text(v: &Value) -> &str {
    v.get("reason").map(|r| s(r, "text")).unwrap_or("")
}

fn steps(v: &Value) -> Vec<Value> {
    v.get("steps").and_then(Value::as_array).cloned().unwrap_or_default()
}

/// `formatTransportStatus`: the card's status line for the network in use.
pub fn status_text(v: &Value) -> String {
    let kind = s(v, "kind");
    if kind == "unknown" {
        return "Choose how Vector connects.".into();
    }
    if s(v, "phase") == "unsupported" {
        return match reason_text(v) {
            "" => format!("This build doesn't include {}.", label(kind)),
            t => t.into(),
        };
    }
    if kind == "clearnet" {
        return "Connects straight to relays and servers.".into();
    }
    let ready = b(v, "ready");
    if kind == "tor" {
        if ready {
            return "Connected.".into();
        }
        let d = v.get("detail").cloned().unwrap_or(Value::Null);
        if s(&d, "status") == "bootstrapping" {
            return match d.get("bootstrap_progress").and_then(Value::as_u64) {
                Some(p) => format!("Bootstrapping {p}%…"),
                None => "Bootstrapping…".into(),
            };
        }
        return if s(v, "phase") == "failed" { "Failed to start." } else { "Starting…" }.into();
    }
    if ready {
        let exit_failing = steps(v).iter().any(|st| s(st, "id") == "exit" && s(st, "state") == "fail");
        return if exit_failing { "Connected inside I2P only." } else { "Connected." }.into();
    }
    if s(v, "phase") == "starting" {
        let router = steps(v).into_iter().find(|st| s(st, "id") == "router");
        return match router {
            Some(r) if kind == "i2p" => if s(&r, "state") != "ok" { "Looking for your router…" } else { "Building tunnels…" }.into(),
            _ => "Starting…".into(),
        };
    }
    match reason_text(v) {
        "" => "Starting…".into(),
        t => t.into(),
    }
}

fn act(on: &OnAct, a: impl Fn() -> NetAct + 'static) -> impl Fn(&gpui::ClickEvent, &mut gpui::Window, &mut gpui::App) + 'static {
    let on = on.clone();
    move |_, w, cx| on(Act::Net(a()), w, cx)
}

pub fn block(r: &Routing, on: &OnAct) -> AnyElement {
    if r.view.is_none() {
        return div().into_any_element();
    }
    let viewed = r.viewed();
    let mut col = div().flex().flex_col().child(picker(r, &viewed, on)).child(card(r, &viewed, on));
    match viewed.as_str() {
        "tor" => col = col.child(tor_options(r, on)),
        "i2p" => col = col.child(i2p_options(r, on)),
        _ => {}
    }
    col.into_any_element()
}

fn picker(r: &Routing, viewed: &str, on: &OnAct) -> AnyElement {
    let kinds = r.kinds();
    let n = kinds.len().max(1);
    let at = kinds.iter().position(|k| k == viewed).unwrap_or(0);
    let in_use = r.in_use().to_string();
    let dot_kind = match &r.view {
        Some(_) if r.partial() => "warn",
        Some(v) if b(v, "ready") => "ok",
        Some(v) if s(v, "phase") == "starting" || reason_code(v) == "session_lost" => "busy",
        _ => "bad",
    };
    let from = r.thumb_from.min(n - 1);
    let thumb = div()
        .absolute()
        .top(px(3.))
        .bottom(px(3.))
        .rounded(px(9.))
        .bg(white_a(0.08))
        .border_1()
        .border_color(white_a(0.07))
        .shadow(vec![gpui::BoxShadow {
            color: ca(0, 0, 0, 0.35),
            offset: point(px(0.), px(1.)),
            blur_radius: px(6.),
            spread_radius: px(0.),
            inset: false,
        }]);
    // Both edges are fractions of the inner width: the thumb measures nothing.
    let thumb = thumb
        .with_animation(
            SharedString::from(format!("net-thumb-{from}-{at}")),
            gpui::Animation::new(std::time::Duration::from_millis(if from == at { 1 } else { 280 }))
                .with_easing(crate::kit::bezier(0.3, 0.7, 0.2, 1.)),
            move |d, t| {
                let i = from as f32 + (at as f32 - from as f32) * t;
                d.left(gpui::relative(i / n as f32)).w(gpui::relative(1. / n as f32))
            },
        );
    let mut pills = div().relative().flex().size_full().child(thumb);
    for k in kinds {
        let active = k == viewed;
        let missing = !r.supported(&k);
        let mut pill = div()
            .id(SharedString::from(format!("net-pill-{k}")))
            .flex_1()
            .min_w_0()
            .h(px(34.))
            .flex()
            .items_center()
            .justify_center()
            .gap(px(7.))
            .px(px(8.))
            .rounded(px(9.))
            .text_size(px(14.))
            .font_weight(FontWeight::MEDIUM)
            .text_color(if active { c(0xffffff) } else { white_a(0.5) })
            .when(!active, |d| d.hover(|d| d.text_color(white_a(0.82))))
            .when(missing, |d| d.opacity(0.4))
            .cursor_pointer()
            .on_click(act(on, {
                let k = k.clone();
                move || NetAct::View(k.clone())
            }));
        if k == in_use {
            pill = pill.child(net_dot(&k, dot_kind));
        }
        pills = pills.child(pill.child(div().whitespace_nowrap().overflow_hidden().text_ellipsis().child(label(&k).to_string())));
    }
    div()
        .mb(px(12.))
        .p(px(3.))
        .h(px(42.))
        .rounded(px(12.))
        .bg(white_a(0.03))
        .border_1()
        .border_color(white_a(0.06))
        .child(pills)
        .into_any_element()
}

fn net_dot(kind: &str, state: &str) -> AnyElement {
    let color = match state {
        "bad" => c(0xff7860),
        "warn" => c(0xffc107),
        _ => dot_color(kind),
    };
    let glow = match state {
        "ok" => Some(Hsla { a: 1., ..color }),
        "bad" => Some(ca(255, 120, 96, 0.6)),
        "warn" => Some(ca(255, 193, 7, 0.5)),
        _ => None,
    };
    let d = div()
        .size(px(7.))
        .flex_shrink_0()
        .rounded_full()
        .bg(color)
        .when_some(glow, |d, g| {
            d.shadow(vec![gpui::BoxShadow { color: g, offset: point(px(0.), px(0.)), blur_radius: px(7.), spread_radius: px(0.), inset: false }])
        });
    if state == "busy" {
        return d
            .with_animation(
                "net-dot-pulse",
                gpui::Animation::new(std::time::Duration::from_millis(1200)).repeat(),
                |d, t| d.opacity(1. - 0.7 * (1. - (2. * t - 1.).abs())),
            )
            .into_any_element();
    }
    d.into_any_element()
}

fn card(r: &Routing, viewed: &str, on: &OnAct) -> AnyElement {
    let v = r.view.as_ref().expect("view");
    let in_use = r.in_use();
    let using = !in_use.is_empty() && viewed == in_use;
    let missing = !r.supported(viewed);
    let state = r.state(viewed);
    let ((nr, ng, nb), soft) = tint(viewed);
    let status = if using {
        status_text(v)
    } else if missing {
        format!("This build doesn't include {}.", label(viewed))
    } else if s(v, "kind") == "unknown" {
        "Choose how Vector connects.".into()
    } else {
        "Not in use.".into()
    };
    let tor_starting = r.tor_starting();
    let in_call = r.call_active();
    let held = r.locked || tor_starting;
    let hold_why = if in_call {
        Some("End the call to change networks.")
    } else if tor_starting {
        Some("Wait for Tor to finish connecting.")
    } else {
        None
    };
    let sub: Option<(String, Option<&'static str>)> = if !using && !missing && hold_why.is_some() {
        hold_why.map(|t| (t.to_string(), None))
    } else if !using || b(v, "ready") {
        None
    } else if s(v, "phase") == "waiting" {
        Some((r.retry_in().map(|n| format!("Retrying in {n}s.")).unwrap_or_default(), Some("Retry now")))
    } else if s(v, "phase") == "failed" {
        Some((reason_text(v).to_string(), Some("Retry")))
    } else if r.stuck() {
        Some((String::new(), Some("Retry")))
    } else {
        None
    };
    let progress = (using && viewed == "tor" && tor_starting)
        .then(|| v.get("detail").and_then(|d| d.get("bootstrap_progress")).and_then(Value::as_u64).map(|p| p as u8))
        .flatten();
    let net = match viewed {
        "tor" => Net::Tor,
        "i2p" => Net::I2p,
        _ => Net::Clearnet,
    };
    let (bg, border) = match (viewed, state) {
        (_, State::Disabled) => (white_a(0.03), white_a(0.06)),
        ("i2p", State::Starting) => (ca(64, 200, 228, 0.05), ca(64, 200, 228, 0.18)),
        ("i2p", State::Connected) => (ca(64, 200, 228, 0.08), ca(64, 200, 228, 0.28)),
        ("clearnet", State::Connected) => (white_a(0.045), white_a(0.12)),
        (_, State::Starting) => (ca(149, 48, 244, 0.06), ca(149, 48, 244, 0.18)),
        (_, State::Connected) => (ca(149, 48, 244, 0.10), ca(149, 48, 244, 0.30)),
        (_, State::Failed) => (ca(231, 76, 60, 0.06), ca(231, 76, 60, 0.25)),
    };
    let status_color = match state {
        State::Connected => soft,
        State::Failed => c(0xff8c75),
        _ => white_a(0.55),
    };
    let help: &'static str = match viewed {
        "tor" => "tor",
        "i2p" => "i2p",
        _ => "transport",
    };
    let title: AnyElement = if s(v, "kind") == "unknown" {
        div().child("Choose a Network").into_any_element()
    } else if viewed == "tor" {
        div()
            .flex()
            .items_start()
            .child(div().pt(px(4.)).child("Route traffic through Tor"))
            .child(div().pt(px(5.)).ml(px(1.)).text_size(px(8.25)).line_height(px(10.)).opacity(0.6).child("™"))
            .into_any_element()
    } else if viewed == "clearnet" {
        div().child("Direct Connection").into_any_element()
    } else {
        div().child(format!("Route traffic through {}", label(viewed))).into_any_element()
    };
    let mut body = div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .items_center()
        .child(
            div()
                .h(px(if viewed == "tor" { 28. } else { 24. }))
                .flex()
                .items_start()
                .gap(px(6.))
                .mb(px(4.))
                .text_size(px(15.))
                .line_height(px(24.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(white_a(0.92))
                .child(title)
                .child(div().h(px(if viewed == "tor" { 28. } else { 24. })).pt(px(if viewed == "tor" { 4. } else { 0. })).flex().items_center().child(info("net-card-info", act(on, move || NetAct::Help(help))))),
        )
        .child(div().min_h(px(16.)).text_size(px(12.)).line_height(px(16.8)).text_color(status_color).text_center().child(status));
    if let Some((text, action)) = sub {
        let locked = r.locked;
        body = body.child(
            div()
                .mt(px(4.))
                .flex()
                .flex_wrap()
                .justify_center()
                .gap_x(px(8.))
                .text_size(px(12.))
                .line_height(px(16.8))
                .text_color(white_a(0.45))
                .when(!text.is_empty(), |d| d.child(text))
                .when_some(action, |d, a| {
                    d.child(
                        div()
                            .id("net-retry")
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(c(ACCENT))
                            .when(locked, |d| d.opacity(0.45))
                            .when(!locked, |d| d.cursor_pointer().hover(|d| d.underline()).on_click(act(on, || NetAct::Retry)))
                            .child(a),
                    )
                }),
        );
    }
    if viewed == "tor" {
        body = body.child(
            div()
                .id("tor-attribution")
                .tooltip(crate::kit::tooltip("Visit torproject.org"))
                .mt(px(6.))
                .h(px(23.))
                .opacity(0.85)
                .hover(|d| d.opacity(1.))
                .cursor_pointer()
                .on_click(act(on, || NetAct::Link("torAttribution")))
                .child(img("icons/tor-logo.svg").h(px(16.)).w(px(26.5))),
        );
    }
    let use_btn = (!using && !missing).then(|| {
        let disabled = held || in_call;
        let k = viewed.to_string();
        div()
            .id("net-use")
            .flex_shrink_0()
            .h(px(32.))
            .px(px(16.))
            .flex()
            .items_center()
            .rounded_full()
            .border_1()
            .border_color(ca(nr, ng, nb, 0.42))
            .bg(ca(nr, ng, nb, 0.12))
            .text_color(soft)
            .text_size(px(13.))
            .font_weight(FontWeight::SEMIBOLD)
            .whitespace_nowrap()
            .when(disabled, |d| d.opacity(if r.locked { 0.7 } else { 0.4 }))
            .when(!disabled, |d| {
                d.cursor_pointer()
                    .hover(move |d| d.bg(ca(nr, ng, nb, 0.22)).border_color(ca(nr, ng, nb, 0.7)).text_color(c(0xffffff)))
                    .active(|d| d.opacity(0.85))
                    .on_click(act(on, move || NetAct::Use(k.clone())))
            })
            .child(format!("Use {}", label(viewed)))
    });
    div()
        .mb(px(18.))
        .flex()
        .items_center()
        .gap(px(16.))
        .py(px(14.))
        .px(px(16.))
        .rounded(px(14.))
        .bg(bg)
        .border_1()
        .border_color(border)
        .child(netglyph::glyph(net, state, progress, 72.))
        .child(body)
        .children(use_btn)
        .into_any_element()
}

fn line_label(text: &'static str, help: &'static str, on: &OnAct) -> impl IntoElement {
    div()
        .flex_1()
        .flex()
        .items_center()
        .gap(px(6.))
        .text_size(px(16.))
        .line_height(px(24.))
        .text_color(white_a(0.8))
        .child(text)
        .child(info(SharedString::from(format!("net-{help}-info")), act(on, move || NetAct::Help(help))))
}

/// `.st-line`: a label with its explainer, controls flush right.
fn st_line(text: &'static str, help: &'static str, right: impl IntoElement, on: &OnAct) -> AnyElement {
    div()
        .min_h(px(36.))
        .mb(px(10.))
        .flex()
        .items_center()
        .gap(px(12.))
        .child(line_label(text, help, on))
        .child(right)
        .into_any_element()
}

fn btn(id: &'static str, text: &'static str, enabled: bool, primary: bool, on: &OnAct, a: impl Fn() -> NetAct + 'static) -> AnyElement {
    div()
        .id(id)
        .h(px(32.))
        .px(px(14.))
        .flex()
        .items_center()
        .gap(px(7.))
        .rounded(px(6.))
        .border_1()
        .border_color(if primary { ca(64, 200, 228, 0.45) } else { c(0x333333) })
        .bg(if primary { ca(64, 200, 228, 0.14) } else { c(0x262626) })
        .text_size(px(12.))
        .text_color(if primary { c(0xc9f2fa) } else { c(0xffffff) })
        .when(!enabled, |d| d.opacity(0.5))
        .when(enabled, |d| {
            d.cursor_pointer()
                .hover(move |d| d.bg(if primary { ca(64, 200, 228, 0.24) } else { c(0x333333) }))
                .on_click(act(on, a))
        })
        .child(text)
        .into_any_element()
}

fn note(text: &str, tone: Option<bool>) -> AnyElement {
    div()
        .mt(px(-4.))
        .mb(px(10.))
        .text_size(px(12.5))
        .line_height(px(17.5))
        .text_color(match tone {
            Some(true) => c(0x7adef0),
            Some(false) => c(0xff8c75),
            None => white_a(0.55),
        })
        .child(text.to_string())
        .into_any_element()
}

fn link(id: &'static str, text: &'static str, color: Hsla, enabled: bool, on: &OnAct, a: impl Fn() -> NetAct + 'static) -> AnyElement {
    div()
        .id(id)
        .text_size(px(13.))
        .line_height(px(24.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(color)
        .when(!enabled, |d| d.opacity(0.45))
        .when(enabled, |d| d.cursor_pointer().hover(|d| d.underline()).on_click(act(on, a)))
        .child(text)
        .into_any_element()
}

fn switch(id: &'static str, on_now: bool, enabled: bool, on: &OnAct, a: impl Fn() -> NetAct + 'static) -> AnyElement {
    let t = toggle(id, on_now, false, act(on, a));
    if enabled { t } else { div().opacity(0.5).child(t).into_any_element() }
}

fn toggle_line(id: &'static str, text: &'static str, help: &'static str, value: bool, enabled: bool, on: &OnAct, a: impl Fn() -> NetAct + 'static) -> AnyElement {
    let on2 = on.clone();
    toggle_row(
        id,
        text,
        Some(Box::new(move |_, w, cx| on2(Act::Net(NetAct::Help(help)), w, cx))),
        switch(id, value, enabled, on, a),
        false,
    )
    .into_any_element()
}

fn field(input: &Entity<TextInput>, w: Option<f32>, mono: bool) -> AnyElement {
    div()
        .h(px(32.))
        .when_some(w, |d, w| d.w(px(w)))
        .when(w.is_none(), |d| d.flex_1())
        .px(px(6.))
        .flex()
        .items_center()
        .rounded(px(6.))
        .border_1()
        .border_color(c(0x333333))
        .bg(c(0x0a0a0a))
        .text_size(px(13.))
        .when(mono, |d| d.font_family(MONO))
        .child(input.clone())
        .into_any_element()
}

fn loopback(port: AnyElement) -> impl IntoElement {
    let host = |t: &'static str| div().font_family(MONO).text_size(px(13.)).line_height(px(24.)).text_color(white_a(0.4)).child(t);
    div().flex().items_center().gap(px(6.)).flex_shrink_0().child(host("127.0.0.1")).child(div().mx(px(-2.)).child(host(":"))).child(port)
}

fn tor_options(r: &Routing, on: &OnAct) -> AnyElement {
    let connected = r.view.as_ref().is_some_and(|v| s(v, "kind") == "tor" && b(v, "ready"));
    let e = &r.engine;
    let mut col = div().flex().flex_col();
    let ext = e.external;
    col = col.child(toggle_line("tor-external", "Use Running Tor", "torExternal", ext, !e.busy && !r.locked, on, move || NetAct::TorEngine(!ext)));
    if ext {
        let right = loopback(
            div()
                .flex()
                .gap(px(6.))
                .children(r.tor_port.as_ref().map(|i| field(i, Some(64.), true)))
                .child(if r.tor_port_dirty {
                    btn("tor-port-save", "Save", !e.busy, true, on, || NetAct::SaveTorPort)
                } else {
                    btn("tor-find", "Find", !e.busy, false, on, || NetAct::FindTor)
                })
                .into_any_element(),
        );
        col = col.child(st_line("Port", "torExternal", right, on));
        col = col.child(match e.found {
            Some(p) => note(&format!("Found {} on port {p}.", if p == 9150 { "Tor Browser" } else { "Tor" }), Some(true)),
            None => note("No Tor found. Open Tor Browser or start tor.", Some(false)),
        });
    } else {
        let br = &r.bridges;
        let enabled = br.enabled;
        col = col.child(toggle_line("tor-bridges", "Use Bridges", "torBridges", enabled, !br.busy, on, move || NetAct::TorBridges(!enabled)));
        if enabled || br.status.as_ref().is_some_and(|s| s.1) {
            col = col.child(bridges_body(r, connected, on));
        }
    }
    let multi = r.multi;
    col = col.child(toggle_line("tor-multi", "Multi-Circuit", "torMultiCircuit", multi, !r.locked, on, move || NetAct::TorMulti(!multi)));
    if connected {
        col = col.child(circuits(r, on));
    }
    col.into_any_element()
}

fn bridges_body(r: &Routing, connected: bool, on: &OnAct) -> AnyElement {
    let br = &r.bridges;
    let lines = r.bridge_lines.as_ref();
    let count = br.saved.lines().filter(|l| !l.trim().is_empty()).count();
    let (status, error) = br.status.clone().unwrap_or_else(|| {
        (if count == 0 { "No bridges configured.".into() } else { format!("{count} bridge{} configured", if count == 1 { "" } else { "s" }) }, false)
    });
    div()
        .flex()
        .flex_col()
        .gap(px(8.))
        .mb(px(10.))
        .children(lines.map(|i| {
            div()
                .min_h(px(96.))
                .p(px(10.))
                .rounded(px(8.))
                .border_1()
                .border_color(c(0x333333))
                .bg(c(0x0a0a0a))
                .font_family(MONO)
                .text_size(px(12.))
                .child(i.clone())
        }))
        .when(br.obfs4_missing, |d| {
            d.child(
                div()
                    .flex()
                    .gap(px(8.))
                    .p(px(10.))
                    .rounded(px(8.))
                    .bg(ca(255, 193, 7, 0.08))
                    .border_1()
                    .border_color(ca(255, 193, 7, 0.3))
                    .text_size(px(12.5))
                    .text_color(c(0xffd27a))
                    .child("⚠")
                    .child("obfs4 bridges need obfs4proxy installed. Saving will fail until it's available."),
            )
        })
        .child(
            div()
                .text_size(px(12.))
                .line_height(px(17.))
                .text_color(white_a(0.5))
                .child("One bridge per line. obfs4 recommended (bypasses DPI). Vanilla IP:port fingerprint also works for private bridges.")
                .child(
                    div()
                        .id("tor-bridges-link")
                        .text_color(c(ACCENT))
                        .cursor_pointer()
                        .on_click(act(on, || NetAct::Link("bridges")))
                        .child("Get bridges at bridges.torproject.org."),
                ),
        )
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(div().text_size(px(12.)).text_color(if error { c(0xff8c75) } else { white_a(0.55) }).child(status))
                .child(btn(
                    "tor-bridges-apply",
                    if connected { "Apply & Reconnect" } else { "Save" },
                    r.bridges_dirty && !br.busy,
                    false,
                    on,
                    || NetAct::ApplyBridges,
                )),
        )
        .into_any_element()
}

fn circuits(r: &Routing, on: &OnAct) -> AnyElement {
    let loading = r.circuits.phase == Load::Loading;
    let refresh = div()
        .id("tor-circuits-refresh")
        .tooltip(crate::kit::tooltip("Build a new circuit"))
        .flex()
        .items_center()
        .gap(px(6.))
        .h(px(22.))
        .pl(px(9.))
        .pr(px(11.))
        .rounded_full()
        .bg(ca(149, 48, 244, 0.12))
        .border_1()
        .border_color(ca(149, 48, 244, 0.30))
        .text_color(c(0xc9a4ff))
        .text_size(px(10.))
        .font_weight(FontWeight::BOLD)
        .when(loading, |d| d.opacity(0.55))
        .when(!loading, |d| {
            d.cursor_pointer()
                .hover(|d| d.bg(ca(149, 48, 244, 0.24)).border_color(ca(178, 102, 255, 0.65)).text_color(c(0xf1e1ff)))
                .on_click(act(on, || NetAct::NewCircuit))
        })
        .child(svg().path("ui/circuit-new.svg").size(px(10.)).text_color(c(0xc9a4ff)))
        .child(crate::kit::spaced("NEW".to_string(), 1.2));
    let head = div()
        .h(px(28.))
        .pb(px(6.))
        .flex()
        .items_center()
        .justify_between()
        .child(div().text_size(px(14.)).line_height(px(17.)).text_color(c(CHROME_HELP)).child(if r.multi { "Circuits" } else { "Active Circuit" }))
        .child(refresh);
    let body: AnyElement = if r.engine.external {
        div().text_size(px(12.)).line_height(px(15.)).text_color(c(CHROME_HELP)).child("That Tor builds the circuits. New gives every server a fresh one.").into_any_element()
    } else if r.multi && r.circuits.phase == Load::Ok {
        let n = r.circuits.count;
        let hosts = r.circuits.hosts;
        div()
            .pt(px(6.))
            .pb(px(2.))
            .flex()
            .items_center()
            .gap(px(12.))
            .child(div().min_w(px(38.1)).text_size(px(28.)).line_height(px(28.)).font_weight(FontWeight::BOLD).text_color(c(0xc9a4ff)).child(n.to_string()))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .text_size(px(14.))
                    .line_height(px(17.))
                    .text_color(white_a(0.8))
                    .child(format!("active circuit{}", if n == 1 { "" } else { "s" }))
                    .child(div().text_size(px(12.)).line_height(px(15.)).text_color(c(CHROME_HELP)).child(format!(
                        "across {hosts} {}. Each one's circuit is in its info.",
                        if hosts == 1 { "host" } else { "relays and servers" }
                    ))),
            )
            .into_any_element()
    } else {
        match &r.circuits.phase {
            Load::Loading | Load::Idle => empty("Building circuit…", false),
            Load::Error(e) => empty(&format!("Failed: {e}"), true),
            Load::Ok if r.circuits.hops.is_empty() => empty("No active circuit.", false),
            Load::Ok => hops(&r.circuits.hops),
        }
    };
    div().mt(px(4.)).flex().flex_col().child(head).child(body).into_any_element()
}

fn empty(text: &str, error: bool) -> AnyElement {
    div()
        .py(px(6.))
        .text_size(px(13.))
        .text_color(if error { c(0xff8c75) } else { white_a(0.5) })
        .child(text.to_string())
        .into_any_element()
}

/// `CircuitHops`: guard to exit, a dot per hop on one rail.
pub fn hops(hops: &[Value]) -> AnyElement {
    let n = hops.len();
    let mut list = div().relative().flex().flex_col().py(px(2.));
    if n > 1 {
        list = list.child(
            div()
                .absolute()
                .left(px(8.5))
                .top(px(15.))
                .bottom(px(15.))
                .w(px(1.))
                .bg(gpui::linear_gradient(180., gpui::linear_color_stop(ca(178, 102, 255, 0.55), 0.), gpui::linear_color_stop(ca(178, 102, 255, 0.92), 1.))),
        );
    }
    for h in hops {
        let pos = s(h, "position");
        let last = matches!(pos, "Exit" | "Onion");
        let mid = matches!(pos, "Middle" | "Rendezvous");
        let fp = s(h, "fingerprint");
        list = list.child(
            div()
                .h(px(26.))
                .flex()
                .items_center()
                .gap(px(12.))
                .child(
                    div().w(px(18.)).flex().justify_center().child(
                        div()
                            .size(px(10.))
                            .rounded_full()
                            .bg(if last { c(0xd9b3ff) } else if mid { ca(178, 102, 255, 0.7) } else { c(0x9530f4) })
                            .border_1()
                            .border_color(c(0xd9b3ff)),
                    ),
                )
                .child(
                    div()
                        .w(px(88.))
                        .text_size(px(9.5))
                        .font_weight(FontWeight::BOLD)
                        .text_color(if last { c(0xd9b3ff) } else { white_a(0.72) })
                        .child(crate::kit::spaced(format!("{}{}", pos.to_uppercase(), if b(h, "is_bridge") { " · BRIDGE" } else { "" }), 1.7)),
                )
                .child(div().flex_1().min_w_0().font_family(MONO).text_size(px(12.)).text_color(white_a(0.92)).child(match s(h, "address") {
                    "" => "—".to_string(),
                    a => a.to_string(),
                }))
                .when(!fp.is_empty(), |d| d.child(div().font_family(MONO).text_size(px(11.)).text_color(white_a(0.42)).child(format!("{}…", &fp[..fp.len().min(8)])))),
        );
    }
    list.into_any_element()
}

/// `TransportSteps`: a network's connection, step by step, as its status reports it.
fn steps_list(v: &Value, extra: Option<AnyElement>) -> Option<AnyElement> {
    let steps = steps(v);
    if steps.is_empty() {
        return None;
    }
    let ((nr, ng, nb), soft) = tint(s(v, "kind"));
    let mut list = div()
        .relative()
        .flex()
        .flex_col()
        .py(px(6.))
        .gap(px(4.))
        .child(
            div()
                .absolute()
                .left(px(8.5))
                .top(px(15.))
                .bottom(px(15.))
                .w(px(1.))
                .bg(gpui::linear_gradient(180., gpui::linear_color_stop(ca(nr, ng, nb, 0.45), 0.), gpui::linear_color_stop(ca(nr, ng, nb, 0.15), 1.))),
        );
    for st in &steps {
        let state = s(st, "state").to_string();
        let dot = div()
            .size(px(10.))
            .rounded_full()
            .border(px(1.5))
            .map(|d| match state.as_str() {
                "ok" => d.bg(ca(nr, ng, nb, 1.)).border_color(soft).shadow(vec![gpui::BoxShadow {
                    color: ca(nr, ng, nb, 0.55),
                    offset: point(px(0.), px(0.)),
                    blur_radius: px(6.),
                    spread_radius: px(0.),
                    inset: false,
                }]),
                "pending" => d.bg(c(0x0a0a0a)).border_color(ca(nr, ng, nb, 1.)),
                "fail" => d.bg(c(0xff7860)).border_color(c(0xff9a85)),
                _ => d.bg(c(0x0a0a0a)).border_color(white_a(0.28)),
            });
        let dot: AnyElement = if state == "pending" {
            dot.with_animation(
                SharedString::from(format!("net-step-{}", s(st, "id"))),
                gpui::Animation::new(std::time::Duration::from_millis(1300)).repeat(),
                move |d, t| {
                    let k = 1. - (2. * t - 1.).abs();
                    d.shadow(vec![gpui::BoxShadow {
                        color: ca(nr, ng, nb, 0.16 * k),
                        offset: point(px(0.), px(0.)),
                        blur_radius: px(0.),
                        spread_radius: px(4. * k),
                        inset: false,
                    }])
                },
            )
            .into_any_element()
        } else {
            dot.into_any_element()
        };
        list = list.child(
            div()
                .py(px(4.))
                .flex()
                .items_center()
                .gap(px(12.))
                .line_height(px(18.))
                .child(div().w(px(18.)).h(px(18.)).flex().items_center().justify_center().child(dot))
                .child(
                    div()
                        .w(px(84.))
                        .overflow_hidden()
                        .text_size(px(9.5))
                        .font_weight(FontWeight::BOLD)
                        .text_color(white_a(0.72))
                        .whitespace_nowrap()
                        .child(crate::kit::spaced(s(st, "label").to_uppercase(), 9.5 * 0.18)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .text_size(px(13.))
                        .text_color(match state.as_str() {
                            "fail" => c(0xff8c75),
                            "off" | "unknown" => white_a(0.5),
                            _ => white_a(0.92),
                        })
                        .child(s(st, "text").to_string()),
                ),
        );
    }
    Some(
        div()
            .mb(px(12.))
            .flex()
            .flex_col()
            .child(div().h(px(23.)).pb(px(6.)).text_size(px(14.)).line_height(px(17.)).text_color(c(CHROME_HELP)).child("Connection"))
            .child(list.children(extra))
            .into_any_element(),
    )
}

fn i2p_options(r: &Routing, on: &OnAct) -> AnyElement {
    let mut col = div().flex().flex_col();
    let Some(cfg) = r.config.clone() else {
        return col.child(foot()).into_any_element();
    };
    let v = r.view.clone().unwrap_or(Value::Null);
    let in_use = s(&v, "kind") == "i2p";
    let i2p_only = s(&cfg, "exit") == "off";
    let right = loopback(
        div()
            .flex()
            .gap(px(6.))
            .children(r.port.as_ref().map(|i| field(i, Some(64.), true)))
            .child(btn("i2p-test", "Test", !r.testing, false, on, || NetAct::TestRouter))
            .when(r.port_dirty, |d| d.child(btn("i2p-port-save", "Save", !r.saving, true, on, || NetAct::SaveRouter)))
            .into_any_element(),
    );
    col = col.child(st_line("Router", "i2pRouter", right, on));
    if let Some((text, ok)) = &r.note {
        col = col.child(note(text, Some(*ok)));
    }
    let cyan = c(0x7adef0);
    let auth: AnyElement = if b(&cfg, "sam_auth") {
        div()
            .flex()
            .items_center()
            .gap(px(10.))
            .child(div().text_size(px(13.)).line_height(px(24.)).text_color(white_a(0.55)).child(format!("Signs in as {}.", s(&cfg, "sam_user"))))
            .child(link("i2p-auth-remove", "Remove", cyan, !r.auth_busy, on, || NetAct::AuthRemove))
            .into_any_element()
    } else if let Some((user, pass)) = &r.auth {
        form(
            Some(("SAM Password", "i2pAuth")),
            vec![("Username", user, None, false), ("Password", pass, None, false)],
            !r.auth_busy,
            on,
            || NetAct::AuthCancel,
            || NetAct::AuthSave,
        )
    } else {
        div()
            .flex()
            .items_center()
            .gap(px(6.))
            .child(link("i2p-auth-add", "Add SAM Password", cyan, true, on, || NetAct::AuthOpen))
            .child(info("i2p-auth-info", act(on, || NetAct::Help("i2pAuth"))))
            .into_any_element()
    };
    col = col.child(div().mb(px(14.)).flex().flex_col().gap(px(4.)).child(auth).when(!r.auth_error.is_empty(), |d| {
        d.child(div().text_size(px(12.5)).text_color(c(0xff8c75)).child(r.auth_error.clone()))
    }));
    if in_use {
        let addresses: Vec<(String, &'static str)> = [("account", "your relays"), ("shared", "other servers")]
            .into_iter()
            .filter_map(|(k, who)| v.get("detail")?.get("addresses")?.get(k)?.as_str().map(|a| (a.to_string(), who)))
            .collect();
        let extra = (!addresses.is_empty()).then(|| address_row(&addresses, on));
        if let Some(list) = steps_list(&v, extra) {
            col = col.child(list);
        }
        let ready = b(&v, "ready");
        col = col.child(st_line("New Address", "i2pIdentity", btn("i2p-renew", "Renew", !r.renewing && ready && !r.locked, false, on, || NetAct::Renew), on));
    }
    col = col.child(toggle_line("i2p-only", "I2P-Only", "i2pOnly", i2p_only, !r.locked, on, move || NetAct::I2pOnly(!i2p_only)));
    if !r.exit_error.is_empty() {
        col = col.child(note(&r.exit_error, Some(false)));
    }
    if i2p_only && r.stranded && in_use {
        col = col.child(only_line(true, on));
    } else if i2p_only {
        col = col.child(only_line(false, on));
    } else {
        col = col.child(outproxies(r, &cfg, &v, on));
    }
    col.child(foot()).into_any_element()
}

fn only_line(warn: bool, on: &OnAct) -> AnyElement {
    let (edge, bg, ink) = if warn {
        (ca(255, 193, 7, 0.6), ca(255, 193, 7, 0.06), c(0xffd27a))
    } else {
        (ca(64, 200, 228, 0.55), ca(64, 200, 228, 0.06), c(0xb3ecf7))
    };
    div()
        .mt(px(-4.))
        .mb(px(12.))
        .py(px(9.))
        .px(px(12.))
        .border_l_2()
        .border_color(edge)
        .rounded_r(px(8.))
        .bg(bg)
        .text_size(px(13.))
        .line_height(px(18.2))
        .text_color(ink)
        .flex()
        .flex_wrap()
        .gap_x(px(4.))
        .when(warn, |d| {
            d.child("None of your relays are on I2P.").child(link("i2p-open-network", "Open Network", ink, true, on, || NetAct::OpenNetwork))
        })
        .when(!warn, |d| d.child("Only I2P servers and I2P addresses you added."))
        .into_any_element()
}

fn address_row(addresses: &[(String, &'static str)], on: &OnAct) -> AnyElement {
    let short = |b32: &str| format!("{}…{}.b32.i2p", &b32[..b32.len().min(4)], b32.get(48..52).unwrap_or(""));
    let mut list = div().flex().flex_col().gap(px(2.));
    for (b32, who) in addresses {
        let copy = b32.clone();
        list = list.child(
            div()
                .id(SharedString::from(format!("net-address-{who}")))
                .flex()
                .items_baseline()
                .gap(px(8.))
                .line_height(px(18.))
                .cursor_pointer()
                .on_click(act(on, move || NetAct::CopyAddress(copy.clone())))
                .child(div().font_family(MONO).text_size(px(12.)).text_color(white_a(0.92)).hover(|d| d.text_color(c(0xb3ecf7))).child(short(b32)))
                .child(div().text_size(px(11.5)).text_color(white_a(0.45)).child(*who)),
        );
    }
    div()
        .py(px(4.))
        .flex()
        .items_start()
        .gap(px(12.))
        .child(
            div().w(px(18.)).h(px(18.)).flex().items_center().justify_center().child(
                div().size(px(10.)).rounded_full().border(px(1.5)).bg(ca(64, 200, 228, 1.)).border_color(c(0xb3ecf7)),
            ),
        )
        .child(div().w(px(84.)).text_size(px(9.5)).line_height(px(18.)).font_weight(FontWeight::BOLD).text_color(white_a(0.72)).child(crate::kit::spaced("ADDRESS", 1.71)))
        .child(list)
        .into_any_element()
}

/// `.i2p-form`: labelled fields over Cancel and Save.
fn form(
    head: Option<(&'static str, &'static str)>,
    fields: Vec<(&'static str, &Entity<TextInput>, Option<f32>, bool)>,
    enabled: bool,
    on: &OnAct,
    cancel: impl Fn() -> NetAct + 'static,
    save: impl Fn() -> NetAct + 'static,
) -> AnyElement {
    let mut row = div().flex().flex_wrap().gap(px(10.));
    for (name, input, width, mono) in fields {
        row = row.child(
            div()
                .flex()
                .flex_col()
                .gap(px(4.))
                .min_w_0()
                .when_some(width, |d, w| d.w(px(w)).flex_shrink_0())
                .when(width.is_none(), |d| d.flex_1().min_w(px(140.)))
                .child(
                    div()
                        .text_size(px(10.5))
                        .font_weight(FontWeight::BOLD)
                        .text_color(white_a(0.45))
                        .child(crate::kit::spaced(name.to_uppercase(), 0.84)),
                )
                .child(
                    div()
                        .h(px(34.))
                        .px(px(10.))
                        .flex()
                        .items_center()
                        .rounded(px(6.))
                        .border_1()
                        .border_color(c(0x333333))
                        .bg(c(0x0a0a0a))
                        .text_size(px(if mono { 12. } else { 13. }))
                        .when(mono, |d| d.font_family(MONO))
                        .child(input.clone()),
                ),
        );
    }
    div()
        .flex()
        .flex_col()
        .gap(px(10.))
        .p(px(12.))
        .rounded(px(10.))
        .border_1()
        .border_color(white_a(0.07))
        .bg(white_a(0.025))
        .when_some(head, |d, (title, help)| {
            d.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .text_size(px(13.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(white_a(0.85))
                    .child(title)
                    .child(info(SharedString::from(format!("form-{help}-info")), act(on, move || NetAct::Help(help)))),
            )
        })
        .child(row)
        .child(
            div()
                .flex()
                .justify_end()
                .gap(px(8.))
                .child(btn("form-cancel", "Cancel", true, false, on, cancel))
                .child(btn("form-save", "Save", enabled, true, on, save)),
        )
        .into_any_element()
}

fn health_of(h: Option<&Value>) -> (&'static str, String, Option<String>) {
    let Some(h) = h else { return ("unknown", "Not tried".into(), None) };
    let why = h.get("last_error").and_then(Value::as_str).map(str::to_string);
    let ports: Vec<String> = h.get("refused_ports").and_then(Value::as_array).into_iter().flatten().map(|p| p.to_string()).collect();
    match s(h, "state") {
        "cooling" => {
            let secs = h.get("cooling_for").and_then(Value::as_f64).unwrap_or(60.);
            ("cooling", format!("Resting {}m", (secs / 60.).ceil().max(1.)), why)
        }
        "failing" => ("failing", "Failing".into(), why),
        "ok" if ports.is_empty() => ("ok", "OK".into(), None),
        "ok" => ("ok", format!("OK, not port {}", ports.join(", ")), None),
        _ if !ports.is_empty() => ("refusing", format!("Refuses port {}", ports.join(", ")), None),
        _ if why.is_some() => ("unknown", "Not reached yet".into(), why),
        _ => ("unknown", "Not tried".into(), None),
    }
}

fn outproxies(r: &Routing, cfg: &Value, v: &Value, on: &OnAct) -> AnyElement {
    let list = cfg.get("outproxies").and_then(Value::as_array).cloned().unwrap_or_default();
    let health = (s(v, "kind") == "i2p").then(|| v.get("detail").and_then(|d| d.get("outproxies")).cloned().unwrap_or(Value::Null));
    let stale = !b(v, "ready");
    let busy = r.ops_busy;
    let head = div()
        .min_h(px(36.))
        .mb(px(10.))
        .flex()
        .items_center()
        .justify_between()
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .text_size(px(16.))
                .line_height(px(24.))
                .text_color(c(0xffffff))
                .child("Outproxies")
                .child(info("i2p-ops-info", act(on, || NetAct::Help("i2pOutproxy")))),
        )
        .when(b(cfg, "customized"), |d| d.child(link("i2p-ops-reset", "Reset", c(0x7adef0), !busy, on, || NetAct::OpReset)));
    let n = list.len();
    let mut rows = div()
        .flex()
        .flex_col()
        .mb(px(10.))
        .py(px(4.))
        .rounded(px(12.))
        .border_1()
        .border_color(white_a(0.07))
        .overflow_hidden()
        .when(busy, |d| d.opacity(0.6));
    for (i, o) in list.iter().enumerate() {
        let enabled = b(o, "enabled");
        let dim = if enabled { 1. } else { 0.45 };
        let (cls, text, why) = health_of(health.as_ref().and_then(|h| h.get(s(o, "id"))));
        let (ink, dot) = match (cls, stale) {
            (_, true) => (white_a(0.4), white_a(0.3)),
            ("ok", _) => (c(0x7adef0), c(0x5fd4ea)),
            ("cooling" | "refusing", _) => (c(0xffc773), c(0xffb84d)),
            ("failing", _) => (c(0xff8c75), c(0xff7860)),
            _ => (white_a(0.5), white_a(0.28)),
        };
        let addr = s(o, "address");
        let short = match addr.strip_suffix(".b32.i2p") {
            Some(b32) if b32.len() >= 52 => format!("{}…{}", &b32[..4], &b32[b32.len() - 4..]),
            _ => addr.to_string(),
        };
        let mv = |dir: isize, path: &'static str, can: bool| {
            div()
                .id(SharedString::from(format!("op-move-{i}-{dir}")))
                .tooltip(crate::kit::tooltip(if dir < 0 { "Move up" } else { "Move down" }))
                .w(px(22.))
                .h(px(17.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(5.))
                .text_color(white_a(0.45))
                .when(!can || busy, |d| d.opacity(0.18))
                .when(can && !busy, |d| {
                    d.cursor_pointer().hover(|d| d.bg(white_a(0.08)).text_color(c(0xffffff))).on_click(act(on, move || NetAct::OpMove(i, dir)))
                })
                .child(svg().path(path).w(px(10.)).h(px(7.)).text_color(white_a(0.45)))
        };
        let row = div()
            .when(i > 0, |d| d.mt(px(3.)).border_t_1().border_color(white_a(0.06)))
            .mx(px(1.))
            .min_h(px(52.))
            .pl(px(6.))
            .pr(px(12.))
            .py(px(6.))
            .flex()
            .items_center()
            .gap(px(10.))
            .bg(white_a(0.02))
            .hover(|d| d.bg(white_a(0.04)))
            .child(div().flex().flex_col().gap(px(2.)).child(mv(-1, "ui/op-up.svg", i > 0)).child(mv(1, "ui/op-down.svg", i + 1 < n)))
            .child(
                div()
                    .size(px(20.))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .bg(ca(64, 200, 228, 0.12))
                    .text_color(c(0x7adef0))
                    .text_size(px(11.))
                    .font_weight(FontWeight::BOLD)
                    .opacity(dim)
                    .child((i + 1).to_string()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .opacity(dim)
                    .child(div().overflow_hidden().whitespace_nowrap().text_ellipsis().text_size(px(14.)).line_height(px(17.5)).font_weight(FontWeight::MEDIUM).text_color(c(0xffffff)).child(s(o, "name").to_string()))
                    .child(div().overflow_hidden().whitespace_nowrap().text_ellipsis().font_family(MONO).text_size(px(11.5)).line_height(px(14.4)).text_color(white_a(0.42)).child(format!("{short}:{}", o.get("port").and_then(Value::as_u64).unwrap_or(0)))),
            )
            .when(health.is_some(), |d| {
                d.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .flex_shrink_0()
                        .text_size(px(12.))
                        .text_color(ink)
                        .whitespace_nowrap()
                        .opacity(dim * if stale { 0.6 } else { 1. })
                        .child(div().size(px(7.)).rounded_full().bg(dot))
                        .child(text),
                )
            })
            .when(!b(o, "builtin"), |d| {
                d.child(
                    div()
                        .id(SharedString::from(format!("op-remove-{i}")))
                        .tooltip(crate::kit::tooltip("Remove"))
                        .size(px(24.))
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(5.))
                        .text_color(white_a(0.45))
                        .when(!busy, |d| d.cursor_pointer().hover(|d| d.bg(white_a(0.08)).text_color(c(0xff8c75))).on_click(act(on, move || NetAct::OpRemove(i))))
                        .child(svg().path("ui/op-remove.svg").size(px(10.)).text_color(white_a(0.45))),
                )
            })
            .child(switch("op-toggle", enabled, !busy, on, move || NetAct::OpToggle(i, !enabled)));
        let _ = why;
        rows = rows.child(row);
    }
    let mut col = div().mb(px(4.)).flex().flex_col().child(head).child(rows);
    if let Some([name, addr, port]) = &r.adding {
        col = col.child(div().mb(px(10.)).child(form(
            None,
            vec![("Name", name, None, false), ("Address", addr, None, true), ("Port", port, Some(76.), true)],
            !busy,
            on,
            || NetAct::OpCancel,
            || NetAct::OpSave,
        )));
    } else if n < MAX_OUTPROXIES {
        col = col.child(
            div().flex().child(
                div()
                    .id("i2p-op-add")
                    .h(px(32.))
                    .px(px(14.))
                    .flex()
                    .items_center()
                    .gap(px(7.))
                    .rounded(px(6.))
                    .border_1()
                    .border_color(c(0x333333))
                    .bg(c(0x262626))
                    .text_size(px(12.))
                    .text_color(c(0xffffff))
                    .when(!busy, |d| d.cursor_pointer().hover(|d| d.bg(c(0x333333))).on_click(act(on, || NetAct::OpAdd)))
                    .child(svg().path("ui/op-add.svg").size(px(11.)).text_color(c(0xffffff)))
                    .child("Add Outproxy"),
            ),
        );
    } else {
        col = col.child(note(&format!("You can add up to {MAX_OUTPROXIES} outproxies."), None));
    }
    if !r.ops_error.is_empty() {
        col = col.child(div().mt(px(4.)).child(note(&r.ops_error, Some(false))));
    }
    col.into_any_element()
}

fn foot() -> AnyElement {
    div()
        .mt(px(14.))
        .mb(px(4.))
        .text_size(px(12.))
        .line_height(px(16.8))
        .text_color(white_a(0.4))
        .child("Works with i2pd or Java I2P with SAM turned on.")
        .into_any_element()
}

/// `startStuck`: a start nothing drives, or one past its kind's budget.
pub fn start_stuck(v: &Value) -> bool {
    let kind = s(v, "kind");
    if s(v, "phase") != "starting" || kind == "unknown" || kind == "clearnet" {
        return false;
    }
    let driven = if kind == "tor" { v.get("detail").map(|d| s(d, "status")) == Some("bootstrapping") } else { !steps(v).is_empty() };
    let since = v.get("since").and_then(Value::as_u64).unwrap_or(0);
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    !driven || now.saturating_sub(since) > if kind == "tor" { 120 } else { 180 }
}

/// `NetworkNotice`: one line over the chat list while the network holds traffic back. The pane
/// is narrow, so the line is the reason's gist and the full reason its tooltip.
pub struct Notice {
    pub text: SharedString,
    pub title: SharedString,
    /// Amber for what mends itself or still half works; red when the user has to act.
    pub warn: bool,
}

pub fn notice(v: &Value, stranded: bool) -> Option<Notice> {
    let line = |v: &Value| {
        let r = v.get("reason")?;
        let full = s(r, "text");
        if full.is_empty() {
            return None;
        }
        let code = s(r, "code");
        let short = match code {
            "router_unreachable" => "I2P router not answering.",
            "sam_refused" => "That port isn't an I2P router.",
            "sam_too_old" => "I2P router too old for Vector.",
            "sam_auth_required" => "I2P router asks for a password.",
            "sam_auth_failed" => "I2P router refused the password.",
            "session_failed" => "I2P router couldn't open a session.",
            "session_lost" => "Reconnecting to I2P.",
            "tor_failed" => "Tor couldn't connect.",
            _ => full,
        };
        Some(Notice { text: short.to_string().into(), title: full.to_string().into(), warn: code == "session_lost" })
    };
    let plain = |text: &str, warn| Some(Notice { text: text.to_string().into(), title: text.to_string().into(), warn });
    match s(v, "kind") {
        "clearnet" | "" => None,
        "unknown" => line(v),
        _ if !b(v, "ready") && s(v, "phase") == "starting" => {
            if !start_stuck(v) {
                return None;
            }
            let label = s(v, "label");
            plain(&format!("{} isn't starting.", if label.is_empty() { "The network" } else { label }), true)
        }
        _ if !b(v, "ready") => line(v),
        _ if stranded => Some(Notice {
            text: "None of your relays are on I2P.".into(),
            title: "I2P-Only is on and none of your relays are on I2P.".into(),
            warn: false,
        }),
        _ if steps(v).iter().any(|st| s(st, "id") == "exit" && s(st, "state") == "fail") => plain("No outproxy is reachable right now.", true),
        _ => None,
    }
}

pub fn notice_line(n: Notice, open: impl Fn(&gpui::ClickEvent, &mut gpui::Window, &mut gpui::App) + 'static) -> AnyElement {
    let (rgb, text, dot): ((u8, u8, u8), u32, u32) = if n.warn { ((255, 193, 7), 0xffd98a, 0xffc107) } else { ((255, 120, 96), 0xffb3a3, 0xff7860) };
    let (bg, hover) = if n.warn { (0.06, 0.1) } else { (0.07, 0.12) };
    let tip = n.title;
    div()
        .id("net-notice")
        .mx(px(15.))
        .mt(px(22.))
        .mb(px(12.))
        .py(px(8.))
        .px(px(12.))
        .flex()
        .items_center()
        .gap(px(9.))
        .border_1()
        .border_color(ca(rgb.0, rgb.1, rgb.2, 0.28))
        .rounded(px(10.))
        .bg(ca(rgb.0, rgb.1, rgb.2, bg))
        .hover(move |d| d.bg(ca(rgb.0, rgb.1, rgb.2, hover)))
        .text_size(px(12.5))
        .line_height(px(15.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(c(text))
        .cursor_pointer()
        .tooltip(crate::kit::tooltip(tip))
        .on_click(open)
        .child(
            div()
                .flex_shrink_0()
                .size(px(7.))
                .rounded_full()
                .bg(c(dot))
                .shadow(vec![gpui::BoxShadow { color: ca(rgb.0, rgb.1, rgb.2, if n.warn { 0.5 } else { 0.6 }), offset: point(px(0.), px(0.)), blur_radius: px(6.), spread_radius: px(0.), inset: false }])
                .with_animation("net-notice-dot", gpui::Animation::new(std::time::Duration::from_millis(1600)).repeat(), |d, t| {
                    let k = if t < 0.5 { t * 2. } else { (1. - t) * 2. };
                    d.opacity(1. - 0.7 * k * k * (3. - 2. * k))
                }),
        )
        .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(n.text))
        .child(svg().path("ui/notice-go.svg").flex_shrink_0().w(px(6.)).h(px(10.)).text_color(c(text)).opacity(0.7))
        .into_any_element()
}
