//! The My Profile account switcher (`ProfileSwitcher.svelte` + `AccountRows.svelte`): the
//! device's accounts hung from the Profile header, to switch to, remove or add.

use std::path::PathBuf;
use std::rc::Rc;

use gpui::AnimationExt;
use gpui::prelude::*;
use gpui::{AnyElement, App, FontWeight, MouseButton, SharedString, Window, div, px, svg};
use crate::pic::img;

use crate::markdown::MONO;
use crate::theme::*;
use crate::timeline::DANGER;

pub struct Row {
    pub npub: String,
    pub name: Option<String>,
    pub avatar: Option<PathBuf>,
}

pub struct Switcher {
    pub rows: Vec<Row>,
    pub active: String,
    /// Delete mode: each row offers its trash, and Add Profile steps aside.
    pub editing: bool,
    /// The device holds as many accounts as its tier allows.
    pub at_cap: bool,
}

pub enum Act {
    Close,
    Pick(String),
    Delete(String),
    Add,
}

pub type OnAct = Rc<dyn Fn(Act, &mut Window, &mut App)>;

fn on_click(
    on: &OnAct,
    act: impl Fn() -> Act + 'static,
) -> impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static {
    let on = on.clone();
    move |_, w, cx| {
        cx.stop_propagation();
        on(act(), w, cx)
    }
}

fn row(s: &Switcher, r: &Row, last: bool, on: &OnAct) -> AnyElement {
    let active = r.npub == s.active;
    let avatar = match &r.avatar {
        Some(path) => img(path.clone())
            .size(px(28.))
            .rounded_full()
            .flex_shrink_0()
            .into_any_element(),
        None => img("icons/user-placeholder.svg")
            .size(px(28.))
            .rounded_full()
            .flex_shrink_0()
            .into_any_element(),
    };
    let npub = r.npub.clone();
    div()
        .id(SharedString::from(format!("switcher-{}", r.npub)))
        .flex()
        .items_center()
        .gap(px(12.))
        .px(px(18.))
        .py(px(6.))
        .when(!last, |d| d.border_b_1().border_color(white_a(0.04)))
        .when(!active, |d| {
            d.cursor_pointer()
                .hover(|s| s.bg(white_a(0.04)))
                .on_click(on_click(on, move || Act::Pick(npub.clone())))
        })
        .child(
            div()
                .size(px(8.))
                .flex_shrink_0()
                .rounded_full()
                .when(active, |d| d.bg(c(ACCENT))),
        )
        .child(avatar)
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .gap(px(8.))
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .font_weight(FontWeight::MEDIUM)
                        .text_size(px(16.))
                        .line_height(px(24.))
                        .text_color(c(0xf7f4f4))
                        .child(r.name.clone().unwrap_or_else(|| "Unnamed".into())),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .mt(px(3.))
                        .truncate()
                        .font_family(MONO)
                        .text_size(px(11.))
                        .line_height(px(24.))
                        .text_color(white_a(0.4))
                        .child(r.npub.clone()),
                ),
        )
        .when(s.editing, |d| {
            let npub = r.npub.clone();
            d.child(
                div()
                    .id(SharedString::from(format!("switcher-trash-{}", r.npub)))
                    .p(px(6.))
                    .flex_shrink_0()
                    .cursor_pointer()
                    .group("row-trash")
                    .on_click(on_click(on, move || Act::Delete(npub.clone())))
                    .child(
                        svg()
                            .path("ui/switcher-trash.svg")
                            .size(px(16.))
                            .text_color(c(DANGER))
                            .group_hover("row-trash", |s| s.size(px(20.)).m(px(-2.))),
                    ),
            )
        })
        .into_any_element()
}

/// The backdrop and the panel, over the column at `left` below a header ending at `top`.
/// `viewport` is (width, height); `hang` drops it from the slim header's avatar.
pub fn view(s: &Switcher, top: f32, left: f32, viewport: (f32, f32), hang: bool, on: &OnAct) -> AnyElement {
    let close = {
        let on = on.clone();
        move |_: &gpui::MouseDownEvent, w: &mut Window, cx: &mut App| on(Act::Close, w, cx)
    };
    let count = s.rows.len();
    let add = (!s.editing).then(|| {
        div()
            .id("profile-switcher-add")
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_center()
            .gap(px(8.))
            .p(px(10.))
            .border_t_1()
            .border_color(white_a(0.06))
            .font_weight(FontWeight::MEDIUM)
            .text_color(if s.at_cap { white_a(0.35) } else { c(ACCENT) })
            .when(!s.at_cap, |d| {
                d.bg(white_a(0.02))
                    .cursor_pointer()
                    .hover(|s| s.bg(white_a(0.04)))
                    .on_click(on_click(on, || Act::Add))
            })
            .child(
                svg()
                    .path("ui/switcher-add.svg")
                    .size(px(24.))
                    .text_color(if s.at_cap { white_a(0.35) } else { c(ACCENT) }),
            )
            .child(if s.at_cap {
                "Maximum Accounts"
            } else {
                "Add Profile"
            })
    });
    let panel = div()
        .id("profile-switcher-panel")
        .occlude()
        .flex()
        .flex_col()
        .overflow_hidden()
        .border_color(white_a(0.08))
        .shadow(vec![gpui::BoxShadow {
            color: gpui::black().opacity(0.35),
            offset: gpui::point(px(0.), px(12.)),
            blur_radius: px(24.),
            spread_radius: px(0.),
            inset: false,
        }])
        .child(
            div()
                .id("profile-switcher-list")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .children(s.rows.iter().enumerate().map(|(i, r)| row(s, r, i + 1 == count, on))),
        )
        .children(add);
    let panel = if hang {
        // `.cc-dropdown`: a menu hanging from the avatar, not a takeover of the list.
        panel
            .absolute()
            .top(px(top))
            .left(px(left))
            .w(px(320_f32.min(viewport.0 - 16.)))
            .max_h(px((viewport.1 * 0.6).min(440.)))
            .bg(c(0x171717))
            .border_1()
            .rounded(px(12.))
    } else {
        panel
            .w_full()
            .max_w(px(420.))
            .max_h(px(viewport.1 - 80.))
            .bg(c(0x161616))
            .border_l_1()
            .border_r_1()
            .border_b_1()
            .rounded_b(px(12.))
    }
    .with_animation(
        "profile-switcher-in",
        gpui::Animation::new(std::time::Duration::from_millis(140)).with_easing(gpui::ease_out_quint()),
        |d, t| d.opacity(t).mt(px(-6. * (1. - t))),
    );
    let backdrop = div().absolute().left(px(left)).right_0().bottom_0().on_mouse_down(MouseButton::Left, close);
    div()
        .absolute()
        .inset_0()
        .child(if hang { backdrop.top_0().left_0() } else { backdrop.top(px(top)).bg(gpui::black().opacity(0.45)) })
        .child(if hang {
            panel.into_any_element()
        } else {
            div().absolute().top(px(top)).left(px(left)).right_0().flex().justify_center().child(panel).into_any_element()
        })
        .into_any_element()
}
