//! Files waiting in the composer to go with the next message: the tray of tiles above the
//! input, each opened in the file preview to rename it or mark it as a spoiler.

use std::sync::Arc;
use std::time::Instant;

use gpui::prelude::*;
use gpui::{AnyElement, Context, Entity, ImageSource, ObjectFit, RenderImage, SharedString, div, px, svg};

use crate::filepreview::FilePreview;
use crate::kit::Tipped as _;
use crate::t;
use crate::theme::{c, white_a};

/// A tile's side, and the tray's height around a row of them.
pub const TILE: f32 = 64.;
const GAP: f32 = 8.;
pub const TRAY: f32 = TILE + 2. * GAP;
const IN_SECS: f32 = 0.22;
const OUT_SECS: f32 = 0.18;
/// Thumbnails are held at 2x the tile, so they stay sharp on a Retina screen.
const THUMB_PX: u32 = (TILE * 2.) as u32;

#[derive(Clone)]
pub struct Staged {
    pub id: u64,
    pub preview: Entity<FilePreview>,
    /// The tile's picture: a photo or GIF shrunk to the tile, a video's first frame.
    pub thumb: Option<Arc<RenderImage>>,
    /// Its blur, drawn while it is a spoiler.
    pub blur: Option<Arc<RenderImage>>,
    pub video: bool,
    pub added: Instant,
    pub leaving: Option<Instant>,
}

impl Staged {
    pub fn live(&self) -> bool {
        self.leaving.is_none()
    }

    /// 0 → 1 as it arrives, and back to 0 as it leaves.
    fn shown(&self, now: Instant) -> f32 {
        let curve = crate::kit::bezier(0.22, 1., 0.36, 1.);
        match self.leaving {
            Some(at) => 1. - curve(((now - at).as_secs_f32() / OUT_SECS).min(1.)),
            None => curve(((now - self.added).as_secs_f32() / IN_SECS).min(1.)),
        }
    }

    /// Gone from view: the tray may drop it.
    pub fn spent(&self, now: Instant) -> bool {
        self.leaving.is_some_and(|at| (now - at).as_secs_f32() >= OUT_SECS)
    }
}

/// The tile's picture and blur, decoded off the main thread at tile size.
pub fn thumb(path: &std::path::Path, ext: &str) -> (Option<RenderImage>, Option<RenderImage>, bool) {
    let video = crate::video::EXTENSIONS.contains(&ext);
    let image = if video {
        crate::video::poster(path, 0.5).and_then(|p| Arc::try_unwrap(p).ok())
    } else if crate::filepreview::IMAGE_EXTS.contains(&ext) || ["heic", "heif", "heics", "heifs"].contains(&ext) {
        crate::backend::pasted::read(path)
            .ok()
            .and_then(|b| crate::media::decode(&b, ext, (THUMB_PX * 2, THUMB_PX * 2)))
            .map(|(i, _, _)| i)
    } else {
        None
    };
    let blur = image.as_ref().and_then(|i| {
        let size = i.size(0);
        // Decoded frames are BGRA, as the GPU takes them.
        let mut bytes = i.as_bytes(0)?.to_vec();
        crate::media::swap_red_blue(&mut bytes);
        let rgba = image::RgbaImage::from_raw(size.width.0 as u32, size.height.0 as u32, bytes)?;
        let hash = crate::util::generate_thumbhash_from_image(&image::DynamicImage::ImageRgba8(rgba))?;
        crate::media::decode_thumbhash(&hash)
    });
    (image, blur, video)
}

/// What a tile asks of the app.
pub enum Act {
    Open(u64),
    Remove(u64),
}

pub type On = std::rc::Rc<dyn Fn(Act, &mut gpui::Window, &mut gpui::App)>;

/// The row of tiles. Arriving tiles grow in and leaving ones fold away, so the row beside
/// them slides rather than jumps.
pub fn tray<V: 'static>(staged: &[Staged], on: On, cx: &mut Context<V>) -> AnyElement {
    let now = Instant::now();
    let tiles = staged.iter().map(|s| {
        let shown = s.shown(now);
        if shown < 1. && (s.leaving.is_some() || now - s.added < std::time::Duration::from_secs_f32(IN_SECS)) {
            crate::kit::keep_moving();
        }
        let p = s.preview.read(cx);
        let (name, ext, spoiler) = (p.shown_name(), p.ext().to_string(), p.spoiler());
        let id = s.id;
        let picture = if spoiler { s.blur.clone().or_else(|| s.thumb.clone()) } else { s.thumb.clone() };
        let face = match picture {
            Some(image) => crate::pic::img(ImageSource::Render(image))
                .size(px(TILE))
                .rounded(px(8.))
                .object_fit(if spoiler { ObjectFit::Fill } else { ObjectFit::Cover })
                .into_any_element(),
            None => div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(4.))
                .child(svg().path(crate::filepreview::file_icon(&ext)).size(px(22.)).text_color(white_a(0.7)))
                .child(
                    div()
                        .max_w(px(TILE - 8.))
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_size(px(10.))
                        .line_height(px(12.))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(white_a(0.55))
                        .child(SharedString::from(ext.to_uppercase())),
                )
                .into_any_element(),
        };
        let open = on.clone();
        let remove = on.clone();
        let tile = div()
            .id(("staged", id))
            .group("staged-tile")
            .relative()
            .size(px(TILE))
            .flex_shrink_0()
            .rounded(px(8.))
            .bg(white_a(0.06))
            .border_1()
            .border_color(white_a(0.08))
            .hover(|d| d.border_color(white_a(0.22)))
            .cursor_pointer()
            .tip(t::composer_staged_edit(name.clone()))
            .on_click(move |_, window, cx| {
                cx.stop_propagation();
                open(Act::Open(id), window, cx)
            })
            .child(face)
            .when(s.video && !spoiler, |d| {
                d.child(
                    div()
                        .absolute()
                        .inset_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            div()
                                .size(px(24.))
                                .rounded_full()
                                .bg(c(0x141414))
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(svg().path("icons/play.svg").size(px(12.)).text_color(white_a(0.95))),
                        ),
                )
            })
            .when(spoiler, |d| {
                d.child(
                    div()
                        .absolute()
                        .inset_0()
                        .rounded(px(8.))
                        .bg(gpui::black().opacity(0.25))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(svg().path("icons/eye-off.svg").size(px(18.)).text_color(white_a(0.9))),
                )
            })
            .child(
                div()
                    .id(("staged-remove", id))
                    .absolute()
                    .top(px(4.))
                    .right(px(4.))
                    .size(px(18.))
                    .rounded_full()
                    .bg(c(0x141414))
                    .border_1()
                    .border_color(c(0x2e2e2e))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .hover(|d| d.bg(c(0x262626)))
                    .aria_label(t::composer_staged_remove(name))
                    .on_click(move |_, window, cx| {
                        cx.stop_propagation();
                        remove(Act::Remove(id), window, cx)
                    })
                    .child(svg().path("ui/chrome-close.svg").size(px(8.)).text_color(c(0xd9d9d9))),
            );
        // The slot grows with the tile, so its neighbours slide; the tile itself pops.
        div()
            .flex_shrink_0()
            .w(px((TILE + GAP) * shown))
            .h(px(TILE))
            .child(gpui::zoom(tile).scale(0.6 + 0.4 * shown).opacity(shown))
    });
    div()
        .id("staged-tray")
        .size_full()
        .pt(px(GAP))
        .pl(px(GAP + 2.))
        .flex()
        .overflow_x_scroll()
        .children(tiles)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn media(name: &str) -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("reference/media").join(name)
    }

    #[test]
    fn a_tile_holds_a_picture_no_larger_than_it_needs() {
        let (image, blur, video) = thumb(&media("photo.png"), "png");
        let size = image.expect("a photo has a thumbnail").size(0);
        assert!(size.width.0 <= THUMB_PX as i32 * 2 && size.height.0 <= THUMB_PX as i32 * 2);
        assert!(blur.is_some() && !video);
    }

    #[test]
    fn a_file_without_a_picture_draws_its_icon() {
        let (image, blur, video) = thumb(&media("golden-hour.mp3"), "mp3");
        assert!(image.is_none() && blur.is_none() && !video);
    }

    #[cfg(feature = "video")]
    #[test]
    fn a_video_tile_shows_its_first_frame() {
        let (image, _, video) = thumb(&media("clip.mp4"), "mp4");
        assert!(image.is_some() && video);
    }
}
