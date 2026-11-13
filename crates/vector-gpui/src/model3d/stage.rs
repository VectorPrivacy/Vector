//! `ModelAttachment.svelte`: a 320 px square to turn a model in, opened full screen by a tap,
//! its animation behind a Play button. A giant model waits for "View in 3D".

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::prelude::*;
use gpui::{AnyElement, App, Bounds, FontWeight, MouseButton, MouseMoveEvent, MouseUpEvent, Pixels, RenderImage, ScrollDelta, ScrollEvent, Window, canvas, div, px, svg};
use crate::pic::img;

use super::view::{Phase, View};
use super::{Src, supports};
use crate::theme::{c, ca};

pub const GIANT: u64 = 50 * 1024 * 1024;
const SIDE: f32 = 320.;
/// A view off screen this long gives its GPU memory back.
const IDLE: Duration = Duration::from_secs(30);

#[derive(Clone, Debug)]
pub enum Act {
    Size(f32, f32),
    Down(f32, f32),
    Move(f32, f32),
    Up,
    Wheel(f32),
    Play,
    Open,
    Reset,
    Arm,
}

pub type OnAct = Rc<dyn Fn(Act, &mut Window, &mut App)>;

/// Chat views by attachment id, and the wake the engine rings when a frame lands.
#[derive(Default)]
pub struct Models {
    views: HashMap<String, View>,
    armed: HashSet<String>,
    wake: Option<futures::channel::mpsc::UnboundedSender<()>>,
    wake_rx: Option<futures::channel::mpsc::UnboundedReceiver<()>>,
    /// A full-screen view stands in front: the chat's hold still.
    pub front: bool,
    pub scale: f32,
    pub active: bool,
    backdrop: Option<((u32, u32), Arc<RenderImage>)>,
    pub retired: Vec<Arc<RenderImage>>,
}

impl Models {
    pub fn waker(&mut self) -> futures::channel::mpsc::UnboundedSender<()> {
        self.wake
            .get_or_insert_with(|| {
                let (tx, rx) = futures::channel::mpsc::unbounded();
                self.wake_rx = Some(rx);
                tx
            })
            .clone()
    }

    /// The receiver the app listens on, once.
    pub fn take_wake(&mut self) -> Option<futures::channel::mpsc::UnboundedReceiver<()>> {
        self.waker();
        self.wake_rx.take()
    }

    pub fn armed(&self, att: &str, size: u64) -> bool {
        size <= GIANT || self.armed.contains(att)
    }

    pub fn view(&mut self, att: &str, ext: &str, src: impl FnOnce() -> Option<Src>) -> Option<&mut View> {
        if !self.views.contains_key(att) {
            let src = src()?;
            let wake = self.waker();
            self.views.insert(att.to_string(), View::new(src, ext, false, wake));
        }
        self.views.get_mut(att)
    }

    pub fn get(&mut self, att: &str) -> Option<&mut View> {
        self.views.get_mut(att)
    }

    pub fn unavailable(&self, att: &str) -> bool {
        self.views.get(att).is_some_and(|v| v.phase == Phase::Unavailable)
    }

    /// True when the act opens the model full screen.
    pub fn act(&mut self, att: &str, act: &Act) -> bool {
        if let Act::Arm = act {
            self.armed.insert(att.to_string());
            return false;
        }
        let scale = self.scale;
        let Some(v) = self.views.get_mut(att) else { return false };
        match *act {
            Act::Size(w, h) => v.set_size(w, h, scale),
            Act::Down(x, y) => v.down(x, y),
            Act::Move(x, y) => v.moved(x, y),
            Act::Up => return v.up(),
            Act::Wheel(d) => v.wheel(d),
            Act::Play => {
                let on = !v.playing;
                v.set_playing(on);
            }
            Act::Reset => v.reset(),
            Act::Open => return true,
            Act::Arm => {}
        }
        false
    }

    /// One frame: true while any view on screen moves on its own.
    pub fn tick(&mut self) -> bool {
        let now = Instant::now();
        self.views.retain(|_, v| v.seen.is_none_or(|t| now - t < IDLE));
        let mut moving = false;
        for v in self.views.values_mut() {
            let on_screen = v.seen.is_some_and(|t| now - t < Duration::from_millis(250));
            moving |= v.tick(on_screen && self.active && !self.front) && on_screen;
            self.retired.append(&mut v.retired);
        }
        moving
    }

    /// The stage's radial glow, drawn once per size.
    fn backdrop(&mut self, w: u32, h: u32) -> Arc<RenderImage> {
        if let Some((size, img)) = &self.backdrop
            && *size == (w, h)
        {
            return img.clone();
        }
        let (cx, cy) = (w as f32 * 0.5, h as f32 * 0.4);
        let far = (w as f32 * 0.5).hypot(h as f32 * 0.6);
        let mut px = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let r = (x as f32 + 0.5 - cx).hypot(y as f32 + 0.5 - cy) / (far * 0.7);
                let a = 0.07 + (0.015 - 0.07) * r.min(1.);
                px.extend_from_slice(&[255, 255, 255, (a * 255.).round() as u8]);
            }
        }
        let img = Arc::new(RenderImage::new(vec![image::Frame::new(image::RgbaImage::from_raw(w, h, px).unwrap_or_default())]));
        if let Some((_, old)) = self.backdrop.replace(((w, h), img.clone())) {
            self.retired.push(old);
        }
        img
    }
}

pub fn badge() -> gpui::Div {
    div()
        .h(px(22.))
        .px(px(7.))
        .flex()
        .items_center()
        .rounded(px(6.))
        .bg(ca(0, 0, 0, 0.45))
        .text_color(ca(255, 255, 255, 0.75))
        .text_size(px(11.))
        .font_weight(FontWeight::BOLD)
        .child("3D")
}

fn tool(id: &'static str, icon: &'static str, act: Act, on: &OnAct) -> AnyElement {
    let tip = match act {
        Act::Open => Some("Full screen"),
        Act::Reset => Some("Reset view"),
        _ => None,
    };
    let on = on.clone();
    div()
        .id(id)
        .when_some(tip, |d, t| d.tooltip(crate::kit::tooltip(t)))
        .size(px(32.))
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .bg(ca(0, 0, 0, 0.5))
        .hover(|d| d.bg(ca(0, 0, 0, 0.7)))
        .cursor_pointer()
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(move |_, w, cx| {
            cx.stop_propagation();
            on(act.clone(), w, cx)
        })
        .child(svg().path(icon).size(px(16.)).text_color(c(0xffffff)))
        .into_any_element()
}

/// Browser wheel units from a GPUI scroll: positive away from the user.
pub fn wheel_y(e: &ScrollEvent) -> f32 {
    match e.delta {
        ScrollDelta::Pixels(p) => -f32::from(p.y),
        ScrollDelta::Lines(l) => -l.y * 50.,
    }
}

/// The measuring layer: reports the stage's size, and follows a drag past its edges.
pub fn measure(size: (f32, f32), dragging: bool, moving: bool, on: OnAct) -> AnyElement {
    let on2 = on.clone();
    canvas(
        move |b: Bounds<Pixels>, w, cx| {
            let s = (f32::from(b.size.width), f32::from(b.size.height));
            if (s.0 - size.0).abs() > 0.5 || (s.1 - size.1).abs() > 0.5 {
                on(Act::Size(s.0, s.1), w, cx);
            }
        },
        move |_, _, window, _| {
            if moving {
                window.request_animation_frame();
            }
            if dragging {
                let on = on2.clone();
                window.on_mouse_event(move |e: &MouseMoveEvent, phase, w, cx| {
                    if phase.bubble() {
                        on(Act::Move(f32::from(e.position.x), f32::from(e.position.y)), w, cx);
                    }
                });
                let on = on2.clone();
                window.on_mouse_event(move |e: &MouseUpEvent, phase, w, cx| {
                    if phase.bubble() && e.button == MouseButton::Left {
                        on(Act::Up, w, cx);
                    }
                });
            }
        },
    )
    .absolute()
    .inset_0()
    .into_any_element()
}

pub struct Facts<'a> {
    pub att: &'a str,
    pub size: u64,
    pub ext: &'a str,
}

/// The stage, or nothing when there is no GPU to draw with (the file box stands alone).
pub fn stage(models: &mut Models, f: &Facts, src: impl FnOnce() -> Option<Src>, on: OnAct) -> Option<AnyElement> {
    if !supports(f.ext) || models.unavailable(f.att) {
        return None;
    }
    let armed = models.armed(f.att, f.size);
    let scale = models.scale.max(1.);
    let mut el = div()
        .id(gpui::ElementId::Name(format!("model3d-{}", f.att).into()))
        .relative()
        .w(px(SIDE))
        .max_w_full()
        .aspect_ratio(1.)
        .rounded(px(12.))
        .overflow_hidden()
        .border_1()
        .border_color(ca(255, 255, 255, 0.06));
    let side = (SIDE * scale.min(2.)).round() as u32;
    let backdrop = models.backdrop(side, side);
    el = el.child(img(backdrop).absolute().inset_0().size_full());
    if !armed {
        let on = on.clone();
        return Some(
            el.child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(12.))
                    .child(badge())
                    .child(div().text_size(px(13.)).text_color(ca(255, 255, 255, 0.6)).child(format!("Large model · {}", crate::media::format_bytes(f.size))))
                    .child(
                        div()
                            .id("model3d-arm")
                            .h(px(32.))
                            .px(px(14.))
                            .flex()
                            .items_center()
                            .rounded(px(6.))
                            .border_1()
                            .border_color(ca(64, 200, 228, 0.45))
                            .bg(ca(64, 200, 228, 0.14))
                            .hover(|d| d.bg(ca(64, 200, 228, 0.24)))
                            .text_size(px(12.))
                            .text_color(c(0xc9f2fa))
                            .cursor_pointer()
                            .on_click(move |_, w, cx| on(Act::Arm, w, cx))
                            .child("View in 3D"),
                    ),
            )
            .into_any_element(),
        );
    }
    let v = models.view(f.att, f.ext, src)?;
    v.seen = Some(Instant::now());
    let size = v.css_size();
    let dragging = v.dragging();
    let moving = v.moving_hint();
    el = el.child(measure(size, dragging, moving, on.clone()));
    if let Some(frame) = v.frame.clone() {
        el = el.child(img(frame).absolute().inset_0().size_full().opacity(v.opacity()));
    }
    let note = |text: String, color| div().absolute().inset_0().flex().items_center().justify_center().p(px(16.)).text_center().text_size(px(13.)).text_color(color).child(text);
    match &v.phase {
        Phase::Loading => el = el.child(note("Loading model…".into(), ca(255, 255, 255, 0.5))),
        Phase::Failed(t) => el = el.child(note(t.clone(), ca(255, 140, 117, 0.9))),
        Phase::Unavailable => {}
        Phase::Ready => {
            let mut top = div().absolute().top(px(8.)).left(px(8.)).right(px(8.)).flex().items_center().gap(px(6.)).child(badge());
            if v.clips.len() > 1 {
                let on = on.clone();
                top = top.child(
                    div()
                        .id("model3d-clip")
                        .min_w_0()
                        .h(px(22.))
                        .px(px(8.))
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .rounded(px(6.))
                        .bg(ca(0, 0, 0, 0.45))
                        .hover(|d| d.bg(ca(0, 0, 0, 0.65)))
                        .text_size(px(12.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(ca(255, 255, 255, 0.85))
                        .cursor_pointer()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(move |_, w, cx| on(Act::Open, w, cx))
                        .child(div().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(v.clips[0].name.clone()))
                        .child(div().flex_shrink_0().text_color(ca(255, 255, 255, 0.5)).child(format!("+{}", v.clips.len() - 1))),
                );
            }
            el = el.child(top);
            let mut tools = div().absolute().right(px(8.)).bottom(px(8.)).flex().gap(px(6.));
            if !v.clips.is_empty() {
                tools = tools.child(tool("model3d-play", if v.playing { "ui/m3d-pause.svg" } else { "ui/m3d-play.svg" }, Act::Play, &on));
            }
            tools = tools.child(tool("model3d-full", "ui/m3d-full.svg", Act::Open, &on)).child(tool("model3d-reset", "ui/m3d-reset.svg", Act::Reset, &on));
            el = el.child(tools);
        }
    }
    let (down, wheel) = (on.clone(), on.clone());
    let ready = v.ready();
    Some(
        el.when(ready, |d| d.cursor(if dragging { gpui::CursorStyle::ClosedHand } else { gpui::CursorStyle::OpenHand }))
            .on_mouse_down(MouseButton::Left, move |e, w, cx| {
                down(Act::Down(f32::from(e.position.x), f32::from(e.position.y)), w, cx);
            })
            .on_scroll(move |e, w, cx| {
                // In a chat a plain wheel scrolls it; ctrl+wheel (a trackpad's pinch) zooms.
                if e.modifiers.control {
                    cx.stop_propagation();
                    wheel(Act::Wheel(wheel_y(e)), w, cx);
                }
            })
            .into_any_element(),
    )
}
