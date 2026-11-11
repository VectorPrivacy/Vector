//! `ImageViewer`: a picture full screen, wheel-zoomed about the cursor, dragged to pan and
//! turned in quarter steps. It opens on the render already on screen and swaps in a full
//! decode when there is a better one.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::prelude::*;
use gpui::{
    AnimationExt, Bounds, Context, EventEmitter, FocusHandle, KeyDownEvent, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, RenderImage, ScrollDelta,
    ScrollEvent, Window, canvas, div, img, point, px, svg,
};

use image::Frame;

use crate::theme::{INTER, c};

/// What opens the viewer: the picture on screen, and where a sharper one comes from.
#[derive(Clone)]
pub struct Open {
    pub image: Arc<RenderImage>,
    /// Natural size, in image pixels.
    pub w: u32,
    pub h: u32,
    pub full: Option<Full>,
}

#[derive(Clone)]
pub struct Full {
    pub hash: Option<[u8; 32]>,
    pub path: Option<PathBuf>,
    pub ext: String,
}

pub struct Closed;

/// The longest side a full decode keeps, in pixels.
const FULL_MAX: (u32, u32) = (4096, 4096);
const MIN_SCALE: f32 = 0.5;
const MAX_SCALE: f32 = 5.;

pub struct Viewer {
    focus: FocusHandle,
    image: Arc<RenderImage>,
    w: u32,
    h: u32,
    /// Quarter turns counter-clockwise.
    turns: u8,
    /// `image` turned by `turns`, drawn as is.
    shown: Arc<RenderImage>,
    scale: f32,
    /// The visual box's top-left in the container.
    t: Point<f32>,
    /// The container, as last laid out.
    area: Bounds<Pixels>,
    /// Image drag: the grab point less the translation.
    drag: Option<Point<f32>>,
    /// Backdrop press: where it began, and whether it has become a drag.
    press: Option<(Point<f32>, bool)>,
    opened: Instant,
    zoomed_at: Option<Instant>,
    settled: bool,
}

impl EventEmitter<Closed> for Viewer {}

impl Viewer {
    pub fn new(open: Open, live: bool, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        if let Some(full) = open.full.clone() {
            cx.spawn(async move |this, cx| {
                let bytes = if live {
                    let path = full
                        .path
                        .as_ref()
                        .map(|p| p.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    crate::backend::run(crate::backend::attachments::bytes(full.hash, path))
                        .await
                        .ok()
                        .flatten()
                } else {
                    let path = full.path.clone();
                    cx.background_executor()
                        .spawn(async move { path.and_then(|p| std::fs::read(p).ok()) })
                        .await
                };
                let Some(bytes) = bytes else { return };
                let ext = full.ext.clone();
                let Some((image, w, h)) = cx
                    .background_executor()
                    .spawn(async move { crate::media::decode(&bytes, &ext, FULL_MAX) })
                    .await
                else {
                    return;
                };
                this.update(cx, |v, cx| {
                    // A sharper copy of the same picture: the box on screen stays put.
                    if (w, h) == (v.w, v.h) {
                        v.image = Arc::new(image);
                        v.shown = turned(&v.image, v.turns);
                        cx.notify();
                    }
                })
                .ok();
            })
            .detach();
        }
        let tip = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            for wait in [500, 500, 2000, 300] {
                cx.background_executor()
                    .timer(Duration::from_millis(wait))
                    .await;
                if tip.update(cx, |_, cx| cx.notify()).is_err() {
                    return;
                }
            }
        })
        .detach();
        Self {
            focus,
            shown: open.image.clone(),
            image: open.image,
            w: open.w,
            h: open.h,
            turns: 0,
            scale: 1.,
            t: point(0., 0.),
            area: Bounds::default(),
            drag: None,
            press: None,
            opened: Instant::now(),
            zoomed_at: Some(Instant::now()),
            settled: false,
        }
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        cx.emit(Closed);
    }

    /// The image's box at scale 1: its natural size, no larger than 95% of the window.
    fn base(&self) -> (f32, f32) {
        let (w, h) = (self.w.max(1) as f32, self.h.max(1) as f32);
        let (mw, mh) = (
            f32::from(self.area.size.width) * 0.95,
            f32::from(self.area.size.height) * 0.95,
        );
        let s = (mw / w).min(mh / h).min(1.);
        (w * s, h * s)
    }

    /// The visual box, axes swapped when a quarter turn stands it on its side.
    fn visual(&self) -> (f32, f32) {
        let (w, h) = self.base();
        let (w, h) = if self.turns % 2 == 1 { (h, w) } else { (w, h) };
        (w * self.scale, h * self.scale)
    }

    /// `updateTransform`: at rest a fitting axis centres; zoomed it only stays on screen.
    fn clamp(&mut self) {
        let (cw, ch) = (
            f32::from(self.area.size.width),
            f32::from(self.area.size.height),
        );
        let (sw, sh) = self.visual();
        let rest = self.scale <= 1.;
        let axis = |t: f32, s: f32, c: f32| {
            if s <= c {
                if rest {
                    (c - s) / 2.
                } else {
                    t.clamp(0., c - s)
                }
            } else {
                t.clamp(c - s, 0.)
            }
        };
        self.t = point(axis(self.t.x, sw, cw), axis(self.t.y, sh, ch));
    }

    fn local(&self, p: Point<Pixels>) -> Point<f32> {
        point(
            f32::from(p.x - self.area.origin.x),
            f32::from(p.y - self.area.origin.y),
        )
    }

    fn wheel(&mut self, e: &ScrollEvent, cx: &mut Context<Self>) {
        let dy = match e.delta {
            ScrollDelta::Pixels(p) => -f32::from(p.y),
            ScrollDelta::Lines(l) => -l.y * 50.,
        };
        if dy == 0. {
            return;
        }
        let factor = (-dy * 0.0035).exp().clamp(0.87, 1.15);
        let scale = (self.scale * factor).clamp(MIN_SCALE, MAX_SCALE);
        let cur = self.local(e.position);
        let at = point(
            (cur.x - self.t.x) / self.scale,
            (cur.y - self.t.y) / self.scale,
        );
        self.scale = scale;
        self.t = point(cur.x - at.x * scale, cur.y - at.y * scale);
        self.clamp();
        self.zoomed_at = Some(Instant::now());
        let me = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(1000))
                .await;
            me.update(cx, |_, cx| cx.notify()).ok();
            cx.background_executor()
                .timer(Duration::from_millis(220))
                .await;
            me.update(cx, |_, cx| cx.notify()).ok();
        })
        .detach();
        cx.notify();
    }

    fn rotate(&mut self, cx: &mut Context<Self>) {
        self.turns = (self.turns + 1) % 4;
        self.shown = turned(&self.image, self.turns);
        self.clamp();
        cx.notify();
    }
}

/// The picture turned `turns` quarter turns counter-clockwise, frame by frame.
fn turned(image: &Arc<RenderImage>, turns: u8) -> Arc<RenderImage> {
    if turns == 0 {
        return image.clone();
    }
    let frames: Vec<Frame> = (0..image.frame_count())
        .filter_map(|i| {
            let size = image.size(i);
            let buf = image::RgbaImage::from_raw(
                size.width.0 as u32,
                size.height.0 as u32,
                image.as_bytes(i)?.to_vec(),
            )?;
            let buf = match turns {
                1 => image::imageops::rotate270(&buf),
                2 => image::imageops::rotate180(&buf),
                _ => image::imageops::rotate90(&buf),
            };
            Some(Frame::from_parts(buf, 0, 0, image.delay(i)))
        })
        .collect();
    if frames.is_empty() {
        return image.clone();
    }
    Arc::new(RenderImage::new(frames))
}

/// A fade between `from` and `to` seconds after `t0`, `dur` long at each end.
fn shown_for(t0: Instant, from: f32, to: f32, dur: f32) -> f32 {
    let e = t0.elapsed().as_secs_f32();
    if e < from || e > to + dur {
        0.
    } else if e < from + dur {
        (e - from) / dur
    } else if e > to {
        1. - (e - to) / dur
    } else {
        1.
    }
}

fn pill(text: impl Into<gpui::SharedString>, opacity: f32) -> gpui::Div {
    div()
        .absolute()
        .left_0()
        .right_0()
        .flex()
        .justify_center()
        .opacity(opacity)
        .child(
            div()
                .px(px(16.))
                .py(px(8.))
                .rounded(px(20.))
                .bg(gpui::black().opacity(0.7))
                .text_size(px(12.))
                .line_height(px(24.))
                .text_color(c(0xffffff))
                .child(text.into()),
        )
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

impl Render for Viewer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let me = cx.entity().downgrade();
        let (sw, sh) = self.visual();
        let settled = self.settled;
        let image = self.shown.clone();
        let dragging = self.drag.is_some();

        let measure = {
            let me = me.clone();
            canvas(
                move |bounds, _, cx| {
                    me.update(cx, |v, cx| {
                        if v.area != bounds {
                            v.area = bounds;
                            v.clamp();
                            v.settled = true;
                            cx.notify();
                        }
                    })
                    .ok();
                },
                |_, _, _, _| {},
            )
            .absolute()
            .inset_0()
        };

        let picture = div()
            .id("viewer-image")
            .absolute()
            .left(px(self.t.x))
            .top(px(self.t.y))
            .w(px(sw))
            .h(px(sh))
            .when(!settled, |d| d.invisible())
            .cursor(if dragging {
                gpui::CursorStyle::ClosedHand
            } else {
                gpui::CursorStyle::OpenHand
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|v, e: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    let p = v.local(e.position);
                    v.drag = Some(point(p.x - v.t.x, p.y - v.t.y));
                    cx.notify();
                }),
            )
            .child(img(image).size_full());

        let tip = shown_for(self.opened, 0.5, 3.0, 0.3);
        let zoom = self
            .zoomed_at
            .map(|t| {
                shown_for(t, 0., 1., 0.2).max(if t.elapsed().as_secs_f32() < 0.2 {
                    1.
                } else {
                    0.
                })
            })
            .unwrap_or(0.);
        let fading = tip > 0. && tip < 1. || zoom > 0. && zoom < 1.;
        if fading {
            window.request_animation_frame();
        }
        let pct = format!("{}%", (self.scale * 100.).round() as i32);

        let close = round_button("viewer-close")
            .absolute()
            .top(px(20.))
            .right(px(20.))
            .on_click(cx.listener(|v, _, _, cx| v.close(cx)))
            .child(
                svg()
                    .path("ui/viewer-close.svg")
                    .size(px(20.))
                    .text_color(c(0xffffff)),
            );
        let controls = div()
            .absolute()
            .top(px(70.))
            .right(px(20.))
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(
                round_button("viewer-rotate")
                    .p(px(9.))
                    .on_click(cx.listener(|v, _, _, cx| v.rotate(cx)))
                    .child(
                        svg()
                            .path("ui/viewer-rotate.svg")
                            .size_full()
                            .text_color(c(0xffffff)),
                    ),
            );

        div()
            .id("image-viewer")
            .track_focus(&self.focus)
            .absolute()
            .inset_0()
            .bg(gpui::black().opacity(0.95))
            .font_family(INTER)
            .cursor(gpui::CursorStyle::PointingHand)
            .on_key_down(cx.listener(|v, e: &KeyDownEvent, _, cx| {
                if e.keystroke.key == "escape" {
                    v.close(cx);
                }
                cx.stop_propagation();
            }))
            .on_scroll(cx.listener(|v, e: &ScrollEvent, _, cx| {
                cx.stop_propagation();
                v.wheel(e, cx)
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|v, e: &MouseDownEvent, _, cx| {
                    v.press = Some((v.local(e.position), false));
                    cx.stop_propagation();
                }),
            )
            .on_mouse_move(cx.listener(|v, e: &MouseMoveEvent, _, cx| {
                let p = v.local(e.position);
                if let Some(start) = v.drag {
                    v.t = point(p.x - start.x, p.y - start.y);
                    v.clamp();
                    cx.notify();
                } else if let Some((start, dragged)) = v.press.as_mut()
                    && ((p.x - start.x).powi(2) + (p.y - start.y).powi(2)).sqrt() > 5.
                {
                    *dragged = true;
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|v, _: &MouseUpEvent, _, cx| {
                    if v.drag.take().is_some() {
                        cx.notify();
                    } else if let Some((_, dragged)) = v.press.take()
                        && !dragged
                    {
                        v.close(cx);
                    }
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|v, _: &MouseUpEvent, _, cx| {
                    v.press = None;
                    if v.drag.take().is_some() {
                        cx.notify();
                    }
                }),
            )
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .overflow_hidden()
                    .child(measure)
                    .child(picture),
            )
            .child(close)
            .child(controls)
            .child(pill(pct, zoom).bottom(px(20.)))
            .child(pill("Scroll to zoom", tip).top(px(20.)))
            .with_animation(
                "viewer-fade",
                gpui::Animation::new(Duration::from_millis(200)),
                |d, t| d.opacity(t),
            )
    }
}
