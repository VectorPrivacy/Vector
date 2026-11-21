//! Small pieces many screens share: letter-spaced and gradient text, the `.vselect`
//! dropdown, the popup dialog and Vector's buttons.

use gpui::{AnimationExt, AnyElement, App, ElementId, FontWeight, Hsla, IntoElement, ParentElement, SharedString, Styled, Window, div, prelude::*, px};
use unicode_segmentation::UnicodeSegmentation;

use crate::lang::LangFonts as _;
use crate::theme::*;

/// CSS `font-variant-numeric: tabular-nums`: every digit as wide as the others.
pub fn tabular() -> gpui::FontFeatures {
    static TNUM: std::sync::LazyLock<gpui::FontFeatures> =
        std::sync::LazyLock::new(|| gpui::FontFeatures(std::sync::Arc::new(vec![("tnum".into(), 1)])));
    TNUM.clone()
}

/// Text with CSS `letter-spacing`: GPUI has none, so each grapheme is laid out on its own,
/// with the spacing after it as CSS adds it (trailing included).
/// Text as a page lays a name out: each run of spaces, tabs and line breaks one space, none at
/// either end. A no-break space stays.
pub fn one_line(text: &str) -> std::borrow::Cow<'_, str> {
    let gap = |c: char| matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0c');
    let t = text.trim_matches(gap);
    if t.len() == text.len() && !t.contains(['\t', '\n', '\r', '\x0c']) && !t.contains("  ") {
        return text.into();
    }
    let mut out = String::with_capacity(t.len());
    let mut open = false;
    for c in t.chars() {
        if gap(c) {
            open = true;
            continue;
        }
        if open {
            out.push(' ');
            open = false;
        }
        out.push(c);
    }
    out.into()
}

/// A name read as [`one_line`] shows it, as the desktop's stylesheet shows every name.
pub fn one_line_de<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    let s = <String as serde::Deserialize>::deserialize(d)?;
    Ok(match one_line(&s) {
        std::borrow::Cow::Borrowed(_) => s,
        std::borrow::Cow::Owned(o) => o,
    })
}

pub fn spaced(text: impl AsRef<str>, spacing: f32) -> gpui::Div {
    let text = text.as_ref();
    // Joined scripts take no letter spacing, as CSS leaves them.
    if crate::inline::has_rtl(text) {
        return div().flex().flex_shrink_0().child(SharedString::from(text.to_string()));
    }
    let mut row = div();
    for g in text.graphemes(true) {
        let g = if g == " " { "\u{00a0}" } else { g };
        row = row.child(div().mr(px(spacing)).child(SharedString::from(g.to_string())));
    }
    letters(div().flex().flex_shrink_0().spoken(SharedString::from(text.to_string())), row)
}

/// [`spaced`] that wraps between words, as a block of spaced text does.
pub fn spaced_wrap(text: impl AsRef<str>, spacing: f32) -> gpui::Div {
    let text = text.as_ref();
    if crate::inline::has_rtl(text) {
        return div().min_w_0().child(SharedString::from(text.to_string()));
    }
    let mut row = div().flex().flex_row().flex_wrap().min_w_0().keep_ltr();
    let mut words = text.split(' ').peekable();
    while let Some(w) = words.next() {
        row = row.child(match words.peek() {
            Some(_) => spaced(format!("{w} "), spacing),
            None => spaced(w, spacing),
        });
    }
    row.spoken(SharedString::from(text.to_string()))
}

/// [`spaced`] that shrinks with its row and ends in "…" where it is cut, as `text-overflow: ellipsis`.
pub fn spaced_cut(text: impl AsRef<str>, spacing: f32) -> gpui::Div {
    let text = text.as_ref();
    if crate::inline::has_rtl(text) {
        return div().min_w_0().truncate().child(SharedString::from(text.to_string()));
    }
    div()
        .flex()
        .min_w_0()
        .overflow_hidden()
        .spoken(SharedString::from(text.to_string()))
        .child(SpacedCut { text: SharedString::from(text.to_string()), spacing })
}

struct SpacedCut {
    text: SharedString,
    spacing: f32,
}

pub struct SpacedLetters {
    letters: Vec<gpui::ShapedLine>,
    ellipsis: gpui::ShapedLine,
    height: gpui::Pixels,
}

impl gpui::Element for SpacedCut {
    type RequestLayoutState = SpacedLetters;
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&gpui::GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (gpui::LayoutId, SpacedLetters) {
        let style = window.text_style();
        let size = style.font_size.to_pixels(window.rem_size());
        let height = style.line_height_in_pixels(window.rem_size());
        let shape = |g: &str| window.text_system().shape_line(SharedString::from(g.to_string()), size, &[style.to_run(g.len())], None);
        let letters: Vec<_> = self.text.graphemes(true).map(shape).collect();
        let ellipsis = shape("…");
        let natural: f32 = letters.iter().map(|l| f32::from(l.width).ceil() + self.spacing).sum();
        let mut layout = gpui::Style::default();
        layout.size.width = px(natural).into();
        layout.size.height = height.into();
        layout.min_size.width = px(0.).into();
        layout.flex_shrink = 1.;
        (window.request_layout(layout, [], cx), SpacedLetters { letters, ellipsis, height })
    }

    fn prepaint(
        &mut self,
        _: Option<&gpui::GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        _: gpui::Bounds<gpui::Pixels>,
        _: &mut SpacedLetters,
        _: &mut Window,
        _: &mut App,
    ) {
    }

    fn paint(
        &mut self,
        _: Option<&gpui::GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: gpui::Bounds<gpui::Pixels>,
        s: &mut SpacedLetters,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let room = f32::from(bounds.size.width);
        // Whole-pixel letters, as `spaced` lays each in its own text box.
        let step = |l: &gpui::ShapedLine| f32::from(l.width).ceil() + self.spacing;
        let natural: f32 = s.letters.iter().map(step).sum();
        let (shown, cut) = if natural <= room + 0.5 {
            (s.letters.len(), false)
        } else {
            let room = room - f32::from(s.ellipsis.width);
            let mut x = 0.;
            let n = s.letters.iter().take_while(|l| {
                x += step(l);
                x <= room
            });
            (n.count(), true)
        };
        let at = |x: f32| gpui::point(bounds.left() + px(x), bounds.top());
        let mut x = 0.;
        for l in &s.letters[..shown] {
            l.paint(at(x), s.height, gpui::TextAlign::Left, None, window, cx).ok();
            x += step(l);
        }
        if cut {
            s.ellipsis.paint(at(x), s.height, gpui::TextAlign::Left, None, window, cx).ok();
        }
    }
}

/// How far a sideways scroller has moved from its start, whichever way the layout runs.
pub fn scrolled(handle: &gpui::ScrollHandle) -> f32 {
    f32::from(handle.offset().x).abs()
}

/// Scrolls a sideways scroller to `at` from its start: mirrored content scrolls rightward.
pub fn scroll_from_start(handle: &gpui::ScrollHandle, at: f32) {
    let x = if gpui::layout_rtl() { at } else { -at };
    handle.set_offset(gpui::point(px(x), handle.offset().y));
}

/// Letters `gap` apart, with no space after the last.
pub fn gapped(text: &str, gap: f32) -> gpui::Div {
    if crate::inline::has_rtl(text) {
        return div().flex().child(SharedString::from(text.to_string()));
    }
    letters(
        div().flex().spoken(SharedString::from(text.to_string())),
        div().gap(px(gap)).children(text.graphemes(true).map(|g| div().child(SharedString::from(g.to_string())))),
    )
}

/// Letters laid one by one: left to right under a mirrored layout too, from `outer`'s start.
fn letters(outer: gpui::Div, row: gpui::Div) -> gpui::Div {
    outer.child(row.flex().flex_row().keep_ltr())
}

pub fn mix(a: Hsla, b: Hsla, t: f32) -> Hsla {
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
    let mut row = div();
    for (i, g) in text.graphemes(true).enumerate() {
        let x = (i as f32 + 0.5) / count;
        // Position on the 200%-wide strip: the element shows [swing*0.5, swing*0.5 + 0.5].
        let p = swing * 0.5 + x * 0.5;
        let t = ((p - 0.1387) / (0.60 - 0.1387)).clamp(0., 1.);
        let g = if g == " " { "\u{00a0}" } else { g };
        row = row.child(div().mr(px(spacing)).text_color(mix(from, to, t)).child(SharedString::from(g.to_string())));
    }
    letters(div().flex().flex_shrink_0().spoken(SharedString::from(text.to_string())), row)
}

/// How far along a slider's track `x` sits, 0 to 1, with `inset` held back at each end for the
/// thumb. Under a mirrored layout the track fills from its right edge.
pub fn track_frac(b: gpui::Bounds<gpui::Pixels>, x: gpui::Pixels, inset: gpui::Pixels) -> f32 {
    let from = if gpui::layout_rtl() { b.right() - inset - x } else { x - b.origin.x - inset };
    (from / (b.size.width - inset * 2.).max(px(1.))).clamp(0., 1.)
}

/// The choice `value` names, or the first.
fn value_of(choices: &[Choice], value: &str) -> &'static str {
    choices.iter().find(|c| c.value == value).or(choices.first()).map_or("", |c| c.value)
}

/// The open [`select`]'s keyboard state: its focus and the highlighted row.
struct SelectKeys {
    id: ElementId,
    focus: Option<gpui::FocusHandle>,
    hi: std::rc::Rc<std::cell::Cell<usize>>,
    /// The option last scrolled into view.
    shown: std::cell::Cell<usize>,
}

/// Where a select's button last drew, inside the band of the window its clipping ancestors
/// leave visible, and its list's scroll.
#[derive(Clone, Default)]
struct SelectPlace {
    button: gpui::Bounds<gpui::Pixels>,
    band: gpui::Bounds<gpui::Pixels>,
    scroll: gpui::ScrollHandle,
}

/// `.vselect-list`'s max-height.
const SELECT_CAP: f32 = 320.;

thread_local! {
    static OPEN_SELECT: std::cell::RefCell<Option<SelectKeys>> = const { std::cell::RefCell::new(None) };
    static SELECT_PLACES: std::cell::RefCell<std::collections::HashMap<ElementId, SelectPlace>> = Default::default();
}

/// `StageProgress`: a ring filled clockwise from the top to `pct` (0 to 1), then the stage's name.
pub fn stage_progress(label: impl Into<SharedString>, pct: f32, size: f32) -> gpui::Div {
    let fill = pct.clamp(0., 1.);
    let color = c(PRIMARY);
    let ring = gpui::canvas(
        |_, _, _| {},
        move |bounds: gpui::Bounds<gpui::Pixels>, _, window, _| {
            use std::f32::consts::TAU;
            let unit = f32::from(bounds.size.width.min(bounds.size.height)) / 24.;
            let (mid, half) = (9. * unit, 1.5 * unit);
            let center = bounds.center();
            let at = |r: f32, a: f32| gpui::point(center.x + px(r * a.sin()), center.y - px(r * a.cos()));
            let band = |from: f32, to: f32| {
                let steps = ((to - from) / TAU * 96.).ceil().max(2.) as usize;
                let mut pts = Vec::with_capacity(steps * 2 + 2);
                for k in 0..=steps {
                    pts.push(at(mid + half, from + (to - from) * k as f32 / steps as f32));
                }
                for k in (0..=steps).rev() {
                    pts.push(at(mid - half, from + (to - from) * k as f32 / steps as f32));
                }
                let mut b = gpui::PathBuilder::fill();
                b.add_polygon(&pts, true);
                b.build().ok()
            };
            if let Some(p) = band(0., TAU) {
                window.paint_path(p, color.opacity(0.22));
            }
            if fill > 0. {
                if let Some(p) = band(0., fill * TAU) {
                    window.paint_path(p, color);
                }
                // Round caps.
                for a in [0., fill * TAU] {
                    let c = at(mid, a);
                    window.paint_quad(gpui::fill(
                        gpui::Bounds::new(gpui::point(c.x - px(half), c.y - px(half)), gpui::size(px(half * 2.), px(half * 2.))),
                        color,
                    ).corner_radii(px(half)));
                }
            }
        },
    )
    .size(px(size))
    .flex_shrink_0();
    let label = label.into();
    div()
        .flex()
        .items_center()
        .gap(px(9.))
        .min_w_0()
        .child(ring)
        .child(div().id("stage-progress-label").status().min_w_0().truncate().child(label))
}

/// `.processing-overlay`: the window dimmed under a card with a spinner and a line of text.
pub fn processing(text: SharedString) -> impl IntoElement {
    div()
        .id("processing-overlay")
        .absolute()
        .inset_0()
        .occlude()
        .flex()
        .items_center()
        .justify_center()
        .bg(ca(0, 0, 0, 0.55))
        .child(
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(16.))
                .px(px(36.))
                .py(px(28.))
                .rounded(px(16.))
                .bg(ca(20, 22, 26, 0.92))
                .shadow(vec![gpui::BoxShadow {
                    color: ca(0, 0, 0, 0.5),
                    offset: gpui::point(px(0.), px(12.)),
                    blur_radius: px(40.),
                    spread_radius: px(0.),
                    inset: false,
                }])
                .child(div().size(px(42.)).child(crate::attachments::spinner(42., 3., 0.8, 0.25, white_a(0.15), c(ACCENT))))
                .child(div().text_size(px(15.2)).text_color(c(0xe8e8ea)).child(text)),
        )
}

/// One choice in a [`select`].
pub struct Choice {
    pub value: &'static str,
    pub label: &'static str,
}

/// A select's choices in the language in use, built once per language.
pub fn choices(key: &'static str, build: impl FnOnce() -> Vec<Choice>) -> &'static [Choice] {
    thread_local!(static BUILT: std::cell::RefCell<std::collections::HashMap<(&'static str, u64), &'static [Choice]>> = Default::default());
    let at = (key, vector_i18n::generation());
    if let Some(c) = BUILT.with(|b| b.borrow().get(&at).copied()) {
        return c;
    }
    let c: &'static [Choice] = Box::leak(build().into_boxed_slice());
    BUILT.with(|b| b.borrow_mut().insert(at, c));
    c
}

/// Text made at runtime for a select's `&'static` choices, kept once per distinct string.
pub fn interned(text: &str) -> &'static str {
    thread_local!(static SEEN: std::cell::RefCell<std::collections::HashSet<&'static str>> = Default::default());
    SEEN.with(|seen| {
        let mut seen = seen.borrow_mut();
        if let Some(s) = seen.get(text) {
            return *s;
        }
        let s: &'static str = Box::leak(text.into());
        seen.insert(s);
        s
    })
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
    select_look(id, width, font, choices, disabled, value, open, SelectLook::default(), on_toggle, on_pick)
}

/// What sets a [`select_look`] apart: a mark before each label, and the tick's colour.
pub struct SelectLook {
    pub lead: Option<std::rc::Rc<dyn Fn(&'static str) -> AnyElement>>,
    pub check: u32,
    pub priority: usize,
}

impl Default for SelectLook {
    fn default() -> Self {
        Self { lead: None, check: PRIMARY, priority: 1 }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn select_look(
    id: impl Into<ElementId>,
    width: f32,
    font: f32,
    choices: &[Choice],
    disabled: &[&str],
    value: &str,
    open: bool,
    look: SelectLook,
    on_toggle: impl Fn(&mut Window, &mut App) + 'static,
    on_pick: impl Fn(&'static str, &mut Window, &mut App) + 'static + Clone,
) -> AnyElement {
    let label = choices
        .iter()
        .find(|c| c.value == value)
        .map(|c| c.label)
        .unwrap_or("");
    let id: ElementId = id.into();
    let on_toggle = std::rc::Rc::new(on_toggle);
    let live: std::rc::Rc<[(&'static str, bool)]> =
        choices.iter().map(|c| (c.value, !disabled.contains(&c.value))).collect();
    let keys = open.then(|| OPEN_SELECT.with(|o| {
        let mut o = o.borrow_mut();
        let at = live.iter().position(|(v, _)| *v == value).unwrap_or(0);
        match o.as_ref() {
            Some(k) if k.id == id => {
                if k.hi.get() == usize::MAX {
                    k.hi.set(at);
                }
            }
            _ => {
                *o = Some(SelectKeys {
                    id: id.clone(),
                    focus: None,
                    hi: std::rc::Rc::new(std::cell::Cell::new(at)),
                    shown: std::cell::Cell::new(usize::MAX),
                });
            }
        }
        o.as_ref().map(|k| (k.focus.clone(), k.hi.clone()))
    })).flatten();
    if !open {
        OPEN_SELECT.with(|o| {
            let mut o = o.borrow_mut();
            if o.as_ref().is_some_and(|k| k.id == id) {
                *o = None;
            }
        });
    }
    let caret = div().w(px(10.)).h(px(6.)).flex_shrink_0().child(
        gpui::svg()
            .path("ui/caret-down.svg")
            .size_full()
            .text_color(c(0x888888))
            .with_transformation(turn(format!("vselect-caret-{id:?}"), open, std::f32::consts::PI, 0.2, css_ease())),
    );
    let toggle = on_toggle.clone();
    let opening = !open;
    let key_id = id.clone();
    let button = div()
        .id(id.clone())
        .role(gpui::Role::ComboBox)
        .aria_expanded(open)
        .aria_value(label)
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
        .on_click(move |_, w, cx| {
            toggle(w, cx);
            // The open list takes focus, so its keys reach it before anything behind.
            if opening {
                let focus = cx.focus_handle();
                w.focus(&focus, cx);
                OPEN_SELECT.with(|o| *o.borrow_mut() = Some(SelectKeys {
                    id: key_id.clone(),
                    focus: Some(focus),
                    hi: std::rc::Rc::new(std::cell::Cell::new(usize::MAX)),
                    shown: std::cell::Cell::new(usize::MAX),
                }));
            }
        })
        .children(look.lead.as_ref().map(|lead| lead(value_of(choices, value))))
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
    let place_id = id.clone();
    let mut root = div()
        .relative()
        .map(|d| if width.is_finite() { d.w(px(width)) } else { d.w_full() })
        .flex_shrink_0()
        .child(button)
        .child(
            gpui::canvas(
                move |b, w, _| {
                    let band = w.content_mask().bounds;
                    SELECT_PLACES.with(|p| {
                        let mut p = p.borrow_mut();
                        if p.len() > 256 && !p.contains_key(&place_id) {
                            p.clear();
                        }
                        let e = p.entry(place_id.clone()).or_default();
                        (e.button, e.band) = (b, band);
                    })
                },
                |_, _, _, _| {},
            )
            .absolute()
            .inset_0(),
        );
    // `.vselect-list`: opacity 0.14s ease and a 4px rise on 0.18s, both ways.
    let shown = ease(format!("vselect-fade-{id:?}").into(), open as u8 as f32, 0.14, css_ease());
    let slide = ease(format!("vselect-slide-{id:?}").into(), open as u8 as f32, 0.18, bezier(0.22, 1., 0.36, 1.));
    if shown > 0. {
        // Toward the side with more room inside what clips it, and never taller than that side.
        let place = SELECT_PLACES.with(|p| p.borrow().get(&id).cloned()).unwrap_or_default();
        let natural = choices.len() as f32 * 34. + 10.;
        let below = f32::from(place.band.bottom() - place.button.bottom()) - 24.;
        let above = f32::from(place.button.top() - place.band.top()) - 24.;
        let up = place.button.size.height > px(0.) && below < natural.min(SELECT_CAP) && above > below;
        let room = (if up { above } else { below }).min(SELECT_CAP).max(96.);
        let mut list = div()
            .id(SharedString::from(format!("vselect-list-{id:?}")))
            .absolute()
            .map(|d| if up { d.bottom(px(42.)) } else { d.top(px(42.)) })
            .left_0()
            .right_0()
            .max_h(px(room))
            .overflow_y_scroll()
            .track_scroll(&place.scroll)
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
            .when(open, |d| d.occlude());
        let hi = keys.as_ref().map(|(_, hi)| hi.get()).unwrap_or(usize::MAX);
        OPEN_SELECT.with(|o| {
            if let Some(k) = o.borrow().as_ref().filter(|k| k.id == id && k.shown.get() != hi && hi != usize::MAX) {
                k.shown.set(hi);
                place.scroll.scroll_to_item(hi);
            }
        });
        if let Some((Some(focus), hi)) = keys {
            let (pick, toggle, live) = (on_pick.clone(), on_toggle.clone(), live.clone());
            // i18n-exempt: key context
            list = list.track_focus(&focus).key_context("Select").on_key_down(move |e, w, cx| {
                let step = |dir: isize| {
                    let n = live.len() as isize;
                    let mut at = hi.get() as isize;
                    for _ in 0..n {
                        at = (at + dir).rem_euclid(n);
                        if live[at as usize].1 {
                            hi.set(at as usize);
                            return;
                        }
                    }
                };
                match e.keystroke.key.as_str() {
                    "escape" | "tab" => toggle(w, cx),
                    "down" => step(1),
                    "up" => step(-1),
                    "enter" | "space" => {
                        if let Some(&(v, true)) = live.get(hi.get()) {
                            pick(v, w, cx);
                        }
                    }
                    _ => return,
                }
                cx.stop_propagation();
                w.refresh();
            });
        }
        for (ix, choice) in choices.iter().enumerate() {
            let selected = choice.value == value;
            let off = disabled.contains(&choice.value);
            let pick = on_pick.clone();
            let v = choice.value;
            list = list.child(
                div()
                    .id(choice.value)
                    .role(gpui::Role::ListBoxOption)
                    .aria_label(choice.label)
                    .aria_selected(selected)
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .h(px(34.))
                    .px(px(8.))
                    .rounded(px(6.))
                    .text_size(px(font))
                    .text_color(c(if selected { 0xffffff } else { 0xcfcfcf }))
                    .when(selected || ix == hi, |s| s.bg(c(0x1f1f1f)))
                    .when(off, |s| s.opacity(0.4))
                    .when(!off && open, |s| {
                        s.hover(|s| s.bg(c(0x1f1f1f)).text_color(c(0xffffff)))
                            .cursor_pointer()
                            .on_click(move |_, w, cx| pick(v, w, cx))
                    })
                    .children(look.lead.as_ref().map(|lead| lead(v)))
                    .child(div().flex_1().min_w_0().truncate().child(choice.label))
                    .when(selected, |s| {
                        s.child(
                            gpui::svg()
                                .path("icons/check.svg")
                                .size(px(14.))
                                .text_color(c(look.check)),
                        )
                    }),
            );
        }
        let list = gpui::zoom(list).shift(0., if up { 4. } else { -4. } * (1. - slide)).opacity(shown);
        root = root.child(gpui::deferred(list).with_priority(look.priority));
    }
    root.into_any_element()
}

/// A popup's content: `popupConfirm` (misc.js).
#[derive(Clone)]
pub struct Popup {
    pub title: SharedString,
    pub text: Option<SharedString>,
    pub icon: Option<&'static str>,
    /// A picture file in place of `icon`, cut to a circle: `.popup-icon-circular`.
    pub picture: Option<SharedString>,
    pub confirm: SharedString,
    /// None for a notice (Okay only).
    pub cancel: Option<SharedString>,
    /// A text field, for a popup that asks for a value.
    pub input: Option<gpui::Entity<crate::composer::TextInput>>,
    /// Small dimmed detail under the text.
    pub fine: Option<SharedString>,
    /// A `.net-link` button after the text; it closes the popup itself.
    pub link: Option<(SharedString, std::rc::Rc<dyn Fn(&mut Window, &mut App)>)>,
}

impl Popup {
    pub fn notice(title: impl Into<SharedString>) -> Self {
        let title: SharedString = title.into();
        Self {
            title: match crate::coretext::say(&title) {
                std::borrow::Cow::Owned(said) => said.into(),
                std::borrow::Cow::Borrowed(_) => title,
            },
            text: None,
            icon: Some("icons/vector_warning.svg"),
            picture: None,
            confirm: crate::t::dialog_okay(),
            cancel: None,
            input: None,
            fine: None,
            link: None,
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
    if let Some(picture) = &p.picture {
        let path = std::path::PathBuf::from(picture.as_str());
        content = content.child(crate::pic::img_face(path, 100.).size(px(100.)).rounded_full().object_fit(gpui::ObjectFit::Cover).mb(px(26.)));
    } else if let Some(icon) = p.icon {
        content = content.child(crate::pic::img_fit(icon, 100.).h(px(100.)).w(px(100.)).mb(px(26.)));
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
        let text = &match crate::coretext::say(text) {
            std::borrow::Cow::Owned(said) => SharedString::from(said),
            std::borrow::Cow::Borrowed(_) => text.clone(),
        };
        let body: AnyElement = if text.contains('<') || text.contains('&') {
            rich(text).into_any_element()
        } else {
            text.clone().into_any_element()
        };
        let list = text.starts_with("• ");
        content = content.child(
            div()
                .when(!list, |d| d.text_center())
                .when(list, |d| d.line_height(px(24.)))
                .text_color(c(TEXT))
                .mb(px(16.))
                .when(p.icon.is_some() || p.picture.is_some(), |d| d.max_w(px(420.)))
                .child(body)
                .when_some(p.link.clone(), |d, (label, go)| {
                    d.child(
                        div().mt(px(16.)).flex().justify_center().child(
                            div()
                                .id("popup-link")
                                .text_color(c(0x59fcb3))
                                .font_weight(FontWeight::SEMIBOLD)
                                .cursor_pointer()
                                .hover(|s| s.underline())
                                .on_click(move |_, w, cx| go(w, cx))
                                .child(label),
                        ),
                    )
                })
                .when_some(p.fine.clone(), |d, fine| {
                    d.child(div().mt(px(22.)).text_center().text_size(px(12.)).opacity(0.5).child(fine))
                }),
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
        .max_w_full()
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
            .px(px(10.))
            .bg(ca(0, 0, 0, 0.85))
            .occlude()
            .child(card)
            // `.popup-container`: the dimming fades in over 0.2 s; the card itself shows at once.
            .with_animation(
                "popup-dim",
                gpui::Animation::new(std::time::Duration::from_millis(200)).with_easing(gpui::ease_in_out),
                |d, t| d.bg(ca(0, 0, 0, 0.85 * t)),
            ),
    )
    // Above every modal a popup can be asked from (Invite People, Community Settings, ...).
    .with_priority(15)
    .into_any_element()
}

/// `qr-overlay-pop`: a modal card grows from 0.92 and fades in over `ms` with CSS `ease-out`.
pub fn pop(id: impl Into<ElementId>, el: impl IntoElement, ms: u64) -> AnyElement {
    pop_or_out(id, el, ms, CLOSING.get())
}

/// [`pop`], or `modal-card-pop-out` (scaling down to 0.92) while its modal fades at `shown`.
pub fn pop_or_out(id: impl Into<ElementId>, el: impl IntoElement, ms: u64, shown: f32) -> AnyElement {
    if shown < 1. {
        return gpui::zoom(el).scale(0.92 + 0.08 * shown).into_any_element();
    }
    gpui::zoom(el)
        .with_animation(
            id,
            gpui::Animation::new(std::time::Duration::from_millis(ms)).with_easing(bezier(0., 0., 0.58, 1.)),
            |z, t| z.scale(0.92 + 0.08 * t).opacity(t),
        )
        .into_any_element()
}

/// A CSS entrance on `transform` and `opacity`: drawn `dy` points lower (negative: higher) and
/// at `scale`, rising into place and fading in over `ms`, eased by `ease`.
pub fn rise(
    id: impl Into<ElementId>,
    el: impl IntoElement,
    ms: u64,
    (dx, dy, scale): (f32, f32, f32),
    ease: impl Fn(f32) -> f32 + 'static,
) -> AnyElement {
    gpui::zoom(el)
        .with_animation(id, gpui::Animation::new(std::time::Duration::from_millis(ms)).with_easing(ease), move |z, t| {
            z.scale(scale + (1. - scale) * t).shift(dx * (1. - t), dy * (1. - t)).opacity(t)
        })
        .into_any_element()
}

/// A list item's marker (`::before` under `transition: opacity, height, background-color 130ms
/// ease-out`): a short tick at the item's middle, or the long bar of the open item, eased between.
pub struct StateBar {
    pub key: SharedString,
    /// From the item's left edge (negative: outside it), and the bar's width.
    pub left: f32,
    pub width: f32,
    /// Rounded on the right only, as a bar on an edge is.
    pub edge: bool,
    pub radius: f32,
    pub tick: Tick,
    /// The long bar: a fixed height, or the item's less this much at each end.
    pub full: Full,
}

pub enum Tick {
    Px(f32),
    /// `scaleY`: this share of the long bar.
    Frac(f32),
}

pub enum Full {
    Px(f32),
    Inset(f32),
}

impl StateBar {
    /// `shown`: drawn at all; `long`: the open item's bar.
    pub fn view(self, shown: bool, long: bool, color: Hsla) -> impl IntoElement {
        let rgba = color.to_rgb();
        let target = [shown as u8 as f32, long as u8 as f32, rgba.r, rgba.g, rgba.b, rgba.a];
        let key = self.key.clone();
        gpui::canvas(|_, _, _| (), move |bounds, _, window, _| {
            let ([o, l, r, g, b, a], moving) = tween(&key, target, 0.13);
            if moving {
                window.request_animation_frame();
            }
            if o <= 0. {
                return;
            }
            let height = f32::from(bounds.size.height);
            let full = match self.full {
                Full::Px(h) => h,
                Full::Inset(i) => (height - 2. * i).max(0.),
            };
            let tick = match self.tick {
                Tick::Px(h) => h,
                Tick::Frac(f) => full * f,
            };
            let h = tick + (full - tick) * l;
            let top = bounds.origin.y + px((height - h) / 2.);
            let bar = gpui::Bounds::new(gpui::point(bounds.origin.x, top), gpui::size(px(self.width), px(h)));
            let r_px = px(self.radius.min(h / 2.));
            let corners = if self.edge {
                gpui::Corners { top_left: px(0.), bottom_left: px(0.), top_right: r_px, bottom_right: r_px }
            } else {
                gpui::Corners::all(r_px)
            };
            let fill = gpui::Hsla::from(gpui::Rgba { r, g, b, a: a * o });
            window.paint_quad(gpui::fill(bar, fill).corner_radii(corners));
        })
        .absolute()
        .top_0()
        .bottom_0()
        .left(px(self.left))
        .w(px(self.width))
    }
}

thread_local!(static TWEENS: std::cell::RefCell<std::collections::HashMap<SharedString, Tween>> = Default::default());

struct Tween {
    from: [f32; 6],
    to: [f32; 6],
    since: std::time::Instant,
}

/// `key`'s values eased toward `to` over `secs` with CSS ease-out from wherever they stood when
/// `to` last changed; the first sight takes `to` as is. Also whether they are still moving.
fn tween(key: &SharedString, to: [f32; 6], secs: f32) -> ([f32; 6], bool) {
    TWEENS.with_borrow_mut(|all| {
        let now = std::time::Instant::now();
        let at = |t: &Tween| -> [f32; 6] {
            let k = bezier(0., 0., 0.58, 1.)(((now - t.since).as_secs_f32() / secs).min(1.));
            std::array::from_fn(|i| t.from[i] + (t.to[i] - t.from[i]) * k)
        };
        let t = all.entry(key.clone()).or_insert(Tween { from: to, to, since: now });
        if t.to != to {
            let from = at(t);
            *t = Tween { from, to, since: now };
        }
        let moving = (now - t.since).as_secs_f32() < secs;
        (if moving { at(t) } else { to }, moving)
    })
}

/// Frames of a white wash, `alpha(x, t)` across `w` pixels at each of `frames` steps of a
/// `ms` loop: stretched over a box and stepped like any animated picture, so a looping
/// placeholder swaps a tile instead of redrawing what holds it.
fn wash(w: u32, frames: u32, ms: u32, alpha: impl Fn(f32, f32) -> f32) -> std::sync::Arc<gpui::RenderImage> {
    let frames: Vec<image::Frame> = (0..frames)
        .map(|f| {
            let t = f as f32 / frames as f32;
            let mut buf = image::RgbaImage::new(w, 1);
            for (x, _, px) in buf.enumerate_pixels_mut() {
                let a = (alpha((x as f32 + 0.5) / w as f32, t).clamp(0., 1.) * 255.).round() as u8;
                *px = image::Rgba([0xff, 0xff, 0xff, a]);
            }
            image::Frame::from_parts(buf, 0, 0, image::Delay::from_numer_denom_ms(ms / frames, 1))
        })
        .collect();
    std::sync::Arc::new(gpui::RenderImage::new(frames))
}

/// `.pack-skel`: a placeholder shape with a light sweeping across it and back every 1.6 s.
pub fn skeleton(radius: f32) -> gpui::Div {
    thread_local! {
        static SHIMMER: std::sync::Arc<gpui::RenderImage> = wash(48, 48, 1600, |u, t| {
            let ease = bezier(0.42, 0., 0.58, 1.);
            // `background-position` 200% to -200% and back, over a 200%-wide gradient.
            let p = if t < 0.5 { 2. - 4. * ease(t * 2.) } else { -2. + 4. * ease(t * 2. - 1.) };
            0.03 + 0.06 * (1. - (u - (1. - p)).abs()).max(0.)
        });
    }
    let sheen = SHIMMER.with(|s| s.clone());
    div().relative().rounded(px(radius)).child(
        gpui::StyledImage::object_fit(crate::pic::img(gpui::ImageSource::Render(sheen)), gpui::ObjectFit::Fill)
            .absolute()
            .inset_0()
            .rounded(px(radius)),
    )
}

/// `img.emoji-img-loading`: a custom emoji's place while it loads, a soft pulse every 1.4 s.
pub fn emoji_loading(size: f32) -> gpui::Div {
    thread_local! {
        static PULSE: std::sync::Arc<gpui::RenderImage> = wash(1, 28, 1400, |_, t| {
            0.04 + 0.06 * bezier(0.42, 0., 0.58, 1.)(1. - (2. * t - 1.).abs())
        });
    }
    let pulse = PULSE.with(|s| s.clone());
    div().size(px(size)).flex_shrink_0().child(
        gpui::StyledImage::object_fit(crate::pic::img(gpui::ImageSource::Render(pulse)), gpui::ObjectFit::Fill)
            .size_full()
            .rounded(px(4.)),
    )
}

thread_local! {
    static EASES: std::cell::RefCell<std::collections::HashMap<SharedString, (f32, f32, std::time::Instant)>> = Default::default();
    static EASING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// A render-time transition: `key` eased toward `to` over `secs` along `curve`, from wherever it
/// stood when `to` last changed, as CSS retargets one. The first sight takes `to` as is.
pub fn ease(key: SharedString, to: f32, secs: f32, curve: impl Fn(f32) -> f32) -> f32 {
    EASES.with_borrow_mut(|all| {
        let now = std::time::Instant::now();
        let at = |e: &(f32, f32, std::time::Instant)| e.0 + (e.1 - e.0) * curve(((now - e.2).as_secs_f32() / secs).min(1.));
        let e = all.entry(key).or_insert((to, to, now));
        if e.1 != to {
            *e = (at(e), to, now);
        }
        if (now - e.2).as_secs_f32() < secs {
            EASING.set(true);
            at(e)
        } else {
            to
        }
    })
}

/// A glyph's `transition: transform` turn: `to` radians while `on`, eased there and back.
pub fn turn(key: impl Into<SharedString>, on: bool, to: f32, secs: f32, curve: impl Fn(f32) -> f32) -> gpui::Transformation {
    gpui::Transformation::rotate(gpui::radians(to * ease(key.into(), on as u8 as f32, secs, curve)))
}

/// CSS `ease`.
pub fn css_ease() -> impl Fn(f32) -> f32 {
    bezier(0.25, 0.1, 0.25, 1.)
}

/// Last in a view that builds rows at layout (a list's): asks for the next frame while an
/// [`ease`] they drew still moves.
pub fn pump() -> impl IntoElement {
    gpui::canvas(
        |_, window, _| {
            if easing() {
                window.request_animation_frame();
            }
        },
        |_, _, _, _| {},
    )
    .absolute()
    .size_0()
}

/// Whether an [`ease`] built since the last call is still moving: its view wants another frame.
pub fn easing() -> bool {
    EASING.replace(false)
}

thread_local!(static CLOSING: std::cell::Cell<f32> = const { std::cell::Cell::new(1.) });

/// Builds a closing modal: the card its view pops in with [`pop`] pops out instead.
pub fn closing<R>(shown: f32, build: impl FnOnce() -> R) -> R {
    let was = CLOSING.replace(shown);
    let r = build();
    CLOSING.set(was);
    r
}

/// `cic-pop-in`: a card that just resolved settles in from 0.97 with a little overshoot.
pub fn settle_pop(id: impl Into<ElementId>, el: impl IntoElement) -> gpui::AnimationElement<gpui::Zoom> {
    gpui::zoom(el).with_animation(
        id,
        gpui::Animation::new(std::time::Duration::from_millis(280)).with_easing(bezier(0.34, 1.56, 0.64, 1.)),
        |z, t| z.scale(0.97 + 0.03 * t).opacity((0.6 + 0.4 * t).min(1.)),
    )
}

/// Which async cards resolved on screen, so only a fresh resolve animates: one already
/// settled when first drawn, or drawn again after scrolling away, fills in place.
#[derive(Default)]
pub struct Settle(std::collections::HashMap<String, Settled>);

enum Settled {
    Loading,
    At(std::time::Instant),
    Done,
}

impl Settle {
    /// The card `key` is drawn, resolved or not: whether its entrance is still playing.
    pub fn fresh(&mut self, key: &str, resolved: bool) -> bool {
        let Some(seen) = self.0.get_mut(key) else {
            self.0.insert(key.to_string(), if resolved { Settled::Done } else { Settled::Loading });
            return false;
        };
        match seen {
            Settled::Loading if resolved => *seen = Settled::At(std::time::Instant::now()),
            Settled::At(at) if at.elapsed().as_millis() >= 300 => *seen = Settled::Done,
            _ => {}
        }
        matches!(seen, Settled::At(_))
    }

    /// `row` is drawn: whether it was drawn before. Ask once per draw, before its parts.
    pub fn row(&mut self, row: &str) -> bool {
        if self.0.contains_key(row) {
            return true;
        }
        self.0.insert(row.to_string(), Settled::Done);
        false
    }

    /// `key`, a part of a row that `row` answered `seen` for, is drawn: whether it arrived
    /// after the row first drew and is still entering. One drawn with its row fills in place.
    pub fn part(&mut self, seen: bool, key: &str) -> bool {
        if !self.0.contains_key(key) {
            self.0.insert(key.to_string(), if seen { Settled::Loading } else { Settled::Done });
        }
        self.fresh(key, true)
    }

    /// `row` drawn again as if for the first time: what it holds now fills in place.
    pub fn forget(&mut self, row: &str) {
        self.0.remove(row);
    }
}

thread_local! {
    static ROLLS: std::cell::RefCell<std::collections::HashMap<SharedString, (usize, usize, std::time::Instant)>> = Default::default();
}

/// Rollers whose key starts with `prefix` show their next count without rolling to it.
pub fn roll_forget(prefix: &str) {
    ROLLS.with_borrow_mut(|all| all.retain(|k, _| !k.starts_with(prefix)));
}

/// `.reaction-count`'s roller: on a change the old number slides out (up on a rise, down on a
/// fall) as the new one slides in from the other edge, inside a `line` px clip.
pub fn roll(key: SharedString, n: usize, line: f32) -> AnyElement {
    const SECS: f32 = 0.34;
    let now = std::time::Instant::now();
    let (from, to, at) = ROLLS.with_borrow_mut(|all| {
        let e = all.entry(key).or_insert((n, n, now - std::time::Duration::from_secs(1)));
        if e.1 != n {
            *e = (e.1, n, now);
        }
        *e
    });
    let t = (now - at).as_secs_f32() / SECS;
    if t >= 1. || from == to {
        return div().child(crate::lang::num(n)).into_any_element();
    }
    let k = bezier(0.22, 1., 0.36, 1.)(t);
    let dir = if to > from { 1. } else { -1. };
    div()
        .relative()
        .overflow_hidden()
        .h(px(line))
        .child(div().relative().top(px(dir * (1. - k) * line)).opacity(k).child(crate::lang::num(to)))
        .child(div().absolute().left_0().top(px(-dir * k * line)).opacity(1. - k).child(crate::lang::num(from)))
        .child(gpui::canvas(|_, window, _| window.request_animation_frame(), |_, _, _, _| {}).absolute().size_0())
        .into_any_element()
}

/// `reaction-pop-in`: a chip that arrived on a drawn message grows in from 0.6.
pub fn chip_pop(id: impl Into<ElementId>, el: impl IntoElement) -> gpui::AnimationElement<gpui::Zoom> {
    gpui::zoom(el).with_animation(
        id,
        gpui::Animation::new(std::time::Duration::from_millis(180)).with_easing(bezier(0.22, 1., 0.36, 1.)),
        |z, t| z.scale(0.6 + 0.4 * t).opacity(t),
    )
}

/// `.closing`: a modal kept on screen from its last open state while it fades out over 0.15 s
/// (`modal-overlay-fade-out`). Closing is seen at render, so every way out gets it.
pub struct Linger<T> {
    last: std::cell::RefCell<Option<T>>,
    gone: std::cell::Cell<Option<Fade>>,
}

/// How far a fade-out has run. It advances by at most a frame's worth per draw, so a slow
/// frame (the closing action's own work) slows the fade instead of skipping most of it.
#[derive(Clone, Copy)]
struct Fade {
    /// 0 to 1.
    t: f32,
    /// The draw before; none until the first frame after the close.
    at: Option<std::time::Instant>,
}

impl Fade {
    const NEW: Self = Self { t: 0., at: None };
    const STEP_MAX: f32 = 1. / 30.;

    fn step(&mut self, secs: f32) -> f32 {
        let now = std::time::Instant::now();
        if let Some(at) = self.at {
            self.t += (now - at).as_secs_f32().min(Self::STEP_MAX) / secs;
        }
        self.at = Some(now);
        self.t
    }
}

impl<T> Default for Linger<T> {
    fn default() -> Self {
        Self { last: Default::default(), gone: Default::default() }
    }
}

const LINGER_SECS: f32 = 0.15;

impl<T: Clone> Linger<T> {
    /// What to draw, and its opacity: 1 while open, falling to 0 once closed.
    pub fn track(&self, live: Option<&T>, window: &mut Window) -> Option<(T, f32)> {
        self.track_for(live, LINGER_SECS, bezier(0., 0., 0.58, 1.), window)
    }

    /// [`Self::track`] over `secs`, eased by `ease`.
    pub fn track_for(&self, live: Option<&T>, secs: f32, ease: impl Fn(f32) -> f32, window: &mut Window) -> Option<(T, f32)> {
        if let Some(v) = live {
            *self.last.borrow_mut() = Some(v.clone());
            self.gone.set(None);
            return Some((v.clone(), 1.));
        }
        let last = self.last.borrow().clone()?;
        let mut fade = self.gone.get().unwrap_or(Fade::NEW);
        let t = fade.step(secs);
        self.gone.set(Some(fade));
        if t >= 1. {
            *self.last.borrow_mut() = None;
            self.gone.set(None);
            return None;
        }
        window.request_animation_frame();
        Some((last, 1. - ease(t)))
    }

    /// True while a closed modal is still fading.
    pub fn leaving(&self) -> bool {
        self.gone.get().is_some()
    }
}

impl<T> Linger<T> {
    /// Fade out what was just closed, for state too large to copy every frame.
    pub fn close(&self, gone: Option<T>) {
        if let Some(v) = gone {
            *self.last.borrow_mut() = Some(v);
            self.gone.set(Some(Fade::NEW));
        }
    }

    /// Draw the closed state while it fades, with its opacity.
    pub fn fading<R>(&self, window: &mut Window, draw: impl FnOnce(&T, f32, &mut Window) -> R) -> Option<R> {
        let mut fade = self.gone.get()?;
        let t = fade.step(LINGER_SECS);
        self.gone.set(Some(fade));
        if t >= 1. {
            *self.last.borrow_mut() = None;
            self.gone.set(None);
            return None;
        }
        window.request_animation_frame();
        let last = self.last.borrow();
        let last = last.as_ref()?;
        let shown = 1. - bezier(0., 0., 0.58, 1.)(t);
        Some(closing(shown, || draw(last, shown, window)))
    }
}

/// A fading modal's view, dimmed and kept from clicks.
pub fn faded(view: impl IntoElement, shown: f32, priority: usize) -> AnyElement {
    div()
        .absolute()
        .inset_0()
        .opacity(shown)
        .child(view)
        .child(gpui::deferred(inert()).with_priority(priority + 1))
        .into_any_element()
}

/// Over a fading modal: takes the clicks its buttons would, as `pointer-events: none` keeps them
/// from a closing modal.
pub fn inert() -> gpui::Div {
    div().absolute().inset_0().occlude()
}

/// `.popup-container` once its popup is answered: the card goes at once, the dimming fades.
pub fn popup_gone(gone: &Linger<()>, open: bool, window: &mut Window) -> Option<AnyElement> {
    // `.popup-container`'s `transition: background-color 0.2s ease`.
    let (_, shown) = gone.track_for(open.then_some(&()), 0.2, bezier(0.25, 0.1, 0.25, 1.), window)?;
    // Not `shown < 1.`: the first frame after the close is at 1, and skipping it flashes undimmed.
    (!open).then(|| {
        gpui::deferred(div().absolute().inset_0().bg(ca(0, 0, 0, 0.85 * shown)).child(inert()))
            .with_priority(15)
            .into_any_element()
    })
}

/// CSS group opacity for a pane over the app background: the pane stays opaque and this cover
/// fades off it, since GPUI's opacity reaches each layer apart and stacked fills bleed through.
pub fn veil(shown: f32) -> Option<gpui::Div> {
    (shown < 1.).then(|| div().absolute().inset_0().bg(c(APP_BG).opacity(1. - shown)))
}

/// A one-shot entrance (`revealPane`) for a surface that comes and goes: it plays when the
/// surface is drawn after a root render that left it out.
#[derive(Default)]
pub struct Reveal {
    seen: bool,
    was: bool,
    at: Option<std::time::Instant>,
}

impl Reveal {
    /// Once per root render, before anything is drawn.
    pub fn frame(&mut self) {
        self.was = std::mem::take(&mut self.seen);
    }

    /// The surface is drawn in this render: how far its entrance has come, 0 to 1.
    pub fn shown(&mut self, secs: f32, window: &mut Window) -> f32 {
        self.seen = true;
        if !self.was {
            self.was = true;
            self.at = Some(std::time::Instant::now());
        }
        let t = self.at.map_or(1., |at| (at.elapsed().as_secs_f32() / secs).min(1.));
        if t < 1. {
            window.request_animation_frame();
        }
        t
    }
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
        if self.0.is_empty() {
            return gpui::div().into_any_element();
        }
        gpui::div()
            .px(gpui::px(8.))
            .py(gpui::px(4.))
            .rounded(gpui::px(6.))
            .bg(crate::theme::c(0x1c1c1c))
            .border_1()
            .border_color(crate::theme::white_a(0.1))
            .font_family(crate::theme::INTER)
            .lang_fonts()
            .text_size(gpui::px(12.))
            .text_color(crate::theme::white_a(0.9))
            .child(self.0.clone())
            // `.global-tooltip`: fades in over 0.15 s.
            .with_animation("tip-in", gpui::Animation::new(std::time::Duration::from_millis(150)).with_easing(css_ease()), |d, t| d.opacity(t))
            .into_any_element()
    }
}

thread_local! {
    static PRESSES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// A press anywhere. A tooltip still waiting to show when one lands stays hidden, as a
/// browser's does: GPUI's delayed show ignores whatever the press opened over it.
pub fn note_press() {
    PRESSES.with(|p| p.set(p.get() + 1));
}

thread_local! {
    static PRESS_FOCUS: std::cell::RefCell<Option<(std::time::Instant, Option<gpui::FocusHandle>)>> = const { std::cell::RefCell::new(None) };
}

/// What held focus as the latest press began, before the press moved it.
pub fn note_press_focus(focus: Option<gpui::FocusHandle>) {
    PRESS_FOCUS.with(|f| *f.borrow_mut() = Some((std::time::Instant::now(), focus)));
}

/// [`note_press_focus`]'s handle, if a press just landed; `None` when none did.
pub fn take_press_focus() -> Option<Option<gpui::FocusHandle>> {
    PRESS_FOCUS.with(|f| f.borrow_mut().take()).filter(|(at, _)| at.elapsed() < std::time::Duration::from_millis(500)).map(|(_, h)| h)
}

fn presses() -> u64 {
    PRESSES.with(|p| p.get())
}

/// A hover tooltip that also describes the element to a screen reader, and names it when
/// the element has no text of its own.
pub trait Tipped: gpui::StatefulInteractiveElement + Sized {
    fn tip(self, text: impl Into<gpui::SharedString>) -> Self {
        let text = text.into();
        self.aria_description(text.clone()).tooltip(tooltip(text))
    }

    /// `role="status"`: read out when it changes, once the reader is idle.
    fn status(self) -> Self {
        self.role(gpui::Role::Status).aria_live(gpui::accesskit::Live::Polite)
    }

    /// `role="alert"`: read out at once.
    fn alert(self) -> Self {
        self.role(gpui::Role::Alert).aria_live(gpui::accesskit::Live::Assertive)
    }

    /// A screen reader's view of a track: its name, the value as shown, and where it sits (0 to 1).
    fn as_slider(self, label: impl Into<gpui::SharedString>, shown: impl Into<gpui::SharedString>, frac: f32) -> Self {
        self.role(gpui::Role::Slider)
            .aria_label(label)
            .aria_value(shown)
            .aria_numeric_value((frac.clamp(0., 1.) * 100.).round() as f64)
            .aria_min_numeric_value(0.)
            .aria_max_numeric_value(100.)
    }

    /// A slider's keys once focused: arrows by `step`, Page Up/Down by a tenth, Home and End.
    /// Under a mirrored layout the track fills from the right, so ← raises it.
    fn slider_keys(self, frac: f32, step: f32, set: impl Fn(f32, &mut gpui::Window, &mut gpui::App) + 'static) -> Self {
        self.track_keys(frac, step, gpui::layout_rtl(), set)
    }

    /// [`slider_keys`](Tipped::slider_keys) for a timeline drawn left to right in every language.
    fn seek_keys(self, frac: f32, step: f32, set: impl Fn(f32, &mut gpui::Window, &mut gpui::App) + 'static) -> Self {
        self.track_keys(frac, step, false, set)
    }

    fn track_keys(self, frac: f32, step: f32, mirrored: bool, set: impl Fn(f32, &mut gpui::Window, &mut gpui::App) + 'static) -> Self {
        use gpui::accesskit::{Action, ActionData};
        let set = std::rc::Rc::new(set);
        let (up, down, to) = (set.clone(), set.clone(), set.clone());
        self.on_a11y_action(Action::Increment, move |_, w, cx| up((frac + step).clamp(0., 1.), w, cx))
            .on_a11y_action(Action::Decrement, move |_, w, cx| down((frac - step).clamp(0., 1.), w, cx))
            .on_a11y_action(Action::SetValue, move |data, w, cx| {
                if let Some(ActionData::NumericValue(v)) = data {
                    to((*v as f32 / 100.).clamp(0., 1.), w, cx);
                }
            })
            .on_key_down(move |e: &gpui::KeyDownEvent, w, cx| {
            let key = match e.keystroke.key.as_str() {
                "left" if mirrored => "right",
                "right" if mirrored => "left",
                k => k,
            };
            let to = match key {
                "left" | "down" => frac - step,
                "right" | "up" => frac + step,
                "pagedown" => frac - 0.1,
                "pageup" => frac + 0.1,
                "home" => 0.,
                "end" => 1.,
                _ => return,
            };
            cx.stop_propagation();
            set(to.clamp(0., 1.), w, cx);
            })
    }
}

impl<T: gpui::StatefulInteractiveElement> Tipped for T {}

pub fn tooltip(
    text: impl Into<gpui::SharedString>,
) -> impl Fn(&mut gpui::Window, &mut gpui::App) -> gpui::AnyView + 'static {
    let text = text.into();
    let at = presses();
    move |_, cx| {
        let shown = if presses() == at { text.clone() } else { SharedString::default() };
        cx.new(|_| Tip(shown)).into()
    }
}

/// `countdownTip`: a hover label counting down each second to `until` (unix seconds), from
/// `now` (unix seconds) at build.
pub fn countdown_tip(
    until: u64,
    now: u64,
    label: fn(i64) -> String,
) -> impl Fn(&mut gpui::Window, &mut gpui::App) -> gpui::AnyView + 'static {
    let at = presses();
    move |_, cx| {
        if presses() != at {
            return cx.new(|_| Tip(SharedString::default())).into();
        }
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
        Some(image) => crate::pic::img_plain(image).size(px(size)),
        None => crate::pic::img_plain(path).size(px(size)),
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
    rich_with(html, gpui::HighlightStyle { font_weight: Some(FontWeight::BOLD), ..Default::default() })
}

/// [`rich`] with its own look for `<b>`.
pub fn rich_with(html: &str, strong: gpui::HighlightStyle) -> gpui::StyledText {
    let (text, bold) = untag(html);
    gpui::StyledText::new(text).with_highlights(bold.into_iter().map(|r| (r, strong)))
}

/// `before <tag>inside</tag> after` in its three parts, for a sentence with one live span.
/// Spaces at the joins become no-break spaces, which line layout can't trim.
pub fn around(text: &str, tag: &str) -> (String, String, String) {
    let (open, close) = (format!("<{tag}>"), format!("</{tag}>"));
    let Some((before, rest)) = text.split_once(&open) else { return (text.to_string(), String::new(), String::new()) };
    let (inside, after) = rest.split_once(&close).unwrap_or((rest, ""));
    let nb = |s: &str| s.replace(' ', "\u{a0}");
    let (b, a) = (before.trim_end_matches(' '), after.trim_start_matches(' '));
    let before = if b.len() < before.len() { format!("{b}{}", nb(&before[b.len()..])) } else { before.to_string() };
    let after = if a.len() < after.len() { format!("{}{a}", nb(&after[..after.len() - a.len()])) } else { after.to_string() };
    (before, inside.to_string(), after)
}

/// The text of [`rich`] markup, and the ranges its `<b>` covers.
pub fn untag(html: &str) -> (String, Vec<std::ops::Range<usize>>) {
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
    (text, bold)
}

/// [`rich`] markup as `(text, bold)` runs.
pub fn runs(html: &str) -> Vec<(String, bool)> {
    let (text, bold) = untag(html);
    let mut out = Vec::with_capacity(bold.len() * 2 + 1);
    let mut at = 0;
    for r in bold {
        if r.start > at {
            out.push((text[at..r.start].to_string(), false));
        }
        out.push((text[r.clone()].to_string(), true));
        at = r.end;
    }
    if at < text.len() {
        out.push((text[at..].to_string(), false));
    }
    out
}

/// A sentence's pieces in the order they show, for a `keep_ltr` row of one element each: cut
/// where the direction changes and reordered as one paragraph (UAX #9; the UI's direction,
/// or the first letter's in a right-to-left UI, as labels take). A piece with nothing to cut,
/// such as a pill, passes `\u{FFFC}` and stays whole. Direction marks are dropped once read.
pub fn visual<T: Clone>(pieces: Vec<(String, T)>) -> Vec<(String, T)> {
    visual_in(pieces, gpui::layout_rtl())
}

fn visual_in<T: Clone>(pieces: Vec<(String, T)>, rtl_ui: bool) -> Vec<(String, T)> {
    let text: String = pieces.iter().map(|p| p.0.as_str()).collect();
    if !crate::inline::has_rtl(&text) {
        return pieces;
    }
    let base = (!rtl_ui).then(unicode_bidi::Level::ltr);
    let info = unicode_bidi::BidiInfo::new(&text, base);
    let para = info.paragraphs.first().map_or(0, |p| p.level.number());
    let mut levels: Vec<u8> = info.levels.iter().map(|l| l.number()).collect();
    // L1: trailing spaces take the paragraph's level.
    for (at, ch) in text.char_indices().rev() {
        if !ch.is_whitespace() {
            break;
        }
        levels[at..at + ch.len_utf8()].fill(para);
    }
    let mark = |c: char| matches!(c, '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}');
    let mut cut: Vec<(String, T, u8)> = Vec::with_capacity(pieces.len() + 2);
    let mut at = 0;
    for (s, tag) in pieces {
        let mut open = false;
        for (k, ch) in s.char_indices() {
            if mark(ch) {
                continue;
            }
            let level = levels[at + k];
            match cut.last_mut() {
                Some((t, _, l)) if open && *l == level => t.push(ch),
                _ => cut.push((ch.to_string(), tag.clone(), level)),
            }
            open = true;
        }
        at += s.len();
    }
    let order = crate::inline::visual_order(&cut.iter().map(|c| c.2).collect::<Vec<_>>());
    let mut slots: Vec<Option<(String, T, u8)>> = cut.into_iter().map(Some).collect();
    order
        .into_iter()
        .filter_map(|k| slots[k].take())
        .map(|(s, tag, level)| {
            // Shaped alone, punctuation has no direction: a right-to-left run's turns here.
            if level % 2 == 1 && !crate::inline::has_rtl(&s) {
                (s.chars().rev().map(|c| unicode_bidi_mirroring::get_mirrored(c).unwrap_or(c)).collect(), tag)
            } else {
                (s, tag)
            }
        })
        .collect()
}

/// Whether a sentence reads left to right in a right-to-left UI (a line not yet translated,
/// say): a row of its pieces then keeps them in reading order with `keep_ltr`. Markup and
/// isolated values don't count (UAX #9 P2).
pub fn reads_ltr(html: &str) -> bool {
    use unicode_bidi::BidiClass::*;
    if !gpui::layout_rtl() {
        return false;
    }
    let (mut tag, mut isolated) = (false, 0u32);
    for ch in html.chars() {
        match (ch, unicode_bidi::bidi_class(ch)) {
            ('<', _) => tag = true,
            ('>', _) => tag = false,
            _ if tag => {}
            (_, LRI | RLI | FSI) => isolated += 1,
            (_, PDI) => isolated = isolated.saturating_sub(1),
            _ if isolated > 0 => {}
            (_, L) => return true,
            (_, R | AL) => return false,
            _ => {}
        }
    }
    false
}

/// [`rich`] markup as one line, `<b>` runs styled by `strong`: shaped whole, so a right-to-left
/// language orders its runs and punctuation as one sentence.
pub fn run_text(html: &str, strong: gpui::HighlightStyle) -> gpui::StyledText {
    let (text, bold) = untag(html);
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
    let at = |b: gpui::Bounds<gpui::Pixels>, x: gpui::Pixels| track_frac(b, x, px(10.));
    let down = {
        let (on, bounds, sliding) = (on.clone(), bounds.clone(), sliding.clone());
        move |e: &gpui::MouseDownEvent, w: &mut Window, cx: &mut App| {
            cx.stop_propagation();
            sliding.set(Some(ix));
            on(ix, at(bounds.get(), e.position.x), w, cx);
        }
    };
    let on_key = on.clone();
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
    let spoken = (label.clone(), shown.clone());
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
                .as_slider(spoken.0, spoken.1, value)
                .slider_keys(value, 0.05, move |f, w, cx| on_key(ix, f, w, cx))
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
    keys: Option<(gpui::FocusHandle, std::rc::Rc<std::cell::Cell<usize>>)>,
    on_close: impl Fn(&mut Window, &mut App) + 'static,
) -> AnyElement {
    let on_pick = std::rc::Rc::new(on_pick);
    let on_slide = std::rc::Rc::new(on_slide);
    let on_close = std::rc::Rc::new(on_close);
    let steps: std::rc::Rc<[usize]> = items
        .iter()
        .enumerate()
        .filter(|(_, i)| !matches!(i.kind, MenuKind::Divider | MenuKind::Header) && i.slider.is_none() && !i.disabled)
        .map(|(ix, _)| ix)
        .collect();
    let hi = keys.as_ref().map_or(usize::MAX, |(_, hi)| hi.get());
    let mut list = div()
        .id("context-menu")
        .role(gpui::Role::Menu)
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
    if let Some((focus, hi)) = keys {
        let (pick, close, steps) = (on_pick.clone(), on_close.clone(), steps.clone());
        let subs: std::rc::Rc<[(bool, bool)]> = items.iter().map(|i| (i.kind == MenuKind::Sub, i.kind == MenuKind::Back)).collect();
        // i18n-exempt: key context
        list = list.track_focus(&focus).key_context("Menu").on_key_down(move |e, w, cx| {
            let at = steps.iter().position(|&ix| ix == hi.get());
            let n = steps.len();
            let go = |to: usize| hi.set(steps[to]);
            match e.keystroke.key.as_str() {
                _ if n == 0 => {
                    if e.keystroke.key == "escape" {
                        close(w, cx);
                    }
                }
                "down" => go(at.map_or(0, |a| (a + 1) % n)),
                "up" => go(at.map_or(n - 1, |a| (a + n - 1) % n)),
                "home" => go(0),
                "end" => go(n - 1),
                "enter" | "space" => {
                    if at.is_some() {
                        pick(hi.get(), w, cx);
                    }
                }
                "right" if at.is_some_and(|_| subs[hi.get()].0) => pick(hi.get(), w, cx),
                "left" => {
                    if let Some(back) = subs.iter().position(|s| s.1) {
                        pick(back, w, cx);
                    }
                }
                "escape" | "tab" => close(w, cx),
                _ => return,
            }
            cx.stop_propagation();
            w.refresh();
        });
    }
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
                .role(if item.checked { gpui::Role::MenuItemCheckBox } else { gpui::Role::MenuItem })
                .when(item.checked, |d| d.aria_toggled(gpui::Toggled::True))
                .when(item.kind == MenuKind::Sub, |d| d.aria_expanded(false))
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
                .when(ix == hi, |d| {
                    if item.danger { d.bg(danger.opacity(0.1)) } else { d.bg(c(0x2a2a2a)).text_color(c(0xffffff)) }
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
    let (c1, c2) = (on_close.clone(), on_close);
    gpui::deferred(
        div()
            .absolute()
            .inset_0()
            .on_mouse_down(gpui::MouseButton::Left, move |_, w, cx| c1(w, cx))
            .on_mouse_down(gpui::MouseButton::Right, move |_, w, cx| c2(w, cx))
            .child(
                gpui::anchored()
                    .anchor(if gpui::layout_rtl() { gpui::Anchor::TopRight } else { gpui::Anchor::TopLeft })
                    .position(at)
                    .snap_to_window_with_margin(px(8.))
                    // `.context-menu.is-visible`: down from 4px above, fading in over 0.12 s.
                    .child(rise(SharedString::from(format!("ctx-in-{:?}", at)), list, 120, (0., -4., 1.), css_ease())),
            ),
    )
    .with_priority(20)
    .into_any_element()
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_name_shows_on_one_line_as_a_page_shows_it() {
        use std::borrow::Cow;
        assert!(matches!(super::one_line("Ann Lee"), Cow::Borrowed("Ann Lee")));
        assert_eq!(super::one_line("  line one\nline two\r\n\n\tthree  "), "line one line two three");
        assert_eq!(super::one_line("a  b"), "a b");
        assert_eq!(super::one_line("no\u{a0}break"), "no\u{a0}break");
        assert_eq!(super::one_line("\n\n"), "");
        let c: crate::data::Community = serde_json::from_str(r#"{"name":"two\nlines","channels":[{"name":"\tgeneral\n"}],"categories":[{"name":"a\r\nb"}]}"#).unwrap();
        assert_eq!((c.name.as_str(), c.channels[0].name.as_str(), c.categories[0].name.as_str()), ("two lines", "general", "a b"));
    }

    #[test]
    fn a_sentence_of_pieces_shows_in_reading_order() {
        let v = |p: &[(&str, u8)], rtl| {
            super::visual_in(p.iter().map(|(t, k)| (t.to_string(), *k)).collect(), rtl)
                .into_iter()
                .map(|(t, k)| format!("{t}|{k}"))
                .collect::<Vec<_>>()
        };
        assert_eq!(v(&[("Alice", 1), (" joined.", 0)], true), ["Alice|1", " joined.|0"], "English in a right-to-left UI");
        assert_eq!(v(&[("\u{5d0}", 1), (" joined", 0)], false), ["\u{5d0}|1", " joined|0"]);
        assert_eq!(
            v(&[("\u{2068}Ann\u{2069}", 1), (" \u{5d4}\u{5e6}\u{5d8}\u{5e8}\u{5e4}\u{5d4}.", 0)], true),
            [" \u{5d4}\u{5e6}\u{5d8}\u{5e8}\u{5e4}\u{5d4}.|0", "Ann|1"],
            "an isolated name, marks dropped"
        );
        assert_eq!(
            v(&[("\u{200f}", 0), ("12", 1), (" PIV \u{5d6}\u{5de}\u{5d9}\u{5df}", 0)], true),
            [" \u{5d6}\u{5de}\u{5d9}\u{5df}|0", "PIV|0", " |0", "12|1"],
            "a Latin word and a number inside a right-to-left sentence"
        );
        assert_eq!(
            v(&[("\u{5d0}", 0), (" (", 1), ("\u{5d1}", 2), (")", 3)], true),
            ["(|3", "\u{5d1}|2", ") |1", "\u{5d0}|0"],
            "right-to-left brackets turn"
        );
        assert_eq!(v(&[("\u{5d0}", 0), (": ", 1), ("\u{5d1}", 2)], false), ["\u{5d1}|2", " :|1", "\u{5d0}|0"]);
    }

    #[test]
    fn around_splits_one_span_and_keeps_its_join_spaces() {
        let nb = '\u{a0}';
        assert_eq!(
            super::around("<hub>Explore</hub> to discover", "hub"),
            (String::new(), "Explore".to_string(), format!("{nb}to discover"))
        );
        assert_eq!(super::around("Open the <hub>Hub</hub>", "hub"), (format!("Open the{nb}"), "Hub".to_string(), String::new()));
        assert_eq!(super::around("no span", "hub"), ("no span".to_string(), String::new(), String::new()));
    }

    #[test]
    fn every_chip_drawn_with_its_message_fills_in_place() {
        let mut settle = super::Settle::default();
        let seen = settle.row("m1");
        assert!(!["a", "b", "c"].iter().any(|k| settle.part(seen, k)));
        let seen = settle.row("m1");
        assert!(settle.part(seen, "d"), "a reaction that lands later pops in");
        assert!(!settle.part(seen, "a"));
    }
}
