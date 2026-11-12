//! `ChatScrollbar.svelte`: the chat's own scrollbar. The timeline holds a run of the history, so
//! a list's bar would only measure that run; this one measures the whole chat. Drag or click to
//! seek, point to read the date there; the wheel falls through to the list.

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Instant;

use gpui::prelude::*;
use gpui::{AnyElement, App, Bounds, FontWeight, MouseButton, Pixels, Window, div, point, px};

use crate::theme::*;

const MIN_THUMB: f32 = 32.;
/// The date eases after its place over this time constant (ms).
const FOLLOW_MS: f32 = 70.;
const LABEL_H: f32 = 34.;

#[derive(Default)]
pub struct Bar {
    /// The track, in window coordinates.
    pub track: Rc<Cell<Option<Bounds<Pixels>>>>,
    pub shown: bool,
    /// 0..1: how far through the chat the view sits.
    pub top: f32,
    /// 0..1: how much of the chat the view shows.
    pub size: f32,
    pub total: usize,
    pub visible: f32,
    pub dragging: bool,
    /// The pointer's offset into the thumb while dragging.
    pub grab: f32,
    /// The pointer's place on the track while it's over it.
    pub hover: Option<f32>,
    /// The date at the place last pointed at, and at the top of the view.
    pub tip: String,
    pub here: String,
    /// The row at the top of the view and the newest held one it's counted from, as last measured.
    pub sig: Option<(usize, String, usize)>,
    /// The newest held message of the view's run, with its rank from the newest.
    pub anchor: Option<(String, usize)>,
    pub ranks: HashMap<String, (Option<(usize, usize)>, Instant)>,
    pub asking: Option<String>,
    pub times: HashMap<(String, usize), Option<u64>>,
    pub point_want: Option<f32>,
    pub pointing: bool,
    pub seek_want: Option<f32>,
    pub seeking: bool,
    label: Cell<(f32, bool, bool)>,
    shown_y: Cell<f32>,
    last_frame: Cell<Option<Instant>>,
}

#[derive(Clone, Copy)]
pub enum Ev {
    Down(f32),
    Move(f32),
    Up(f32),
    Leave,
    /// The wheel, in pixels, for the list beneath.
    Wheel(f32),
}

pub type OnEv = Rc<dyn Fn(Ev, &mut Window, &mut App)>;

impl Bar {
    pub fn track_h(&self) -> f32 {
        self.track.get().map_or(0., |b| f32::from(b.size.height))
    }

    pub fn thumb(&self) -> (f32, f32) {
        let th = self.track_h();
        let h = th.min((self.size * th).max(MIN_THUMB));
        (h, (th - h) * self.top)
    }

    /// The place a press at `y` would put the view, the thumb held at `offset`.
    pub fn at(&self, y: f32, offset: f32) -> f32 {
        let (h, _) = self.thumb();
        ((y - offset) / (self.track_h() - h).max(1.)).clamp(0., 1.)
    }

    pub fn on_thumb(&self, y: f32) -> bool {
        let (h, top) = self.thumb();
        y >= top && y <= top + h
    }

    /// The message a place on the track puts at the top of the view, counted from the newest.
    pub fn newer_at(&self, p: f32) -> usize {
        let span = (self.total as f32 - self.visible).max(0.);
        (self.total as f32 - 1. - p * span).round().max(0.) as usize
    }

    pub fn reset(&mut self) {
        let track = self.track.clone();
        *self = Self { track, ..Self::default() };
    }
}

/// `Intl.DateTimeFormat([], { dateStyle: 'medium' })`.
pub fn date(ms: u64) -> String {
    use chrono::TimeZone;
    chrono::Local.timestamp_millis_opt(ms as i64).single().map(|d| d.format("%b %-d, %Y").to_string()).unwrap_or_default()
}

pub fn render(bar: &Bar, window: &mut Window, on: OnEv) -> AnyElement {
    let (thumb_h, thumb_y) = bar.thumb();
    let hovering = bar.hover.is_some();
    let on_thumb = !bar.dragging && bar.hover.is_some_and(|y| bar.on_thumb(y));
    let text = if on_thumb { &bar.here } else { &bar.tip };
    let tip_on = !text.is_empty() && (hovering || bar.dragging);
    // Where the date shows and how; both stay put while it hides.
    let (mut y, mut here, was_on) = bar.label.get();
    if tip_on {
        here = bar.dragging || on_thumb;
        y = if here { thumb_y + thumb_h / 2. } else { bar.hover.unwrap_or(0.) };
    }
    bar.label.set((y, here, tip_on));
    let now = Instant::now();
    let mut shown = bar.shown_y.get();
    if tip_on && !was_on {
        shown = y;
    } else if (y - shown).abs() >= 0.25 {
        let dt = bar.last_frame.get().map_or(16., |t| (now - t).as_secs_f32() * 1000.).min(64.);
        shown += (y - shown) * (1. - (-dt / FOLLOW_MS).exp());
        window.request_animation_frame();
    } else {
        shown = y;
    }
    bar.shown_y.set(shown);
    bar.last_frame.set(((y - shown).abs() >= 0.25).then_some(now));

    let lit = hovering || bar.dragging;
    let probe = bar.track.clone();
    let (down, mv, leave, wheel) = (on.clone(), on.clone(), on.clone(), on.clone());
    let dragging = bar.dragging;
    let rel = |b: Option<Bounds<Pixels>>, p: gpui::Point<Pixels>| b.map_or(0., |b| f32::from(p.y - b.top()));
    let label = (!text.is_empty()).then(|| {
        div()
            .absolute()
            .right(px(24.))
            .top(px(shown - LABEL_H / 2.))
            .h(px(LABEL_H))
            .flex()
            .items_center()
            .gap(px(6.))
            .px(px(10.))
            .rounded(px(6.))
            .border_1()
            .border_color(if here { ca(0x33, 0xdb, 0x98, 0.45) } else { white_a(0.08) })
            .bg(c(0x141414))
            .shadow(vec![gpui::BoxShadow {
                color: ca(0, 0, 0, 0.5),
                offset: point(px(0.), px(4.)),
                blur_radius: px(14.),
                spread_radius: px(0.),
                inset: false,
            }])
            .font_family(INTER)
            .text_size(px(12.))
            .font_weight(FontWeight::MEDIUM)
            .whitespace_nowrap()
            .text_color(if here { c(PRIMARY) } else { c(0xffffff) })
            .when(!tip_on, |d| d.invisible())
            .when(here, |d| d.child(div().size(px(6.)).rounded_full().bg(c(PRIMARY))))
            .child(text.clone())
    });
    div()
        .id("chat-scrollbar")
        .absolute()
        .top(px(4.))
        .bottom(px(4.))
        .right_0()
        .w(px(22.))
        .when(!bar.shown, |d| d.invisible())
        .when(bar.shown, |d| d.occlude())
        .on_scroll(move |e, w, cx| wheel(Ev::Wheel(f32::from(e.delta.pixel_delta(w.line_height()).y)), w, cx))
        .child(
            gpui::canvas(
                move |b, _, _| probe.set(Some(b)),
                move |b, _, window, _| {
                    // Held, the thumb follows the pointer anywhere until it lets go.
                    if !dragging {
                        return;
                    }
                    let (on_move, on_up) = (on.clone(), on.clone());
                    window.on_mouse_event(move |e: &gpui::MouseMoveEvent, phase, window, cx| {
                        if phase == gpui::DispatchPhase::Bubble {
                            on_move(Ev::Move(f32::from(e.position.y - b.top())), window, cx);
                        }
                    });
                    window.on_mouse_event(move |e: &gpui::MouseUpEvent, phase, window, cx| {
                        if phase == gpui::DispatchPhase::Bubble && e.button == MouseButton::Left {
                            on_up(Ev::Up(f32::from(e.position.y - b.top())), window, cx);
                        }
                    });
                },
            )
            .absolute()
            .size_full(),
        )
        .on_mouse_down(MouseButton::Left, {
            let track = bar.track.clone();
            move |e, w, cx| {
                cx.stop_propagation();
                down(Ev::Down(rel(track.get(), e.position)), w, cx)
            }
        })
        .on_mouse_move({
            let track = bar.track.clone();
            move |e, w, cx| {
                if !dragging {
                    mv(Ev::Move(rel(track.get(), e.position)), w, cx)
                }
            }
        })
        .on_hover(move |h, w, cx| {
            if !*h {
                leave(Ev::Leave, w, cx)
            }
        })
        .child(
            div()
                .absolute()
                .top(px(thumb_y))
                .right(px(if lit { 4. } else { 5. }))
                .w(px(if lit { 8. } else { 6. }))
                .h(px(thumb_h))
                .rounded(px(3.))
                .bg(white_a(if lit { 0.4 } else { 0.22 })),
        )
        .children(label)
        .into_any_element()
}

/// `.scroll-return-bar`: back to the newest message, with the count of those waiting below.
pub fn return_button(unread: u32, on: Rc<dyn Fn(&mut Window, &mut App)>) -> AnyElement {
    let lit = unread > 0;
    let label = match unread {
        0 => "Jump to present".to_string(),
        1 => "1 new message".to_string(),
        n if n > 99 => "99+ new messages".to_string(),
        n => format!("{n} new messages"),
    };
    let ink = if lit { c(PRIMARY) } else { c(0xffffff) };
    div()
        .absolute()
        .left_0()
        .right_0()
        .bottom(px(42.))
        .flex()
        .justify_center()
        .child(
            div()
                .id("chat-scroll-return")
                .occlude()
                .h(px(32.))
                .flex()
                .items_center()
                .gap(px(8.))
                .pl(px(12.))
                .pr(px(16.))
                .rounded_full()
                .border_1()
                .border_color(if lit { ca(0x33, 0xdb, 0x98, 0.45) } else { white_a(0.1) })
                .bg(c(0x141414))
                .shadow(vec![gpui::BoxShadow {
                    color: ca(0, 0, 0, 0.5),
                    offset: point(px(0.), px(4.)),
                    blur_radius: px(16.),
                    spread_radius: px(0.),
                    inset: false,
                }])
                .font_family(INTER)
                .text_size(px(13.))
                .font_weight(FontWeight::MEDIUM)
                .whitespace_nowrap()
                .text_color(ink)
                .cursor_pointer()
                .hover(move |d| {
                    let d = d.bg(c(0x1c1c1c));
                    if lit { d } else { d.border_color(white_a(0.2)) }
                })
                .active(|d| d.opacity(0.75))
                .on_click(move |_, w, cx| on(w, cx))
                .child(gpui::svg().path("icons/chevron-down.svg").size(px(14.)).text_color(ink))
                .child(label),
        )
        .into_any_element()
}
