//! Communities: the rail's rows, a Community's channel pane, its channel header and the
//! member panel beside the conversation.

use std::collections::HashSet;
use std::rc::Rc;

use gpui::AnimationExt;
use gpui::prelude::*;
use gpui::{AnyElement, App, Entity, FontWeight, SharedString, Window, div, px, svg};
use crate::pic::img;

use crate::composer::TextInput;
use crate::data::{Channel, Community, Store};
use crate::theme::*;

pub enum Act {
    /// A member list's warning mark: why they're listed apart.
    Unlisted(String),
    Open(String),
    /// Back to Messages.
    Home,
    /// Right-click on the Messages door.
    MessagesMenu(gpui::Point<gpui::Pixels>),
    /// An unread DM from the rail.
    Dm(String),
    Channel(String),
    /// A voice channel's chat, without joining its call.
    ChannelChat(String),
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
    /// A rail folder opened or shut.
    RailFolder(String),
    /// Right-click on a rail row: a community's key, or a folder's id.
    RailMenu(Option<String>, Option<String>, gpui::Point<gpui::Pixels>),
    RailDrop(
        vector_core::rail_layout::RailDrag,
        vector_core::rail_layout::RailDrop,
    ),
    /// Invite People, for the open Community.
    Invite,
    CategoryMenu(String, gpui::Point<gpui::Pixels>),
    /// The open channel's ⋯.
    ChatMenu,
    EditChannel(String),
    /// A member tapped: their mini profile, beside what was painted under the key.
    Member(String, SharedString),
    /// A member right-clicked: their menu.
    MemberMenu(String, gpui::Point<gpui::Pixels>),
    /// A roster section head's menu: a role's carries its id.
    SectionMenu(String, gpui::Point<gpui::Pixels>),
}

pub type OnAct = Rc<dyn Fn(Act, &mut Window, &mut App)>;

/// The open Community and what its panes hold.
pub struct View {
    pub id: String,
    pub members: Vec<String>,
    pub admins: Vec<String>,
    /// Seen posting outside the Guestbook: npub → `no_join` | `departed`. Listed, never counted.
    pub unlisted: std::collections::HashMap<String, String>,
    /// No roster yet, cached or read.
    pub loading: bool,
    pub panel: bool,
    pub search: Entity<TextInput>,
    pub folded: HashSet<String>,
}

struct GraphRole {
    id: String,
    name: String,
    position: i64,
    color: u32,
    channel: Option<String>,
    display: bool,
}

/// `get_community_role_graph`: every role with its rank, who holds which, and on the new roles
/// each Private Channel's readers.
#[derive(Default)]
pub struct Graph {
    activated: bool,
    roles: Vec<GraphRole>,
    grants: std::collections::HashMap<String, Vec<String>>,
    readers: std::collections::HashMap<String, HashSet<String>>,
}

impl Graph {
    pub fn parse(v: &serde_json::Value) -> Self {
        let text = |v: &serde_json::Value| v.as_str().map(str::to_string);
        let list = |v: &serde_json::Value| v.as_array().into_iter().flatten().filter_map(text).collect::<Vec<_>>();
        Graph {
            activated: v["activated"].as_bool().unwrap_or(false),
            roles: v["roles"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|r| {
                    Some(GraphRole {
                        id: text(&r["role_id"])?,
                        name: text(&r["name"]).unwrap_or_default(),
                        position: r["position"].as_i64().unwrap_or(i64::MAX),
                        color: r["color"].as_u64().unwrap_or(0) as u32,
                        channel: text(&r["channel_id"]).map(|c| c.to_ascii_lowercase()),
                        display: r["display"].as_bool().unwrap_or(false),
                    })
                })
                .collect(),
            grants: v["grants"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|g| Some((text(&g["npub"])?, list(&g["role_ids"]))))
                .collect(),
            readers: v["readers"]
                .as_object()
                .into_iter()
                .flatten()
                .map(|(k, v)| (k.to_ascii_lowercase(), list(v).into_iter().collect()))
                .collect(),
        }
    }

    /// Heads a section: on the new roles only a role shown separately; rank still counts them all.
    fn heads(&self, r: &GraphRole) -> bool {
        !self.activated || r.display
    }

    /// Their highest role (lowest position) among those `pick` admits.
    fn highest(&self, npub: &str, pick: impl Fn(&GraphRole) -> bool) -> Option<&GraphRole> {
        self.grants
            .get(npub)?
            .iter()
            .filter_map(|id| self.roles.iter().find(|r| &r.id == id))
            .filter(|r| pick(r))
            .min_by_key(|r| r.position)
    }

    /// Who may read Private Channel `channel`: the owner, and on the old roles the holders of a
    /// role scoped to it.
    fn readers(&self, channel: &str, owner: Option<&str>) -> HashSet<String> {
        let mut out: HashSet<String> = owner.map(str::to_string).into_iter().collect();
        if self.activated {
            out.extend(self.readers.get(channel).into_iter().flatten().cloned());
        } else {
            let access: HashSet<&str> =
                self.roles.iter().filter(|r| r.channel.as_deref() == Some(channel)).map(|r| r.id.as_str()).collect();
            out.extend(self.grants.iter().filter(|(_, ids)| ids.iter().any(|i| access.contains(i.as_str()))).map(|(n, _)| n.clone()));
        }
        out
    }
}

const OWNER: u32 = 0xfce459;
const LABEL: u32 = 0x8a8a8a;

/// The community's logo, or the group placeholder until it has one.
pub fn logo(store: &Store, id: &str, size: f32, radius: f32) -> gpui::Stateful<gpui::Img> {
    match store.icon(id) {
        Some(path) => img(path).object_fit(gpui::ObjectFit::Cover),
        None => img("icons/group-placeholder.svg"),
    }
    .size(px(size))
    .aspect_ratio(1.)
    .rounded(px(radius))
    .flex_shrink_0()
}

/// `.banner-art`: a banner cut to the 238 x 70 template at `width`, grey until it paints.
pub fn banner_art(path: std::path::PathBuf, width: f32) -> gpui::Stateful<gpui::Img> {
    img(path)
        .w(px(width))
        .h(px(width * 70. / 238.))
        .aspect_ratio(238. / 70.)
        .bg(c(0x484848))
        .object_fit(gpui::ObjectFit::Cover)
}

fn click(
    on: &OnAct,
    act: impl Fn() -> Act + 'static,
) -> impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static {
    let on = on.clone();
    move |_, window, cx| on(act(), window, cx)
}

fn icon(path: &'static str, size: f32, color: gpui::Hsla) -> gpui::Svg {
    svg()
        .path(path)
        .size(px(size))
        .flex_shrink_0()
        .text_color(color)
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
    store
        .chat(&ch.channel_id)
        .map(|chat| store.unread_count(chat))
        .unwrap_or(0)
}

fn channel_row(
    store: &Store,
    ch: &Channel,
    open: bool,
    owner: bool,
    on: &OnAct,
) -> impl IntoElement {
    let id = ch.channel_id.clone();
    let group = SharedString::from(format!("ch-{}", ch.channel_id));
    let unread = !open && channel_unread(store, ch) > 0;
    let ink = if open || unread {
        c(0xffffff)
    } else {
        white_a(0.55)
    };
    let action = |path: &'static str, shown: bool, act: Option<Rc<dyn Fn() -> Act>>| {
        let tip = match path {
            "icons/add-user.svg" => Some("Invite People"),
            "icons/settings.svg" => Some("Edit Channel"),
            "icons/chat-bubble.svg" => Some("Open Chat"),
            _ => None,
        };
        div()
            .id(path)
            .when_some(tip.filter(|_| shown), |d, t| d.tooltip(crate::kit::tooltip(t)))
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
        .id(SharedString::from(format!(
            "chatlist-channel-{}",
            ch.channel_id
        )))
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
        .when(!open, |d| {
            d.hover(|d| d.bg(white_a(0.045)).text_color(c(0xffffff)))
        })
        .on_click(click(on, move || Act::Channel(id.clone())))
        .on_mouse_down(gpui::MouseButton::Right, {
            let (on, id) = (on.clone(), ch.channel_id.clone());
            move |e, w, cx| on(Act::ChannelMenu(id.clone(), e.position), w, cx)
        })
        .child(icon(channel_glyph(ch), 18., ink))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_size(px(15.))
                .line_height(px(24.))
                .when(unread, |d| d.font_weight(FontWeight::MEDIUM))
                .child(ch.name.clone()),
        )
        .when(owner && !ch.private, |d| {
            d.child(action("icons/add-user.svg", open, Some(Rc::new(|| Act::Invite))))
        })
        .when(owner, |d| {
            let gear = ch.channel_id.clone();
            d.child(action("icons/settings.svg", open, Some(Rc::new(move || Act::EditChannel(gear.clone())))))
        })
        .when(ch.is_voice(), |d| {
            let chat = ch.channel_id.clone();
            d.child(action("icons/chat-bubble.svg", open, Some(Rc::new(move || Act::ChannelChat(chat.clone())))))
        })
}

/// `.chatlist-community-head-alert`: pulses, since noticing a raid late costs the community.
fn raid_pip() -> impl IntoElement {
    let danger = c(crate::timeline::DANGER);
    // A ring, not a box-shadow spread: GPUI keeps a shadow's corner radius as the spread grows.
    let ring = div().absolute().rounded_full().with_animation(
        "raid-pulse",
        gpui::Animation::new(std::time::Duration::from_millis(1900)).repeat(),
        move |d, t| {
            let (spread, alpha) = if t < 0.7 {
                let k = gpui::ease_out_quint()(t / 0.7);
                (7. * k, 0.7 * (1. - k))
            } else {
                (7. * (1. - (t - 0.7) / 0.3), 0.)
            };
            d.left(px(-spread)).top(px(-spread)).size(px(8. + 2. * spread)).bg(danger.opacity(alpha))
        },
    );
    div()
        .relative()
        .size(px(8.))
        .flex_shrink_0()
        .child(ring)
        .child(div().absolute().inset_0().rounded_full().bg(danger))
}

/// `#chats` over a Community: its head, then its channels by Category.
pub fn sidebar(
    store: &Store,
    view: &View,
    com: &Community,
    open: Option<&str>,
    raid: bool,
    on: &OnAct,
    (voice, on_voice, anchors): (&crate::voice::Voice, &crate::voice::OnAct, &crate::kit::Anchors),
    width: f32,
    slim: bool,
) -> AnyElement {
    let row = |ch: &Channel| {
        let open = open == Some(ch.channel_id.as_str());
        let row = channel_row(store, ch, open, com.is_owner, on).into_any_element();
        if !ch.is_voice() || (voice.present(&ch.channel_id).is_empty() && voice.joining.as_deref() != Some(ch.channel_id.as_str())) {
            return vec![row];
        }
        let who = crate::voice::members(voice, store, &com.community_id, &ch.channel_id, on_voice, anchors);
        vec![row, who]
    };
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
                .child(
                    div()
                        .truncate()
                        .font_family(RUBIK)
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_size(px(15.))
                        .line_height(px(18.))
                        .text_color(c(0xffffff))
                        .child(com.name.clone()),
                )
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
        .child(
            div()
                .size(px(30.))
                .flex()
                .items_center()
                .justify_center()
                .child(icon("icons/chevron-down.svg", 19.5, white_a(0.4))),
        );

    let banner = store
        .banner(&com.community_id)
        .filter(|_| !store.hidden_banners.contains(&com.community_id))
        .map(|path| {
            div()
                .id("ws-banner")
                .flex_none()
                .border_b_1()
                .border_color(c(0x1f1f1f))
                .on_mouse_down(gpui::MouseButton::Right, {
                    let on = on.clone();
                    move |e, w, cx| on(Act::BannerMenu(e.position), w, cx)
                })
                .child(banner_art(path, width))
        });
    let head = div()
        .flex_none()
        .flex()
        .flex_col()
        .child(head)
        .children(banner);

    let owner = com.is_owner;
    let loose: Vec<&Channel> = com
        .channels
        .iter()
        .filter(|ch| ch.category_id.is_none() && ch.visible())
        .collect();
    let mut sections = Vec::new();
    if !loose.is_empty() {
        sections.push(
            div()
                .pt(px(10.))
                .pb(px(2.))
                .flex()
                .flex_col()
                .gap(px(2.))
                .children(loose.iter().flat_map(|ch| row(ch)))
                .into_any_element(),
        );
    }
    for cat in &com.categories {
        let inside: Vec<&Channel> = com
            .channels
            .iter()
            .filter(|ch| ch.category_id.as_deref() == Some(cat.category_id.as_str()) && ch.visible())
            .collect();
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
                    .on_mouse_down(gpui::MouseButton::Right, {
                        let (on, id) = (on.clone(), cat.category_id.clone());
                        move |e, w, cx| on(Act::CategoryMenu(id.clone(), e.position), w, cx)
                    })
                    .child(
                        crate::kit::spaced(cat.name.to_uppercase(), 0.99)
                            .font_weight(FontWeight::BOLD)
                            .text_size(px(11.))
                            .line_height(px(24.))
                            .text_color(c(LABEL)),
                    )
                    .child(
                        div()
                            .size(px(18.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(icon("icons/chevron-down.svg", 11.7, white_a(0.38)).when(
                                closed,
                                |s| {
                                    s.with_transformation(gpui::Transformation::rotate(
                                        gpui::radians(-std::f32::consts::FRAC_PI_2),
                                    ))
                                },
                            )),
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
                .when(!closed, |d| {
                    d.child(div().pb(px(2.)).flex().flex_col().gap(px(2.)).children(
                        inside.iter().flat_map(|ch| row(ch)),
                    ))
                })
                .into_any_element(),
        );
    }
    div()
        .w(px(width))
        .flex_shrink_0()
        .h_full()
        .flex()
        .flex_col()
        .when(!slim, |d| d.bg(c(LIST_BG)).border_r_1())
        .border_color(c(DIVIDER))
        .child(head)
        .child(
            div()
                .id("community-channels")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .children(sections),
        )
        .into_any_element()
}

/// The conversation header over a channel: its glyph and name, its topic past a hairline,
/// then the menu and the member panel's toggle.
pub fn header(
    ch: &Channel,
    panel: bool,
    probe: Rc<std::cell::Cell<Option<gpui::Bounds<gpui::Pixels>>>>,
    pins: Option<crate::pins::OnAct>,
    on: &OnAct,
) -> impl IntoElement {
    div()
        .h(px(61.))
        .flex_shrink_0()
        .map(|d| crate::theme::chat_header_ground(d, false))
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
                        .child(
                            div()
                                .font_family(RUBIK)
                                .font_weight(FontWeight::BOLD)
                                .text_size(px(18.72))
                                .line_height(px(24.))
                                .text_color(c(TEXT))
                                .child(ch.name.clone()),
                        ),
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
        .children(pins.map(crate::pins::button))
        .child(
            div()
                .id("chat-menu")
                .relative()
                .size(px(40.))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .on_click(click(on, || Act::ChatMenu))
                .child(
                    gpui::canvas(move |b, _, _| probe.set(Some(b)), |_, _, _, _| {})
                        .absolute()
                        .size_full(),
                )
                .child(icon("ui/menu-dots.svg", 28., c(ICON_MUTED))),
        )
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
                .child(icon(
                    if panel {
                        "ui/details-toggle-open.svg"
                    } else {
                        "ui/details-toggle.svg"
                    },
                    22.8,
                    c(ICON_MUTED),
                )),
        )
}

/// The slim layout's channel header: the community's face and `#name` centred over the topic,
/// as a DM's is, with only the channel's menu.
pub fn slim_header(
    store: &Store,
    com: &Community,
    ch: &Channel,
    probe: Rc<std::cell::Cell<Option<gpui::Bounds<gpui::Pixels>>>>,
    on: &OnAct,
) -> impl IntoElement {
    let face = match store.icon(&com.community_id) {
        Some(path) => img(path).size(px(22.)).rounded_full().flex_shrink_0().into_any_element(),
        None => img("icons/group-placeholder.svg").size(px(22.)).rounded_full().flex_shrink_0().into_any_element(),
    };
    let topic = ch.topic.clone().filter(|t| !t.is_empty());
    div()
        .h(px(61.))
        .flex_shrink_0()
        .relative()
        .map(|d| crate::theme::chat_header_ground(d, false))
        .flex()
        .justify_center()
        .child(
            div()
                .id("slim-chat-title")
                .cursor_pointer()
                .on_click(click(on, || Act::Panel))
                .pt(px(if topic.is_some() { 8. } else { 18. }))
                .max_w(gpui::relative(0.6))
                .flex()
                .flex_col()
                .items_center()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .child(face)
                        .child(
                            div()
                                .truncate()
                                .font_family(RUBIK)
                                .font_weight(FontWeight::BOLD)
                                .text_size(px(18.72))
                                .line_height(px(24.))
                                .text_color(c(TEXT))
                                .child(if ch.is_voice() { ch.name.clone() } else { format!("#{}", ch.name) }),
                        ),
                )
                .children(topic.map(|t| {
                    div()
                        .max_w_full()
                        .truncate()
                        .opacity(0.8)
                        .text_size(px(14.))
                        .line_height(px(24.))
                        .text_color(c(TEXT))
                        .child(t)
                })),
        )
        .child(
            div()
                .id("chat-menu")
                .absolute()
                .right(px(10.))
                .top(px(10.))
                .size(px(40.))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .on_click(click(on, || Act::ChatMenu))
                .child(gpui::canvas(move |b, _, _| probe.set(Some(b)), |_, _, _, _| {}).absolute().size_full())
                .child(icon("ui/menu-dots.svg", 28., c(ICON_MUTED))),
        )
}

/// The roster's search: `body.ws`'s slim pill, else the boxed field.
pub fn member_search_look(slim: bool) -> crate::composer::Look {
    crate::composer::Look {
        family: INTER,
        weight: FontWeight::MEDIUM,
        size: px(if slim { 16. } else { 13. }),
        line_height: px(if slim { 20. } else { 16. }),
        color: c(0xffffff),
        placeholder: c(0x757575),
        ..Default::default()
    }
}

fn status_line(text: &str) -> AnyElement {
    let slim = crate::theme::is_slim();
    let look = if slim {
        crate::inline::Look::new(INTER, 12., 16.2, white_a(0.38))
    } else {
        crate::inline::Look::new(INTER, 11.5, 15.5, white_a(0.38))
    };
    crate::inline::inline(crate::preview::atoms(text, usize::MAX, &look), look)
        .single_line()
        .into_any_element()
}

fn member_row(store: &Store, cid: &str, npub: &str, owner: bool, colour: Option<u32>, unlisted: Option<&str>, on: &OnAct, anchors: &crate::kit::Anchors) -> impl IntoElement {
    let status = store
        .profiles
        .get(npub)
        .map(|p| p.status.title.clone())
        .filter(|t| !t.is_empty());
    let key = SharedString::from(format!("roster:{npub}"));
    let (tap, k, n) = (on.clone(), key.clone(), npub.to_string());
    let (menu, m) = (on.clone(), npub.to_string());
    // Phone widths keep the base roster row; `body.ws` tightens it around a larger avatar.
    let slim = crate::theme::is_slim();
    div()
        .id(SharedString::from(format!("member-{npub}")))
        .h(px(if slim { 46. } else { 40. }))
        .rounded(px(6.))
        .flex()
        .items_center()
        .px(px(if slim { 10. } else { 8. }))
        .cursor_pointer()
        .hover(|d| d.bg(white_a(0.045)))
        .on_click(move |_, w, cx| tap(Act::Member(n.clone(), k.clone()), w, cx))
        .on_mouse_down(gpui::MouseButton::Right, move |e, w, cx| {
            cx.stop_propagation();
            menu(Act::MemberMenu(m.clone(), e.position), w, cx)
        })
        .child(
            div()
                .relative()
                .child(crate::ui::avatar(store, npub, if slim { 25. } else { 32. }))
                .child(crate::kit::anchor(anchors, key)),
        )
        .child(
            div()
                .ml(px(if slim { 10. } else { 12. }))
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(4.))
                        .child(
                            div()
                                .truncate()
                                .font_weight(if slim { FontWeight::NORMAL } else { FontWeight::SEMIBOLD })
                                .text_size(px(14.))
                                .line_height(px(if slim { 24. } else { 18. }))
                                .text_color(c(colour.unwrap_or(0xf7f4f4)))
                                .child(store.name_in(npub, Some(cid))),
                        )
                        .when(owner, |d| {
                            d.child(
                                div()
                                    .id("rank")
                                    .ml(px(5.))
                                    .flex()
                                    .tooltip(crate::kit::tooltip("Owner"))
                                    .child(icon("icons/crown.svg", 15., c(OWNER))),
                            )
                        })
                        .when(store.profiles.get(npub).is_some_and(|p| p.bot) && !store.stream_hidden(npub), |d| {
                            d.child(
                                div()
                                    .id("bot")
                                    .flex()
                                    .tooltip(crate::kit::tooltip("Bot"))
                                    .child(svg().path("icons/bot.svg").size(px(14.)).flex_shrink_0().text_color(c(ACCENT))),
                            )
                        })
                        .children(crate::timeline::timeout_mark(store, Some(cid), npub))
                        .children(unlisted.map(|why| {
                            let label = if why == "departed" { "Posting after leaving" } else { "Never announced joining" };
                            let (tap, why) = (on.clone(), why.to_string());
                            div()
                                .id("unlisted")
                                .flex()
                                .p(px(8.))
                                .m(px(-8.))
                                .cursor_pointer()
                                .tooltip(crate::kit::tooltip(label))
                                .on_click(move |_, w, cx| {
                                    cx.stop_propagation();
                                    tap(Act::Unlisted(why.clone()), w, cx)
                                })
                                .child(icon("icons/vector_warning.svg", 14., c(0xfbbf24)))
                        })),
                )
                .children(status.map(|s| {
                    div()
                        .truncate()
                        .text_size(px(if slim { 12. } else { 11.5 }))
                        .line_height(px(if slim { 16.2 } else { 15.5 }))
                        .text_color(white_a(0.38))
                        .child(status_line(&s))
                })),
        )
}

/// The roster's sections, filtered by the search, by rank, or why there are none.
pub fn roster<E: ParentElement>(
    d: E,
    store: &Store,
    open: &str,
    view: &View,
    com: &Community,
    on: &OnAct,
    anchors: &crate::kit::Anchors,
    cx: &App,
) -> E {
    let query = view.search.read(cx).text().trim().to_lowercase();
    let owner = com.owner_npub.as_deref();
    let matches = |n: &&String| {
        query.is_empty()
            || store
                .name_in(n, Some(&com.community_id))
                .to_lowercase()
                .contains(&query)
            || n.contains(&query)
    };
    let is_admin = |n: &str| owner == Some(n) || view.admins.iter().any(|a| a == n);
    let graph = store.role_graphs.get(&com.community_id);
    // A Private Channel lists only who may read it; unscoped until the graph lands.
    let scope = graph
        .zip(com.channel(open).filter(|ch| ch.private))
        .map(|(g, ch)| g.readers(&ch.channel_id.to_ascii_lowercase(), owner));
    let everyone = || {
        view.members
            .iter()
            .chain(view.unlisted.keys().filter(|n| !view.members.contains(n)))
            .filter(|n| scope.as_ref().is_none_or(|s| s.contains(n.as_str())))
    };
    let named = |n: &str| {
        store.profiles.get(n).is_some_and(|p| !p.name.is_empty() || !p.display_name.is_empty())
            || store.nicknames.get(&com.community_id).is_some_and(|k| k.contains_key(n))
    };
    // Owner, then admins, then members; names A to Z and bare keys last, the same in every channel.
    let key = |n: &&String| {
        let tier = if owner == Some(n.as_str()) { 0 } else if is_admin(n) { 1 } else { 2 };
        let unnamed = !store.stream_hidden(n) && !named(n);
        (tier, unnamed, store.name_in(n, Some(&com.community_id)).to_lowercase())
    };
    let mut everyone: Vec<&String> = everyone().filter(matches).collect();
    everyone.sort_by_cached_key(key);
    let mut order: Vec<(String, String)> = Vec::new();
    if let Some(g) = graph {
        let mut heads: Vec<&GraphRole> = g.roles.iter().filter(|r| g.heads(r)).collect();
        heads.sort_by_key(|r| r.position);
        order.extend(heads.into_iter().map(|r| (format!("sec-role:{}", r.id), r.name.clone())));
    }
    if order.is_empty() && !graph.is_some_and(|g| g.activated) {
        order.push(("sec-admin".into(), "Admin".into()));
    }
    order.push(("sec-members".into(), "Members".into()));
    let mut buckets: Vec<Vec<&String>> = vec![Vec::new(); order.len()];
    for n in everyone {
        // A role the graph no longer defines must not drop its holder off the roster.
        let want = match graph.and_then(|g| g.highest(n, |r| g.heads(r))) {
            Some(r) => format!("sec-role:{}", r.id),
            None if is_admin(n) => "sec-admin".into(),
            None => "sec-members".into(),
        };
        let ix = order.iter().position(|(k, _)| *k == want).unwrap_or(order.len() - 1);
        buckets[ix].push(n);
    }
    let colour = |n: &str| graph.filter(|g| g.activated).and_then(|g| g.highest(n, |r| r.color != 0)).map(|r| r.color);
    let section = |key: String, label: String, list: Vec<&String>| {
        let closed = view.folded.contains(&key);
        (!list.is_empty()).then(|| {
            div()
                .child(
                    div()
                        .id(SharedString::from(key.clone()))
                        .h(px(36.))
                        .rounded(px(6.))
                        .flex()
                        .items_start()
                        .pt(px(4.))
                        .px(px(if crate::theme::is_slim() { 6. } else { 8. }))
                        .gap(px(6.))
                        .cursor_pointer()
                        .on_click(click(on, { let key = key.clone(); move || Act::Fold(key.clone()) }))
                        .on_mouse_down(gpui::MouseButton::Right, {
                            let (on, key) = (on.clone(), key.clone());
                            move |e, w, cx| {
                                cx.stop_propagation();
                                on(Act::SectionMenu(key.clone(), e.position), w, cx)
                            }
                        })
                        .child(
                            crate::kit::spaced(label.to_uppercase(), 0.99)
                                .font_weight(FontWeight::BOLD)
                                .text_size(px(11.))
                                .line_height(px(24.))
                                .text_color(c(LABEL)),
                        )
                        .child(
                            div()
                                .flex_1()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_size(px(11.))
                                .line_height(px(24.))
                                .text_color(white_a(0.32))
                                .child(list.iter().filter(|n| !view.unlisted.contains_key(n.as_str())).count().to_string()),
                        )
                        .child(
                            div()
                                .mt(px(8.))
                                .size(px(16.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(icon("icons/chevron-down.svg", 10.4, white_a(0.32)).when(
                                    closed,
                                    |s| {
                                        s.with_transformation(gpui::Transformation::rotate(
                                            gpui::radians(-std::f32::consts::FRAC_PI_2),
                                        ))
                                    },
                                )),
                        ),
                )
                .when(!closed, |d| {
                    d.children(list.into_iter().map(|n| {
                        member_row(store, &com.community_id, n, owner == Some(n.as_str()), colour(n), view.unlisted.get(n.as_str()).map(String::as_str), on, anchors)
                    }))
                })
        })
    };
    let sections: Vec<_> = order
        .into_iter()
        .zip(buckets)
        .filter_map(|((key, label), list)| section(key, label, list))
        .enumerate()
        .map(|(i, s)| s.when(i > 0, |d| d.mt(px(12.))))
        .collect();
    if !sections.is_empty() {
        return d.children(sections);
    }
    let (text, italic) = match (view.loading, query.is_empty()) {
        (true, true) => ("Loading members…", false),
        (_, true) => ("No one has spoken yet. Members appear here once they post.", true),
        _ => ("No matches.", true),
    };
    d.child(
        div()
            .mt(px(16.))
            .p(px(14.))
            .text_center()
            .text_size(if italic { px(16.) } else { px(12.5) })
            .line_height(px(if italic { 24. } else { 18. }))
            .text_color(c(TEXT))
            .opacity(if italic { 0.5 } else { 0.4 })
            .when(italic, |d| d.italic())
            .child(text),
    )
}

/// `#group-overview`: the search over the roster.
pub fn members_panel(
    store: &Store,
    open: &str,
    view: &View,
    com: &Community,
    on: &OnAct,
    anchors: &crate::kit::Anchors,
    cx: &App,
) -> impl IntoElement {
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
            div()
                .h(px(61.))
                .flex_shrink_0()
                .map(|d| crate::theme::chat_header_ground(d, false))
                .flex()
                .items_center()
                .px(px(14.))
                .child(
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
                        .child(
                            icon("icons/search.svg", 16., white_a(0.4))
                                .absolute()
                                .left(px(13.))
                                .top(px(7.)),
                        )
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
                .map(|d| roster(d, store, open, view, com, on, anchors, cx)),
        )
}

/// An author's name colour in a channel: their topmost coloured role, on the new roles.
pub fn author_colour(store: &Store, chat_id: &str, npub: &str) -> Option<u32> {
    let g = store.role_graphs.get(&store.community_of(chat_id)?.community_id)?;
    g.activated.then(|| g.highest(npub, |r| r.color != 0)).flatten().map(|r| r.color)
}

/// The Owner chip after an author's name in a channel; on the new roles the name's colour stands instead.
pub fn author_badge(store: &Store, chat_id: &str, npub: &str) -> Option<AnyElement> {
    let com = store.community_of(chat_id)?;
    if store.role_graphs.get(&com.community_id).is_some_and(|g| g.activated) {
        return None;
    }
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
pub fn invite_row(
    inv: &crate::data::Invite,
    open: Click,
    accept: Click,
    decline: Click,
) -> impl IntoElement {
    let button = |id: String, icon_path: &'static str, tip: &'static str, ink: gpui::Hsla, on: Click| {
        div()
            .id(SharedString::from(id))
            .tooltip(crate::kit::tooltip(tip))
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
        .id(SharedString::from(format!(
            "community-invite-{}",
            inv.community_id
        )))
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
        .child(
            crate::pic::img("icons/group-placeholder.svg")
                .size(px(40.))
                .rounded(px(7.56))
                .flex_shrink_0(),
        )
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
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_size(px(15.))
                                .line_height(px(20.))
                                .text_color(c(0xffffff))
                                .child(inv.name.clone()),
                        )
                        .child(
                            svg()
                                .path("icons/users-multi.svg")
                                .size(px(14.))
                                .flex_shrink_0()
                                .text_color(white_a(0.55)),
                        ),
                )
                .child(
                    div()
                        .mt(px(-4.))
                        .truncate()
                        .text_size(px(13.))
                        .line_height(px(18.))
                        .text_color(white_a(0.85))
                        .child("Community invite"),
                ),
        )
        .child(
            div()
                .ml_auto()
                .flex()
                .gap(px(8.))
                .child(button(
                    format!("invite-accept-{}", inv.community_id),
                    "icons/check.svg",
                    "Accept Invite",
                    c(ACCENT),
                    accept,
                ))
                .child(button(
                    format!("invite-decline-{}", inv.community_id),
                    "icons/x.svg",
                    "Decline Invite",
                    c(crate::timeline::DANGER),
                    decline,
                )),
        )
}

/// `InviteDetails.svelte`: who sent a pending invite, when, and until when.
pub fn invite_details(
    store: &Store,
    inv: &crate::data::Invite,
    close: Click,
    accept: Click,
    decline: Click,
) -> impl IntoElement {
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
    let sent = if inv.sent_at > 0 {
        ("Sent", inv.sent_at)
    } else {
        ("Received", inv.received_at)
    };
    let row = |first: bool, label: &'static str, value: AnyElement| {
        div()
            .h(px(42.))
            .px(px(14.))
            .flex()
            .items_center()
            .justify_between()
            .gap(px(12.))
            .when(!first, |d| {
                d.h(px(43.)).border_t_1().border_color(c(0x1e1e1e))
            })
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
            .map(|d| {
                if primary {
                    d.border_color(gpui::transparent_black())
                        .bg(c(ACCENT))
                        .text_color(c(0x000000))
                } else {
                    d.border_color(c(crate::timeline::DANGER))
                        .text_color(c(0xffffff))
                }
            })
            .on_click(on)
            .child(label)
    };
    let card = div()
        .id("invite-details")
        .w_full()
        .max_w(px(422.))
        .p(px(21.))
        .rounded(px(16.))
        .border_1()
        .border_color(c(0x262626))
        .bg(ca(20, 20, 20, 0.95))
        .shadow(vec![gpui::BoxShadow {
            color: ca(0, 0, 0, 0.4),
            offset: gpui::point(px(0.), px(8.)),
            blur_radius: px(32.),
            spread_radius: px(0.),
            inset: false,
        }])
        .occlude()
        .on_click(|_, _, cx| cx.stop_propagation())
        .flex()
        .flex_col()
        .child(
            div()
                .flex()
                .flex_col()
                .items_center()
                .child(
                    img("icons/group-placeholder.svg")
                        .size(px(72.))
                        .rounded_full(),
                )
                .child(
                    div()
                        .mt(px(12.))
                        .max_w_full()
                        .truncate()
                        .font_weight(FontWeight::BOLD)
                        .text_size(px(20.))
                        .line_height(px(24.))
                        .text_color(c(0xffffff))
                        .child(inv.name.clone()),
                )
                .child(
                    div()
                        .mt(px(4.))
                        .text_size(px(13.))
                        .line_height(px(24.))
                        .text_color(white_a(0.5))
                        .child("Community invite"),
                ),
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
                .child(row(
                    false,
                    sent.0,
                    div()
                        .child(crate::util::relative_time(sent.1, now))
                        .into_any_element(),
                ))
                .child(row(
                    false,
                    "Expires",
                    div()
                        .child(crate::util::relative_time(inv.ends_at, now))
                        .into_any_element(),
                )),
        )
        .child(
            div()
                .mt(px(25.))
                .flex()
                .gap(px(10.))
                .child(button("invite-decline", "Decline", false, decline))
                .child(button("invite-accept", "Accept", true, accept)),
        );
    gpui::deferred(
        div()
            .id("invite-details-overlay")
            .absolute()
            .inset_0()
            .px(px(19.))
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

#[cfg(test)]
mod tests {
    use super::Graph;

    fn graph(activated: bool) -> Graph {
        Graph::parse(&serde_json::json!({
            "activated": activated,
            "roles": [
                {"role_id": "core", "name": "Core", "position": 1, "color": 0x59fcb3, "channel_id": null, "display": true},
                {"role_id": "art", "name": "Art", "position": 3, "color": 0xffb35c, "channel_id": "ABCD", "display": false},
                {"role_id": "mod", "name": "Mods", "position": 2, "color": 0, "channel_id": null, "display": true},
            ],
            "grants": [
                {"npub": "a", "role_ids": ["art", "core"]},
                {"npub": "b", "role_ids": ["mod", "art"]},
                {"npub": "c", "role_ids": ["art"]},
            ],
            "readers": {"ABCD": ["b"]},
        }))
    }

    #[test]
    fn highest_role_picks_the_lowest_position() {
        let g = graph(true);
        assert_eq!(g.highest("a", |r| g.heads(r)).map(|r| r.id.as_str()), Some("core"));
        assert_eq!(g.highest("b", |r| g.heads(r)).map(|r| r.id.as_str()), Some("mod"));
        assert!(g.highest("c", |r| g.heads(r)).is_none());
        // A colourless role passes its colour down to the next one.
        assert_eq!(g.highest("b", |r| r.color != 0).map(|r| r.color), Some(0xffb35c));
    }

    #[test]
    fn private_readers_follow_the_roles_in_use() {
        let owner = Some("o");
        let new = graph(true).readers("abcd", owner);
        assert_eq!(new, ["o", "b"].iter().map(|s| s.to_string()).collect());
        let old = graph(false).readers("abcd", owner);
        assert_eq!(old, ["o", "a", "b", "c"].iter().map(|s| s.to_string()).collect());
        // The old roles head a section with every role, shown separately or not.
        let g = graph(false);
        assert_eq!(g.highest("c", |r| g.heads(r)).map(|r| r.id.as_str()), Some("art"));
    }
}
