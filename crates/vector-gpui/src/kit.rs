//! Small pieces many screens share: letter-spaced and gradient text, the `.vselect`
//! dropdown, the popup dialog and Vector's buttons.

use gpui::{
    AnyElement, App, ElementId, FontWeight, Hsla, IntoElement, ParentElement, SharedString, Styled,
    Window, div, img, prelude::*, px,
};
use unicode_segmentation::UnicodeSegmentation;

use crate::theme::*;

/// Text with CSS `letter-spacing`: GPUI has none, so each grapheme is laid out on its own,
/// with the spacing after it as CSS adds it (trailing included).
pub fn spaced(text: impl AsRef<str>, spacing: f32) -> gpui::Div {
    let mut row = div().flex().flex_row().flex_shrink_0();
    for g in text.as_ref().graphemes(true) {
        let g = if g == " " { "\u{00a0}" } else { g };
        row = row.child(
            div()
                .mr(px(spacing))
                .child(SharedString::from(g.to_string())),
        );
    }
    row
}

fn mix(a: Hsla, b: Hsla, t: f32) -> Hsla {
    let (a, b) = (a.to_rgb(), b.to_rgb());
    gpui::Rgba {
        r: a.r + (b.r - a.r) * t,
        g: a.g + (b.g - a.g) * t,
        b: a.b + (b.b - a.b) * t,
        a: a.a + (b.a - a.a) * t,
    }
    .into()
}

/// `.startup-subtext-gradient`: a `linear-gradient(to right, #2b976c 13.87%, #59fcb3 60%)` at
/// 200% size, breathing across the text over 6s. Each glyph takes the colour under its middle.
pub fn gradient_text(text: &str, spacing: f32, phase: f32) -> gpui::Div {
    gradient_text_in(text, spacing, phase, c(ACCENT_DEEP), c(ACCENT))
}

/// [`gradient_text`] between two other colours.
pub fn gradient_text_in(text: &str, spacing: f32, phase: f32, from: gpui::Hsla, to: gpui::Hsla) -> gpui::Div {
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
        row = row.child(
            div()
                .mr(px(spacing))
                .text_color(mix(from, to, t))
                .child(SharedString::from(g.to_string())),
        );
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
    select_with(
        id,
        width,
        14.,
        choices,
        &[],
        value,
        open,
        on_toggle,
        on_pick,
    )
}

/// [`select`] at a font size, with some choices `disabled`.
#[allow(clippy::too_many_arguments)]
pub fn select_with(
    id: impl Into<ElementId>,
    width: f32,
    font: f32,
    choices: &[Choice],
    disabled: &[&str],
    value: &str,
    open: bool,
    on_toggle: impl Fn(&mut Window, &mut App) + 'static,
    on_pick: impl Fn(&'static str, &mut Window, &mut App) + 'static + Clone,
) -> AnyElement {
    let label = choices
        .iter()
        .find(|c| c.value == value)
        .map(|c| c.label)
        .unwrap_or("");
    let caret = div().w(px(10.)).h(px(6.)).flex_shrink_0().child(
        gpui::svg()
            .path("ui/caret-down.svg")
            .size_full()
            .text_color(c(0x888888))
            .when(open, |s| {
                s.with_transformation(gpui::Transformation::rotate(gpui::radians(
                    std::f32::consts::PI,
                )))
            }),
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
        .text_size(px(font))
        .line_height(px(font * 1.2))
        .text_color(c(0xffffff))
        .cursor_pointer()
        .on_click(move |_, w, cx| on_toggle(w, cx))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .child(label),
        )
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
            let off = disabled.contains(&choice.value);
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
                    .text_size(px(font))
                    .text_color(c(if selected { 0xffffff } else { 0xcfcfcf }))
                    .when(selected, |s| s.bg(c(0x1f1f1f)))
                    .when(off, |s| s.opacity(0.4))
                    .when(!off, |s| {
                        s.hover(|s| s.bg(c(0x1f1f1f)).text_color(c(0xffffff)))
                            .cursor_pointer()
                            .on_click(move |_, w, cx| pick(v, w, cx))
                    })
                    .child(div().flex_1().min_w_0().child(choice.label))
                    .when(selected, |s| {
                        s.child(
                            gpui::svg()
                                .path("icons/check.svg")
                                .size(px(14.))
                                .text_color(c(PRIMARY)),
                        )
                    }),
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
        Self {
            title: title.into(),
            text: None,
            icon: Some("icons/vector_warning.svg"),
            confirm: "Okay".into(),
            cancel: None,
            input: None,
        }
    }
}

/// `#popup-container.active` and its `.popup`. `answer(true)` confirms.
pub fn popup(
    p: &Popup,
    answer: impl Fn(bool, &mut Window, &mut App) + 'static + Clone,
) -> AnyElement {
    let ok = answer.clone();
    let mut content = div().flex().flex_col().items_center().p(px(5.)).mb(px(10.));
    if let Some(icon) = p.icon {
        content = content.child(img(icon).h(px(100.)).w(px(100.)).mb(px(26.)));
    }
    content = content.child(
        div()
            .font_weight(FontWeight::BOLD)
            .text_size(px(24.))
            .line_height(px(24.))
            .text_center()
            .my(px(20.))
            .child(p.title.clone()),
    );
    if let Some(text) = &p.text {
        let body: AnyElement = if text.contains('<') || text.contains('&') {
            rich(text).into_any_element()
        } else {
            text.clone().into_any_element()
        };
        content = content.child(
            div()
                .text_center()
                .text_color(c(TEXT))
                .mb(px(16.))
                .when(p.icon.is_some(), |d| d.max_w(px(420.)))
                .child(body),
        );
    }
    if let Some(input) = &p.input {
        content = content.child(
            div()
                .w_full()
                .h(px(41.2))
                .px(px(19.2))
                .rounded(px(8.))
                .bg(c(DIVIDER))
                .flex()
                .items_center()
                .child(input.clone()),
        );
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
    fn render(
        &mut self,
        _: &mut gpui::Window,
        _: &mut gpui::Context<Self>,
    ) -> impl gpui::IntoElement {
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

pub fn tooltip(
    text: impl Into<gpui::SharedString>,
) -> impl Fn(&mut gpui::Window, &mut gpui::App) -> gpui::AnyView + 'static {
    let text = text.into();
    move |_, cx| cx.new(|_| Tip(text.clone())).into()
}

/// `countdownTip`: a hover label counting down each second to `until` (unix seconds), from
/// `now` (unix seconds) at build.
pub fn countdown_tip(
    until: u64,
    now: u64,
    label: fn(i64) -> String,
) -> impl Fn(&mut gpui::Window, &mut gpui::App) -> gpui::AnyView + 'static {
    move |_, cx| {
        let start = std::time::Instant::now();
        cx.new(|cx: &mut gpui::Context<Tip>| {
            cx.spawn(async move |tip, cx| loop {
                cx.background_executor().timer(std::time::Duration::from_secs(1)).await;
                let left = until as i64 - (now + start.elapsed().as_secs()) as i64;
                if tip.update(cx, |t, cx| { t.0 = label(left).into(); cx.notify(); }).is_err() {
                    break;
                }
            })
            .detach();
            Tip(label(until as i64 - now as i64).into())
        })
        .into()
    }
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
                renderer
                    .render_parsed(&parsed, gpui::SvgSize::Size(gpui::size(d, d)))
                    .ok()
            })
            .clone()
    });
    match image {
        Some(image) => gpui::img(image).size(px(size)),
        None => gpui::img(path).size(px(size)),
    }
}

/// `--page-gutter` for a pane `width` wide: a share of the pane that gives way first as it
/// narrows, holding the content at 584px, then falls to 18px.
pub fn page_gutter(width: f32) -> f32 {
    (width * 0.18).min((width - 584.) / 2.).max(18.)
}

/// CSS `cubic-bezier(x1, y1, x2, y2)` as an easing: solve x for t by Newton, then read y.
pub fn bezier(x1: f32, y1: f32, x2: f32, y2: f32) -> impl Fn(f32) -> f32 {
    let at = |a: f32, b: f32, t: f32| {
        3. * a * (1. - t) * (1. - t) * t + 3. * b * (1. - t) * t * t + t * t * t
    };
    let slope = |a: f32, b: f32, t: f32| {
        3. * a * (1. - t) * (1. - t) + 6. * (b - a) * (1. - t) * t + 3. * (1. - b) * t * t
    };
    move |x| {
        let mut t = x;
        for _ in 0..8 {
            let d = slope(x1, x2, t);
            if d.abs() < 1e-6 {
                break;
            }
            t = (t - (at(x1, x2, t) - x) / d).clamp(0., 1.);
        }
        at(y1, y2, t)
    }
}

/// The HTML subset explainers are written in: `<b>`, `<br>` and a few entities.
pub fn rich(html: &str) -> gpui::StyledText {
    let mut text = String::with_capacity(html.len());
    let mut bold = Vec::new();
    let mut open: Option<usize> = None;
    let mut rest = html;
    while !rest.is_empty() {
        if let Some(r) = rest.strip_prefix("<b>") {
            open = Some(text.len());
            rest = r;
        } else if let Some(r) = rest.strip_prefix("</b>") {
            if let Some(start) = open.take() {
                bold.push(start..text.len());
            }
            rest = r;
        } else if let Some(r) = rest.strip_prefix("<br>") {
            text.push('\n');
            rest = r;
        } else if let Some((ent, ch)) = [
            ("&nbsp;", '\u{a0}'),
            ("&amp;", '&'),
            ("&lt;", '<'),
            ("&gt;", '>'),
            ("&quot;", '"'),
        ]
        .iter()
        .find(|(e, _)| rest.starts_with(e))
        {
            text.push(*ch);
            rest = &rest[ent.len()..];
        } else {
            let ch = rest.chars().next().unwrap();
            text.push(ch);
            rest = &rest[ch.len_utf8()..];
        }
    }
    let strong = gpui::HighlightStyle {
        font_weight: Some(FontWeight::BOLD),
        ..Default::default()
    };
    gpui::StyledText::new(text).with_highlights(bold.into_iter().map(|r| (r, strong)))
}

/// Text safe inside a popup's `<b>`/`<br>` markup.
pub fn escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// Where tappable things were last painted, by key: what a popover anchors beside.
pub type Anchors = std::rc::Rc<std::cell::RefCell<std::collections::HashMap<SharedString, gpui::Bounds<gpui::Pixels>>>>;

/// Records its parent's bounds under `key` as it paints.
pub fn anchor(anchors: &Anchors, key: SharedString) -> impl IntoElement {
    let anchors = anchors.clone();
    gpui::canvas(move |b, _, _| {
        anchors.borrow_mut().insert(key, b);
    }, |_, _, _, _| {})
    .absolute()
    .inset_0()
}

/// One row of a `.context-menu`.
#[derive(Clone, Default)]
pub struct MenuItem {
    pub label: SharedString,
    pub icon: Option<SharedString>,
    pub danger: bool,
    pub checked: bool,
    pub kind: MenuKind,
    pub disabled: bool,
    /// A dimmed qualifier after the label.
    pub hint: Option<SharedString>,
    /// A colour choice: its dot leads the row.
    pub swatch: Option<gpui::Hsla>,
    /// A live value from 0 to 1 and how it reads: applies as it moves, the menu stays open.
    pub slider: Option<(f32, SharedString)>,
    /// A place to go: the icon leads the label, like a launcher's.
    pub lead: bool,
}

#[derive(Clone, Copy, Default, PartialEq)]
pub enum MenuKind {
    #[default]
    Item,
    Divider,
    /// A section label, e.g. @EVERYONE above its choices.
    Header,
    /// Drills into a submenu in place.
    Sub,
    /// The row back out of a submenu.
    Back,
}

impl MenuItem {
    pub fn new(label: impl Into<SharedString>, icon: &str) -> Self {
        Self {
            label: label.into(),
            icon: (!icon.is_empty()).then(|| SharedString::from(format!("icons/{icon}.svg"))),
            ..Default::default()
        }
    }
    pub fn divider() -> Self {
        Self {
            kind: MenuKind::Divider,
            ..Default::default()
        }
    }
    pub fn header(label: impl Into<SharedString>) -> Self {
        Self {
            label: label.into(),
            kind: MenuKind::Header,
            ..Default::default()
        }
    }
    pub fn back(label: impl Into<SharedString>) -> Self {
        Self {
            label: label.into(),
            kind: MenuKind::Back,
            ..Default::default()
        }
    }
    pub fn sub(mut self) -> Self {
        self.kind = MenuKind::Sub;
        self
    }
    pub fn disabled(mut self, on: bool) -> Self {
        self.disabled = on;
        self
    }
    pub fn hint(mut self, hint: Option<String>) -> Self {
        self.hint = hint.map(Into::into);
        self
    }
    pub fn danger(mut self) -> Self {
        self.danger = true;
        self
    }
    pub fn lead(mut self) -> Self {
        self.lead = true;
        self
    }
    pub fn checked(mut self, on: bool) -> Self {
        self.checked = on;
        self
    }
    pub fn slider(label: impl Into<SharedString>, value: f32, shown: impl Into<SharedString>) -> Self {
        Self {
            label: label.into(),
            slider: Some((value.clamp(0., 1.), shown.into())),
            ..Default::default()
        }
    }
    pub fn swatch(mut self, color: gpui::Hsla) -> Self {
        self.swatch = Some(color);
        self
    }
}

type OnSlide = std::rc::Rc<dyn Fn(usize, f32, &mut Window, &mut App)>;

/// `.context-menu-slider`: the label and its value over an `.st-slider`.
fn menu_slider(
    ix: usize,
    value: f32,
    label: SharedString,
    shown: SharedString,
    on: OnSlide,
    sliding: std::rc::Rc<std::cell::Cell<Option<usize>>>,
) -> AnyElement {
    let bounds = std::rc::Rc::new(std::cell::Cell::new(gpui::Bounds::<gpui::Pixels>::default()));
    let at = |b: gpui::Bounds<gpui::Pixels>, x: gpui::Pixels| {
        ((x - b.origin.x - px(10.)) / (b.size.width - px(20.)).max(px(1.))).clamp(0., 1.)
    };
    let down = {
        let (on, bounds, sliding) = (on.clone(), bounds.clone(), sliding.clone());
        move |e: &gpui::MouseDownEvent, w: &mut Window, cx: &mut App| {
            cx.stop_propagation();
            sliding.set(Some(ix));
            on(ix, at(bounds.get(), e.position.x), w, cx);
        }
    };
    let record = bounds.clone();
    let track = gpui::canvas(
        move |b, _, _| record.set(b),
        move |_, _, window, _| {
            if sliding.get() != Some(ix) {
                return;
            }
            let (on, bounds, s) = (on.clone(), bounds.clone(), sliding.clone());
            window.on_mouse_event(move |e: &gpui::MouseMoveEvent, phase, w, cx| {
                if phase != gpui::DispatchPhase::Bubble || s.get() != Some(ix) {
                    return;
                }
                if !e.dragging() {
                    s.set(None);
                    return;
                }
                on(ix, at(bounds.get(), e.position.x), w, cx);
            });
            let s = sliding.clone();
            window.on_mouse_event(move |_: &gpui::MouseUpEvent, _, _, _| s.set(None));
        },
    )
    .absolute()
    .inset_0();
    let fill = crate::theme::c(crate::theme::PRIMARY);
    div()
        .pt(px(9.))
        .pb(px(11.))
        .px(px(14.))
        .min_w(px(190.))
        .child(
            div()
                .flex()
                .justify_between()
                .mb(px(9.))
                .text_size(px(14.))
                .line_height(px(24.))
                .text_color(c(0xb2b2b2))
                .child(label)
                .child(div().text_color(c(0xffffff)).child(shown)),
        )
        .child(
            div()
                .id(("ctx-slider", ix))
                .relative()
                .w_full()
                .h(px(20.))
                .cursor_pointer()
                .on_mouse_down(gpui::MouseButton::Left, down)
                .child(track)
                .child(
                    div()
                        .absolute()
                        .left_0()
                        .right_0()
                        .top(px(7.))
                        .h(px(6.))
                        .rounded(px(3.))
                        .bg(c(0x1a1a1a))
                        .child(div().h_full().rounded(px(3.)).bg(fill).w(gpui::relative(value))),
                )
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .left(gpui::relative(value))
                        .ml(px(-10. * value * 2. + 0.))
                        .size(px(20.))
                        .rounded_full()
                        .bg(c(0xffffff))
                        .shadow(vec![gpui::BoxShadow {
                            color: ca(0, 0, 0, 0.35),
                            offset: gpui::point(px(0.), px(1.)),
                            blur_radius: px(4.),
                            spread_radius: px(0.),
                            inset: false,
                        }]),
                ),
        )
        .into_any_element()
}

/// `ContextMenu.svelte`: rows at the press, kept inside the window. A press elsewhere closes it.
pub fn context_menu(
    at: gpui::Point<gpui::Pixels>,
    items: &[MenuItem],
    on_pick: impl Fn(usize, &mut Window, &mut App) + 'static,
    on_slide: impl Fn(usize, f32, &mut Window, &mut App) + 'static,
    sliding: std::rc::Rc<std::cell::Cell<Option<usize>>>,
    on_close: impl Fn(&mut Window, &mut App) + 'static,
) -> AnyElement {
    let on_pick = std::rc::Rc::new(on_pick);
    let on_slide = std::rc::Rc::new(on_slide);
    let mut list = div()
        .id("context-menu")
        .min_w(px(160.))
        .max_w(px(320.))
        .bg(c(0x1e1e1e))
        .border_1()
        .border_color(c(0x393b3c))
        .rounded(px(10.))
        .overflow_hidden()
        .shadow(vec![gpui::BoxShadow {
            color: ca(0, 0, 0, 0.5),
            offset: gpui::point(px(0.), px(4.)),
            blur_radius: px(18.),
            spread_radius: px(0.),
            inset: false,
        }])
        .occlude()
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_mouse_down(gpui::MouseButton::Right, |_, _, cx| cx.stop_propagation());
    for (ix, item) in items.iter().enumerate() {
        match item.kind {
            MenuKind::Divider => {
                list = list.child(div().h(px(1.)).my(px(4.)).bg(white_a(0.06)));
                continue;
            }
            MenuKind::Header => {
                list = list.child(
                    div()
                        .pt(px(9.))
                        .pb(px(4.))
                        .px(px(14.))
                        .text_size(px(11.))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(c(0x6f6f6f))
                        .whitespace_nowrap()
                        .child(spaced(item.label.to_uppercase(), 0.66)),
                );
                continue;
            }
            _ => {}
        }
        if let Some((value, shown)) = &item.slider {
            list = list.child(menu_slider(ix, *value, item.label.clone(), shown.clone(), on_slide.clone(), sliding.clone()));
            continue;
        }
        let pick = on_pick.clone();
        let danger = c(crate::timeline::DANGER);
        let back = item.kind == MenuKind::Back;
        let lead = item.lead && !back;
        let icon = match item.kind {
            MenuKind::Sub => Some(SharedString::from("icons/chevron-right.svg")),
            _ if item.checked => Some(SharedString::from("icons/check.svg")),
            _ => item.icon.clone(),
        };
        let ink = if item.danger {
            danger
        } else if back {
            c(0x8b8b8b)
        } else {
            c(0xb2b2b2)
        };
        let live = !item.disabled;
        let swatch = item.swatch;
        let label = div()
            .when(swatch.is_some(), |d| d.flex_1())
            .min_w_0()
            .overflow_hidden()
            .text_ellipsis()
            .flex()
            .items_baseline()
            .child(item.label.clone())
            .children(
                item.hint
                    .clone()
                    .map(|h| div().ml(px(6.)).opacity(0.5).text_size(px(11.9)).child(h)),
            );
        list = list.child(
            div()
                .id(("ctx", ix))
                .flex()
                .items_center()
                .when(!back && swatch.is_none() && !lead, |d| d.justify_between().gap(px(12.)))
                .when(lead, |d| d.gap(px(12.)))
                .when_some(swatch, |d, color| {
                    d.gap(px(10.)).child(
                        div()
                            .size(px(14.))
                            .flex_shrink_0()
                            .rounded_full()
                            .bg(color)
                            .border_1()
                            .border_color(gpui::white().opacity(0.18)),
                    )
                })
                .when(back, |d| d.gap(px(8.)))
                .px(px(14.))
                .when(lead, |d| d.pr(px(22.)))
                .py(px(10.))
                .text_size(px(14.))
                .line_height(px(24.))
                .whitespace_nowrap()
                .text_color(ink)
                .when(!live, |d| d.opacity(0.45))
                .when(live, |d| d.cursor_pointer())
                .when(live && item.danger, |d| {
                    d.hover(move |d| d.bg(danger.opacity(0.1)))
                })
                .when(live && !item.danger, |d| {
                    d.hover(|d| d.bg(c(0x2a2a2a)).text_color(c(0xffffff)))
                })
                .when(live, |d| d.on_click(move |_, w, cx| pick(ix, w, cx)))
                .when(back, |d| {
                    d.child(
                        gpui::svg()
                            .path("icons/chevron-left.svg")
                            .size(px(20.))
                            .flex_shrink_0()
                            .text_color(ink),
                    )
                })
                .when(lead, |d| {
                    d.children(icon.clone().map(|i| gpui::svg().path(i).size(px(20.)).flex_shrink_0().text_color(ink)))
                })
                .child(label)
                .when(!back && !lead, |d| {
                    d.children(icon.map(|i| {
                        let color = if item.checked && item.kind != MenuKind::Sub {
                            c(crate::theme::PRIMARY)
                        } else {
                            ink
                        };
                        gpui::svg()
                            .path(i)
                            .size(px(20.))
                            .flex_shrink_0()
                            .text_color(color)
                    }))
                }),
        );
    }
    let close = std::rc::Rc::new(on_close);
    let (c1, c2) = (close.clone(), close);
    gpui::deferred(
        div()
            .absolute()
            .inset_0()
            .on_mouse_down(gpui::MouseButton::Left, move |_, w, cx| c1(w, cx))
            .on_mouse_down(gpui::MouseButton::Right, move |_, w, cx| c2(w, cx))
            .child(
                gpui::anchored()
                    .position(at)
                    .snap_to_window_with_margin(px(8.))
                    .child(list),
            ),
    )
    .with_priority(20)
    .into_any_element()
}
