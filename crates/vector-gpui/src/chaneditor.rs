//! Channel Settings (`community/ChannelEditor.svelte`): Community Settings' layout over one
//! channel or Category. Overview holds the name, topic and view; Permissions holds one
//! overwrite per role or member, each bit allowed, denied or inherited. A Category has
//! Permissions alone. Edits wait behind one save bar, and a close with any pending is refused.

use std::rc::Rc;

use gpui::prelude::*;
use gpui::{AnyElement, App, Entity, FontWeight, ScrollHandle, SharedString, Window, div, px, svg};
use serde_json::Value;

use crate::commsettings::{block, field_head, heading, hint, input_box, lede, save_btn, short_npub, text_btn};
use crate::composer::TextInput;
use crate::data::Store;
use crate::settings::{Anchor, Category, NavOn, anchor};
use crate::theme::*;

pub const NAME_MAX: usize = 64;
pub const TOPIC_MAX: usize = 1024;
const PICK_MAX: usize = 100;
const DEFAULT_TINT: u32 = 0x99aab5;

pub struct Perm {
    pub bit: u32,
    pub name: &'static str,
    pub desc: &'static str,
}

const fn perm(bit: u32, name: &'static str, desc: &'static str) -> Perm {
    Perm { bit, name, desc }
}

pub const CHANNEL: &[Perm] = &[
    perm(13, "View Channel", "See the channel and read it. A private channel's key follows this."),
    perm(14, "Send Messages", "Post here. Without it for @everyone, only those you allow can post."),
    perm(15, "Send in Threads", "Reply inside a thread."),
    perm(16, "Attach Files", "Share images, videos and files."),
    perm(17, "Add Reactions", "React to messages."),
    perm(18, "Read Message History", "Read what was said before they could see the channel."),
    perm(5, "Manage Messages", "Hide messages from members beneath them here."),
];

pub const VOICE: &[Perm] = &[
    perm(19, "Connect", "Join the channel's call."),
    perm(20, "Speak", "Be heard in the call."),
    perm(21, "Video", "Share a camera or a screen."),
    perm(22, "Priority Speaker", "Lower others while speaking."),
    perm(23, "Mute Members", "Mute others in the call."),
    perm(24, "Deafen Members", "Stop others hearing the call."),
    perm(25, "Move Members", "Move others between calls."),
    perm(26, "Timeout Members", "Time out members beneath them here."),
];

const GROUPS: [(&str, &str, &str, &[Perm]); 2] = [
    ("perm-channel", "Channel", "view send threads attach files reactions history manage messages hide", CHANNEL),
    ("perm-voice", "Voice", "connect speak video priority mute deafen move timeout", VOICE),
];

const VIEWS: [(&str, &str, &str); 2] = [
    ("chat", "Text", "Messages first."),
    ("voice", "Voice", "Opens its call, with the chat beside it."),
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tri {
    Deny,
    Inherit,
    Allow,
}

pub struct Role {
    pub id: String,
    pub name: String,
    pub color: u32,
    pub position: i64,
}

pub struct Overwrite {
    /// `everyone`, `role:<id>` or `member:<hex>`.
    pub key: String,
    pub npub: Option<String>,
    pub allow: u64,
    pub deny: u64,
}

/// `get_channel_permissions`, parsed.
#[derive(Default)]
pub struct Perms {
    pub activated: bool,
    pub can_manage: bool,
    pub synced: bool,
    pub category: Option<String>,
    pub grantable: u64,
    pub roles: Vec<Role>,
    pub overwrites: Vec<Overwrite>,
    /// (hex, npub) of everyone a member overwrite can name.
    pub members: Vec<(String, String)>,
}

fn bits(v: &Value) -> u64 {
    v.as_str()
        .and_then(|s| s.parse().ok())
        .or_else(|| v.as_u64())
        .unwrap_or(0)
}

impl Perms {
    pub fn parse(v: &Value) -> Self {
        let str_of = |v: &Value| v.as_str().map(String::from);
        Self {
            activated: v["activated"].as_bool().unwrap_or(false),
            can_manage: v["can_manage"].as_bool().unwrap_or(false),
            synced: !v["synced"].is_null() && v["synced"] != Value::Bool(false),
            category: str_of(&v["category"]),
            grantable: bits(&v["grantable"]),
            roles: v["roles"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|r| Role {
                    id: str_of(&r["role_id"]).unwrap_or_default(),
                    name: str_of(&r["name"]).unwrap_or_default(),
                    color: r["color"].as_u64().unwrap_or(0) as u32,
                    position: r["position"].as_i64().unwrap_or(0),
                })
                .collect(),
            overwrites: v["overwrites"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|o| {
                    let kind = o["kind"].as_str().unwrap_or("everyone");
                    Overwrite {
                        key: match o["id"].as_str() {
                            Some(id) if kind != "everyone" => format!("{kind}:{id}"),
                            _ => "everyone".into(),
                        },
                        npub: str_of(&o["npub"]),
                        allow: bits(&o["allow"]),
                        deny: bits(&o["deny"]),
                    }
                })
                .collect(),
            members: v["members"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|m| Some((str_of(&m["id"])?, str_of(&m["npub"])?)))
                .collect(),
        }
    }

    pub fn stored(&self, key: &str) -> (u64, u64) {
        self.overwrites
            .iter()
            .find(|o| o.key == key)
            .map_or((0, 0), |o| (o.allow, o.deny))
    }

    fn open(&self) -> bool {
        self.activated && self.can_manage
    }
}

pub struct Saved {
    pub name: String,
    pub topic: String,
    pub view: String,
}

pub struct ChannelEditor {
    pub community: String,
    pub channel: String,
    pub is_category: bool,
    pub primary: bool,
    pub section: &'static str,
    pub anchor: Option<&'static str>,
    pub search: Entity<TextInput>,
    pub scroll: ScrollHandle,
    pub name: Entity<TextInput>,
    pub topic: Entity<TextInput>,
    pub view: String,
    pub private: bool,
    pub saved: Saved,
    pub perms: Option<Perms>,
    pub target: String,
    pub draft: (u64, u64),
    pub busy: bool,
    pub error: Option<String>,
    /// A refused close or switch: the bar says why until this passes.
    pub warn_until: Option<std::time::Instant>,
    pub warn_switch: bool,
    pub adding: bool,
    pub add_closed: Option<std::time::Instant>,
    pub add_query: Entity<TextInput>,
}

pub enum Act {
    Close,
    Section(&'static str),
    Anchor(&'static str, &'static str),
    ClearSearch,
    View(&'static str),
    Private(bool),
    Sync(bool),
    Target(String),
    Bit(u32, Tri),
    ToggleAdd,
    CloseAdd,
    Pick(String),
    Reset,
    Save,
    Delete,
}

pub type OnAct = Rc<dyn Fn(Act, &mut Window, &mut App)>;

fn click(on: &OnAct, act: impl Fn() -> Act + 'static) -> impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static {
    let on = on.clone();
    move |_, w, cx| on(act(), w, cx)
}

pub struct Target {
    pub key: String,
    pub label: String,
    pub color: Option<u32>,
    pub member: bool,
    pub set: bool,
}

/// Who a member target names: their name, else the start of their key.
fn member_label(store: &Store, npub: Option<&str>, hex: &str) -> String {
    match npub {
        Some(n) if store.profiles.get(n).is_some_and(|p| !p.name.is_empty() || !p.display_name.is_empty() || !p.nickname.is_empty()) => store.name(n),
        Some(n) => short_npub(n),
        None => format!("{}…", &hex[..hex.len().min(10)]),
    }
}

impl ChannelEditor {
    pub fn overview_dirty(&self, cx: &App) -> bool {
        let name = self.name.read(cx).text() != self.saved.name;
        if self.is_category {
            return name;
        }
        name || self.topic.read(cx).text() != self.saved.topic || self.view != self.saved.view
    }

    pub fn overwrite_dirty(&self) -> bool {
        self.perms.as_ref().is_some_and(|p| p.stored(&self.target) != self.draft)
    }

    pub fn dirty(&self, cx: &App) -> bool {
        self.overview_dirty(cx) || self.overwrite_dirty()
    }

    pub fn warn(&self) -> bool {
        self.warn_until.is_some_and(|t| std::time::Instant::now() < t)
    }

    pub fn valid(&self, cx: &App) -> bool {
        let name = self.name.read(cx).text();
        !name.trim().is_empty()
            && name.len() <= NAME_MAX
            && (self.is_category || self.topic.read(cx).text().len() <= TOPIC_MAX)
    }

    fn perms_open(&self) -> bool {
        self.perms.as_ref().is_some_and(Perms::open)
    }

    fn syncable(&self) -> bool {
        self.perms_open()
            && !self.is_category
            && self.perms.as_ref().is_some_and(|p| p.synced || p.category.is_some())
    }

    pub fn bit(&self, bit: u32) -> Tri {
        let b = 1u64 << bit;
        if self.draft.0 & b != 0 {
            Tri::Allow
        } else if self.draft.1 & b != 0 {
            Tri::Deny
        } else {
            Tri::Inherit
        }
    }

    pub fn set_bit(&mut self, bit: u32, to: Tri) {
        let b = 1u64 << bit;
        let (mut allow, mut deny) = self.draft;
        allow &= !b;
        deny &= !b;
        match to {
            Tri::Allow => allow |= b,
            Tri::Deny => deny |= b,
            Tri::Inherit => {}
        }
        self.draft = (allow, deny);
    }

    /// A bit you lack can't be set, but one already set may be cleared.
    pub fn can_touch(&self, bit: u32) -> bool {
        let Some(p) = self.perms.as_ref() else { return false };
        let b = 1u64 << bit;
        let (allow, deny) = p.stored(&self.target);
        p.grantable & b == b || (allow | deny) & b == b
    }

    /// A private channel opens only to a role's or a member's allow, whatever @everyone says.
    fn closed_to_everyone(&self, bit: u32) -> bool {
        self.private && self.target == "everyone" && bit == 13
    }

    /// The targets with an overwrite here, @everyone last, plus whichever is open.
    pub fn targets(&self, store: &Store) -> Vec<Target> {
        let Some(p) = self.perms.as_ref() else { return Vec::new() };
        let mut keys: Vec<&str> = vec!["everyone"];
        for k in p.overwrites.iter().map(|o| o.key.as_str()).chain([self.target.as_str()]) {
            if !keys.contains(&k) {
                keys.push(k);
            }
        }
        let mut out: Vec<(f64, Target)> = keys
            .into_iter()
            .map(|key| {
                let (allow, deny) = p.stored(key);
                let set = allow != 0 || deny != 0;
                if key == "everyone" {
                    return (f64::INFINITY, Target { key: key.into(), label: "@everyone".into(), color: None, member: false, set: true });
                }
                let (kind, id) = key.split_once(':').unwrap_or((key, ""));
                if kind == "role" {
                    let role = p.roles.iter().find(|r| r.id == id);
                    return (
                        role.map_or(1e9, |r| r.position as f64),
                        Target {
                            key: key.into(),
                            label: role.map_or("Deleted role".into(), |r| r.name.clone()),
                            color: Some(role.map_or(0, |r| r.color)),
                            member: false,
                            set,
                        },
                    );
                }
                let npub = p
                    .overwrites
                    .iter()
                    .find(|o| o.key == key)
                    .and_then(|o| o.npub.clone())
                    .or_else(|| p.members.iter().find(|m| m.0 == id).map(|m| m.1.clone()));
                (2e9, Target { key: key.into(), label: member_label(store, npub.as_deref(), id), color: None, member: true, set })
            })
            .collect();
        out.sort_by(|a, b| a.0.total_cmp(&b.0));
        out.into_iter().map(|(_, t)| t).collect()
    }

    pub fn sections(&self) -> Vec<Category> {
        let activated = self.perms.as_ref().is_some_and(|p| p.activated);
        let mut overview = vec![anchor("ch-name", "Name", "name rename title")];
        if !self.is_category {
            overview.push(anchor("ch-topic", "Topic", "topic description about"));
            overview.push(anchor("ch-view", "Opens As", "view text voice opens"));
            if activated {
                overview.push(anchor("ch-private", "Privacy", "private privacy public hidden key"));
            }
        }
        let mut permissions: Vec<Anchor> = Vec::new();
        if self.syncable() {
            permissions.push(anchor("ch-sync", "Category Sync", "sync category inherit"));
        }
        permissions.push(anchor("ch-targets", "Roles & Members", "roles members everyone overwrite who"));
        if self.perms_open() && !self.perms.as_ref().is_some_and(|p| p.synced) {
            permissions.extend(GROUPS.iter().map(|(id, title, keys, _)| anchor(id, title, keys)));
        }
        vec![
            Category { id: "overview", label: "Overview", icon: "info", danger: false, anchors: overview },
            Category { id: "permissions", label: "Permissions", icon: "shield-filled", danger: false, anchors: permissions },
        ]
    }

    /// Roles without rules here yet, then members, each matching the picker's query.
    pub fn addable(&self, store: &Store, cx: &App) -> (Vec<&Role>, Vec<(String, String, String)>) {
        let Some(p) = self.perms.as_ref() else { return (Vec::new(), Vec::new()) };
        let q = self.add_query.read(cx).text().trim().to_lowercase();
        let taken = |key: String| key == self.target || p.overwrites.iter().any(|o| o.key == key);
        let roles = p
            .roles
            .iter()
            .filter(|r| !taken(format!("role:{}", r.id)))
            .filter(|r| q.is_empty() || r.name.to_lowercase().contains(&q))
            .collect();
        let mut members: Vec<(String, String, String)> = p
            .members
            .iter()
            .filter(|(id, _)| !taken(format!("member:{id}")))
            .filter_map(|(id, npub)| {
                let named = store.profiles.get(npub.as_str()).is_some_and(|p| !p.name.is_empty() || !p.display_name.is_empty() || !p.nickname.is_empty());
                let name = if named { store.name(npub) } else { String::new() };
                (q.is_empty() || format!("{name} {npub}").to_lowercase().contains(&q)).then(|| (id.clone(), npub.clone(), name))
            })
            .collect();
        members.sort_by(|a, b| {
            let ka = if a.2.is_empty() { &a.1 } else { &a.2 };
            let kb = if b.2.is_empty() { &b.1 } else { &b.2 };
            ka.cmp(kb)
        });
        (roles, members)
    }
}

fn perm_text(name: impl IntoElement, desc: impl Into<SharedString>) -> gpui::Div {
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .gap(px(2.))
        .child(div().text_size(px(14.5)).line_height(px(24.)).text_color(c(0xe8e8e8)).child(name))
        .child(div().text_size(px(12.5)).line_height(px(17.5)).text_color(c(0x7a7a7a)).child(desc.into()))
}

fn overview(m: &ChannelEditor, on: &OnAct, cx: &App) -> Vec<AnyElement> {
    let editable = !m.busy;
    let name_len = m.name.read(cx).text().len();
    let name_warn = name_len > NAME_MAX || m.name.read(cx).text().trim().is_empty();
    let mut out = vec![block(
        true,
        div()
            .child(heading("Name"))
            .child(field_head(if m.is_category { "Category Name" } else { "Channel Name" }, Some((name_len, NAME_MAX, name_warn))))
            .child(input_box(&m.name, 42., editable)),
    )
    .into_any_element()];
    if m.is_category {
        return out;
    }
    let topic_len = m.topic.read(cx).text().len();
    out.push(
        block(
            false,
            div()
                .child(heading("Topic"))
                .child(field_head("What this channel is for", Some((topic_len, TOPIC_MAX, topic_len > TOPIC_MAX))))
                .child(
                    div()
                        .h(px(114.))
                        .px(px(13.))
                        .py(px(11.))
                        .rounded(px(10.))
                        .border_1()
                        .border_color(c(0x2c2c2c))
                        .bg(c(0x111111))
                        .overflow_hidden()
                        .when(!editable, |d| d.opacity(0.6))
                        .child(m.topic.clone()),
                ),
        )
        .into_any_element(),
    );
    let views = div().flex().gap(px(8.)).children(VIEWS.iter().map(|&(id, name, desc)| {
        let on_ = m.view == id;
        div()
            .id(SharedString::from(format!("ce-view-{id}")))
            .w(px((596. - 16.) / 3.))
            .min_w_0()
            .min_h(px(78.))
            .px(px(12.))
            .py(px(10.))
            .flex()
            .flex_col()
            .gap(px(3.))
            .rounded(px(10.))
            .border_1()
            .font_weight(FontWeight::SEMIBOLD)
            .when(on_, |d| d.border_color(c(PRIMARY)).bg(c(0x0f1d17)))
            .when(!on_, |d| d.border_color(c(0x262626)).bg(c(0x0c0c0c)).hover(|d| d.bg(c(0x131313))))
            .when(!editable, |d| d.opacity(0.6))
            .when(editable, |d| d.cursor_pointer().on_click(click(on, move || Act::View(id))))
            .child(div().text_size(px(14.5)).line_height(px(18.)).text_color(c(0xe8e8e8)).child(name))
            .child(div().text_size(px(12.5)).line_height(px(17.5)).text_color(c(0x7a7a7a)).child(desc))
    }));
    out.push(block(false, div().child(heading("Opens As")).child(lede("How a member's client opens the channel. It changes nothing about who can do what.")).child(views)).into_any_element());
    if m.perms.as_ref().is_some_and(|p| p.activated) {
        let private = m.private;
        let toggle_on = on.clone();
        out.push(
            block(
                false,
                div().child(heading("Privacy")).child(
                    div()
                        .py(px(12.))
                        .flex()
                        .items_center()
                        .gap(px(16.))
                        .child(perm_text(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(6.))
                                .child(svg().path("icons/locked.svg").size(px(13.)).text_color(c(0xe8e8e8)))
                                .child("Private Channel"),
                            if m.saved.view == "voice" {
                                "Only members and roles you let in can join it."
                            } else {
                                "Only members and roles you let in can see it."
                            },
                        ))
                        .child(div().flex_shrink_0().when(m.busy, |d| d.opacity(0.4)).child(crate::settings::toggle(
                            "ce-private",
                            private,
                            false,
                            move |_, w, cx| toggle_on(Act::Private(!private), w, cx),
                        ))),
                ),
            )
            .into_any_element(),
        );
    }
    out
}

fn tri(m: &ChannelEditor, p: &Perm, live: bool, on: &OnAct) -> impl IntoElement {
    let state = m.bit(p.bit);
    let bit = p.bit;
    let cell = |to: Tri, first: bool| {
        let lit = state == to;
        let (bg, fg) = match (to, lit) {
            (Tri::Deny, true) => (c(0x42132f), c(crate::timeline::DANGER)),
            (Tri::Inherit, true) => (c(0x1b1b1b), c(0xcfcfcf)),
            (Tri::Allow, true) => (c(0x0f3a29), c(PRIMARY)),
            _ => (c(0x0c0c0c), c(0x6f6f6f)),
        };
        let glyph: AnyElement = match to {
            Tri::Deny => svg().path("icons/x.svg").size(px(12.)).text_color(fg).into_any_element(),
            Tri::Allow => svg().path("icons/check.svg").size(px(12.)).text_color(fg).into_any_element(),
            Tri::Inherit => div().font_weight(FontWeight::SEMIBOLD).text_size(px(13.)).line_height(px(13.)).text_color(fg).child("/").into_any_element(),
        };
        div()
            .id(SharedString::from(format!("ce-tri-{bit}-{to:?}")))
            .w(px(30.))
            .h(px(26.))
            .flex()
            .items_center()
            .justify_center()
            .bg(bg)
            .when(!first, |d| d.border_l_1().border_color(c(0x262626)))
            .when(live, |d| d.cursor_pointer().on_click(click(on, move || Act::Bit(bit, to))))
            .child(glyph)
    };
    div()
        .flex()
        .flex_shrink_0()
        .rounded(px(8.))
        .border_1()
        .border_color(c(0x262626))
        .overflow_hidden()
        .child(cell(Tri::Deny, true))
        .child(cell(Tri::Inherit, false))
        .child(cell(Tri::Allow, false))
}

fn dot(color: u32) -> impl IntoElement {
    div()
        .size(px(12.))
        .flex_shrink_0()
        .rounded_full()
        .bg(c(if color == 0 { DEFAULT_TINT } else { color }))
        .border_1()
        .border_color(white_a(0.15))
}

fn add_picker(m: &ChannelEditor, store: &Store, on: &OnAct, cx: &App) -> AnyElement {
    let (roles, members) = m.addable(store, cx);
    let label = |text: &'static str| {
        div()
            .px(px(8.))
            .pt(px(10.))
            .pb(px(4.))
            .text_size(px(11.))
            .line_height(px(13.))
            .font_weight(FontWeight::BOLD)
            .text_color(c(0x7a7a7a))
            .child(text.to_uppercase())
    };
    let row = |id: SharedString, lead: AnyElement, name: String, sub: Option<String>, key: String| {
        let group = id.clone();
        div()
            .id(id)
            .group(group.clone())
            .h(px(38.))
            .px(px(8.))
            .flex()
            .items_center()
            .gap(px(10.))
            .rounded(px(8.))
            .cursor_pointer()
            .hover(|d| d.bg(c(0x1c1c1c)))
            .on_click(click(on, move || Act::Pick(key.clone())))
            .child(div().w(px(22.)).flex_shrink_0().flex().items_center().justify_center().child(lead))
            .child(div().min_w_0().truncate().text_size(px(14.)).font_weight(FontWeight::MEDIUM).text_color(c(0xe6e6e6)).child(name))
            .children(sub.map(|s| div().flex_1().min_w_0().truncate().text_size(px(12.)).text_color(c(0x6a6a6a)).child(s)))
            .child(
                svg()
                    .path("icons/plus.svg")
                    .ml_auto()
                    .size(px(14.))
                    .flex_shrink_0()
                    .text_color(c(ACCENT))
                    .invisible()
                    .group_hover(group, |d| d.visible()),
            )
    };
    let mut list = div().id("ce-add-list").flex_1().min_h_0().overflow_y_scroll().px(px(6.)).pt(px(4.)).pb(px(8.));
    let empty = roles.is_empty() && members.is_empty();
    if !roles.is_empty() {
        list = list.child(label("Roles"));
        for r in roles {
            list = list.child(row(
                SharedString::from(format!("ce-add-role-{}", r.id)),
                dot(r.color).into_any_element(),
                r.name.clone(),
                None,
                format!("role:{}", r.id),
            ));
        }
    }
    if !members.is_empty() {
        list = list.child(label("Members"));
        let more = members.len().saturating_sub(PICK_MAX);
        for (id, npub, name) in members.into_iter().take(PICK_MAX) {
            let veiled = store.stream_hidden(&npub);
            let shown = if name.is_empty() { short_npub(&npub) } else { name.clone() };
            let sub = (!name.is_empty() && !veiled).then(|| short_npub(&npub));
            list = list.child(row(
                SharedString::from(format!("ce-add-member-{id}")),
                crate::ui::avatar(store, &npub, 22.).into_any_element(),
                shown,
                sub,
                format!("member:{id}"),
            ));
        }
        if more > 0 {
            list = list.child(div().px(px(8.)).py(px(14.)).text_center().text_size(px(13.)).text_color(c(0x7a7a7a)).child(format!("Search to find the other {more}.")));
        }
    }
    if empty {
        let q = m.add_query.read(cx).text().trim().to_string();
        list = list.child(div().px(px(8.)).py(px(14.)).text_center().text_size(px(13.)).text_color(c(0x7a7a7a)).child(format!("Nobody matches \"{q}\".")));
    }
    let close = on.clone();
    gpui::deferred(
        div()
            .id("ce-add-pop")
            .absolute()
            .top(px(40.))
            .right_0()
            .w(px(360.))
            .max_h(px(380.))
            .flex()
            .flex_col()
            .rounded(px(14.))
            .border_1()
            .border_color(c(0x2c2e2f))
            .bg(c(0x101010))
            .shadow(vec![gpui::BoxShadow {
                color: ca(0, 0, 0, 0.6),
                offset: gpui::point(px(0.), px(18.)),
                blur_radius: px(50.),
                spread_radius: px(0.),
                inset: false,
            }])
            .overflow_hidden()
            .occlude()
            .on_click(|_, _, cx| cx.stop_propagation())
            .on_mouse_down_out(move |_, w, cx| close(Act::CloseAdd, w, cx))
            .child(
                div().flex_none().p(px(10.)).border_b_1().border_color(c(0x1f1f1f)).child(
                    div()
                        .relative()
                        .h(px(36.))
                        .pl(px(34.))
                        .pr(px(12.))
                        .flex()
                        .items_center()
                        .rounded(px(9.))
                        .border_1()
                        .border_color(c(0x262626))
                        .bg(c(0x161616))
                        .child(svg().path("icons/search.svg").absolute().left(px(12.)).top(px(10.)).size(px(14.)).text_color(c(0x7a7a7a)))
                        .child(div().flex_1().min_w_0().child(m.add_query.clone())),
                ),
            )
            .child(list),
    )
    .with_priority(13)
    .into_any_element()
}

fn permissions(m: &ChannelEditor, store: &Store, on: &OnAct, cx: &App) -> Vec<AnyElement> {
    let Some(p) = m.perms.as_ref() else {
        return vec![block(true, div().child(heading("Roles & Members")).child(hint("Loading..."))).into_any_element()];
    };
    if !p.open() {
        return vec![block(
            true,
            div().child(heading("Roles & Members")).child(lede(if p.activated {
                "Changing who can do what here needs Manage Roles."
            } else {
                "Channel permissions come with the new roles. Switch to them in Community Settings, under Roles."
            })),
        )
        .into_any_element()];
    }
    let mut out = Vec::new();
    if m.syncable() {
        let synced = p.synced;
        out.push(
            block(
                true,
                div().child(heading("Category Sync")).child(
                    div()
                        .mb(px(12.))
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap(px(12.))
                        .child(
                            div().text_size(px(14.)).line_height(px(21.)).text_color(c(0x9a9a9a)).child(if synced {
                                "This channel follows its category's permissions. Edit them from the category, or stop syncing to set its own."
                            } else {
                                "This channel sets its own permissions."
                            }),
                        )
                        .child(
                            div()
                                .id("ce-sync")
                                .h(px(34.))
                                .px(px(14.))
                                .flex_shrink_0()
                                .flex()
                                .items_center()
                                .rounded(px(8.))
                                .font_weight(FontWeight::MEDIUM)
                                .text_size(px(14.))
                                .text_color(c(PRIMARY))
                                .when(m.busy, |d| d.opacity(0.45))
                                .when(!m.busy, |d| d.cursor_pointer().hover(|d| d.underline()).on_click(click(on, move || Act::Sync(!synced))))
                                .child(if synced { "Stop syncing" } else { "Sync with its category" }),
                        ),
                ),
            )
            .into_any_element(),
        );
    }
    if p.synced {
        return out;
    }
    let first = out.is_empty();
    let targets = m.targets(store);
    let (roles, members) = m.addable(store, cx);
    let can_add = p.roles.iter().any(|r| !targets.iter().any(|t| t.key == format!("role:{}", r.id)))
        || p.members.iter().any(|(id, _)| !targets.iter().any(|t| t.key == format!("member:{id}")));
    drop((roles, members));
    let open_label = targets.iter().find(|t| t.key == m.target).map(|t| t.label.clone());
    let head = div()
        .relative()
        .mb(px(20.))
        .flex()
        .items_center()
        .justify_between()
        .gap(px(12.))
        .child(div().text_size(px(24.)).line_height(px(28.)).font_weight(FontWeight::BOLD).text_color(c(0xffffff)).child("Roles & Members"))
        .when(can_add, |d| {
            d.child(
                div()
                    .id("ce-add")
                    .h(px(34.))
                    .px(px(14.))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .rounded(px(8.))
                    .bg(c(PRIMARY))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_size(px(14.))
                    .text_color(c(0x000000))
                    .when(m.busy, |d| d.opacity(0.45))
                    .when(!m.busy, |d| d.cursor_pointer().hover(|d| d.opacity(0.92)).on_click(click(on, || Act::ToggleAdd)))
                    .child(svg().path("icons/plus.svg").size(px(12.)).text_color(c(0x000000)))
                    .child("Add"),
            )
        })
        .when(m.adding, |d| d.child(add_picker(m, store, on, cx)));
    let mut rows = div().mb(px(10.)).rounded(px(12.)).border_1().border_color(c(0x262626)).overflow_hidden();
    for (i, t) in targets.iter().enumerate() {
        let on_ = t.key == m.target;
        let key = t.key.clone();
        rows = rows.child(
            div()
                .id(SharedString::from(format!("ce-target-{}", t.key)))
                .h(px(if i == 0 { 46. } else { 47. }))
                .pl(px(8.))
                .pr(px(14.))
                .flex()
                .items_center()
                .gap(px(12.))
                .bg(c(if on_ { 0x171717 } else { 0x0c0c0c }))
                .when(i > 0, |d| d.border_t_1().border_color(c(0x1e1e1e)))
                .when(!on_, |d| d.hover(|d| d.bg(c(0x131313))))
                .when(!m.busy, |d| d.cursor_pointer().on_click(click(on, move || Act::Target(key.clone()))))
                .child(if t.member {
                    div()
                        .size(px(18.))
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(svg().path("icons/user-circle.svg").size(px(16.)).text_color(c(0x6f6f6f)))
                        .into_any_element()
                } else {
                    dot(t.color.unwrap_or(DEFAULT_TINT)).into_any_element()
                })
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_size(px(14.))
                        .line_height(px(24.))
                        .when(on_, |d| d.font_weight(FontWeight::MEDIUM))
                        .text_color(c(if on_ { 0xffffff } else { 0xe8e8e8 }))
                        .child(t.label.clone()),
                )
                .child(div().ml_auto().flex().items_center().gap(px(12.)).when(!t.set, |d| {
                    d.child(div().flex_shrink_0().text_size(px(12.5)).line_height(px(24.)).text_color(c(0x6f6f6f)).child("no rules yet"))
                }).child(div().size(px(16.)).flex_shrink_0().flex().items_center().justify_center().when(on_, |d| {
                    d.child(svg().path("icons/check.svg").size(px(16.)).text_color(c(PRIMARY)))
                }))),
        );
    }
    out.push(
        block(
            first,
            div()
                .child(head)
                .child(lede(if m.is_category {
                    "Channels synced to this category follow these rules."
                } else {
                    "Pick who to set rules for. Anything left alone follows what they hold across the community."
                }))
                .child(rows),
        )
        .into_any_element(),
    );
    for (_, title, _, perms) in GROUPS {
        let mut list = div().flex().flex_col();
        for (i, perm) in perms.iter().enumerate() {
            let closed = m.closed_to_everyone(perm.bit);
            let live = m.can_touch(perm.bit) && !closed && !m.busy;
            list = list.child(
                div()
                    .py(px(6.))
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .when(i > 0, |d| d.border_t_1().border_color(c(0x1a1a1a)))
                    .when(!m.can_touch(perm.bit) || closed, |d| d.opacity(0.5))
                    .child(perm_text(
                        perm.name,
                        if closed { "A private channel opens only to the roles and members you allow." } else { perm.desc },
                    ))
                    .child(tri(m, perm, live, on)),
            );
        }
        out.push(
            block(
                false,
                div()
                    .child(
                        div()
                            .mb(px(20.))
                            .flex()
                            .text_size(px(24.))
                            .line_height(px(28.))
                            .font_weight(FontWeight::BOLD)
                            .text_color(c(0xffffff))
                            .child(title)
                            .children(open_label.clone().map(|l| {
                                div().ml(px(7.)).font_weight(FontWeight::NORMAL).text_color(c(0x8a8a8a)).child(format!("for {l}"))
                            })),
                    )
                    .child(list),
            )
            .into_any_element(),
        );
    }
    out
}

fn save_bar(m: &ChannelEditor, on: &OnAct, cx: &App) -> Option<AnyElement> {
    let dirty = m.dirty(cx);
    if !dirty && !m.busy && m.error.is_none() {
        return None;
    }
    let warn = m.warn() && dirty;
    let open = m.perms.as_ref().map(|_| m.targets_label(cx)).flatten();
    let (text, err): (String, bool) = if m.busy {
        ("Saving...".into(), false)
    } else if warn {
        (
            if m.warn_switch {
                format!("Save or reset the rules for {} first", open.unwrap_or_else(|| "this target".into()))
            } else {
                "Save or reset your changes before closing".into()
            },
            true,
        )
    } else if let Some(e) = m.error.clone() {
        (e, true)
    } else {
        ("You have unsaved changes".into(), false)
    };
    let live = !m.busy && dirty;
    Some(
        div()
            .absolute()
            .bottom(px(22.))
            .left_0()
            .right_0()
            .flex()
            .justify_center()
            .px(px(22.))
            .child(
                div()
                    .w_full()
                    .max_w(px(600.))
                    .flex()
                    .items_center()
                    .gap(px(16.))
                    .pl(px(18.))
                    .pr(px(10.))
                    .py(px(10.))
                    .rounded(px(12.))
                    .border_1()
                    .border_color(if warn { c(crate::timeline::DANGER) } else { c(0x2c2c2c) })
                    .bg(c(0x151515))
                    .shadow(vec![gpui::BoxShadow {
                        color: ca(0, 0, 0, 0.55),
                        offset: gpui::point(px(0.), px(10.)),
                        blur_radius: px(30.),
                        spread_radius: px(0.),
                        inset: false,
                    }])
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(14.))
                            .line_height(px(24.))
                            .text_color(if err { c(crate::timeline::DANGER) } else { c(0xe6e6e6) })
                            .child(text),
                    )
                    .child(
                        div()
                            .flex()
                            .gap(px(6.))
                            .child(text_btn("ce-reset", "Reset", live, click(on, || Act::Reset)))
                            .child(save_btn("ce-save", "Save Changes", live && m.valid(cx), click(on, || Act::Save))),
                    ),
            )
            .into_any_element(),
    )
}

impl ChannelEditor {
    fn targets_label(&self, _cx: &App) -> Option<String> {
        let p = self.perms.as_ref()?;
        Some(match self.target.split_once(':') {
            None => "@everyone".into(),
            Some(("role", id)) => p.roles.iter().find(|r| r.id == id).map_or("Deleted role".into(), |r| r.name.clone()),
            Some((_, id)) => p
                .members
                .iter()
                .find(|m| m.0 == id)
                .map_or_else(|| format!("{}…", &id[..id.len().min(10)]), |m| short_npub(&m.1)),
        })
    }
}

pub fn view(m: &ChannelEditor, store: &Store, on: &OnAct, cx: &App) -> impl IntoElement + use<> {
    let sections = m.sections();
    let section = sections.iter().find(|s| s.id == m.section).unwrap_or(&sections[0]);
    let active = m
        .anchor
        .filter(|a| section.anchors.iter().any(|x| x.id == *a))
        .unwrap_or(section.anchors[0].id);
    let glyph = if m.is_category {
        "icons/folder.svg"
    } else if m.saved.view == "voice" {
        "icons/volume-max.svg"
    } else if m.private {
        "icons/locked.svg"
    } else {
        "icons/channel-hash.svg"
    };
    let title = if m.saved.name.is_empty() {
        if m.is_category { "Category".to_string() } else { "Channel".to_string() }
    } else {
        m.saved.name.clone()
    };
    let head = div()
        .mx(px(10.))
        .mb(px(17.))
        .pb(px(16.))
        .border_b_1()
        .border_color(c(0x262626))
        .flex()
        .items_center()
        .gap(px(8.))
        .child(div().ml(px(-4.)).size(px(24.)).flex_shrink_0().child(svg().path(glyph).size_full().text_color(c(0xffffff))))
        .child(div().min_w_0().truncate().font_weight(FontWeight::BOLD).text_size(px(14.)).line_height(px(17.)).text_color(c(0xffffff)).child(title))
        .into_any_element();
    let footer = (m.is_category || !m.primary).then(|| {
        let what = if m.is_category { "Delete Category" } else { "Delete Channel" };
        div()
            .mt(px(12.))
            .pt(px(16.))
            .px(px(8.))
            .border_t_1()
            .border_color(c(0x262626))
            .child(
                div()
                    .id("ce-delete")
                    .h(px(38.))
                    .px(px(11.))
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .rounded(px(8.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_size(px(15.))
                    .line_height(px(18.))
                    .text_color(c(crate::timeline::DANGER))
                    .hover(|d| d.bg(c(0x151515)))
                    .when(m.busy, |d| d.opacity(0.45))
                    .when(!m.busy, |d| d.cursor_pointer().on_click(click(on, || Act::Delete)))
                    .child(svg().path("icons/trash.svg").size(px(17.)).flex_shrink_0().text_color(c(crate::timeline::DANGER)))
                    .child(div().min_w_0().truncate().child(what)),
            )
            .into_any_element()
    });
    let (a, b, d) = (on.clone(), on.clone(), on.clone());
    let nav_on = NavOn {
        section: Rc::new(move |id, w, cx| a(Act::Section(id), w, cx)),
        anchor: Rc::new(move |(s, x), w, cx| b(Act::Anchor(s, x), w, cx)),
        clear: Rc::new(move |_, w, cx| d(Act::ClearSearch, w, cx)),
    };
    let nav = crate::settings::section_nav(&sections, section.id, &[], active, &m.search, head, footer, 249., 0x0b0b0b, &nav_on, cx);
    let blocks = match section.id {
        "overview" => overview(m, on, cx),
        _ => permissions(m, store, on, cx),
    };
    let close = on.clone();
    let main = div()
        .flex_1()
        .min_w_0()
        .h_full()
        .relative()
        .flex()
        .flex_col()
        .child(
            div()
                .h(px(50.))
                .flex_shrink_0()
                .pl(px(28.))
                .pr(px(18.))
                .flex()
                .items_center()
                .justify_between()
                .border_b_1()
                .border_color(c(0x171717))
                .child(div().text_size(px(16.)).line_height(px(19.)).text_color(c(0xffffff)).child(section.label))
                .child(
                    div()
                        .id("ce-close")
                        .mt(px(2.))
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap(px(2.))
                        .cursor_pointer()
                        .group("ce-close")
                        .on_click(move |_, w, cx| close(Act::Close, w, cx))
                        .child(
                            div()
                                .size(px(32.))
                                .rounded_full()
                                .border_1()
                                .border_color(c(0x4a4a4a))
                                .flex()
                                .items_center()
                                .justify_center()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_size(px(12.))
                                .text_color(c(0x9a9a9a))
                                .group_hover("ce-close", |d| d.text_color(c(0xffffff)).border_color(c(0x6a6a6a)))
                                .child("✕"),
                        )
                        .child(div().font_weight(FontWeight::SEMIBOLD).text_size(px(9.)).line_height(px(10.)).text_color(c(0x9a9a9a)).child("ESC")),
                ),
        )
        .child(
            div()
                .id(SharedString::from(format!("ce-scroll-{}", section.id)))
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .track_scroll(&m.scroll)
                .pt(px(12.))
                .pb(px(140.))
                .when(m.busy, |d| d.opacity(0.6))
                .children(blocks.into_iter().map(|b| div().w_full().max_w(px(720.)).mx_auto().px(px(62.)).child(b))),
        )
        .children(save_bar(m, on, cx));
    let card = div()
        .id("ce-card")
        .w_full()
        .max_w(px(1182.))
        .h_full()
        .max_h(px(862.))
        .flex()
        .rounded(px(18.))
        .border_1()
        .border_color(c(0x2c2e2f))
        .bg(c(0x050505))
        .overflow_hidden()
        .font_family(RUBIK)
        .occlude()
        .on_click(|_, _, cx| cx.stop_propagation())
        .child(nav)
        .child(main);
    let dismiss = on.clone();
    gpui::deferred(
        div()
            .id("ce-overlay")
            .absolute()
            .inset_0()
            .bg(ca(0, 0, 0, 0.85))
            .flex()
            .items_center()
            .justify_center()
            .p(px(25.))
            .occlude()
            .on_click(move |_, w, cx| dismiss(Act::Close, w, cx))
            .child(card),
    )
    .with_priority(12)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn perms_parse_and_targets_sort() {
        let v = serde_json::json!({
            "activated": true, "can_manage": true, "synced": null, "category": "ab",
            "grantable": "18446744073709551615",
            "roles": [{"role_id": "r2", "name": "Low", "color": 0, "position": 2}, {"role_id": "r1", "name": "High", "color": 5, "position": 1}],
            "overwrites": [
                {"kind": "everyone", "id": null, "allow": "0", "deny": "16384"},
                {"kind": "role", "id": "r2", "allow": "16384", "deny": "0"},
                {"kind": "role", "id": "r1", "allow": "0", "deny": "0"},
            ],
            "members": [],
        });
        let p = Perms::parse(&v);
        assert!(p.open() && !p.synced);
        assert_eq!(p.grantable, u64::MAX);
        assert_eq!(p.stored("everyone"), (0, 1 << 14));
        assert_eq!(p.stored("role:r2"), (1 << 14, 0));
    }
}
