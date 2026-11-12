//! The slim layout below widescreen (`console.css`, `ChatListPane.svelte`): a header over the
//! community rail and the pane beside it, the tab bar, and screens that take the window.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{AnimationExt, Focusable};
use gpui::{
    AnyElement, Bounds, Context, Entity, FontWeight, ListAlignment, ListState, Pixels,
    SharedString, Window, div, img, list, px, svg,
};
use vector_core::chat::SerializableChat;

use super::{MenuAct, VectorApp, avatar, preview_line_at, slim_badge};
use crate::composer::TextInput;
use crate::data::{self, Community, Store};
use crate::kit;
use crate::theme::*;

/// Narrowest window that gets the multi-pane layout (`WS_MIN_W`).
pub(super) const WIDE_MIN: f32 = 900.;

const SURFACE: u32 = 0x171717;
const EDGE: u32 = 0x474747;
const RAIL_W: f32 = 68.;
const HEADER_H: f32 = 52.;
const NAV_H: f32 = 65.;
const GUTTER: f32 = 16.;
/// A list row and the gap under it.
pub(super) const ROW_PITCH: f32 = 72.;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum Filter {
    All,
    Messages,
    Communities,
    Requests,
}

impl Filter {
    fn label(self) -> &'static str {
        match self {
            Filter::All => "All",
            Filter::Messages => "Messages",
            Filter::Communities => "Communities",
            Filter::Requests => "Requests",
        }
    }
}

#[derive(Clone, PartialEq)]
enum Item {
    Archive,
    Chat(String),
    /// Unfolded under the archive row; the last closes the group with a rule.
    Archived(String, bool),
    Community(String),
}

pub(super) struct Slim {
    /// The community rail is out (Discord) rather than folded (Telegram).
    pub rail: bool,
    /// Display's Hide Community Rail: folded with no pull tab.
    pub hidden: bool,
    pub filter: Filter,
    /// A community's channels stand over the channel on screen.
    pub drawer: bool,
    pub search: Option<Entity<TextInput>>,
    pub(super) list: ListState,
    keys: RefCell<Vec<Item>>,
    tabs: Rc<RefCell<HashMap<Filter, Bounds<Pixels>>>>,
    /// Where the tab bar slides from: the tab last marked.
    bar_from: Option<(Filter, f32, f32)>,
    new_btn: Rc<std::cell::Cell<Option<Bounds<Pixels>>>>,
}

impl Slim {
    pub fn new() -> Self {
        Self {
            rail: crate::backend::device::get("cc_rail").is_none_or(|v| v != "false"),
            hidden: crate::backend::device::get("cc_rail_hidden").is_some_and(|v| v == "true"),
            filter: Filter::Messages,
            drawer: false,
            search: None,
            list: ListState::new(0, ListAlignment::Top, px(400.)),
            keys: RefCell::new(Vec::new()),
            tabs: Rc::default(),
            bar_from: None,
            new_btn: Rc::default(),
        }
    }

    /// The tabs on offer: communities live on the rail while it shows.
    fn rail_out(&self) -> bool {
        self.rail && !self.hidden
    }

    fn filters(&self) -> &'static [Filter] {
        if self.rail_out() {
            &[Filter::Messages, Filter::Requests]
        } else {
            &[Filter::All, Filter::Messages, Filter::Communities, Filter::Requests]
        }
    }

    /// The tab in force, which a fold or an unfold can leave pointing at one that's gone.
    fn active(&self) -> Filter {
        let f = self.filters();
        if f.contains(&self.filter) { self.filter } else { f[0] }
    }
}

fn community_unread(store: &Store, com: &Community) -> bool {
    com.channels
        .iter()
        .any(|ch| store.chat(&ch.channel_id).is_some_and(|chat| store.unread_count(chat) > 0))
}

/// A community's newest channel message: (its channel, when).
fn community_latest<'a>(store: &'a Store, com: &Community) -> Option<(&'a SerializableChat, u64)> {
    com.channels
        .iter()
        .filter_map(|ch| store.chat(&ch.channel_id))
        .filter_map(|chat| chat.messages.last().map(|m| (chat, m.at)))
        .max_by_key(|(_, at)| *at)
}

impl VectorApp {
    pub(super) fn slim_set_rail(&mut self, open: bool, cx: &mut Context<Self>) {
        self.slim.rail = open;
        crate::backend::device::set("cc_rail", open.to_string());
        cx.notify();
    }

    fn slim_query(&self, cx: &gpui::App) -> String {
        self.slim
            .search
            .as_ref()
            .map(|s| s.read(cx).text().trim().to_lowercase())
            .unwrap_or_default()
    }

    /// Out of the chat on screen: a community of many channels to its drawer, else the list.
    pub(super) fn slim_back(&mut self, cx: &mut Context<Self>) {
        let multi = self.community.as_ref().is_some_and(|v| {
            self.store.borrow().community(&v.id).is_some_and(|c| c.channels.len() > 1)
        });
        if multi && !self.slim.drawer {
            self.slim.drawer = true;
        } else {
            self.slim.drawer = false;
            if self.community.is_some() {
                self.leave_community();
            } else {
                self.open = None;
                if self.live {
                    crate::backend::messaging::set_active_chat(None);
                }
            }
        }
        cx.notify();
    }

    fn slim_home(&mut self, cx: &mut Context<Self>) {
        self.settings = None;
        self.nexus = None;
        self.profile = None;
        self.new_chat = None;
        self.create_community = None;
        self.slim.drawer = false;
        cx.notify();
    }

    fn slim_new_menu(&mut self, cx: &mut Context<Self>) {
        let b = self.slim.new_btn.get().unwrap_or_default();
        self.show_menu(
            gpui::point(b.right() - px(180.), b.bottom() + px(6.)),
            vec![
                (kit::MenuItem::new("New Message", "chat-circle").lead(), MenuAct::NewMessage),
                (kit::MenuItem::new("New Community", "users-multi").lead(), MenuAct::NewCommunity),
                (kit::MenuItem::new("Add Friend", "add-user").lead(), MenuAct::AddFriend),
                (kit::MenuItem::new("Bookmark", "bookmark").lead(), MenuAct::Bookmark),
                (kit::MenuItem::new("App Store", "nexus").lead(), MenuAct::AppStore),
            ],
            cx,
        );
    }

    /// Everything under the window chrome while the window is slim.
    pub(super) fn slim_main(
        &mut self,
        store: &Store,
        on_community: &crate::community::OnAct,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let width = f32::from(window.viewport_size().width);
        let screen = |body: AnyElement| div().flex_1().min_h_0().w_full().flex().flex_col().child(body);
        if let Some(settings) = self.settings.as_ref() {
            let app = cx.entity().downgrade();
            let on: crate::settings::OnAct = Rc::new(move |act, window, cx| {
                app.update(cx, |app, cx| app.on_settings(act, window, cx)).ok();
            });
            let view = div().flex_1().min_h_0().child(crate::settings::slim_view(settings, &on, cx));
            return screen(view.into_any_element()).child(self.slim_navbar(store, cx)).into_any_element();
        }
        if self.nexus.is_some() {
            return screen(div().flex_1().min_h_0().children(self.nexus_view(width, cx)).into_any_element())
                .into_any_element();
        }
        if let Some(m) = self.create_community.as_ref() {
            let app = cx.entity().downgrade();
            let on: crate::createcommunity::OnAct = Rc::new(move |act, window, cx| {
                app.update(cx, |app, cx| app.on_create_community(act, window, cx)).ok();
            });
            return screen(crate::createcommunity::view(m, store, width, &on, cx)).into_any_element();
        }
        if let Some(nc) = self.new_chat.as_ref() {
            let app = cx.entity().downgrade();
            let on: crate::newchat::OnAct = Rc::new(move |act, window, cx| {
                app.update(cx, |app, cx| app.on_new_chat(act, window, cx)).ok();
            });
            let text = nc.filter.read(cx).text();
            let invite = crate::newchat::invite_key(text)
                .map(|k| self.invite_previews.get(&k).cloned().unwrap_or(crate::newchat::Preview::Loading))
                .or_else(|| crate::newchat::go_link(text).map(crate::newchat::Preview::Go));
            return screen(crate::newchat::view(nc, store, width, invite.as_ref(), &on, cx).into_any_element())
                .into_any_element();
        }
        if let Some(p) = self.profile.as_ref() {
            let app = cx.entity().downgrade();
            let on: crate::profile::OnAct = Rc::new(move |act, window, cx| {
                app.update(cx, |app, cx| app.on_profile(act, window, cx)).ok();
            });
            let pane = crate::profile::pane(store, p, &on, window, cx);
            return screen(div().flex_1().min_h_0().flex().child(pane).into_any_element())
                .child(self.slim_navbar(store, cx))
                .into_any_element();
        }
        let drawer = self.slim.drawer && self.community.is_some();
        if let Some(id) = self.open.clone().filter(|_| !drawer) {
            let back = cx.listener(|app, _: &gpui::ClickEvent, _, cx| app.slim_back(cx));
            return div()
                .flex_1()
                .min_h_0()
                .w_full()
                .relative()
                .flex()
                .child(self.chat_pane(store, id, on_community, window, cx))
                .child(
                    // .nav-back-btn
                    div()
                        .id("chat-back-btn")
                        .absolute()
                        .left(px(5.))
                        .top(px(10.))
                        .size(px(40.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .on_click(back)
                        .child(svg().path("icons/chevron-double-left.svg").size(px(30.)).text_color(c(ACCENT))),
                )
                .into_any_element();
        }
        let header = self.slim_header(store, window, cx);
        let body = self.slim_body(store, width, on_community, window, cx);
        let col = div()
            .flex_1()
            .min_h_0()
            .w_full()
            .flex()
            .flex_col()
            .bg(c(0x050505))
            .child(header)
            .child(body);
        if drawer {
            col.into_any_element()
        } else {
            col.child(self.slim_navbar(store, cx)).into_any_element()
        }
    }

    /// `.cc-header`: your account, search, the scanner and the New menu.
    fn slim_header(&mut self, store: &Store, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let search = match &self.slim.search {
            Some(s) => s.clone(),
            None => {
                let s = cx.new(|cx| {
                    TextInput::new("Search...", cx).with_look(crate::composer::Look {
                        family: INTER,
                        weight: FontWeight::NORMAL,
                        size: px(15.),
                        line_height: px(24.),
                        color: c(0xffffff),
                        placeholder: white_a(0.55),
                        ..Default::default()
                    })
                });
                cx.subscribe_in(&s, window, |app, _, _: &crate::composer::InputChanged, _, cx| {
                    app.slim.list.scroll_to(gpui::ListOffset::default());
                    cx.notify();
                })
                .detach();
                self.slim.search = Some(s.clone());
                s
            }
        };
        let typed = !search.read(cx).text().is_empty();
        let me = store.my_npub.clone();
        let btn = |id: &'static str, icon: &'static str| {
            div()
                .id(id)
                .size(px(36.))
                .flex_shrink_0()
                .rounded(px(9.))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .text_color(white_a(0.85))
                .hover(|s| s.bg(white_a(0.06)).text_color(c(0xffffff)))
                .child(svg().path(icon).size(px(24.)).text_color(white_a(0.85)))
        };
        div()
            .h(px(HEADER_H))
            .flex_shrink_0()
            .pl(px(12.))
            .pr(px(10.))
            .flex()
            .items_center()
            .gap(px(8.))
            .bg(c(SURFACE))
            .child(
                div()
                    .id("cc-account")
                    .h(px(40.))
                    .pr(px(2.))
                    .flex()
                    .items_center()
                    .gap(px(5.))
                    .cursor_pointer()
                    .on_click(cx.listener(|app, _, _, cx| {
                        if app.switcher.is_some() {
                            app.switcher = None;
                            cx.notify();
                        } else {
                            app.open_switcher(cx);
                        }
                    }))
                    .child(avatar(store, &me, 32.))
                    .child(svg().path("ui/cc-caret.svg").w(px(9.)).h(px(6.)).text_color(white_a(0.75))),
            )
            .child(
                div()
                    .id("cc-search")
                    .flex_1()
                    .min_w_0()
                    .h(px(36.))
                    .ml(px(4.))
                    .pl(px(10.))
                    .pr(px(6.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .rounded(px(9.))
                    .border_1()
                    .border_color(white_a(0.12))
                    .cursor_text()
                    .on_click(cx.listener(|app, _, window, cx| {
                        if let Some(s) = &app.slim.search {
                            window.focus(&s.focus_handle(cx), cx);
                        }
                        if app.open.is_some() || app.community.is_some() {
                            app.leave_community();
                            app.open = None;
                            app.slim.drawer = false;
                        }
                    }))
                    .child(svg().path("ui/cc-search.svg").size(px(22.)).flex_shrink_0().text_color(white_a(0.55)))
                    .child(div().flex_1().min_w_0().child(search))
                    .when(typed, |d| {
                        d.child(
                            div()
                                .id("cc-search-clear")
                                .size(px(26.))
                                .flex_shrink_0()
                                .rounded_full()
                                .flex()
                                .items_center()
                                .justify_center()
                                .cursor_pointer()
                                .on_click(cx.listener(|app, _, window, cx| {
                                    if let Some(s) = &app.slim.search {
                                        s.update(cx, |t, cx| t.clear(window, cx));
                                    }
                                    window.blur(cx);
                                    cx.notify();
                                }))
                                .child(svg().path("ui/cc-clear.svg").size(px(16.)).text_color(white_a(0.6))),
                        )
                    }),
            )
            .child(btn("cc-scan", "ui/cc-scan.svg").on_click(cx.listener(|app, _, window, cx| {
                app.open_qr_scanner(window, cx);
            })))
            .child(
                btn("cc-new", "ui/cc-plus.svg")
                    .ml(px(-4.))
                    .on_click(cx.listener(|app, _, _, cx| app.slim_new_menu(cx)))
                    .child({
                        let probe = self.slim.new_btn.clone();
                        gpui::canvas(move |b, _, _| probe.set(Some(b)), |_, _, _, _| {}).absolute().size_full()
                    }),
            )
            .into_any_element()
    }

    /// `.cc-body`: the rail beside the pane, which is the filtered list or a community.
    fn slim_body(
        &mut self,
        store: &Store,
        width: f32,
        on_community: &crate::community::OnAct,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let query = self.slim_query(cx);
        let rail = self.slim.rail_out() && query.is_empty();
        let pane_w = width - if rail { RAIL_W } else { 0. };
        let community = self
            .community
            .as_ref()
            .filter(|_| query.is_empty())
            .and_then(|v| Some((v, store.community(&v.id)?)));
        let pane: AnyElement = match community {
            Some((view, com)) => div()
                .flex_1()
                .min_w_0()
                .h_full()
                .bg(white_a(0.025))
                .child(crate::community::sidebar(
                    store,
                    view,
                    com,
                    self.open.as_deref(),
                    self.raid.get(&com.community_id).is_some_and(|r| r.0 || r.1 > 0),
                    on_community,
                    (&self.voice, &self.on_voice_handler(cx), &self.anchors),
                    pane_w,
                ))
                .into_any_element(),
            None => self.slim_pane(store, &query, window, cx),
        };
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .when(rail, |d| d.child(self.slim_rail(store, on_community, cx)))
            .child(pane)
            .into_any_element()
    }

    /// `.cc-rail`: the Messages door, the communities, a tile to make one and the fold.
    fn slim_rail(&self, store: &Store, on_community: &crate::community::OnAct, cx: &mut Context<Self>) -> AnyElement {
        let active = self.community.as_ref().map(|v| v.id.as_str());
        let home = active.is_none();
        let any_unread = store.chats.iter().any(|c| store.listed(c) && store.unread_count(c) > 0);
        let voice: HashMap<String, bool> = store
            .communities
            .iter()
            .filter_map(|c| Some((c.community_id.clone(), self.voice.badge(store, &c.community_id)?)))
            .collect();
        let on = on_community.clone();
        let door = div()
            .id("cc-rail-messages")
            .relative()
            .size(px(45.))
            .mx_auto()
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .on_click(move |_, window, cx| on(crate::community::Act::Home, window, cx))
            .child(
                svg()
                    .path("ui/rail-mail.svg")
                    .w(px(33.))
                    .h(px(27.))
                    .text_color(if home { c(0xffffff) } else { white_a(0.6) }),
            )
            .when(home, |d| {
                d.child(
                    div()
                        .absolute()
                        .left(px((45. - (RAIL_W - 1.5)) / 2.))
                        .top(px(3.))
                        .bottom(px(3.))
                        .w(px(4.))
                        .rounded_r(px(4.))
                        .bg(c(ACCENT)),
                )
            })
            .when(any_unread, |d| {
                d.child(
                    slim_badge(c(PRIMARY))
                        .top(px(4.))
                        .left(px(34.))
                        .size(px(10.))
                        .border_2()
                        .border_color(c(SURFACE)),
                )
            });
        let tiles = crate::rail::render_slim(store, &store.rail, &self.rail_open, active, RAIL_W - 1.5, &voice, on_community);
        div()
            .id("cc-rail")
            .w(px(RAIL_W))
            .flex_shrink_0()
            .h_full()
            .flex()
            .flex_col()
            .pb(px(4.))
            .bg(c(SURFACE))
            .border_t(px(1.5))
            .border_r(px(1.5))
            .border_color(c(EDGE))
            .rounded_tr(px(14.))
            .overflow_y_scroll()
            .child(div().pt(px(10.)).child(door))
            .child(div().mt(px(16.)).mx(px(14.)).mb(px(4.)).h(px(1.5)).flex_shrink_0().bg(c(EDGE)))
            .child(tiles)
            .child(
                div()
                    .id("cc-rail-add")
                    .tooltip(kit::tooltip("Create Community"))
                    .mt(px(5.))
                    .size(px(45.))
                    .flex_shrink_0()
                    .mx_auto()
                    .rounded(px(8.5))
                    .bg(white_a(0.05))
                    .hover(|s| s.bg(white_a(0.1)))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .on_click(cx.listener(|app, _, window, cx| app.open_create_community(window, cx)))
                    .child(svg().path("ui/plus-thin.svg").size(px(22.)).text_color(c(PRIMARY))),
            )
            .child(
                div()
                    .id("cc-rail-fold")
                    .group("cc-rail-fold")
                    .tooltip(kit::tooltip("Hide communities"))
                    .mt(px(13.))
                    .w(px(45.))
                    .h(px(36.))
                    .flex_shrink_0()
                    .mx_auto()
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .on_click(cx.listener(|app, _, _, cx| app.slim_set_rail(false, cx)))
                    .child(
                        svg()
                            .path("icons/chevron-double-left.svg")
                            .size(px(20.))
                            .text_color(white_a(0.45))
                            .group_hover("cc-rail-fold", |s| s.text_color(c(0xffffff))),
                    ),
            )
            .into_any_element()
    }

    /// The list's pane: the filters (or, searching, every match) over the rows.
    fn slim_pane(&mut self, store: &Store, query: &str, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let searching = !query.is_empty();
        let filter = self.slim.active();
        let mut items: Vec<(Item, u64)> = Vec::new();
        let mut archived: Vec<(String, u64)> = Vec::new();
        let mut dots: HashMap<Filter, bool> = HashMap::new();
        let mut communities_seen = std::collections::HashSet::new();
        for chat in &store.chats {
            if !store.listed(chat) {
                continue;
            }
            let request = store.is_request(chat);
            let at = chat.messages.last().map(|m| m.at).unwrap_or(0);
            let unread = store.unread_count(chat) > 0;
            if unread {
                dots.insert(if request { Filter::Requests } else { Filter::Messages }, true);
            }
            let name_hit = || store.name(&chat.id).to_lowercase().contains(query);
            let keep = if searching {
                name_hit()
            } else {
                match filter {
                    Filter::Requests => request,
                    Filter::Messages | Filter::All => !request,
                    Filter::Communities => false,
                }
            };
            if keep && !searching && store.is_archived(chat) {
                archived.push((chat.id.clone(), at));
            } else if keep {
                items.push((Item::Chat(chat.id.clone()), at));
            }
        }
        for com in &store.communities {
            if !communities_seen.insert(com.community_id.clone()) {
                continue;
            }
            if community_unread(store, com) {
                dots.insert(Filter::Communities, true);
            }
            let keep = if searching {
                com.name.to_lowercase().contains(query)
            } else {
                matches!(filter, Filter::All | Filter::Communities)
            };
            if keep {
                let at = community_latest(store, com).map(|(_, at)| at).unwrap_or(0);
                items.push((Item::Community(com.community_id.clone()), at));
            }
        }
        items.sort_by(|a, b| {
            let pin = |i: &Item| match i {
                Item::Chat(id) => store.pin_rank(id).unwrap_or(usize::MAX),
                _ => usize::MAX,
            };
            pin(&a.0).cmp(&pin(&b.0)).then(b.1.cmp(&a.1))
        });
        archived.sort_by(|a, b| {
            let pin = |id: &str| store.pin_rank(id).unwrap_or(usize::MAX);
            pin(&a.0).cmp(&pin(&b.0)).then(b.1.cmp(&a.1))
        });
        let forced = items.is_empty();
        let shown = self.archive.shown(forced);
        let has_archive = !archived.is_empty();
        let mut rows: Vec<Item> = Vec::with_capacity(items.len() + archived.len() + 1);
        if has_archive && (shown || self.archive.height(false) > 0.5) {
            rows.push(Item::Archive);
            if shown && self.archive.expanded {
                let n = archived.len();
                rows.extend(archived.iter().enumerate().map(|(k, (id, _))| Item::Archived(id.clone(), k + 1 == n)));
            }
        }
        rows.extend(items.into_iter().map(|(i, _)| i));
        let items = rows;
        let archive_row = (items.first() == Some(&Item::Archive))
            .then(|| self.slim_archive_row(store, &archived, forced, cx));
        {
            let mut last = self.slim.keys.borrow_mut();
            if *last != items {
                let top = self.slim.list.logical_scroll_top();
                self.slim.list.reset(items.len());
                self.slim.list.scroll_to(top);
                *last = items.clone();
            } else if items.first() == Some(&Item::Archive) {
                self.slim.list.remeasure_items(0..1);
            }
        }
        let tabs = (!searching).then(|| self.slim_tabs(filter, &dots, window, cx));
        let empty = (items.is_empty() && !searching && filter == Filter::Requests).then(|| {
            div()
                .mx(px(24.))
                .my(px(28.))
                .text_size(px(14.))
                .line_height(px(20.))
                .text_center()
                .text_color(white_a(0.45))
                .child("New people who message you show up here.")
        });
        let items = Rc::new(items);
        let archive_row = Rc::new(std::cell::RefCell::new(archive_row));
        let shared = self.store.clone();
        let view = cx.entity().downgrade();
        div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .children(tabs)
            .children(empty)
            .child(
                div()
                    .id("cc-list-scroll")
                    .flex_1()
                    .min_h_0()
                    .px(px(8.))
                    .on_scroll(cx.listener(move |app, e: &gpui::ScrollEvent, _, cx| {
                        app.archive_wheel(e, forced, has_archive, cx);
                        app.archive_scrolled(forced);
                    }))
                    .child(
                    list(self.slim.list.clone(), move |i, _, _| {
                        let Some(item) = items.get(i) else {
                            return div().into_any_element();
                        };
                        if *item == Item::Archive {
                            let row = archive_row.borrow_mut().take().unwrap_or_else(|| div().into_any_element());
                            return div().pt(px(8.)).child(row).into_any_element();
                        }
                        let store = shared.borrow();
                        let (chat_id, rule) = match item {
                            Item::Chat(id) => (Some(id), false),
                            Item::Archived(id, last) => (Some(id), *last),
                            _ => (None, false),
                        };
                        let row = match item {
                            Item::Archive => None,
                            Item::Chat(_) | Item::Archived(..) => chat_id.and_then(|id| store.chat(id)).map(|chat| {
                                let id = &chat.id;
                                let id: SharedString = id.clone().into();
                                let (open_view, menu_view, menu_id) = (view.clone(), view.clone(), id.to_string());
                                slim_row(&store, chat)
                                    .on_click(move |_, window, cx| {
                                        let id = id.clone();
                                        open_view
                                            .update(cx, |app, cx| {
                                                app.slim.drawer = false;
                                                app.open_chat(id, window, cx);
                                                cx.notify();
                                            })
                                            .ok();
                                    })
                                    .on_mouse_down(gpui::MouseButton::Right, move |e, _, cx| {
                                        let (id, at) = (menu_id.clone(), e.position);
                                        menu_view.update(cx, |app, cx| app.chat_row_menu(id, at, cx)).ok();
                                    })
                                    .into_any_element()
                            }),
                            Item::Community(id) => store.community(id).map(|com| {
                                let (v, key) = (view.clone(), id.clone());
                                community_row(&store, com)
                                    .on_click(move |_, window, cx| {
                                        let key = key.clone();
                                        v.update(cx, |app, cx| {
                                            app.on_community(crate::community::Act::Open(key), window, cx)
                                        })
                                        .ok();
                                    })
                                    .into_any_element()
                            }),
                        };
                        let top = if i == 0 { 8. } else { 0. };
                        div()
                            .pt(px(top))
                            .pb(px(2.))
                            .children(row)
                            .when(rule, |d| d.pb(px(8.)).border_b_1().border_color(white_a(0.06)).mb(px(6.)))
                            .into_any_element()
                    })
                    .size_full(),
                ),
            )
            .into_any_element()
    }

    /// `ArchiveRow.svelte` at phone size: the folded chats' names and count.
    fn slim_archive_row(&self, store: &Store, archived: &[(String, u64)], forced: bool, cx: &mut Context<Self>) -> AnyElement {
        let f = &self.archive;
        let shown = f.shown(forced);
        let names = archived.iter().map(|(id, _)| store.name(id)).collect::<Vec<_>>().join(", ");
        let unread = archived.iter().any(|(id, _)| store.chat(id).is_some_and(|c| store.unread_count(c) > 0));
        let expanded = f.expanded;
        div()
            .h(px(f.height(shown)))
            .overflow_hidden()
            .flex()
            .flex_col()
            .justify_end()
            .child(
                div()
                    .id("cc-archive")
                    .h(px(ROW_PITCH))
                    .pb(px(2.))
                    .flex_shrink_0()
                    .opacity(f.opacity(shown))
                    .when(shown, |d| {
                        d.cursor_pointer().on_click(cx.listener(|app, _, _, cx| {
                            app.archive.expanded = !app.archive.expanded;
                            cx.notify();
                        }))
                    })
                    .child(
                        div()
                            .h(px(70.))
                            .px(px(9.))
                            .rounded(px(14.))
                            .flex()
                            .items_center()
                            .hover(|s| s.bg(white_a(0.04)))
                            .child(
                                div()
                                    .size(px(50.))
                                    .flex_shrink_0()
                                    .rounded_full()
                                    .bg(white_a(0.06))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .child(svg().path("icons/archive.svg").size(px(22.)).text_color(c(ACCENT))),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .gap(px(3.))
                                    .child(
                                        div()
                                            .pl(px(12.))
                                            .pr(px(10.))
                                            .flex()
                                            .items_center()
                                            .child(
                                                div()
                                                    .min_w_0()
                                                    .truncate()
                                                    .font_family(RUBIK)
                                                    .text_size(px(16.))
                                                    .line_height(px(20.))
                                                    .text_color(c(0xb2b2b2))
                                                    .child("Archived Chats"),
                                            )
                                            .child(
                                                div()
                                                    .ml_auto()
                                                    .flex_shrink_0()
                                                    .min_w(px(16.))
                                                    .px(px(5.5))
                                                    .py(px(2.5))
                                                    .rounded_full()
                                                    .bg(if unread { c(PRIMARY).into() } else { white_a(0.14) })
                                                    .text_center()
                                                    .font_family(INTER)
                                                    .text_size(px(11.))
                                                    .line_height(px(11.))
                                                    .font_weight(FontWeight::BOLD)
                                                    .text_color(if unread { c(0x000000).into() } else { white_a(0.8) })
                                                    .child(archived.len().to_string()),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .pl(px(12.))
                                            .pr(px(10.))
                                            .min_w_0()
                                            .truncate()
                                            .font_family(RUBIK)
                                            .text_size(px(14.))
                                            .line_height(px(19.))
                                            .text_color(c(0x737373))
                                            .child(names),
                                    ),
                            )
                            .child(
                                div()
                                    .w(px(26.))
                                    .ml(px(2.))
                                    .flex_shrink_0()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .opacity(0.45)
                                    .hover(|s| s.opacity(1.))
                                    .child(
                                        svg()
                                            .path("icons/chevron-down.svg")
                                            .size(px(16.))
                                            .text_color(white_a(0.75))
                                            .with_transformation(gpui::Transformation::rotate(gpui::radians(
                                                if expanded { std::f32::consts::PI } else { 0. },
                                            ))),
                                    ),
                            ),
                    ),
            )
            .into_any_element()
    }

    /// `.cc-tabs-row`: the pull for a folded rail, then the filters with their sliding bar.
    fn slim_tabs(
        &mut self,
        active: Filter,
        dots: &HashMap<Filter, bool>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let filters = self.slim.filters();
        let many = filters.len() > 2;
        let probes = self.slim.tabs.clone();
        let tabs = filters.iter().map(|&f| {
            let on = f == active;
            let dot = dots.get(&f).copied().unwrap_or(false) && !on;
            let probe = probes.clone();
            let tab = div()
                .id(SharedString::from(format!("cc-tab-{}", f.label())))
                .relative()
                .h_full()
                .flex()
                .items_center()
                .justify_center()
                .font_family(RUBIK)
                .text_size(px(if many { 14. } else { 15. }))
                .font_weight(if on { FontWeight::BOLD } else { FontWeight::MEDIUM })
                .text_color(if on { c(0xffffff) } else { white_a(0.38) })
                .cursor_pointer()
                .when(!on, |d| d.hover(|s| s.text_color(white_a(0.75))))
                .on_click(cx.listener(move |app, _, _, cx| {
                    if app.slim.active() != f {
                        let from = app.slim.active();
                        if let Some(b) = app.slim.tabs.borrow().get(&from) {
                            app.slim.bar_from = Some((f, f32::from(b.left()), f32::from(b.size.width)));
                        }
                        app.slim.filter = f;
                    }
                    app.slim.list.scroll_to(gpui::ListOffset::default());
                    cx.notify();
                }))
                .child(
                    div().relative().child(f.label()).when(dot, |d| {
                        d.child(div().absolute().top(px(-1.)).right(px(-9.)).size(px(8.)).rounded_full().bg(c(ACCENT)))
                    }),
                )
                .child(gpui::canvas(move |b, _, _| { probe.borrow_mut().insert(f, b); }, |_, _, _, _| {}).absolute().size_full());
            if many { tab.flex_grow(1.).px(px(6.)) } else { tab.flex_1() }
        });
        let row_probe = Rc::new(std::cell::Cell::new(None::<Bounds<Pixels>>));
        let bar = {
            let tabs = self.slim.tabs.borrow();
            let row = tabs.get(&active).copied();
            row.map(|b| {
                let (left, w) = (f32::from(b.left()), f32::from(b.size.width));
                let bar = div().absolute().bottom(px(-1.)).h(px(2.)).bg(c(PRIMARY));
                match self.slim.bar_from.filter(|(to, _, _)| *to == active) {
                    Some((_, l0, w0)) => bar
                        .with_animation(
                            SharedString::from(format!("cc-tab-bar-{}", active.label())),
                            gpui::Animation::new(std::time::Duration::from_millis(240))
                                .with_easing(kit::bezier(0.2, 0.8, 0.2, 1.)),
                            move |d, t| d.left(px(l0 + (left - l0) * t)).w(px(w0 + (w - w0) * t)),
                        )
                        .into_any_element(),
                    None => bar.left(px(left)).w(px(w)).into_any_element(),
                }
            })
        };
        if bar.is_none() {
            window.request_animation_frame();
        }
        // The bar is placed relative to the row; tabs are measured in window space.
        let origin = self.slim.tabs.borrow().get(&filters[0]).map(|b| f32::from(b.left())).unwrap_or(0.);
        let bar = bar.map(|b| div().absolute().inset_0().ml(px(-origin)).child(b));
        let _ = row_probe;
        let folded = !self.slim.rail_out() && !self.slim.hidden;
        div()
            .h(px(49.))
            .flex_shrink_0()
            .flex()
            .items_center()
            .child(if folded {
                div()
                    .id("cc-rail-pull")
                    .tooltip(kit::tooltip("Show communities"))
                    .group("cc-rail-pull")
                    .w(px(26.))
                    .h(px(44.))
                    .mt(px(2.))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .cursor_pointer()
                    .on_click(cx.listener(|app, _, _, cx| app.slim_set_rail(true, cx)))
                    .child(
                        div()
                            .w(px(16.))
                            .h(px(32.))
                            .bg(c(SURFACE))
                            .border_t(px(1.5))
                            .border_b(px(1.5))
                            .border_r(px(1.5))
                            .border_color(c(EDGE))
                            .rounded_r(px(9.))
                            .flex()
                            .items_center()
                            .child(
                                svg()
                                    .path("icons/chevron-right.svg")
                                    .size(px(12.))
                                    .text_color(white_a(0.55))
                                    .group_hover("cc-rail-pull", |s| s.text_color(c(0xffffff))),
                            ),
                    )
                    .into_any_element()
            } else {
                div().w(px(GUTTER)).flex_shrink_0().into_any_element()
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h(px(47.))
                    .mt(px(2.))
                    .mr(px(GUTTER))
                    .relative()
                    .flex()
                    .border_b_1()
                    .border_color(white_a(0.2))
                    .children(tabs)
                    .children(bar),
            )
            .into_any_element()
    }

    /// `.navbar`: Profile, Chat and Settings.
    fn slim_navbar(&self, store: &Store, cx: &mut Context<Self>) -> AnyElement {
        let me = store.my_npub.clone();
        let on_profile = self.profile.as_ref().is_some_and(|p| p.npub == me);
        let on_settings = self.settings.is_some();
        let on_chat = !on_profile && !on_settings;
        let tab = |id: &'static str, icon: &'static str, label: &'static str, on: bool| {
            div()
                .id(id)
                .flex()
                .flex_col()
                .items_center()
                .cursor_pointer()
                .when(!on, |d| d.opacity(0.5))
                .child(svg().path(icon).size(px(30.)).text_color(c(0xffffff)))
                .child(div().mt(px(-2.)).text_size(px(12.)).line_height(px(24.)).text_color(c(TEXT)).child(label))
        };
        div()
            .h(px(NAV_H))
            .flex_shrink_0()
            .w_full()
            .bg(c(0x131313))
            .flex()
            .items_center()
            .justify_evenly()
            .child(tab("nav-profile", "icons/user-circle.svg", "Profile", on_profile).on_click(cx.listener(
                move |app, _, _, cx| {
                    app.slim_home(cx);
                    let me = app.store.borrow().my_npub.clone();
                    app.open_profile(me, cx);
                },
            )))
            .child(tab("nav-chat", "icons/chats.svg", "Chat", on_chat).on_click(cx.listener(|app, _, _, cx| {
                app.slim_home(cx);
            })))
            .child(tab("nav-settings", "icons/settings.svg", "Settings", on_settings).on_click(cx.listener(
                |app, _, window, cx| {
                    app.slim_home(cx);
                    app.open_settings("appearance", window, cx);
                },
            )))
            .into_any_element()
    }
}

/// `body.cc .chatlist-contact`: a chat at thumb size.
fn slim_row(store: &Store, chat: &SerializableChat) -> gpui::Stateful<gpui::Div> {
    let unread = store.unread_count(chat) > 0;
    let last = chat.messages.last().map(|m| m.at).unwrap_or(0);
    let pinned = store.pin_rank(&chat.id).is_some();
    row_shell(
        SharedString::from(format!("cc-row-{}", chat.id)),
        avatar(store, &chat.id, 52.),
        store.name(&chat.id),
        unread,
        pinned,
        preview_line_at(store, chat, if unread { c(0xb2b2b2) } else { c(0x737373) }, 14., 19.),
        data::time_ago(store.now_ms(), last),
    )
}

/// A community as one row of the folded list: its newest channel's line.
fn community_row(store: &Store, com: &Community) -> gpui::Stateful<gpui::Div> {
    let unread = community_unread(store, com);
    let latest = community_latest(store, com);
    let color = if unread { c(0xb2b2b2) } else { c(0x737373) };
    let face = match store.icon(&com.community_id) {
        Some(path) => img(path).size(px(52.)).rounded(px(52. * 0.188_889)).flex_shrink_0().into_any_element(),
        None => img("icons/group-placeholder.svg").size(px(52.)).rounded(px(52. * 0.188_889)).flex_shrink_0().into_any_element(),
    };
    let preview = match latest {
        Some((chat, _)) => {
            let channel = com.channel(&chat.id).map(|ch| ch.name.clone()).unwrap_or_default();
            div()
                .flex()
                .min_w_0()
                .overflow_hidden()
                .child(
                    div()
                        .flex_shrink_0()
                        .font_family(RUBIK)
                        .text_size(px(14.))
                        .line_height(px(19.))
                        .text_color(color)
                        .child(format!("#{channel}: ")),
                )
                .child(div().min_w_0().overflow_hidden().child(preview_line_at(store, chat, color, 14., 19.)))
                .into_any_element()
        }
        None => div().into_any_element(),
    };
    row_shell(
        SharedString::from(format!("cc-com-{}", com.community_id)),
        face,
        com.name.clone(),
        unread,
        false,
        preview,
        latest.map(|(_, at)| data::time_ago(store.now_ms(), at)).unwrap_or_default(),
    )
}

fn row_shell(
    id: SharedString,
    face: AnyElement,
    name: String,
    unread: bool,
    pinned: bool,
    preview: AnyElement,
    time: String,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .h(px(70.))
        .px(px(9.))
        .rounded(px(14.))
        .flex()
        .items_center()
        .cursor_pointer()
        .hover(|s| s.bg(white_a(0.04)))
        .child(face)
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .h(px(20.))
                        .mb(px(3.))
                        .pl(px(12.))
                        .pr(px(10.))
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .font_family(RUBIK)
                                .text_size(px(16.))
                                .line_height(px(20.))
                                .font_weight(if unread { FontWeight::BOLD } else { FontWeight::NORMAL })
                                .text_color(if unread { c(0xffffff) } else { c(0xb2b2b2) })
                                .child(name),
                        )
                        .when(pinned, |d| {
                            d.child(svg().path("icons/pin.svg").size(px(14.)).flex_shrink_0().text_color(c(ACCENT)))
                        }),
                )
                .child(
                    div()
                        .h(px(24.))
                        .pr(px(10.))
                        .flex()
                        .items_center()
                        .child(div().flex_1().min_w_0().ml(px(12.)).mr(px(8.)).overflow_hidden().child(preview))
                        .child(div().flex_shrink_0().text_size(px(12.)).text_color(c(0x808080)).child(time)),
                ),
        )
}
