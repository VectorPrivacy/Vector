//! A DM's wallpaper (`WallpaperLayer.svelte`, `applyChatWallpaper`): the picture covers the
//! chat pane behind everything, blurred and dimmed as its sender chose. The filter is baked
//! into one image at the pane's size off the UI thread, newest request first, so a slider
//! drag never queues stale work.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::RenderImage;
use gpui::{
    Bounds, Corners, CursorStyle, InteractiveElement, IntoElement, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement, Pixels, StatefulInteractiveElement,
    Styled, canvas, div, fill, point, prelude::*, px, size, svg,
};
use std::cell::Cell;
use std::rc::Rc;

use crate::theme::*;

pub const DEFAULT_BLUR: u8 = 5;
pub const DEFAULT_DIM: u8 = 50;

/// What the layer should show.
#[derive(Clone, PartialEq, Debug)]
pub struct Key {
    pub path: PathBuf,
    pub ts: u64,
    pub blur: u8,
    pub dim: u8,
    pub size: (u32, u32),
}

/// A picked image staged for the open chat, before Save.
pub struct Preview {
    pub chat: String,
    pub path: PathBuf,
    pub blur: u8,
    pub dim: u8,
    pub ts: u64,
}

#[derive(Default)]
pub struct Wallpaper {
    pub preview: Option<Preview>,
    pub busy: bool,
    pub label: String,
    /// The edit bar's fade: whether it is coming in, and since when.
    pub edit: Option<(bool, Instant)>,
    shown: Option<(Key, Arc<RenderImage>)>,
    running: Option<Key>,
    queued: Option<Key>,
    /// The last two pictures decoded, so going back to a chat paints at once.
    decoded: Vec<((PathBuf, u64), Arc<image::RgbaImage>)>,
    /// The slider being dragged: blur when true.
    pub dragging: Rc<Cell<Option<bool>>>,
}

/// The work to start: bake `key` from its picture, decoded already or not.
pub struct Job {
    pub key: Key,
    pub decoded: Option<Arc<image::RgbaImage>>,
}

impl Wallpaper {
    /// The image to paint for `want`: the one baked for it, or the last while it bakes.
    pub fn image(&self, want: Option<&Key>) -> Option<Arc<RenderImage>> {
        let want = want?;
        self.shown
            .as_ref()
            .filter(|(k, _)| k.path == want.path && k.ts == want.ts)
            .map(|(_, i)| i.clone())
    }

    /// Ask for `want`; returns work to start when nothing is baking.
    pub fn want(&mut self, want: Option<Key>) -> Option<Job> {
        let Some(want) = want else {
            self.shown = None;
            self.queued = None;
            return None;
        };
        if self.shown.as_ref().is_some_and(|(k, _)| *k == want) || self.running.as_ref() == Some(&want) {
            self.queued = None;
            return None;
        }
        if self.running.is_some() {
            self.queued = Some(want);
            return None;
        }
        Some(self.start(want))
    }

    fn start(&mut self, key: Key) -> Job {
        self.running = Some(key.clone());
        let decoded = self
            .decoded
            .iter()
            .find(|(k, _)| k.0 == key.path && k.1 == key.ts)
            .map(|(_, d)| d.clone());
        Job { key, decoded }
    }

    /// A bake finished; returns the next to start, if one was asked for meanwhile.
    pub fn baked(&mut self, key: Key, decoded: Option<Arc<image::RgbaImage>>, image: Option<Arc<RenderImage>>) -> Option<Job> {
        self.running = None;
        if let Some(d) = decoded {
            let id = (key.path.clone(), key.ts);
            self.decoded.retain(|(k, _)| *k != id);
            self.decoded.push((id, d));
            if self.decoded.len() > 2 {
                self.decoded.remove(0);
            }
        }
        if let Some(image) = image {
            self.shown = Some((key, image));
        }
        let next = self.queued.take()?;
        Some(self.start(next))
    }

    pub fn previewing(&self) -> bool {
        self.preview.is_some()
    }

    pub fn set_editing(&mut self, on: bool) {
        self.edit = Some((on, Instant::now()));
    }

    /// The edit bar's opacity, and whether it is still in the layout.
    pub fn edit_alpha(&self) -> Option<f32> {
        let (on, since) = self.edit?;
        let t = (since.elapsed().as_secs_f32() / 0.25).min(1.);
        if on {
            Some(t)
        } else if t < 1. {
            Some(1. - t)
        } else {
            None
        }
    }

    pub fn moving(&self) -> bool {
        self.edit.is_some_and(|(_, s)| s.elapsed() < Duration::from_millis(260))
    }
}

/// Decode, cover the pane (scaled 2% so the blurred edge falls outside), blur and dim.
pub fn bake(job: Job) -> (Key, Option<Arc<image::RgbaImage>>, Option<Arc<RenderImage>>) {
    let _trim = crate::trim::OnDrop;
    let Job { key, decoded } = job;
    let (w, h) = (key.size.0.max(1), key.size.1.max(1));
    let src = match decoded {
        Some(d) => Some(d),
        None => image::open(&key.path).ok().map(|i| Arc::new(shrink(i.to_rgba8(), w, h))),
    };
    let Some(src) = src else { return (key, None, None) };
    let (sw, sh) = (src.width() as f32, src.height() as f32);
    let scale = (w as f32 / sw).max(h as f32 / sh) * 1.02;
    let (cw, ch) = ((w as f32 / scale).min(sw), (h as f32 / scale).min(sh));
    let (cx, cy) = (((sw - cw) / 2.) as u32, ((sh - ch) / 2.) as u32);
    let crop = image::imageops::crop_imm(&*src, cx, cy, cw.round() as u32, ch.round() as u32).to_image();
    let mut out = image::imageops::resize(&crop, w, h, image::imageops::FilterType::Triangle);
    if key.blur > 0 {
        out = image::imageops::fast_blur(&out, key.blur as f32);
    }
    let k = key.dim.clamp(0, 100) as u32;
    for p in out.pixels_mut() {
        for c in &mut p.0[..3] {
            *c = (*c as u32 * k / 100) as u8;
        }
    }
    let frame = image::Frame::new(crate::media::to_bgra(out));
    let image = Arc::new(RenderImage::new(vec![frame]));
    (key, Some(src), Some(image))
}

/// The decoded picture is kept for re-bakes; a photo far larger than the pane it covers
/// keeps only what a blurred cover of that pane can show.
fn shrink(img: image::RgbaImage, w: u32, h: u32) -> image::RgbaImage {
    let (sw, sh) = (img.width() as f32, img.height() as f32);
    let keep = (w as f32 / sw).max(h as f32 / sh) * 1.5;
    if keep >= 1. {
        return img;
    }
    let (nw, nh) = ((sw * keep).round().max(1.) as u32, (sh * keep).round().max(1.) as u32);
    image::imageops::resize(&img, nw, nh, image::imageops::FilterType::Triangle)
}

pub type OnAct = Rc<dyn Fn(Act, &mut gpui::Window, &mut gpui::App)>;

pub enum Act {
    Cancel,
    Save,
    /// A slider moved: blur when true, else brightness.
    Slide(bool, u8),
}

/// `#wallpaper-edit-bar`: over the header while a picture is staged.
pub fn edit_bar(label: &str, busy: bool, alpha: f32, on: &OnAct) -> impl IntoElement {
    let side = |id: &'static str, icon: &'static str, sz: f32, tint: gpui::Hsla, text: &'static str, act: fn() -> Act| {
        let on = on.clone();
        div()
            .id(id)
            .flex()
            .items_center()
            .gap(px(6.))
            .when(busy, |d| d.opacity(0.5))
            .when(!busy, |d| {
                d.cursor_pointer().hover(|s| s.opacity(0.75)).on_click(move |_, w, cx| {
                    cx.stop_propagation();
                    on(act(), w, cx)
                })
            })
            .child(svg().path(icon).size(px(sz)).text_color(tint))
            .child(div().text_size(px(16.)).line_height(px(24.)).child(text))
    };
    div()
        .id("wallpaper-edit-bar")
        .occlude()
        .absolute()
        .top_0()
        .left_0()
        .right_0()
        .h(px(60.))
        .px(px(20.))
        .flex()
        .items_center()
        .justify_between()
        .bg(gpui::rgb(0x161616))
        .opacity(alpha)
        .child(side("wallpaper-edit-cancel-btn", "icons/edit-x.svg", 14., c(crate::timeline::DANGER), "Cancel", || Act::Cancel))
        .child(
            div()
                .absolute()
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .text_size(px(12.))
                .line_height(px(24.))
                .text_color(white_a(0.6))
                .child(label.to_string()),
        )
        .child(side("wallpaper-edit-save-btn", "icons/save.svg", 16., c(0x33DB98), "Save", || Act::Save))
}

/// `.wallpaper-preview-bar`: blur and brightness, over the composer.
pub fn preview_bar(blur: u8, dim: u8, dragging: Rc<Cell<Option<bool>>>, on: &OnAct) -> impl IntoElement {
    let row = |is_blur: bool, icon: &'static str, value: u8, (lo, hi): (u8, u8)| {
        let (on, dragging) = (on.clone(), dragging.clone());
        let pct = ((value.saturating_sub(lo)) as f32 / (hi - lo) as f32).clamp(0., 1.);
        div()
            .flex()
            .items_center()
            .gap(px(12.))
            .h(px(20.))
            .child(
                div()
                    .id(if is_blur { "wp-blur-icon" } else { "wp-dim-icon" })
                    .flex_shrink_0()
                    .tooltip(crate::kit::tooltip(if is_blur { "Blur" } else { "Brightness" }))
                    .child(svg().path(icon).size(px(20.)).text_color(white_a(0.72))),
            )
            .child(
                canvas(
                    move |b, window, _| window.insert_hitbox(b, gpui::HitboxBehavior::Normal),
                    move |b: Bounds<Pixels>, hitbox, window, _| {
                        let (x, w) = (f32::from(b.origin.x), f32::from(b.size.width));
                        let mid = f32::from(b.origin.y) + f32::from(b.size.height) / 2.;
                        let track = Bounds::new(point(px(x), px(mid - 5.)), size(px(w), px(10.)));
                        window.paint_quad(fill(track, white_a(0.08)).corner_radii(Corners::all(px(5.))));
                        if pct > 0. {
                            let lit = Bounds::new(track.origin, size(px(w * pct), px(10.)));
                            window.paint_quad(fill(lit, c(PRIMARY)).corner_radii(Corners {
                                top_left: px(5.),
                                bottom_left: px(5.),
                                top_right: px(if pct >= 1. { 5. } else { 0. }),
                                bottom_right: px(if pct >= 1. { 5. } else { 0. }),
                            }));
                        }
                        let cx_ = x + 7. + pct * (w - 14.);
                        let shadow = Bounds::new(point(px(cx_ - 8.), px(mid - 6.)), size(px(16.), px(16.)));
                        window.paint_quad(fill(shadow, gpui::black().opacity(0.12)).corner_radii(Corners::all(px(8.))));
                        let thumb = Bounds::new(point(px(cx_ - 7.), px(mid - 7.)), size(px(14.), px(14.)));
                        window.paint_quad(fill(thumb, gpui::white()).corner_radii(Corners::all(px(7.))));
                        window.set_cursor_style(CursorStyle::PointingHand, &hitbox);
                        let value_at = move |px_x: Pixels| {
                            let t = ((f32::from(px_x) - x - 7.) / (w - 14.).max(1.)).clamp(0., 1.);
                            (lo as f32 + t * (hi - lo) as f32).round() as u8
                        };
                        {
                            let (on, dragging, hitbox) = (on.clone(), dragging.clone(), hitbox.clone());
                            window.on_mouse_event(move |e: &MouseDownEvent, phase, window, cx| {
                                if phase.bubble() && e.button == MouseButton::Left && hitbox.is_hovered(window) {
                                    dragging.set(Some(is_blur));
                                    cx.stop_propagation();
                                    on(Act::Slide(is_blur, value_at(e.position.x)), window, cx);
                                }
                            });
                        }
                        {
                            let (on, dragging) = (on.clone(), dragging.clone());
                            window.on_mouse_event(move |e: &MouseMoveEvent, phase, window, cx| {
                                if phase.bubble() && dragging.get() == Some(is_blur) && e.dragging() {
                                    on(Act::Slide(is_blur, value_at(e.position.x)), window, cx);
                                }
                            });
                        }
                        let dragging = dragging.clone();
                        window.on_mouse_event(move |_: &MouseUpEvent, phase, _, _| {
                            if phase.bubble() && dragging.get() == Some(is_blur) {
                                dragging.set(None);
                            }
                        });
                    },
                )
                .flex_1()
                .h(px(16.)),
            )
    };
    div()
        .id("wallpaper-preview-bar")
        .occlude()
        .flex_shrink_0()
        .h(px(81.))
        .px(px(8.))
        .py(px(14.))
        .flex()
        .items_center()
        .bg(gpui::rgb(0x171717))
        .border_t_1()
        .border_l_1()
        .border_r_1()
        .border_color(gpui::rgb(0x474747))
        .rounded_t(px(12.))
        .shadow(vec![gpui::BoxShadow {
            color: gpui::black().opacity(0.5),
            offset: point(px(0.), px(-8.)),
            blur_radius: px(32.),
            spread_radius: px(0.),
            inset: false,
        }])
        .child(
            div()
                .flex_1()
                .min_w_0()
                .px(px(12.))
                .flex()
                .flex_col()
                .gap(px(12.))
                .child(row(true, "icons/eye-off.svg", blur, (0, 30)))
                .child(row(false, "icons/bulb.svg", dim, (10, 100))),
        )
}
