//! Small pieces many screens share: letter-spaced and gradient text, the `.vselect`
//! dropdown, the popup dialog and Vector's buttons.

use gpui::{
    AnyElement, App, ElementId, FontWeight, Hsla, IntoElement, ParentElement, SharedString, Styled, Window, div, img,
    prelude::*, px,
};
use unicode_segmentation::UnicodeSegmentation;

use crate::theme::*;

/// Text with CSS `letter-spacing`: GPUI has none, so each grapheme is laid out on its own,
/// with the spacing after it as CSS adds it (trailing included).
pub fn spaced(text: impl AsRef<str>, spacing: f32) -> gpui::Div {
    let mut row = div().flex().flex_row().flex_shrink_0();
    for g in text.as_ref().graphemes(true) {
        let g = if g == " " { "\u{00a0}" } else { g };
        row = row.child(div().mr(px(spacing)).child(SharedString::from(g.to_string())));
    }
    row
}

fn mix(a: Hsla, b: Hsla, t: f32) -> Hsla {
    let (a, b) = (a.to_rgb(), b.to_rgb());
    gpui::Rgba { r: a.r + (b.r - a.r) * t, g: a.g + (b.g - a.g) * t, b: a.b + (b.b - a.b) * t, a: a.a + (b.a - a.a) * t }.into()
}

/// `.startup-subtext-gradient`: a `linear-gradient(to right, #2b976c 13.87%, #59fcb3 60%)` at
/// 200% size, breathing across the text over 6s. Each glyph takes the colour under its middle.
pub fn gradient_text(text: &str, spacing: f32, phase: f32) -> gpui::Div {
    let from = c(ACCENT_DEEP);
    let to = c(ACCENT);
    let count = text.graphemes(true).count().max(1) as f32;
    // background-position swings 0% → 100% → 0%: the visible half of the 200% strip slides.
    let swing = 0.5 - 0.5 * (phase * std::f32::consts::TAU).cos();
    let mut row = div().flex().flex_row().flex_shrink_0();
    for (i, g) in text.graphemes(true).enumerate() {
        let x = (i as f32 + 0.5) / count;
        // Position on the 200%-wide strip: the element shows [swing*0.5, swing*0.5 + 0.5].
        let p = swing * 0.5 + x * 0.5;
        let t = ((p - 0.1387) / (0.60 - 0.1387)).clamp(0., 1.);
        let g = if g == " " { "\u{00a0}" } else { g };
        row = row.child(div().mr(px(spacing)).text_color(mix(from, to, t)).child(SharedString::from(g.to_string())));
    }
    row
}

/// One choice in a [`select`].
pub struct Choice {
    pub value: &'static str,
    pub label: &'static str,
}

/// The `.vselect` dropdown: a button showing the choice, and its list when `open`.
pub fn select(
    id: impl Into<ElementId>,
    width: f32,
    choices: &[Choice],
    value: &str,
    open: bool,
    on_toggle: impl Fn(&mut Window, &mut App) + 'static,
    on_pick: impl Fn(&'static str, &mut Window, &mut App) + 'static + Clone,
) -> AnyElement {
    let label = choices.iter().find(|c| c.value == value).map(|c| c.label).unwrap_or("");
    let caret = div()
        .w(px(10.))
        .h(px(6.))
        .flex_shrink_0()
        .child(
            gpui::svg()
                .path("ui/caret-down.svg")
                .size_full()
                .text_color(c(0x888888))
                .when(open, |s| s.with_transformation(gpui::Transformation::rotate(gpui::radians(std::f32::consts::PI)))),
        );
    let button = div()
        .id(id.into())
        .flex()
        .items_center()
        .gap(px(8.))
        .w_full()
        .h(px(36.))
        .px(px(12.))
        .border_1()
        .border_color(c(if open { 0x474747 } else { 0x333333 }))
        .hover(|s| s.border_color(c(0x474747)))
        .rounded(px(6.))
        .bg(c(0x0a0a0a))
        .text_size(px(14.))
        .line_height(px(17.))
        .text_color(c(0xffffff))
        .cursor_pointer()
        .on_click(move |_, w, cx| on_toggle(w, cx))
        .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(label))
        .child(caret);
    let mut root = div().relative().w(px(width)).flex_shrink_0().child(button);
    if open {
        let mut list = div()
            .absolute()
            .top(px(42.))
            .left_0()
            .right_0()
            .p(px(4.))
            .border_1()
            .border_color(c(0x2a2a2a))
            .rounded(px(8.))
            .bg(c(0x111111))
            .shadow(vec![gpui::BoxShadow {
                color: ca(0, 0, 0, 0.55),
                offset: gpui::point(px(0.), px(12.)),
                blur_radius: px(32.),
                spread_radius: px(0.),
                inset: false,
            }])
            .occlude();
        for choice in choices {
            let selected = choice.value == value;
            let pick = on_pick.clone();
            let v = choice.value;
            list = list.child(
                div()
                    .id(choice.value)
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .h(px(34.))
                    .px(px(8.))
                    .rounded(px(6.))
                    .text_size(px(14.))
                    .text_color(c(if selected { 0xffffff } else { 0xcfcfcf }))
                    .when(selected, |s| s.bg(c(0x1f1f1f)))
                    .hover(|s| s.bg(c(0x1f1f1f)).text_color(c(0xffffff)))
                    .cursor_pointer()
                    .on_click(move |_, w, cx| pick(v, w, cx))
                    .child(div().flex_1().min_w_0().child(choice.label))
                    .when(selected, |s| s.child(gpui::svg().path("icons/check.svg").size(px(14.)).text_color(c(PRIMARY)))),
            );
        }
        root = root.child(gpui::deferred(list).with_priority(1));
    }
    root.into_any_element()
}

/// A popup's content: `popupConfirm` (misc.js).
#[derive(Clone)]
pub struct Popup {
    pub title: SharedString,
    pub text: Option<SharedString>,
    pub icon: Option<&'static str>,
    pub confirm: SharedString,
    /// None for a notice (Okay only).
    pub cancel: Option<SharedString>,
    /// A text field, for a popup that asks for a value.
    pub input: Option<gpui::Entity<crate::composer::TextInput>>,
}

impl Popup {
    pub fn notice(title: impl Into<SharedString>) -> Self {
        Self { title: title.into(), text: None, icon: Some("icons/vector_warning.svg"), confirm: "Okay".into(), cancel: None, input: None }
    }
}

/// `#popup-container.active` and its `.popup`. `answer(true)` confirms.
pub fn popup(p: &Popup, answer: impl Fn(bool, &mut Window, &mut App) + 'static + Clone) -> AnyElement {
    let ok = answer.clone();
    let mut content = div().flex().flex_col().items_center().p(px(5.)).mb(px(10.));
    if let Some(icon) = p.icon {
        content = content.child(img(icon).h(px(100.)).w(px(100.)).mb(px(26.)));
    }
    content = content.child(
        div().font_weight(FontWeight::BOLD).text_size(px(24.)).line_height(px(24.)).text_center().my(px(20.)).child(p.title.clone()),
    );
    if let Some(text) = &p.text {
        content = content.child(div().text_center().text_color(c(TEXT)).mb(px(16.)).max_w(px(420.)).child(text.clone()));
    }
    if let Some(input) = &p.input {
        content = content.child(div().w_full().h(px(41.2)).px(px(19.2)).rounded(px(8.)).bg(c(DIVIDER)).flex().items_center().child(input.clone()));
    }
    let mut buttons = div().flex().justify_center().gap(px(10.)).child(
        div()
            .id("popup-confirm")
            .px(px(20.))
            .py(px(10.))
            .rounded(px(8.))
            .border_1()
            .border_color(gpui::transparent_black())
            .bg(c(0x58fcb3))
            .text_color(c(0x000000))
            .font_weight(FontWeight::SEMIBOLD)
            .line_height(px(19.2))
            .cursor_pointer()
            .on_click(move |_, w, cx| ok(true, w, cx))
            .child(p.confirm.clone()),
    );
    if let Some(cancel) = &p.cancel {
        let no = answer.clone();
        buttons = buttons.child(
            div()
                .id("popup-cancel")
                .px(px(20.))
                .py(px(10.))
                .rounded(px(8.))
                .border_1()
                .border_color(white_a(0.2))
                .font_weight(FontWeight::SEMIBOLD)
                .line_height(px(19.2))
                .cursor_pointer()
                .on_click(move |_, w, cx| no(false, w, cx))
                .child(cancel.clone()),
        );
    }
    let card = div()
        .flex()
        .flex_col()
        .p(px(30.))
        .mx(px(10.))
        .border_1()
        .border_color(c(PRIMARY))
        .rounded(px(8.))
        .bg(c(APP_BG))
        .shadow(vec![gpui::BoxShadow {
            color: ca(51, 219, 152, 0.145),
            offset: gpui::point(px(0.), px(0.)),
            blur_radius: px(15.),
            spread_radius: px(0.),
                inset: false,
        }])
        .font_family(INTER)
        .text_color(c(TEXT))
        .child(content)
        .child(buttons);
    gpui::deferred(
        div()
            .id("popup-container")
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(ca(0, 0, 0, 0.85))
            .occlude()
            .child(card),
    )
    .with_priority(10)
    .into_any_element()
}

/// A hover label, where the web client sets a `title`.
pub struct Tip(gpui::SharedString);

impl gpui::Render for Tip {
    fn render(&mut self, _: &mut gpui::Window, _: &mut gpui::Context<Self>) -> impl gpui::IntoElement {
        use gpui::{ParentElement, Styled};
        gpui::div()
            .px(gpui::px(8.))
            .py(gpui::px(4.))
            .rounded(gpui::px(6.))
            .bg(crate::theme::c(0x1c1c1c))
            .border_1()
            .border_color(crate::theme::white_a(0.1))
            .font_family(crate::theme::INTER)
            .text_size(gpui::px(12.))
            .text_color(crate::theme::white_a(0.9))
            .child(self.0.clone())
    }
}

pub fn tooltip(text: impl Into<gpui::SharedString>) -> impl Fn(&mut gpui::Window, &mut gpui::App) -> gpui::AnyView + 'static {
    let text = text.into();
    move |_, cx| cx.new(|_| Tip(text.clone())).into()
}

/// A plain `<input>`: Inter 500 16px, Chromium's placeholder grey.
pub fn field_look() -> crate::composer::Look {
    crate::composer::Look {
        family: INTER,
        weight: FontWeight::MEDIUM,
        size: px(16.),
        line_height: px(19.2),
        color: c(0xffffff),
        placeholder: c(0x757575),
        ..Default::default()
    }
}

thread_local! {
    static SHARP: std::cell::RefCell<std::collections::HashMap<(SharedString, u32), Option<std::sync::Arc<gpui::RenderImage>>>> = Default::default();
}

/// An SVG asset rasterized at the size it shows: `img` rasterizes at the file's own size, which
/// aliases when a large drawing shows small.
pub fn sharp_svg(path: impl Into<SharedString>, size: f32, window: &Window, cx: &App) -> gpui::Img {
    let path = path.into();
    let device = (size * window.scale_factor()).ceil() as u32;
    let image = SHARP.with(|cache| {
        cache
            .borrow_mut()
            .entry((path.clone(), device))
            .or_insert_with(|| {
                let bytes = cx.asset_source().load(&path).ok().flatten()?;
                let renderer = cx.svg_renderer();
                let parsed = renderer.parse_svg(&bytes).ok()?;
                let d = gpui::DevicePixels(device as i32);
                renderer.render_parsed(&parsed, gpui::SvgSize::Size(gpui::size(d, d))).ok()
            })
            .clone()
    });
    match image {
        Some(image) => gpui::img(image).size(px(size)),
        None => gpui::img(path).size(px(size)),
    }
}
