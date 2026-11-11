//! The widescreen Main Menu (`shell/StartMenu.svelte`): search, the app's own places down the
//! left, the pins on the right, and you with your audio switches along the bottom. It grows
//! out of your row in the rail.

use std::rc::Rc;

use gpui::prelude::*;
use gpui::AnimationExt;
use gpui::{AnyElement, App, Entity, FontWeight, SharedString, Window, div, img, px, svg};
use vector_core::rail_layout::{RailLayout, RailNode};

use crate::composer::TextInput;
use crate::data::Store;
use crate::theme::*;

pub struct StartMenu {
    pub layout: RailLayout,
    pub list: bool,
    pub search: Entity<TextInput>,
    /// The folder open in the pins' place.
    pub folder: Option<String>,
    pub mic: vector_core::calls::settings::MicState,
    pub accounts: usize,
    pub opened: std::time::Instant,
}

pub enum Act {
    Close,
    Nav(&'static str),
    Open(String),
    Folder(Option<String>),
    View(bool),
    AddPin,
    Mute,
    Deafen,
    Devices,
}

pub type OnAct = Rc<dyn Fn(Act, &mut Window, &mut App)>;

fn click(on: &OnAct, act: impl Fn() -> Act + 'static) -> impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static {
    let on = on.clone();
    move |_, w, cx| on(act(), w, cx)
}

enum Glyph {
    Picture(std::path::PathBuf, bool),
    Initial(String, bool),
    Icon(SharedString),
}

struct Resolved {
    label: String,
    sub: String,
    glyph: Glyph,
    missing: bool,
}

fn initial(name: &str) -> String {
    name.chars().find(|c| c.is_alphanumeric()).map(|c| c.to_uppercase().collect()).unwrap_or_else(|| "?".into())
}

/// What a pin shows, from what this device holds; a key it can't resolve keeps its place.
fn resolve(key: &str, store: &Store) -> Resolved {
    let icon = |name: &str| Glyph::Icon(SharedString::from(format!("icons/{name}.svg")));
    let r = |label: String, sub: &str, glyph: Glyph, missing: bool| Resolved { label, sub: sub.into(), glyph, missing };
    let parts: Vec<&str> = key.split('/').collect();
    match parts.as_slice() {
        ["dm", npub] => {
            let label = store.name(npub);
            let glyph = match store.avatar_path(npub) {
                Some(p) => Glyph::Picture(p, true),
                None => Glyph::Initial(initial(&label), true),
            };
            r(label, "Direct Message", glyph, false)
        }
        ["settings", rest @ ..] => {
            let cats = crate::settings::categories(&crate::settings::Offers { voice: false, routing: true, updates: true });
            let Some(cat) = rest.first().and_then(|id| cats.iter().find(|c| c.id == *id)) else {
                return r("Settings".into(), "Settings", icon("settings"), false);
            };
            match rest.get(1).and_then(|id| cat.anchors.iter().find(|a| a.id == *id)) {
                Some(a) => r(a.label.into(), &format!("Settings › {}", cat.label), icon(cat.icon), false),
                None => r(cat.label.into(), "Settings", icon(cat.icon), false),
            }
        }
        ["requests"] => r("Requests".into(), "Messages", icon("mail"), false),
        ["nexus"] => r("App Store".into(), "Mini Apps", icon("nexus"), false),
        ["app", id] => r(id.to_string(), "Mini App", icon("grid-fill"), true),
        ["c", ..] => r("Community".into(), "A community you're not in", icon("users-multi"), true),
        _ => r(key.into(), "", icon("share"), true),
    }
}

fn glyph(g: &Glyph, size: f32) -> AnyElement {
    match g {
        Glyph::Picture(p, round) => img(p.clone()).size(px(size)).flex_shrink_0().when(*round, |d| d.rounded_full()).when(!*round, |d| d.rounded(px(10.))).into_any_element(),
        Glyph::Initial(ch, round) => div()
            .size(px(size))
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(c(0x333333))
            .when(*round, |d| d.rounded_full())
            .when(!*round, |d| d.rounded(px(10.)))
            .text_size(px(if size > 40. { 27. } else { 12. }))
            .font_weight(FontWeight::BOLD)
            .text_color(c(0xdddddd))
            .child(ch.clone())
            .into_any_element(),
        Glyph::Icon(path) => svg().path(path.clone()).size(px(if size >= 40. { 34. } else { size * 0.7 })).text_color(c(0xdddddd)).into_any_element(),
    }
}

/// A grid tile is named beneath; a list row reads like a search result.
fn tile(id: SharedString, label: String, sub: Option<&str>, g: AnyElement, plate: bool, missing: bool, list: bool, on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static) -> AnyElement {
    if list {
        let mut text = div().flex().flex_col().min_w_0().child(div().text_size(px(13.)).line_height(px(18.)).text_color(c(0xffffff)).overflow_hidden().text_ellipsis().whitespace_nowrap().child(label));
        if let Some(sub) = sub {
            text = text.child(div().text_size(px(12.)).line_height(px(16.)).text_color(white_a(0.45)).child(sub.to_string()));
        }
        return div()
            .id(id)
            .flex()
            .items_center()
            .gap(px(12.))
            .px(px(10.))
            .py(px(5.))
            .rounded(px(10.))
            .hover(|d| d.bg(c(0x2a2a2a)))
            .cursor_pointer()
            .on_click(on_click)
            .child(div().size(px(28.)).flex_shrink_0().flex().items_center().justify_center().when(missing, |d| d.opacity(0.5)).child(g))
            .child(text)
            .into_any_element();
    }
    let group = SharedString::from(format!("{id}-g"));
    div()
        .id(id)
        .group(group.clone())
        .min_w_0()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(8.))
        .cursor_pointer()
        .on_click(on_click)
        .child(
            div()
                .size(px(80.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(12.))
                .when(plate, |d| d.border(px(1.5)).border_color(c(0x3a3a3a)).bg(c(0x222222)).group_hover(group.clone(), |d| d.border_color(c(PRIMARY))))
                .when(!plate, |d| d.p(px(7.5)))
                .when(missing, |d| d.opacity(0.5))
                .child(g),
        )
        .child(
            div()
                .max_w_full()
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .text_size(px(13.))
                .line_height(px(24.))
                .text_color(white_a(0.6))
                .group_hover(group, |d| d.text_color(c(0xffffff)))
                .child(label),
        )
        .into_any_element()
}

fn nav_item(id: &'static str, label: &'static str, icon: &'static str, dot: bool, on: &OnAct) -> impl IntoElement {
    div()
        .id(id)
        .relative()
        .w(px(200.))
        .h(px(42.))
        .flex_shrink_0()
        .flex()
        .items_center()
        .gap(px(14.))
        .px(px(12.))
        .rounded(px(10.))
        .hover(|d| d.bg(c(0x2a2a2a)))
        .cursor_pointer()
        .text_size(px(15.))
        .line_height(px(24.))
        .text_color(c(0xe6e6e6))
        .on_click(click(on, move || Act::Nav(id)))
        .child(svg().path(icon).size(px(24.)).flex_shrink_0().text_color(c(0x9a9a9a)))
        .child(label)
        .when(dot, |d| d.child(div().absolute().left(px(30.)).top(px(8.)).size(px(8.)).rounded_full().bg(c(PRIMARY))))
}

fn audio(id: &'static str, icon: &'static str, off: bool, toggle: Act, on: &OnAct) -> impl IntoElement {
    let toggle = Rc::new(std::cell::RefCell::new(Some(toggle)));
    let on2 = on.clone();
    div()
        .flex()
        .items_center()
        .rounded(px(10.))
        .hover(|d| d.bg(c(0x1c1c1c)))
        .child(
            div()
                .id(id)
                .p(px(7.))
                .rounded(px(10.))
                .cursor_pointer()
                .on_click(move |_, w, cx| {
                    // Each render builds a fresh act; one press takes it.
                    if let Some(act) = toggle.borrow_mut().take() {
                        on2(act, w, cx);
                    }
                })
                .child(svg().path(icon).size(px(26.)).text_color(c(if off { crate::timeline::DANGER } else { 0x888888 }))),
        )
        .child(
            div()
                .id(SharedString::from(format!("{id}-more")))
                .px(px(4.))
                .py(px(7.))
                .rounded(px(10.))
                .cursor_pointer()
                .on_click(click(on, || Act::Devices))
                .child(svg().path("icons/chevron-down.svg").size(px(14.)).text_color(c(0x888888))),
        )
}

/// The menu's bottom-left corner is the rail row's, so its footer stands where the row stood.
pub fn render(sm: &StartMenu, store: &Store, window: &Window, on: &OnAct) -> AnyElement {
    let viewport = window.viewport_size();
    let height = (viewport.height - px(20.)).min(px(562.));
    let list = sm.list;
    let me = store.my_npub.clone();
    let unread = store.chats.iter().any(|ch| store.listed(ch) && store.unread_count(ch) > 0);

    let search = div()
        .mx(px(28.))
        .mt(px(24.))
        .h(px(44.))
        .flex_shrink_0()
        .flex()
        .items_center()
        .gap(px(12.))
        .px(px(18.))
        .rounded(px(22.))
        .bg(c(0x0f0f0f))
        .child(svg().path("icons/search.svg").size(px(18.)).flex_shrink_0().text_color(c(0xffffff)))
        .child(div().flex_1().min_w_0().font_family(RUBIK).child(sm.search.clone()));

    let nav = div()
        .w(px(233.))
        .flex_shrink_0()
        .mt(px(6.))
        .mb(px(22.))
        .pt(px(6.))
        .pl(px(18.))
        .border_r_1()
        .border_color(c(0x333333))
        .flex()
        .flex_col()
        .gap(px(4.))
        .child(nav_item("accounts", "Accounts", if sm.accounts > 1 { "icons/users-multi-fill.svg" } else { "icons/add-user.svg" }, false, on))
        .child(nav_item("profile", "Profile", "icons/start-profile.svg", false, on))
        .child(nav_item("chat", "Chat", "icons/chats-fill.svg", unread, on))
        .child(nav_item("bookmarks", "Bookmarks", "icons/bookmark-fill.svg", false, on))
        .child(nav_item("miniapps", "Mini Apps", "icons/grid-fill.svg", false, on))
        .child(nav_item("settings", "Settings", "icons/start-settings.svg", false, on));

    let folder = sm.folder.as_ref().and_then(|id| {
        sm.layout.nodes.iter().find_map(|n| match n {
            RailNode::Folder { id: fid, name, keys, .. } if fid == id => Some((name.clone(), keys.clone())),
            _ => None,
        })
    });
    let view_btn = |id: &'static str, icon: &'static str, on_: bool, want: bool| {
        div()
            .id(id)
            .p(px(4.))
            .cursor_pointer()
            .on_click(click(on, move || Act::View(want)))
            .child(svg().path(icon).size(px(16.)).text_color(c(if on_ { 0xffffff } else { 0x777777 })))
    };
    let title = match &folder {
        Some((name, _)) => if name.is_empty() { "Folder".to_string() } else { name.clone() },
        None => "Pinned".into(),
    };
    let head = div()
        .h(px(32.))
        .flex_shrink_0()
        .flex()
        .items_center()
        .text_size(px(11.))
        .line_height(px(24.))
        .font_weight(FontWeight::BOLD)
        .text_color(c(0x8a8a8a))
        .when(folder.is_some(), |d| {
            d.child(
                div()
                    .id("stm-back")
                    .mr(px(6.))
                    .cursor_pointer()
                    .on_click(click(on, || Act::Folder(None)))
                    .child(svg().path("icons/chevron-left.svg").size(px(14.)).text_color(c(0x8a8a8a))),
            )
        })
        .child(crate::kit::spaced(title.to_uppercase(), 11. * 0.09))
        .child(div().ml_auto().flex().gap(px(2.)).child(view_btn("stm-grid", "icons/grid.svg", !list, false)).child(view_btn("stm-list", "icons/list.svg", list, true)));

    let mut tiles: Vec<AnyElement> = Vec::new();
    let keys: Vec<RailNode> = match &folder {
        Some((_, keys)) => keys.iter().map(|k| RailNode::item(k.clone())).collect(),
        None => sm.layout.nodes.clone(),
    };
    for (ix, node) in keys.iter().enumerate() {
        match node {
            RailNode::Item { key } => {
                let r = resolve(key, store);
                let plate = matches!(r.glyph, Glyph::Icon(_));
                let k = key.clone();
                tiles.push(tile(SharedString::from(format!("stm-pin-{ix}")), r.label, Some(r.sub.as_str()).filter(|s| !s.is_empty()), glyph(&r.glyph, if list { 28. } else if plate { 40. } else { 65. }), plate, r.missing, list, click(on, move || Act::Open(k.clone()))));
            }
            RailNode::Folder { id, name, keys, .. } => {
                let mini = keys.iter().take(4).map(|k| glyph(&resolve(k, store).glyph, if list { 12. } else { 22. }));
                let grid = div().w(px(if list { 28. } else { 50. })).flex().flex_wrap().gap(px(if list { 4. } else { 6. })).justify_center().children(mini);
                let fid = id.clone();
                tiles.push(tile(SharedString::from(format!("stm-folder-{ix}")), if name.is_empty() { "Folder".into() } else { name.clone() }, Some("Folder"), grid.into_any_element(), true, false, list, click(on, move || Act::Folder(Some(fid.clone())))));
            }
        }
    }
    if folder.is_none() {
        let add = svg().path("icons/plus-circle.svg").size(px(if list { 22. } else { 34. })).text_color(c(0xbbbbbb)).into_any_element();
        tiles.push(tile("stm-add".into(), "Add Pin".into(), None, add, true, false, list, click(on, || Act::AddPin)));
    }
    let grid = if list {
        div().flex().flex_col().gap(px(2.)).children(tiles).into_any_element()
    } else {
        let mut rows = div().flex().flex_col().gap(px(16.));
        let mut tiles = tiles.into_iter().peekable();
        while tiles.peek().is_some() {
            let mut row = div().flex().gap(px(14.));
            for _ in 0..4 {
                row = row.child(div().flex_1().min_w_0().flex().justify_center().children(tiles.next()));
            }
            rows = rows.child(row);
        }
        rows.into_any_element()
    };
    let pane = div()
        .id("stm-pane")
        .flex_1()
        .min_w_0()
        .mt(px(6.))
        .mb(px(22.))
        .pl(px(24.))
        .pr(px(14.))
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .child(head)
        .child(div().mt(px(10.)).child(grid));

    let name = if store.me().is_some() { store.name(&me) } else { "You".into() };
    let short = if me.len() > 16 { format!("{}…", &me[..16]) } else { me.clone() };
    let deaf = sm.mic.deafened;
    let muted = sm.mic.muted || deaf;
    let footer = div()
        .flex_shrink_0()
        .flex()
        .items_center()
        .gap(px(6.))
        .pt(px(16.))
        .px(px(26.))
        .pb(px(18.))
        .bg(c(0x0d0d0d))
        .child(
            div()
                .id("stm-me")
                .flex()
                .items_center()
                .gap(px(14.))
                .min_w_0()
                .cursor_pointer()
                .on_click(click(on, || Act::Close))
                .child(crate::ui::avatar(store, &me, 48.))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(3.))
                        .min_w_0()
                        .max_w(px(220.))
                        .child(div().text_size(px(18.)).line_height(px(24.)).font_weight(FontWeight::SEMIBOLD).text_color(c(0xffffff)).overflow_hidden().text_ellipsis().whitespace_nowrap().child(name))
                        .child(div().text_size(px(13.)).line_height(px(24.)).text_color(white_a(0.5)).overflow_hidden().text_ellipsis().whitespace_nowrap().child(short)),
                ),
        )
        .child(div().flex_1())
        .child(audio("stm-deafen", if deaf { "icons/headphones-off.svg" } else { "icons/headphones.svg" }, deaf, Act::Deafen, on))
        .child(audio("stm-mute", if muted { "icons/mic-off.svg" } else { "icons/mic-on.svg" }, muted, Act::Mute, on));

    let grow = gpui::Animation::new(std::time::Duration::from_millis(280)).with_easing(crate::kit::bezier(0.2, 0.9, 0.25, 1.));
    let menu = div()
        .id("stm")
        .absolute()
        .left(px(8.))
        .bottom(px(8.))
        .w(px(682.))
        .h(height)
        .flex()
        .flex_col()
        .rounded(px(24.))
        .border_1()
        .border_color(c(0x333333))
        .bg(c(0x1a1a1a))
        .overflow_hidden()
        .font_family(INTER)
        .text_color(c(0xffffff))
        .occlude()
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(search)
        .child(div().mt(px(16.)).flex_1().min_h_0().flex().child(nav).child(pane))
        .child(footer)
        .with_animation(SharedString::from(format!("stm-grow-{:?}", sm.opened)), grow, |d, t| d.opacity(t));
    let close = on.clone();
    div()
        .id("stm-dim")
        .absolute()
        .inset_0()
        .bg(ca(0, 0, 0, 0.6))
        .on_mouse_down(gpui::MouseButton::Left, move |_, w, cx| close(Act::Close, w, cx))
        .child(menu)
        .into_any_element()
}
