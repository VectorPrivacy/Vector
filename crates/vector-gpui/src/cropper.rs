//! `Cropper`: a square (or fixed-aspect) cut of a picture, moved, resized from a corner or drawn afresh, with the
//! picture shown where it will live. It answers with a rectangle in the picture's own pixels.

use std::sync::Arc;
use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    AnimationExt, Bounds, Context, EventEmitter, FocusHandle, Hsla, KeyDownEvent, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, PathBuilder, Pixels, Point, RenderImage,
    SharedString, Window, canvas, div, img, point, px,
};
use vector_core::image_crop::CropRect;

use crate::theme::{PRIMARY, RUBIK, c};

/// Display px: below this the corner dots crowd out the middle a move grabs.
const MIN_EDGE: f32 = 36.;
/// The dots sit 7px outside the box; the picture is fitted inside this margin so they stay in.
const INSET: f32 = 10.;
const CARD_BG: u32 = 0x0b0b0b;

pub enum Answer {
    Crop(CropRect),
    Cancel,
}

impl EventEmitter<Answer> for Cropper {}

#[derive(Clone, Copy, Default, PartialEq)]
struct Rect {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

enum Gesture {
    /// The grab point less the box's corner.
    Move(Point<f32>),
    /// The corner that stays put.
    Resize(Point<f32>),
}

pub struct Cropper {
    focus: FocusHandle,
    image: Arc<RenderImage>,
    /// The picture's size, upright.
    natural: (u32, u32),
    title: SharedString,
    hint: SharedString,
    round: bool,
    /// Width over height of the cut.
    aspect: f32,
    stage: Bounds<Pixels>,
    /// The fitted picture and the box, in stage px.
    img: Rect,
    crop: Rect,
    gesture: Option<Gesture>,
}

impl Cropper {
    pub fn new(
        image: Arc<RenderImage>,
        natural: (u32, u32),
        title: impl Into<SharedString>,
        hint: impl Into<SharedString>,
        round: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        Self {
            focus,
            image,
            natural,
            title: title.into(),
            hint: hint.into(),
            round,
            aspect: 1.,
            stage: Bounds::default(),
            img: Rect::default(),
            crop: Rect::default(),
            gesture: None,
        }
    }

    /// A cut of this width over height instead of a square; never round.
    pub fn aspect(mut self, aspect: f32) -> Self {
        self.aspect = aspect.max(0.05);
        self.round = false;
        self
    }

    fn max_w(&self) -> f32 {
        self.img.w.min(self.img.h * self.aspect)
    }

    fn min_w(&self) -> f32 {
        self.max_w().min(MIN_EDGE * self.aspect.max(1.))
    }

    fn lay_out(&mut self, stage: Bounds<Pixels>) {
        self.stage = stage;
        let (sw, sh) = (f32::from(stage.size.width), f32::from(stage.size.height));
        let (nw, nh) = (self.natural.0.max(1) as f32, self.natural.1.max(1) as f32);
        let scale = ((sw - INSET * 2.) / nw).min((sh - INSET * 2.) / nh);
        let (w, h) = ((nw * scale).round(), (nh * scale).round());
        self.img = Rect {
            x: ((sw - w) / 2.).round(),
            y: ((sh - h) / 2.).round(),
            w,
            h,
        };
        let (bw, bh) = (self.max_w(), self.max_w() / self.aspect);
        self.crop = Rect {
            x: self.img.x + (w - bw) / 2.,
            y: self.img.y + (h - bh) / 2.,
            w: bw,
            h: bh,
        };
    }

    /// The box in the picture's pixels.
    fn rect(&self) -> Option<CropRect> {
        if self.img.w <= 0. {
            return None;
        }
        let (nw, nh) = self.natural;
        let k = nw as f32 / self.img.w;
        let w = ((self.crop.w * k).round() as u32).min(nw).max(1);
        let h = ((self.crop.h * k).round() as u32).min(nh).max(1);
        Some(CropRect {
            x: (((self.crop.x - self.img.x) * k).round().max(0.) as u32).min(nw - w),
            y: (((self.crop.y - self.img.y) * k).round().max(0.) as u32).min(nh - h),
            w,
            h,
            sw: Some(nw),
            sh: Some(nh),
        })
    }

    fn local(&self, p: Point<Pixels>) -> Point<f32> {
        point(
            f32::from(p.x - self.stage.origin.x),
            f32::from(p.y - self.stage.origin.y),
        )
    }

    fn inside_img(&self, p: Point<f32>) -> bool {
        p.x >= self.img.x
            && p.x <= self.img.x + self.img.w
            && p.y >= self.img.y
            && p.y <= self.img.y + self.img.h
    }

    fn drag(&mut self, p: Point<f32>) {
        let i = self.img;
        match self.gesture {
            Some(Gesture::Move(g)) => {
                self.crop.x = (p.x - g.x).clamp(i.x, i.x + i.w - self.crop.w);
                self.crop.y = (p.y - g.y).clamp(i.y, i.y + i.h - self.crop.h);
            }
            Some(Gesture::Resize(a)) => {
                let (right, down) = (p.x >= a.x, p.y >= a.y);
                // The anchor stays put; the box grows toward the pointer only as far as the picture allows.
                let room_w = if right { i.x + i.w - a.x } else { a.x - i.x };
                let room_h = if down { i.y + i.h - a.y } else { a.y - i.y };
                let k = self.aspect;
                let w = (p.x - a.x)
                    .abs()
                    .max((p.y - a.y).abs() * k)
                    .min(room_w)
                    .min(room_h * k)
                    .max(self.min_w());
                self.crop = Rect {
                    x: if right { a.x } else { a.x - w },
                    y: if down { a.y } else { a.y - w / k },
                    w,
                    h: w / k,
                };
                let w = self.crop.w.clamp(self.min_w(), self.max_w());
                let h = w / k;
                self.crop = Rect {
                    x: self.crop.x.clamp(i.x, i.x + i.w - w),
                    y: self.crop.y.clamp(i.y, i.y + i.h - h),
                    w,
                    h,
                };
            }
            None => {}
        }
    }

    fn answer(&mut self, crop: bool, cx: &mut Context<Self>) {
        match crop.then(|| self.rect()).flatten() {
            Some(r) => cx.emit(Answer::Crop(r)),
            None if !crop => cx.emit(Answer::Cancel),
            None => {}
        }
    }
}

/// `area` filled with `color` everywhere but `hole` (a circle, or a square when not `round`).
fn surround(
    area: Bounds<Pixels>,
    hole: Bounds<Pixels>,
    round: bool,
    color: Hsla,
    window: &mut Window,
) {
    let mut b = PathBuilder::fill();
    let (o, s) = (area.origin, area.size);
    b.add_polygon(
        &[
            o,
            point(o.x + s.width, o.y),
            point(o.x + s.width, o.y + s.height),
            point(o.x, o.y + s.height),
        ],
        true,
    );
    let (x, y, w, h) = (
        hole.origin.x,
        hole.origin.y,
        hole.size.width,
        hole.size.height,
    );
    if round {
        let (cx, cy, r) = (x + w / 2., y + h / 2., w / 2.);
        let k = r * 0.552_284_8;
        b.move_to(point(cx + r, cy));
        b.cubic_bezier_to(
            point(cx, cy + r),
            point(cx + r, cy + k),
            point(cx + k, cy + r),
        );
        b.cubic_bezier_to(
            point(cx - r, cy),
            point(cx - k, cy + r),
            point(cx - r, cy + k),
        );
        b.cubic_bezier_to(
            point(cx, cy - r),
            point(cx - r, cy - k),
            point(cx - k, cy - r),
        );
        b.cubic_bezier_to(
            point(cx + r, cy),
            point(cx + k, cy - r),
            point(cx + r, cy - k),
        );
        b.close();
    } else {
        b.add_polygon(
            &[
                point(x, y),
                point(x + w, y),
                point(x + w, y + h),
                point(x, y + h),
            ],
            true,
        );
    }
    if let Ok(path) = b.build() {
        window.paint_path(path, color);
    }
}

fn button(id: &'static str, label: &'static str, primary: bool) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .px(px(22.))
        .py(px(9.))
        .rounded(px(10.))
        .border_1()
        .text_size(px(13.))
        .line_height(px(15.))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .cursor_pointer()
        .map(|d| {
            if primary {
                d.bg(c(0x59fcb3).opacity(0.18))
                    .border_color(c(PRIMARY))
                    .text_color(c(PRIMARY))
                    .hover(|s| s.bg(c(0x59fcb3).opacity(0.3)))
            } else {
                d.bg(c(0x141414).opacity(0.85))
                    .border_color(gpui::white().opacity(0.14))
                    .text_color(c(0xffffff))
                    .hover(|s| s.bg(c(0x2a2a2a)).border_color(gpui::white().opacity(0.28)))
            }
        })
        .child(label)
}

impl Render for Cropper {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let me = cx.entity().downgrade();
        let (i, b, round) = (self.img, self.crop, self.round);
        let ready = i.w > 0.;

        let measure = canvas(
            move |bounds, _, cx| {
                me.update(cx, |v, cx| {
                    if v.stage.size != bounds.size {
                        v.lay_out(bounds);
                        cx.notify();
                    } else {
                        v.stage = bounds;
                    }
                })
                .ok();
            },
            move |bounds, _, window, _| {
                if ready {
                    let hole = Bounds::new(
                        bounds.origin + point(px(b.x), px(b.y)),
                        gpui::size(px(b.w), px(b.h)),
                    );
                    surround(bounds, hole, round, gpui::black().opacity(0.6), window);
                }
            },
        )
        .absolute()
        .inset_0();

        let handle = |corner: &'static str, x: bool, y: bool| {
            div()
                .id(corner)
                .absolute()
                .size(px(18.))
                .rounded_full()
                .border_2()
                .border_color(gpui::black().opacity(0.55))
                .child(div().size_full().rounded_full().bg(c(0xffffff)))
                .map(|d| if x { d.right(px(-9.)) } else { d.left(px(-9.)) })
                .map(|d| if y { d.bottom(px(-9.)) } else { d.top(px(-9.)) })
                .cursor(if x == y {
                    gpui::CursorStyle::ResizeUpLeftDownRight
                } else {
                    gpui::CursorStyle::ResizeUpRightDownLeft
                })
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |v, _: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        let c = v.crop;
                        v.gesture = Some(Gesture::Resize(point(
                            if x { c.x } else { c.x + c.w },
                            if y { c.y } else { c.y + c.h },
                        )));
                    }),
                )
        };

        let crop_box = div()
            .id("crop-box")
            .absolute()
            .left(px(b.x))
            .top(px(b.y))
            .w(px(b.w))
            .h(px(b.h))
            .border_2()
            .border_color(c(0xffffff))
            .when(round, |d| d.rounded_full())
            .cursor(gpui::CursorStyle::OpenHand)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|v, e: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    let p = v.local(e.position);
                    v.gesture = Some(Gesture::Move(point(p.x - v.crop.x, p.y - v.crop.y)));
                }),
            )
            .child(handle("crop-tl", false, false))
            .child(handle("crop-tr", true, false))
            .child(handle("crop-bl", false, true))
            .child(handle("crop-br", true, true));

        let stage = div()
            .id("crop-stage")
            .relative()
            .flex_1()
            .min_w_0()
            .h(px(320.))
            .overflow_hidden()
            .rounded(px(12.))
            .bg(c(0x050505))
            .cursor(gpui::CursorStyle::Crosshair)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|v, e: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    let p = v.local(e.position);
                    if v.inside_img(p) {
                        v.gesture = Some(Gesture::Resize(p));
                    }
                }),
            )
            .when(ready, |d| {
                d.child(
                    img(self.image.clone())
                        .absolute()
                        .left(px(i.x))
                        .top(px(i.y))
                        .w(px(i.w))
                        .h(px(i.h)),
                )
            })
            .child(measure)
            .when(ready, |d| d.child(crop_box));

        let aspect = self.aspect;
        let chip = |size: f32| {
            let size = if aspect > 1. { size * 1.25 } else { size };
            let k = if b.w > 0. { size / b.w } else { 0. };
            div()
                .relative()
                .w(px(size))
                .h(px(size / aspect))
                .overflow_hidden()
                .rounded(px(6.))
                .bg(c(0x050505))
                .when(ready, |d| {
                    d.child(
                        img(self.image.clone())
                            .absolute()
                            .left(px((i.x - b.x) * k))
                            .top(px((i.y - b.y) * k))
                            .w(px(i.w * k))
                            .h(px(i.h * k)),
                    )
                    .when(round, |d| {
                        d.child(
                            canvas(
                                |_, _, _| {},
                                |bounds, _, window, _| {
                                    surround(bounds, bounds, true, c(CARD_BG), window)
                                },
                            )
                            .absolute()
                            .inset_0(),
                        )
                    })
                })
        };

        let preview = div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .flex_none()
            .child(
                div()
                    .text_size(px(11.))
                    .line_height(px(24.))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(gpui::white().opacity(0.4))
                    .child("PREVIEW"),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(12.))
                    .w(px(120.))
                    .child(chip(96.))
                    .child(chip(48.))
                    .when(aspect <= 1., |d| d.child(chip(24.))),
            );

        let card = div()
            .id("crop-card")
            .font_family(RUBIK)
            .flex()
            .flex_col()
            .gap(px(16.))
            .w_full()
            .max_w(px(560.))
            .p(px(20.))
            .bg(c(CARD_BG))
            .border_1()
            .border_color(c(0x2c2e2f))
            .rounded(px(18.))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .child(
                        div()
                            .text_size(px(17.))
                            .line_height(px(24.))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(c(0xffffff))
                            .child(self.title.clone()),
                    )
                    .when(!self.hint.is_empty(), |d| {
                        d.child(
                            div()
                                .mt(px(4.))
                                .text_size(px(12.5))
                                .line_height(px(24.))
                                .text_color(gpui::white().opacity(0.5))
                                .child(self.hint.clone()),
                        )
                    }),
            )
            .child(div().flex().gap(px(16.)).child(stage).child(preview))
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(10.))
                    .child(
                        button("crop-cancel", "Cancel", false)
                            .on_click(cx.listener(|v, _, _, cx| v.answer(false, cx))),
                    )
                    .child(
                        button("crop-confirm", "Crop", true)
                            .when(!ready, |d| d.opacity(0.5))
                            .on_click(cx.listener(|v, _, _, cx| v.answer(true, cx))),
                    ),
            )
            .with_animation(
                "crop-pop",
                gpui::Animation::new(Duration::from_millis(160))
                    .with_easing(gpui::ease_out_quint()),
                |d, t| d.opacity(t),
            );

        div()
            .id("crop-overlay")
            .track_focus(&self.focus)
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .p(px(26.))
            .bg(gpui::black().opacity(0.82))
            .on_key_down(cx.listener(|v, e: &KeyDownEvent, _, cx| {
                match e.keystroke.key.as_str() {
                    "escape" => v.answer(false, cx),
                    "enter" => v.answer(true, cx),
                    _ => return,
                }
                cx.stop_propagation();
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|v, _, _, cx| v.answer(false, cx)),
            )
            .on_mouse_move(cx.listener(|v, e: &MouseMoveEvent, _, cx| {
                if v.gesture.is_some() {
                    let p = v.local(e.position);
                    v.drag(p);
                    cx.notify();
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|v, _: &MouseUpEvent, _, _| v.gesture = None),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|v, _: &MouseUpEvent, _, _| v.gesture = None),
            )
            .child(card)
    }
}
