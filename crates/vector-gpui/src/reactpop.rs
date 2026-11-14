//! `ReactionPopups.svelte`: a reaction chip's hover tip ("reacted by Alice and Bob") and its
//! details popup (who reacted), each above the chip, else below, kept inside the chat pane.
//! Both measure themselves on their first frame and place on the next.

use std::cell::Cell;
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{AnyElement, App, Bounds, FontWeight, ImageSource, Pixels, SharedString, Size, Window, div, point, px};
use crate::pic::img;

use crate::data::Store;
use crate::theme::*;

pub struct Pop {
    pub msg: String,
    pub emoji: String,
    /// The chip's anchor key.
    pub key: SharedString,
    pub size: Rc<Cell<Option<Size<Pixels>>>>,
}

impl Pop {
    pub fn new(msg: String, emoji: String) -> Self {
        let key = chip_key(&msg, &emoji);
        Self { msg, emoji, key, size: Rc::default() }
    }
}

pub fn chip_key(msg: &str, emoji: &str) -> SharedString {
    format!("rx:{msg}:{emoji}").into()
}

fn find<'a>(store: &'a Store, id: &str) -> Option<(&'a str, &'a vector_core::Message)> {
    store.chats.iter().find_map(|c| c.messages.iter().find(|m| m.id == id).map(|m| (c.id.as_str(), m)))
}

/// 1 → "Alice"; 2 → "Alice and Bob"; 3 → all three; 4+ → three and "N others".
fn names(list: &[String]) -> String {
    match list {
        [a] => a.clone(),
        [a, b] => format!("{a} and {b}"),
        [a, b, c] => format!("{a}, {b}, and {c}"),
        _ => {
            let others = list.len() - 3;
            format!("{}, and {others} other{}", list[..3].join(", "), if others == 1 { "" } else { "s" })
        }
    }
}

/// The chip's glyph: its custom picture, else the twemoji, else the text.
fn glyph(emoji: &str, custom: Option<ImageSource>, size: f32) -> AnyElement {
    match custom.or_else(|| crate::emoji::twemoji(emoji).map(Into::into)) {
        Some(src) => img(src).size(px(size)).flex_shrink_0().into_any_element(),
        None => div().text_size(px(size * 0.8)).child(emoji.to_string()).into_any_element(),
    }
}

/// Above the anchor by `gap`, else below; centred or left-aligned, clamped to the pane.
fn place(pop: &Pop, anchor: Bounds<Pixels>, pane: Bounds<Pixels>, gap: f32, centred: bool) -> (f32, f32) {
    let Some(sz) = pop.size.get() else { return (0., 0.) };
    let (w, h) = (f32::from(sz.width), f32::from(sz.height));
    let mut top = f32::from(anchor.top()) - h - gap;
    if top < 10. {
        top = f32::from(anchor.bottom()) + gap;
    }
    let left = if centred { f32::from(anchor.center().x) - w / 2. } else { f32::from(anchor.left()) };
    let min = f32::from(pane.left()) + 10.;
    let max = f32::from(pane.right()) - w - 10.;
    (left.min(max).max(min), top)
}

fn shell(pop: &Pop, at: (f32, f32), body: gpui::Stateful<gpui::Div>) -> AnyElement {
    let probe = pop.size.clone();
    let measured = probe.get().is_some();
    gpui::deferred(
        gpui::anchored().position(point(px(at.0), px(at.1))).child(
            body.when(!measured, |d| d.invisible()).child(
                gpui::canvas(
                    move |b, window, _| {
                        if probe.get() != Some(b.size) {
                            probe.set(Some(b.size));
                            window.request_animation_frame();
                        }
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            ),
        ),
    )
    .with_priority(11)
    .into_any_element()
}

pub fn tip(pop: &Pop, store: &Store, custom: Option<ImageSource>, anchor: Bounds<Pixels>, pane: Bounds<Pixels>) -> Option<AnyElement> {
    let (chat, msg) = find(store, &pop.msg)?;
    let who: Vec<String> = store
        .counted(chat, msg)
        .filter(|r| r.emoji == pop.emoji)
        .map(|r| store.chat_name(&r.author_id, chat))
        .collect();
    if who.is_empty() {
        return None;
    }
    let body = div()
        .id("reaction-tip")
        .relative()
        .max_w(px(320.))
        .flex()
        .items_center()
        .gap(px(6.))
        .px(px(10.))
        .py(px(6.))
        .bg(c(0x1a1a1a))
        .border_1()
        .border_color(white_a(0.08))
        .rounded(px(8.))
        .shadow(vec![gpui::BoxShadow {
            color: ca(0, 0, 0, 0.4),
            offset: point(px(0.), px(4.)),
            blur_radius: px(12.),
            spread_radius: px(0.),
            inset: false,
        }])
        .font_family(INTER)
        .text_size(px(13.))
        .line_height(px(17.))
        .text_color(white_a(0.92))
        .child(glyph(&pop.emoji, custom, 16.))
        .child(div().min_w_0().truncate().child(format!("reacted by {}", names(&who))))
        .child(
            div()
                .flex_shrink_0()
                .ml(px(4.))
                .pl(px(8.))
                .border_l_1()
                .border_color(white_a(0.1))
                .text_size(px(11.))
                .text_color(white_a(0.4))
                .child("Right-click for details"),
        );
    Some(shell(pop, place(pop, anchor, pane, 6., true), body))
}

pub fn details(
    pop: &Pop,
    store: &Store,
    custom: Option<ImageSource>,
    anchor: Bounds<Pixels>,
    pane: Bounds<Pixels>,
    on_close: Rc<dyn Fn(&mut Window, &mut App)>,
) -> Option<AnyElement> {
    let (chat, msg) = find(store, &pop.msg)?;
    let reactors: Vec<&str> = store.counted(chat, msg).filter(|r| r.emoji == pop.emoji).map(|r| r.author_id.as_str()).collect();
    if reactors.is_empty() {
        return None;
    }
    // A custom emoji is named by its shortcode; a stock one by its display name.
    let label = if custom.is_some() {
        pop.emoji.clone()
    } else {
        crate::emoji::EMOJIS
            .iter()
            .find(|e| e.emoji == pop.emoji)
            .map(|e| if e.display.is_empty() { e.name } else { e.display })
            .map(|n| {
                let mut c = n.chars();
                c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
            })
            .unwrap_or_default()
    };
    let rows = reactors.into_iter().map(|npub| {
        div()
            .id(SharedString::from(format!("rx-who-{npub}")))
            .flex()
            .items_center()
            .gap(px(10.))
            .px(px(14.))
            .py(px(6.))
            .hover(|d| {
                d.bg(gpui::linear_gradient(
                    90.,
                    gpui::linear_color_stop(white_a(0.04), 0.),
                    gpui::linear_color_stop(white_a(0.), 1.),
                ))
            })
            .child(div().flex_shrink_0().child(crate::ui::avatar(store, npub, 25.)))
            .child(div().min_w_0().truncate().text_size(px(13.)).text_color(c(0xe0e0e0)).child(store.chat_name(npub, chat)))
    });
    let close = on_close.clone();
    let body = div()
        .id("reaction-details")
        .relative()
        .occlude()
        .min_w(px(180.))
        .max_w(px(320.))
        .max_h(px(250.))
        .py(px(4.))
        .bg(c(0x1a1a1a))
        .border_1()
        .border_color(white_a(0.08))
        .rounded(px(12.))
        .shadow(vec![gpui::BoxShadow {
            color: ca(0, 0, 0, 0.5),
            offset: point(px(0.), px(8.)),
            blur_radius: px(32.),
            spread_radius: px(0.),
            inset: false,
        }])
        .font_family(INTER)
        .on_mouse_down_out(move |_, w, cx| close(w, cx))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .px(px(14.))
                .pt(px(6.))
                .pb(px(8.))
                .mb(px(4.))
                .border_b_1()
                .border_color(white_a(0.06))
                .child(glyph(&pop.emoji, custom, 18.))
                .children((!label.is_empty()).then(|| {
                    div()
                        .min_w_0()
                        .truncate()
                        .text_size(px(13.))
                        .font_weight(FontWeight::NORMAL)
                        .text_color(white_a(0.45))
                        .child(label)
                })),
        )
        .child(div().id("reaction-details-list").overflow_y_scroll().max_h(px(200.)).children(rows));
    Some(shell(pop, place(pop, anchor, pane, 4., false), body))
}
