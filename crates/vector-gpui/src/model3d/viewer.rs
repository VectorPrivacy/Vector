//! `ModelViewer.svelte`: a model full screen in the image viewer's frame. Drag to turn, wheel
//! to zoom, double-click to reset; an animated model gets its clips, a scrubber and speed.

use std::time::Duration;

use futures::StreamExt;
use gpui::prelude::*;
use gpui::{
    AnimationExt, Bounds, Context, EventEmitter, FocusHandle, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels,
    ScrollEvent, Window, canvas, div, img, px, svg,
};

use super::Src;
use super::stage::wheel_y;
use super::view::{Phase, View};
use crate::theme::{ACCENT, INTER, c, ca};

pub enum Event {
    Closed,
    File,
}

#[derive(Clone, Copy, PartialEq)]
pub enum FileAction {
    Reveal,
    Save,
}

pub struct ModelViewer {
    focus: FocusHandle,
    view: View,
    name: String,
    file: Option<FileAction>,
    scrub: Option<Bounds<Pixels>>,
    scrubbing: bool,
    picking: bool,
}

impl EventEmitter<Event> for ModelViewer {}

const THUMB: f32 = 16.;

fn fmt(t: f32) -> String {
    let t = t.max(0.);
    format!("{}:{:02}", (t / 60.).floor() as u32, (t % 60.).floor() as u32)
}

impl ModelViewer {
    pub fn new(src: Src, ext: &str, name: String, file: Option<FileAction>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let (wake, mut rx) = futures::channel::mpsc::unbounded();
        cx.spawn(async move |this, cx| {
            while rx.next().await.is_some() {
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        })
        .detach();
        Self { focus, view: View::new(src, ext, true, wake), name, file, scrub: None, scrubbing: false, picking: false }
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        cx.emit(Event::Closed);
    }

    fn scrub_to(&mut self, x: f32) {
        let Some(b) = self.scrub else { return };
        let w = f32::from(b.size.width) - THUMB;
        if w <= 0. {
            return;
        }
        let pos = ((x - f32::from(b.origin.x) - THUMB / 2.) / w).clamp(0., 1.);
        let d = self.view.duration();
        self.view.seek(pos * d);
    }

    fn anim_bar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let v = &self.view;
        let playing = v.playing;
        let duration = v.duration();
        let time = v.time();
        let pos = if duration > 0. { (time / duration).min(1.) } else { 0. };
        let me = cx.entity().downgrade();
        let btn = |id: &'static str| {
            div()
                .id(id)
                .flex_shrink_0()
                .h(px(32.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(8.))
                .bg(ca(255, 255, 255, 0.08))
                .hover(|d| d.bg(ca(255, 255, 255, 0.16)))
                .cursor_pointer()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        };
        let scrubber = div()
            .id("model-viewer-scrub")
            .relative()
            .flex_1()
            .min_w(px(60.))
            .h(px(THUMB))
            .cursor_pointer()
            .child(
                canvas(
                    move |b, _, cx| {
                        me.update(cx, |v, _| v.scrub = Some(b)).ok();
                    },
                    move |b, _, window, _| {
                        let w = f32::from(b.size.width) - THUMB;
                        let fill = pos * w + THUMB / 2.;
                        let track = |x0: f32, x1: f32, color| {
                            gpui::fill(
                                Bounds::new(gpui::point(b.origin.x + px(x0), b.origin.y + px(5.)), gpui::size(px((x1 - x0).max(0.)), px(6.))),
                                color,
                            )
                            .corner_radii(px(3.))
                        };
                        window.paint_quad(track(0., f32::from(b.size.width), ca(255, 255, 255, 0.18)));
                        window.paint_quad(track(0., fill, c(ACCENT)));
                        window.paint_quad(
                            gpui::fill(Bounds::new(gpui::point(b.origin.x + px(pos * w), b.origin.y), gpui::size(px(THUMB), px(THUMB))), c(0xffffff)).corner_radii(px(THUMB / 2.)),
                        );
                    },
                )
                .absolute()
                .top_0()
                .bottom_0()
                .left(px(20.))
                .right(px(20.)),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|v, e: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    v.scrubbing = true;
                    v.scrub_to(f32::from(e.position.x));
                    cx.notify();
                }),
            );
        let mut bar = div()
            .absolute()
            .bottom(px(24.))
            .left_0()
            .right_0()
            .flex()
            .justify_center()
            .px(px(16.))
            .child(
                div()
                    .id("model-viewer-anim")
                    .w_full()
                    .max_w(px(560.))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .py(px(10.))
                    .px(px(12.))
                    .rounded(px(14.))
                    .bg(ca(20, 20, 20, 0.82))
                    .border_1()
                    .border_color(ca(255, 255, 255, 0.08))
                    .text_color(c(0xffffff))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(
                        btn("model-viewer-play")
                            .w(px(32.))
                            .on_click(cx.listener(move |v, _, _, cx| {
                                v.view.set_playing(!playing);
                                cx.notify();
                            }))
                            .child(svg().path(if playing { "ui/m3d-pause.svg" } else { "ui/m3d-play.svg" }).size(px(16.)).text_color(c(0xffffff))),
                    )
                    .child(scrubber)
                    .child(div().flex_shrink_0().text_size(px(12.)).text_color(ca(255, 255, 255, 0.6)).child(format!("{} / {}", fmt(time), fmt(duration))))
                    .child(
                        btn("model-viewer-speed")
                            .min_w(px(44.))
                            .px(px(8.))
                            .text_size(px(13.))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .on_click(cx.listener(|v, _, _, cx| {
                                v.view.cycle_speed();
                                cx.notify();
                            }))
                            .child(format!("{}×", self.view.speed)),
                    )
                    .children((self.view.clips.len() > 1).then(|| self.clip_select(cx))),
            );
        bar = bar.occlude();
        bar
    }

    fn clip_select(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let open = self.picking;
        let name = self.view.clips.get(self.view.clip).map(|c| c.name.clone()).unwrap_or_default();
        let mut root = div().relative().w(px(170.)).min_w_0().flex_shrink_1().child(
            div()
                .id("model-viewer-clip")
                .flex()
                .items_center()
                .gap(px(8.))
                .w_full()
                .h(px(32.))
                .px(px(12.))
                .border_1()
                .border_color(c(if open { 0x474747 } else { 0x333333 }))
                .hover(|s| s.border_color(c(0x474747)))
                .rounded(px(6.))
                .bg(c(0x0a0a0a))
                .text_size(px(14.))
                .cursor_pointer()
                .on_click(cx.listener(|v, _, _, cx| {
                    v.picking = !v.picking;
                    cx.notify();
                }))
                .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(name))
                .child(
                    div().w(px(10.)).h(px(6.)).flex_shrink_0().child(
                        svg().path("ui/caret-down.svg").size_full().text_color(c(0x888888)).when(open, |s| s.with_transformation(gpui::Transformation::rotate(gpui::radians(std::f32::consts::PI)))),
                    ),
                ),
        );
        if open {
            let mut list = div()
                .id("model-viewer-clips")
                .absolute()
                .bottom(px(38.))
                .left_0()
                .right_0()
                .max_h(px(260.))
                .overflow_y_scroll()
                .p(px(4.))
                .border_1()
                .border_color(c(0x2a2a2a))
                .rounded(px(8.))
                .bg(c(0x111111))
                .occlude();
            for (i, clip) in self.view.clips.iter().enumerate() {
                let selected = i == self.view.clip;
                list = list.child(
                    div()
                        .id(("model-viewer-clip-item", i))
                        .px(px(10.))
                        .py(px(7.))
                        .rounded(px(5.))
                        .text_size(px(14.))
                        .text_color(c(if selected { ACCENT } else { 0xffffff }))
                        .hover(|d| d.bg(c(0x1c1c1c)))
                        .cursor_pointer()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .on_click(cx.listener(move |v, _, _, cx| {
                            v.view.set_clip(i);
                            v.picking = false;
                            cx.notify();
                        }))
                        .child(clip.name.clone()),
                );
            }
            root = root.child(list);
        }
        root.into_any_element()
    }
}

fn round_button(id: &'static str) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .size(px(40.))
        .rounded_full()
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .bg(gpui::rgba(0x17171770))
        .hover(|s| s.bg(c(0x171717)))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
}

impl Render for ModelViewer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.view.set_size(f32::from(window.viewport_size().width), f32::from(window.viewport_size().height), window.scale_factor());
        let moving = self.view.tick(window.is_window_active());
        if moving {
            window.request_animation_frame();
        }
        for old in self.view.retired.drain(..) {
            let _ = window.drop_image(old);
        }
        let dragging = self.view.dragging();
        let scrubbing = self.scrubbing;
        let measure = canvas(
            |_, _, _| {},
            {
                let me = cx.entity().downgrade();
                move |_, _, window, _| {
                    if !(dragging || scrubbing) {
                        return;
                    }
                    let m = me.clone();
                    window.on_mouse_event(move |e: &MouseMoveEvent, phase, _, cx| {
                        if phase.bubble() {
                            m.update(cx, |v, cx| {
                                if v.scrubbing {
                                    v.scrub_to(f32::from(e.position.x));
                                } else {
                                    v.view.moved(f32::from(e.position.x), f32::from(e.position.y));
                                }
                                cx.notify();
                            })
                            .ok();
                        }
                    });
                    let m = me.clone();
                    window.on_mouse_event(move |e: &MouseUpEvent, phase, _, cx| {
                        if phase.bubble() && e.button == MouseButton::Left {
                            m.update(cx, |v, cx| {
                                v.scrubbing = false;
                                v.view.up();
                                cx.notify();
                            })
                            .ok();
                        }
                    });
                }
            },
        )
        .absolute()
        .inset_0();

        let note = |text: String, color| div().absolute().inset_0().flex().items_center().justify_center().text_size(px(14.)).text_color(color).child(text);
        let mut root = div()
            .id("model-viewer")
            .track_focus(&self.focus)
            .absolute()
            .inset_0()
            .bg(gpui::black().opacity(0.95))
            .font_family(INTER)
            .occlude()
            .cursor(if dragging { gpui::CursorStyle::ClosedHand } else { gpui::CursorStyle::OpenHand })
            .on_key_down(cx.listener(|v, e: &KeyDownEvent, _, cx| {
                if e.keystroke.key == "escape" {
                    v.close(cx);
                }
                cx.stop_propagation();
            }))
            .on_scroll(cx.listener(|v, e: &ScrollEvent, _, cx| {
                cx.stop_propagation();
                v.view.wheel(wheel_y(e));
                cx.notify();
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|v, e: &MouseDownEvent, _, cx| {
                    v.picking = false;
                    if e.click_count >= 2 {
                        v.view.reset();
                    } else {
                        v.view.down(f32::from(e.position.x), f32::from(e.position.y));
                    }
                    cx.notify();
                }),
            )
            .child(measure);
        if let Some(frame) = self.view.frame.clone() {
            root = root.child(img(frame).absolute().inset_0().size_full());
        }
        match &self.view.phase {
            Phase::Loading => root = root.child(note("Loading model…".into(), ca(255, 255, 255, 0.55))),
            Phase::Failed(t) => root = root.child(note(t.clone(), ca(255, 140, 117, 0.9))),
            Phase::Unavailable => root = root.child(note("The 3D viewer couldn't load.".into(), ca(255, 140, 117, 0.9))),
            Phase::Ready => {}
        }
        root = root.child(
            div()
                .absolute()
                .top(px(28.))
                .left(px(24.))
                .right(px(90.))
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .text_size(px(14.))
                .text_color(ca(255, 255, 255, 0.7))
                .child(self.name.clone()),
        );
        root = root.child(
            round_button("model-viewer-close")
                .absolute()
                .top(px(20.))
                .right(px(20.))
                .on_click(cx.listener(|v, _, _, cx| v.close(cx)))
                .child(svg().path("ui/viewer-close.svg").size(px(20.)).text_color(c(0xffffff))),
        );
        let mut controls = div().absolute().top(px(70.)).right(px(20.)).flex().flex_col().gap(px(10.));
        if let Some(f) = self.file {
            controls = controls.child(
                round_button("model-viewer-file")
                    .p(px(9.))
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(Event::File)))
                    .child(svg().path(if f == FileAction::Reveal { "ui/mv-reveal.svg" } else { "ui/mv-save.svg" }).size_full().text_color(c(0xffffff))),
            );
        }
        controls = controls.child(
            round_button("model-viewer-reset")
                .p(px(9.))
                .on_click(cx.listener(|v, _, _, cx| {
                    v.view.reset();
                    cx.notify();
                }))
                .child(svg().path("ui/mv-reset.svg").size_full().text_color(c(0xffffff))),
        );
        root = root.child(controls);
        if !self.view.clips.is_empty() && self.view.ready() {
            root = root.child(self.anim_bar(cx));
        }
        root.with_animation("model-viewer-fade", gpui::Animation::new(Duration::from_millis(200)), |d, t| d.opacity(t))
    }
}
