//! A message's attachments (`Attachments.svelte` and its leaves): pictures, the thumbhash blur
//! a picture waits behind, and file boxes. Transfer state lives in [`Media`]; the message only
//! says whether its bytes are here.

use crate::kit::Tipped as _;
use crate::t;
use std::f32::consts::TAU;
use std::sync::LazyLock;
use std::time::Instant;

use gpui::{AnyElement, Bounds, FontWeight, Hsla, IntoElement, ParentElement, PathBuilder, Pixels, SharedString, Styled, canvas, div, point, prelude::*, px, svg};
use crate::pic::img;
use vector_core::types::{Attachment, Message};

use crate::data;
use crate::media::{self, Media, Slot, Source, Transfer};
use crate::theme::*;
use crate::timeline::{Act, Ctx, DANGER};

const IMAGES: [&str; 13] = [
    "png", "jpeg", "jpg", "gif", "webp", "jxl", "svg", "bmp", "tiff", "tif", "ico", "heic", "heif",
];
/// Pictures that wait behind their blur rather than a file box.
const BLURRED: [&str; 11] = ["png", "jpeg", "jpg", "gif", "webp", "jxl", "tiff", "tif", "ico", "heic", "heif"];

const BOX_BG: u32 = 0x161616;
const BOX_BG_HOVER: u32 = 0x1d1d1d;
const BOX_LINE: u32 = 0x262626;
const BOX_LINE_HOVER: u32 = 0x363636;

pub static EPOCH: LazyLock<Instant> = LazyLock::new(Instant::now);

#[derive(Clone, Copy, PartialEq)]
enum Phase {
    Ready,
    Downloading,
    Idle,
}

fn ext(att: &Attachment) -> String {
    att.extension.to_ascii_lowercase()
}

fn spoiler(att: &Attachment) -> bool {
    att.name.to_uppercase().starts_with("SPOILER_")
}

fn source(ctx: &Ctx, att: &Attachment) -> Source {
    if att.resident || att.path.is_empty() {
        if let Some(h) = vector_core::simd::hex::hex_to_bytes_32_checked(&att.id) {
            return Source::Held(h);
        }
    }
    let p = std::path::Path::new(&att.path);
    Source::File(if p.is_absolute() {
        p.to_path_buf()
    } else {
        ctx.store.media_root.join(p)
    })
}

/// `.dmsg-attachments`: each attachment in the state it is in.
pub fn attachments(ctx: &Ctx, m: &Message) -> Option<AnyElement> {
    if m.attachments.is_empty() {
        return None;
    }
    let mut media = ctx.media.borrow_mut();
    let mut col = div().mt(px(4.)).flex().flex_col().items_start();
    let mut prev_bottom = 0.;
    for (i, att) in m.attachments.iter().enumerate() {
        let e = ext(att);
        let unsent = m.mine && att.url.is_empty();
        let ready = att.downloaded || att.resident;
        let failed = media.failed.get(&att.id).cloned();
        let auto = media.auto_download.on
            && !ready
            && !ctx.held
            && !unsent
            && failed.is_none()
            && !media.auto_fired.contains(&att.id)
            && att.size > 0
            && att.size <= media.auto_download.limit;
        if auto {
            media.auto_fired.insert(att.id.clone());
            media.auto_queue.push((m.id.clone(), att.id.clone()));
        }
        let downloading =
            media.transfers.contains_key(&att.id) || media.started.contains(&att.id) || auto;
        let phase = if ready {
            Phase::Ready
        } else if downloading {
            Phase::Downloading
        } else {
            Phase::Idle
        };
        // Auto-download weighs a file by its size, so one sent without it asks its server.
        if matches!(phase, Phase::Idle)
            && att.size == 0
            && !unsent
            && !ctx.held
            && (att.url.starts_with("https://") || att.url.starts_with("http://"))
            && media.sized.insert(att.id.clone())
        {
            media.size_queue.push((m.id.clone(), att.id.clone()));
        }
        let (el, bottom) = match phase {
            Phase::Ready if crate::audio::EXTENSIONS.contains(&e.as_str()) && !unsent => {
                let input = match source(ctx, att) {
                    Source::Held(h) => crate::audio::AudioInputSpec::Held(h),
                    Source::File(p) => crate::audio::AudioInputSpec::File(p),
                };
                media.audio.want_probe(&att.id, &input);
                if !att.name.is_empty() {
                    media.audio.want_tags(&att.id, &input);
                }
                let transcribable = media
                    .audio
                    .tags_read(&att.id)
                    .map_or(att.name.is_empty(), |music| crate::transcript::supported(att, true, music));
                if transcribable && !m.mine && m.at + 60_000 > vector_core::clock::now_ms() {
                    media.audio.want_transcript(&att.id, &input);
                }
                let duration = media.audio.duration(&att.id);
                let tags = media.audio.tags(&att.id).cloned();
                let (on_act, msg, aid) = (ctx.on_act.clone(), m.id.clone(), att.id.clone());
                (
                    crate::audio::view(
                        att,
                        media.audio.player(&att.id),
                        duration,
                        tags.as_deref(),
                        media.audio.sheets.get(&att.id),
                        media.audio.all_tracks.contains(&att.id),
                        transcribable.then_some(&media.audio.transcripts),
                        &media.audio.narrow,
                        std::rc::Rc::new(move |a, w, cx| on_act(&msg, Act::Audio(aid.clone(), a), w, cx)),
                    ),
                    6.,
                )
            }
            Phase::Ready
                if crate::video::EXTENSIONS.contains(&e.as_str()) && !media.videos.failed(&att.id) =>
            {
                let input = match source(ctx, att) {
                    Source::Held(h) => crate::audio::AudioInputSpec::Held(h),
                    Source::File(p) => crate::audio::AudioInputSpec::File(p),
                };
                media.videos.want_probe(&att.id, &input);
                match media.videos.clip(&att.id) {
                    Some(clip) => {
                        let (on_act, msg, aid) = (ctx.on_act.clone(), m.id.clone(), att.id.clone());
                        let size = clip.info.as_ref().map(|i| within(ctx, crate::video::fit(i.w, i.h)));
                        let view = crate::video::view(
                            &att.id,
                            clip,
                            m.pending,
                            size,
                            (8., 8.),
                            std::rc::Rc::new(move |a, w, cx| on_act(&msg, Act::Video(aid.clone(), a), w, cx)),
                        );
                        let view = if m.mine && m.pending {
                            let (w, h) = size.unwrap_or((crate::video::MAX_W, crate::video::MAX_H * 0.6));
                            div().relative().child(view).child(upload_overlay(ctx, m, media.transfers.get(&m.id), w, h)).into_any_element()
                        } else {
                            view
                        };
                        (view, 6.)
                    }
                    None => (video_placeholder(ctx, att), 6.),
                }
            }
            Phase::Ready if crate::model3d::supports(&e) => (model(ctx, &mut media, m, att), 6.),
            Phase::Ready if IMAGES.contains(&e.as_str()) => {
                match picture(ctx, &mut media, m, att) {
                    Some(el) => (el, 6.),
                    None => (file_box(ctx, &media, m, att, FileState::Local, None), 6.),
                }
            }
            Phase::Ready => {
                if e == "xdc" && !media.xdc.contains_key(&att.id) {
                    media.xdc.insert(att.id.clone(), None);
                    let src = match source(ctx, att) {
                        Source::Held(h) => crate::media::Source::Held(h),
                        Source::File(p) => crate::media::Source::File(p),
                    };
                    media.xdc_queue.push((att.id.clone(), src));
                }
                (file_box(ctx, &media, m, att, FileState::Local, None), 6.)
            }
            _ if BLURRED.contains(&e.as_str())
                && let Some(el) = blur(ctx, &mut media, m, att, phase, failed.as_deref()) =>
            {
                (el, 6.)
            }
            Phase::Downloading => (
                file_box(ctx, &media, m, att, FileState::Downloading, None),
                6.,
            ),
            Phase::Idle if unsent => {
                let label = if m.failed { t::file_not_sent() } else { t::file_sending() };
                (
                    file_box(ctx, &media, m, att, FileState::Remote, Some(&label)),
                    6.,
                )
            }
            Phase::Idle => {
                let state = if failed.is_some() {
                    FileState::Failed
                } else {
                    FileState::Remote
                };
                (file_box(ctx, &media, m, att, state, None), 6.)
            }
        };
        let top: f32 = if i == 0 { 0. } else { 4f32.max(prev_bottom) };
        col = col.child(div().mt(px(top)).max_w_full().child(el));
        prev_bottom = bottom;
    }
    Some(col.child(div().h(px(prev_bottom))).into_any_element())
}

/// `ModelAttachment.svelte`: the stage, and the file's box only when the stage can't stand
/// in for it: while it uploads, or when the model won't open here.
fn model(ctx: &Ctx, media: &mut Media, m: &Message, att: &Attachment) -> AnyElement {
    let (on_act, msg, aid) = (ctx.on_act.clone(), m.id.clone(), att.id.clone());
    let on: crate::model3d::stage::OnAct = std::rc::Rc::new(move |a, w, cx| on_act(&msg, Act::Model(aid.clone(), a), w, cx));
    let src = source(ctx, att);
    let ext = ext(att);
    let facts = crate::model3d::stage::Facts { att: &att.id, size: att.size, ext: &ext };
    let stage = crate::model3d::stage::stage(
        &mut media.models,
        &facts,
        move || match src {
            Source::Held(h) => vector_core::media::open(&h).map(crate::model3d::Src::Held),
            Source::File(p) => Some(crate::model3d::Src::File(p)),
        },
        on,
    );
    let failed = media.models.get(&att.id).is_some_and(|v| matches!(v.phase, crate::model3d::view::Phase::Failed(_)));
    let show_file = stage.is_none() || (m.mine && m.pending) || failed;
    div()
        .w(px(320.))
        .max_w_full()
        .flex()
        .flex_col()
        .gap(px(6.))
        .children(stage)
        .when(show_file, |d| d.child(file_box(ctx, media, m, att, FileState::Local, None)))
        .into_any_element()
}

/// `.file-ext-badge` (`attachFileExtBadge`): the file type in a picture's corner, left off
/// one too small to carry it without covering more than a quarter of it.
pub fn ext_badge(ext: &str, w: f32, h: f32) -> Option<AnyElement> {
    let ext = ext.trim().to_uppercase();
    let (bw, bh) = (12. + 6.5 * ext.chars().count() as f32, 14.);
    (!ext.is_empty() && bw <= w * 0.25 && bh <= h * 0.25).then(|| {
        div()
            .absolute()
            .bottom(px(8.))
            .right(px(1.))
            .px(px(6.))
            .rounded(px(14.))
            .bg(ca(0, 0, 0, 0.25))
            .text_color(c(0xffffff))
            .text_size(px(10.))
            .line_height(px(14.))
            .child(ext)
            .into_any_element()
    })
}

/// A fitted size scaled down, aspect kept, to the row it sits in.
fn within(ctx: &Ctx, (w, h): (f32, f32)) -> (f32, f32) {
    if ctx.content_w > 0. && w > ctx.content_w {
        (ctx.content_w, h * ctx.content_w / w)
    } else {
        (w, h)
    }
}

/// Sized from what the sender said while the file's own header is read.
fn video_placeholder(ctx: &Ctx, att: &Attachment) -> AnyElement {
    let (w, h) = within(ctx, att
        .img_meta
        .as_ref()
        .filter(|i| i.width > 0 && i.height > 0)
        .map_or((crate::video::MAX_W, 253.), |i| crate::video::fit(i.width, i.height)));
    div().w(px(w)).h(px(h)).rounded(px(8.)).bg(gpui::black()).into_any_element()
}

fn act(
    ctx: &Ctx,
    m: &Message,
    a: Act,
) -> impl Fn(&gpui::ClickEvent, &mut gpui::Window, &mut gpui::App) + 'static {
    let (on_act, id) = (ctx.on_act.clone(), m.id.clone());
    move |_, w, cx| {
        cx.stop_propagation();
        on_act(&id, a.clone(), w, cx)
    }
}

/// `ImageAttachment`: the picture at its fitted size, or its blur under a Spoiler veil.
fn picture(ctx: &Ctx, media: &mut Media, m: &Message, att: &Attachment) -> Option<AnyElement> {
    let meta = att
        .img_meta
        .as_ref()
        .filter(|i| i.width > 0 && i.height > 0);
    let veiled = spoiler(att) && !media.revealed.contains(&att.id);
    if veiled
        && let Some(meta) = meta
        && let (w, h) = within(ctx, blur_size(meta))
        && let Some(thumb) = media.thumb(&meta.thumbhash, device(ctx, w, h))
    {
        return Some(
            div()
                .id(SharedString::from(format!("veil-{}", att.id)))
                .relative()
                .w(px(w))
                .h(px(h))
                .cursor_pointer()
                .on_click(act(ctx, m, Act::RevealMedia(att.id.clone())))
                .child(img(thumb).size_full().rounded(px(8.)))
                .child(
                    div()
                        .absolute()
                        .inset_0()
                        .flex()
                        .flex_col()
                        .items_center()
                        .justify_center()
                        .gap(px(4.))
                        .child(
                            svg()
                                .path("icons/eye-off.svg")
                                .size(px(22.))
                                .text_color(c(0xffffff)),
                        )
                        .child(
                            div()
                                .text_size(px(13.))
                                .line_height(px(18.2))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(c(0xffffff))
                                .child(t::file_spoiler()),
                        ),
                )
                .into_any_element(),
        );
    }
    let max = |w: f32, h: f32| {
        (
            ((w * ctx.scale).ceil() as u32).max(1),
            ((h * ctx.scale).ceil() as u32).max(1),
        )
    };
    let want = match meta {
        Some(i) => {
            let (w, h) = within(ctx, media::fit(i.width, i.height));
            max(w, h)
        }
        None => max(media::MAX_W, media::MAX_H),
    };
    let pending = m.mine && m.pending;
    match media.image(&att.id, source(ctx, att), &ext(att), want) {
        Slot::Ready { image, w, h } => {
            let (fw, fh) = within(ctx, media::fit(w, h));
            let (hash, path) = match source(ctx, att) {
                Source::Held(h) => (Some(h), None),
                Source::File(p) => (None, Some(p)),
            };
            let open = crate::viewer::Open {
                image: image.clone(),
                w,
                h,
                full: Some(crate::viewer::Full {
                    hash,
                    path,
                    ext: ext(att),
                    url: None,
                }),
            };
            Some(
                div()
                    .id(SharedString::from(format!("pic-{}", att.id)))
                    .relative()
                    .when(!pending, |d| {
                        d.cursor_pointer().on_click(act(ctx, m, Act::View(open)))
                    })
                    .child(
                        img(image)
                            .w(px(fw))
                            .h(px(fh))
                            .rounded(px(8.))
                            .when(pending, |i| i.opacity(0.25)),
                    )
                    .when(pending, |d| d.child(upload_overlay(ctx, m, media.transfers.get(&m.id), fw, fh)))
                    .when(!pending && media.image_types, |d| d.children(ext_badge(&ext(att), fw, fh)))
                    .into_any_element(),
            )
        }
        // Holds the size it will draw, so the row doesn't grow under the reader.
        Slot::Loading => Some(match meta {
            Some(i) => {
                let (w, h) = within(ctx, media::fit(i.width, i.height));
                div().w(px(w)).h(px(h)).into_any_element()
            }
            None => div().into_any_element(),
        }),
        Slot::Failed => None,
    }
}

/// A blur's box: the fitted width, rounded as the `width` attribute rounds it.
fn blur_size(meta: &vector_core::types::ImageMetadata) -> (f32, f32) {
    let (w, _) = media::fit(meta.width, meta.height);
    let w = w.round();
    (
        w,
        (w * meta.height as f32 / meta.width as f32).min(media::MAX_H),
    )
}

fn device(ctx: &Ctx, w: f32, h: f32) -> (u32, u32) {
    (
        (w * ctx.scale).round() as u32,
        (h * ctx.scale).round() as u32,
    )
}

/// `ThumbhashAttachment`: the blur at the size the picture will take, under its download
/// ring or a Download box. None when there is no blur, so the caller shows a file box.
fn blur(
    ctx: &Ctx,
    media: &mut Media,
    m: &Message,
    att: &Attachment,
    phase: Phase,
    failed: Option<&str>,
) -> Option<AnyElement> {
    let meta = att
        .img_meta
        .as_ref()
        .filter(|i| i.width > 0 && i.height > 0)?;
    let (w, h) = within(ctx, blur_size(meta));
    let thumb = media.thumb(&meta.thumbhash, device(ctx, w, h))?;
    let downloading = phase == Phase::Downloading;
    let running = media.transfers.contains_key(&att.id);
    let opacity = if downloading {
        if running { 0.7 } else { 0.8 }
    } else {
        0.6
    };
    let overlay = if downloading {
        let t = media.transfers.get(&att.id).cloned().unwrap_or_default();
        let size = 48f32.min(w.min(h) * 0.7);
        let note = t.note();
        let mut o = div()
            .relative()
            .flex()
            .items_center()
            .justify_center()
            .child(ring(size, t.pct));
        if h >= 118. && w >= 120. && !note.is_empty() {
            o = o.child(
                div()
                    .absolute()
                    .top(px(size + 7.))
                    .w(px(200.))
                    .flex()
                    .justify_center()
                    .text_size(px(11.))
                    .line_height(px(11.))
                    .text_color(white_a(0.82))
                    .whitespace_nowrap()
                    .child(note),
            );
        }
        o.into_any_element()
    } else if ctx.held {
        held_box(w, h)
    } else {
        download_box(ctx, m, att, w, h, failed, media.resume_progress(&att.id, att.size))
    };
    Some(
        div()
            .relative()
            .w(px(w))
            .h(px(h))
            .child(img(thumb).size_full().rounded(px(8.)).opacity(opacity))
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(overlay),
            )
            .into_any_element(),
    )
}

/// `.thumb-held`: a request's media waits for the chat to be accepted.
fn held_box(w: f32, h: f32) -> AnyElement {
    let compact = w < 150. || h < 40.;
    div()
        .id("thumb-held")
        .max_w(px(w * 0.94))
        .flex()
        .items_center()
        .gap(px(8.))
        .when(compact, |d| d.p(px(7.)).tip(t::file_accept_to_view()))
        .when(!compact, |d| d.pl(px(10.)).pr(px(14.)).py(px(7.)))
        .border_1()
        .border_color(c(0x363636))
        .rounded(px(10.))
        .bg(ca(12, 12, 12, 0.82))
        .text_size(px(13.))
        .line_height(px(18.))
        .text_color(white_a(0.92))
        .whitespace_nowrap()
        .child(svg().path("icons/shield-alert.svg").size(px(18.)).flex_shrink_0().text_color(c(PRIMARY)))
        .when(!compact, |d| d.child(div().min_w_0().truncate().child(t::file_accept_to_view())))
        .into_any_element()
}

/// `.thumb-download`: the icon tile and two lines, or the tile alone on a small blur.
fn download_box(
    ctx: &Ctx,
    m: &Message,
    att: &Attachment,
    w: f32,
    h: f32,
    failed: Option<&str>,
    paused: Option<String>,
) -> AnyElement {
    // Every server the message names says the file is gone: nothing on this side is at fault.
    let gone = failed == Some(crate::backend::attachments::GONE_EVERYWHERE);
    // A failure says why even where the row won't fit: the words go under the icon.
    let stacked = failed.is_some() && (w < 205. || h < 76.) && w >= 120. && h >= 140.;
    let compact = (w < 205. || h < 76.) && !stacked;
    let color = if failed.is_some() {
        c(DANGER)
    } else {
        c(PRIMARY)
    };
    let icon = if gone {
        "icons/file-minus.svg"
    } else if failed.is_some() {
        "icons/refresh.svg"
    } else {
        "icons/download.svg"
    };
    let tile_size = if compact { 40. } else { 44. };
    let tile = div()
        .size(px(tile_size))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .border_1()
        .border_color(if failed.is_some() {
            c(DANGER).opacity(0.45)
        } else {
            c(BOX_LINE_HOVER)
        })
        .rounded(px(10.))
        .child(
            svg()
                .path(icon)
                .size(px(tile_size * 0.64))
                .text_color(color),
        );
    let title = match failed {
        Some(_) if gone => t::file_unavailable().to_string(),
        Some(_) => t::file_download_failed().to_string(),
        None if paused.is_some() => t::file_resume_download().to_string(),
        None if att.extension.trim().is_empty() => t::file_download().to_string(),
        None => t::file_download_type(att.extension.trim().to_uppercase()).to_string(),
    };
    let sub = match failed {
        Some(_) if gone => t::file_gone().to_string(),
        Some(e) if !e.is_empty() => e.chars().take(64).collect(),
        Some(_) => t::file_tap_retry().to_string(),
        None if paused.is_some() => t::file_resume_done(paused.unwrap_or_default()).to_string(),
        None if att.size > 0 => media::format_bytes(att.size),
        None => t::file_unknown_size().to_string(),
    };
    let base = div()
        .id(SharedString::from(format!("dl-{}", att.id)))
        .cursor_pointer()
        .on_click(act(ctx, m, Act::Download(att.id.clone())));
    if compact {
        return base
            .tip(format!("{title} · {sub}")) // i18n-exempt: separator
            .child(
                tile.bg(ca(12, 12, 12, 0.82))
                    .hover(|s| s.bg(ca(24, 24, 24, 0.88)).border_color(c(0x4a4a4a))),
            )
            .into_any_element();
    }
    base.max_w(px(w * 0.94))
        .flex()
        .items_center()
        .gap(px(12.))
        .p(px(10.))
        .border_1()
        .border_color(c(BOX_LINE_HOVER))
        .rounded(px(12.))
        .bg(ca(12, 12, 12, 0.82))
        .hover(|s| s.bg(ca(24, 24, 24, 0.88)).border_color(c(0x4a4a4a)))
        .when(stacked, |d| d.flex_col().gap(px(8.)))
        .child(tile)
        .child(
            div()
                .min_w_0()
                .when(!stacked, |d| d.pr(px(4.)))
                .when(stacked, |d| d.items_center().text_center())
                .flex()
                .flex_col()
                .child(
                    div()
                        .text_size(px(14.))
                        .line_height(px(17.5))
                        .text_color(white_a(0.92))
                        .when(!stacked, |d| d.whitespace_nowrap().overflow_hidden().text_ellipsis())
                        .child(title),
                )
                .child(
                    div()
                        .mt(px(2.))
                        .text_size(px(12.))
                        .line_height(px(15.))
                        .text_color(white_a(0.34))
                        .when(!stacked, |d| d.whitespace_nowrap().overflow_hidden().text_ellipsis())
                        .child(sub),
                ),
        )
        .into_any_element()
}

/// `UploadOverlay`: over a picture or video still uploading, the ring, a line under it whenever
/// the ring can't speak for itself (on media with room for one), and the cancel, shown on hover
/// until the last byte has left.
fn upload_overlay(ctx: &Ctx, m: &Message, t: Option<&Transfer>, w: f32, h: f32) -> AnyElement {
    const RING: f32 = 48.;
    let note = t.map(Transfer::upload_note).filter(|n| !n.is_empty() && w >= 120. && h >= 118.);
    let group = SharedString::from(format!("upload-{}", m.id));
    div()
        .absolute()
        .top_0()
        .left_0()
        .w(px(w))
        .h(px(h))
        .group(group.clone())
        .flex()
        .items_center()
        .justify_center()
        .child(
            div()
                .relative()
                .size(px(RING))
                .child(ring(RING, t.and_then(|t| t.pct)))
                // `.media-progress-note`: hung under the ring, so the ring never moves for it.
                .children(note.map(|n| {
                    div()
                        .absolute()
                        .top(px(RING + 7.))
                        .left(px(RING / 2. - 120.))
                        .w(px(240.))
                        .flex()
                        .justify_center()
                        .whitespace_nowrap()
                        .text_size(px(11.))
                        .line_height(px(13.))
                        .text_color(white_a(0.82))
                        .child(n)
                })),
        )
        .when(!t.is_some_and(Transfer::publishing), |d| {
            // `.upload-cancel-btn`: fills the ring's hole.
            d.child(
                div().absolute().inset_0().flex().items_center().justify_center().child(
                    div()
                        .id(SharedString::from(format!("upload-cancel-{}", m.id)))
                        .size(px(40.))
                        .rounded_full()
                        .bg(gpui::black().opacity(0.6))
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(px(16.))
                        .text_color(c(0xffffff))
                        .cursor_pointer()
                        .opacity(0.)
                        .group_hover(group, |s| s.opacity(1.))
                        .on_click(act(ctx, m, Act::CancelUpload(m.id.clone())))
                        .child("\u{2715}"),
                ),
            )
        })
        .into_any_element()
}

/// `.miniapp-downloading-spinner`: a 4px ring filled to `pct` from the top, turning once
/// every two seconds.
pub fn ring(size: f32, pct: Option<f32>) -> AnyElement {
    spinner(
        size,
        3.5,
        2.,
        pct.unwrap_or(5.).clamp(0., 100.) / 100.,
        white_a(0.2),
        c(PRIMARY),
    )
}

/// A turning ring: `fill` of it in the accent over a `track`, one turn every `period` seconds.
pub fn spinner(
    size: f32,
    thickness: f32,
    period: f32,
    fill: f32,
    track: gpui::Hsla,
    color: gpui::Hsla,
) -> AnyElement {
    canvas(
        |_, _, _| {},
        move |bounds: Bounds<Pixels>, _, window, _| {
            let turn = (EPOCH.elapsed().as_secs_f32() / period).fract() * TAU;
            let center = bounds.center();
            let r_out = f32::from(bounds.size.width.min(bounds.size.height)) / 2.;
            let r_in = r_out - thickness;
            let arc = |from: f32, to: f32| -> Option<gpui::Path<Pixels>> {
                let steps = ((to - from).abs() / TAU * 96.).ceil().max(2.) as usize;
                let at =
                    |r: f32, a: f32| point(center.x + px(r * a.sin()), center.y - px(r * a.cos()));
                let mut pts = Vec::with_capacity(steps * 2 + 2);
                for k in 0..=steps {
                    pts.push(at(r_out, from + (to - from) * k as f32 / steps as f32));
                }
                for k in (0..=steps).rev() {
                    pts.push(at(r_in, from + (to - from) * k as f32 / steps as f32));
                }
                let mut b = PathBuilder::fill();
                b.add_polygon(&pts, true);
                b.build().ok()
            };
            let split = turn + fill * TAU;
            if fill < 1.
                && let Some(p) = arc(split, turn + TAU)
            {
                window.paint_path(p, track);
            }
            if fill > 0.
                && let Some(p) = arc(turn, split)
            {
                window.paint_path(p, color);
            }
            window.request_animation_frame();
        },
    )
    .size(px(size))
    .flex_shrink_0()
    .into_any_element()
}

#[derive(Clone, Copy, PartialEq)]
enum FileState {
    Local,
    Downloading,
    Remote,
    Failed,
}

/// `FileBox`: icon, name and what the file is doing; the box is the action.
/// A Mini App shared as a link: nothing fetched until asked, then the attachment's own card.
#[derive(Clone)]
pub enum XdcUrl {
    Probing,
    Absent,
    Fetching,
    Failed(String),
    Ready(crate::backend::xdcurl::Resolved),
}

pub fn xdc_url_key(msg: &str, url: &str) -> String {
    format!("{msg}|{url}")
}

/// The card for a message's `.xdc` link, in the shape of an uploaded Mini App's.
pub fn xdc_url_card(ctx: &Ctx, m: &Message) -> Option<AnyElement> {
    if m.pending || m.failed || ctx.held || !m.attachments.is_empty() {
        return None;
    }
    let url = crate::backend::xdcurl::find(&m.content)?.to_string();
    let key = xdc_url_key(&m.id, &url);
    let mut media = ctx.media.borrow_mut();
    let state = match media.xdc_urls.get(&key) {
        Some(s) => s.clone(),
        None => {
            media.xdc_urls.insert(key.clone(), XdcUrl::Probing);
            media.xdc_url_queue.push((m.id.clone(), url.clone()));
            XdcUrl::Probing
        }
    };
    let mut att = Attachment { extension: "xdc".into(), name: crate::backend::xdcurl::file_name(&url), id: url.clone(), ..Default::default() };
    let tap = |a: Act| Some(a);
    let host = crate::backend::xdcurl::host(&url);
    let el = match state {
        XdcUrl::Ready(r) => {
            att.id = format!("xdcurl:{key}");
            att.name = r.name.clone();
            att.webxdc_topic = Some(r.topic.clone());
            att.downloaded = true;
            if !media.xdc.contains_key(&att.id) {
                media.xdc.insert(att.id.clone(), None);
                media.xdc_queue.push((att.id.clone(), crate::media::Source::File(r.path.clone())));
            }
            file_box_on(ctx, &media, m, &att, FileState::Local, None, tap(Act::OpenXdcUrl(url)))
        }
        XdcUrl::Fetching => file_box_on(ctx, &media, m, &att, FileState::Downloading, None, None),
        XdcUrl::Failed(e) => {
            let label = t::file_failed_retry(e.chars().take(48).collect::<String>()).to_string();
            file_box_on(ctx, &media, m, &att, FileState::Failed, Some(&label), tap(Act::XdcUrl(url)))
        }
        XdcUrl::Probing | XdcUrl::Absent => {
            let label = t::file_tap_to_load(&*host).to_string();
            file_box_on(ctx, &media, m, &att, FileState::Remote, Some(&label), tap(Act::XdcUrl(url)))
        }
    };
    // The trailing whitespace after Tauri's card wrapper lays a 6px line under it.
    Some(div().pb(px(6.)).flex().flex_col().items_start().child(el).into_any_element())
}

fn file_box(
    ctx: &Ctx,
    media: &Media,
    m: &Message,
    att: &Attachment,
    state: FileState,
    label: Option<&str>,
) -> AnyElement {
    file_box_on(ctx, media, m, att, state, label, None)
}

/// `tap` takes over the box's click and its button: for a card no attachment backs.
fn file_box_on(
    ctx: &Ctx,
    media: &Media,
    m: &Message,
    att: &Attachment,
    state: FileState,
    label: Option<&str>,
    tap: Option<Act>,
) -> AnyElement {
    let synthetic = tap.is_some() || m.attachments.iter().all(|a| a.id != att.id);
    let (icon, description) = data::file_type(&att.extension);
    let mini = icon == "gift";
    let icon = if mini { "gamepad" } else { icon };
    let package = mini
        .then(|| media.xdc.get(&att.id).cloned().flatten())
        .flatten()
        .filter(|_| state == FileState::Local);
    let title: SharedString = match &package {
        Some(p) => if p.name.is_empty() { crate::t::file_type_mini_app() } else { crate::kit::one_line(&p.name).into_owned().into() },
        None if att.name.is_empty() => description.clone(),
        None => crate::kit::one_line(&att.name).into_owned().into(),
    };
    let live = mini.then(|| {
        let key = att.webxdc_topic.clone().unwrap_or_else(|| format!("att:{}", att.id));
        media.xdc_live.get(&key).cloned().unwrap_or_default()
    });
    let e = ext(att);
    let kind = if e.is_empty() {
        description.to_string()
    } else {
        format!(".{e}")
    };
    let size = (att.size > 0).then(|| media::format_bytes(att.size));
    let glyph_color = if state == FileState::Failed {
        c(DANGER)
    } else {
        white_a(0.7)
    };
    let title_color = if state == FileState::Local {
        c(PRIMARY)
    } else {
        white_a(0.92)
    };

    let uploading = m.mine && m.pending && state == FileState::Local;
    let state = if uploading {
        FileState::Downloading
    } else {
        state
    };
    let text = div().flex_1().min_w_0().flex().flex_col();
    let text = if state == FileState::Downloading {
        let t: Transfer = media
            .transfers
            .get(if uploading { &m.id } else { &att.id })
            .cloned()
            .unwrap_or_default();
        let note = t.note();
        text.child(
            div()
                .flex()
                .items_baseline()
                .whitespace_nowrap()
                .text_size(px(15.))
                .line_height(px(18.75))
                .child(
                    div()
                        .flex_shrink(1.)
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .text_color(title_color)
                        .child(title),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .flex()
                        .text_size(px(12.75))
                        .text_color(white_a(0.34))
                        .child(size.map(|s| format!(" — {s}")).unwrap_or_default())
                        .child(div().ml(px(4.5)).w(px(112.)).child(if note.is_empty() {
                            String::new()
                        } else {
                            format!("· {note}") // i18n-exempt: separator
                        })),
                ),
        )
        .child(bar(t.pct))
    } else {
        let sub = match (label, state) {
            (Some(l), _) => l.to_string(),
            (None, FileState::Failed) if media.failed.get(&att.id).is_some_and(|r| r == crate::backend::attachments::GONE_EVERYWHERE) => {
                t::file_unavailable_gone().into()
            }
            (None, FileState::Failed) => {
                let reason: String = media
                    .failed
                    .get(&att.id)
                    .map(|r| r.chars().take(64).collect())
                    .unwrap_or_default();
                if reason.is_empty() {
                    t::file_download_failed_retry().into()
                } else {
                    t::file_failed_retry(reason).into()
                }
            }
            _ => match (media.resume_note(&att.id, att.size), &size) {
                (Some(r), _) => r,
                (None, Some(s)) => t::file_kind_size(&*kind, &**s).into(),
                (None, None) => kind,
            },
        };
        let text = text.child(
            div()
                .text_size(px(15.))
                .line_height(px(18.75))
                .text_color(title_color)
                .whitespace_nowrap()
                .overflow_hidden()
                .text_ellipsis()
                .child(title),
        );
        match live.as_ref().filter(|_| state == FileState::Local && label.is_none()) {
            Some(live) => text.child(session_line(ctx, live)),
            None => text.child(
                div()
                    .mt(px(3.))
                    .text_size(px(12.))
                    .line_height(px(15.))
                    .text_color(white_a(0.55))
                    .whitespace_nowrap()
                    .child(sub),
            ),
        }
    };

    let button = |icon: &'static str, color: Hsla, border: Hsla, a: Act| {
        div()
            .id(SharedString::from(format!("fb-act-{}", att.id)))
            .size(px(34.))
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_center()
            .border_1()
            .border_color(border)
            .rounded(px(8.))
            .cursor_pointer()
            .hover(|s| s.bg(white_a(0.07)))
            .on_click(act(ctx, m, a))
            .child(svg().path(icon).size(px(17.)).text_color(color))
    };
    let action = match (state, label) {
        (FileState::Remote, _) if synthetic => tap.clone().map(|a| button("icons/download.svg", c(PRIMARY), c(BOX_LINE_HOVER), a)),
        (FileState::Failed, _) if synthetic => tap.clone().map(|a| button("icons/refresh.svg", c(DANGER), c(DANGER).opacity(0.45), a)),
        _ if synthetic => None,
        (_, Some(_)) => None,
        (FileState::Downloading, _) if uploading => {
            let done = media
                .transfers
                .get(&m.id)
                .and_then(|t| t.pct)
                .is_some_and(|p| p >= 100.);
            (!done).then(|| {
                button(
                    "icons/x.svg",
                    white_a(0.7),
                    c(BOX_LINE_HOVER),
                    Act::CancelUpload(m.id.clone()),
                )
            })
        }
        (FileState::Downloading, _) => Some(button(
            "icons/x.svg",
            white_a(0.7),
            c(BOX_LINE_HOVER),
            Act::CancelDownload(att.id.clone()),
        )),
        (FileState::Failed, _) => Some(button(
            "icons/refresh.svg",
            c(DANGER),
            c(DANGER).opacity(0.45),
            Act::Download(att.id.clone()),
        )),
        (FileState::Remote, _) => Some(button(
            "icons/download.svg",
            c(PRIMARY),
            c(BOX_LINE_HOVER),
            Act::Download(att.id.clone()),
        )),
        (FileState::Local, _) => None,
    };
    let click = match state {
        _ if synthetic => tap,
        FileState::Local => Some(Act::OpenFile(att.id.clone())),
        FileState::Remote | FileState::Failed if label.is_none() => {
            Some(Act::Download(att.id.clone()))
        }
        _ => None,
    };

    div()
        .id(SharedString::from(format!("fb-{}", att.id)))
        .flex()
        .items_center()
        .when(ctx.vw > 480., |d| d.gap(px(12.)).px(px(12.)).py(px(10.)))
        .when(ctx.vw <= 480., |d| d.gap(px(10.)).p(px(10.)))
        .min_w(px(240.))
        .max_w(px(500f32.min(ctx.vw - 100.)))
        .border_1()
        .border_color(c(BOX_LINE))
        .rounded(px(12.))
        .bg(c(BOX_BG))
        .when(state != FileState::Downloading, |d| {
            d.hover(|s| s.bg(c(BOX_BG_HOVER)).border_color(c(BOX_LINE_HOVER)))
                .cursor_pointer()
        })
        .when_some(click, |d, a| d.on_click(act(ctx, m, a)))
        .child(
            div()
                .size(px(40.))
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_center()
                .child(match package.as_ref().and_then(|p| p.icon.clone()) {
                    Some(path) => img(path)
                        .size(px(40.))
                        .rounded(px(8.))
                        .object_fit(gpui::ObjectFit::Cover)
                        .into_any_element(),
                    None => svg()
                        .path(format!("icons/{icon}.svg"))
                        .size(px(32.))
                        .text_color(glyph_color)
                        .into_any_element(),
                }),
        )
        .child(text)
        .children(action)
        .into_any_element()
}

/// `.file-box-session`: what the Mini App is doing, and who else is in it.
fn session_line(ctx: &Ctx, live: &crate::media::XdcLive) -> AnyElement {
    let players = live.peers.len();
    let label = if live.active {
        t::file_app_playing()
    } else if players > 0 {
        t::file_app_join()
    } else {
        t::file_app_play()
    };
    let mut peers: Vec<&String> = live.peers.iter().collect();
    peers.sort();
    peers.truncate(5);
    let n = peers.len();
    div()
        .mt(px(3.))
        .flex()
        .items_center()
        .gap(px(10.))
        .text_size(px(12.))
        .line_height(px(15.))
        .whitespace_nowrap()
        .child(
            div()
                .text_color(if live.active { c(PRIMARY) } else { white_a(0.55) })
                .child(label),
        )
        .when(players > 0, |d| {
            d.child(
                div()
                    .px(px(8.))
                    .py(px(2.))
                    .rounded(px(10.))
                    .bg(c(PRIMARY).opacity(0.15))
                    .border(px(0.5))
                    .border_color(c(PRIMARY).opacity(0.3))
                    .text_color(c(PRIMARY))
                    .text_size(px(10.2))
                    .font_weight(FontWeight::MEDIUM)
                    .flex()
                    .items_center()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .mr(px(4.))
                            .children(peers.iter().enumerate().map(|(i, npub)| {
                                div()
                                    .when(i > 0, |d| d.ml(px(-5.)))
                                    .size(px(14.))
                                    .rounded_full()
                                    .border_1()
                                    .border_color(c(0x1a1a2e))
                                    .overflow_hidden()
                                    .child(crate::ui::avatar(ctx.store, npub, 14.))
                                    .into_any_element()
                            }))
                            .when(n == 0, |d| {
                                d.child(svg().path("icons/group-placeholder.svg").size(px(14.)))
                            }),
                    )
                    .child(t::file_app_online(players)),
            )
        })
        .into_any_element()
}

/// `.file-box-bar`: 4px, the theme colour up to the progress.
fn bar(pct: Option<f32>) -> impl IntoElement {
    let fill = pct.unwrap_or(0.).clamp(0., 100.) / 100.;
    div()
        .mt(px(7.))
        .h(px(4.))
        .w_full()
        .rounded_full()
        .bg(white_a(0.12))
        .overflow_hidden()
        .child(
            div()
                .h_full()
                .w(gpui::relative(fill))
                .rounded_full()
                .bg(c(PRIMARY)),
        )
}
