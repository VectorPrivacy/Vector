//! What to share on a call: every screen and every window, each with a thumbnail of how it
//! looks now. The system's own picker stands here in the web client; this one wears Vector's
//! popup card.

use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    AnyElement, App, FontWeight, InteractiveElement, IntoElement, ObjectFit, ParentElement, RenderImage,
    StatefulInteractiveElement, Styled, StyledImage, Window, div, img, prelude::FluentBuilder, px,
};

use crate::callvideo::screen::{Source, Target};
use crate::theme::{APP_BG, INTER, PRIMARY, TEXT, c, ca, white_a};

pub struct Choice {
    pub target: Target,
    pub name: String,
    pub thumb: Option<(Arc<RenderImage>, u32, u32)>,
}

pub struct Picker {
    /// None while the sources are being looked for.
    pub choices: Option<Result<Vec<Choice>, String>>,
    pub selected: Option<usize>,
    /// A share is already running: the pick replaces its source.
    pub change: bool,
}

impl Picker {
    pub fn new(change: bool) -> Self {
        Self { choices: None, selected: None, change }
    }

    pub fn fill(&mut self, found: Result<Vec<Source>, String>) {
        self.choices = Some(found.map(|v| {
            v.into_iter()
                .map(|s| Choice {
                    target: s.target,
                    name: s.name,
                    thumb: s.thumb.and_then(|(w, h, bgra)| {
                        let buf = image::RgbaImage::from_raw(w, h, bgra)?;
                        Some((Arc::new(RenderImage::new(vec![image::Frame::new(buf)])), w, h))
                    }),
                })
                .collect()
        }));
        self.selected = self.choices.as_ref().and_then(|c| c.as_ref().ok()).and_then(|c| (!c.is_empty()).then_some(0));
    }

    pub fn images(&self) -> Vec<Arc<RenderImage>> {
        self.choices
            .iter()
            .flatten()
            .flatten()
            .filter_map(|c| c.thumb.as_ref().map(|t| t.0.clone()))
            .collect()
    }

    pub fn chosen(&self) -> Option<Target> {
        let list = self.choices.as_ref()?.as_ref().ok()?;
        list.get(self.selected?).map(|c| c.target)
    }
}

pub enum Act {
    Select(usize),
    Share,
    Cancel,
}

pub type OnAct = Rc<dyn Fn(Act, &mut Window, &mut App)>;

fn tile(i: usize, ch: &Choice, selected: bool, on: &OnAct) -> AnyElement {
    let (pick, go) = (on.clone(), on.clone());
    div()
        .id(("screen-pick", i))
        .flex()
        .flex_col()
        .gap(px(6.))
        .w(px(176.))
        .p(px(6.))
        .rounded(px(8.))
        .border_1()
        .border_color(if selected { c(PRIMARY) } else { gpui::transparent_black() })
        .bg(if selected { ca(89, 252, 179, 0.08) } else { gpui::transparent_black() })
        .hover(|d| d.bg(white_a(0.05)))
        .cursor_pointer()
        .on_click(move |e, w, cx| {
            pick(Act::Select(i), w, cx);
            if e.click_count() == 2 {
                go(Act::Share, w, cx);
            }
        })
        .child(
            div()
                .relative()
                .w_full()
                .h(px(102.))
                .rounded(px(6.))
                .bg(c(0x000000))
                .overflow_hidden()
                .children(ch.thumb.as_ref().map(|(image, _, _)| img(image.clone()).absolute().inset_0().size_full().object_fit(ObjectFit::Contain))),
        )
        .child(div().truncate().text_size(px(12.)).text_color(if selected { c(0xffffff) } else { c(0xb2b2b2) }).child(ch.name.clone()))
        .into_any_element()
}

fn button(id: &'static str, label: &'static str, primary: bool, enabled: bool, on: impl Fn(&mut Window, &mut App) + 'static) -> impl IntoElement {
    div()
        .id(id)
        .px(px(20.))
        .py(px(10.))
        .rounded(px(8.))
        .border_1()
        .border_color(if primary { gpui::transparent_black() } else { white_a(0.2) })
        .when(primary, |d| d.bg(c(0x58fcb3)).text_color(c(0x000000)))
        .font_weight(FontWeight::SEMIBOLD)
        .line_height(px(19.2))
        .when(!enabled, |d| d.opacity(0.4))
        .when(enabled, |d| d.cursor_pointer().on_click(move |_, w, cx| on(w, cx)))
        .child(label)
}

pub fn render(p: &Picker, win: gpui::Size<gpui::Pixels>, on: &OnAct) -> AnyElement {
    let body: AnyElement = match &p.choices {
        None => div().py(px(40.)).text_center().text_color(c(0xb2b2b2)).child("Looking for screens and windows…").into_any_element(),
        Some(Err(e)) => div().py(px(40.)).text_center().text_color(c(0xb2b2b2)).child(e.clone()).into_any_element(),
        Some(Ok(list)) => {
            let group = |title: &'static str, monitors: bool| {
                let tiles: Vec<AnyElement> = list
                    .iter()
                    .enumerate()
                    .filter(|(_, ch)| matches!(ch.target, Target::Monitor(_)) == monitors)
                    .map(|(i, ch)| tile(i, ch, p.selected == Some(i), on))
                    .collect();
                (!tiles.is_empty()).then(|| {
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(8.))
                        .child(div().text_size(px(11.)).font_weight(FontWeight::SEMIBOLD).text_color(c(PRIMARY)).child(title.to_uppercase()))
                        .child(div().flex().flex_wrap().gap(px(8.)).children(tiles))
                })
            };
            div()
                .id("screen-pick-list")
                .flex()
                .flex_col()
                .gap(px(18.))
                .max_h(px((f32::from(win.height) - 220.).max(200.)))
                .overflow_y_scroll()
                .children(group("Screens", true))
                .children(group("Windows", false))
                .into_any_element()
        }
    };
    let (share, cancel) = (on.clone(), on.clone());
    let can = p.chosen().is_some();
    let card = div()
        .id("screen-pick")
        .flex()
        .flex_col()
        .gap(px(18.))
        .w(px(f32::from(win.width).min(620.) - 20.))
        .p(px(24.))
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
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(div().text_size(px(18.)).font_weight(FontWeight::SEMIBOLD).child(if p.change { "Share something else" } else { "Share your screen" }))
        .child(body)
        .child(
            div()
                .flex()
                .justify_end()
                .gap(px(10.))
                .child(button("screen-pick-cancel", "Cancel", false, true, move |w, cx| cancel(Act::Cancel, w, cx)))
                .child(button("screen-pick-share", "Share", true, can, move |w, cx| share(Act::Share, w, cx))),
        );
    let dismiss = on.clone();
    gpui::deferred(
        div()
            .id("screen-pick-backdrop")
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(ca(0, 0, 0, 0.85))
            .occlude()
            .on_mouse_down(gpui::MouseButton::Left, move |_, w, cx| dismiss(Act::Cancel, w, cx))
            .child(card),
    )
    .with_priority(16)
    .into_any_element()
}
