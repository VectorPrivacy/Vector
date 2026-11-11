//! `CreateChannelModal.svelte`: a Channel's kind, name and privacy, or a Category's name,
//! in one card.

use std::rc::Rc;

use gpui::prelude::*;
use gpui::{div, px, svg, App, Entity, FontWeight, SharedString, Window};

use crate::composer::TextInput;
use crate::theme::*;

pub struct CreateChannel {
    pub community: String,
    pub category: Option<String>,
    pub category_name: Option<String>,
    /// Makes a Category rather than a Channel.
    pub is_category: bool,
    pub voice: bool,
    pub private: bool,
    pub name: Entity<TextInput>,
    pub busy: bool,
    pub error: Option<String>,
}

pub enum Act {
    Voice(bool),
    Private,
    Close,
    Submit,
}

pub type OnAct = Rc<dyn Fn(Act, &mut Window, &mut App)>;

/// The protocol's name cap, in bytes.
pub const MAX: usize = 64;

impl CreateChannel {
    pub fn valid(&self, cx: &App) -> bool {
        let name = self.name.read(cx).text();
        !name.trim().is_empty() && name.len() <= MAX
    }
}

const ACCENT_TINT: u32 = 0x1b2621;

fn label(text: &str) -> impl IntoElement {
    crate::kit::spaced(text.to_uppercase(), 1.1).mb(px(8.)).font_weight(FontWeight::BOLD).text_size(px(11.)).line_height(px(24.)).text_color(white_a(0.5))
}

fn kind_row(m: &CreateChannel, voice: bool, on: &OnAct) -> impl IntoElement {
    let selected = m.voice == voice;
    let (icon, name, desc) = if voice { ("icons/volume-max.svg", "Voice", "Hang out over voice and video") } else { ("icons/channel-hash.svg", "Text", "Messages, images and files") };
    let on = on.clone();
    div()
        .id(if voice { "ccm-voice" } else { "ccm-text" })
        .h(px(60.))
        .flex()
        .items_center()
        .gap(px(12.))
        .px(px(14.))
        .rounded(px(12.))
        .border_1()
        .cursor_pointer()
        .when(selected, |d| d.border_color(c(ACCENT).opacity(0.55)).bg(c(ACCENT_TINT)))
        .when(!selected, |d| d.border_color(c(0x262626)).bg(c(0x161616)).hover(|d| d.bg(c(0x1b1b1b))))
        .on_click(move |_, w, cx| on(Act::Voice(voice), w, cx))
        .child(div().size(px(36.)).flex_shrink_0().rounded_full().bg(c(0x222222)).flex().items_center().justify_center().child(svg().path(icon).size(px(18.)).text_color(if selected { c(ACCENT) } else { c(0xcfcfcf) })))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(div().font_weight(FontWeight::SEMIBOLD).text_size(px(14.5)).line_height(px(17.)).text_color(c(0xf0f0f0)).child(name))
                .child(div().text_size(px(12.5)).line_height(px(15.)).text_color(c(0x8a8a8a)).child(desc)),
        )
        .child(
            div()
                .size(px(18.))
                .flex_shrink_0()
                .rounded_full()
                .border_2()
                .when(selected, |d| d.border_color(c(ACCENT)).flex().items_center().justify_center().child(div().size(px(8.)).rounded_full().bg(c(ACCENT))))
                .when(!selected, |d| d.border_color(c(0x555555))),
        )
}

pub fn view(m: &CreateChannel, on: &OnAct, window: &Window, cx: &App) -> impl IntoElement + use<> {
    let noun = if m.is_category { "Category" } else { "Channel" };
    let valid = m.valid(cx) && !m.busy;
    let bytes = m.name.read(cx).text().len();
    let close = on.clone();
    let submit = on.clone();
    let private = on.clone();
    let card = div()
        .id("ccm-card")
        .w(px(440.))
        .flex()
        .flex_col()
        .pt(px(18.))
        .pb(px(16.))
        .px(px(20.))
        .rounded(px(18.))
        .border_1()
        .border_color(c(0x2c2e2f))
        .bg(c(0x101010))
        .shadow(vec![gpui::BoxShadow { color: ca(0, 0, 0, 0.55), offset: gpui::point(px(0.), px(18.)), blur_radius: px(60.), spread_radius: px(0.), inset: false }])
        .occlude()
        .on_click(|_, _, cx| cx.stop_propagation())
        .child(
            div()
                .flex()
                .items_start()
                .justify_between()
                .gap(px(12.))
                .mb(px(18.))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(3.))
                        .min_w_0()
                        .child(div().font_weight(FontWeight::BOLD).text_size(px(19.)).line_height(px(24.)).text_color(c(0xffffff)).child(format!("Create {noun}")))
                        .children(m.category_name.clone().map(|n| div().truncate().text_size(px(12.5)).text_color(c(0x8a8a8a)).child(format!("in {n}")))),
                )
                .child(
                    div()
                        .id("ccm-close")
                        .px(px(6.))
                        .py(px(4.))
                        .cursor_pointer()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_size(px(14.))
                        .line_height(px(14.))
                        .text_color(white_a(0.4))
                        .hover(|d| d.text_color(c(0xffffff)))
                        .on_click(move |_, w, cx| close(Act::Close, w, cx))
                        .child("✕"),
                ),
        )
        .when(!m.is_category, |d| d.child(label("Channel Type")).child(div().flex().flex_col().gap(px(8.)).mb(px(18.)).child(kind_row(m, false, on)).child(kind_row(m, true, on))))
        .child(label(&format!("{noun} Name")))
        .child(
            div()
                .relative()
                .h(px(43.))
                .rounded(px(10.))
                .border_1()
                .border_color(c(PRIMARY))
                .bg(c(0x111111))
                .flex()
                .items_center()
                .pl(px(if m.is_category { 13. } else { 38. }))
                .pr(px(44.))
                .when(!m.is_category, |d| {
                    d.child(svg().path(if m.voice { "icons/volume-max.svg" } else { "icons/channel-hash.svg" }).absolute().left(px(13.)).top(px(13.5)).size(px(16.)).text_color(c(0x8a8a8a)))
                })
                .child(div().flex_1().min_w_0().child(m.name.clone()))
                .when(bytes > MAX - 16, |d| {
                    d.child(div().absolute().right(px(13.)).top(px(12.)).text_size(px(12.)).text_color(if bytes > MAX { c(crate::timeline::DANGER) } else { white_a(0.4) }).child((MAX as i64 - bytes as i64).to_string()))
                }),
        )
        .when(!m.is_category, |d| {
            d.child(
                div()
                    .mt(px(10.))
                    .pt(px(12.))
                    .pb(px(2.))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(16.))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .text_size(px(14.5))
                                    .line_height(px(24.))
                                    .text_color(white_a(0.8))
                                    .child(svg().path("icons/locked.svg").size(px(13.)).mr(px(6.)).text_color(c(0xcfcfcf)))
                                    .child("Private Channel"),
                            )
                            .child(div().text_size(px(12.5)).line_height(px(17.5)).text_color(c(0x7a7a7a)).child(if m.voice { "Only members and roles you let in can join it." } else { "Only members and roles you let in can see it." })),
                    )
                    .child(crate::settings::toggle("ccm-private", m.private, false, move |_, w, cx| private(Act::Private, w, cx))),
            )
        })
        .children(m.error.clone().map(|e| div().mt(px(12.)).text_size(px(12.5)).line_height(px(17.5)).text_color(c(crate::timeline::DANGER)).child(e)))
        .child(
            div()
                .mt(px(20.))
                .flex()
                .justify_end()
                .gap(px(8.))
                .child(
                    div()
                        .id("ccm-cancel")
                        .h(px(34.))
                        .px(px(14.))
                        .rounded(px(8.))
                        .flex()
                        .items_center()
                        .cursor_pointer()
                        .font_weight(FontWeight::MEDIUM)
                        .text_size(px(14.))
                        .text_color(c(0xcfcfcf))
                        .hover(|d| d.text_color(c(0xffffff)).underline())
                        .on_click({
                            let on = on.clone();
                            move |_, w, cx| on(Act::Close, w, cx)
                        })
                        .child("Cancel"),
                )
                .child(
                    div()
                        .id("ccm-save")
                        .h(px(34.))
                        .px(px(14.))
                        .rounded(px(8.))
                        .flex()
                        .items_center()
                        .bg(c(PRIMARY))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_size(px(14.))
                        .text_color(c(0x000000))
                        .when(!valid, |d| d.opacity(0.45))
                        .when(valid, |d| d.cursor_pointer().on_click(move |_, w, cx| submit(Act::Submit, w, cx)))
                        .child(if m.busy { "Creating…".to_string() } else { format!("Create {noun}") }),
                ),
        );
    let close = on.clone();
    let _ = window;
    gpui::deferred(
        div()
            .id("ccm-overlay")
            .absolute()
            .inset_0()
            .bg(ca(0, 0, 0, 0.72))
            .flex()
            .items_center()
            .justify_center()
            .p(px(16.))
            .occlude()
            .on_click(move |_, w, cx| close(Act::Close, w, cx))
            .child(card),
    )
    .with_priority(13)
}

pub fn placeholder(is_category: bool, voice: bool) -> SharedString {
    if is_category {
        "New Category".into()
    } else if voice {
        "Lounge".into()
    } else {
        "new-channel".into()
    }
}
