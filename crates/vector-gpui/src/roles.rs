//! Community Settings → Roles (`RolesSection.svelte`): the list in display order, dragged to
//! reorder, and the editor for one role: its look, its permissions and its members. Order and
//! edits are drafts under the save bar; membership and deletion act at once.

use crate::kit::Tipped as _;
use crate::lang::LangFonts as _;
use std::collections::HashSet;

use gpui::prelude::*;
use gpui::{AnyElement, App, FontWeight, SharedString, Window, div, px, svg};
use serde_json::Value;

use crate::commsettings::{
    Act as CsAct, CommSettings, OnAct, block, click, field_head, heading, hint, indicator,
    input_box, lede, member_row, save_btn, search_box, short_npub, text_btn,
};
use crate::data::Store;
use crate::settings::Label;
use crate::t;
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
    pub name: Label,
    pub desc: Label,
    pub v2: bool,
}

const fn p(bit: u32, name: Label, desc: Label) -> Perm {
    Perm {
        bit,
        name,
        desc,
        v2: false,
    }
}

const fn p2(bit: u32, name: Label, desc: Label) -> Perm {
    Perm {
        bit,
        name,
        desc,
        v2: true,
    }
}

pub const COMMUNITY: &[Perm] = &[
    p(2, t::roles_perm_manage_community, t::roles_perm_manage_community_desc),
    p(1, t::roles_perm_manage_channels, t::roles_perm_manage_channels_desc),
    p(0, t::roles_perm_manage_roles, t::roles_perm_manage_roles_desc),
    p(6, t::roles_perm_create_invites, t::roles_perm_create_invites_desc),
    p(8, t::roles_perm_view_audit_log, t::roles_perm_view_audit_log_desc),
];
pub const MODERATION: &[Perm] = &[
    p(3, t::roles_perm_kick, t::roles_perm_kick_desc),
    p(4, t::roles_perm_ban, t::roles_perm_ban_desc),
    p(5, t::roles_perm_manage_messages, t::roles_perm_manage_messages_desc),
    p(11, t::roles_perm_pin, t::roles_perm_pin_desc),
    p2(26, t::roles_perm_timeout, t::roles_perm_timeout_desc),
    p(28, t::roles_perm_manage_nicknames, t::roles_perm_manage_nicknames_desc),
];
pub const CHANNELS: &[Perm] = &[
    p(13, t::roles_perm_view_channels, t::roles_perm_view_channels_desc),
    p(14, t::roles_perm_send, t::roles_perm_send_desc),
    p(15, t::roles_perm_threads, t::roles_perm_threads_desc),
    p(16, t::roles_perm_attach, t::roles_perm_attach_desc),
    p(17, t::roles_perm_react, t::roles_perm_react_desc),
    p(18, t::roles_perm_history, t::roles_perm_history_desc),
];
pub const VOICE: &[Perm] = &[
    p(19, t::roles_perm_connect, t::roles_perm_connect_desc),
    p(20, t::roles_perm_speak, t::roles_perm_speak_desc),
    p(21, t::roles_perm_video, t::roles_perm_video_desc),
    p(22, t::roles_perm_priority, t::roles_perm_priority_desc),
    p(23, t::roles_perm_mute, t::roles_perm_mute_desc),
    p(24, t::roles_perm_deafen, t::roles_perm_deafen_desc),
    p(25, t::roles_perm_move, t::roles_perm_move_desc),
];
pub const CHAT: &[Perm] = &[
    p(9, t::roles_perm_mention_everyone, t::roles_perm_mention_everyone_desc),
    p2(29, t::roles_perm_change_nickname, t::roles_perm_change_nickname_desc),
];

pub fn groups(v2: bool) -> Vec<(Label, &'static [Perm])> {
    if v2 {
        vec![
            (t::roles_group_community, COMMUNITY),
            (t::roles_group_moderation, MODERATION),
            (t::roles_group_channels, CHANNELS),
            (t::roles_group_voice, VOICE),
            (t::roles_group_chat, CHAT),
        ]
    } else {
        vec![(t::roles_group_community, COMMUNITY), (t::roles_group_moderation, MODERATION), (t::roles_group_chat, CHAT)]
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
            .lang_fonts()
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

fn count(text: SharedString) -> impl IntoElement {
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
            div().child(heading(t::roles_title())).child(hint(t::roles_loading())),
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
            .map_or_else(|| t::roles_private_channel().to_string(), str::to_string)
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
                        .child(t::roles_owner()),
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
                .child(t::roles_none()),
        );
    }
    for role in all {
        let id = role["role_id"].as_str().unwrap_or("").to_string();
        let manageable = role["manageable"].as_bool().unwrap_or(false);
        let color = role["color"].as_u64().unwrap_or(0) as u32;
        let name = role["name"]
            .as_str()
            .filter(|n| !n.is_empty())
            .map_or_else(|| t::roles_unnamed().to_string(), str::to_string);
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
            .child(count(t::roles_held(held)))
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
                .child(count(t::roles_held_everyone()))
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
                    .child(div().text_size(px(14.5)).text_color(c(0xe8e8e8)).child(t::roles_upgrade_title()))
                    .child(div().text_size(px(12.5)).line_height(px(17.5)).text_color(c(0x7a7a7a)).child(t::roles_upgrade_text())),
            )
            .child(save_btn("role-switch", t::roles_upgrade(), !r.busy, click(on, || CsAct::Role(Act::Activate))))
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
                    .child(div().text_size(px(24.)).line_height(px(28.)).font_weight(FontWeight::BOLD).text_color(c(0xffffff)).child(t::roles_title()))
                    .when(can_manage && total < max, |d| d.child(small_btn("role-create", t::roles_create(), true, click(on, || CsAct::Role(Act::New))))),
            )
            .child(lede(t::roles_lede()))
            .children(upgrade)
            .child(rows)
            .child(hint(t::roles_count(total, max))),
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
    name: SharedString,
    desc: SharedString,
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
            .map_or_else(|| t::roles_private_channel().to_string(), str::to_string)
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
        .child(t::roles_back());
    let mut out = div().child(div().flex().child(back));
    if e.everyone {
        out = out.child(block(true, div().child(heading("@everyone")).child(lede(t::roles_everyone_lede()))));
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
                .tip(t::roles_custom_colour())
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
            .child(if name.trim().is_empty() { t::roles_preview_unnamed() } else { name.trim().to_string().into() });
        let mut display = div()
            .child(heading(if e.id.is_none() { t::roles_new() } else { t::roles_section_role_display() }))
            .when(!editable, |d| d.child(lede(t::roles_read_only())))
            .child(field_head(
                t::roles_name(),
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
                    .child(t::roles_colour()),
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
                    t::roles_display_separately(),
                    t::roles_display_separately_desc(),
                    e.draft.display,
                    editable,
                    click(on, || CsAct::Role(Act::Display)),
                )))
                .child(perm_row(
                    "role-mention".into(),
                    t::roles_mentionable(),
                    t::roles_mentionable_desc(),
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
                    .child(t::roles_access()),
            );
            if e.id.is_none() {
                let choices: Vec<crate::kit::Choice> = std::iter::once(crate::kit::Choice {
                    value: "",
                    label: crate::kit::interned(&t::roles_access_every_channel()),
                })
                .chain(
                    view["channels"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter(|c| c["private"].as_bool().unwrap_or(false))
                        .filter_map(|c| {
                            let id = crate::kit::interned(c["channel_id"].as_str()?);
                            let label = crate::kit::interned(&t::roles_access_only(c["name"].as_str().unwrap_or("")));
                            Some(crate::kit::Choice { value: id, label })
                        }),
                )
                .collect();
                let (t, pk) = (on.clone(), on.clone());
                display = display
                    .child(div().mt(px(10.)).w(px(400.)).max_w_full().child(crate::kit::select_look("role-access", f32::INFINITY, 14., &choices, &[], e.draft.channel_id.as_deref().unwrap_or(""), r.access_open, crate::kit::SelectLook { priority: crate::commsettings::LIST_PRIORITY, ..Default::default() }, move |w, cx| t(CsAct::Role(Act::AccessOpen), w, cx), move |v, w, cx| pk(CsAct::Role(Act::Access(v)), w, cx))))
                    .child(hint(t::roles_access_hint()).mt(px(10.)));
            } else {
                display = display.child(
                    hint(match &e.draft.channel_id {
                        Some(ch) => t::roles_access_scoped(channel_name(ch)),
                        None => t::roles_access_everywhere(),
                    })
                    .mt(px(2.)),
                );
            }
        }
        out = out.child(block(true, display));
    }
    let mut perms = div()
        .child(heading(if e.everyone { t::roles_section_everyone_permissions() } else { t::roles_section_role_permissions() }))
        .child(lede(t::roles_permissions_lede()));
    for (i, (title, list)) in groups(v2).into_iter().enumerate() {
        perms = perms.child(
            crate::kit::spaced(title().to_uppercase(), 0.66)
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
                (p.name)(),
                (p.desc)(),
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
                body = body.child(empty(if q.is_empty() { t::roles_all_hold() } else { t::roles_no_matches() }));
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
                        t::roles_cancel(),
                        true,
                        click(on, || CsAct::Role(Act::Adding(false))),
                    ))
                    .child(save_btn(
                        "role-add",
                        if r.busy {
                            t::roles_adding()
                        } else if n == 0 {
                            t::roles_add()
                        } else {
                            t::roles_add_count(n)
                        },
                        n > 0 && !r.busy,
                        click(on, || CsAct::Role(Act::AddPicked)),
                    )),
            );
        } else {
            let shown: Vec<&String> = holders.iter().filter(|n| matches(n)).collect();
            if shown.is_empty() {
                body = body.child(empty(if q.is_empty() { t::roles_nobody() } else { t::roles_no_matches() }));
            }
            for npub in shown {
                let key = npub.clone();
                let group = SharedString::from(format!("role-held-{npub}"));
                let remove: AnyElement = if editable {
                    div()
                        .id(SharedString::from(format!("role-rm-{npub}")))
                        .tip(t::roles_remove())
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
                                .child(t::roles_members()),
                        )
                        .when(editable && !r.adding, |d| {
                            d.child(small_btn(
                                "role-add-members",
                                t::roles_add_members(),
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
                        .child(t::roles_delete()),
                ),
            ));
        }
    }
    out.into_any_element()
}

fn empty(text: SharedString) -> impl IntoElement {
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
        None => vec![anchor("role-list", t::roles_section_role_list, t::roles_section_role_list_keys)],
        Some(e) if e.everyone => vec![anchor("role-permissions", t::roles_section_everyone_permissions, t::roles_section_everyone_permissions_keys)],
        Some(e) => {
            let mut v = vec![
                anchor("role-display", t::roles_section_role_display, t::roles_section_role_display_keys),
                anchor("role-permissions", t::roles_section_role_permissions, t::roles_section_role_permissions_keys),
            ];
            if e.id.is_some() {
                v.push(anchor("role-members", t::roles_section_role_members, t::roles_section_role_members_keys));
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
