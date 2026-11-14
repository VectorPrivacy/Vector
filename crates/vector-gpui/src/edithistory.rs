//! `EditHistoryPopup.svelte`: a message's revisions as a timeline, newest first, each edit as
//! what it changed. Placed by the room either side of the row the `(edited)` mark sits in, or a
//! bottom sheet on a narrow window.

use std::cell::Cell;
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{AnyElement, App, Bounds, Pixels, SharedString, Size, Window, div, point, px};

use crate::data::Store;
use crate::inline::{Atom, Kind, Look};
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

// Diff runs ride through markdown as private-use sentinels, then become their looks.
const DEL: char = '\u{F0200}';
const INS: char = '\u{F0201}';
const END: char = '\u{F0202}';
const GAP: char = '\u{F0203}';

/// A line's block marker (`#`, `>`, `-`, `1.`) at the start of `text`, kept outside a change so
/// a changed heading or quote still parses.
fn block_marker(text: &str) -> usize {
    let b = text.as_bytes();
    let mut i = b.iter().take_while(|c| matches!(c, b' ' | b'\t')).count();
    let ws = |i: usize| matches!(b.get(i), Some(b' ' | b'\t'));
    match b.get(i) {
        Some(b'#') => {
            let n = b[i..].iter().take_while(|c| **c == b'#').count();
            if n <= 6 && ws(i + n) { i + n + 1 } else { 0 }
        }
        Some(b'>') => {
            i += 1;
            if ws(i) { i + 1 } else { i }
        }
        Some(b'-' | b'*' | b'+') if ws(i + 1) => i + 2,
        Some(c) if c.is_ascii_digit() => {
            let n = b[i..].iter().take_while(|c| c.is_ascii_digit()).count();
            if matches!(b.get(i + n), Some(b'.' | b')')) && ws(i + n + 1) { i + n + 2 } else { 0 }
        }
        _ => 0,
    }
}

/// The diff as one markdown source, changes and folds marked with sentinels.
fn diff_source(runs: &[crate::textdiff::Run]) -> String {
    use crate::textdiff::{Op, Run};
    let mut src = String::new();
    for run in runs {
        match run {
            Run::Gap { lines: true, .. } => src.extend(['\n', GAP, '\n']),
            Run::Gap { .. } => src.extend([' ', GAP, ' ']),
            Run::Text(Op::Eq, t) => src.push_str(t),
            Run::Text(op, t) => {
                let lead = if src.is_empty() || src.ends_with('\n') { block_marker(t) } else { 0 };
                src.push_str(&t[..lead]);
                src.push(if *op == Op::Ins { INS } else { DEL });
                src.push_str(&t[lead..]);
                src.push(END);
            }
        }
    }
    src
}

fn text_look() -> Look {
    Look::new(INTER, 13., 18.85, white_a(0.85))
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Eq,
    Ins,
    Del,
}

/// Splits each text atom at the sentinels; a change can cross blocks, so the mode carries on.
fn mark_atoms(atoms: &[Atom], mode: &mut Mode) -> Vec<Atom> {
    let mut out = Vec::with_capacity(atoms.len());
    for atom in atoms {
        let Atom::Text { text, look, kind } = atom else {
            out.push(atom.clone());
            continue;
        };
        let mut buf = String::new();
        let flush = |buf: &mut String, mode: Mode, out: &mut Vec<Atom>| {
            if buf.is_empty() {
                return;
            }
            let push = |t: &str, look: Look, out: &mut Vec<Atom>| {
                if !t.is_empty() {
                    out.push(Atom::Text { text: t.to_string().into(), look, kind: kind.clone() });
                }
            };
            let mut changed = look.clone();
            match mode {
                Mode::Eq => {
                    push(buf, look.clone(), out);
                    buf.clear();
                    return;
                }
                Mode::Ins => {
                    changed.color = c(0xffffff);
                    changed.bg = Some(c(PRIMARY).opacity(0.2));
                }
                Mode::Del => {
                    changed.color = c(0xff6dc3);
                    changed.bg = Some(c(crate::call::DANGER).opacity(0.12));
                    changed.strike = true;
                }
            }
            // Spaces at a change's edges aren't part of it: struck, they read as a dash.
            let body = buf.trim();
            let lead = &buf[..buf.len() - buf.trim_start().len()];
            let trail = if body.is_empty() { "" } else { &buf[buf.trim_end().len()..] };
            push(lead, look.clone(), out);
            push(body, changed, out);
            push(trail, look.clone(), out);
            buf.clear();
        };
        for ch in text.chars() {
            match ch {
                DEL | INS | END => {
                    flush(&mut buf, *mode, &mut out);
                    *mode = match ch {
                        DEL => Mode::Del,
                        INS => Mode::Ins,
                        _ => Mode::Eq,
                    };
                }
                GAP => {
                    flush(&mut buf, *mode, &mut out);
                    let mut dim = look.clone();
                    dim.color = white_a(0.35);
                    dim.strike = false;
                    dim.bg = None;
                    out.push(Atom::Text { text: "…".into(), look: dim, kind: Kind::Plain });
                }
                _ => buf.push(ch),
            }
        }
        flush(&mut buf, *mode, &mut out);
    }
    out
}

fn mark_blocks(blocks: &mut [crate::markdown::Block], mode: &mut Mode) {
    use crate::markdown::Block;
    let para = |p: &mut crate::markdown::Para, mode: &mut Mode| p.atoms = mark_atoms(&p.atoms, mode).into();
    for b in blocks {
        match b {
            Block::Para(p) => para(p, mode),
            Block::Quote(inner) => mark_blocks(inner, mode),
            Block::List { items, .. } => items.iter_mut().for_each(|i| mark_blocks(i, mode)),
            Block::Table { head, rows, .. } => {
                head.iter_mut().chain(rows.iter_mut().flatten()).for_each(|p| para(p, mode));
            }
            Block::Code { text, .. } => {
                if text.contains([DEL, INS, END, GAP]) {
                    *text = text.replace([DEL, INS, END, GAP], "").into();
                }
            }
            Block::Rule => {}
        }
    }
}

/// A revision as the chat renders it: markdown, mentions, emoji; diff sentinels drawn as runs.
fn rendered(source: &str, tags: &[(String, String)], store: &Store, media: &std::cell::RefCell<crate::media::Media>, base: Look, key: &str) -> AnyElement {
    let revealed = std::collections::HashSet::new();
    let custom = |code: &str| {
        let (_, url) = tags.iter().find(|(c, _)| c == code)?;
        Some(media.borrow_mut().custom(&crate::timeline::emoji_source(store, url), false))
    };
    let mut blocks = crate::markdown::parse(
        source.trim(),
        &crate::markdown::Ctx {
            base,
            revealed: &revealed,
            now: store.now_ms() as f64 / 1000.,
            mention: Some(&|npub: &str| store.name_in(npub, None)),
            tags: &[],
            mask: None,
            colors: &[],
            go: None,
            custom: Some(&custom),
        },
    );
    mark_blocks(&mut blocks, &mut Mode::Eq);
    let click: crate::inline::OnClick = Rc::new(|_, _, _| {});
    crate::timeline::stack(&blocks, key, &click, 0., 18.85)
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
                .child(rendered(&diff_source(&runs), &h.tags, store, media, { let mut l = text_look(); l.color = white_a(0.6); l }, &format!("eh-{i}")))
        } else {
            // The original: a short preview, whole on a click once it's cut short.
            let long = texts[0].len() > 140 || texts[0].matches('\n').count() > 2;
            let collapsed = !open && long;
            div()
                .id(("eh-text", i))
                .relative()
                .when(collapsed, |d| d.max_h(px(13. * 4.4)).overflow_hidden())
                .when(long, |d| d.cursor_pointer().on_click(move |_, w, cx| toggle(i, w, cx)))
                .child(rendered(&texts[0], &h.tags, store, media, text_look(), "eh-0"))
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
