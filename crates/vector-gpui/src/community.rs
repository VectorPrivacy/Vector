//! Communities: the rail's rows, a Community's channel pane, its channel header and the
//! member panel beside the conversation.

use std::collections::HashSet;
use std::rc::Rc;

use gpui::prelude::*;
use gpui::AnimationExt;
use gpui::{div, img, px, svg, AnyElement, App, Entity, FontWeight, SharedString, Window};

use crate::composer::TextInput;
use crate::data::{Channel, Community, Store};
use crate::theme::*;

pub enum Act {
    Open(String),
    /// Back to Messages.
    Home,
    Channel(String),
    /// A Category or member section folded or unfolded.
    Fold(String),
    Panel,
    /// The Community's own menu, hung off its header.
    HeadMenu,
    BannerMenu(gpui::Point<gpui::Pixels>),
    ChannelMenu(String, gpui::Point<gpui::Pixels>),
    /// A Category's "+": a new channel inside it.
    AddChannel(String),
    Create,
    /// Invite People, for the open Community.
    Invite,
}

pub type OnAct = Rc<dyn Fn(Act, &mut Window, &mut App)>;

/// The open Community and what its panes hold.
pub struct View {
    pub id: String,
    pub members: Vec<String>,
    pub admins: Vec<String>,
    pub panel: bool,
    pub search: Entity<TextInput>,
    pub folded: HashSet<String>,
}

const OWNER: u32 = 0xfce459;
const LABEL: u32 = 0x8a8a8a;

/// The community's logo, or the group placeholder until it has one.
pub fn logo(store: &Store, id: &str, size: f32, radius: f32) -> gpui::Img {
    match store.icon(id) {
        Some(path) => img(path).object_fit(gpui::ObjectFit::Cover),
        None => img("icons/group-placeholder.svg"),
    }
    .size(px(size))
    .rounded(px(radius))
    .flex_shrink_0()
}

/// `.banner-art`: a banner cut to the 238 x 70 template at `width`, grey until it paints.
pub fn banner_art(path: std::path::PathBuf, width: f32) -> gpui::Img {
    img(path).w(px(width)).h(px(width * 70. / 238.)).bg(c(0x484848)).object_fit(gpui::ObjectFit::Cover)
}

fn click(on: &OnAct, act: impl Fn() -> Act + 'static) -> impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static {
    let on = on.clone();
    move |_, window, cx| on(act(), window, cx)
}

/// `#ws-rail-spaces` rows: a rounded square per Community, the open one marked by a bar.
pub fn rail_rows(store: &Store, active: Option<&str>, on: &OnAct) -> impl IntoElement {
    div().children(store.communities.iter().filter(|c| !c.dissolved).map(|com| {
        let open = active == Some(com.community_id.as_str());
        let unread = !open && com.channels.iter().any(|ch| channel_unread(store, ch) > 0);
        let id = com.community_id.clone();
        div()
            .id(SharedString::from(format!("rail-c-{}", com.community_id)))
            .relative()
            .mx(px(8.))
            .h(px(31.))
            .rounded(px(10.))
            .flex()
            .items_center()
            .px(px(8.))
            .gap(px(10.))
            .cursor_pointer()
            .hover(|d| d.bg(white_a(0.045)))
            .on_click(click(on, move || Act::Open(id.clone())))
            .when(open, |d| d.child(div().absolute().left(px(-8.)).top(px(3.)).w(px(4.)).h(px(25.)).rounded_r(px(4.)).bg(c(PRIMARY))))
            .when(unread, |d| d.child(div().absolute().left(px(-8.)).top(px(10.5)).w(px(4.)).h(px(10.)).rounded_r(px(4.)).bg(c(ICON_MUTED))))
            .child(logo(store, &com.community_id, 26., 4.9))
            .child(div().flex_1().min_w_0().truncate().font_family(RUBIK).text_size(px(14.)).text_color(c(0xffffff)).child(com.name.clone()))
    }))
}

fn icon(path: &'static str, size: f32, color: gpui::Hsla) -> gpui::Svg {
    svg().path(path).size(px(size)).flex_shrink_0().text_color(color)
}

fn channel_glyph(ch: &Channel) -> &'static str {
    if ch.is_voice() {
        "icons/volume-max.svg"
    } else if ch.private {
        "icons/locked.svg"
    } else {
        "icons/channel-hash.svg"
    }
}

fn channel_unread(store: &Store, ch: &Channel) -> usize {
    store.chat(&ch.channel_id).map(|chat| store.unread_count(chat)).unwrap_or(0)
}

fn channel_row(store: &Store, ch: &Channel, open: bool, owner: bool, on: &OnAct) -> impl IntoElement {
    let id = ch.channel_id.clone();
    let group = SharedString::from(format!("ch-{}", ch.channel_id));
    let unread = !open && channel_unread(store, ch) > 0;
    let ink = if open || unread { c(0xffffff) } else { white_a(0.55) };
    let action = |path: &'static str, shown: bool, act: Option<fn() -> Act>| {
        div()
            .id(path)
            .when_some(act, |d, act| {
                let on = on.clone();
                d.on_click(move |_, w, cx| {
                    cx.stop_propagation();
                    on(act(), w, cx)
                })
            })
            .size(px(18.))
            .flex()
            .items_center()
            .justify_center()
            .flex_shrink_0()
            .opacity(if shown { 0.6 } else { 0. })
            .group_hover(group.clone(), |d| d.opacity(0.6))
            .child(icon(path, 14., c(0xffffff)))
    };
    div()
        .id(SharedString::from(format!("chatlist-channel-{}", ch.channel_id)))
        .group(group.clone())
        .mx(px(8.))
        .h(px(32.))
        .flex_shrink_0()
        .rounded(px(4.))
        .flex()
        .items_center()
        .gap(px(8.))
        .px(px(8.))
        .cursor_pointer()
        .text_color(ink)
        .when(open, |d| d.bg(white_a(0.09)))
        .when(!open, |d| d.hover(|d| d.bg(white_a(0.045)).text_color(c(0xffffff))))
        .on_click(click(on, move || Act::Channel(id.clone())))
        .on_mouse_down(gpui::MouseButton::Right, {
            let (on, id) = (on.clone(), ch.channel_id.clone());
            move |e, w, cx| on(Act::ChannelMenu(id.clone(), e.position), w, cx)
        })
        .child(icon(channel_glyph(ch), 18., ink))
        .child(div().flex_1().min_w_0().truncate().text_size(px(15.)).line_height(px(24.)).when(unread, |d| d.font_weight(FontWeight::MEDIUM)).child(ch.name.clone()))
        .when(ch.is_voice(), |d| d.child(action("icons/chat-bubble.svg", false, None)))
        .when(!ch.is_voice() && owner, |d| d.gap(px(8.)).child(action("icons/add-user.svg", open, Some(|| Act::Invite))).child(action("icons/settings.svg", open, None)))
}

/// `.chatlist-community-head-alert`: pulses, since noticing a raid late costs the community.
fn raid_pip() -> impl IntoElement {
    let danger = c(crate::timeline::DANGER);
    div().size(px(8.)).flex_shrink_0().rounded_full().bg(danger).with_animation(
        "raid-pulse",
        gpui::Animation::new(std::time::Duration::from_millis(1900)).repeat(),
        move |d, t| {
            let (spread, alpha) = if t < 0.7 {
                let k = gpui::ease_out_quint()(t / 0.7);
                (7. * k, 0.7 * (1. - k))
            } else {
                (7. * (1. - (t - 0.7) / 0.3), 0.)
            };
            d.shadow(vec![gpui::BoxShadow { color: danger.opacity(alpha), offset: gpui::point(px(0.), px(0.)), blur_radius: px(0.), spread_radius: px(spread), inset: false }])
        },
    )
}

/// `#chats` over a Community: its head, then its channels by Category.
pub fn sidebar(store: &Store, view: &View, com: &Community, open: Option<&str>, raid: bool, on: &OnAct) -> AnyElement {
    let head = div()
        .id("chatlist-community-head")
        .h(px(61.))
        .flex_shrink_0()
        .flex()
        .items_center()
        .gap(px(10.))
        .px(px(12.))
        .bg(c(0x171717))
        .border_b_1()
        .border_color(c(DIVIDER))
        .cursor_pointer()
        .hover(|d| d.bg(white_a(0.045)))
        .on_click(click(on, || Act::HeadMenu))
        .on_mouse_down(gpui::MouseButton::Right, {
            let on = on.clone();
            move |_, w, cx| on(Act::HeadMenu, w, cx)
        })
        .child(logo(store, &com.community_id, 36., 11.))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(div().truncate().font_family(RUBIK).font_weight(FontWeight::SEMIBOLD).text_size(px(15.)).line_height(px(18.)).text_color(c(0xffffff)).child(com.name.clone()))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(2.5))
                        .text_size(px(12.))
                        .line_height(px(14.))
                        .text_color(white_a(0.42))
                        .child(icon("icons/users-multi.svg", 13., white_a(0.42)))
                        .child(match view.members.len() {
                            1 => "1 Member".to_string(),
                            n => format!("{n} Members"),
                        }),
                ),
        )
        .when(raid, |d| d.child(raid_pip()))
        .child(div().size(px(30.)).flex().items_center().justify_center().child(icon("icons/chevron-down.svg", 19.5, white_a(0.4))));

    let banner = store.banner(&com.community_id).filter(|_| !store.hidden_banners.contains(&com.community_id)).map(|path| {
        div()
            .id("ws-banner")
            .flex_none()
            .border_b_1()
            .border_color(c(0x1f1f1f))
            .on_mouse_down(gpui::MouseButton::Right, {
                let on = on.clone();
                move |e, w, cx| on(Act::BannerMenu(e.position), w, cx)
            })
            .child(banner_art(path, f32::from(crate::ui::LIST_W)))
    });
    let head = div().flex_none().flex().flex_col().child(head).children(banner);

    let owner = com.is_owner;
    let loose: Vec<&Channel> = com.channels.iter().filter(|ch| ch.category_id.is_none()).collect();
    let mut sections = Vec::new();
    if !loose.is_empty() {
        sections.push(div().pt(px(10.)).pb(px(2.)).flex().flex_col().gap(px(2.)).children(loose.iter().map(|ch| channel_row(store, ch, open == Some(ch.channel_id.as_str()), owner, on))).into_any_element());
    }
    for cat in &com.categories {
        let inside: Vec<&Channel> = com.channels.iter().filter(|ch| ch.category_id.as_deref() == Some(cat.category_id.as_str())).collect();
        let closed = view.folded.contains(&cat.category_id);
        let key = cat.category_id.clone();
        let head = div()
            .flex()
            .items_center()
            .gap(px(4.))
            .pt(px(10.))
            .pb(px(4.))
            .px(px(12.))
            .child(
                div()
                    .id(SharedString::from(format!("cat-{}", cat.category_id)))
                    .flex_1()
                    .min_w_0()
                    .h(px(28.))
                    .px(px(4.))
                    .rounded(px(6.))
                    .flex()
                    .items_center()
                    .gap(px(2.))
                    .cursor_pointer()
                    .on_click(click(on, move || Act::Fold(key.clone())))
                    .child(crate::kit::spaced(cat.name.to_uppercase(), 0.99).font_weight(FontWeight::BOLD).text_size(px(11.)).line_height(px(24.)).text_color(c(LABEL)))
                    .child(
                        div()
                            .size(px(18.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(icon("icons/chevron-down.svg", 11.7, white_a(0.38)).when(closed, |s| s.with_transformation(gpui::Transformation::rotate(gpui::radians(-std::f32::consts::FRAC_PI_2))))),
                    ),
            )
            .when(owner, |d| {
                let key = cat.category_id.clone();
                d.child(
                    div()
                        .id(SharedString::from(format!("cat-add-{}", cat.category_id)))
                        .size(px(22.))
                        .rounded(px(6.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .hover(|d| d.bg(white_a(0.045)))
                        .on_click(click(on, move || Act::AddChannel(key.clone())))
                        .child(icon("icons/plus.svg", 14.3, white_a(0.38))),
                )
            });
        sections.push(
            div()
                .when(!sections.is_empty(), |d| d.mt(px(14.)))
                .child(head)
                .when(!closed, |d| d.child(div().pb(px(2.)).flex().flex_col().gap(px(2.)).children(inside.iter().map(|ch| channel_row(store, ch, open == Some(ch.channel_id.as_str()), owner, on)))))
                .into_any_element(),
        );
    }
    div()
        .w(px(crate::ui::LIST_W))
        .flex_shrink_0()
        .h_full()
        .flex()
        .flex_col()
        .bg(c(LIST_BG))
        .border_r_1()
        .border_color(c(DIVIDER))
        .child(head)
        .child(div().id("community-channels").flex_1().min_h_0().overflow_y_scroll().children(sections))
        .into_any_element()
}

/// The conversation header over a channel: its glyph and name, its topic past a hairline,
/// then the menu and the member panel's toggle.
pub fn header(ch: &Channel, panel: bool, on: &OnAct) -> impl IntoElement {
    div()
        .h(px(61.))
        .flex_shrink_0()
        .bg(c(CHAT_HEADER_BG))
        .border_b_1()
        .border_color(c(DIVIDER))
        .flex()
        .items_center()
        .pl(px(20.))
        .pr(px(10.))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .mt(px(5.))
                .flex()
                .items_center()
                .gap(px(12.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .flex_shrink_0()
                        .gap(px(8.))
                        .child(icon(channel_glyph(ch), 19., white_a(0.45)))
                        .child(div().font_family(RUBIK).font_weight(FontWeight::BOLD).text_size(px(18.72)).line_height(px(24.)).text_color(c(TEXT)).child(ch.name.clone())),
                )
                .children(ch.topic.clone().filter(|t| !t.is_empty()).map(|topic| {
                    div()
                        .flex_1()
                        .min_w_0()
                        .pl(px(12.))
                        .border_l_1()
                        .border_color(white_a(0.15))
                        .opacity(0.8)
                        .truncate()
                        .text_size(px(13.5))
                        .line_height(px(18.))
                        .text_color(white_a(0.5))
                        .child(topic)
                })),
        )
        .child(div().size(px(40.)).flex().items_center().justify_center().child(icon("ui/menu-dots.svg", 28., c(ICON_MUTED))))
        .child(
            div()
                .id("details-toggle")
                .ml(px(4.))
                .size(px(32.))
                .rounded(px(6.))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .text_color(c(ICON_MUTED))
                .hover(|d| d.bg(white_a(0.045)))
                .on_click(click(on, || Act::Panel))
                .child(icon(if panel { "ui/details-toggle-open.svg" } else { "ui/details-toggle.svg" }, 22.8, c(ICON_MUTED))),
        )
}

fn status_line(text: &str) -> AnyElement {
    let look = crate::inline::Look::new(INTER, 11.5, 15.5, white_a(0.38));
    crate::inline::inline(crate::preview::atoms(text, usize::MAX, &look), look).single_line().into_any_element()
}

fn member_row(store: &Store, cid: &str, npub: &str, owner: bool) -> impl IntoElement {
    let status = store.profiles.get(npub).map(|p| p.status.title.clone()).filter(|t| !t.is_empty());
    div()
        .h(px(40.))
        .rounded(px(6.))
        .flex()
        .items_center()
        .px(px(8.))
        .hover(|d| d.bg(white_a(0.045)))
        .child(crate::ui::avatar(store, npub, 32.))
        .child(
            div()
                .ml(px(12.))
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(9.))
                        .child(div().truncate().font_weight(FontWeight::SEMIBOLD).text_size(px(14.)).line_height(px(18.)).text_color(c(0xf7f4f4)).child(store.name_in(npub, Some(cid))))
                        .when(owner, |d| d.child(icon("icons/crown.svg", 15., c(OWNER)))),
                )
                .children(status.map(|s| div().truncate().text_size(px(11.5)).line_height(px(15.5)).text_color(white_a(0.38)).child(status_line(&s)))),
        )
}

/// `#group-overview`: the roster, filtered by the search, by rank.
pub fn members_panel(store: &Store, view: &View, com: &Community, on: &OnAct, cx: &App) -> impl IntoElement {
    let query = view.search.read(cx).text().trim().to_lowercase();
    let owner = com.owner_npub.as_deref();
    let matches = |n: &&String| query.is_empty() || store.name_in(n, Some(&com.community_id)).to_lowercase().contains(&query) || n.contains(&query);
    let is_admin = |n: &str| owner == Some(n) || view.admins.iter().any(|a| a == n);
    let admins: Vec<&String> = view.members.iter().filter(|n| is_admin(n)).filter(matches).collect();
    let rest: Vec<&String> = view.members.iter().filter(|n| !is_admin(n)).filter(matches).collect();
    let section = |key: &'static str, label: &'static str, list: Vec<&String>| {
        let closed = view.folded.contains(key);
        (!list.is_empty()).then(|| {
            div()
                .child(
                    div()
                        .id(key)
                        .h(px(36.))
                        .rounded(px(6.))
                        .flex()
                        .items_start()
                        .pt(px(4.))
                        .px(px(8.))
                        .gap(px(6.))
                        .cursor_pointer()
                        .on_click(click(on, move || Act::Fold(key.to_string())))
                        .child(crate::kit::spaced(label, 0.99).font_weight(FontWeight::BOLD).text_size(px(11.)).line_height(px(24.)).text_color(c(LABEL)))
                        .child(div().flex_1().font_weight(FontWeight::SEMIBOLD).text_size(px(11.)).line_height(px(24.)).text_color(white_a(0.32)).child(list.len().to_string()))
                        .child(
                            div().mt(px(8.)).size(px(16.)).flex().items_center().justify_center().child(
                                icon("icons/chevron-down.svg", 10.4, white_a(0.32)).when(closed, |s| s.with_transformation(gpui::Transformation::rotate(gpui::radians(-std::f32::consts::FRAC_PI_2)))),
                            ),
                        ),
                )
                .when(!closed, |d| d.children(list.into_iter().map(|n| member_row(store, &com.community_id, n, owner == Some(n.as_str())))))
        })
    };
    let admin_section = section("sec-admin", "ADMIN", admins);
    let gap = admin_section.is_some();
    div()
        .id("group-overview")
        .w(px(240.))
        .flex_shrink_0()
        .h_full()
        .flex()
        .flex_col()
        .bg(c(0x070707))
        .border_l_1()
        .border_color(c(DIVIDER))
        .child(
            div().h(px(61.)).flex_shrink_0().bg(c(CHAT_HEADER_BG)).border_b_1().border_color(c(DIVIDER)).flex().items_center().px(px(14.)).child(
                div()
                    .relative()
                    .w_full()
                    .h(px(32.))
                    .rounded_full()
                    .border_1()
                    .border_color(c(DIVIDER))
                    .flex()
                    .items_center()
                    .pl(px(37.))
                    .pr(px(12.))
                    .child(icon("icons/search.svg", 16., white_a(0.4)).absolute().left(px(13.)).top(px(7.)))
                    .child(div().flex_1().min_w_0().child(view.search.clone())),
            ),
        )
        .child(
            div()
                .id("member-roster")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .px(px(8.))
                .children(admin_section)
                .children(section("sec-members", "MEMBERS", rest).map(|s| s.when(gap, |d| d.mt(px(12.))))),
        )
}

/// Owner and admin marks after an author's name in a channel.
pub fn author_badge(store: &Store, chat_id: &str, npub: &str) -> Option<AnyElement> {
    let com = store.community_of(chat_id)?;
    let owner = com.owner_npub.as_deref() == Some(npub);
    owner.then(|| {
        crate::kit::spaced("OWNER", 0.5)
            .flex_shrink_0()
            .pt(px(2.75))
            .pb(px(1.25))
            .px(px(5.))
            .rounded(px(3.))
            .bg(c(OWNER))
            .font_family(RUBIK)
            .font_weight(FontWeight::SEMIBOLD)
            .text_size(px(9.))
            .line_height(px(9.))
            .text_color(c(0x000000))
            .into_any_element()
    })
}

type Click = Box<dyn Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static>;

/// `InviteRow.svelte`: a pending invite, shaped like the Community row it becomes.
pub fn invite_row(inv: &crate::data::Invite, open: Click, accept: Click, decline: Click) -> impl IntoElement {
    let button = |id: String, icon_path: &'static str, ink: gpui::Hsla, on: Click| {
        div()
            .id(SharedString::from(id))
            .size(px(28.))
            .flex_shrink_0()
            .rounded_full()
            .border_2()
            .border_color(ink)
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .hover(|d| d.bg(white_a(0.04)))
            .on_click(move |e, w, cx| {
                cx.stop_propagation();
                on(e, w, cx)
            })
            .child(svg().path(icon_path).size(px(16.)).text_color(ink))
    };
    div()
        .id(SharedString::from(format!("community-invite-{}", inv.community_id)))
        .mx(px(7.))
        .mb(px(4.))
        .h(px(50.))
        .flex_shrink_0()
        .bg(white_a(0.05))
        .flex()
        .items_center()
        .pl(px(5.))
        .pr(px(15.))
        .cursor_pointer()
        .on_click(open)
        .child(gpui::img("icons/group-placeholder.svg").size(px(40.)).rounded(px(7.56)).flex_shrink_0())
        .child(
            div()
                .w(px(99.))
                .flex_shrink_0()
                .pl(px(10.))
                .flex()
                .flex_col()
                .font_family(RUBIK)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .child(div().min_w_0().truncate().text_size(px(15.)).line_height(px(20.)).text_color(c(0xffffff)).child(inv.name.clone()))
                        .child(svg().path("icons/users-multi.svg").size(px(14.)).flex_shrink_0().text_color(white_a(0.55))),
                )
                .child(div().mt(px(-4.)).truncate().text_size(px(13.)).line_height(px(18.)).text_color(white_a(0.85)).child("Community invite")),
        )
        .child(
            div()
                .ml_auto()
                .flex()
                .gap(px(8.))
                .child(button(format!("invite-accept-{}", inv.community_id), "icons/check.svg", c(ACCENT), accept))
                .child(button(format!("invite-decline-{}", inv.community_id), "icons/x.svg", c(crate::timeline::DANGER), decline)),
        )
}

/// `InviteDetails.svelte`: who sent a pending invite, when, and until when.
pub fn invite_details(store: &Store, inv: &crate::data::Invite, close: Click, accept: Click, decline: Click) -> impl IntoElement {
    let now = (store.now_ms() / 1000) as i64;
    let person = |npub: &String| {
        div()
            .flex()
            .items_center()
            .gap(px(8.))
            .min_w_0()
            .child(crate::ui::avatar(store, npub, 22.))
            .child(div().truncate().child(store.name(npub)))
    };
    let owner = inv
        .owner
        .as_deref()
        .and_then(|h| nostr_sdk::prelude::PublicKey::from_hex(h).ok())
        .and_then(|k| nostr_sdk::prelude::ToBech32::to_bech32(&k).ok())
        .filter(|o| *o != inv.inviter);
    let sent = if inv.sent_at > 0 { ("Sent", inv.sent_at) } else { ("Received", inv.received_at) };
    let row = |first: bool, label: &'static str, value: AnyElement| {
        div()
            .h(px(42.))
            .px(px(14.))
            .flex()
            .items_center()
            .justify_between()
            .gap(px(12.))
            .when(!first, |d| d.h(px(43.)).border_t_1().border_color(c(0x1e1e1e)))
            .text_size(px(14.))
            .line_height(px(24.))
            .text_color(c(0xe8e8e8))
            .child(div().flex_shrink_0().text_color(white_a(0.5)).child(label))
            .child(value)
    };
    let button = |id: &'static str, label: &'static str, primary: bool, on: Click| {
        div()
            .id(id)
            .flex_1()
            .h(px(42.))
            .rounded(px(8.))
            .border_1()
            .flex()
            .items_center()
            .justify_center()
            .font_weight(FontWeight::SEMIBOLD)
            .text_size(px(16.))
            .cursor_pointer()
            .hover(|d| d.opacity(0.85))
            .map(|d| if primary { d.border_color(gpui::transparent_black()).bg(c(ACCENT)).text_color(c(0x000000)) } else { d.border_color(c(crate::timeline::DANGER)).text_color(c(0xffffff)) })
            .on_click(on)
            .child(label)
    };
    let card = div()
        .id("invite-details")
        .w(px(422.))
        .p(px(21.))
        .rounded(px(16.))
        .border_1()
        .border_color(c(0x262626))
        .bg(ca(20, 20, 20, 0.95))
        .shadow(vec![gpui::BoxShadow { color: ca(0, 0, 0, 0.4), offset: gpui::point(px(0.), px(8.)), blur_radius: px(32.), spread_radius: px(0.), inset: false }])
        .occlude()
        .on_click(|_, _, cx| cx.stop_propagation())
        .flex()
        .flex_col()
        .child(
            div()
                .flex()
                .flex_col()
                .items_center()
                .child(img("icons/group-placeholder.svg").size(px(72.)).rounded_full())
                .child(div().mt(px(12.)).max_w_full().truncate().font_weight(FontWeight::BOLD).text_size(px(20.)).line_height(px(24.)).text_color(c(0xffffff)).child(inv.name.clone()))
                .child(div().mt(px(4.)).text_size(px(13.)).line_height(px(24.)).text_color(white_a(0.5)).child("Community invite")),
        )
        .child(
            div()
                .mt(px(15.))
                .rounded(px(12.))
                .border_1()
                .border_color(c(0x262626))
                .flex()
                .flex_col()
                .child(row(true, "From", person(&inv.inviter).into_any_element()))
                .children(owner.map(|o| row(false, "Owner", person(&o).into_any_element())))
                .child(row(false, sent.0, div().child(crate::util::relative_time(sent.1, now)).into_any_element()))
                .child(row(false, "Expires", div().child(crate::util::relative_time(inv.ends_at, now)).into_any_element())),
        )
        .child(div().mt(px(25.)).flex().gap(px(10.)).child(button("invite-decline", "Decline", false, decline)).child(button("invite-accept", "Accept", true, accept)));
    gpui::deferred(
        div()
            .id("invite-details-overlay")
            .absolute()
            .inset_0()
            .bg(ca(0, 0, 0, 0.85))
            .flex()
            .items_center()
            .justify_center()
            .occlude()
            .on_click(close)
            .child(card),
    )
    .with_priority(13)
}
