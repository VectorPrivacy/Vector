//! `MiniProfile.svelte`: the compact preview an avatar or name tap opens. Banner, the
//! overlapping avatar, status pill, name, fingerprint, bio, their roles in the community it
//! was opened within, and two actions. Anchored beside the tap, else centred.

use std::rc::Rc;
use std::time::Instant;

use gpui::prelude::*;
use gpui::{AnyElement, App, Bounds, Hsla, Pixels, Point, SharedString, Size, Window, div, point, px, svg};
use crate::pic::img;
use serde_json::Value;

use crate::data::Store;
use crate::theme::*;

/// Relays stay silent for an identity with no metadata; after this long, call it Anon.
const ANON_AFTER_SECS: u64 = 6;
const WIDTH: f32 = 282.;
const MARGIN: f32 = 8.;

pub struct Mini {
    pub npub: String,
    pub community: Option<String>,
    /// What was tapped, in window coordinates; none centres it.
    pub at: Option<Bounds<Pixels>>,
    pub opened: Instant,
    pub roles: Option<Roles>,
    /// The role a change is under way for.
    pub busy: Option<String>,
}

pub struct Roles {
    pub owner: bool,
    pub roles: Vec<Role>,
    pub addable: bool,
}

pub struct Role {
    pub id: String,
    pub name: String,
    pub tint: Hsla,
    pub channel: String,
    pub removable: bool,
}

#[derive(Clone)]
pub enum Act {
    Close,
    Message(String),
    View(String),
    AddRole(Point<Pixels>),
    RemoveRole(String),
}

pub type OnAct = Rc<dyn Fn(Act, &mut Window, &mut App)>;

/// `roleTint`: a role's colour, or Discord's default grey.
pub fn tint(color: Option<u64>) -> Hsla {
    match color {
        Some(c) if c > 0 => crate::theme::c(c as u32),
        _ => crate::theme::c(0x99aab5),
    }
}

fn rank(view: &Value, npub: &str) -> i64 {
    roles_of(view)
        .filter(|r| holds(r, npub))
        .filter_map(|r| r["position"].as_i64())
        .min()
        .unwrap_or(i64::MAX)
}

fn roles_of(view: &Value) -> impl Iterator<Item = &Value> {
    view["roles"].as_array().into_iter().flatten()
}

fn holds(role: &Value, npub: &str) -> bool {
    role["holders"].as_array().is_some_and(|h| h.iter().any(|x| x.as_str() == Some(npub)))
}

/// `canEditMemberRoles`: Manage Roles and a strict outrank; the owner's own only by the owner.
pub fn can_edit(view: &Value, owner: Option<&str>, npub: &str) -> bool {
    if !view["can_manage"].as_bool().unwrap_or(false) {
        return false;
    }
    let is_owner = view["is_owner"].as_bool().unwrap_or(false);
    if owner == Some(npub) {
        return is_owner && view["activated"].as_bool().unwrap_or(false);
    }
    is_owner || view["rank"].as_i64().is_some_and(|r| r < rank(view, npub))
}

/// `loadMiniProfileRoles`: their standing, from the roles view.
pub fn roles(view: &Value, owner: Option<&str>, npub: &str) -> Roles {
    let editable = can_edit(view, owner, npub);
    let channel = |id: &str| {
        view["channels"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|c| c["channel_id"].as_str() == Some(id))
            .and_then(|c| c["name"].as_str())
            .unwrap_or_default()
            .to_string()
    };
    let manageable = |r: &Value| r["manageable"].as_bool().unwrap_or(false);
    Roles {
        owner: owner == Some(npub),
        roles: roles_of(view)
            .filter(|r| holds(r, npub))
            .map(|r| Role {
                id: r["role_id"].as_str().unwrap_or_default().to_string(),
                name: r["name"].as_str().unwrap_or_default().to_string(),
                tint: tint(r["color"].as_u64()),
                channel: r["channel_id"].as_str().map(channel).unwrap_or_default(),
                removable: editable && manageable(r),
            })
            .collect(),
        addable: editable && roles_of(view).any(|r| manageable(r) && !holds(r, npub)),
    }
}

fn click(on: &OnAct, act: Act) -> impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static {
    let on = on.clone();
    move |_, w, cx| {
        cx.stop_propagation();
        on(act.clone(), w, cx)
    }
}

/// Right of the tap, else left, else centred on it; clamped to the window.
fn place(at: Bounds<Pixels>, window: Size<Pixels>) -> Point<Pixels> {
    let w = f32::from(window.width);
    let (left, right) = (f32::from(at.left()), f32::from(at.right()));
    let mut x = right + MARGIN;
    if x + WIDTH + MARGIN > w {
        x = left - WIDTH - MARGIN;
        if x < MARGIN {
            x = (left + f32::from(at.size.width) / 2. - WIDTH / 2.).min(w - WIDTH - MARGIN).max(MARGIN);
        }
    }
    point(px(x), at.top().max(px(MARGIN)))
}

pub fn render(m: &Mini, store: &Store, window: Size<Pixels>, anchors: &crate::kit::Anchors, on: &OnAct) -> AnyElement {
    let npub = m.npub.as_str();
    let p = store.profiles.get(npub);
    let veiled = store.stream_hidden(npub);
    let real = p
        .map(|p| [&p.nickname, &p.name, &p.display_name].into_iter().find(|n| !n.is_empty()).cloned().unwrap_or_default())
        .unwrap_or_default();
    let nick = (!veiled && p.is_none_or(|p| p.nickname.is_empty()))
        .then(|| {
            m.community
                .as_ref()
                .and_then(|c| store.nicknames.get(c))
                .and_then(|n| n.get(npub))
                .filter(|n| !n.is_empty())
                .cloned()
        })
        .flatten();
    let shown = if veiled { store.name(npub) } else { nick.clone().unwrap_or_else(|| real.clone()) };
    let aka = nick.as_ref().filter(|n| !real.is_empty() && **n != real).map(|_| real.clone());
    let settled = p.is_some() || m.opened.elapsed().as_secs() >= ANON_AFTER_SECS;
    let loading = shown.is_empty() && !settled;
    let name = if !shown.is_empty() {
        shown
    } else if settled {
        "Anon".into()
    } else {
        "Loading…".into()
    };
    let status = if veiled { String::new() } else { p.map(|p| p.status.title.trim().to_string()).unwrap_or_default() };
    let about = if veiled { String::new() } else { p.map(|p| p.about.trim().to_string()).unwrap_or_default() };
    let fingerprint = if veiled {
        String::new()
    } else if npub.len() > 16 {
        format!("{}…{}", &npub[..12], &npub[npub.len() - 4..])
    } else {
        npub.to_string()
    };
    let banner = p
        .filter(|p| !veiled && !p.banner_cached.is_empty() && !store.pictures_held(npub))
        .map(|p| store.media_root.join(&p.banner_cached));
    let bot = p.is_some_and(|p| p.bot);
    let roles = m.roles.as_ref().filter(|r| r.owner || !r.roles.is_empty() || r.addable);

    let banner = div()
        .h(px(60.))
        .flex_shrink_0()
        .overflow_hidden()
        .bg(gpui::linear_gradient(
            135.,
            gpui::linear_color_stop(crate::call::accent_mix(0.18).blend(c(0x1a1a1a).opacity(0.82)), 0.),
            gpui::linear_color_stop(c(0x1a1a1a), 1.),
        ))
        .relative()
        .children(banner.map(|path| {
            img(path)
                .w(px(WIDTH - 2.))
                .h(px(60.))
                .aspect_ratio((WIDTH - 2.) / 60.)
                .object_fit(gpui::ObjectFit::Cover)
        }));
    let avatar = div()
        .id("mini-profile-avatar")
        .motion_live()
        .absolute()
        .top(px(28.))
        .left(px(12.))
        .size(px(70.))
        .rounded_full()
        .border(px(3.))
        .border_color(c(0x1a1a1a))
        .bg(c(0x1a1a1a))
        .overflow_hidden()
        .cursor_pointer()
        .on_click(click(on, Act::View(m.npub.clone())))
        .child(crate::ui::avatar(store, npub, 64.));
    let status = (!status.is_empty()).then(|| {
        div()
            .absolute()
            .left(px(84.))
            .top(px(50.))
            .max_w(px(WIDTH - 2. - 94.))
            .flex()
            .items_center()
            .gap(px(6.))
            .px(px(9.))
            .py(px(4.))
            .rounded(px(12.))
            .bg(ca(0, 0, 0, 0.55))
            .border_1()
            .border_color(white_a(0.08))
            .child(div().size(px(7.)).flex_shrink_0().rounded_full().bg(c(ACCENT)))
            .child(
                div().min_w_0().overflow_hidden().child(if crate::profile::status_has_emoji(store, npub) {
                    crate::profile::status(store, npub, crate::inline::Look::new(INTER, 11.5, 24., white_a(0.92))).into_any_element()
                } else {
                    crate::inline::emoji_line(&status, crate::inline::Look::new(INTER, 11.5, 24., white_a(0.92)), 13., 2.).into_any_element()
                }),
            )
    });
    let action = |id: &'static str, label: &'static str, primary: bool, act: Act| {
        div()
            .id(id)
            .flex_1()
            .h(px(29.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(7.))
            .border_1()
            .text_size(px(12.5))
            .font_weight(gpui::FontWeight::MEDIUM)
            .cursor_pointer()
            .when(primary, |d| {
                d.bg(c(PRIMARY)).border_color(gpui::transparent_black()).text_color(c(0x000000)).hover(|d| d.opacity(0.92))
            })
            .when(!primary, |d| {
                d.bg(white_a(0.04)).border_color(white_a(0.08)).text_color(c(0xffffff)).hover(|d| d.bg(white_a(0.1)))
            })
            .on_click(click(on, act))
            .child(label)
    };
    let roles = roles.map(|r| {
        let busy = m.busy.is_some();
        let pill = |content: gpui::Div| {
            div()
                .flex()
                .items_center()
                .gap(px(5.))
                .max_w_full()
                .h(px(22.))
                .pl(px(6.))
                .pr(px(8.))
                .rounded(px(6.))
                .bg(white_a(0.05))
                .border_1()
                .border_color(white_a(0.07))
                .text_size(px(12.))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(white_a(0.88))
                .child(content)
        };
        let mut list = div().flex().flex_wrap().gap(px(4.));
        if r.owner {
            list = list.child(pill(
                div()
                    .flex()
                    .items_center()
                    .gap(px(5.))
                    .child(svg().path("icons/crown.svg").size(px(11.)).text_color(c(0xfce459)))
                    .child(div().text_color(c(0xfce459)).child("Owner")),
            ));
        }
        for role in &r.roles {
            let group = SharedString::from(format!("mp-role-{}", role.id));
            let dot = div()
                .id(SharedString::from(format!("mp-dot-{}", role.id)))
                .size(px(10.))
                .flex_shrink_0()
                .rounded_full()
                .bg(role.tint)
                .border_1()
                .border_color(white_a(0.15))
                .flex()
                .items_center()
                .justify_center()
                .when(role.removable, |d| {
                    d.group_hover(group.clone(), |d| d.size(px(12.)).mx(px(-1.)))
                        .when(!busy, |d| d.cursor_pointer().on_click(click(on, Act::RemoveRole(role.id.clone()))))
                        .child(
                            svg()
                                .path("icons/x.svg")
                                .size_full()
                                .text_color(gpui::transparent_black())
                                .group_hover(group.clone(), |s| s.text_color(c(0x1a1a1a))),
                        )
                });
            let label = div()
                .min_w_0()
                .truncate()
                .when(!role.channel.is_empty() && role.channel == role.name, |d| {
                    d.child(div().mr(px(1.)).text_color(white_a(0.4)).child("#"))
                })
                .flex()
                .child(role.name.clone());
            list = list.child(
                pill(div().flex().items_center().gap(px(5.)).min_w_0().child(dot).child(label))
                    .group(group)
                    .when(m.busy.as_deref() == Some(role.id.as_str()), |d| d.opacity(0.45)),
            );
        }
        if r.addable {
            let (on, spot) = (on.clone(), anchors.clone());
            list = list.child(
                div()
                    .id("mp-role-add")
                    .relative()
                    .size(px(22.))
                    .rounded(px(6.))
                    .border_1()
                    .border_dashed()
                    .border_color(white_a(0.18))
                    .flex()
                    .items_center()
                    .justify_center()
                    .when(!busy, |d| {
                        d.cursor_pointer().hover(|d| d.bg(white_a(0.08)).border_color(white_a(0.3))).on_mouse_down(
                            gpui::MouseButton::Left,
                            move |e, w, cx| {
                                cx.stop_propagation();
                                let at = spot.borrow().get("mp-role-add").map_or(e.position, |b| b.bottom_left() + point(px(0.), px(4.)));
                                on(Act::AddRole(at), w, cx)
                            },
                        )
                    })
                    .child(svg().path("icons/plus.svg").size(px(12.)).text_color(white_a(0.6)))
                    .child(crate::kit::anchor(anchors, "mp-role-add".into())),
            );
        }
        div()
            .flex()
            .flex_col()
            .gap(px(5.))
            .child(
                crate::kit::spaced("ROLES", 0.63)
                    .mt(px(4.))
                    .text_size(px(10.5))
                    .line_height(px(24.))
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(white_a(0.45)),
            )
            .child(list)
    });
    let body = div()
        .pt(px(38.))
        .px(px(12.))
        .pb(px(12.))
        .flex()
        .flex_col()
        .gap(px(5.))
        .child(
            div()
                .flex()
                .items_center()
                .text_size(px(16.))
                .line_height(px(19.2))
                .font_weight(if loading { gpui::FontWeight::MEDIUM } else { gpui::FontWeight::BOLD })
                .text_color(if loading { white_a(0.4) } else { c(0xffffff) })
                .child(crate::inline::emoji_line(
                    &name,
                    {
                        let mut look = crate::inline::Look::new(INTER, 16., 19.2, if loading { white_a(0.4) } else { c(0xffffff) });
                        look.weight = if loading { gpui::FontWeight::MEDIUM } else { gpui::FontWeight::BOLD };
                        look
                    },
                    18.,
                    3.,
                ))
                .when(bot, |d| {
                    d.child(svg().path("icons/bot.svg").ml(px(5.)).size(px(14.)).text_color(c(ACCENT)))
                }),
        )
        .children(aka.map(|a| sub(a).mt(px(-10.))))
        .child(sub(fingerprint).mt(px(-10.)))
        .when(!about.is_empty(), |d| {
            d.child(
                div()
                    .px(px(2.))
                    .py(px(3.))
                    .text_center()
                    .text_size(px(12.5))
                    .line_height(px(17.5))
                    .text_color(white_a(0.78))
                    .max_h(px(17.5 * 4.))
                    .overflow_hidden()
                    .flex()
                    .flex_col()
                    .items_center()
                    .children(about.split('\n').enumerate().map(|(i, line)| {
                        let on = on.clone();
                        crate::profile::rich(store, None, line, &[], crate::inline::Look::new(INTER, 12.5, 17.5, white_a(0.78)), true)
                            .id(("mini-profile-about", i))
                            .on_click(Rc::new(move |t, w, cx| {
                                if let crate::inline::Target::Mention(npub) = t {
                                    on(Act::View(npub.to_string()), w, cx)
                                }
                            }))
                    })),
            )
        })
        .children(roles)
        .child(
            div()
                .mt(px(4.))
                .flex()
                .gap(px(6.))
                .child(action("mp-message", "Send Message", true, Act::Message(m.npub.clone())))
                .child(action("mp-view", "View Profile", false, Act::View(m.npub.clone()))),
        );
    let card = div()
        .id("mini-profile")
        .relative()
        .w(px(WIDTH))
        .max_h(window.height - px(32.))
        .flex()
        .flex_col()
        .rounded(px(12.))
        .bg(c(0x1a1a1a))
        .border_1()
        .border_color(white_a(0.08))
        .overflow_hidden()
        .shadow(vec![gpui::BoxShadow {
            color: ca(0, 0, 0, 0.55),
            offset: point(px(0.), px(10.)),
            blur_radius: px(36.),
            spread_radius: px(0.),
            inset: false,
        }])
        .occlude()
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(banner)
        .child(body)
        .child(avatar)
        .children(status);
    let close = on.clone();
    let placed = match m.at {
        Some(at) => gpui::anchored()
            .position(place(at, window))
            .snap_to_window_with_margin(px(MARGIN))
            .child(card)
            .into_any_element(),
        None => div()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .child(card.w(px(340f32.min(f32::from(window.width) - 32.))))
            .into_any_element(),
    };
    gpui::deferred(
        div()
            .absolute()
            .inset_0()
            .bg(ca(0, 0, 0, 0.55))
            .occlude()
            .on_mouse_down(gpui::MouseButton::Left, move |_, w, cx| close(Act::Close, w, cx))
            .on_scroll(|_, _, cx| cx.stop_propagation())
            .child(placed),
    )
    .with_priority(12)
    .into_any_element()
}

fn sub(text: String) -> gpui::Div {
    div()
        .text_size(px(11.))
        .line_height(px(24.))
        .font_family(crate::markdown::MONO)
        .text_color(white_a(0.45))
        .child(text)
}
