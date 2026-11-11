//! The `:shortcode` autocomplete (`emoji-shortcodes.js`, `ShortcodePopup.svelte`).

use std::rc::Rc;

use gpui::{
    AnyElement, App, FontWeight, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, Window, div, img, prelude::*, px,
};

use crate::media::{Custom, Media};
use crate::picker::{Cell, EmojiPicker};
use crate::theme::*;

/// Text emoticons, keyed on what follows the `:`.
const EMOTICONS: &[(&str, &str)] = &[
    (")", "🙂"),
    ("-)", "🙂"),
    ("(", "🙁"),
    ("-(", "🙁"),
    ("'(", "😢"),
    ("D", "😄"),
    ("-D", "😄"),
    ("P", "😛"),
    ("p", "😛"),
    ("-P", "😛"),
    ("-p", "😛"),
    ("3", "😺"),
    ("o", "😮"),
    ("O", "😮"),
    ("-o", "😮"),
    ("-O", "😮"),
    ("|", "😐"),
    ("-|", "😐"),
    ("/", "😕"),
    ("\\", "😕"),
    ("*", "😘"),
    ("-*", "😘"),
];

const SHOWN: usize = 5;

pub struct Popup {
    /// The `:`'s offset in the draft.
    pub start: usize,
    pub header: &'static str,
    pub items: Vec<Cell>,
    pub active: usize,
}

/// The `:` the caret is typing after: one at the start or after whitespace, with no space since.
/// Its offset, and what follows it less a closing `:`.
pub fn trigger(text: &str, caret: usize) -> Option<(usize, &str)> {
    let before = text.get(..caret)?;
    for (i, ch) in before.char_indices().rev() {
        if ch == ':' {
            let opens = before[..i]
                .chars()
                .next_back()
                .is_none_or(char::is_whitespace);
            if !opens {
                return None;
            }
            let q = &before[i + 1..];
            return Some((i, q.strip_suffix(':').unwrap_or(q)));
        }
        if ch.is_whitespace() {
            return None;
        }
    }
    None
}

/// What the draft's `:` suggests, or None for no panel.
pub fn suggest(text: &str, caret: usize, picker: &EmojiPicker, emoticons: bool) -> Option<Popup> {
    let (start, query) = trigger(text, caret)?;
    let filtered = || {
        let mut v = if query.is_empty() {
            picker.recents()
        } else {
            picker.search(query, false)
        };
        v.truncate(SHOWN);
        v
    };
    let items = match EMOTICONS.iter().find(|(k, _)| *k == query) {
        Some(_) if !emoticons => return None,
        Some((_, ch)) => {
            let first = crate::emoji::by_char(ch)?;
            let mut v = vec![Cell::Stock(first)];
            v.extend(
                filtered()
                    .into_iter()
                    .filter(|c| !matches!(c, Cell::Stock(e) if e.emoji == *ch)),
            );
            v.truncate(SHOWN);
            v
        }
        None => filtered(),
    };
    (!items.is_empty()).then(|| Popup {
        start,
        header: if query.is_empty() {
            "Recently Used"
        } else {
            "Emojis"
        },
        items,
        active: 0,
    })
}

/// What a pick puts in place of `:query`.
pub fn insert(cell: &Cell) -> String {
    match cell {
        Cell::Stock(e) => format!("{} ", e.emoji),
        Cell::Custom(e) => format!(":{}: ", e.code),
    }
}

/// `.emoji-shortcode-selector`, above the composer box.
pub fn view(
    p: &Popup,
    media: &mut Media,
    pick: Rc<dyn Fn(usize, &mut Window, &mut App)>,
) -> AnyElement {
    let rows = p.items.iter().enumerate().map(|(i, cell)| {
        let (art, label): (AnyElement, String) = match cell {
            Cell::Stock(e) => (
                match crate::emoji::twemoji(e.emoji) {
                    Some(path) => img(path).size(px(28.)).flex_shrink_0().into_any_element(),
                    None => div()
                        .size(px(28.))
                        .flex_shrink_0()
                        .text_size(px(22.))
                        .line_height(px(28.))
                        .text_center()
                        .child(e.emoji)
                        .into_any_element(),
                },
                format!(":{}:", e.shortcode),
            ),
            Cell::Custom(e) => (
                match media.custom(&e.src, false) {
                    Custom::Ready(i) => img(gpui::ImageSource::Render(i))
                        .size(px(28.))
                        .flex_shrink_0()
                        .object_fit(gpui::ObjectFit::Contain)
                        .into_any_element(),
                    _ => div().size(px(28.)).flex_shrink_0().into_any_element(),
                },
                format!(":{}:", e.code),
            ),
        };
        let active = i == p.active;
        let pick = pick.clone();
        div()
            .id(SharedString::from(format!("shortcode-{i}")))
            .relative()
            .flex()
            .items_center()
            .gap(px(10.))
            .px(px(12.))
            .py(px(8.))
            .cursor_pointer()
            .text_size(px(14.))
            .text_color(white_a(0.9))
            .hover(|s| s.bg(white_a(0.08)))
            .when(active, |d| {
                d.bg(white_a(0.08)).child(
                    div()
                        .absolute()
                        .left_0()
                        .top(px(4.))
                        .bottom(px(4.))
                        .w(px(3.))
                        .rounded_r(px(3.))
                        .bg(c(ACCENT)),
                )
            })
            .on_mouse_down(gpui::MouseButton::Left, move |_, w, cx| {
                cx.stop_propagation();
                pick(i, w, cx)
            })
            .child(art)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .child(label),
            )
    });
    div()
        .id("emoji-shortcode-selector")
        .occlude()
        .w_full()
        .max_w(px(342.))
        .py(px(6.))
        .bg(c(0x0a0a0a))
        .border_1()
        .border_color(ca(90, 252, 180, 0.36))
        .rounded(px(12.))
        .overflow_hidden()
        .font_family(RUBIK)
        .shadow(vec![gpui::BoxShadow {
            color: ca(0, 0, 0, 0.5),
            offset: gpui::point(px(0.), px(-8.)),
            blur_radius: px(32.),
            spread_radius: px(0.),
            inset: false,
        }])
        .child(
            div()
                .pt(px(6.))
                .px(px(12.))
                .pb(px(4.))
                .text_size(px(11.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(c(PRIMARY))
                .opacity(0.7)
                .child(p.header.to_uppercase()),
        )
        .children(rows)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::trigger;

    #[test]
    fn a_colon_opens_only_after_space_or_at_the_start() {
        assert_eq!(trigger(":sm", 3), Some((0, "sm")));
        assert_eq!(trigger("hi :sm", 6), Some((3, "sm")));
        assert_eq!(trigger("hi:sm", 5), None);
        assert_eq!(trigger("hi :sm x", 8), None);
        assert_eq!(trigger("a :)", 4), Some((2, ")")));
        assert_eq!(trigger("a :cat:", 7), None);
    }
}
