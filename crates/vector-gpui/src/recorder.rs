//! The voice recorder (`voice.js`, `VoiceRecorderUI.svelte`): hold the mic to record, slide
//! left to cancel, drag up to lock, then a preview to play, scrub, delete or send.

use std::time::Instant;

use gpui::{
    AnyElement, App, Bounds, Corners, CursorStyle, HitboxBehavior, InteractiveElement,
    IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement,
    Pixels, Point, StatefulInteractiveElement, Styled, Window, canvas, div, fill,
    point, prelude::*, px, size, svg,
};

use crate::theme::*;

pub const HOLD_MS: u64 = 200;
const LOCK_DRAG: f32 = 85.;
const LOCK_SHOW: f32 = 5.;
const CANCEL_DRAG: f32 = 100.;
const RED: u32 = 0xff4444;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum State {
    Idle,
    /// Pressed, waiting out the hold.
    Pending,
    Recording,
    /// Recording with the finger off: the dot stops it.
    Locked,
    Preview,
    /// Sliding out after a cancel.
    Cancelled,
}

#[derive(Clone, Copy, PartialEq)]
enum Axis {
    X,
    Y,
}

pub struct Recorder {
    pub state: State,
    /// Bumped per press, so a hold timer from an earlier one is ignored.
    pub press: u64,
    origin: Point<Pixels>,
    axis: Option<Axis>,
    last_axis: Option<Axis>,
    /// The leftward and upward drag, clamped at zero.
    dx: f32,
    dy: f32,
    pub status: &'static str,
    started: Option<Instant>,
    pub tooltip: Option<Instant>,
    lock_fading: Option<Instant>,
    cancelled: Option<Instant>,
    /// The dot easing home after a drag.
    returning: Option<(Instant, f32, f32)>,
    // The preview's playback.
    pub source: Option<u32>,
    pub duration: u64,
    pub playing: bool,
    start_pos: u64,
    play_started: Option<Instant>,
    paused_at: u64,
    /// Bumped when the composer's controls come back, for their fade.
    pub fade: u64,
}

impl Default for Recorder {
    fn default() -> Self {
        Recorder {
            state: State::Idle,
            press: 0,
            origin: point(px(0.), px(0.)),
            axis: None,
            last_axis: None,
            dx: 0.,
            dy: 0.,
            status: "Recording...",
            started: None,
            tooltip: None,
            lock_fading: None,
            cancelled: None,
            returning: None,
            source: None,
            duration: 0,
            playing: false,
            start_pos: 0,
            play_started: None,
            paused_at: 0,
            fade: 0,
        }
    }
}

/// What the app should do after a gesture.
#[derive(Debug, PartialEq)]
pub enum Effect {
    None,
    Lock,
    Cancel,
    Stop,
}

impl Recorder {
    /// The row belongs to the recorder.
    pub fn busy(&self) -> bool {
        matches!(
            self.state,
            State::Recording | State::Locked | State::Preview | State::Cancelled
        )
    }

    pub fn press(&mut self, at: Point<Pixels>) -> Option<u64> {
        if self.state != State::Idle {
            return None;
        }
        self.origin = at;
        self.axis = None;
        self.last_axis = None;
        self.dx = 0.;
        self.dy = 0.;
        self.state = State::Pending;
        self.press += 1;
        Some(self.press)
    }

    pub fn fixture(&mut self, state: &str) {
        self.started = Some(Instant::now() - std::time::Duration::from_secs(4));
        match state {
            "locked" => self.state = State::Locked,
            "preview" => {
                self.preview(0, 7400);
                self.paused_at = 2600;
            }
            _ => self.state = State::Recording,
        }
    }

    pub fn recording(&mut self) {
        self.state = State::Recording;
        self.status = "Recording...";
        self.started = Some(Instant::now());
    }

    pub fn moved(&mut self, at: Point<Pixels>) -> Effect {
        if !matches!(self.state, State::Recording | State::Pending) {
            return Effect::None;
        }
        let dx = f32::from(self.origin.x - at.x);
        let dy = f32::from(self.origin.y - at.y);
        if self.axis.is_some() && dx.abs() < 15. && dy.abs() < 15. {
            self.axis = None;
        }
        if self.axis.is_none() && (dx.abs() > 15. || dy.abs() > 15.) {
            self.axis = Some(if dx.abs() > dy.abs() { Axis::X } else { Axis::Y });
        }
        if self.axis == Some(Axis::X) && dx > CANCEL_DRAG {
            return Effect::Cancel;
        }
        if self.axis == Some(Axis::Y) && dy > LOCK_DRAG && self.state == State::Recording {
            return Effect::Lock;
        }
        self.dx = dx.max(0.);
        self.dy = dy.max(0.);
        let dominant = (self.dx > 0. || self.dy > 0.)
            .then(|| if self.dx >= self.dy { Axis::X } else { Axis::Y });
        self.last_axis = self.axis.or(dominant);
        Effect::None
    }

    /// The finger lifted: a short press only hints, a recording stops for its preview.
    pub fn released(&mut self) -> Effect {
        self.reset_drag();
        match self.state {
            State::Pending => {
                self.state = State::Idle;
                self.tooltip = Some(Instant::now());
                Effect::None
            }
            State::Recording => Effect::Stop,
            _ => Effect::None,
        }
    }

    fn reset_drag(&mut self) {
        if self.dx > 0. || self.dy > 0. {
            let (x, y) = self.dot_offset();
            self.returning = Some((Instant::now(), x, y));
        }
        self.axis = None;
        self.last_axis = None;
        self.dx = 0.;
        self.dy = 0.;
    }

    pub fn lock(&mut self) {
        self.reset_drag();
        self.lock_fading = Some(Instant::now());
    }

    /// The lock target's fade is done: locked.
    pub fn locked(&mut self) {
        self.lock_fading = None;
        if self.state == State::Recording {
            self.state = State::Locked;
        }
    }

    pub fn cancel(&mut self) {
        self.reset_drag();
        self.state = State::Cancelled;
        self.cancelled = Some(Instant::now());
        self.source = None;
    }

    pub fn idle(&mut self) {
        let back = self.busy();
        *self = Recorder {
            press: self.press,
            tooltip: self.tooltip,
            fade: self.fade + back as u64,
            ..Recorder::default()
        };
    }

    pub fn preview(&mut self, source: u32, duration: u64) {
        self.state = State::Preview;
        self.source = Some(source);
        self.duration = duration;
        self.playing = false;
        self.paused_at = 0;
    }

    pub fn position(&self) -> u64 {
        if !self.playing {
            return self.paused_at;
        }
        (self.start_pos + self.play_started.map_or(0, |s| s.elapsed().as_millis() as u64))
            .min(self.duration)
    }

    pub fn played(&mut self, pos: u64) {
        self.playing = true;
        self.start_pos = pos;
        self.play_started = Some(Instant::now());
    }

    pub fn paused(&mut self) {
        self.paused_at = self.position();
        self.playing = false;
    }

    pub fn ended(&mut self) {
        self.playing = false;
        self.paused_at = 0;
    }

    pub fn seek(&mut self, ms: u64) {
        if self.playing {
            self.start_pos = ms;
            self.play_started = Some(Instant::now());
        } else {
            self.paused_at = ms;
        }
    }

    pub fn elapsed(&self) -> String {
        let s = self.started.map_or(0, |t| t.elapsed().as_secs());
        format!("{}:{:02}", s / 60, s % 60)
    }

    fn cancelling(&self) -> bool {
        self.last_axis == Some(Axis::X) && self.dx > 0.
    }

    fn dot_offset(&self) -> (f32, f32) {
        match self.axis.or(self.last_axis) {
            Some(Axis::X) => (-self.dx, 0.),
            Some(Axis::Y) => (0., -self.dy),
            None => (0., 0.),
        }
    }

    /// Anything still easing: the pulse, the timer, a fade or the dot's return.
    pub fn animating(&self) -> bool {
        matches!(self.state, State::Pending | State::Recording | State::Locked | State::Cancelled)
            || (self.state == State::Preview && self.playing)
            || self.returning.is_some_and(|(t, _, _)| t.elapsed().as_millis() < 160)
            || self.tooltip.is_some_and(|t| t.elapsed().as_millis() < 2200)
    }

    /// Timers that end a phase: the lock fade, the cancel slide.
    pub fn due(&self) -> Option<Due> {
        if self.lock_fading.is_some_and(|t| t.elapsed().as_millis() >= 200) {
            return Some(Due::Locked);
        }
        if self.state == State::Cancelled && self.cancelled.is_some_and(|t| t.elapsed().as_millis() >= 300) {
            return Some(Due::Idle);
        }
        None
    }
}

pub enum Due {
    Locked,
    Idle,
}

pub type On = std::rc::Rc<dyn Fn(Ui, &mut Window, &mut App)>;

/// What the recorder's controls ask for.
#[derive(Clone, Copy, Debug)]
pub enum Ui {
    Press(Point<Pixels>),
    Move(Point<Pixels>),
    Release,
    DotClick,
    Delete,
    PlayPause,
    Seek(f32),
}

fn pulse(t: f32) -> f32 {
    let p = (t / 1.5).fract();
    if p < 0.5 { p * 2. } else { 2. - p * 2. }
}

/// `.voice-recording-ui`: the readout that stands where the editor was.
pub fn strip(r: &Recorder) -> AnyElement {
    let clock = r.started.map_or(0., |t| t.elapsed().as_secs_f32());
    let cancelling = r.cancelling();
    let progress = (r.dx / CANCEL_DRAG).min(1.);
    let timer_o = if cancelling { 1. - (r.dx / 20.).min(1.) } else { 1. };
    let status_o = if cancelling { 1. - (r.dx / 50.).min(1.) } else { 1. };
    let (slide_x, slide_o, hot) = if cancelling {
        (r.dx * 0.5, 1. - progress * 0.5, progress > 0.5)
    } else {
        (0., 1., false)
    };
    let nudge = {
        let p = (clock % 1.) * 2.;
        let p = if p < 1. { p } else { 2. - p };
        -5. * (p * p * (3. - 2. * p))
    };
    let out = r
        .cancelled
        .filter(|_| r.state == State::Cancelled)
        .map(|t| (t.elapsed().as_secs_f32() / 0.3).min(1.));
    let row = div()
        .flex_1()
        .min_w_0()
        .min_h(px(45.))
        .px(px(12.))
        .flex()
        .items_center()
        .gap(px(12.))
        .font_family(INTER)
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .flex_shrink_0()
                .opacity(status_o)
                .child(
                    div()
                        .size(px(8.))
                        .rounded_full()
                        .bg(c(RED))
                        .opacity(1. - 0.6 * pulse(clock * 1.5)),
                )
                .child(
                    div()
                        .text_size(px(14.))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(white_a(0.8))
                        .child(r.status),
                ),
        )
        .child(
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_end()
                .gap(px(6.))
                .pr(px(12.))
                .when(r.state == State::Locked, |d| d.invisible())
                .opacity(slide_o)
                .relative()
                .left(px(-slide_x))
                .text_color(white_a(0.5))
                .child(
                    div()
                        .size(px(20.))
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            svg()
                                .path("icons/chevron-double-left.svg")
                                .size(px(16.))
                                .relative()
                                .left(px(nudge))
                                .text_color(if hot { c(RED) } else { white_a(0.5) }),
                        ),
                )
                .child(div().text_size(px(13.)).whitespace_nowrap().child("Slide to cancel")),
        )
        .child(
            div()
                .flex_shrink_0()
                .pr(px(35.))
                .text_size(px(16.))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(gpui::white())
                .opacity(timer_o)
                .child(r.elapsed()),
        );
    match out {
        Some(p) => div()
            .flex_1()
            .min_w_0()
            .overflow_hidden()
            .child(row.relative().left(gpui::relative(-p)).opacity(1. - p))
            .into_any_element(),
        None => row.into_any_element(),
    }
}

/// `.voice-recorder-red-circle`: over the mic while recording; a stop square once locked.
pub fn dot(r: &Recorder, on: On) -> Option<AnyElement> {
    if !matches!(r.state, State::Pending | State::Recording | State::Locked) {
        return None;
    }
    let locked = r.state == State::Locked;
    let (mut x, mut y) = r.dot_offset();
    if let Some((t, x0, y0)) = r.returning {
        let p = (t.elapsed().as_secs_f32() / 0.15).min(1.);
        if x == 0. && y == 0. && p < 1. {
            let e = 1. - (1. - p).powi(2);
            x = x0 * (1. - e);
            y = y0 * (1. - e);
        }
    }
    let side = if locked { 24. } else { 26. };
    let clock = r.started.map_or(0., |t| t.elapsed().as_secs_f32());
    let ring = if r.state == State::Recording { pulse(clock) } else { 0. };
    Some(
        div()
            .absolute()
            .right(px(9.5 + (26. - side) / 2.))
            .top(gpui::relative(0.5))
            .mt(px(-side / 2.))
            .child(
                div()
                    .id("voice-dot")
                    .relative()
                    .left(px(x))
                    .top(px(y))
                    .size(px(side))
                    .rounded(px(if locked { 6. } else { side / 2. }))
                    .bg(c(RED))
                    .when(ring > 0., |d| {
                        d.shadow(vec![gpui::BoxShadow {
                            color: ca(255, 68, 68, 0.4 * (1. - ring)),
                            offset: point(px(0.), px(0.)),
                            blur_radius: px(0.),
                            spread_radius: px(8. * ring),
                            inset: false,
                        }])
                    })
                    .when(locked, |d| {
                        d.cursor_pointer()
                            .hover(|s| s.opacity(0.95))
                            .on_click(move |_, w, cx| on(Ui::DotClick, w, cx))
                    }),
            )
            .into_any_element(),
    )
}

/// The hold hint and the lock target, over the box.
pub fn overlays(r: &Recorder) -> Vec<AnyElement> {
    let mut out = Vec::new();
    if let Some(t) = r.tooltip {
        let ms = t.elapsed().as_millis() as f32;
        let p = if ms < 200. {
            ms / 200.
        } else if ms < 2000. {
            1.
        } else {
            1. - ((ms - 2000.) / 200.).min(1.)
        };
        if p > 0. {
            out.push(
                div()
                    .absolute()
                    .bottom(px(60. - 5. * (1. - p)))
                    .right(px(12.))
                    .px(px(12.))
                    .py(px(6.))
                    .rounded(px(16.))
                    .bg(ca(0, 0, 0, 0.8))
                    .text_size(px(12.))
                    .text_color(gpui::white())
                    .whitespace_nowrap()
                    .opacity(p)
                    .child("Hold to record")
                    .into_any_element(),
            );
        }
    }
    let active = matches!(r.state, State::Recording | State::Locked);
    if active && r.state == State::Recording {
        let show = r.axis == Some(Axis::Y) && r.dy >= LOCK_SHOW;
        let v = if show { ((r.dy - LOCK_SHOW) / (LOCK_DRAG - LOCK_SHOW)).min(1.) } else { 0. };
        let fading = r.lock_fading.map_or(1., |t| 1. - (t.elapsed().as_secs_f32() / 0.2).min(1.));
        let o = v * fading;
        if o > 0. {
            out.push(
                div()
                    .absolute()
                    .right(px(12.))
                    .bottom(px(55.))
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(4.))
                    .opacity(o)
                    .child(
                        div()
                            .relative()
                            .top(px(-v * 15.))
                            .size(px(36. * (1. + v * 0.2)))
                            .rounded_full()
                            .bg(white_a(0.15))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(svg().path("icons/locked.svg").size(px(16.)).text_color(white_a(0.7))),
                    )
                    .child(
                        div()
                            .size(px(20.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .opacity(1. - v)
                            .child(
                                svg()
                                    .path("icons/chevron-double-left.svg")
                                    .size(px(16.))
                                    .text_color(white_a(0.9))
                                    .with_transformation(gpui::Transformation::rotate(gpui::radians(
                                        std::f32::consts::FRAC_PI_2,
                                    ))),
                            ),
                    )
                    .into_any_element(),
            );
        }
    }
    out
}

/// `.voice-preview-ui`: delete, play, the scrub bar and the time.
pub fn preview(r: &Recorder, on: On) -> AnyElement {
    let pos = r.position();
    let frac = if r.duration > 0 { pos as f32 / r.duration as f32 } else { 0. };
    let round = |id: &'static str, icon: &'static str, side: f32, color: gpui::Hsla, ui: Ui, on: On| {
        div()
            .id(id)
            .size(px(45.))
            .flex_shrink_0()
            .rounded_full()
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .group(id)
            .on_click(move |_, w, cx| on(ui, w, cx))
            .child(
                svg()
                    .path(icon)
                    .size(px(side))
                    .text_color(color)
                    .group_hover(id, |s| s.opacity(0.7)),
            )
    };
    let seek = on.clone();
    let bar = div()
        .flex_1()
        .h(px(10.))
        .child(
            canvas(
                |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
                move |bounds: Bounds<Pixels>, hitbox, window, _| {
                    let w = f32::from(bounds.size.width);
                    window.paint_quad(fill(bounds, white_a(0.08)).corner_radii(Corners::all(px(5.))));
                    if frac > 0. {
                        window.paint_quad(
                            fill(
                                Bounds::new(bounds.origin, size(px(w * frac), px(10.))),
                                c(PRIMARY),
                            )
                            .corner_radii(Corners {
                                top_left: px(5.),
                                bottom_left: px(5.),
                                top_right: px(0.),
                                bottom_right: px(0.),
                            }),
                        );
                    }
                    let hx = if frac > 0. { w * frac - 7. } else { 0. };
                    let handle = Bounds::new(
                        point(bounds.origin.x + px(hx), bounds.origin.y - px(2.)),
                        size(px(14.), px(14.)),
                    );
                    window.paint_quad(
                        gpui::quad(handle, px(7.), gpui::white(), px(0.), gpui::transparent_black(), Default::default()),
                    );
                    window.set_cursor_style(CursorStyle::PointingHand, &hitbox);
                    let frac_at = move |p: Pixels| f32::from(p - bounds.origin.x) / w.max(1.);
                    let dragging = std::rc::Rc::new(std::cell::Cell::new(false));
                    {
                        let (dragging, seek, hitbox) = (dragging.clone(), seek.clone(), hitbox.clone());
                        window.on_mouse_event(move |e: &MouseDownEvent, phase, window, cx| {
                            if phase.bubble() && e.button == MouseButton::Left && hitbox.is_hovered(window) {
                                dragging.set(true);
                                seek(Ui::Seek(frac_at(e.position.x)), window, cx);
                            }
                        });
                    }
                    {
                        let (dragging, seek) = (dragging.clone(), seek.clone());
                        window.on_mouse_event(move |e: &MouseMoveEvent, phase, window, cx| {
                            if phase.bubble() && dragging.get() && e.dragging() {
                                seek(Ui::Seek(frac_at(e.position.x)), window, cx);
                            }
                        });
                    }
                    window.on_mouse_event(move |e: &MouseUpEvent, phase, window, cx| {
                        if phase.bubble() && dragging.replace(false) {
                            seek(Ui::Seek(frac_at(e.position.x)), window, cx);
                        }
                    });
                },
            )
            .size_full(),
        );
    div()
        .flex_1()
        .min_w_0()
        .min_h(px(45.))
        .flex()
        .items_center()
        .justify_between()
        .child(round("voice-preview-delete", "icons/trash.svg", 20., c(crate::timeline::DANGER), Ui::Delete, on.clone()))
        .child(
            div()
                .flex_1()
                .max_w(gpui::relative(0.75))
                .mx_auto()
                .flex()
                .items_center()
                .justify_center()
                .gap(px(10.))
                .child(round(
                    "voice-preview-play",
                    if r.playing { "icons/pause.svg" } else { "icons/play.svg" },
                    29.3,
                    gpui::white(),
                    Ui::PlayPause,
                    on,
                ))
                .child(bar)
                .child(
                    div()
                        .w(px(45.))
                        .flex_shrink_0()
                        .text_size(px(12.))
                        .text_color(white_a(0.6))
                        .child(crate::audio::format_time(pos)),
                ),
        )
        .into_any_element()
}

/// While a press or a recording is live, the pointer anywhere drives it.
pub fn tracker(on: On) -> impl IntoElement {
    canvas(
        |_, _, _| {},
        move |_, _, window, _| {
            let mv = on.clone();
            window.on_mouse_event(move |e: &MouseMoveEvent, phase, window, cx| {
                if phase.bubble() {
                    mv(Ui::Move(e.position), window, cx);
                }
            });
            let up = on.clone();
            window.on_mouse_event(move |e: &MouseUpEvent, phase, window, cx| {
                if phase.bubble() && e.button == MouseButton::Left {
                    up(Ui::Release, window, cx);
                }
            });
        },
    )
    .absolute()
    .size_0()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_leftward_drag_cancels_and_an_upward_one_locks() {
        let mut r = Recorder::default();
        r.press(point(px(500.), px(700.)));
        r.recording();
        assert_eq!(r.moved(point(px(480.), px(700.))), Effect::None);
        assert_eq!(r.moved(point(px(390.), px(700.))), Effect::Cancel);
        let mut r = Recorder::default();
        r.press(point(px(500.), px(700.)));
        r.recording();
        assert_eq!(r.moved(point(px(500.), px(680.))), Effect::None);
        assert_eq!(r.moved(point(px(500.), px(600.))), Effect::Lock);
    }

    #[test]
    fn a_short_press_only_hints() {
        let mut r = Recorder::default();
        r.press(point(px(0.), px(0.)));
        assert_eq!(r.released(), Effect::None);
        assert_eq!(r.state, State::Idle);
        assert!(r.tooltip.is_some());
    }
}
