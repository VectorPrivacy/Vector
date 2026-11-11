//! A message's attachments (`Attachments.svelte` and its leaves): pictures, the thumbhash blur
//! a picture waits behind, and file boxes. Transfer state lives in [`Media`]; the message only
//! says whether its bytes are here.

use std::f32::consts::TAU;
use std::sync::LazyLock;
use std::time::Instant;

use gpui::{
    AnyElement, Bounds, FontWeight, Hsla, IntoElement, ParentElement, PathBuilder, Pixels,
    SharedString, Styled, canvas, div, img, point, prelude::*, px, svg,
};
use vector_core::types::{Attachment, Message};

use crate::data;
use crate::media::{self, Media, Slot, Source, Transfer};
use crate::theme::*;
use crate::timeline::{Act, Ctx, DANGER};

const IMAGES: [&str; 10] = [
    "png", "jpeg", "jpg", "gif", "webp", "svg", "bmp", "tiff", "tif", "ico",
];
/// Pictures that wait behind their blur rather than a file box.
const BLURRED: [&str; 8] = ["png", "jpeg", "jpg", "gif", "webp", "tiff", "tif", "ico"];

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
        let (el, bottom) = match phase {
            Phase::Ready if IMAGES.contains(&e.as_str()) => {
                match picture(ctx, &mut media, m, att) {
                    Some(el) => (el, 6.),
                    None => (file_box(ctx, &media, m, att, FileState::Local, None), 6.),
                }
            }
            Phase::Ready => (file_box(ctx, &media, m, att, FileState::Local, None), 6.),
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
                let label = if m.failed { "Not sent" } else { "Sending…" };
                (
                    file_box(ctx, &media, m, att, FileState::Remote, Some(label)),
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
        col = col.child(div().mt(px(top)).child(el));
        prev_bottom = bottom;
    }
    Some(col.child(div().h(px(prev_bottom))).into_any_element())
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
        && let (w, h) = blur_size(meta)
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
                                .child("Spoiler"),
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
            let (w, h) = media::fit(i.width, i.height);
            max(w, h)
        }
        None => max(media::MAX_W, media::MAX_H),
    };
    let pending = m.mine && m.pending;
    let upload = media.transfers.get(&m.id).and_then(|t| t.pct);
    match media.image(&att.id, source(ctx, att), &ext(att), want) {
        Slot::Ready { image, w, h } => {
            let (fw, fh) = media::fit(w, h);
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
                    .when(pending, |d| {
                        d.child(
                            div()
                                .absolute()
                                .inset_0()
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(ring(48., upload)),
                        )
                    })
                    .into_any_element(),
            )
        }
        // Holds the size it will draw, so the row doesn't grow under the reader.
        Slot::Loading => Some(match meta {
            Some(i) => {
                let (w, h) = media::fit(i.width, i.height);
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
    let (w, h) = blur_size(meta);
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
    } else {
        download_box(ctx, m, att, w, h, failed)
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

/// `.thumb-download`: the icon tile and two lines, or the tile alone on a small blur.
fn download_box(
    ctx: &Ctx,
    m: &Message,
    att: &Attachment,
    w: f32,
    h: f32,
    failed: Option<&str>,
) -> AnyElement {
    let compact = w < 205. || h < 76.;
    let color = if failed.is_some() {
        c(DANGER)
    } else {
        c(PRIMARY)
    };
    let icon = if failed.is_some() {
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
        Some(_) => "Download Failed".to_string(),
        None => format!("Download {}", att.extension.to_uppercase())
            .trim()
            .to_string(),
    };
    let sub = match failed {
        Some(e) if !e.is_empty() => e.chars().take(64).collect(),
        Some(_) => "Tap to Retry".to_string(),
        None if att.size > 0 => media::format_bytes(att.size),
        None => "Unknown Size".to_string(),
    };
    let base = div()
        .id(SharedString::from(format!("dl-{}", att.id)))
        .cursor_pointer()
        .on_click(act(ctx, m, Act::Download(att.id.clone())));
    if compact {
        return base
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
        .child(tile)
        .child(
            div()
                .min_w_0()
                .pr(px(4.))
                .flex()
                .flex_col()
                .child(
                    div()
                        .text_size(px(14.))
                        .line_height(px(17.5))
                        .text_color(white_a(0.92))
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .text_ellipsis()
                        .child(title),
                )
                .child(
                    div()
                        .mt(px(2.))
                        .text_size(px(12.))
                        .line_height(px(15.))
                        .text_color(white_a(0.34))
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .text_ellipsis()
                        .child(sub),
                ),
        )
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
fn file_box(
    ctx: &Ctx,
    media: &Media,
    m: &Message,
    att: &Attachment,
    state: FileState,
    label: Option<&str>,
) -> AnyElement {
    let (icon, description) = data::file_type(&att.extension);
    let icon = if icon == "gift" { "gamepad" } else { icon };
    let title: SharedString = if att.name.is_empty() {
        description.into()
    } else {
        att.name.clone().into()
    };
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
                            format!("· {note}")
                        })),
                ),
        )
        .child(bar(t.pct))
    } else {
        let sub = match (label, state) {
            (Some(l), _) => l.to_string(),
            (None, FileState::Failed) => {
                let reason: String = media
                    .failed
                    .get(&att.id)
                    .map(|r| r.chars().take(64).collect())
                    .unwrap_or_default();
                if reason.is_empty() {
                    "Download Failed · Tap to Retry".into()
                } else {
                    format!("Failed: {reason} · Tap to Retry")
                }
            }
            _ => match &size {
                Some(s) => format!("{kind} — {s}"),
                None => kind,
            },
        };
        text.child(
            div()
                .text_size(px(15.))
                .line_height(px(18.75))
                .text_color(title_color)
                .whitespace_nowrap()
                .overflow_hidden()
                .text_ellipsis()
                .child(title),
        )
        .child(
            div()
                .mt(px(3.))
                .text_size(px(12.))
                .line_height(px(15.))
                .text_color(white_a(0.55))
                .whitespace_nowrap()
                .child(sub),
        )
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
        .gap(px(12.))
        .px(px(12.))
        .py(px(10.))
        .min_w(px(240.))
        .max_w(px(500.))
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
                .child(
                    svg()
                        .path(format!("icons/{icon}.svg"))
                        .size(px(32.))
                        .text_color(glyph_color),
                ),
        )
        .child(text)
        .children(action)
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
