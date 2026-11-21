//! A DM's wallpaper (`WallpaperLayer.svelte`, `applyChatWallpaper`): the picture covers the
//! chat pane behind everything, blurred and dimmed as its sender chose. The filter is baked
//! into one image at the pane's size off the UI thread, newest request first, so a slider
//! drag never queues stale work.

use crate::kit::Tipped as _;
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
    /// The chats' bakes before it, newest last, so going back to one paints on the first frame.
    recent: Vec<(Key, Arc<RenderImage>)>,
    running: Option<Key>,
    queued: Option<Key>,
    /// The last two pictures decoded, so going back to a chat paints at once.
    decoded: Vec<((PathBuf, u64), Arc<image::RgbaImage>)>,
    /// The slider being dragged: blur when true.
    pub dragging: Rc<Cell<Option<bool>>>,
    /// A picture that would not decode, not retried every frame.
    failed: Option<Key>,
    /// The pane size last asked for, and since when: a resize rebakes once it settles.
    sized: Option<((u32, u32), Instant)>,
    /// A wake is set for when the resize settles.
    pub waking: bool,
}

/// How long the pane must hold a size before the picture is baked again for it.
pub const SETTLE: Duration = Duration::from_millis(180);

/// Bakes kept besides the one shown: each is a pane-sized bitmap.
const RECENT: usize = 3;

/// The work to start: bake `key` from its picture, decoded already or not.
pub struct Job {
    pub key: Key,
    pub decoded: Option<Arc<image::RgbaImage>>,
}

impl Wallpaper {
    /// The image to paint for `want`: the one baked for it, or the last while it bakes.
    pub fn image(&self, want: Option<&Key>) -> Option<Arc<RenderImage>> {
        let want = want?;
        let same = |k: &Key| k.path == want.path && k.ts == want.ts;
        self.shown
            .as_ref()
            .filter(|(k, _)| same(k))
            .or_else(|| self.recent.iter().rev().find(|(k, _)| same(k)))
            .map(|(_, i)| i.clone())
    }

    /// Ask for `want`; returns work to start when nothing is baking.
    pub fn want(&mut self, want: Option<Key>) -> Option<Job> {
        let Some(want) = want else {
            self.keep_shown();
            self.queued = None;
            return None;
        };
        if let Some(i) = self.recent.iter().position(|(k, _)| *k == want) {
            let back = self.recent.remove(i);
            self.keep_shown();
            self.shown = Some(back);
            self.queued = None;
            return None;
        }
        if self.shown.as_ref().is_some_and(|(k, _)| *k == want) || self.running.as_ref() == Some(&want) || self.failed.as_ref() == Some(&want) {
            self.queued = None;
            return None;
        }
        let resized = self.shown.as_ref().is_some_and(|(k, _)| {
            k.path == want.path && k.ts == want.ts && k.blur == want.blur && k.dim == want.dim
        });
        let now = Instant::now();
        let since = match self.sized {
            Some((size, at)) if size == want.size => at,
            _ => {
                self.sized = Some((want.size, now));
                now
            }
        };
        if resized && now - since < SETTLE {
            return None;
        }
        if self.running.is_some() {
            self.queued = Some(want);
            return None;
        }
        Some(self.start(want))
    }

    /// When a resize settles enough to rebake, if one is waiting.
    pub fn settles_in(&self) -> Option<Duration> {
        let (_, at) = self.sized?;
        let left = SETTLE.checked_sub(at.elapsed())?;
        (!left.is_zero()).then_some(left)
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
        match image {
            Some(image) => {
                if self.shown.as_ref().is_some_and(|(k, _)| k.path != key.path || k.ts != key.ts) {
                    self.keep_shown();
                }
                self.shown = Some((key, image));
            }
            None => self.failed = Some(key),
        }
        let next = self.queued.take()?;
        Some(self.start(next))
    }

    /// The shown bake moves to the recent ones, replacing an older bake of the same picture.
    fn keep_shown(&mut self) {
        let Some((key, image)) = self.shown.take() else { return };
        self.recent.retain(|(k, _)| k.path != key.path || k.ts != key.ts);
        self.recent.push((key, image));
        if self.recent.len() > RECENT {
            self.recent.remove(0);
        }
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

/// How far below the pane's size a bake can sit: blurred, a picture the GPU stretches back up
/// looks the same, at a fraction of the blur, dim and upload.
fn bake_scale(blur: u8) -> f32 {
    match blur {
        0 => 1.,
        1..=3 => 0.5,
        _ => 0.25,
    }
}

/// Decode, cover the pane (scaled 2% so the blurred edge falls outside), blur and dim.
pub fn bake(job: Job) -> (Key, Option<Arc<image::RgbaImage>>, Option<Arc<RenderImage>>) {
    let _trim = crate::trim::OnDrop;
    let Job { key, decoded } = job;
    let (pw, ph) = (key.size.0.max(1), key.size.1.max(1));
    let src = match decoded {
        Some(d) => Some(d),
        None => match crate::pic::read(&key.path).map_err(|e| e.to_string()).and_then(|b| decode(&b, pw, ph)) {
            Ok(i) => Some(Arc::new(i)),
            Err(e) => {
                vector_core::log_warn!("[Wallpaper] {} unreadable: {e}", key.path.display());
                None
            }
        },
    };
    let Some(src) = src else { return (key, None, None) };
    let f = bake_scale(key.blur);
    let (w, h) = (((pw as f32 * f).round() as u32).max(1), ((ph as f32 * f).round() as u32).max(1));
    let (sw, sh) = (src.width() as f32, src.height() as f32);
    let scale = (w as f32 / sw).max(h as f32 / sh) * 1.02;
    let (cw, ch) = ((w as f32 / scale).min(sw), (h as f32 / scale).min(sh));
    let (cx, cy) = (((sw - cw) / 2.) as u32, ((sh - ch) / 2.) as u32);
    let crop = image::imageops::crop_imm(&*src, cx, cy, cw.round() as u32, ch.round() as u32).to_image();
    let mut out = gpui::resample(&crop, w, h, image::imageops::FilterType::Triangle);
    if key.blur > 0 {
        out = image::imageops::fast_blur(&out, key.blur as f32 * f);
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

/// The picture as kept for re-bakes: a photo far larger than the `w`×`h` pane it covers keeps
/// only what a cover of that pane can show, and a JPEG decodes straight to that size.
fn decode(bytes: &[u8], w: u32, h: u32) -> Result<image::RgbaImage, String> {
    let keep = |sw: u32, sh: u32| (w as f32 / sw as f32).max(h as f32 / sh as f32) * 1.5;
    if image::guess_format(bytes).ok() == Some(image::ImageFormat::Jpeg) {
        let need = |sw: u32, sh: u32| {
            let k = keep(sw, sh);
            if k >= 1. {
                return Some(((sw, sh), None));
            }
            let t = (((sw as f32 * k).round() as u32).max(1), ((sh as f32 * k).round() as u32).max(1));
            Some((t, Some(t)))
        };
        if let Some((img, _, _)) = gpui::jpeg_into(bytes, need, |_, _, w, h| (0, 0, w, h), image::imageops::FilterType::Triangle, false) {
            return Ok(img);
        }
    }
    let img = image::load_from_memory(bytes).map_err(|e| e.to_string())?.to_rgba8();
    let k = keep(img.width(), img.height());
    if k >= 1. {
        return Ok(img);
    }
    let (nw, nh) = (((img.width() as f32 * k).round() as u32).max(1), ((img.height() as f32 * k).round() as u32).max(1));
    Ok(gpui::resample(&img, nw, nh, image::imageops::FilterType::Triangle))
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
    let side = |id: &'static str, icon: &'static str, sz: f32, tint: gpui::Hsla, text: gpui::SharedString, act: fn() -> Act| {
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
        .child(side("wallpaper-edit-cancel-btn", "icons/edit-x.svg", 14., c(crate::timeline::DANGER), crate::t::dialog_cancel(), || Act::Cancel))
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
        .child(side("wallpaper-edit-save-btn", "icons/save.svg", 16., c(0x33DB98), crate::t::dialog_save(), || Act::Save))
}

/// `.wallpaper-preview-bar`: blur and brightness, over the composer.
pub fn preview_bar(blur: u8, dim: u8, dragging: Rc<Cell<Option<bool>>>, on: &OnAct) -> impl IntoElement {
    let row = |is_blur: bool, icon: &'static str, value: u8, (lo, hi): (u8, u8)| {
        let (on, dragging) = (on.clone(), dragging.clone());
        let pct = ((value.saturating_sub(lo)) as f32 / (hi - lo) as f32).clamp(0., 1.);
        div()
            .id(if is_blur { "wp-blur" } else { "wp-dim" })
            .as_slider(
                if is_blur { crate::t::wallpaper_blur() } else { crate::t::wallpaper_brightness() },
                if is_blur { value.to_string() } else { format!("{value}%") },
                pct,
            )
            .slider_keys(pct, if is_blur { 1. } else { 5. } / (hi - lo) as f32, {
                let on = on.clone();
                move |f, w, cx| on(Act::Slide(is_blur, (lo as f32 + f * (hi - lo) as f32).round() as u8), w, cx)
            })
            .flex()
            .items_center()
            .gap(px(12.))
            .h(px(20.))
            .child(
                div()
                    .id(if is_blur { "wp-blur-icon" } else { "wp-dim-icon" })
                    .flex_shrink_0()
                    .tip(if is_blur { crate::t::wallpaper_blur() } else { crate::t::wallpaper_brightness() })
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
                        // Mirrored, the track fills from its right end.
                        let rtl = gpui::layout_rtl();
                        if pct > 0. {
                            let lit_x = if rtl { x + w * (1. - pct) } else { x };
                            let lit = Bounds::new(point(px(lit_x), track.origin.y), size(px(w * pct), px(10.)));
                            let (start, end) = (px(5.), px(if pct >= 1. { 5. } else { 0. }));
                            let (left, right) = if rtl { (end, start) } else { (start, end) };
                            window.paint_quad(fill(lit, c(PRIMARY)).corner_radii(Corners {
                                top_left: left,
                                bottom_left: left,
                                top_right: right,
                                bottom_right: right,
                            }));
                        }
                        let along = if rtl { 1. - pct } else { pct };
                        let cx_ = x + 7. + along * (w - 14.);
                        let shadow = Bounds::new(point(px(cx_ - 8.), px(mid - 6.)), size(px(16.), px(16.)));
                        window.paint_quad(fill(shadow, gpui::black().opacity(0.12)).corner_radii(Corners::all(px(8.))));
                        let thumb = Bounds::new(point(px(cx_ - 7.), px(mid - 7.)), size(px(14.), px(14.)));
                        window.paint_quad(fill(thumb, gpui::white()).corner_radii(Corners::all(px(7.))));
                        window.set_cursor_style(CursorStyle::PointingHand, &hitbox);
                        let value_at = move |px_x: Pixels| {
                            let t = crate::kit::track_frac(b, px_x, px(7.));
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

#[cfg(test)]
mod tests {
    use super::*;

    fn key(name: &str) -> Key {
        Key { path: PathBuf::from(name), ts: 1, blur: 5, dim: 50, size: (100, 80) }
    }

    fn pic() -> Arc<RenderImage> {
        Arc::new(RenderImage::new(vec![image::Frame::new(image::RgbaImage::new(1, 1))]))
    }

    fn bake_now(w: &mut Wallpaper, k: &Key) {
        let job = w.want(Some(k.clone())).expect("a bake starts");
        assert!(w.baked(job.key, None, Some(pic())).is_none());
    }

    #[test]
    fn going_back_to_a_chat_paints_its_bake_at_once() {
        let mut w = Wallpaper::default();
        let (a, b) = (key("a.jpg"), key("b.jpg"));
        bake_now(&mut w, &a);
        bake_now(&mut w, &b);
        assert!(w.want(Some(a.clone())).is_none(), "no rebake");
        assert!(w.image(Some(&a)).is_some());
        assert!(w.want(None).is_none());
        assert!(w.image(Some(&a)).is_some(), "a chat with no wallpaper keeps the others");
        assert!(w.want(Some(b.clone())).is_none());
        assert!(w.image(Some(&b)).is_some());
    }

    fn photo(w: u32, h: u32) -> PathBuf {
        let img = image::RgbImage::from_fn(w, h, |x, y| image::Rgb([(x % 251) as u8, (y % 241) as u8, ((x ^ y) % 256) as u8]));
        let path = std::env::temp_dir().join(format!("vector-wallpaper-{w}x{h}-{}.jpg", std::process::id()));
        img.save(&path).unwrap();
        path
    }

    #[test]
    fn a_blurred_bake_sits_below_the_pane_and_covers_it() {
        let path = photo(1600, 1200);
        for (blur, side) in [(0u8, (700u32, 450u32)), (2, (350, 225)), (5, (175, 113))] {
            let key = Key { path: path.clone(), ts: 1, blur, dim: 50, size: (700, 450) };
            let (_, decoded, image) = bake(Job { key, decoded: None });
            let image = image.expect("baked");
            let size = image.size(0);
            assert_eq!((size.width.0 as u32, size.height.0 as u32), side, "blur {blur}");
            let d = decoded.unwrap();
            assert!(d.width() < 1600 && d.width() as f32 >= 700. * 1.5 - 2., "kept {}x{}", d.width(), d.height());
        }
        let _ = std::fs::remove_file(path);
    }

    #[test]
    #[ignore = "timing; run with --ignored --nocapture"]
    fn bake_time() {
        let path = photo(4000, 3000);
        for blur in [0u8, 5, 20] {
            let key = Key { path: path.clone(), ts: 1, blur, dim: 50, size: (1400, 900) };
            let t = Instant::now();
            let (_, decoded, _) = bake(Job { key: key.clone(), decoded: None });
            let first = t.elapsed();
            let t = Instant::now();
            let _ = bake(Job { key, decoded });
            eprintln!("blur {blur}: first {first:?}, rebake {:?}", t.elapsed());
        }
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn only_a_few_bakes_are_kept() {
        let mut w = Wallpaper::default();
        let keys: Vec<Key> = (0..RECENT + 2).map(|i| key(&format!("{i}.jpg"))).collect();
        for k in &keys {
            bake_now(&mut w, k);
        }
        assert!(w.image(Some(&keys[0])).is_none());
        for k in &keys[1..] {
            assert!(w.image(Some(k)).is_some());
        }
    }
}
