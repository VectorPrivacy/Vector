//! `.rekey-progress-overlay`: a long community operation's ring, title and step. It can't be
//! closed; the operation closes it.

use std::time::Instant;

use gpui::{AnyElement, FontWeight, InteractiveElement, IntoElement, ParentElement, SharedString, Styled, div, px, svg};

use crate::theme::{ACCENT, c};

const SWEEP: f32 = 0.55;

pub struct Rekey {
    pub title: SharedString,
    pub step: SharedString,
    from: f32,
    to: f32,
    since: Instant,
}

impl Rekey {
    pub fn new(title: impl Into<SharedString>) -> Self {
        Self { title: title.into(), step: "Starting...".into(), from: 0., to: 0., since: Instant::now() }
    }

    /// Sweep to `pct` from wherever the ring is now.
    pub fn set(&mut self, pct: f32, label: Option<String>) {
        self.from = self.value();
        self.to = pct.clamp(0., 100.);
        self.since = Instant::now();
        if let Some(l) = label.filter(|l| !l.is_empty()) {
            self.step = l.into();
        }
    }

    pub fn animating(&self) -> bool {
        self.since.elapsed().as_secs_f32() < SWEEP
    }

    fn value(&self) -> f32 {
        let t = (self.since.elapsed().as_secs_f32() / SWEEP).min(1.);
        self.from + (self.to - self.from) * crate::kit::bezier(0.4, 0., 0.2, 1.)(t)
    }
}

pub fn view(r: &Rekey) -> AnyElement {
    let shown = r.value();
    let amber = gpui::rgba(0xffc857d9);
    div()
        .absolute()
        .inset_0()
        .p(px(20.))
        .bg(gpui::black().opacity(0.8))
        .flex()
        .items_center()
        .justify_center()
        .occlude()
        .child(
            div()
                .w_full()
                .max_w(px(374.))
                .pt(px(28.))
                .px(px(26.))
                .pb(px(24.))
                .rounded(px(12.))
                .bg(c(0x0a0a0a))
                .border_1()
                .border_color(gpui::white().opacity(0.08))
                .flex()
                .flex_col()
                .items_center()
                .text_center()
                .child(
                    div()
                        .relative()
                        .size(px(104.))
                        .mt(px(4.))
                        .mb(px(18.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            div().absolute().inset_0().child(crate::attachments::spinner(
                                104.,
                                8.,
                                f32::MAX,
                                shown / 100.,
                                gpui::white().opacity(0.13),
                                c(ACCENT),
                            )),
                        )
                        .child(
                            div()
                                .text_size(px(21.))
                                .font_weight(FontWeight::BOLD)
                                .text_color(gpui::white())
                                .child(format!("{}%", shown.round() as u32)),
                        ),
                )
                .child(
                    div()
                        .mb(px(5.))
                        .text_size(px(16.))
                        .font_weight(FontWeight::BOLD)
                        .text_color(gpui::white())
                        .child(r.title.clone()),
                )
                .child(
                    div()
                        .min_h(px(15.6))
                        .text_size(px(13.))
                        .text_color(gpui::white().opacity(0.62))
                        .child(r.step.clone()),
                )
                .child(
                    div()
                        .mt(px(14.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .gap(px(6.))
                        .text_size(px(12.))
                        .text_color(amber)
                        .child(svg().path("icons/info.svg").size(px(14.)).flex_none().text_color(amber))
                        .child("Do not close the app during this process"),
                ),
        )
        .into_any_element()
}
