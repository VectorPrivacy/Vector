//! `ProfileScreen`: the expanded profile, ours or a contact's. Header, banner and avatar,
//! names and status, the contact options, description and npub.

use std::rc::Rc;

use gpui::prelude::*;
use gpui::{
    AnyElement, App, FontWeight, Hsla, MouseButton, ObjectFit, SharedString, Window, div, img, px,
    svg,
};

use crate::data::Store;
use crate::theme::*;
use crate::timeline::DANGER;

/// What the screen asks the app to do.
#[derive(Clone, Copy, PartialEq)]
pub enum Act {
    Back,
    Message,
    ToggleMute,
    Share,
    CopyNpub,
    Qr,
    More,
    Nickname,
    Block,
    Edit,
    SetStatus,
    Switcher,
    /// The open switcher's delete mode.
    SwitcherEdit,
    /// Leave Edit Mode: save, or (true) cancel.
    ExitEdit(bool),
    RemoveBanner,
    /// Pick a new picture: the banner, or (false) the avatar.
    Pick(bool),
    /// Open a badge's card.
    Badge(Badge),
}

#[derive(Clone, Copy, PartialEq)]
pub enum Badge {
    Inviter,
    Fawkes,
    BugHunter,
    V2026,
}

pub type OnAct = Rc<dyn Fn(Act, &mut Window, &mut App)>;

/// The open profile and its transient UI.
pub struct Screen {
    pub npub: String,
    /// The chat to return to.
    pub back: Option<SharedString>,
    pub more_open: bool,
    /// Copy feedback: (which, when).
    pub copied: Option<(Act, std::time::Instant)>,
    pub edit: Option<Edit>,
    /// The account switcher hung from this header: open, and whether in delete mode.
    pub switcher: Option<bool>,
    pub badges: crate::backend::badges::Badges,
}

/// Edit Mode's draft of our own profile.
pub struct Edit {
    pub name: gpui::Entity<crate::composer::TextInput>,
    pub bio: gpui::Entity<crate::composer::TextInput>,
    pub removed_banner: bool,
    pub snap_name: String,
    pub snap_about: String,
    pub avatar: Option<Pick>,
    pub banner: Option<Pick>,
}

/// A picture picked in Edit Mode, shown in place until it uploads on save.
pub struct Pick {
    pub path: std::path::PathBuf,
    pub crop: Option<vector_core::image_crop::CropRect>,
    pub preview: std::sync::Arc<gpui::RenderImage>,
}

impl Edit {
    pub fn dirty(&self, cx: &App) -> bool {
        self.removed_banner
            || self.avatar.is_some()
            || self.banner.is_some()
            || self.name.read(cx).text().trim() != self.snap_name
            || self.bio.read(cx).text().trim() != self.snap_about
    }
}

impl Screen {
    pub fn new(npub: String, back: Option<SharedString>) -> Self {
        Self {
            npub,
            back,
            more_open: false,
            copied: None,
            edit: None,
            switcher: None,
            badges: Default::default(),
        }
    }

    fn copied(&self, which: Act) -> bool {
        self.copied
            .is_some_and(|(w, t)| w == which && t.elapsed().as_millis() < 2000)
    }
}

/// `#profile .profile-header`'s fill over the app background: rgba(22,22,22,.85).
const HEADER_BG: u32 = 0x131313;
const OPTION_BORDER: u32 = 0x393b3c;
const OPTION_TEXT: u32 = 0xb2b2b2;

fn click(on: &OnAct, a: Act) -> impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static {
    let on = on.clone();
    move |_, w, cx| {
        cx.stop_propagation();
        on(a, w, cx)
    }
}

/// `.profile-option`: a 74×70 tile, icon over label.
fn option(
    id: &'static str,
    icon: &'static str,
    icon_size: f32,
    tint: u32,
    label: &'static str,
    active: bool,
) -> gpui::Stateful<gpui::Div> {
    let inset = (30. - icon_size) / 2.;
    div()
        .id(id)
        .relative()
        .mx(px(6.))
        .w(px(74.))
        .h(px(70.))
        .flex_shrink_0()
        .rounded(px(12.))
        .border_2()
        .border_color(c(if active { 0x959899 } else { OPTION_BORDER }))
        .bg(c(if active { 0x171717 } else { APP_BG }))
        .text_color(c(if active { 0xffffff } else { OPTION_TEXT }))
        .cursor_pointer()
        .hover(|s| {
            s.bg(c(0x171717))
                .border_color(c(0x959899))
                .text_color(c(0xffffff))
        })
        .flex()
        .flex_col()
        .items_center()
        .pt(px(8. + inset))
        .child(
            svg()
                .path(icon)
                .size(px(icon_size))
                .text_color(c(tint))
                .flex_shrink_0(),
        )
        .child(
            div()
                .mt(px(-2. + inset))
                .w(px(50.))
                .text_size(px(12.))
                .line_height(px(24.))
                .text_center()
                .child(label),
        )
}

/// `.profile-share-btn`: an option tile at three quarters size, pinned to the banner.
fn small_option(
    id: &'static str,
    icon: &'static str,
    label: Option<&'static str>,
    edit: bool,
) -> gpui::Stateful<gpui::Div> {
    let d = div()
        .id(id)
        .absolute()
        .top(px(10.))
        .rounded(px(9.))
        .border(px(1.5))
        .border_color(c(OPTION_BORDER))
        .bg(c(APP_BG))
        .text_color(c(OPTION_TEXT))
        .cursor_pointer()
        .hover(|s| {
            s.bg(c(0x171717))
                .border_color(c(0x959899))
                .text_color(c(0xffffff))
        })
        .flex()
        .flex_col()
        .items_center()
        .w(px(55.5));
    match label {
        Some(label) => d
            .h(px(52.5))
            .pt(px(if edit { 8.3 } else { 6. }))
            .child(
                svg()
                    .path(icon)
                    .size(px(22.5))
                    .text_color(c(PRIMARY))
                    .flex_shrink_0(),
            )
            .child(
                div()
                    .mt(px(if edit { -1.5 } else { -1.5 }))
                    .text_size(px(9.))
                    .line_height(px(18.))
                    .child(label),
            ),
        None => d
            .h(px(55.5))
            .pt(px(7.5))
            .child(svg().path(icon).size(px(37.5)).text_color(c(0xffffff))),
    }
}

pub fn pane(store: &Store, s: &Screen, on: &OnAct, window: &Window, cx: &App) -> AnyElement {
    let Some(p) = store.profiles.get(&s.npub) else {
        return div().flex_1().h_full().bg(c(APP_BG)).into_any_element();
    };
    let mine = p.mine;
    let name = store.name(&s.npub);
    let status = p.status.title.clone();
    let status_text = if status.is_empty() && mine {
        "Set a Status".to_string()
    } else {
        status.clone()
    };
    let secondary = [&p.nickname, &p.name, &p.display_name]
        .into_iter()
        .find(|n| !n.is_empty())
        .cloned()
        .unwrap_or_else(|| {
            if mine {
                "Anonymous".into()
            } else {
                format!("{}…", &s.npub[..s.npub.len().min(10)])
            }
        });
    let muted = store.chat(&s.npub).is_some_and(|c| c.muted);
    let editing = s.edit.as_ref().filter(|_| mine);
    let banner = (!p.banner_cached.is_empty()
        && !store.pictures_held(&p.id)
        && !editing.is_some_and(|e| e.removed_banner))
    .then(|| store.media_root.join(&p.banner_cached));
    let dark = mine || !p.banner.is_empty();
    let banner_h = if dark { 200. } else { 115. };

    // ── header ──
    let header = {
        let info = if mine {
            div()
                .id("my-profile-switcher")
                .mt(px(5.))
                .px(px(8.))
                .py(px(4.))
                .rounded(px(6.))
                .flex()
                .items_center()
                .gap(px(6.))
                .cursor_pointer()
                .hover(|s| s.bg(white_a(0.05)))
                .on_click(click(on, Act::Switcher))
                .child(
                    div()
                        .font_weight(FontWeight::BOLD)
                        .text_size(px(18.))
                        .line_height(px(24.))
                        .text_color(c(0xf7f4f4))
                        .child("My Profile"),
                )
                .child(
                    svg()
                        .path("ui/switcher-chevron.svg")
                        .size(px(14.))
                        .text_color(white_a(0.7))
                        .when(s.switcher.is_some(), |i| {
                            i.with_transformation(gpui::Transformation::rotate(gpui::radians(
                                std::f32::consts::PI,
                            )))
                        }),
                )
                .into_any_element()
        } else {
            div()
                .max_w(gpui::relative(0.6))
                .min_w_0()
                .flex()
                .flex_col()
                .items_center()
                .child(
                    div()
                        .mt(px(5.))
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .min_w_0()
                        .child(crate::ui::avatar(store, &s.npub, 22.))
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .font_family(RUBIK)
                                .font_weight(FontWeight::BOLD)
                                .text_size(px(18.72))
                                .line_height(px(24.))
                                .child(name.clone()),
                        ),
                )
                .when(!status.is_empty(), |d| {
                    d.child(div().w_full().flex().justify_center().opacity(0.8).child(
                        crate::inline::emoji_line(
                            &status,
                            crate::inline::Look::new(INTER, 14., 21., c(TEXT)),
                            15.,
                            1.,
                        ),
                    ))
                })
                .into_any_element()
        };
        div()
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .h(px(60.))
            .bg(c(HEADER_BG))
            .flex()
            .items_center()
            .justify_center()
            .when(!mine, |d| {
                d.child(
                    div()
                        .id("profile-back")
                        .absolute()
                        .top(px(10.))
                        .left(px(5.))
                        .p(px(5.))
                        .cursor_pointer()
                        .on_click(click(on, Act::Back))
                        .child(
                            svg()
                                .path("icons/chevron-double-left.svg")
                                .size(px(30.))
                                .text_color(c(ACCENT)),
                        ),
                )
            })
            .when_some(s.switcher, |d, deleting| {
                // .profile-switcher-trash-toggle
                d.child(
                    div()
                        .id("profile-switcher-trash-toggle")
                        .absolute()
                        .left(px(10.))
                        .top(px(13.))
                        .p(px(5.))
                        .rounded(px(4.))
                        .cursor_pointer()
                        .group("switcher-trash")
                        .on_click(click(on, Act::SwitcherEdit))
                        .child(
                            svg()
                                .path("ui/switcher-trash.svg")
                                .size(px(24.))
                                .text_color(if deleting { c(DANGER) } else { white_a(0.45) })
                                .when(!deleting, |i| {
                                    i.group_hover("switcher-trash", |s| s.text_color(white_a(0.75)))
                                }),
                        ),
                )
            })
            .child(info)
            .when_some(editing, |d, e| d.child(edit_bar(e.dirty(cx), on)))
    };

    // ── banner and avatar ──
    let mut banner_box = div()
        .relative()
        .w_full()
        .h(px(banner_h))
        .flex_shrink_0()
        .bg(c(if dark { 0x1b1b1b } else { 0x0a0a0a }));
    if let Some(pick) = editing.and_then(|e| e.banner.as_ref()) {
        banner_box = banner_box.child(
            img(pick.preview.clone())
                .absolute()
                .inset_0()
                .size_full()
                .object_fit(ObjectFit::Cover),
        );
    } else if let Some(path) = banner {
        banner_box = banner_box.child(
            img(path)
                .absolute()
                .inset_0()
                .size_full()
                .object_fit(ObjectFit::Cover),
        );
    }
    if editing.is_some() {
        banner_box = banner_box.child(
            div()
                .id("profile-banner")
                .absolute()
                .inset_0()
                .cursor_pointer()
                .hover(|s| s.bg(c(APP_BG).opacity(0.25)))
                .on_click(click(on, Act::Pick(true))),
        );
    }
    if mine && editing.is_none() {
        banner_box = banner_box
            .child(
                small_option("profile-edit-btn", "icons/edit.svg", Some("Edit"), true)
                    .left(px(18.5))
                    .on_click(click(on, Act::Edit)),
            )
            .child(
                small_option(
                    "profile-share-btn",
                    if s.copied(Act::Share) {
                        "icons/check.svg"
                    } else {
                        "icons/share.svg"
                    },
                    Some("Share"),
                    false,
                )
                .right(px(16.))
                .on_click(click(on, Act::Share)),
            );
    }
    if editing.is_none() {
        banner_box = banner_box.child(
            small_option("profile-qr-btn", "ui/qr.svg", None, false)
                .right(px(16.))
                .top(px(if mine { 68. } else { 10. }))
                .on_click(click(on, Act::Qr)),
        );
    }

    let picked = editing
        .and_then(|e| e.avatar.as_ref())
        .map(|p| p.preview.clone());
    let big_avatar = match store.avatar_path(&s.npub) {
        _ if picked.is_some() => img(picked.unwrap())
            .size(px(175.))
            .rounded_full()
            .object_fit(ObjectFit::Cover)
            .into_any_element(),
        Some(path) => img(path)
            .size(px(175.))
            .rounded_full()
            .object_fit(ObjectFit::Cover)
            .into_any_element(),
        None => img("icons/user-placeholder.svg")
            .size(px(175.))
            .rounded_full()
            .into_any_element(),
    };
    let avatar = div()
        .id("profile-avatar")
        .mt(px(-87.))
        .size(px(191.))
        .flex_shrink_0()
        .rounded_full()
        .bg(c(APP_BG))
        .flex()
        .items_center()
        .justify_center()
        .child(
            div()
                .when(editing.is_some(), |d| d.hover(|s| s.opacity(0.75)))
                .child(big_avatar),
        )
        .when(editing.is_some(), |d| {
            d.cursor_pointer().on_click(click(on, Act::Pick(false)))
        });

    if let Some(e) = editing {
        let body = div()
            .w_full()
            .flex()
            .flex_col()
            .items_center()
            .child(banner_box)
            .child(avatar)
            .child(edit_fields(e, p.banner.trim().is_empty(), on));
        return frame(body, header, true);
    }

    // ── names, status, options ──
    let mut body = div()
        .w_full()
        .flex()
        .flex_col()
        .items_center()
        .child(banner_box)
        .child(avatar)
        .child(
            div()
                .mt(px(10.))
                .w(gpui::relative(0.6))
                .text_center()
                .font_weight(FontWeight::BOLD)
                .text_size(px(18.72))
                .line_height(px(24.))
                .flex()
                .justify_center()
                .gap(px(4.))
                .child(secondary)
                .when(p.bot, |d| {
                    d.child(
                        svg()
                            .path("icons/bot.svg")
                            .size(px(18.))
                            .mt(px(3.))
                            .text_color(c(PRIMARY)),
                    )
                }),
        );
    let status_line = div()
        .id("profile-secondary-status")
        .mt(px(5.))
        .w(gpui::relative(0.9))
        .text_center()
        .text_size(px(14.))
        .line_height(px(21.))
        .opacity(0.8)
        .when(mine, |d| {
            d.cursor_pointer().on_click(click(on, Act::SetStatus))
        })
        .flex()
        .justify_center()
        .when(status_text.is_empty(), |d| d.h_0())
        .child(crate::inline::emoji_line(
            &status_text,
            crate::inline::Look::new(INTER, 14., 21., c(TEXT)),
            15.,
            1.,
        ));
    body = body.child(status_line);
    // #profile-badges: empty, its margin collapses into the options'.
    if s.badges.invites > 0 || s.badges.fawkes || s.badges.bug > 0 || s.badges.v2026 {
        let b = s.badges;
        let badge = |id: &'static str, src: SharedString, first: bool, which: Badge| {
            div()
                .id(id)
                .size(px(30.))
                .when(!first, |d| d.ml(px(5.)))
                .cursor_pointer()
                .on_click(click(on, Act::Badge(which)))
                .child(crate::kit::sharp_svg(src, 30., window, cx))
        };
        body = body.child(
            div()
                .mt(px(5.))
                .flex()
                .when(b.invites > 0, |d| {
                    d.child(badge(
                        "badge-inviter",
                        "icons/vector_badge_hex_placeholder.svg".into(),
                        true,
                        Badge::Inviter,
                    ))
                })
                .when(b.fawkes, |d| {
                    d.child(badge(
                        "badge-fawkes",
                        "icons/fawkes_mask.svg".into(),
                        false,
                        Badge::Fawkes,
                    ))
                })
                .when(b.bug > 0, |d| {
                    d.child(badge(
                        "badge-bug",
                        format!("icons/bughunter_{}.svg", b.bug).into(),
                        false,
                        Badge::BugHunter,
                    ))
                })
                .when(b.v2026, |d| {
                    d.child(badge(
                        "badge-v2026",
                        "icons/vforvector2026.svg".into(),
                        false,
                        Badge::V2026,
                    ))
                }),
        );
    }

    if !mine {
        let more = {
            let mut m = option(
                "profile-option-more",
                "icons/dots-horizontal.svg",
                33.,
                0xffffff,
                "More",
                s.more_open,
            )
            .on_click(click(on, Act::More));
            if s.more_open {
                let item = |id: &'static str,
                            label: &'static str,
                            icon: &'static str,
                            danger: bool,
                            a: Act| {
                    let tint = if danger { c(DANGER) } else { c(OPTION_TEXT) };
                    div()
                        .id(id)
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap(px(10.))
                        .px(px(14.))
                        .py(px(10.))
                        .text_size(px(14.))
                        .line_height(px(24.))
                        .text_color(tint)
                        .whitespace_nowrap()
                        .cursor_pointer()
                        .when(danger, |d| d.hover(|s| s.bg(ca(255, 46, 169, 0.1))))
                        .when(!danger, |d| {
                            d.hover(|s| s.bg(c(0x2a2a2a)).text_color(c(0xffffff)))
                        })
                        .on_click(click(on, a))
                        .child(label)
                        .child(svg().path(icon).size(px(18.)).text_color(if danger {
                            c(DANGER)
                        } else {
                            c(0xffffff)
                        }))
                };
                let close = on.clone();
                m = m.child(
                    gpui::deferred(
                        div()
                            .id("profile-more-dropdown")
                            .absolute()
                            .bottom(px(78.))
                            .right(px(-2.))
                            .min_w(px(150.))
                            .rounded(px(10.))
                            .border_1()
                            .border_color(c(OPTION_BORDER))
                            .bg(c(0x1e1e1e))
                            .overflow_hidden()
                            .shadow(vec![gpui::BoxShadow {
                                color: ca(0, 0, 0, 0.4),
                                offset: gpui::point(px(0.), px(4.)),
                                blur_radius: px(16.),
                                spread_radius: px(0.),
                                inset: false,
                            }])
                            .on_mouse_down_out(move |_, w, cx| close(Act::More, w, cx))
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .child(item(
                                "profile-more-nickname",
                                "Nickname",
                                "icons/edit.svg",
                                false,
                                Act::Nickname,
                            ))
                            .child(item(
                                "profile-more-block",
                                if p.is_blocked { "Unblock" } else { "Block" },
                                "icons/x-user.svg",
                                true,
                                Act::Block,
                            )),
                    )
                    .with_priority(1),
                );
            }
            m
        };
        body = body.child(
            div()
                .mt(px(15.))
                .h(px(50.))
                .w_full()
                .flex()
                .justify_center()
                .child(
                    option(
                        "profile-option-message",
                        "icons/message.svg",
                        30.,
                        0xffffff,
                        "Message",
                        false,
                    )
                    .on_click(click(on, Act::Message)),
                )
                .child(
                    option(
                        "profile-option-mute",
                        if muted {
                            "icons/volume-mute.svg"
                        } else {
                            "icons/volume-max.svg"
                        },
                        30.,
                        0xffffff,
                        if muted { "Unmute" } else { "Mute" },
                        false,
                    )
                    .on_click(click(on, Act::ToggleMute)),
                )
                .child(
                    option(
                        "profile-option-share",
                        if s.copied(Act::Share) {
                            "icons/check.svg"
                        } else {
                            "icons/share.svg"
                        },
                        30.,
                        PRIMARY,
                        "Share",
                        false,
                    )
                    .on_click(click(on, Act::Share)),
                )
                .child(more),
        );
    }

    let about = p.about.trim_end().to_string();
    let placeholder = about.is_empty();
    body = body
        .child(
            div()
                .mt(px(if mine { 10. } else { 50. }))
                .w(gpui::relative(0.9))
                .text_center()
                .text_size(px(14.))
                .line_height(px(24.))
                .opacity(if placeholder { 0.5 } else { 0.8 })
                .child(if placeholder {
                    "No description yet".to_string()
                } else {
                    about
                }),
        )
        .child(
            div()
                .mt(px(40.))
                .font_weight(FontWeight::BOLD)
                .text_size(px(18.72))
                .line_height(px(24.))
                .text_color(c(0xf7f4f4))
                .child(if mine { "My nPub Key" } else { "nPub Key" }),
        )
        .child(
            div().mt(px(5.)).mb(px(40.)).w(gpui::relative(0.85)).child(
                div()
                    .mx(px(-6.))
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .pl(px(8.))
                    .pr(px(4.))
                    .pb(px(4.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_center()
                            .whitespace_nowrap()
                            .text_size(px(14.))
                            .line_height(px(24.))
                            .opacity(0.9)
                            .child(format!(
                                "{}...{}",
                                &s.npub[..16.min(s.npub.len())],
                                &s.npub[s.npub.len().saturating_sub(16)..]
                            )),
                    )
                    .child(
                        div()
                            .id("profile-npub-copy")
                            .size(px(28.))
                            .flex_shrink_0()
                            .rounded(px(8.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .on_click(click(on, Act::CopyNpub))
                            .child(
                                svg()
                                    .path(if s.copied(Act::CopyNpub) {
                                        "icons/check.svg"
                                    } else {
                                        "icons/copy.svg"
                                    })
                                    .size(px(20.))
                                    .text_color(c(PRIMARY)),
                            ),
                    ),
            ),
        );

    frame(body, header, mine)
}

fn frame(body: gpui::Div, header: gpui::Div, mine: bool) -> AnyElement {
    div()
        .flex_1()
        .min_w_0()
        .h_full()
        .relative()
        .bg(c(APP_BG))
        .child(
            div()
                .id("profile-content")
                .absolute()
                .top(px(60.))
                .left_0()
                .right_0()
                .bottom(px(if mine { 0. } else { 6. }))
                .overflow_y_scroll()
                .child(body),
        )
        .child(header)
        .into_any_element()
}

/// `#profile-edit-bar`: Exit, the state of the draft, Save.
fn edit_bar(dirty: bool, on: &OnAct) -> gpui::Div {
    let side = |id: &'static str,
                icon: &'static str,
                size: f32,
                tint: Hsla,
                label: &'static str,
                a: Act| {
        div()
            .id(id)
            .flex()
            .items_center()
            .gap(px(6.))
            .cursor_pointer()
            .hover(|s| s.opacity(0.75))
            .on_click(click(on, a))
            .child(svg().path(icon).size(px(size)).text_color(tint))
            .child(div().text_size(px(16.)).line_height(px(24.)).child(label))
    };
    div()
        .absolute()
        .inset_0()
        .px(px(20.))
        .flex()
        .items_center()
        .justify_between()
        .bg(c(0x151515))
        .child(side(
            "profile-edit-cancel-btn",
            "icons/edit-x.svg",
            14.,
            c(DANGER),
            "Exit",
            Act::ExitEdit(true),
        ))
        .child(
            div()
                .absolute()
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .text_size(px(12.))
                .line_height(px(24.))
                .text_color(white_a(0.6))
                .opacity(0.8)
                .child(if dirty {
                    "Unsaved Changes Made"
                } else {
                    "Edit Mode is Enabled"
                }),
        )
        .child(side(
            "profile-edit-save-btn",
            "icons/save.svg",
            16.,
            c(PRIMARY),
            "Save",
            Act::ExitEdit(false),
        ))
}

/// `ProfileEditFields`: Remove Banner, Username and Bio.
fn edit_fields(e: &Edit, no_banner: bool, on: &OnAct) -> gpui::Div {
    let label = |text: &'static str| {
        div()
            .w_full()
            .text_center()
            .text_size(px(11.))
            .line_height(px(24.))
            .text_color(white_a(0.4))
            .child(text)
    };
    let wrapper = || {
        div()
            .w_full()
            .mt(px(8.))
            .px(px(14.))
            .py(px(6.))
            .rounded(px(6.))
            .border_1()
            .border_color(white_a(0.12))
            .bg(white_a(0.05))
    };
    div()
        .mt(px(16.))
        .w(gpui::relative(0.9))
        .flex()
        .flex_col()
        .when(!no_banner && !e.removed_banner, |d| {
            d.child(
                div()
                    .id("profile-remove-banner")
                    .self_center()
                    .mb(px(12.))
                    .px(px(6.))
                    .py(px(2.))
                    .text_size(px(12.5))
                    .text_color(c(0x8a8a8a))
                    .hover(|s| s.text_color(c(DANGER)))
                    .cursor_pointer()
                    .on_click(click(on, Act::RemoveBanner))
                    .child("Remove Banner"),
            )
        })
        .child(label("USERNAME"))
        .child(wrapper().child(div().h(px(24.)).flex().items_center().child(e.name.clone())))
        .child(div().mt(px(12.)).child(label("BIO")))
        .child(
            wrapper()
                .pb(px(12.))
                .child(div().min_h(px(60.)).child(e.bio.clone())),
        )
}
