//! Community Settings → Roles (`RolesSection.svelte`): the list in display order, dragged to
//! reorder, and the editor for one role: its look, its permissions and its members. Order and
//! edits are drafts under the save bar; membership and deletion act at once.

use std::collections::HashSet;

use gpui::prelude::*;
use gpui::{AnyElement, App, FontWeight, SharedString, Window, div, px, svg};
use serde_json::Value;

use crate::commsettings::{
    Act as CsAct, CommSettings, OnAct, block, click, field_head, heading, hint, indicator,
    input_box, lede, member_row, save_btn, search_box, short_npub, text_btn,
};
use crate::data::Store;
use crate::theme::*;

pub const NAME_MAX: usize = 64;
const DEFAULT_TINT: u32 = 0x99aab5;

/// Vector's palette, two rows of eight: brights, then warm and muted.
pub const COLORS: [u32; 16] = [
    0x59fcb3, 0x4ade80, 0x38bdf8, 0x60a5fa, 0x818cf8, 0xa78bfa, 0xe879c9, 0xff2ea9, 0xfb7185,
    0xf87171, 0xfb923c, 0xfbbf24, 0xa3a380, 0x5eead4, 0x94a3b8, 0x64748b,
];

pub struct Perm {
    pub bit: u32,
    pub name: &'static str,
    pub desc: &'static str,
    pub v2: bool,
}

const fn p(bit: u32, name: &'static str, desc: &'static str) -> Perm {
    Perm {
        bit,
        name,
        desc,
        v2: false,
    }
}

const fn p2(bit: u32, name: &'static str, desc: &'static str) -> Perm {
    Perm {
        bit,
        name,
        desc,
        v2: true,
    }
}

pub const COMMUNITY: &[Perm] = &[
    p(
        2,
        "Manage Community",
        "Change the community's name, icon and description.",
    ),
    p(1, "Manage Channels", "Create, rename and delete channels."),
    p(
        0,
        "Manage Roles",
        "Create and edit roles beneath their own, and give them to members.",
    ),
    p(
        6,
        "Create Invites",
        "Make invite links and invite people directly.",
    ),
    p(
        8,
        "View Audit Log",
        "See the record of moderation and management actions.",
    ),
];
pub const MODERATION: &[Perm] = &[
    p(
        3,
        "Kick Members",
        "Remove members beneath them. A kicked member can rejoin.",
    ),
    p(
        4,
        "Ban Members",
        "Ban members beneath them, silencing them and cutting them off for good.",
    ),
    p(
        5,
        "Manage Messages",
        "Hide messages from members beneath them.",
    ),
    p(
        11,
        "Pin Messages",
        "Pin messages to a channel for everyone to see.",
    ),
    p2(
        26,
        "Timeout Members",
        "Silence members beneath them for a while.",
    ),
    p(
        28,
        "Manage Nicknames",
        "Change the nicknames of members beneath them.",
    ),
];
pub const CHANNELS: &[Perm] = &[
    p(
        13,
        "View Channels",
        "See channels and read what is posted in them.",
    ),
    p(14, "Send Messages", "Post in channels."),
    p(15, "Send in Threads", "Reply in threads."),
    p(16, "Attach Files", "Share images, videos and files."),
    p(17, "Add Reactions", "React to messages."),
    p(
        18,
        "Read Message History",
        "Read what was posted before they could see a channel.",
    ),
];
pub const VOICE: &[Perm] = &[
    p(19, "Connect", "Join calls."),
    p(20, "Speak", "Talk in calls."),
    p(21, "Video", "Share their camera or screen in calls."),
    p(
        22,
        "Priority Speaker",
        "Be heard over others while they talk.",
    ),
    p(23, "Mute Members", "Mute members beneath them in calls."),
    p(
        24,
        "Deafen Members",
        "Deafen members beneath them in calls.",
    ),
    p(
        25,
        "Move Members",
        "Move members beneath them between calls.",
    ),
];
pub const CHAT: &[Perm] = &[
    p(
        9,
        "Mention @everyone",
        "Notify everyone at once, and mention any role.",
    ),
    p2(
        29,
        "Change Nickname",
        "Choose what they go by in this community.",
    ),
];

pub fn groups(v2: bool) -> Vec<(&'static str, &'static [Perm])> {
    if v2 {
        vec![
            ("Community", COMMUNITY),
            ("Moderation", MODERATION),
            ("Channels", CHANNELS),
            ("Voice", VOICE),
            ("Chat", CHAT),
        ]
    } else {
        vec![
            ("Community", COMMUNITY),
            ("Moderation", MODERATION),
            ("Chat", CHAT),
        ]
    }
}

#[derive(Clone, PartialEq, Default)]
pub struct Draft {
    pub name: String,
    pub color: u32,
    pub permissions: u64,
    pub channel_id: Option<String>,
    pub display: bool,
    pub mentionable: bool,
}

/// The role open in the editor: `id` None is one being made.
pub struct Edit {
    pub id: Option<String>,
    pub everyone: bool,
    pub draft: Draft,
    pub base: Draft,
}

#[derive(Default)]
pub struct Roles {
    pub loaded: bool,
    pub view: Option<Value>,
    pub order: Option<Vec<String>>,
    pub edit: Option<Edit>,
    pub members: Vec<String>,
    pub busy: bool,
    pub adding: bool,
    pub pick: HashSet<String>,
    pub access_open: bool,
    pub custom: bool,
}

pub enum Act {
    Open(String),
    New,
    Everyone,
    Back,
    Color(u32),
    Custom,
    Perm(u32),
    Display,
    Mentionable,
    Move {
        id: String,
        to: String,
        before: bool,
    },
    Adding(bool),
    Pick(String),
    AddPicked,
    Remove(String),
    Delete,
    Activate,
    AccessOpen,
    Access(&'static str),
}

pub fn num(v: &Value) -> u64 {
    v.as_str()
        .and_then(|s| s.parse().ok())
        .or_else(|| v.as_u64())
        .unwrap_or(0)
}

pub fn role_of(v: &Value) -> Draft {
    Draft {
        name: v["name"].as_str().unwrap_or("").to_string(),
        color: v["color"].as_u64().unwrap_or(0) as u32,
        permissions: num(&v["permissions"]),
        channel_id: v["channel_id"].as_str().map(String::from),
        display: v["display"].as_bool().unwrap_or(false),
        mentionable: v["mentionable"].as_bool().unwrap_or(false),
    }
}

impl Roles {
    pub fn v2(&self) -> bool {
        self.view
            .as_ref()
            .is_some_and(|v| v["activated"].as_bool().unwrap_or(false))
    }

    pub fn find(&self, id: &str) -> Option<&Value> {
        self.view.as_ref()?["roles"]
            .as_array()?
            .iter()
            .find(|r| r["role_id"].as_str() == Some(id))
    }

    /// The list as the draft orders it, or as published.
    pub fn rows(&self) -> Vec<&Value> {
        let Some(all) = self.view.as_ref().and_then(|v| v["roles"].as_array()) else {
            return Vec::new();
        };
        match &self.order {
            Some(order) => order.iter().filter_map(|id| self.find(id)).collect(),
            None => all.iter().collect(),
        }
    }

    pub fn published_order(&self) -> Vec<String> {
        self.view
            .as_ref()
            .and_then(|v| v["roles"].as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|r| r["role_id"].as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn edit_dirty(&self, name: &str) -> bool {
        self.edit.as_ref().is_some_and(|e| {
            e.id.is_none()
                || Draft {
                    name: name.to_string(),
                    ..e.draft.clone()
                } != e.base
        })
    }

    pub fn editable(&self) -> bool {
        let (Some(e), Some(v)) = (&self.edit, &self.view) else {
            return false;
        };
        if e.everyone {
            v["everyone_editable"].as_bool().unwrap_or(false)
        } else if let Some(id) = &e.id {
            self.find(id)
                .is_some_and(|r| r["manageable"].as_bool().unwrap_or(false))
        } else {
            v["can_manage"].as_bool().unwrap_or(false)
        }
    }

    /// A bit you lack can't be added, but one the role already had may be kept or removed.
    pub fn can_toggle(&self, bit: u64) -> bool {
        let Some(e) = &self.edit else { return false };
        let grantable = self
            .view
            .as_ref()
            .map(|v| num(&v["grantable"]))
            .unwrap_or(0);
        self.editable()
            && (e.draft.permissions & bit == bit
                || grantable & bit == bit
                || e.base.permissions & bit == bit)
    }

    pub fn holders(&self) -> Vec<String> {
        self.edit
            .as_ref()
            .and_then(|e| e.id.as_deref())
            .and_then(|id| self.find(id))
            .and_then(|r| r["holders"].as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|h| h.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    }
}

fn tint(color: u32) -> gpui::Hsla {
    c(if color == 0 { DEFAULT_TINT } else { color })
}

fn dot(color: gpui::Hsla) -> impl IntoElement {
    div()
        .size(px(12.))
        .flex_shrink_0()
        .rounded_full()
        .bg(color)
        .border_1()
        .border_color(white_a(0.15))
}

#[derive(Clone)]
pub struct RoleDrag {
    id: String,
    name: SharedString,
    color: u32,
}

struct Ghost(SharedString, u32);

impl Render for Ghost {
    fn render(&mut self, _: &mut Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
        div()
            .w(px(360.))
            .h(px(46.))
            .px(px(14.))
            .flex()
            .items_center()
            .gap(px(12.))
            .rounded(px(10.))
            .bg(c(0x161616))
            .opacity(0.92)
            .shadow(vec![gpui::BoxShadow {
                color: ca(0, 0, 0, 0.55),
                offset: gpui::point(px(0.), px(8.)),
                blur_radius: px(20.),
                spread_radius: px(0.),
                inset: false,
            }])
            .font_family(RUBIK)
            .child(dot(tint(self.1)))
            .child(
                div()
                    .text_size(px(14.))
                    .text_color(c(0xe8e8e8))
                    .child(self.0.clone()),
            )
    }
}

fn row_shell(id: SharedString, first: bool) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .relative()
        .h(px(if first { 46. } else { 47. }))
        .pl(px(8.))
        .pr(px(14.))
        .flex()
        .items_center()
        .gap(px(12.))
        .bg(c(0x0c0c0c))
        .when(!first, |d| d.border_t_1().border_color(c(0x1e1e1e)))
}

fn grip(icon: &'static str, color: u32) -> impl IntoElement {
    div()
        .size(px(18.))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .child(svg().path(icon).size(px(16.)).text_color(c(color)))
}

fn count(text: String) -> impl IntoElement {
    div()
        .ml_auto()
        .flex_shrink_0()
        .text_size(px(12.5))
        .line_height(px(24.))
        .text_color(c(0x6f6f6f))
        .child(text)
}

fn open_caret() -> impl IntoElement {
    div()
        .size(px(16.))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .child(
            svg()
                .path("icons/chevron-right.svg")
                .size(px(10.))
                .text_color(c(0x555555)),
        )
}

fn list(m: &CommSettings, store: &Store, on: &OnAct) -> AnyElement {
    let r = &m.roles;
    let Some(view) = r.view.as_ref() else {
        return block(
            true,
            div().child(heading("Roles")).child(hint("Loading...")),
        )
        .into_any_element();
    };
    let v2 = r.v2();
    let max = view["max_roles"].as_u64().unwrap_or(100) as usize;
    let total = view["roles"].as_array().map(Vec::len).unwrap_or(0);
    let can_manage = view["can_manage"].as_bool().unwrap_or(false);
    let channel_name = |id: &str| {
        view["channels"]
            .as_array()
            .and_then(|a| a.iter().find(|c| c["channel_id"].as_str() == Some(id)))
            .and_then(|c| c["name"].as_str())
            .unwrap_or("a private channel")
            .to_string()
    };
    let mut rows = div()
        .mb(px(10.))
        .rounded(px(12.))
        .border_1()
        .border_color(c(0x262626))
        .overflow_hidden();
    let mut first = true;
    let owner = store
        .community(&m.community)
        .and_then(|c| c.owner_npub.clone());
    if let Some(owner) = owner {
        rows = rows.child(
            row_shell("role-owner".into(), true)
                .child(grip("icons/locked.svg", 0x444444))
                .child(
                    div()
                        .size(px(14.))
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            svg()
                                .path("icons/crown.svg")
                                .size(px(10.))
                                .text_color(c(0xfce459)),
                        ),
                )
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .font_weight(FontWeight::MEDIUM)
                        .text_size(px(14.))
                        .line_height(px(24.))
                        .text_color(c(0xfce459))
                        .child("Owner"),
                )
                .child(
                    div()
                        .ml_auto()
                        .max_w(gpui::relative(0.4))
                        .truncate()
                        .text_size(px(12.5))
                        .line_height(px(24.))
                        .text_color(c(0x6f6f6f))
                        .child(if store.profiles.contains_key(owner.as_str()) {
                            store.name(&owner)
                        } else {
                            short_npub(&owner)
                        }),
                )
                .child(div().size(px(16.)).flex_shrink_0()),
        );
        first = false;
    }
    let all = r.rows();
    if all.is_empty() && !v2 {
        rows = rows.child(
            div()
                .py(px(14.))
                .text_center()
                .text_size(px(12.5))
                .text_color(white_a(0.4))
                .child("No roles yet. Create one to start giving members permissions."),
        );
    }
    for role in all {
        let id = role["role_id"].as_str().unwrap_or("").to_string();
        let manageable = role["manageable"].as_bool().unwrap_or(false);
        let color = role["color"].as_u64().unwrap_or(0) as u32;
        let name = role["name"]
            .as_str()
            .filter(|n| !n.is_empty())
            .unwrap_or("Unnamed role")
            .to_string();
        let held = role["holders"].as_array().map(Vec::len).unwrap_or(0);
        let open = on.clone();
        let key = id.clone();
        let mut row = row_shell(SharedString::from(format!("role-{id}")), first)
            .cursor_pointer()
            .hover(|d| d.bg(c(0x131313)))
            .on_click(move |_, w, cx| open(CsAct::Role(Act::Open(key.clone())), w, cx))
            .child(grip(
                if manageable {
                    "icons/grip.svg"
                } else {
                    "icons/locked.svg"
                },
                if manageable { 0x555555 } else { 0x444444 },
            ))
            .child(dot(tint(color)))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_size(px(14.))
                    .line_height(px(24.))
                    .text_color(if manageable { c(0xe8e8e8) } else { c(0x9a9a9a) })
                    .child(name.clone()),
            )
            .children(role["channel_id"].as_str().map(|ch| {
                div()
                    .min_w_0()
                    .truncate()
                    .px(px(8.))
                    .py(px(1.))
                    .rounded(px(9.))
                    .bg(c(0x1b1b1b))
                    .text_size(px(12.))
                    .text_color(c(0x9a9a9a))
                    .child(format!("#{}", channel_name(ch)))
            }))
            .child(count(format!(
                "{held} {}",
                if held == 1 { "member" } else { "members" }
            )))
            .child(open_caret());
        if manageable {
            let drag = RoleDrag {
                id: id.clone(),
                name: name.into(),
                color,
            };
            row = row.on_drag(drag, |d: &RoleDrag, _, _, cx| {
                cx.new(|_| Ghost(d.name.clone(), d.color))
            });
            for before in [true, false] {
                let on = on.clone();
                let to = id.clone();
                let zone = div()
                    .id(SharedString::from(format!("role-{id}-{before}")))
                    .absolute()
                    .left_0()
                    .right_0()
                    .h(gpui::relative(0.5))
                    .when(before, |d| d.top_0())
                    .when(!before, |d| d.bottom_0())
                    .drag_over::<RoleDrag>(move |d, _, _, _| {
                        if before {
                            d.border_t_2().border_color(c(PRIMARY))
                        } else {
                            d.border_b_2().border_color(c(PRIMARY))
                        }
                    })
                    .on_drop(move |d: &RoleDrag, w, cx| {
                        if d.id != to {
                            on(
                                CsAct::Role(Act::Move {
                                    id: d.id.clone(),
                                    to: to.clone(),
                                    before,
                                }),
                                w,
                                cx,
                            );
                        }
                    });
                row = row.child(zone);
            }
        }
        rows = rows.child(row);
        first = false;
    }
    if v2 {
        let editable = view["everyone_editable"].as_bool().unwrap_or(false);
        rows = rows.child(
            row_shell("role-everyone".into(), first)
                .cursor_pointer()
                .hover(|d| d.bg(c(0x131313)))
                .on_click(click(on, || CsAct::Role(Act::Everyone)))
                .child(grip("icons/locked.svg", 0x444444))
                .child(dot(c(DEFAULT_TINT)))
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_size(px(14.))
                        .line_height(px(24.))
                        .text_color(if editable { c(0xcfcfcf) } else { c(0x9a9a9a) })
                        .child("@everyone"),
                )
                .child(count("all members".into()))
                .child(open_caret()),
        );
    }
    let upgrade = (!v2 && view["is_owner"].as_bool().unwrap_or(false)).then(|| {
        div()
            .mb(px(14.))
            .px(px(14.))
            .py(px(12.))
            .flex()
            .items_center()
            .gap(px(14.))
            .rounded(px(12.))
            .border_1()
            .border_color(c(ACCENT).opacity(0.35))
            .bg(c(0x0f1915))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(div().text_size(px(14.5)).text_color(c(0xe8e8e8)).child("New roles are available"))
                    .child(div().text_size(px(12.5)).line_height(px(17.5)).text_color(c(0x7a7a7a)).child("@everyone, channel and voice permissions, and roles you can delete. Every role carries over, and private channels keep who can read them.")),
            )
            .child(save_btn("role-switch", "Switch", !r.busy, click(on, || CsAct::Role(Act::Activate))))
    });
    block(
        true,
        div()
            .child(
                div()
                    .mb(px(20.))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(12.))
                    .child(div().text_size(px(24.)).line_height(px(28.)).font_weight(FontWeight::BOLD).text_color(c(0xffffff)).child("Roles"))
                    .when(can_manage && total < max, |d| d.child(small_btn("role-create", "Create Role", true, click(on, || CsAct::Role(Act::New))))),
            )
            .child(lede("Members get the permissions of every role they hold. A role can manage only the roles beneath it, so the order matters: drag to arrange."))
            .children(upgrade)
            .child(rows)
            .child(hint(format!("{total} of {max} roles"))),
    )
    .into_any_element()
}

fn small_btn(
    id: &'static str,
    label: impl Into<SharedString>,
    live: bool,
    on: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    div()
        .id(id)
        .h(px(34.))
        .px(px(14.))
        .flex_shrink_0()
        .rounded(px(8.))
        .bg(c(PRIMARY))
        .flex()
        .items_center()
        .font_weight(FontWeight::SEMIBOLD)
        .text_size(px(13.))
        .text_color(c(0x000000))
        .when(!live, |d| d.opacity(0.45))
        .when(live, |d| {
            d.cursor_pointer().hover(|d| d.opacity(0.92)).on_click(on)
        })
        .child(label.into())
        .into_any_element()
}

fn perm_row(
    id: SharedString,
    name: &'static str,
    desc: &'static str,
    on_: bool,
    live: bool,
    on: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .py(px(12.))
        .flex()
        .items_center()
        .gap(px(16.))
        .border_b_1()
        .border_color(c(0x1a1a1a))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(3.))
                .child(
                    div()
                        .text_size(px(14.5))
                        .line_height(px(24.))
                        .text_color(c(0xe8e8e8))
                        .child(name),
                )
                .child(
                    div()
                        .text_size(px(12.5))
                        .line_height(px(17.5))
                        .text_color(c(0x7a7a7a))
                        .child(desc),
                ),
        )
        .child(div().flex_shrink_0().when(!live, |d| d.opacity(0.4)).child(
            crate::settings::toggle(id, on_, false, move |e, w, cx| {
                if live {
                    on(e, w, cx)
                }
            }),
        ))
}

fn swatch(
    id: SharedString,
    bg: gpui::Hsla,
    on_: bool,
    live: bool,
    f: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .size(px(30.))
        .flex_shrink_0()
        .rounded(px(8.))
        .bg(bg)
        .when(on_, |d| {
            d.border_2()
                .border_color(c(0x050505))
                .shadow(vec![gpui::BoxShadow {
                    color: c(0xffffff),
                    offset: gpui::point(px(0.), px(0.)),
                    blur_radius: px(0.),
                    spread_radius: px(2.),
                    inset: false,
                }])
        })
        .when(!live, |d| d.opacity(0.5))
        .when(live, |d| d.cursor_pointer().on_click(f))
}

fn editor(m: &CommSettings, store: &Store, on: &OnAct, cx: &App) -> AnyElement {
    let r = &m.roles;
    let (Some(e), Some(view)) = (&r.edit, &r.view) else {
        return div().into_any_element();
    };
    let editable = r.editable();
    let v2 = r.v2();
    let channel_name = |id: &str| {
        view["channels"]
            .as_array()
            .and_then(|a| a.iter().find(|c| c["channel_id"].as_str() == Some(id)))
            .and_then(|c| c["name"].as_str())
            .unwrap_or("a private channel")
            .to_string()
    };
    let back = div()
        .id("role-back")
        .mt(px(22.))
        .mb(px(-8.))
        .pl(px(4.))
        .pr(px(10.))
        .py(px(6.))
        .flex()
        .items_center()
        .gap(px(6.))
        .w_auto()
        .rounded(px(8.))
        .cursor_pointer()
        .font_weight(FontWeight::SEMIBOLD)
        .text_size(px(13.))
        .line_height(px(24.))
        .text_color(c(0x9a9a9a))
        .hover(|d| d.text_color(c(0xffffff)).bg(c(0x151515)))
        .on_click(click(on, || CsAct::Role(Act::Back)))
        .child(
            svg()
                .path("icons/chevron-left.svg")
                .size(px(16.))
                .text_color(c(0x9a9a9a)),
        )
        .child("All Roles");
    let mut out = div().child(div().flex().child(back));
    if e.everyone {
        out = out.child(block(true, div().child(heading("@everyone")).child(lede("What every member holds before any role. A role can add to it, never take from it."))));
    } else {
        let name = m.role_name.read(cx).text().to_string();
        let bytes = name.len();
        let custom = e.draft.color != 0 && !COLORS.contains(&e.draft.color);
        let pick = |color: u32| {
            let on = on.clone();
            move |_: &gpui::ClickEvent, w: &mut Window, cx: &mut App| {
                on(CsAct::Role(Act::Color(color)), w, cx)
            }
        };
        let specials = div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .pr(px(12.))
            .border_r_1()
            .border_color(c(0x1f1f1f))
            .child(
                swatch(
                    "role-sw-default".into(),
                    c(DEFAULT_TINT),
                    e.draft.color == 0,
                    editable,
                    pick(0),
                )
                .overflow_hidden()
                .child(
                    gpui::canvas(
                        |_, _, _| (),
                        |b, _, window, _| {
                            let mut path = gpui::PathBuilder::stroke(px(2.6));
                            path.move_to(b.origin + gpui::point(px(30.), px(0.)));
                            path.line_to(b.origin + gpui::point(px(0.), px(30.)));
                            if let Ok(p) = path.build() {
                                window.paint_path(p, c(0x050505));
                            }
                        },
                    )
                    .size_full(),
                ),
            )
            .child(
                swatch(
                    "role-sw-custom".into(),
                    if custom {
                        c(e.draft.color)
                    } else {
                        c(0x1b1b1b)
                    },
                    custom,
                    editable,
                    click(on, || CsAct::Role(Act::Custom)),
                )
                .when(!custom, |d| d.border_1().border_color(c(0x333333)))
                .flex()
                .items_center()
                .justify_center()
                .child(
                    svg()
                        .path("icons/palette.svg")
                        .size(px(16.))
                        .text_color(c(0xffffff)),
                ),
            );
        let grid = div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .children(COLORS.chunks(8).map(|row| {
                div().flex().gap(px(8.)).children(row.iter().map(|&color| {
                    swatch(
                        SharedString::from(format!("role-sw-{color:06x}")),
                        c(color),
                        e.draft.color == color,
                        editable,
                        pick(color),
                    )
                }))
            }));
        let preview = div()
            .mt(px(14.))
            .px(px(12.))
            .py(px(6.))
            .flex()
            .items_center()
            .gap(px(8.))
            .rounded(px(8.))
            .bg(c(0x111111))
            .font_weight(FontWeight::MEDIUM)
            .text_size(px(14.))
            .line_height(px(23.))
            .text_color(if e.draft.color == 0 {
                c(0xe8e8e8)
            } else {
                c(e.draft.color)
            })
            .child(dot(tint(e.draft.color)))
            .child(if name.trim().is_empty() {
                "new role".to_string()
            } else {
                name.trim().to_string()
            });
        let mut display = div()
            .child(heading(if e.id.is_none() {
                "New Role"
            } else {
                "Display"
            }))
            .when(!editable, |d| {
                d.child(lede(
                    "This role sits at or above your own, so you can view it but not change it.",
                ))
            })
            .child(field_head(
                "Role Name",
                Some((bytes, NAME_MAX, bytes > NAME_MAX || name.trim().is_empty())),
            ))
            .child(input_box(&m.role_name, 42., editable))
            .child(
                div()
                    .mt(px(22.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_size(px(13.))
                    .line_height(px(24.))
                    .text_color(c(0xcfcfcf))
                    .child("Role Colour"),
            )
            .child(
                div()
                    .mt(px(10.))
                    .flex()
                    .gap(px(12.))
                    .child(specials)
                    .child(grid),
            )
            .when_some(m.role_hex.as_ref().filter(|_| r.custom), |d, input| {
                d.child(
                    div()
                        .mt(px(10.))
                        .w(px(160.))
                        .child(input_box(input, 35., editable).px(px(12.))),
                )
            })
            .child(div().flex().child(preview));
        if v2 {
            display = display
                .child(div().mt(px(22.)).child(perm_row(
                    "role-display".into(),
                    "Show members separately",
                    "List its members under the role's name in the member list.",
                    e.draft.display,
                    editable,
                    click(on, || CsAct::Role(Act::Display)),
                )))
                .child(perm_row(
                    "role-mention".into(),
                    "Allow anyone to mention this role",
                    "Otherwise only members who can mention @everyone notify it.",
                    e.draft.mentionable,
                    editable,
                    click(on, || CsAct::Role(Act::Mentionable)),
                ));
        } else {
            display = display.child(
                div()
                    .mt(px(22.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_size(px(13.))
                    .line_height(px(24.))
                    .text_color(c(0xcfcfcf))
                    .child("Access"),
            );
            if e.id.is_none() {
                let choices: Vec<crate::kit::Choice> = std::iter::once(crate::kit::Choice {
                    value: "",
                    label: "Every channel",
                })
                .chain(
                    view["channels"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter(|c| c["private"].as_bool().unwrap_or(false))
                        .filter_map(|c| {
                            let id: &'static str =
                                Box::leak(c["channel_id"].as_str()?.to_string().into_boxed_str());
                            let label: &'static str = Box::leak(
                                format!("#{} only", c["name"].as_str().unwrap_or(""))
                                    .into_boxed_str(),
                            );
                            Some(crate::kit::Choice { value: id, label })
                        }),
                )
                .collect();
                let (t, pk) = (on.clone(), on.clone());
                display = display
                    .child(div().mt(px(10.)).child(crate::kit::select("role-access", 400., &choices, e.draft.channel_id.as_deref().unwrap_or(""), r.access_open, move |w, cx| t(CsAct::Role(Act::AccessOpen), w, cx), move |v, w, cx| pk(CsAct::Role(Act::Access(v)), w, cx))))
                    .child(hint("A role scoped to a private channel is that channel's access list: holding it is what lets someone read there. This can't be changed once the role exists.").mt(px(10.)));
            } else {
                display = display.child(
                    hint(match &e.draft.channel_id {
                        Some(ch) => format!(
                            "#{} only. Holding this role is what lets someone read that channel.",
                            channel_name(ch)
                        ),
                        None => "Every channel.".into(),
                    })
                    .mt(px(2.)),
                );
            }
        }
        out = out.child(block(true, display));
    }
    let mut perms = div()
        .child(heading("Permissions"))
        .child(lede("You can only turn on permissions you hold yourself."));
    for (i, (title, list)) in groups(v2).into_iter().enumerate() {
        perms = perms.child(
            crate::kit::spaced(title.to_uppercase(), 0.66)
                .mt(px(if i == 0 { 4. } else { 22. }))
                .mb(px(10.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_size(px(11.))
                .line_height(px(13.))
                .text_color(c(0x6f6f6f)),
        );
        for p in list.iter().filter(|p| v2 || !p.v2) {
            let bit = 1u64 << p.bit;
            let has = e.draft.permissions & bit == bit;
            let b = p.bit;
            perms = perms.child(perm_row(
                SharedString::from(format!("role-perm-{}", p.bit)),
                p.name,
                p.desc,
                has,
                r.can_toggle(bit),
                click(on, move || CsAct::Role(Act::Perm(b))),
            ));
        }
    }
    out = out.child(block(false, perms));
    if let Some(id) = e.id.as_ref().filter(|_| !e.everyone) {
        let q = m.role_query.read(cx).text().trim().to_lowercase();
        let matches = |n: &String| {
            q.is_empty()
                || format!("{} {}", store.name(n), n)
                    .to_lowercase()
                    .contains(&q)
        };
        let holders = r.holders();
        let mut body = div()
            .mt(px(6.))
            .mb(px(12.))
            .when(r.busy, |d| d.opacity(0.55));
        if r.adding {
            let held: HashSet<&String> = holders.iter().collect();
            let candidates: Vec<&String> = r
                .members
                .iter()
                .filter(|n| !held.contains(n))
                .filter(|n| matches(n))
                .collect();
            if candidates.is_empty() {
                body = body.child(empty(if q.is_empty() {
                    "Everyone already holds this role."
                } else {
                    "No matches."
                }));
            }
            for npub in candidates {
                let key = npub.clone();
                body = body.child(member_row(
                    store,
                    npub,
                    "role-pick",
                    indicator(r.pick.contains(npub)).into_any_element(),
                    Some(Box::new(click(on, move || {
                        CsAct::Role(Act::Pick(key.clone()))
                    }))),
                ));
            }
            let n = r.pick.len();
            body = body.child(
                div()
                    .mt(px(10.))
                    .flex()
                    .justify_end()
                    .gap(px(8.))
                    .child(text_btn(
                        "role-add-cancel",
                        "Cancel",
                        true,
                        click(on, || CsAct::Role(Act::Adding(false))),
                    ))
                    .child(save_btn(
                        "role-add",
                        if r.busy {
                            "Adding...".to_string()
                        } else if n == 0 {
                            "Add".to_string()
                        } else {
                            format!("Add {n}")
                        },
                        n > 0 && !r.busy,
                        click(on, || CsAct::Role(Act::AddPicked)),
                    )),
            );
        } else {
            let shown: Vec<&String> = holders.iter().filter(|n| matches(n)).collect();
            if shown.is_empty() {
                body = body.child(empty(if q.is_empty() {
                    "Nobody holds this role yet."
                } else {
                    "No matches."
                }));
            }
            for npub in shown {
                let key = npub.clone();
                let group = SharedString::from(format!("role-held-{npub}"));
                let remove: AnyElement = if editable {
                    div()
                        .id(SharedString::from(format!("role-rm-{npub}")))
                        .tooltip(crate::kit::tooltip("Remove from role"))
                        .ml_auto()
                        .size(px(28.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(8.))
                        .cursor_pointer()
                        .invisible()
                        .group_hover(group, |d| d.visible())
                        .hover(|d| d.bg(c(0x1e1e1e)))
                        .on_click(click(on, move || CsAct::Role(Act::Remove(key.clone()))))
                        .child(
                            svg()
                                .path("icons/x.svg")
                                .size(px(14.))
                                .text_color(c(0x9a9a9a)),
                        )
                        .into_any_element()
                } else {
                    div().into_any_element()
                };
                body = body.child(member_row(store, npub, "role-held", remove, None));
            }
        }
        let _ = id;
        out = out.child(block(
            false,
            div()
                .child(
                    div()
                        .mb(px(20.))
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap(px(12.))
                        .child(
                            div()
                                .text_size(px(24.))
                                .line_height(px(28.))
                                .font_weight(FontWeight::BOLD)
                                .text_color(c(0xffffff))
                                .child("Members"),
                        )
                        .when(editable && !r.adding, |d| {
                            d.child(small_btn(
                                "role-add-members",
                                "Add Members",
                                true,
                                click(on, || CsAct::Role(Act::Adding(true))),
                            ))
                        }),
                )
                .child(div().mb(px(8.)).flex().child(search_box(&m.role_query)))
                .child(body),
        ));
        if v2 && editable {
            out = out.child(block(
                false,
                div().flex().child(
                    div()
                        .id("role-delete")
                        .h(px(34.))
                        .px(px(14.))
                        .flex()
                        .items_center()
                        .rounded(px(8.))
                        .border_1()
                        .border_color(c(crate::timeline::DANGER).opacity(0.45))
                        .bg(ca(255, 46, 169, 0.12))
                        .font_weight(FontWeight::MEDIUM)
                        .text_size(px(14.))
                        .text_color(c(crate::timeline::DANGER))
                        .when(r.busy, |d| d.opacity(0.45))
                        .when(!r.busy, |d| {
                            d.cursor_pointer()
                                .hover(|d| d.bg(ca(255, 46, 169, 0.18)))
                                .on_click(click(on, || CsAct::Role(Act::Delete)))
                        })
                        .child("Delete Role"),
                ),
            ));
        }
    }
    out.into_any_element()
}

fn empty(text: &'static str) -> impl IntoElement {
    div()
        .py(px(4.))
        .text_center()
        .text_size(px(12.5))
        .line_height(px(18.))
        .text_color(white_a(0.4))
        .child(text)
}

pub fn view(m: &CommSettings, store: &Store, on: &OnAct, cx: &App) -> AnyElement {
    if m.roles.edit.is_some() {
        editor(m, store, on, cx)
    } else {
        list(m, store, on)
    }
}

pub fn anchors(r: &Roles) -> Vec<crate::settings::Anchor> {
    use crate::settings::anchor;
    match &r.edit {
        None => vec![anchor(
            "role-list",
            "Role List",
            "roles role list order rank create permissions",
        )],
        Some(e) if e.everyone => vec![anchor(
            "role-permissions",
            "Permissions",
            "everyone role permissions ban kick manage",
        )],
        Some(e) => {
            let mut v = vec![
                anchor(
                    "role-display",
                    "Display",
                    "role name colour color access channel scope",
                ),
                anchor(
                    "role-permissions",
                    "Permissions",
                    "role permissions ban kick manage",
                ),
            ];
            if e.id.is_some() {
                v.push(anchor(
                    "role-members",
                    "Members",
                    "role members holders assign give",
                ));
            }
            v
        }
    }
}

pub fn parse_hex(text: &str) -> Option<u32> {
    let t = text.trim().trim_start_matches('#');
    (t.len() == 6)
        .then(|| u32::from_str_radix(t, 16).ok())
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drafts_and_colours() {
        assert_eq!(parse_hex("#59fcb3"), Some(0x59fcb3));
        assert_eq!(parse_hex("59fcb"), None);
        let v =
            serde_json::json!({"name": "Mods", "color": 5, "permissions": "56", "display": true});
        let d = role_of(&v);
        assert_eq!((d.permissions, d.color, d.display), (56, 5, true));
        assert_eq!(groups(true).len(), 5);
    }
}
