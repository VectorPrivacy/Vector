//! `EditHistoryPopup.svelte`: a message's revisions, oldest first, laid against the row the
//! `(edited)` mark sits in: above it, else below when there's no room. Measured once shown,
//! the way the web popup is, so the side is known before it paints.

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
}

pub type OnClose = Rc<dyn Fn(&mut Window, &mut App)>;

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
fn atoms(text: &str, tags: &[(String, String)], store: &Store, media: &std::cell::RefCell<crate::media::Media>) -> Vec<Atom> {
    let look = Look::new(INTER, 12., 15.6, white_a(0.85));
    let size = look.size * 1.15;
    let mut out = Vec::new();
    let mut plain = |t: &str, out: &mut Vec<Atom>| {
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

pub fn render(
    h: &EditHistory,
    store: &Store,
    media: &std::cell::RefCell<crate::media::Media>,
    viewport: Size<Pixels>,
    on_close: &OnClose,
) -> AnyElement {
    let total = h.entries.len();
    let entries = h.entries.iter().enumerate().map(|(i, e)| {
        let label = if i == 0 {
            Some("ORIGINAL")
        } else if i + 1 == total {
            Some("CURRENT")
        } else {
            None
        };
        let text = store.stream_mask(&e.content).unwrap_or_else(|| e.content.clone());
        div()
            .when(i > 0, |d| d.mt(px(6.)))
            .px(px(10.))
            .py(px(8.))
            .bg(c(0x0f0f0f))
            .rounded(px(6.))
            .border_l(px(3.))
            .border_color(c(if i + 1 == total && total > 1 {
                PRIMARY
            } else if i == 0 {
                0x666666
            } else {
                0x555555
            }))
            .child(
                div()
                    .flex()
                    .items_baseline()
                    .text_size(px(10.))
                    .line_height(px(12.))
                    .text_color(white_a(0.4))
                    .child(when(e.edited_at))
                    .children(label.map(|l| div().ml(px(6.)).text_size(px(9.)).text_color(white_a(0.3)).child(l))),
            )
            .child(div().mt(px(2.)).child(inline(atoms(&text, &h.tags, store, media), Look::new(INTER, 12., 15.6, white_a(0.85)))))
    });
    let measured = h.size.get();
    let (left, top) = match measured {
        Some(sz) => {
            let above = f32::from(h.anchor.top()) - f32::from(sz.height) - 4.;
            let y = if above < 10. { f32::from(h.anchor.bottom()) + 4. } else { above };
            let x = f32::from(h.anchor.left()).min(f32::from(viewport.width - sz.width) - 10.).max(10.);
            (x, y)
        }
        None => (0., 0.),
    };
    let (probe, scroll) = (h.size.clone(), h.scroll.clone());
    let close = on_close.clone();
    let list_id = SharedString::from(format!("edit-history-{}", h.msg));
    gpui::deferred(
        gpui::anchored().position(point(px(left), px(top))).child(
            div()
                .id("edit-history-popup")
                .occlude()
                .when(measured.is_none(), |d| d.invisible())
                .min_w(px(280.))
                .max_w(px(400.))
                .max_h(px(300.))
                .rounded(px(12.))
                .bg(c(0x1a1a1a))
                .shadow(vec![gpui::BoxShadow {
                    color: ca(0, 0, 0, 0.5),
                    offset: point(px(0.), px(4.)),
                    blur_radius: px(20.),
                    spread_radius: px(0.),
                    inset: false,
                }])
                .overflow_hidden()
                .font_family(INTER)
                .on_mouse_down_out(move |_, w, cx| close(w, cx))
                .child(
                    gpui::canvas(
                        move |b, window, _| {
                            if probe.get() != Some(b.size) {
                                // The latest revision sits at the bottom.
                                if probe.get().is_none() {
                                    scroll.scroll_to_bottom();
                                }
                                probe.set(Some(b.size));
                                window.request_animation_frame();
                            }
                        },
                        |_, _, _, _| {},
                    )
                    .absolute()
                    .size_full(),
                )
                .child(div().id(list_id).track_scroll(&h.scroll).p(px(8.)).max_h(px(280.)).overflow_y_scroll().children(entries)),
        ),
    )
    .with_priority(12)
    .into_any_element()
}
