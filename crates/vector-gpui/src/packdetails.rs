//! The Pack Details modal (`PackDetailsOverlay.svelte`, `PackDetailsModal.svelte`): a pack a
//! `vector://emojis/pack/<naddr>` link names, with its emojis and Add or Remove.

use std::rc::Rc;

use gpui::{
    AnyElement, App, FontWeight, InteractiveElement, IntoElement, ParentElement, ScrollHandle,
    StatefulInteractiveElement, Styled, Window, div, img, prelude::*, px, svg,
};

use crate::media::{Custom, Media};
use crate::packs::State;
use crate::theme::*;

const DANGER: u32 = 0xff2ea9;

pub struct Details {
    pub naddr: String,
    pub scroll: ScrollHandle,
}

impl Details {
    pub fn new(naddr: String) -> Self {
        Self {
            naddr,
            scroll: ScrollHandle::new(),
        }
    }
}

pub enum Act {
    Close,
    Toggle,
}

pub type On = Rc<dyn Fn(Act, &mut Window, &mut App)>;

/// `src` maps a picture's URL to where it is read from.
pub fn view(
    d: &Details,
    max_display: usize,
    media: &mut Media,
    src: &dyn Fn(&str) -> String,
    on: On,
) -> AnyElement {
    let state = media.packs.get(&d.naddr);
    let body: AnyElement = match &state {
        State::Loading => div()
            .py(px(48.))
            .px(px(20.))
            .flex()
            .flex_col()
            .items_center()
            .gap(px(14.))
            .child(div().size(px(28.)).child(crate::attachments::spinner(
                28.,
                2.5,
                0.9,
                0.28,
                ca(89, 252, 179, 0.2),
                c(PRIMARY),
            )))
            .child(
                div()
                    .text_size(px(13.))
                    .text_color(white_a(0.55))
                    .child("Loading pack…"),
            )
            .into_any_element(),
        State::Failed(e, _) => div()
            .py(px(40.))
            .px(px(20.))
            .flex()
            .flex_col()
            .items_center()
            .child(
                div()
                    .mb(px(8.))
                    .text_size(px(16.))
                    .font_weight(FontWeight::BOLD)
                    .text_color(c(DANGER))
                    .child("Pack unavailable"),
            )
            .child(
                div()
                    .text_size(px(13.))
                    .text_color(white_a(0.55))
                    .child(if e.is_empty() {
                        "Failed to fetch".to_string()
                    } else {
                        e.clone()
                    }),
            )
            .into_any_element(),
        State::Ready(pack) => {
            let title = [&pack.title, &pack.identifier]
                .into_iter()
                .find(|t| !t.is_empty())
                .cloned()
                .unwrap_or_else(|| "Untitled".into());
            let letter = title
                .trim()
                .chars()
                .next()
                .map(|c| c.to_uppercase().to_string())
                .unwrap_or_default();
            let logo =
                (!pack.image_url.is_empty()).then(|| media.custom(&src(&pack.image_url), true));
            let logo_ready = matches!(logo, Some(Custom::Ready(_)));
            let logo_face: AnyElement = match logo {
                Some(Custom::Ready(i)) => img(gpui::ImageSource::Render(i))
                    .size_full()
                    .map(|i| gpui::StyledImage::object_fit(i, gpui::ObjectFit::Cover))
                    .into_any_element(),
                _ => div()
                    .text_size(px(28.))
                    .font_weight(FontWeight::BOLD)
                    .text_color(c(PRIMARY))
                    .child(letter)
                    .into_any_element(),
            };
            let n = pack.emojis.len();
            let thumbs: Vec<AnyElement> = pack
                .emojis
                .iter()
                .take(max_display)
                .filter_map(|e| match media.custom(&src(&e.url), false) {
                    Custom::Missing(_) => None,
                    Custom::Ready(i) => Some(
                        img(gpui::ImageSource::Render(i))
                            .size_full()
                            .map(|i| gpui::StyledImage::object_fit(i, gpui::ObjectFit::Contain))
                            .into_any_element(),
                    ),
                    Custom::Pending => Some(div().into_any_element()),
                })
                .collect();
            let mut grid = div().flex().flex_col().gap(px(6.));
            if thumbs.is_empty() {
                grid = grid.child(
                    div()
                        .py(px(28.))
                        .px(px(12.))
                        .text_center()
                        .text_size(px(13.))
                        .text_color(white_a(0.4))
                        .child("Empty pack"),
                );
            }
            let mut it = thumbs.into_iter().peekable();
            while it.peek().is_some() {
                let mut row = div().flex().gap(px(6.));
                let mut k = 0;
                for t in it.by_ref().take(6) {
                    row = row.child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .aspect_ratio(1.)
                            .p(px(6.))
                            .rounded(px(6.))
                            .child(t),
                    );
                    k += 1;
                }
                for _ in k..6 {
                    row = row.child(div().flex_1().min_w_0().p(px(6.)));
                }
                grid = grid.child(row);
            }
            let busy = media.packs.busy.contains(&pack.id);
            let subscribed = media.packs.subscribed.contains(&pack.id);
            let label = match (busy, subscribed) {
                (true, true) => "Removing…",
                (true, false) => "Adding…",
                (false, true) => "Remove Pack",
                (false, false) => "Add Pack",
            };
            let toggle = on.clone();
            div()
                .child(
                    div()
                        .mb(px(16.))
                        .flex()
                        .items_center()
                        .gap(px(14.))
                        .child(
                            div()
                                .size(px(64.))
                                .flex_shrink_0()
                                .rounded(px(14.))
                                .overflow_hidden()
                                .when(!logo_ready, |d| d.bg(white_a(0.04)))
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(logo_face),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child(
                                    div()
                                        .mb(px(4.))
                                        .text_size(px(18.))
                                        .line_height(px(24.))
                                        .font_weight(FontWeight::BOLD)
                                        .text_color(c(0xffffff))
                                        .whitespace_nowrap()
                                        .overflow_hidden()
                                        .text_ellipsis()
                                        .child(title),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .text_size(px(12.))
                                        .line_height(px(24.))
                                        .text_color(white_a(0.5))
                                        .child(
                                            div()
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .text_color(c(PRIMARY))
                                                .child(n.to_string()),
                                        )
                                        .child(format!(
                                            "\u{a0}Emoji{}",
                                            if n == 1 { "" } else { "s" }
                                        )),
                                ),
                        ),
                )
                .when(!pack.description.is_empty(), |d| {
                    d.child(
                        div()
                            .mb(px(16.))
                            .text_size(px(13.))
                            .line_height(px(19.5))
                            .text_color(white_a(0.7))
                            .child(pack.description.clone()),
                    )
                })
                .child(
                    div()
                        .id("pack-details-grid")
                        .mb(px(18.))
                        .p(px(12.))
                        .rounded(px(12.))
                        .bg(ca(0, 0, 0, 0.35))
                        .max_h(px(240.))
                        .overflow_y_scroll()
                        .track_scroll(&d.scroll)
                        .child(grid),
                )
                .child(
                    div()
                        .id("pack-details-action")
                        .w_full()
                        .p(px(14.))
                        .rounded(px(12.))
                        .flex()
                        .justify_center()
                        .text_size(px(15.))
                        .line_height(px(19.))
                        .font_weight(FontWeight::BOLD)
                        .when(!subscribed, |d| d.bg(c(PRIMARY)).text_color(c(0x0a0a0a)))
                        .when(subscribed, |d| {
                            d.bg(ca(255, 46, 169, 0.1))
                                .border_1()
                                .border_color(ca(255, 46, 169, 0.4))
                                .text_color(c(DANGER))
                        })
                        .when(busy, |d| d.opacity(0.55))
                        .when(!busy, |d| {
                            d.cursor_pointer()
                                .hover(|s| s.opacity(0.92))
                                .on_click(move |_, w, cx| toggle(Act::Toggle, w, cx))
                        })
                        .child(label),
                )
                .into_any_element()
        }
    };
    let close = on.clone();
    let backdrop = on;
    div()
        .id("pack-details-overlay")
        .occlude()
        .absolute()
        .inset_0()
        .p(px(24.))
        .flex()
        .items_center()
        .justify_center()
        .bg(ca(0, 0, 0, 0.78))
        .font_family(INTER)
        .on_click(move |_, w, cx| backdrop(Act::Close, w, cx))
        .child(
            div()
                .id("pack-details-card")
                .relative()
                .w_full()
                .max_w(px(442.))
                .bg(c(0x141414))
                .border_1()
                .border_color(white_a(0.08))
                .rounded(px(18.))
                .overflow_hidden()
                .shadow(vec![gpui::BoxShadow {
                    color: ca(0, 0, 0, 0.6),
                    offset: gpui::point(px(0.), px(24.)),
                    blur_radius: px(60.),
                    spread_radius: px(0.),
                    inset: false,
                }])
                .on_click(|_, _, cx| cx.stop_propagation())
                .child(
                    div()
                        .id("pack-details-close")
                        .absolute()
                        .top(px(12.))
                        .right(px(12.))
                        .size(px(30.))
                        .rounded_full()
                        .bg(white_a(0.06))
                        .border_1()
                        .border_color(white_a(0.08))
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .hover(|s| s.bg(white_a(0.12)).border_color(white_a(0.18)))
                        .on_click(move |_, w, cx| {
                            cx.stop_propagation();
                            close(Act::Close, w, cx)
                        })
                        .child(
                            svg()
                                .path("icons/x.svg")
                                .size(px(14.))
                                .text_color(c(0xffffff)),
                        ),
                )
                .child(div().pt(px(28.)).px(px(24.)).pb(px(20.)).child(body)),
        )
        .into_any_element()
}
