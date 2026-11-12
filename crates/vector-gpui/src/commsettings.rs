//! Community Settings (`community/CommunitySettings.svelte`): the settings layout in a card over
//! the app. Overview, Relays and Bans edit a draft behind one save bar; closing with unsaved
//! changes is refused, and the bar says why.

use std::collections::HashSet;
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Entity, FontWeight, ScrollHandle, SharedString, Window, div, img, px, svg,
};

use crate::composer::TextInput;
use crate::data::Store;
use crate::settings::{Anchor, Category, NavOn, anchor};
use crate::theme::*;

pub const NAME_MAX: usize = 32;
pub const DESCRIPTION_MAX: usize = 500;
pub const RELAYS_MAX: usize = 5;
pub const VOICE_RELAYS_MAX: usize = 8;
pub const BANS_MAX: usize = 500;

pub struct Saved {
    pub name: String,
    pub description: String,
    pub relays: Vec<String>,
    pub voice: Vec<String>,
}

#[derive(Default)]
pub struct Audit {
    pub loaded: bool,
    pub loading: bool,
    pub error: Option<String>,
    pub entries: Vec<serde_json::Value>,
    pub compacted: bool,
    pub action: &'static str,
    pub open: HashSet<String>,
    pub shown: usize,
    pub filter_open: bool,
}

pub const AUDIT_PAGE: usize = 50;

/// The Sync Report: how this device reads the community's control plane.
#[derive(Default)]
pub struct Report {
    pub loading: bool,
    pub error: Option<String>,
    pub data: Option<serde_json::Value>,
    pub text: SharedString,
}

/// A picked image awaiting Save: the file, the part to upload, and what shows meanwhile.
pub struct Pick {
    pub path: std::path::PathBuf,
    pub crop: Option<vector_core::image_crop::CropRect>,
    pub preview: std::sync::Arc<gpui::RenderImage>,
    pub natural: (u32, u32),
}

pub struct CommSettings {
    pub community: String,
    pub section: &'static str,
    pub anchor: Option<&'static str>,
    pub search: Entity<TextInput>,
    pub scroll: ScrollHandle,
    pub caps: HashSet<String>,
    pub saved: Saved,
    pub name: Entity<TextInput>,
    pub description: Entity<TextInput>,
    pub relays: Vec<String>,
    pub voice: Vec<String>,
    pub relay_input: Entity<TextInput>,
    pub voice_input: Entity<TextInput>,
    pub relay_edit: Option<(usize, Entity<TextInput>)>,
    pub relay_error: Option<String>,
    pub voice_error: Option<String>,
    pub bans: Vec<String>,
    pub ban_query: Entity<TextInput>,
    pub ban_sel: HashSet<String>,
    pub unbanning: bool,
    pub audit: Audit,
    pub advanced: bool,
    pub report: Option<Report>,
    pub roles: crate::roles::Roles,
    pub role_name: Entity<TextInput>,
    pub role_query: Entity<TextInput>,
    pub role_hex: Option<Entity<TextInput>>,
    pub audit_query: Entity<TextInput>,
    pub saving: bool,
    /// Upload percent while an image saves.
    pub progress: Option<u8>,
    pub icon_pick: Option<Pick>,
    pub icon_removed: bool,
    pub banner_pick: Option<Pick>,
    pub banner_removed: bool,
    /// A refused close: the bar says why until this passes.
    pub warn_until: Option<std::time::Instant>,
}

pub enum Act {
    PickIcon,
    PickBanner,
    RemoveIcon,
    RemoveBanner,
    Close,
    Section(&'static str),
    Anchor(&'static str, &'static str),
    ClearSearch,
    AddRelay,
    RemoveRelay(usize),
    EditRelay(usize),
    CommitEdit,
    CancelEdit,
    AddVoice,
    RemoveVoice(usize),
    Ban(String),
    BanAll,
    ClearBans,
    Unban,
    Reset,
    Save,
    Learn,
    AuditRefresh,
    AuditToggle(String),
    AuditMore,
    AuditFilter,
    AuditPick(&'static str),
    SyncReport,
    ReportClose,
    ReportCopy,
    Role(crate::roles::Act),
}

pub type OnAct = Rc<dyn Fn(Act, &mut Window, &mut App)>;

pub(crate) fn click(
    on: &OnAct,
    act: impl Fn() -> Act + 'static,
) -> impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static {
    let on = on.clone();
    move |_, w, cx| on(act(), w, cx)
}

impl CommSettings {
    pub fn can(&self, cap: &str) -> bool {
        self.caps.contains(cap)
    }

    pub fn overview_dirty(&self, cx: &App) -> bool {
        self.name.read(cx).text() != self.saved.name
            || self.description.read(cx).text() != self.saved.description
            || self.icon_pick.is_some()
            || self.banner_pick.is_some()
            || self.icon_removed
            || self.banner_removed
    }

    pub fn roles_dirty(&self, cx: &App) -> bool {
        self.roles.order.is_some() || self.roles.edit_dirty(self.role_name.read(cx).text())
    }

    pub fn dirty(&self, cx: &App) -> bool {
        self.overview_dirty(cx)
            || self.relays != self.saved.relays
            || self.voice != self.saved.voice
            || self.roles_dirty(cx)
    }

    pub fn warn(&self) -> bool {
        self.warn_until
            .is_some_and(|t| std::time::Instant::now() < t)
    }

    pub fn sections(&self) -> Vec<Category> {
        let section = |id, label, icon, anchors: Vec<Anchor>| Category {
            id,
            label,
            icon,
            danger: false,
            anchors,
        };
        let mut out = vec![
            section(
                "overview",
                "Overview",
                "info",
                vec![
                    anchor(
                        "identity",
                        "Icon & Name",
                        "icon avatar logo picture image name title rename",
                    ),
                    anchor(
                        "banner",
                        "Banner",
                        "banner cover header art background picture image",
                    ),
                    anchor(
                        "description",
                        "Description",
                        "description about bio summary",
                    ),
                ],
            ),
            section(
                "relays",
                "Relays",
                "globe",
                vec![
                    anchor(
                        "relay-list",
                        "Hosting Relays",
                        "relays servers hosting network nostr",
                    ),
                    anchor(
                        "voice-relays",
                        "Voice Relays",
                        "voice relays iroh calls audio",
                    ),
                ],
            ),
        ];
        if self.can("manage_roles") {
            out.push(section(
                "roles",
                "Roles",
                "shield-filled",
                crate::roles::anchors(&self.roles),
            ));
        }
        if self.can("ban") {
            out.push(section(
                "bans",
                "Bans",
                "x-user",
                vec![anchor(
                    "ban-list",
                    "Banned Members",
                    "bans banned unban blocked removed",
                )],
            ));
        }
        if self.can("view_audit_log") {
            out.push(section(
                "audit",
                "Audit Log",
                "clock",
                vec![anchor(
                    "audit-list",
                    "Audit Log",
                    "audit log history changes actions who when record",
                )],
            ));
        }
        out
    }
}

/// `relayAddress`: `raw` as a relay the list can hold, or why not.
pub fn relay_address(raw: &str, list: &[String], except: Option<usize>) -> Result<String, String> {
    let mut text = raw.trim().to_string();
    if text.is_empty() {
        return Err(String::new());
    }
    if !text.contains("://") {
        text = format!("wss://{text}");
    }
    let (scheme, rest) = text.split_once("://").unwrap_or(("", ""));
    let host = rest
        .split(['/', ':', '?', '#'])
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    if host.is_empty() || host.contains(char::is_whitespace) {
        return Err("That isn't a relay address.".into());
    }
    let native = host.ends_with(".onion") || host.ends_with(".i2p");
    let scheme = scheme.to_ascii_lowercase();
    if !(scheme == "wss" || (scheme == "ws" && native)) {
        return Err("Use wss://, or ws:// for an .onion or .i2p address.".into());
    }
    let address = text.trim_end_matches('/').to_string();
    let same = |a: &String| a.trim_end_matches('/').eq_ignore_ascii_case(&address);
    if list
        .iter()
        .enumerate()
        .any(|(i, r)| Some(i) != except && same(r))
    {
        return Err("That relay is already listed.".into());
    }
    Ok(address)
}

/// A voice relay as the list holds it (`https://host/`), or why not.
pub fn voice_address(raw: &str, list: &[String]) -> Result<String, String> {
    let mut text = raw.trim().to_string();
    if text.is_empty() {
        return Err(String::new());
    }
    if !text.contains("://") {
        text = format!("https://{text}");
    }
    let (scheme, rest) = text.split_once("://").unwrap_or(("", ""));
    if !scheme.eq_ignore_ascii_case("https") {
        return Err("A voice relay is an https:// address.".into());
    }
    if rest.split(['/', '?', '#']).next().unwrap_or("").is_empty() {
        return Err("That isn't a relay address.".into());
    }
    let normal = format!("{}/", text.trim_end_matches('/'));
    if list.contains(&normal) {
        return Err("That relay is already listed.".into());
    }
    if list.len() >= VOICE_RELAYS_MAX {
        return Err(format!("At most {VOICE_RELAYS_MAX} voice relays."));
    }
    Ok(normal)
}

pub(crate) fn heading(text: &'static str) -> impl IntoElement {
    div()
        .mb(px(20.))
        .text_size(px(24.))
        .line_height(px(28.))
        .font_weight(FontWeight::BOLD)
        .text_color(c(0xffffff))
        .child(text)
}

pub(crate) fn lede(text: &'static str) -> impl IntoElement {
    div()
        .mt(px(-8.))
        .mb(px(18.))
        .text_size(px(14.))
        .line_height(px(21.))
        .text_color(c(0x9a9a9a))
        .child(text)
}

pub(crate) fn hint(text: impl Into<SharedString>) -> gpui::Div {
    div()
        .text_size(px(12.5))
        .line_height(px(18.125))
        .text_color(c(0x7a7a7a))
        .child(text.into())
}

pub(crate) fn block(first: bool, body: impl IntoElement) -> impl IntoElement {
    div()
        .pt(px(30.))
        .pb(px(34.))
        .when(!first, |d| d.border_t_1().border_color(c(0x262626)))
        .child(body)
}

pub(crate) fn field_head(
    label: &'static str,
    count: Option<(usize, usize, bool)>,
) -> impl IntoElement {
    div()
        .mb(px(8.))
        .flex()
        .items_center()
        .justify_between()
        .child(
            div()
                .font_weight(FontWeight::MEDIUM)
                .text_size(px(13.))
                .line_height(px(24.))
                .text_color(c(0xcfcfcf))
                .child(label),
        )
        .children(count.map(|(n, max, warn)| {
            div()
                .text_size(px(12.))
                .line_height(px(24.))
                .text_color(if warn {
                    c(crate::timeline::DANGER)
                } else {
                    c(0x6f6f6f)
                })
                .child(format!("{n}/{max}"))
        }))
}

pub(crate) fn input_box(input: &Entity<TextInput>, h: f32, editable: bool) -> gpui::Div {
    div()
        .h(px(h))
        .px(px(13.))
        .rounded(px(10.))
        .border_1()
        .border_color(c(0x2c2c2c))
        .bg(c(0x111111))
        .flex()
        .items_center()
        .overflow_hidden()
        .when(!editable, |d| d.opacity(0.6))
        .child(div().flex_1().min_w_0().child(input.clone()))
}

pub(crate) fn save_btn(
    id: impl Into<gpui::ElementId>,
    label: impl Into<SharedString>,
    live: bool,
    on: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(id.into())
        .h(px(34.))
        .px(px(14.))
        .flex_shrink_0()
        .rounded(px(8.))
        .bg(c(PRIMARY))
        .flex()
        .items_center()
        .justify_center()
        .font_weight(FontWeight::SEMIBOLD)
        .text_size(px(14.))
        .text_color(c(0x000000))
        .when(!live, |d| d.opacity(0.45))
        .when(live, |d| {
            d.cursor_pointer().hover(|d| d.opacity(0.9)).on_click(on)
        })
        .child(label.into())
}

fn edit_veil() -> gpui::Div {
    div()
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .bg(ca(0, 0, 0, 0.55))
        .invisible()
        .child(
            svg()
                .path("icons/edit.svg")
                .size(px(22.))
                .text_color(c(0xffffff)),
        )
}

fn new_tag() -> gpui::Div {
    div()
        .absolute()
        .right(px(6.))
        .bottom(px(6.))
        .px(px(7.))
        .py(px(1.))
        .rounded(px(8.))
        .bg(c(PRIMARY))
        .text_color(c(0x000000))
        .font_weight(FontWeight::BOLD)
        .text_size(px(10.))
        .line_height(px(16.))
        .child("New")
}

fn remove_btn(
    id: &'static str,
    label: &'static str,
    on: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .px(px(6.))
        .py(px(2.))
        .text_size(px(12.5))
        .line_height(px(24.))
        .text_color(c(0x8a8a8a))
        .cursor_pointer()
        .hover(|d| d.text_color(c(crate::timeline::DANGER)))
        .on_click(on)
        .child(label)
}

/// A pick's crop, drawn at `width` like `.banner-art` with a rect.
fn cut(p: &Pick, width: f32, height: f32, radius: f32) -> AnyElement {
    let (nw, nh) = (p.natural.0.max(1) as f32, p.natural.1.max(1) as f32);
    let (x, y, w) = match p.crop {
        Some(r) => (r.x as f32, r.y as f32, r.w.max(1) as f32),
        None => {
            let w = nw.min(nh * width / height);
            ((nw - w) / 2., (nh - w * height / width) / 2., w)
        }
    };
    let k = width / w;
    div()
        .relative()
        .w(px(width))
        .h(px(height))
        .overflow_hidden()
        .rounded(px(radius))
        .bg(c(0x484848))
        .child(
            img(p.preview.clone())
                .absolute()
                .left(px(-x * k))
                .top(px(-y * k))
                .w(px(nw * k))
                .h(px(nh * k)),
        )
        .into_any_element()
}

fn overview(m: &CommSettings, store: &Store, on: &OnAct, cx: &App) -> AnyElement {
    let editable = m.can("manage_metadata");
    let name_len = m.name.read(cx).text().chars().count();
    let desc_len = m.description.read(cx).text().chars().count();
    let saved_icon = (!m.icon_removed)
        .then(|| store.icon(&m.community))
        .flatten();
    let has_icon = m.icon_pick.is_some() || saved_icon.is_some();
    let icon: AnyElement = match (&m.icon_pick, saved_icon) {
        (Some(p), _) => cut(p, 88., 88., 24.),
        (None, Some(path)) => img(path)
            .size_full()
            .rounded(px(24.))
            .object_fit(gpui::ObjectFit::Cover)
            .into_any_element(),
        (None, None) => img("icons/group-placeholder.svg")
            .size_full()
            .rounded(px(24.))
            .into_any_element(),
    };
    let saved_banner = (!m.banner_removed)
        .then(|| store.banner(&m.community))
        .flatten();
    let has_banner = m.banner_pick.is_some() || saved_banner.is_some();
    let banner: AnyElement = match (&m.banner_pick, saved_banner) {
        (Some(p), _) => cut(p, 238., 70., 12.),
        (None, Some(path)) => crate::community::banner_art(path, 238.)
            .rounded(px(12.))
            .into_any_element(),
        (None, None) => div()
            .w(px(238.))
            .h(px(70.))
            .rounded(px(12.))
            .border_1()
            .border_dashed()
            .border_color(c(0x333333))
            .flex()
            .items_center()
            .justify_center()
            .gap(px(10.))
            .text_color(white_a(0.45))
            .when(editable, |d| {
                d.group_hover("cs-banner", |d| {
                    d.border_color(c(0x555555)).text_color(white_a(0.7))
                })
            })
            .child(
                svg()
                    .path("icons/image.svg")
                    .size(px(24.))
                    .text_color(white_a(0.45)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(
                        div()
                            .text_size(px(12.5))
                            .line_height(px(15.))
                            .child(if editable {
                                "Add a banner"
                            } else {
                                "No banner"
                            }),
                    )
                    .when(editable, |d| {
                        d.child(
                            div()
                                .text_size(px(11.))
                                .line_height(px(13.2))
                                .opacity(0.7)
                                .child("(952 × 280)"),
                        )
                    }),
            )
            .into_any_element(),
    };
    div()
        .child(block(
            true,
            div().child(heading("Icon & Name")).child(
                div()
                    .flex()
                    .items_start()
                    .gap(px(22.))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .items_center()
                            .gap(px(6.))
                            .flex_shrink_0()
                            .child(
                                div()
                                    .id("cs-icon")
                                    .group("cs-icon")
                                    .relative()
                                    .size(px(88.))
                                    .rounded(px(24.))
                                    .overflow_hidden()
                                    .bg(c(0x161616))
                                    .child(icon)
                                    .when(editable, |d| d.cursor_pointer().on_click(click(on, || Act::PickIcon)).child(edit_veil().group_hover("cs-icon", |d| d.visible())))
                                    .when(m.icon_pick.is_some(), |d| d.child(new_tag())),
                            )
                            .when(editable && has_icon, |d| d.child(remove_btn("cs-icon-x", "Remove", click(on, || Act::RemoveIcon)))),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(field_head("Community Name", Some((name_len, NAME_MAX, name_len == 0))))
                            .child(input_box(&m.name, 42., editable))
                            .child(hint("The icon shows on the rail and beside every invite. Square images crop best.").mt(px(10.))),
                    ),
            ),
        ))
        .child(block(
            false,
            div().child(heading("Banner")).child(
                div()
                    .flex()
                    .flex_col()
                    .items_start()
                    .gap(px(10.))
                    .child(
                        div()
                            .id("cs-banner")
                            .group("cs-banner")
                            .relative()
                            .w(px(238.))
                            .rounded(px(12.))
                            .overflow_hidden()
                            .bg(c(0x161616))
                            .child(banner)
                            .when(editable, |d| d.cursor_pointer().on_click(click(on, || Act::PickBanner)))
                            .when(editable && has_banner, |d| d.child(edit_veil().group_hover("cs-banner", |d| d.visible())))
                            .when(m.banner_pick.is_some(), |d| d.child(new_tag())),
                    )
                    .when(editable && has_banner, |d| d.child(remove_btn("cs-banner-x", "Remove Banner", click(on, || Act::RemoveBanner)))),
            ),
        ))
        .child(block(
            false,
            div()
                .child(heading("Description"))
                .child(field_head("What this community is about", Some((desc_len, DESCRIPTION_MAX, false))))
                .child(
                    div()
                        .h(px(136.5))
                        .px(px(13.))
                        .py(px(11.))
                        .rounded(px(10.))
                        .border_1()
                        .border_color(c(0x2c2c2c))
                        .bg(c(0x111111))
                        .overflow_hidden()
                        .when(!editable, |d| d.opacity(0.6))
                        .child(m.description.clone()),
                ),
        ))
        .into_any_element()
}

fn relay_row(
    first: bool,
    icon: &'static str,
    url: &str,
    editing: Option<&Entity<TextInput>>,
    actions: Vec<AnyElement>,
) -> impl IntoElement {
    let (scheme, rest) = match url.find("://") {
        Some(i) => (&url[..i + 3], url[i + 3..].trim_end_matches('/')),
        None => ("", url),
    };
    div()
        .h(px(if first { 46. } else { 47. }))
        .px(px(16.))
        .flex()
        .items_center()
        .gap(px(12.))
        .bg(c(0x0c0c0c))
        .when(!first, |d| d.border_t_1().border_color(c(0x1e1e1e)))
        .child(
            svg()
                .path(icon)
                .size(px(16.))
                .flex_shrink_0()
                .text_color(c(0x7a7a7a)),
        )
        .when_some(editing, |d, input| {
            d.child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h(px(30.))
                    .px(px(8.))
                    .flex()
                    .items_center()
                    .rounded(px(6.))
                    .border_1()
                    .border_color(c(ACCENT))
                    .bg(c(0x050505))
                    .overflow_hidden()
                    .child(input.clone()),
            )
        })
        .when(editing.is_none(), |d| {
            d.child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(14.))
                    .line_height(px(24.))
                    .text_color(c(0xe8e8e8))
                    .child(
                        gpui::StyledText::new(format!("{scheme}{rest}")).with_highlights([(
                            0..scheme.len(),
                            gpui::HighlightStyle {
                                color: Some(c(0x6a6a6a)),
                                ..Default::default()
                            },
                        )]),
                    ),
            )
        })
        .children(actions)
}

fn relay_action(
    id: SharedString,
    icon: &'static str,
    on: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    div()
        .id(id)
        .size(px(26.))
        .flex_shrink_0()
        .rounded(px(6.))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .hover(|d| d.bg(white_a(0.06)))
        .on_click(on)
        .child(svg().path(icon).size(px(14.)).text_color(c(0x7a7a7a)))
        .into_any_element()
}

fn add_row(
    id: &'static str,
    input: &Entity<TextInput>,
    full: bool,
    live: bool,
    on: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .mt(px(12.))
        .flex()
        .gap(px(8.))
        .child(input_box(input, 35., !full).px(px(12.)).flex_1())
        .child(save_btn(id, "Add", live && !full, on).into_any_element())
}

fn relays(m: &CommSettings, on: &OnAct, cx: &App) -> AnyElement {
    let editable = m.can("manage_metadata");
    let many = m.relays.len() > 1;
    let list = div()
        .rounded(px(12.))
        .border_1()
        .border_color(c(0x262626))
        .overflow_hidden()
        .children(m.relays.iter().enumerate().map(|(i, url)| {
            let editing = m
                .relay_edit
                .as_ref()
                .filter(|(at, _)| *at == i)
                .map(|(_, input)| input);
            let mut actions = Vec::new();
            if editable && editing.is_none() {
                actions.push(relay_action(
                    SharedString::from(format!("cs-relay-e-{i}")),
                    "icons/edit.svg",
                    click(on, move || Act::EditRelay(i)),
                ));
                if many {
                    actions.push(relay_action(
                        SharedString::from(format!("cs-relay-x-{i}")),
                        "icons/x.svg",
                        click(on, move || Act::RemoveRelay(i)),
                    ));
                }
            }
            relay_row(i == 0, "icons/globe.svg", url, editing, actions)
        }));
    let voice = div()
        .rounded(px(12.))
        .border_1()
        .border_color(c(0x262626))
        .overflow_hidden()
        .map(|d| {
            if m.voice.is_empty() {
                d.child(
                    div()
                        .mx(px(16.))
                        .my(px(14.))
                        .flex()
                        .items_center()
                        .gap(px(4.))
                        .child(hint("Using iroh's public relays, run by n0."))
                        .child(
                            div()
                                .id("cs-iroh")
                                .cursor_pointer()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_size(px(12.5))
                                .line_height(px(18.125))
                                .text_color(c(ACCENT))
                                .hover(|d| d.underline())
                                .on_click(click(on, || Act::Learn))
                                .child("Learn more"),
                        ),
                )
            } else {
                d.children(m.voice.iter().enumerate().map(|(i, url)| {
                    let actions = if editable {
                        vec![relay_action(
                            SharedString::from(format!("cs-voice-x-{i}")),
                            "icons/x.svg",
                            click(on, move || Act::RemoveVoice(i)),
                        )]
                    } else {
                        Vec::new()
                    };
                    relay_row(i == 0, "icons/volume-max.svg", url, None, actions)
                }))
            }
        });
    let relay_text = !m.relay_input.read(cx).text().trim().is_empty();
    let voice_text = !m.voice_input.read(cx).text().trim().is_empty();
    div()
        .child(block(
            true,
            div()
                .child(heading("Hosting Relays"))
                .child(lede("Every message, channel and member list in this community lives on these relays, end-to-end encrypted."))
                .child(list)
                .when(editable, |d| {
                    d.child(add_row("cs-relay-add", &m.relay_input, m.relays.len() >= RELAYS_MAX, relay_text, click(on, || Act::AddRelay)))
                        .children(m.relay_error.clone().map(|e| hint(e).mt(px(10.)).text_color(c(crate::timeline::DANGER))))
                        .child(hint("A new relay is checked and given this community's whole history before anyone moves to it, and the old relays tell members where everyone went.").mt(px(10.)))
                }),
        ))
        .child(block(
            false,
            div()
                .child(heading("Voice Relays"))
                .child(lede("Voice channels connect through these relays, end-to-end encrypted. Each relay sees where callers connect from."))
                .child(voice)
                .when(editable, |d| {
                    d.child(add_row("cs-voice-add", &m.voice_input, m.voice.len() >= VOICE_RELAYS_MAX, voice_text, click(on, || Act::AddVoice)))
                        .children(m.voice_error.clone().map(|e| hint(e).mt(px(10.)).text_color(c(crate::timeline::DANGER))))
                }),
        ))
        .into_any_element()
}

/// `MemberRow`: avatar, name and key hint, with `trailing` at the end.
pub(crate) fn member_row(
    store: &Store,
    npub: &str,
    prefix: &str,
    trailing: AnyElement,
    on_click: Option<Box<dyn Fn(&gpui::ClickEvent, &mut Window, &mut App)>>,
) -> gpui::Stateful<gpui::Div> {
    let named = store.profiles.get(npub).is_some_and(|p| {
        !p.name.is_empty() || !p.display_name.is_empty() || !p.nickname.is_empty()
    });
    let name = if named {
        store.name(npub)
    } else {
        short_npub(npub)
    };
    div()
        .id(SharedString::from(format!("{prefix}-{npub}")))
        .group(SharedString::from(format!("{prefix}-{npub}")))
        .h(px(35.))
        .px(px(10.))
        .flex()
        .items_center()
        .rounded(px(6.))
        .hover(|d| d.bg(ca(255, 255, 255, 0.085)))
        .when_some(on_click, |d, f| d.cursor_pointer().on_click(f))
        .child(div().mr(px(10.)).child(crate::ui::avatar(store, npub, 25.)))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .items_center()
                .gap(px(12.))
                .child(
                    div()
                        .truncate()
                        .text_size(px(14.))
                        .line_height(px(24.))
                        .text_color(c(0xf7f4f4))
                        .child(name),
                )
                .when(named, |d| {
                    d.child(
                        div()
                            .flex_shrink_0()
                            .text_size(px(11.5))
                            .line_height(px(24.))
                            .text_color(c(0x6a6a6a))
                            .child(short_npub(npub)),
                    )
                }),
        )
        .child(trailing)
}

pub(crate) fn short_npub(npub: &str) -> String {
    if npub.len() < 20 {
        return npub.to_string();
    }
    format!("{}…{}", &npub[..12], &npub[npub.len() - 6..])
}

pub(crate) fn indicator(on: bool) -> impl IntoElement {
    div()
        .size(px(22.))
        .flex_shrink_0()
        .rounded(px(4.))
        .border_2()
        .border_color(if on { c(PRIMARY) } else { white_a(0.28) })
        .when(on, |d| {
            d.bg(c(PRIMARY))
                .flex()
                .items_center()
                .justify_center()
                .child(
                    svg()
                        .path("icons/check.svg")
                        .size(px(14.))
                        .text_color(c(0x000000)),
                )
        })
}

pub fn ban_rows(m: &CommSettings, store: &Store, cx: &App) -> Vec<String> {
    let q = m.ban_query.read(cx).text().trim().to_lowercase();
    m.bans
        .iter()
        .filter(|n| {
            q.is_empty()
                || format!("{} {}", store.name(n), n)
                    .to_lowercase()
                    .contains(&q)
        })
        .cloned()
        .collect()
}

fn bans(m: &CommSettings, store: &Store, on: &OnAct, cx: &App) -> AnyElement {
    let rows = ban_rows(m, store, cx);
    let shown_sel = rows.iter().filter(|n| m.ban_sel.contains(*n)).count();
    let all = !rows.is_empty() && shown_sel == rows.len();
    let querying = !m.ban_query.read(cx).text().trim().is_empty();
    let list = if rows.is_empty() {
        div()
            .py(px(4.))
            .text_center()
            .text_size(px(12.5))
            .line_height(px(18.))
            .text_color(white_a(0.4))
            .child(if querying {
                "No matches."
            } else {
                "Nobody is banned from this community."
            })
            .into_any_element()
    } else {
        div()
            .child(
                div()
                    .id("cs-bans-all")
                    .h(px(34.))
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .rounded(px(6.))
                    .cursor_pointer()
                    .hover(|d| d.bg(ca(255, 255, 255, 0.085)))
                    .on_click(click(on, || Act::BanAll))
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(13.))
                            .line_height(px(24.))
                            .text_color(c(0x9a9a9a))
                            .child(format!(
                                "{} {} ({})",
                                if all { "Deselect" } else { "Select" },
                                if querying { "matches" } else { "all" },
                                rows.len()
                            )),
                    )
                    .child(indicator(all)),
            )
            .children(rows.into_iter().map(|npub| {
                let picked = m.ban_sel.contains(&npub);
                let id = npub.clone();
                member_row(
                    store,
                    &npub,
                    "cs-ban",
                    indicator(picked).into_any_element(),
                    Some(Box::new(click(on, move || Act::Ban(id.clone())))),
                )
            }))
            .into_any_element()
    };
    block(
        true,
        div()
            .child(heading("Banned Members"))
            .child(lede("A banned member is silenced everywhere in this community and can't rejoin until they're unbanned."))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(14.))
                    .child(search_box(&m.ban_query))
                    .child(div().text_size(px(12.)).line_height(px(24.)).text_color(if m.bans.len() >= BANS_MAX { c(crate::timeline::DANGER) } else { c(0x6f6f6f) }).child(format!("{} of {BANS_MAX}", m.bans.len()))),
            )
            .child(div().mt(px(8.)).when(m.unbanning, |d| d.opacity(0.5)).child(list)),
    )
    .into_any_element()
}

fn audit_member(store: &Store) -> impl Fn(Option<&str>) -> String + '_ {
    move |npub| match npub {
        None => "someone".into(),
        Some(n) if store.profiles.contains_key(n) => store.name(n),
        Some(n) => short_npub(n),
    }
}

pub fn audit_rows<'a>(m: &'a CommSettings, store: &Store, cx: &App) -> Vec<&'a serde_json::Value> {
    let q = m.audit_query.read(cx).text().trim().to_lowercase();
    let member = audit_member(store);
    m.audit
        .entries
        .iter()
        .filter(|e| {
            m.audit.action == "all"
                || crate::audit::family_of(e["action"].as_str().unwrap_or("")) == m.audit.action
        })
        .filter(|e| {
            q.is_empty()
                || crate::audit::plain_text(e, &member)
                    .to_lowercase()
                    .contains(&q)
        })
        .collect()
}

fn audit(m: &CommSettings, store: &Store, on: &OnAct, cx: &App) -> AnyElement {
    use crate::audit::{Tone, details, sentence, tone_of, when};
    let a = &m.audit;
    let member = audit_member(store);
    let rows = audit_rows(m, store, cx);
    let querying = !m.audit_query.read(cx).text().trim().is_empty() || a.action != "all";
    let now = store.now_ms();
    let pick = on.clone();
    let toggle = on.clone();
    let list: AnyElement = if a.loading && a.entries.is_empty() {
        audit_empty("Reading this community's history…").into_any_element()
    } else if let Some(e) = a.error.clone().filter(|_| a.entries.is_empty()) {
        hint(e)
            .my(px(18.))
            .text_center()
            .text_color(c(crate::timeline::DANGER))
            .into_any_element()
    } else {
        let mut list = div()
            .mt(px(6.))
            .mb(px(12.))
            .flex()
            .flex_col()
            .gap(px(2.))
            .when(a.loading, |d| d.opacity(0.55));
        if rows.is_empty() {
            list = list.child(audit_empty(if querying {
                "No matches."
            } else {
                "Nothing has happened here yet."
            }));
        }
        for e in rows.iter().take(a.shown) {
            let id = e["id"].as_str().unwrap_or("").to_string();
            let lines = details(e, &member);
            let open = a.open.contains(&id);
            let badge = match tone_of(e["action"].as_str().unwrap_or("")) {
                Tone::Add => c(ACCENT),
                Tone::Remove => c(crate::timeline::DANGER),
                Tone::Change => c(0xfbbf24),
            };
            let actor = e["actor"].as_str();
            let mut text = String::new();
            let mut bold = Vec::new();
            for (part, b) in sentence(e, &member) {
                if b {
                    bold.push((
                        text.len()..text.len() + part.len(),
                        gpui::HighlightStyle {
                            color: Some(c(0xffffff)),
                            font_weight: Some(FontWeight::SEMIBOLD),
                            ..Default::default()
                        },
                    ));
                }
                text.push_str(&part);
            }
            let openable = !lines.is_empty();
            let on = on.clone();
            let key = id.clone();
            list = list.child(
                div()
                    .id(SharedString::from(format!("audit-{id}")))
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .px(px(12.))
                    .py(px(10.))
                    .rounded(px(8.))
                    .when(open, |d| d.bg(white_a(0.04)))
                    .when(openable, |d| {
                        d.cursor_pointer()
                            .hover(|d| d.bg(white_a(0.04)))
                            .on_click(move |_, w, cx| on(Act::AuditToggle(key.clone()), w, cx))
                    })
                    .child(
                        div()
                            .relative()
                            .size(px(32.))
                            .flex_shrink_0()
                            .child(match actor {
                                Some(n) => crate::ui::avatar(store, n, 32.),
                                None => img("icons/group-placeholder.svg")
                                    .size(px(32.))
                                    .rounded_full()
                                    .into_any_element(),
                            })
                            .child(
                                div()
                                    .absolute()
                                    .right(px(-2.))
                                    .bottom(px(-2.))
                                    .size(px(14.))
                                    .rounded_full()
                                    .border_2()
                                    .border_color(c(0x050505))
                                    .bg(badge),
                            ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_size(px(14.))
                                    .line_height(px(19.6))
                                    .text_color(white_a(0.7))
                                    .child(gpui::StyledText::new(text).with_highlights(bold)),
                            )
                            .child(
                                div()
                                    .mt(px(2.))
                                    .text_size(px(12.))
                                    .line_height(px(24.))
                                    .text_color(white_a(0.38))
                                    .child(when(e["at"].as_u64().unwrap_or(0), now)),
                            ),
                    )
                    .when(openable, |d| {
                        d.child(
                            div()
                                .size(px(18.))
                                .flex_shrink_0()
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(
                                    svg()
                                        .path("icons/chevron-down.svg")
                                        .size(px(10.))
                                        .text_color(white_a(0.38))
                                        .when(open, |s| {
                                            s.with_transformation(gpui::Transformation::rotate(
                                                gpui::radians(std::f32::consts::PI),
                                            ))
                                        }),
                                ),
                        )
                    }),
            );
            if open && openable {
                list = list.child(
                    div()
                        .ml(px(56.))
                        .mb(px(6.))
                        .pt(px(2.))
                        .pr(px(12.))
                        .pb(px(8.))
                        .flex()
                        .flex_col()
                        .gap(px(4.))
                        .text_size(px(13.))
                        .line_height(px(18.2))
                        .text_color(white_a(0.6))
                        .children(lines.into_iter().map(|l| div().child(l))),
                );
            }
        }
        let more = rows.len() > a.shown;
        div()
            .child(list)
            .when(more, |d| d.child(div().flex().justify_center().mb(px(12.)).child(text_btn("audit-more", "Show more", true, click(on, || Act::AuditMore)))))
            .when(a.compacted, |d| d.child(hint("Some history from before a key rotation this device didn't live through can't be read back.")))
            .into_any_element()
    };
    let filter = crate::kit::select(
        "audit-filter",
        170.,
        crate::audit::FAMILIES,
        a.action,
        a.filter_open,
        move |w, cx| toggle(Act::AuditFilter, w, cx),
        move |v, w, cx| pick(Act::AuditPick(v), w, cx),
    );
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
                    .child(
                        div()
                            .text_size(px(24.))
                            .line_height(px(28.))
                            .font_weight(FontWeight::BOLD)
                            .text_color(c(0xffffff))
                            .child("Audit Log"),
                    )
                    .child(
                        div()
                            .flex()
                            .gap(px(4.))
                            .when(m.advanced, |d| {
                                d.child(text_btn(
                                    "audit-report",
                                    "Sync Report",
                                    !m.report.as_ref().is_some_and(|r| r.loading),
                                    click(on, || Act::SyncReport),
                                ))
                            })
                            .child(text_btn(
                                "audit-refresh",
                                "Refresh",
                                !a.loading,
                                click(on, || Act::AuditRefresh),
                            )),
                    ),
            )
            .child(lede(
                "Every change to this community, signed by whoever made it.",
            ))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(14.))
                    .child(search_box(&m.audit_query))
                    .child(div().w(px(170.)).flex_shrink_0().child(filter)),
            )
            .child(div().mt(px(8.)).child(list)),
    )
    .into_any_element()
}

fn audit_empty(text: &'static str) -> impl IntoElement {
    div()
        .my(px(18.))
        .text_center()
        .text_size(px(12.5))
        .line_height(px(18.))
        .text_color(white_a(0.4))
        .child(text)
}

pub(crate) fn search_box(input: &Entity<TextInput>) -> impl IntoElement {
    div()
        .flex_1()
        .h(px(41.))
        .relative()
        .flex()
        .items_center()
        .pl(px(44.))
        .pr(px(12.))
        .rounded(px(8.))
        .border_1()
        .border_color(ca(57, 57, 57, 0.5))
        .overflow_hidden()
        .child(
            svg()
                .path("icons/search.svg")
                .absolute()
                .left(px(12.))
                .top(px(8.))
                .size(px(24.))
                .text_color(white_a(0.4)),
        )
        .child(div().flex_1().min_w_0().child(input.clone()))
}

fn save_bar(m: &CommSettings, on: &OnAct, cx: &App) -> Option<AnyElement> {
    let dirty = m.dirty(cx);
    let warn = m.warn() && dirty;
    let unban = !warn && m.section == "bans" && !m.ban_sel.is_empty();
    if !unban && !dirty {
        return None;
    }
    let n = m.ban_sel.len();
    let (text, actions): (String, Vec<AnyElement>) = if unban {
        let label = if n == m.bans.len() && n > 1 {
            "Unban All".to_string()
        } else {
            format!("Unban {n}")
        };
        (
            if m.unbanning {
                format!("Unbanning {n}...")
            } else {
                format!("{n} {} selected", if n == 1 { "member" } else { "members" })
            },
            vec![
                text_btn(
                    "cs-clear-bans",
                    "Clear",
                    !m.unbanning,
                    click(on, || Act::ClearBans),
                ),
                save_btn("cs-unban", label, !m.unbanning, click(on, || Act::Unban))
                    .into_any_element(),
            ],
        )
    } else {
        let name_empty = m.name.read(cx).text().trim().is_empty();
        (
            if m.saving {
                m.progress
                    .map(|p| format!("Saving {p}%"))
                    .unwrap_or_else(|| "Saving...".into())
                    .into()
            } else if warn {
                "Save or reset your changes before closing".into()
            } else {
                "You have unsaved changes".into()
            },
            vec![
                text_btn("cs-reset", "Reset", !m.saving, click(on, || Act::Reset)),
                save_btn(
                    "cs-save",
                    "Save Changes",
                    !m.saving && !name_empty,
                    click(on, || Act::Save),
                )
                .into_any_element(),
            ],
        )
    };
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
                    .border_color(if warn {
                        c(crate::timeline::DANGER)
                    } else {
                        c(0x2c2c2c)
                    })
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
                            .text_color(if warn {
                                c(crate::timeline::DANGER)
                            } else {
                                c(0xe6e6e6)
                            })
                            .child(text),
                    )
                    .child(div().flex().gap(px(6.)).children(actions)),
            )
            .into_any_element(),
    )
}

pub(crate) fn text_btn(
    id: &'static str,
    label: &'static str,
    live: bool,
    on: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    div()
        .id(id)
        .h(px(34.))
        .px(px(14.))
        .rounded(px(8.))
        .flex()
        .items_center()
        .font_weight(FontWeight::MEDIUM)
        .text_size(px(14.))
        .text_color(c(0xcfcfcf))
        .when(!live, |d| d.opacity(0.45))
        .when(live, |d| {
            d.cursor_pointer()
                .hover(|d| d.bg(white_a(0.06)).text_color(c(0xffffff)))
                .on_click(on)
        })
        .child(label)
        .into_any_element()
}

pub fn view(m: &CommSettings, store: &Store, on: &OnAct, cx: &App) -> impl IntoElement + use<> {
    let sections = m.sections();
    let section = sections
        .iter()
        .find(|s| s.id == m.section)
        .unwrap_or(&sections[0]);
    let active = m
        .anchor
        .filter(|a| section.anchors.iter().any(|x| x.id == *a))
        .unwrap_or(section.anchors[0].id);
    let name = store
        .community(&m.community)
        .map(|c| c.name.clone())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "Community".into());
    let head = div()
        .mx(px(10.))
        .mb(px(17.))
        .pb(px(16.))
        .border_b_1()
        .border_color(c(0x262626))
        .flex()
        .items_center()
        .gap(px(8.))
        .child(crate::community::logo(store, &m.community, 28., 8.))
        .child(
            div()
                .min_w_0()
                .truncate()
                .font_weight(FontWeight::BOLD)
                .text_size(px(14.))
                .line_height(px(17.))
                .text_color(c(0xffffff))
                .child(name),
        )
        .into_any_element();
    let (a, b, d) = (on.clone(), on.clone(), on.clone());
    let nav_on = NavOn {
        section: Rc::new(move |id, w, cx| a(Act::Section(id), w, cx)),
        anchor: Rc::new(move |(s, x), w, cx| b(Act::Anchor(s, x), w, cx)),
        clear: Rc::new(move |_, w, cx| d(Act::ClearSearch, w, cx)),
    };
    let nav = crate::settings::section_nav(
        &sections,
        section.id,
        &[],
        active,
        &m.search,
        head,
        None,
        249.,
        0x0b0b0b,
        &nav_on,
        cx,
    );
    let body = match section.id {
        "overview" => overview(m, store, on, cx),
        "relays" => relays(m, on, cx),
        "bans" => bans(m, store, on, cx),
        "roles" => crate::roles::view(m, store, on, cx),
        _ => audit(m, store, on, cx),
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
                .child(
                    div()
                        .text_size(px(16.))
                        .line_height(px(19.))
                        .text_color(c(0xffffff))
                        .child(section.label),
                )
                .child(
                    div()
                        .id("cs-close")
                        .mt(px(2.))
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap(px(2.))
                        .cursor_pointer()
                        .group("cs-close")
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
                                .group_hover("cs-close", |d| {
                                    d.text_color(c(0xffffff)).border_color(c(0x6a6a6a))
                                })
                                .child("✕"),
                        )
                        .child(
                            div()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_size(px(9.))
                                .line_height(px(10.))
                                .text_color(c(0x9a9a9a))
                                .child("ESC"),
                        ),
                ),
        )
        .child(
            div()
                .id(SharedString::from(format!("cs-scroll-{}", section.id)))
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .track_scroll(&m.scroll)
                .child(
                    div()
                        .w_full()
                        .max_w(px(720.))
                        .mx_auto()
                        .px(px(62.))
                        .pt(px(12.))
                        .pb(px(140.))
                        .child(body),
                ),
        )
        .children(save_bar(m, on, cx));
    let card = div()
        .id("cs-card")
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
    let report = m.report.as_ref().map(|r| sync_report(r, on));
    let dismiss = on.clone();
    gpui::deferred(
        div()
            .id("cs-overlay")
            .absolute()
            .inset_0()
            .bg(ca(0, 0, 0, 0.85))
            .flex()
            .items_center()
            .justify_center()
            .p(px(25.))
            .occlude()
            .on_click(move |_, w, cx| dismiss(Act::Close, w, cx))
            .child(card)
            .children(report),
    )
    .with_priority(12)
}

const REPORT_KINDS: &[(u64, &str)] = &[
    (0, "community"),
    (1, "role"),
    (2, "channel"),
    (3, "grant"),
    (4, "banlist"),
    (8, "invite links"),
    (10, "dissolved"),
    (11, "pins"),
    (13, "role"),
    (14, "permissions"),
    (15, "category"),
    (16, "tenure"),
    (17, "channel sync"),
    (18, "writers"),
];

fn kind_name(k: &serde_json::Value) -> String {
    let n = k.as_u64().unwrap_or(u64::MAX);
    REPORT_KINDS.iter().find(|(id, _)| *id == n).map(|(_, s)| s.to_string()).unwrap_or_else(|| format!("kind {k}"))
}

fn truthy(v: &serde_json::Value) -> bool {
    match v {
        serde_json::Value::Null => false,
        serde_json::Value::Bool(b) => *b,
        serde_json::Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        serde_json::Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

/// Refused, held back, or not this device's newest.
pub fn report_flagged(data: &serde_json::Value) -> Vec<&serde_json::Value> {
    let ver = |v: &serde_json::Value| v["version"].as_f64();
    data["control"]["entities"]
        .as_array()
        .map(|ents| {
            ents.iter()
                .filter(|e| {
                    let newest = &e["newest"];
                    let has = truthy(newest);
                    truthy(&e["refused"])
                        || truthy(&e["carried"])
                        || truthy(&e["kept"])
                        || truthy(&e["mine"])
                        || (has && !truthy(&newest["admitted"]))
                        || (has
                            && truthy(&e["floor"])
                            && ver(newest) > ver(&e["floor"])
                            && !(truthy(&e["folds_to"]) && ver(&e["folds_to"]) >= ver(newest)))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn report_row(e: &serde_json::Value) -> String {
    let v = |x: &serde_json::Value| x["version"].as_u64().map(|n| n.to_string()).unwrap_or_else(|| "–".into());
    let shown = |x: &serde_json::Value| x.as_str().map(str::to_string).unwrap_or_else(|| x.to_string());
    let mut out = format!(
        " {}{} · held v{} · newest v{}",
        shown(&e["entity"]),
        if truthy(&e["mine"]) { " (yours)" } else { "" },
        v(&e["floor"]),
        v(&e["newest"]),
    );
    if truthy(&e["newest"]) {
        out += &format!(" by {}", shown(&e["newest"]["author"]));
    }
    if truthy(&e["refused"]) {
        out += &format!(" · refused: {}", shown(&e["refused"]));
    }
    if truthy(&e["carried"]) {
        out += "· kept, refused";
    }
    if truthy(&e["kept"]) {
        out += "· kept";
    }
    if truthy(&e["newest"]) && !truthy(&e["newest"]["admitted"]) {
        out += "· not admitted";
    }
    out
}

fn report_chip(text: impl Into<SharedString>, bad: bool) -> impl IntoElement {
    let tone = if bad { c(crate::timeline::DANGER) } else { white_a(0.14) };
    div()
        .px(px(10.))
        .py(px(4.))
        .rounded_full()
        .border_1()
        .border_color(tone)
        .text_size(px(12.))
        .line_height(px(24.))
        .text_color(if bad { tone } else { white_a(0.8) })
        .child(text.into())
}

fn report_label(text: &'static str) -> impl IntoElement {
    div()
        .mt(px(16.))
        .mb(px(6.))
        .text_size(px(11.))
        .line_height(px(24.))
        .font_weight(FontWeight::BOLD)
        .text_color(white_a(0.45))
        .child(crate::kit::spaced(text.to_uppercase(), 11. * 0.12))
}

fn sync_report(r: &Report, on: &OnAct) -> AnyElement {
    let shown = |x: &serde_json::Value| x.as_str().map(str::to_string).unwrap_or_else(|| x.to_string());
    let mut body = div()
        .id("sr-body")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .px(px(18.))
        .py(px(14.));
    if r.loading {
        body = body.child(div().p(px(2.)).mb(px(11.)).text_size(px(12.5)).line_height(px(18.)).text_color(white_a(0.4)).child("Reading the control plane…"));
    } else if let Some(err) = &r.error {
        body = body.child(div().text_size(px(13.)).text_color(c(crate::timeline::DANGER)).child(err.clone()));
    } else if let Some(d) = &r.data {
        let ctl = &d["control"];
        let ep = &d["epoch"];
        let mut chips = div().flex().flex_wrap().gap(px(6.)).child(report_chip(
            format!("{} {}", shown(&d["client"]), shown(&d["version"])),
            false,
        ));
        if truthy(ctl) && !truthy(&ctl["error"]) {
            let settled = truthy(&ctl["converged"]);
            let cut = truthy(&ctl["truncated"]);
            chips = chips
                .child(report_chip(format!("Epoch {}", shown(&ctl["root_epoch"])), false))
                .child(report_chip(if settled { "Authority settled" } else { "Authority unsettled" }, !settled))
                .child(report_chip(if cut { "Read cut short" } else { "Read whole" }, cut))
                .child(report_chip(
                    format!("{} edits, {} pages", shown(&ctl["editions"]), shown(&ctl["pages_read"])),
                    false,
                ))
                .when(truthy(&ctl["would_update"]), |d| d.child(report_chip("Unsaved changes", true)));
        } else if truthy(&ctl["error"]) {
            chips = chips.child(report_chip(format!("Control: {}", shown(&ctl["error"])), true));
        }
        if truthy(&ep["error"]) {
            chips = chips.child(report_chip(format!("Epoch: {}", shown(&ep["error"])), true));
        }
        body = body.child(chips);
        let flagged = report_flagged(d);
        if !flagged.is_empty() {
            body = body.child(report_label("Worth a look")).child(
                div().pl(px(18.)).flex().flex_col().children(flagged.into_iter().map(|e| {
                    div()
                        .relative()
                        .text_size(px(13.))
                        .line_height(px(22.))
                        .text_color(white_a(0.85))
                        .child(div().absolute().left(px(-12.)).child("•"))
                        .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .child(div().font_weight(FontWeight::BOLD).child(kind_name(&e["kind"])))
                                .child(report_row(e)),
                        )
                })),
            );
        }
        body = body.child(report_label("Full report")).child(
            div()
                .id("sr-pre")
                .max_h(px(342.))
                .overflow_scroll()
                .px(px(12.))
                .py(px(10.))
                .rounded(px(10.))
                .border_1()
                .border_color(white_a(0.08))
                .bg(c(0x070707))
                .font_family(MONO)
                .text_size(px(11.))
                .line_height(px(16.))
                .text_color(c(0xf6f6f6))
                .whitespace_nowrap()
                .child(r.text.clone()),
        );
    }
    let close = on.clone();
    let card = div()
        .id("sr-card")
        .w_full()
        .max_w(px(720.))
        .max_h_full()
        .flex()
        .flex_col()
        .rounded(px(18.))
        .border_1()
        .border_color(c(0x2c2e2f))
        .bg(c(0x101010))
        .overflow_hidden()
        .occlude()
        .on_click(|_, _, cx| cx.stop_propagation())
        .child(
            div()
                .flex()
                .items_start()
                .justify_between()
                .gap(px(12.))
                .pt(px(16.))
                .px(px(18.))
                .pb(px(12.))
                .border_b_1()
                .border_color(ca(57, 57, 57, 0.5))
                .child(
                    div()
                        .child(div().text_size(px(17.)).line_height(px(24.)).font_weight(FontWeight::SEMIBOLD).text_color(c(0xf6f6f6)).child("Sync Report"))
                        .child(
                            div()
                                .mt(px(4.))
                                .text_size(px(13.))
                                .line_height(px(24.))
                                .text_color(white_a(0.55))
                                .child("How this device reads the community. Versions and short IDs only, safe to share."),
                        ),
                )
                .child(
                    div()
                        .id("sr-close")
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap(px(2.))
                        .cursor_pointer()
                        .group("sr-close")
                        .on_click(move |_, w, cx| close(Act::ReportClose, w, cx))
                        .child(
                            div()
                                .size(px(30.))
                                .rounded_full()
                                .border(px(1.5))
                                .border_color(c(0x4a4a4a))
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_size(px(12.))
                                .text_color(c(0x9a9a9a))
                                .group_hover("sr-close", |d| d.text_color(c(0xffffff)).border_color(c(0x9a9a9a)))
                                .child("✕"),
                        ),
                ),
        )
        .child(body)
        .child(
            div()
                .flex()
                .justify_end()
                .gap(px(8.))
                .px(px(18.))
                .py(px(12.))
                .border_t_1()
                .border_color(ca(57, 57, 57, 0.5))
                .child(text_btn("sr-done", "Close", true, click(on, || Act::ReportClose)))
                .child(save_btn("sr-copy", "Copy Report", !r.text.is_empty(), click(on, || Act::ReportCopy))),
        );
    let dismiss = on.clone();
    div()
        .id("sr-overlay")
        .absolute()
        .inset_0()
        .bg(ca(0, 0, 0, 0.7))
        .flex()
        .items_center()
        .justify_center()
        .p(px(16.))
        .occlude()
        .on_click(move |_, w, cx| {
            cx.stop_propagation();
            dismiss(Act::ReportClose, w, cx)
        })
        .child(card)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relay_addresses() {
        let list = vec!["wss://a.example".to_string()];
        assert_eq!(
            relay_address("relay.example.com/", &list, None).as_deref(),
            Ok("wss://relay.example.com")
        );
        assert!(relay_address("ws://plain.example", &list, None).is_err());
        assert_eq!(
            relay_address("ws://abc.onion", &list, None).as_deref(),
            Ok("ws://abc.onion")
        );
        assert_eq!(
            relay_address("WSS://A.example/", &list, None),
            Err("That relay is already listed.".into())
        );
        assert_eq!(
            voice_address("relay.example.com", &[]).as_deref(),
            Ok("https://relay.example.com/")
        );
        assert!(voice_address("http://relay.example.com", &[]).is_err());
    }
}
