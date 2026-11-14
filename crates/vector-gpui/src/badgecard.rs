//! The badge card (`ui/BadgeCard.svelte`): a badge, what earned it and what it grants, tilting
//! under the pointer. Depth is parallax: each layer drifts by its height above the card.

use gpui::prelude::*;
use gpui::{
    AnyElement, App, FontWeight, HighlightStyle, MouseButton, Pixels, Point, SharedString,
    StyledText, Window, div, linear_color_stop, linear_gradient, point, px, svg,
};

use crate::backend::badges::Badges;
use crate::profile::Badge;
use crate::theme::*;

#[derive(Clone, Copy)]
pub enum Run {
    Plain,
    Bold,
    /// The faint italic aside under a description.
    Aside,
}

pub struct Card {
    pub svg: SharedString,
    pub title: &'static str,
    pub subtitle: Option<String>,
    /// Paragraphs of runs, each with the space below it.
    pub desc: Vec<(Vec<(String, Run)>, f32)>,
    /// Bug Hunter's tier rail: the held tier of three.
    pub tier: Option<u8>,
    pub access: &'static str,
    /// The pointer over the card, from its centre, each axis in -1..1.
    pub tilt: Point<f32>,
    pub opened: std::time::Instant,
}

impl Card {
    pub fn of(which: Badge, b: Badges) -> Self {
        let p = |s: &str| (s.to_string(), Run::Plain);
        let (svg, title, subtitle, desc, tier, access): (
            String,
            _,
            _,
            Vec<(Vec<(String, Run)>, f32)>,
            _,
            _,
        ) = match which {
            Badge::Inviter => (
                "icons/vector_badge_placeholder.svg".into(),
                "Vector Beta Inviter",
                None,
                vec![(
                    vec![
                        p("Acquired by inviting "),
                        (
                            format!(
                                "{} {}",
                                b.invites,
                                if b.invites == 1 { "user" } else { "users" }
                            ),
                            Run::Bold,
                        ),
                        p(" to the Vector Beta!"),
                    ],
                    0.,
                )],
                None,
                "",
            ),
            Badge::Fawkes => (
                "icons/fawkes_mask.svg".into(),
                "V for Vector Badge",
                None,
                vec![
                    (
                        vec![p(
                            "Acquired by logging in on Guy Fawkes Day\u{a0}(November 5, 2025).",
                        )],
                        21.,
                    ),
                    (
                        vec![(
                            "Remember, remember the 5th of November...".into(),
                            Run::Aside,
                        )],
                        0.,
                    ),
                ],
                None,
                "Full Premium Access",
            ),
            Badge::BugHunter => (
                format!("icons/bughunter_{}.svg", b.bug.clamp(1, 3)),
                "Bug Hunter",
                Some(format!("Tier {}", b.bug)),
                vec![(
                    vec![p(
                        "Bug Hunter badges are one of the most prestigious awards to true contributors of Vector who have identified and reported bugs or issues.",
                    )],
                    0.,
                )],
                Some(b.bug),
                if b.bug >= 3 {
                    "Full Premium Access"
                } else {
                    "Partial Premium Access"
                },
            ),
            Badge::V2026 => (
                "icons/vforvector2026.svg".into(),
                "V for Vector Badge",
                Some("2026 Edition".into()),
                vec![
                    (
                        vec![p(
                            "Remember, Remember... the\u{a0}Fifth\u{a0}of\u{a0}November.",
                        )],
                        10.,
                    ),
                    (
                        vec![p(
                            "\u{201c}People should not be afraid of their governments. Governments should be afraid of their people.\u{201d}",
                        )],
                        0.,
                    ),
                ],
                None,
                "Partial Premium Access",
            ),
        };
        Self {
            svg: svg.into(),
            title,
            subtitle,
            desc,
            tier,
            access,
            tilt: Point::default(),
            opened: std::time::Instant::now(),
        }
    }
}

fn paragraph(runs: &[(String, Run)]) -> AnyElement {
    let mut text = String::new();
    let mut highlights = Vec::new();
    let mut aside = false;
    for (t, kind) in runs {
        let start = text.len();
        text.push_str(t);
        match kind {
            Run::Plain => {}
            Run::Bold => highlights.push((
                start..text.len(),
                HighlightStyle {
                    font_weight: Some(FontWeight::BOLD),
                    color: Some(c(0xf6f6f6)),
                    ..Default::default()
                },
            )),
            Run::Aside => aside = true,
        }
    }
    let line = div().w_full().text_center();
    if aside {
        return line
            .text_size(px(13.))
            .italic()
            .opacity(0.5)
            .child(text)
            .into_any_element();
    }
    line.child(StyledText::new(text).with_highlights(highlights))
        .into_any_element()
}

pub enum Ev {
    Close,
    Pointer(Point<Pixels>),
    Leave,
}

pub fn view(
    card: &Card,
    window: &Window,
    cx: &App,
    on: impl Fn(Ev, &mut Window, &mut App) + Clone + 'static,
) -> AnyElement {
    // Layer heights above the card (`translateZ`), as drift per unit of tilt.
    let drift = |z: f32| point(px(card.tilt.x * z * 0.12), px(card.tilt.y * z * 0.12));
    let shift = |d: gpui::Div, z: f32| {
        let o = drift(z);
        d.relative().left(o.x).top(o.y)
    };
    let t = (card.opened.elapsed().as_secs_f32() / 0.42).min(1.);
    let ease = 1. - (1. - t).powi(3);
    let tiers = card.tier.map(|held| {
        let mut row = div()
            .mt(px(18.))
            .flex()
            .items_center()
            .justify_center()
            .gap(px(8.));
        for n in 1..=3u8 {
            let filled = n <= held;
            row = row.child(
                div()
                    .size(px(40.))
                    .flex_shrink_0()
                    .when(!filled, |d| d.opacity(0.35))
                    .child(
                        crate::kit::sharp_svg(format!("icons/bughunter_{n}.svg"), 40., window, cx)
                            .grayscale(!filled),
                    ),
            );
            if n < 3 {
                row = row.child(
                    div()
                        .w(px(20.))
                        .h(px(3.))
                        .rounded(px(2.))
                        .flex_shrink_0()
                        .bg(if held >= n + 1 {
                            c(ACCENT)
                        } else {
                            white_a(0.15)
                        }),
                );
            }
        }
        shift(row, 12.)
    });
    let access = (!card.access.is_empty()).then(|| {
        shift(
            div()
                .mt(px(20.))
                .flex()
                .items_center()
                .justify_center()
                .gap(px(7.))
                .text_color(c(ACCENT))
                .text_size(px(15.))
                .line_height(px(16.))
                .font_weight(FontWeight::MEDIUM)
                .child(
                    svg()
                        .path("ui/award.svg")
                        .size(px(16.))
                        .flex_shrink_0()
                        .text_color(c(ACCENT)),
                )
                .child(card.access),
            14.,
        )
    });
    let glare = {
        // .badge-card-glare: a soft light where the pointer catches the card.
        let strength = (card.tilt.x.abs() + card.tilt.y.abs()).min(1.);
        div()
            .absolute()
            .left(gpui::relative(0.5 + card.tilt.x * 0.5))
            .top(gpui::relative(0.5 + card.tilt.y * 0.5))
            .size(px(1.))
            .rounded_full()
            .shadow(vec![gpui::BoxShadow {
                color: gpui::white().opacity(0.07 * strength),
                offset: point(px(0.), px(0.)),
                blur_radius: px(160.),
                spread_radius: px(60.),
                inset: false,
            }])
    };
    let close = on.clone();
    let track = on.clone();
    let leave = on;
    div()
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .bg(gpui::black().opacity(0.72 * ease))
        .occlude()
        .on_mouse_down(MouseButton::Left, move |_, w, cx| close(Ev::Close, w, cx))
        .child(
            div()
                .id("badge-card")
                .relative()
                .w(px(358.))
                .pt(px(36.))
                .px(px(28.))
                .pb(px(28.))
                .rounded(px(22.))
                .overflow_hidden()
                .border_1()
                .border_color(c(0x393b3c))
                .bg(linear_gradient(
                    160.,
                    linear_color_stop(c(0x171717), 0.),
                    linear_color_stop(c(0x16181a), 1.),
                ))
                .shadow(vec![gpui::BoxShadow {
                    color: gpui::black().opacity(0.6),
                    offset: point(px(0.), px(24.)),
                    blur_radius: px(70.),
                    spread_radius: px(0.),
                    inset: false,
                }])
                .opacity(ease)
                .top(px(10. * (1. - ease)))
                .font_family(INTER)
                .text_size(px(14.))
                .line_height(px(21.))
                .text_color(c(0xb2b2b2))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_mouse_move(move |e, w, cx| track(Ev::Pointer(e.position), w, cx))
                .on_hover(move |hovered, w, cx| {
                    if !*hovered {
                        leave(Ev::Leave, w, cx)
                    }
                })
                .child(glare)
                .child(
                    shift(div().flex().justify_center().mb(px(18.)), 46.)
                        .child(crate::kit::sharp_svg(card.svg.clone(), 172., window, cx)),
                )
                .child(
                    shift(
                        div()
                            .w_full()
                            .text_center()
                            .font_weight(FontWeight::BOLD)
                            .text_size(px(20.))
                            .line_height(px(24.))
                            .text_color(c(0xffffff))
                            .mb(px(10.)),
                        26.,
                    )
                    .child(card.title),
                )
                .children(card.subtitle.clone().map(|s| {
                    shift(
                        div()
                            .w_full()
                            .mt(px(-4.))
                            .mb(px(12.))
                            .flex()
                            .justify_center()
                            .text_size(px(13.))
                            .line_height(px(24.))
                            .text_color(c(0x8a8a8a)),
                        22.,
                    )
                    .child(crate::kit::spaced(&s, 0.78))
                }))
                .child(
                    shift(div().w_full().flex().flex_col(), 14.).children(
                        card.desc
                            .iter()
                            .map(|(runs, gap)| div().mb(px(*gap)).child(paragraph(runs))),
                    ),
                )
                .children(tiers)
                .children(access),
        )
        .into_any_element()
}
