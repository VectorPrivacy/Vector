//! `CallOverlay.svelte`: a ring card while a call comes in, otherwise a pill that floats above
//! every pane, drags anywhere, remembers where it was left, and opens into volume, the voice
//! meter, the processing switches and a plain-language view of the connection.

use std::cell::Cell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    AnimationExt, AnyElement, App, Bounds, BoxShadow, Hsla, InteractiveElement, IntoElement,
    ParentElement, Pixels, Point, SharedString, StatefulInteractiveElement, Styled, Window, canvas,
    div, point, prelude::FluentBuilder, px, svg,
};
use serde_json::Value;

use crate::data::Store;
use crate::theme::{ACCENT, c, ca, white_a};

const HISTORY: usize = 60;
/// The Vector theme's `--danger-color`.
pub(crate) const DANGER: u32 = 0xff2ea9;
const OPEN_W: f32 = 300.;
const PILL_MAX_W: f32 = 320.;
/// How long an ended call stays up to read why.
pub const ENDED_LINGER: Duration = Duration::from_millis(2500);

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Phase {
    Ringing,
    Connecting,
    Active,
    Ended,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Quality {
    Excellent,
    Good,
    Fair,
    Poor,
}

impl Quality {
    pub(crate) fn color(self) -> Hsla {
        match self {
            Quality::Excellent => c(0x59fcb3),
            Quality::Good => c(0xfce459),
            Quality::Fair => c(0xfca559),
            Quality::Poor => c(DANGER),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Quality::Excellent => "Excellent",
            Quality::Good => "Good",
            Quality::Fair => "Fair",
            Quality::Poor => "Poor",
        }
    }
}

pub(crate) fn grade(lost: f32, rtt: f32) -> Quality {
    if lost < 0.5 && rtt < 150. {
        Quality::Excellent
    } else if lost < 2. && rtt < 300. {
        Quality::Good
    } else if lost < 5. && rtt < 500. {
        Quality::Fair
    } else {
        Quality::Poor
    }
}

#[derive(Clone, Default)]
pub struct Stats {
    pub rtt_ms: u32,
    pub path: String,
    pub bitrate_kbps: u32,
    pub net_loss: f32,
    pub received: u64,
    pub concealed: u64,
}

#[derive(Clone, Copy)]
struct Sample {
    rtt: f32,
    lost: f32,
}

#[derive(Clone, Copy)]
pub(crate) struct Drag {
    pub(crate) start: Point<Pixels>,
    pub(crate) origin: Point<Pixels>,
    pub(crate) moved: bool,
}

/// The one call, mirrored from the backend's `call_state` events.
pub struct Call {
    pub id: Option<String>,
    pub peer: String,
    pub outgoing: bool,
    pub phase: Option<Phase>,
    pub reason: Option<String>,
    pub muted: bool,
    pub peer_muted: bool,
    pub mic_off: bool,
    pub volume: f32,
    active_ms: u64,
    received_at: Instant,
    pub video_offered: bool,
    peer_away: bool,
    stalled: bool,
    stats: Option<Stats>,
    history: VecDeque<Sample>,
    quality: Option<Quality>,
    levels: (f32, f32),
    pub expanded: bool,
    pub pos: Option<Point<Pixels>>,
    pub ended_at: Option<Instant>,
    /// The graph sample under the pointer.
    pub hover: Option<usize>,
    pill: Rc<Cell<Option<Bounds<Pixels>>>>,
    slider: Rc<Cell<Option<Bounds<Pixels>>>>,
    graph: Rc<Cell<Option<Bounds<Pixels>>>>,
    drag: Rc<Cell<Option<Drag>>>,
    sliding: Rc<Cell<bool>>,
}

impl Default for Call {
    fn default() -> Self {
        Self {
            id: None,
            peer: String::new(),
            outgoing: false,
            phase: None,
            reason: None,
            muted: false,
            peer_muted: false,
            mic_off: false,
            volume: 1.,
            active_ms: 0,
            received_at: Instant::now(),
            video_offered: false,
            peer_away: false,
            stalled: false,
            stats: None,
            history: VecDeque::new(),
            quality: None,
            levels: (0., 0.),
            expanded: false,
            pos: None,
            ended_at: None,
            hover: None,
            pill: Rc::default(),
            slider: Rc::default(),
            graph: Rc::default(),
            drag: Rc::default(),
            sliding: Rc::default(),
        }
    }
}

fn phase_of(s: &str) -> Option<Phase> {
    Some(match s {
        "ringing" => Phase::Ringing,
        "connecting" => Phase::Connecting,
        "active" => Phase::Active,
        "ended" => Phase::Ended,
        _ => return None,
    })
}

impl Call {
    /// `s` is the backend's CallState, or null when there is no call.
    pub fn apply_state(&mut self, s: &Value) {
        if s.is_null() {
            self.clear();
            return;
        }
        let str_of = |k: &str| {
            s.get(k)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        let bool_of = |k: &str| s.get(k).and_then(Value::as_bool).unwrap_or(false);
        let id = str_of("id");
        if self.id.as_deref() != Some(id.as_str()) {
            self.stats = None;
            self.history.clear();
            self.quality = None;
            self.levels = (0., 0.);
            self.hover = None;
        }
        self.id = Some(id);
        self.peer = str_of("peer");
        self.outgoing = bool_of("outgoing");
        self.phase = s.get("phase").and_then(Value::as_str).and_then(phase_of);
        self.reason = s.get("reason").and_then(Value::as_str).map(str::to_string);
        self.muted = bool_of("muted");
        self.peer_muted = bool_of("peer_muted");
        self.mic_off = bool_of("mic_off");
        self.volume = s.get("volume").and_then(Value::as_f64).unwrap_or(1.) as f32;
        self.active_ms = s.get("active_ms").and_then(Value::as_u64).unwrap_or(0);
        self.received_at = Instant::now();
        self.video_offered = bool_of("video_offered");
        self.peer_away = bool_of("peer_away");
        self.stalled = bool_of("stalled");
        self.ended_at = (self.phase == Some(Phase::Ended)).then(Instant::now);
        if self.phase != Some(Phase::Active) && self.phase != Some(Phase::Ended) {
            self.expanded = false;
        }
    }

    pub fn clear(&mut self) {
        self.id = None;
        self.phase = None;
        self.stats = None;
        self.history.clear();
        self.quality = None;
        self.expanded = false;
        self.ended_at = None;
    }

    /// An ended call is gone once it has been up long enough to read.
    pub fn expire(&mut self) -> bool {
        if self.ended_at.is_some_and(|t| t.elapsed() >= ENDED_LINGER) {
            self.clear();
            return true;
        }
        false
    }

    pub fn apply_level(&mut self, l: &Value) {
        if l.get("id").and_then(Value::as_str) != self.id.as_deref() {
            return;
        }
        let f = |k: &str| l.get(k).and_then(Value::as_f64).unwrap_or(0.) as f32;
        self.levels = (f("mic"), f("peer"));
    }

    /// One stats line a second: lost audio for this second is what the speaker filled in, over what arrived.
    pub fn apply_stats(&mut self, st: &Value) {
        if st.get("id").and_then(Value::as_str) != self.id.as_deref() {
            return;
        }
        let u = |k: &str| st.get(k).and_then(Value::as_u64).unwrap_or(0);
        let next = Stats {
            rtt_ms: u("rtt_ms") as u32,
            path: st
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            bitrate_kbps: u("bitrate_kbps") as u32,
            net_loss: st.get("net_loss").and_then(Value::as_f64).unwrap_or(0.) as f32,
            received: u("received"),
            concealed: u("concealed"),
        };
        let mut lost = 0.;
        if let Some(prev) = &self.stats
            && next.received > prev.received
        {
            lost = 100. * next.concealed.saturating_sub(prev.concealed) as f32
                / (next.received - prev.received) as f32;
        }
        self.history.push_back(Sample {
            rtt: next.rtt_ms as f32,
            lost: lost.max(0.),
        });
        if self.history.len() > HISTORY {
            self.history.pop_front();
        }
        self.stats = Some(next);
        let recent: Vec<_> = self.history.iter().rev().take(10).collect();
        let n = recent.len() as f32;
        self.quality = (n > 0.).then(|| {
            grade(
                recent.iter().map(|r| r.lost).sum::<f32>() / n,
                recent.iter().map(|r| r.rtt).sum::<f32>() / n,
            )
        });
    }

    pub fn live(&self) -> bool {
        self.phase == Some(Phase::Active)
    }

    /// Ringing for us, which takes the ring card rather than the pill.
    pub fn incoming(&self) -> bool {
        self.id.is_some() && self.phase == Some(Phase::Ringing) && !self.outgoing
    }

    fn status(&self) -> String {
        match self.phase {
            Some(Phase::Ringing) => if self.outgoing {
                "Calling…"
            } else {
                "Incoming call"
            }
            .into(),
            Some(Phase::Connecting) => "Connecting…".into(),
            Some(Phase::Active) if self.peer_away => "Waiting for them to come back…".into(),
            Some(Phase::Active) if self.stalled => "Reconnecting…".into(),
            Some(Phase::Active) => {
                clock(self.active_ms + self.received_at.elapsed().as_millis() as u64)
            }
            Some(Phase::Ended) => reason_text(self.reason.as_deref()).into(),
            None => String::new(),
        }
    }

    fn lost_pct(&self) -> f32 {
        let recent: Vec<_> = self.history.iter().rev().take(10).collect();
        if recent.is_empty() {
            return 0.;
        }
        recent.iter().map(|r| r.lost).sum::<f32>() / recent.len() as f32
    }
}

fn reason_text(reason: Option<&str>) -> &'static str {
    match reason {
        Some("rejected") => "Declined",
        Some("busy") => "Busy",
        Some("no_answer") => "No answer",
        Some("missed") => "Missed call",
        Some("connect_failed") => "Could not connect",
        Some("disconnected") => "Connection lost",
        Some("audio_failed") => "Microphone unavailable",
        Some("send_failed") => "Could not reach the relays",
        Some("answered_elsewhere") => "Answered on another device",
        _ => "Call ended",
    }
}

pub fn clock(ms: u64) -> String {
    let s = ms / 1000;
    let m = s / 60;
    let h = m / 60;
    if h > 0 {
        format!("{h}:{:02}:{:02}", m % 60, s % 60)
    } else {
        format!("{:02}:{:02}", m % 60, s % 60)
    }
}

fn lost_word(v: f32) -> String {
    if v < 0.05 {
        "nothing lost".into()
    } else if v < 1. {
        "under 1% lost".into()
    } else {
        format!("{v:.prec$}% lost", prec = if v < 10. { 1 } else { 0 })
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Switch {
    AutoGain,
    EchoCancel,
    NoiseSuppress,
}

#[derive(Clone, Debug)]
pub enum Act {
    Accept,
    Reject,
    Hangup,
    Mic,
    Toggle,
    Move(Point<Pixels>),
    Moved,
    Volume(f32),
    Audio(Switch, bool),
    Hover(Option<usize>),
}

pub type OnAct = Rc<dyn Fn(Act, &mut Window, &mut App)>;

/// The processing switches as the panel shows them.
pub struct Processing {
    pub auto_gain: bool,
    pub echo_cancel: bool,
    pub noise_suppress: bool,
}

pub(crate) fn accent_mix(pct: f32) -> Hsla {
    let mut a = c(ACCENT);
    a.a = pct.clamp(0., 1.);
    a
}

/// The phone glyph; hung up, it turns about its own box.
pub(crate) fn phone(size: f32, color: Hsla, down: bool) -> impl IntoElement {
    let s = svg().path("ui/call.svg").size(px(size)).text_color(color);
    if down {
        s.with_transformation(gpui::Transformation::rotate(gpui::radians(
            135f32.to_radians(),
        )))
    } else {
        s
    }
}

pub(crate) fn round_btn(id: &'static str, size: f32) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .size(px(size))
        .flex_shrink_0()
        .rounded_full()
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
}

/// `.call-btn`: 34px, a faint wash; on, the accent's.
pub(crate) fn call_btn(id: &'static str, on_state: bool) -> gpui::Stateful<gpui::Div> {
    round_btn(id, 34.)
        .bg(if on_state {
            accent_mix(0.22)
        } else {
            white_a(0.08)
        })
        .text_color(if on_state { c(ACCENT) } else { white_a(0.88) })
        .hover(move |d| {
            d.bg(if on_state {
                accent_mix(0.30)
            } else {
                white_a(0.14)
            })
            .text_color(if on_state { c(ACCENT) } else { c(0xffffff) })
        })
}

fn mic_btn(call: &Call, on: &OnAct) -> impl IntoElement {
    let off = call.muted || call.mic_off;
    let o = on.clone();
    call_btn("call-mic", off)
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(move |_, w, cx| o(Act::Mic, w, cx))
        .child(
            svg()
                .path(if off {
                    "icons/mic-off.svg"
                } else {
                    "icons/mic-on.svg"
                })
                .size(px(18.7))
                .text_color(if off { c(ACCENT) } else { white_a(0.88) }),
        )
}

fn hangup_btn(on: &OnAct) -> impl IntoElement {
    let o = on.clone();
    round_btn("call-hangup", 34.)
        .bg(c(DANGER))
        .hover(|d| d.bg(c(0xff4db6)))
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(move |_, w, cx| o(Act::Hangup, w, cx))
        .child(phone(20., c(0xffffff), true))
}

/// The ring card over everything while a call comes in.
fn ring(call: &Call, store: &Store, on: &OnAct) -> AnyElement {
    let (decline, accept) = (on.clone(), on.clone());
    let pulse = round_btn("call-accept", 60.)
        .bg(c(ACCENT))
        .on_click(move |_, w, cx| accept(Act::Accept, w, cx))
        .child(phone(28., c(0x111111), false))
        .with_animation(
            "call-ring-pulse",
            gpui::Animation::new(Duration::from_millis(1200)).repeat(),
            |d, t| {
                let (spread, alpha) = if t < 0.5 {
                    let k = ease_in_out(t * 2.);
                    (12. * k, 0.45 * (1. - k))
                } else {
                    let k = ease_in_out((t - 0.5) * 2.);
                    (12. * (1. - k), 0.45 * k)
                };
                d.shadow(vec![BoxShadow {
                    color: accent_mix(alpha),
                    offset: point(px(0.), px(0.)),
                    blur_radius: px(0.),
                    spread_radius: px(spread),
                    inset: false,
                }])
            },
        );
    div()
        .id("call-ring-overlay")
        .absolute()
        .inset_0()
        .occlude()
        .bg(ca(0, 0, 0, 0.7))
        .flex()
        .items_center()
        .justify_center()
        .child(
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(8.))
                .min_w(px(306.))
                .pt(px(28.))
                .px(px(32.))
                .pb(px(24.))
                .rounded(px(20.))
                .bg(c(0x1b1b1b))
                .border_1()
                .border_color(white_a(0.08))
                .child(crate::ui::avatar(store, &call.peer, 84.))
                .child(
                    div()
                        .mt(px(8.))
                        .max_w(px(400.))
                        .truncate()
                        .text_size(px(18.))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(c(0xffffff))
                        .child(store.name(&call.peer)),
                )
                .child(div().text_size(px(13.)).text_color(c(0xb2b2b2)).child(
                    if call.video_offered {
                        "Incoming video call"
                    } else {
                        "Incoming call"
                    },
                ))
                .child(
                    div()
                        .flex()
                        .gap(px(40.))
                        .mt(px(18.))
                        .child(
                            round_btn("call-decline", 60.)
                                .bg(c(DANGER))
                                .on_click(move |_, w, cx| decline(Act::Reject, w, cx))
                                .child(phone(28., c(0xffffff), true)),
                        )
                        .child(pulse),
                ),
        )
        .into_any_element()
}

fn ease_in_out(t: f32) -> f32 {
    let t = t.clamp(0., 1.);
    t * t * (3. - 2. * t)
}

fn section(title: &'static str) -> impl IntoElement {
    div()
        .mt(px(6.))
        .pt(px(10.))
        .border_t_1()
        .border_color(c(0x262626))
        .text_size(px(11.))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(c(ACCENT))
        .child(title.to_uppercase())
}

fn label(text: &'static str) -> impl IntoElement {
    div()
        .w(px(84.))
        .flex_shrink_0()
        .text_size(px(12.5))
        .text_color(white_a(0.8))
        .child(text)
}

fn value(text: impl Into<SharedString>) -> gpui::Div {
    let text = text.into();
    div()
        .flex()
        .items_center()
        .gap(px(5.))
        .text_size(px(13.))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(c(0xffffff))
        .when(!text.is_empty(), |d| d.child(text))
}

fn hint(text: impl Into<SharedString>) -> impl IntoElement {
    div()
        .text_size(px(11.))
        .font_weight(gpui::FontWeight::NORMAL)
        .text_color(c(0x8a8a8a))
        .child(text.into())
}

pub(crate) fn dot(q: Quality) -> impl IntoElement {
    div()
        .size(px(7.))
        .flex_shrink_0()
        .rounded_full()
        .bg(q.color())
}

/// The neon toggle at the panel's 0.8 scale.
fn switch(
    id: &'static str,
    label_text: &'static str,
    on_state: bool,
    which: Switch,
    on: &OnAct,
) -> impl IntoElement {
    let o = on.clone();
    div()
        .flex()
        .items_center()
        .justify_between()
        .py(px(2.))
        .text_size(px(12.5))
        .text_color(c(0xf6f6f6))
        .child(label_text)
        .child(
            div()
                .id(id)
                .relative()
                .w(px(40.))
                .h(px(19.2))
                .rounded(px(9.6))
                .cursor_pointer()
                .bg(if on_state {
                    gpui::linear_gradient(
                        90.,
                        gpui::linear_color_stop(c(0x33db98), 0.),
                        gpui::linear_color_stop(c(0x00ff9d), 1.),
                    )
                } else {
                    c(0x1a1a1a).into()
                })
                .on_click(move |_, w, cx| o(Act::Audio(which, !on_state), w, cx))
                .child(
                    div()
                        .absolute()
                        .top(px(1.6))
                        .left(px(if on_state { 22.4 } else { 1.6 }))
                        .size(px(16.))
                        .rounded_full()
                        .bg(if on_state { c(0xffffff) } else { c(0x333333) }),
                ),
        )
}

/// `VoiceMeter`: the gradient spans the track, the level uncovers as much as it reaches.
fn meter(level: f32, active: bool) -> impl IntoElement {
    let level = level.clamp(0., 1.);
    div()
        .flex_1()
        .h(px(8.))
        .rounded(px(4.))
        .bg(c(0x1a1a1a))
        .overflow_hidden()
        .child(
            div()
                .h_full()
                .w(gpui::relative(level))
                .overflow_hidden()
                .rounded(px(4.))
                .child(
                    div()
                        .h_full()
                        .w(gpui::relative(if level > 0. { 1. / level } else { 0. }))
                        .flex()
                        .when(!active, |d| d.opacity(0.35))
                        .child(
                            div()
                                .h_full()
                                .w(gpui::relative(0.8))
                                .bg(gpui::linear_gradient(
                                    90.,
                                    gpui::linear_color_stop(c(0x6ed99e), 0.),
                                    gpui::linear_color_stop(c(0xf7e470), 1.),
                                )),
                        )
                        .child(div().h_full().flex_1().bg(gpui::linear_gradient(
                            90.,
                            gpui::linear_color_stop(c(0xf7e470), 0.),
                            gpui::linear_color_stop(c(0xea6662), 1.),
                        ))),
                ),
        )
}

/// Their volume, 0 to 200%: an 8px track, the accent to the value, a 14px thumb.
fn volume_slider(call: &Call, on: &OnAct) -> impl IntoElement {
    let pct = (call.volume / 2.).clamp(0., 1.);
    let at = |b: Bounds<Pixels>, x: Pixels| {
        let v = ((x - b.origin.x) / b.size.width.max(px(1.))).clamp(0., 1.) * 200.;
        (v / 5.).round() * 5. / 100.
    };
    let bounds = call.slider.clone();
    let dragging = call.sliding.clone();
    let down = {
        let (on, bounds, dragging) = (on.clone(), bounds.clone(), dragging.clone());
        move |e: &gpui::MouseDownEvent, w: &mut Window, cx: &mut App| {
            cx.stop_propagation();
            let Some(b) = bounds.get() else { return };
            dragging.set(true);
            on(Act::Volume(at(b, e.position.x)), w, cx);
        }
    };
    let track = {
        let (on, bounds, dragging) = (on.clone(), bounds.clone(), dragging.clone());
        let record = bounds.clone();
        canvas(
            move |b, _, _| record.set(Some(b)),
            move |_, _, window, _| {
                if !dragging.get() {
                    return;
                }
                let (on, bounds, d) = (on.clone(), bounds.clone(), dragging.clone());
                window.on_mouse_event(move |e: &gpui::MouseMoveEvent, phase, w, cx| {
                    if phase != gpui::DispatchPhase::Bubble || !d.get() {
                        return;
                    }
                    if !e.dragging() {
                        d.set(false);
                        return;
                    }
                    if let Some(b) = bounds.get() {
                        on(Act::Volume(at(b, e.position.x)), w, cx);
                    }
                });
                let d = dragging.clone();
                window.on_mouse_event(move |_: &gpui::MouseUpEvent, _, _, _| d.set(false));
            },
        )
        .absolute()
        .inset_0()
    };
    div()
        .flex()
        .items_center()
        .gap(px(10.))
        .pt(px(8.))
        .child(label("Their volume"))
        .child(
            div()
                .id("call-volume")
                .relative()
                .flex_1()
                .min_w_0()
                .h(px(16.))
                .cursor_pointer()
                .on_mouse_down(gpui::MouseButton::Left, down)
                .child(track)
                .child(
                    div()
                        .absolute()
                        .left_0()
                        .right_0()
                        .top(px(4.))
                        .h(px(8.))
                        .rounded(px(4.))
                        .bg(white_a(0.1))
                        .child(
                            div()
                                .h_full()
                                .rounded_l(px(4.))
                                .bg(c(ACCENT))
                                .w(gpui::relative(pct)),
                        ),
                )
                .child(
                    div()
                        .absolute()
                        .top(px(1.))
                        .left(gpui::relative(pct))
                        .child(
                            div()
                                .ml(px(-14. * pct))
                                .size(px(14.))
                                .rounded_full()
                                .bg(c(0xffffff))
                                .shadow(vec![BoxShadow {
                                    color: ca(0, 0, 0, 0.3),
                                    offset: point(px(0.), px(1.)),
                                    blur_radius: px(4.),
                                    spread_radius: px(0.),
                                    inset: false,
                                }]),
                        ),
                ),
        )
        .child(
            value(format!("{}%", (call.volume * 100.).round() as i32))
                .w(px(40.))
                .flex_shrink_0()
                .justify_end(),
        )
}

const GRAPH_H: f32 = 72.;
const GRAPH_HEAD: f32 = 34.;

/// Delay as a line, lost audio as bars, over the last minute; the sample under the pointer reads out.
fn graph(call: &Call, on: &OnAct) -> AnyElement {
    let hist: Vec<Sample> = call.history.iter().copied().collect();
    if hist.len() < 2 {
        return div()
            .h(px(GRAPH_H))
            .rounded(px(8.))
            .bg(white_a(0.04))
            .flex()
            .items_center()
            .justify_center()
            .text_size(px(11.5))
            .text_color(c(0x8a8a8a))
            .child("Measuring the connection…")
            .into_any_element();
    }
    let max_rtt = hist.iter().map(|r| r.rtt).fold(200f32, f32::max);
    let hover = call.hover.filter(|i| *i < hist.len());
    let n = 60usize;
    let off = n - hist.len();
    let record = call.graph.clone();
    let samples = hist.clone();
    let plot = canvas(
        move |b, _, _| record.set(Some(b)),
        move |b, _, window, _| {
            let w = f32::from(b.size.width);
            let step = w / (n - 1) as f32;
            let at = |i: usize, r: &Sample| {
                let x = (off + i) as f32 * step;
                let y = GRAPH_H - 2. - (r.rtt / max_rtt) * (GRAPH_H - GRAPH_HEAD - 6.);
                (x, y)
            };
            let mut lost = c(0xfca559);
            lost.a = 0.85;
            for (i, r) in samples.iter().enumerate() {
                let hgt =
                    ((r.lost / 10.) * (GRAPH_H - GRAPH_HEAD - 4.)).min(GRAPH_H - GRAPH_HEAD - 4.);
                if hgt <= 0.5 {
                    continue;
                }
                let (x, _) = at(i, r);
                let bw = (step - 0.5).max(1.5);
                window.paint_quad(gpui::fill(
                    Bounds::new(
                        b.origin + point(px(x - step / 2.), px(GRAPH_H - 2. - hgt)),
                        gpui::size(px(bw), px(hgt)),
                    ),
                    lost,
                ));
            }
            let mut path = gpui::PathBuilder::stroke(px(1.5));
            for (i, r) in samples.iter().enumerate() {
                let (x, y) = at(i, r);
                let p = b.origin + point(px(x), px(y));
                if i == 0 {
                    path.move_to(p)
                } else {
                    path.line_to(p)
                }
            }
            if let Ok(p) = path.build() {
                window.paint_path(p, c(ACCENT));
            }
            if let Some(i) = hover {
                let (x, y) = at(i, &samples[i]);
                window.paint_quad(
                    gpui::fill(
                        Bounds::new(
                            b.origin + point(px(x - 3.), px(y - 3.)),
                            gpui::size(px(6.), px(6.)),
                        ),
                        c(0xffffff),
                    )
                    .corner_radii(px(3.)),
                );
            }
        },
    )
    .size_full();
    let pick = {
        let (on, bounds) = (on.clone(), call.graph.clone());
        let len = hist.len();
        move |e: &gpui::MouseMoveEvent, w: &mut Window, cx: &mut App| {
            let Some(b) = bounds.get() else { return };
            let step = f32::from(b.size.width) / (n - 1) as f32;
            let x = f32::from(e.position.x - b.origin.x);
            let i =
                ((x / step).round() as isize - off as isize).clamp(0, len as isize - 1) as usize;
            on(Act::Hover(Some(i)), w, cx);
        }
    };
    let leave = {
        let on = on.clone();
        move |hovered: &bool, w: &mut Window, cx: &mut App| {
            if !*hovered {
                on(Act::Hover(None), w, cx);
            }
        }
    };
    let readout = |i: usize| {
        let r = hist[i];
        let ago = hist.len() - 1 - i;
        div()
            .flex()
            .gap(px(3.))
            .child(
                div()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(c(ACCENT))
                    .child(format!("{} ms", r.rtt as u32)),
            )
            .child(format!(
                "· {} · {}",
                lost_word(r.lost),
                if ago == 0 {
                    "now".to_string()
                } else {
                    format!("{ago}s ago")
                }
            ))
    };
    let legend = match hover {
        Some(i) => div()
            .text_size(px(11.5))
            .text_color(c(0xf6f6f6))
            .child(readout(i))
            .into_any_element(),
        None => div()
            .flex()
            .justify_between()
            .w_full()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(5.))
                    .child(div().w(px(10.)).h(px(3.)).rounded(px(2.)).bg(c(ACCENT)))
                    .child(format!("delay, up to {} ms", max_rtt as u32)),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(5.))
                    .child(div().w(px(10.)).h(px(7.)).rounded(px(2.)).bg(c(0xfca559)))
                    .child("lost audio"),
            )
            .into_any_element(),
    };
    div()
        .flex()
        .flex_col()
        .gap(px(4.))
        .child(
            div()
                .id("call-graph")
                .relative()
                .h(px(GRAPH_H))
                .rounded(px(8.))
                .bg(white_a(0.04))
                .on_mouse_move(pick)
                .on_hover(leave)
                .child(plot),
        )
        .child(
            div()
                .flex()
                .min_h(px(14.))
                .text_size(px(10.5))
                .text_color(c(0x8a8a8a))
                .child(legend),
        )
        .into_any_element()
}

fn panel(call: &Call, proc_: &Processing, on: &OnAct) -> impl IntoElement {
    let live = call.live();
    let stats = call.stats.as_ref();
    let lost = call.lost_pct();
    let lost_text = if lost < 0.05 {
        "none".to_string()
    } else if lost < 1. {
        "under 1%".into()
    } else {
        format!("{lost:.prec$}%", prec = if lost < 10. { 1 } else { 0 })
    };
    let path = match stats.map(|s| s.path.as_str()) {
        Some("relay") => "via relay",
        Some("direct") => "direct",
        _ => "",
    };
    let bitrate = stats.map(|s| s.bitrate_kbps).filter(|b| *b > 0);
    let bitrate_hint = match (stats.map(|s| s.net_loss).unwrap_or(0.), bitrate) {
        (l, _) if l >= 4. => "lowered, the network is dropping packets",
        (_, Some(b)) if b >= 64 => "full quality",
        _ => "",
    };
    let row = |l: &'static str, v: AnyElement| div().flex().items_center().child(label(l)).child(v);
    div()
        .id("call-panel")
        .flex()
        .flex_col()
        .gap(px(10.))
        .pt(px(6.))
        .px(px(14.))
        .pb(px(14.))
        .cursor_default()
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(volume_slider(call, on))
        .when(!call.muted && !call.mic_off, |d| {
            d.child(
                div()
                    .flex()
                    .items_center()
                    .child(label("Your voice"))
                    .child(meter(call.levels.0, live)),
            )
        })
        .child(section("Voice processing"))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(6.))
                .child(switch(
                    "call-agc",
                    "Automatic gain",
                    proc_.auto_gain,
                    Switch::AutoGain,
                    on,
                ))
                .child(switch(
                    "call-aec",
                    "Echo cancellation",
                    proc_.echo_cancel,
                    Switch::EchoCancel,
                    on,
                ))
                .child(switch(
                    "call-ns",
                    "Noise suppression",
                    proc_.noise_suppress,
                    Switch::NoiseSuppress,
                    on,
                )),
        )
        .child(section("Connection"))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(6.))
                .child(row(
                    "Quality",
                    match call.quality {
                        Some(q) => value("").child(dot(q)).child(q.label()).into_any_element(),
                        None => value("Measuring…").into_any_element(),
                    },
                ))
                .child(row(
                    "Delay",
                    value(
                        stats
                            .map(|s| format!("{} ms", s.rtt_ms))
                            .unwrap_or_else(|| "…".into()),
                    )
                    .when(!path.is_empty(), |d| d.child(hint(path)))
                    .into_any_element(),
                ))
                .child(row("Lost audio", value(lost_text).into_any_element()))
                .child(row(
                    "Bitrate",
                    value(
                        bitrate
                            .map(|b| format!("{b} kbit/s"))
                            .unwrap_or_else(|| "…".into()),
                    )
                    .when(!bitrate_hint.is_empty(), |d| d.child(hint(bitrate_hint)))
                    .into_any_element(),
                )),
        )
        .child(graph(call, on))
}

/// The pill: its row drags or, tapped, opens the panel.
fn pill(call: &Call, store: &Store, proc_: &Processing, top: f32, on: &OnAct) -> AnyElement {
    let live = call.live();
    let ended = call.phase == Some(Phase::Ended);
    let talk = if live && !call.peer_muted {
        ((call.levels.1 - 0.3) / 0.55).clamp(0., 1.)
    } else {
        0.
    };
    let ring = vec![
        BoxShadow {
            color: accent_mix(0.12 + talk * 0.88),
            offset: point(px(0.), px(0.)),
            blur_radius: px(0.),
            spread_radius: px(2.),
            inset: false,
        },
        BoxShadow {
            color: accent_mix(talk * 0.7),
            offset: point(px(0.), px(0.)),
            blur_radius: px(talk * 12.),
            spread_radius: px(0.),
            inset: false,
        },
    ];
    let status = div()
        .flex()
        .items_center()
        .gap(px(5.))
        .h(px(13.2))
        .text_size(px(11.5))
        .line_height(px(13.2))
        .text_color(c(0xb2b2b2))
        .when_some(call.quality.filter(|_| live), |d, q| d.child(dot(q)))
        .child(div().truncate().child(format!(
            "{}{}",
            call.status(),
            if live && call.peer_muted {
                " · muted"
            } else {
                ""
            }
        )));
    let drag = call.drag.clone();
    let bounds = call.pill.clone();
    let down = move |e: &gpui::MouseDownEvent, _: &mut Window, _: &mut App| {
        if let Some(b) = bounds.get() {
            drag.set(Some(Drag {
                start: e.position,
                origin: b.origin,
                moved: false,
            }));
        }
    };
    let watch = {
        let (drag, record, on) = (call.drag.clone(), call.pill.clone(), on.clone());
        canvas(
            move |b, _, _| record.set(Some(b)),
            move |_, _, window, _| {
                if drag.get().is_none() {
                    return;
                }
                let (d, o) = (drag.clone(), on.clone());
                window.on_mouse_event(move |e: &gpui::MouseMoveEvent, phase, w, cx| {
                    let Some(mut g) = d.get() else { return };
                    if phase != gpui::DispatchPhase::Bubble {
                        return;
                    }
                    let delta = e.position - g.start;
                    if !g.moved && f32::from(delta.x).hypot(f32::from(delta.y)) < 4. {
                        return;
                    }
                    g.moved = true;
                    d.set(Some(g));
                    o(Act::Move(g.origin + delta), w, cx);
                });
                let (d, o) = (drag.clone(), on.clone());
                window.on_mouse_event(move |_: &gpui::MouseUpEvent, phase, w, cx| {
                    if phase != gpui::DispatchPhase::Bubble {
                        return;
                    }
                    if let Some(g) = d.take() {
                        o(if g.moved { Act::Moved } else { Act::Toggle }, w, cx);
                    }
                });
            },
        )
        .absolute()
        .inset_0()
    };
    let row = div()
        .id("call-pill-row")
        .relative()
        .flex()
        .items_center()
        .gap(px(8.))
        .pt(px(6.))
        .pb(px(6.))
        .pl(px(6.))
        .pr(px(8.))
        .bg(c(0x262626))
        .rounded_t(px(15.))
        .when(!call.expanded, |d| d.rounded_b(px(15.)))
        .cursor_grab()
        .when(call.expanded, |d| d.border_b_1().border_color(c(0x333333)))
        .on_mouse_down(gpui::MouseButton::Left, down)
        .child(
            div()
                .flex()
                .flex_shrink_0()
                .rounded_full()
                .shadow(ring)
                .child(crate::ui::avatar(store, &call.peer, 30.)),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .truncate()
                        .text_size(px(13.))
                        .line_height(px(15.))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(c(0xf6f6f6))
                        .child(store.name(&call.peer)),
                )
                .child(status),
        )
        .when(!ended, |d| d.child(mic_btn(call, on)).child(hangup_btn(on)));
    let body = div()
        .id("call-pill")
        .relative()
        .flex()
        .flex_col()
        .when(call.expanded, |d| d.w(px(OPEN_W)))
        .when(!call.expanded, |d| d.max_w(px(PILL_MAX_W)))
        .rounded(px(16.))
        .bg(ca(20, 20, 20, 0.95))
        .border_1()
        .border_color(c(0x333333))
        .shadow(vec![BoxShadow {
            color: ca(0, 0, 0, 0.4),
            offset: point(px(0.), px(8.)),
            blur_radius: px(32.),
            spread_radius: px(0.),
            inset: false,
        }])
        .overflow_hidden()
        .occlude()
        .when(ended, |d| d.opacity(0.7))
        .child(watch)
        .child(row)
        .when(call.expanded && !ended, |d| d.child(panel(call, proc_, on)));
    match call.pos {
        Some(p) => div()
            .absolute()
            .left(p.x)
            .top(p.y)
            .child(body)
            .into_any_element(),
        None => div()
            .absolute()
            .top(px(top))
            .left_0()
            .right_0()
            .flex()
            .justify_center()
            .child(body)
            .into_any_element(),
    }
}

/// Kept on screen for a pill of `size`: off the chrome strip and 8px from every edge.
pub fn clamp(
    p: Point<Pixels>,
    size: gpui::Size<Pixels>,
    window: gpui::Size<Pixels>,
    chrome: f32,
) -> Point<Pixels> {
    let top = chrome + 8.;
    let x = f32::from(p.x)
        .min(f32::from(window.width - size.width) - 8.)
        .max(8.);
    let y = f32::from(p.y)
        .min(f32::from(window.height - size.height) - 8.)
        .max(top);
    point(px(x), px(y))
}

impl Call {
    pub fn at(pos: Option<Point<Pixels>>) -> Self {
        Self {
            pos,
            ..Default::default()
        }
    }

    pub fn pill_size(&self) -> gpui::Size<Pixels> {
        self.pill
            .get()
            .map(|b| b.size)
            .unwrap_or(gpui::size(px(PILL_MAX_W), px(48.)))
    }
}

/// The overlay for the call in progress, if there is one.
pub fn render(
    call: &Call,
    store: &Store,
    proc_: &Processing,
    chrome: f32,
    on: &OnAct,
) -> Option<AnyElement> {
    call.id.as_ref()?;
    call.phase?;
    Some(if call.incoming() {
        gpui::deferred(ring(call, store, on))
            .with_priority(15)
            .into_any_element()
    } else {
        gpui::deferred(
            div()
                .absolute()
                .inset_0()
                .child(pill(call, store, proc_, chrome + 10., on)),
        )
        .with_priority(7)
        .into_any_element()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_reads_like_the_desktop() {
        assert_eq!(clock(0), "00:00");
        assert_eq!(clock(65_000), "01:05");
        assert_eq!(clock(3_725_000), "1:02:05");
    }

    #[test]
    fn lost_audio_is_concealed_over_received() {
        let mut call = Call::default();
        call.apply_state(&serde_json::json!({ "id": "a", "peer": "p", "phase": "active" }));
        call.apply_stats(
            &serde_json::json!({ "id": "a", "rtt_ms": 40, "received": 100, "concealed": 0 }),
        );
        call.apply_stats(
            &serde_json::json!({ "id": "a", "rtt_ms": 40, "received": 150, "concealed": 5 }),
        );
        assert!((call.history.back().unwrap().lost - 10.).abs() < 1e-4);
        assert_eq!(call.quality, Some(Quality::Poor));
        call.apply_stats(&serde_json::json!({ "id": "other", "rtt_ms": 900 }));
        assert_eq!(call.history.len(), 2);
    }
}
