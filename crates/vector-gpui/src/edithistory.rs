//! `EditHistoryPopup.svelte`: a message's revisions as a timeline, newest first, each edit as
//! what it changed. Placed by the room either side of the row the `(edited)` mark sits in, or a
//! bottom sheet on a narrow window.

use std::cell::Cell;
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{AnyElement, App, Bounds, Pixels, SharedString, Size, Window, div, point, px};
use unicode_segmentation::UnicodeSegmentation;

use crate::data::Store;
use crate::inline::{Atom, Kind, Look, inline};
use crate::theme::*;

pub struct EditHistory {
    pub msg: String,
    pub entries: Vec<vector_core::types::EditEntry>,
    /// The message row, in window coordinates.
    pub anchor: Bounds<Pixels>,
    /// Custom emoji: the message's own tags, then the equipped packs'.
    pub tags: Vec<(String, String)>,
    pub size: Rc<Cell<Option<Size<Pixels>>>>,
    pub scroll: gpui::ScrollHandle,
    /// Revisions shown whole, by index.
    pub open: std::collections::HashSet<usize>,
}

pub type OnClose = Rc<dyn Fn(&mut Window, &mut App)>;
/// A revision opened whole, or folded back.
pub type OnToggle = Rc<dyn Fn(usize, &mut Window, &mut App)>;

/// `toLocaleString(month: short, day, hour: 2-digit, minute: 2-digit)`.
fn when(ms: u64) -> String {
    use chrono::TimeZone;
    chrono::Local
        .timestamp_millis_opt(ms as i64)
        .single()
        .map(|d| d.format("%b %-d, %I:%M %p").to_string())
        .unwrap_or_default()
}

/// The words as written: no markdown, emoji and `:shortcode:`s drawn.
fn atoms(text: &str, tags: &[(String, String)], store: &Store, media: &std::cell::RefCell<crate::media::Media>, look: &Look) -> Vec<Atom> {
    let look = look.clone();
    let size = look.size * 1.15;
    let mut out = Vec::new();
    let plain = |t: &str, out: &mut Vec<Atom>| {
        let mut start = 0;
        for (i, g) in t.grapheme_indices(true) {
            if let Some(src) = crate::emoji::twemoji(g) {
                if i > start {
                    out.push(Atom::Text { text: t[start..i].to_string().into(), look: look.clone(), kind: Kind::Plain });
                }
                out.push(Atom::Image { src: src.into(), size, drop: look.size * 0.2, tip: None });
                start = i + g.len();
            }
        }
        if start < t.len() {
            out.push(Atom::Text { text: t[start..].to_string().into(), look: look.clone(), kind: Kind::Plain });
        }
    };
    let mut last = 0;
    for (a, b) in crate::markdown::shortcodes(text) {
        let code = &text[a + 1..b - 1];
        let Some((_, url)) = tags.iter().find(|(c, _)| c == code) else { continue };
        let custom = media.borrow_mut().custom(&crate::timeline::emoji_source(store, url), false);
        let atom = match custom {
            crate::media::Custom::Ready(image) => Atom::Image { src: image.into(), size, drop: look.size * 0.2, tip: None },
            crate::media::Custom::Pending => Atom::Gap(size),
            crate::media::Custom::Missing(_) => continue,
        };
        plain(&text[last..a], &mut out);
        out.push(atom);
        last = b;
    }
    plain(&text[last..], &mut out);
    out
}

/// A revision's words with each run in its diff look: removed struck on red, added on mint,
/// unchanged dimmed, folds as an ellipsis.
fn diff_atoms(runs: &[crate::textdiff::Run], tags: &[(String, String)], store: &Store, media: &std::cell::RefCell<crate::media::Media>) -> Vec<Atom> {
    use crate::textdiff::{Op, Run};
    let mut out = Vec::new();
    for run in runs {
        match run {
            Run::Gap { lines, .. } => {
                let mut look = text_look();
                look.color = white_a(0.35);
                if *lines {
                    out.push(Atom::Break);
                }
                out.push(Atom::Text { text: " … ".into(), look, kind: Kind::Plain });
                if *lines {
                    out.push(Atom::Break);
                }
            }
            Run::Text(op, text) => {
                let mut look = text_look();
                match op {
                    Op::Eq => look.color = white_a(0.6),
                    Op::Ins => {
                        look.color = c(0xffffff);
                        look.bg = Some(c(PRIMARY).opacity(0.2));
                    }
                    Op::Del => {
                        look.color = c(0xff6dc3);
                        look.bg = Some(c(crate::call::DANGER).opacity(0.12));
                        look.strike = true;
                    }
                }
                // Spaces at a change's edges aren't part of it: struck, they read as a dash.
                let body = if *op == Op::Eq { text.as_str() } else { text.trim() };
                let lead = &text[..text.len() - text.trim_start().len()];
                let trail = &text[text.trim_end().len()..];
                if *op != Op::Eq && !lead.is_empty() && !body.is_empty() {
                    out.extend(atoms_in(lead, tags, store, media, text_look()));
                }
                out.extend(atoms_in(if body.is_empty() { text } else { body }, tags, store, media, look));
                if *op != Op::Eq && !trail.is_empty() && !body.is_empty() {
                    out.extend(atoms_in(trail, tags, store, media, text_look()));
                }
            }
        }
    }
    out
}

fn text_look() -> Look {
    Look::new(INTER, 13., 18.85, white_a(0.85))
}

/// Text with its line breaks kept.
fn atoms_in(text: &str, tags: &[(String, String)], store: &Store, media: &std::cell::RefCell<crate::media::Media>, look: Look) -> Vec<Atom> {
    let mut out = Vec::new();
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            out.push(Atom::Break);
        }
        if !line.is_empty() {
            out.extend(atoms(line, tags, store, media, &look));
        }
    }
    out
}

pub fn render(
    h: &EditHistory,
    store: &Store,
    media: &std::cell::RefCell<crate::media::Media>,
    viewport: Size<Pixels>,
    on_close: &OnClose,
    on_toggle: &OnToggle,
) -> AnyElement {
    let total = h.entries.len();
    let (vw, vh) = (f32::from(viewport.width), f32::from(viewport.height));
    let sheet = vw <= 640.;
    let texts: Vec<String> = h.entries.iter().map(|e| store.stream_mask(&e.content).unwrap_or_else(|| e.content.clone())).collect();
    // Newest first: each edit as what it changed against the version before it.
    let revisions = (0..total).rev().map(|i| {
        let open = h.open.contains(&i);
        let current = i + 1 == total;
        let head = div()
            .flex()
            .items_baseline()
            .mb(px(3.))
            .text_size(px(11.))
            .line_height(px(16.))
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .text_color(white_a(0.55))
            .child(if i == 0 { "Original" } else { "Edited" })
            .child(div().ml(px(4.)).font_weight(gpui::FontWeight::NORMAL).text_color(white_a(0.32)).child(when(h.entries[i].edited_at)));
        let toggle = on_toggle.clone();
        let body = if i > 0 {
            let ops = crate::textdiff::diff_words(&texts[i - 1], &texts[i]);
            let runs = if open {
                ops.into_iter().map(|(op, t)| crate::textdiff::Run::Text(op, t)).collect()
            } else {
                crate::textdiff::fold_unchanged(&ops, 6, 40)
            };
            div()
                .id(("eh-text", i))
                .cursor_pointer()
                .on_click(move |_, w, cx| toggle(i, w, cx))
                .child(inline(diff_atoms(&runs, &h.tags, store, media), text_look()))
        } else {
            // The original: a short preview, whole on a click once it's cut short.
            let long = texts[0].len() > 140 || texts[0].matches('\n').count() > 2;
            let collapsed = !open && long;
            div()
                .id(("eh-text", i))
                .relative()
                .when(collapsed, |d| d.max_h(px(13. * 4.4)).overflow_hidden())
                .when(long, |d| d.cursor_pointer().on_click(move |_, w, cx| toggle(i, w, cx)))
                .child(inline(atoms_in(&texts[0], &h.tags, store, media, text_look()), text_look()))
                .when(collapsed, |d| {
                    d.child(div().absolute().left_0().right_0().bottom_0().h(px(26.)).bg(gpui::linear_gradient(
                        180.,
                        gpui::linear_color_stop(ca(0x14, 0x14, 0x14, 0.), 0.),
                        gpui::linear_color_stop(ca(0x14, 0x14, 0x14, 1.), 1.),
                    )))
                })
        };
        let last = i == 0;
        div()
            .relative()
            .pl(px(16.))
            .when(!last, |d| d.pb(px(12.)))
            // The rail between dots, and this revision's dot.
            .when(!last, |d| d.child(div().absolute().left(px(3.)).top(px(14.)).bottom(px(-2.)).w(px(1.)).bg(white_a(0.09))))
            .child(div().absolute().left_0().top(px(5.)).size(px(7.)).rounded_full().bg(if current { c(PRIMARY) } else { white_a(0.22) }))
            .child(head)
            .child(body)
    });
    let width = if sheet { vw } else { 400f32.min(vw - 20.) };
    let cap = 560f32.min(vh * 0.7);
    // Placed by the room either side of the row, so it grows from its anchored edge.
    let (above, below) = (f32::from(h.anchor.top()) - 44., vh - f32::from(h.anchor.bottom()) - 16.);
    let up = above >= cap.min(320.) || above >= below;
    let left = f32::from(h.anchor.left()).min(vw - width - 10.).max(10.);
    let (pos, corner, max_h) = if sheet {
        (point(px(0.), px(vh)), gpui::Anchor::BottomLeft, vh * 0.75)
    } else if above.max(below) < 260. {
        let max_h = cap.min(vh - 60.);
        (point(px(left), px(44f32.max((vh - max_h) / 2.))), gpui::Anchor::TopLeft, max_h)
    } else if up {
        (point(px(left), h.anchor.top() - px(6.)), gpui::Anchor::BottomLeft, cap.min(above))
    } else {
        (point(px(left), h.anchor.bottom() + px(6.)), gpui::Anchor::TopLeft, cap.min(below))
    };
    let (close, close_btn) = (on_close.clone(), on_close.clone());
    let list_id = SharedString::from(format!("edit-history-{}", h.msg));
    let panel = div()
        .id("edit-history-popup")
        .occlude()
        .w(px(width))
        .max_h(px(max_h))
        .flex()
        .flex_col()
        .bg(c(0x141414))
        .border_1()
        .border_color(white_a(0.06))
        .map(|d| if sheet { d.rounded_tl(px(16.)).rounded_tr(px(16.)).border_b_0() } else { d.rounded(px(12.)) })
        .shadow(vec![gpui::BoxShadow {
            color: ca(0, 0, 0, 0.55),
            offset: point(px(0.), px(8.)),
            blur_radius: px(28.),
            spread_radius: px(0.),
            inset: false,
        }])
        .overflow_hidden()
        .font_family(INTER)
        .on_mouse_down_out(move |_, w, cx| close(w, cx))
        .child(
            div()
                .flex()
                .flex_shrink_0()
                .items_center()
                .justify_between()
                .pt(px(10.))
                .pr(px(8.))
                .pb(px(6.))
                .pl(px(14.))
                .text_size(px(12.))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(white_a(0.5))
                .child("Edit history")
                .child(
                    div()
                        .id("eh-close")
                        .size(px(26.))
                        .rounded_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .hover(|d| d.bg(white_a(0.07)))
                        .on_click(move |_, w, cx| close_btn(w, cx))
                        .child(gpui::svg().path("icons/x.svg").size(px(14.)).text_color(white_a(0.5))),
                ),
        )
        .child(
            div()
                .id(list_id)
                .track_scroll(&h.scroll)
                .min_h_0()
                .flex_shrink(1.)
                .overflow_y_scroll()
                .px(px(14.))
                .pb(px(12.))
                .children(revisions),
        );
    let _ = &h.size;
    gpui::deferred(
        div()
            .when(sheet, |d| {
                d.child(gpui::anchored().position(point(px(0.), px(0.))).child(div().w(px(vw)).h(px(vh)).bg(ca(0, 0, 0, 0.45))))
            })
            .child(gpui::anchored().anchor(corner).position(pos).child(panel)),
    )
    .with_priority(12)
    .into_any_element()
}
