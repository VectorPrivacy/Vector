//! The widescreen shell: window chrome, rail, chat list and the open chat. Every size and
//! offset is the Tauri layout's own (measured from `reference/` renders); the comment on a
//! block names the CSS selector it reproduces.

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{
    AnyElement, Context, Entity, Focusable, FontWeight, IntoElement, ListAlignment, ListState, MouseButton,
    ParentElement, Render, SharedString, Styled, Window, WindowControlArea, div, img, linear_color_stop,
    linear_gradient, list, prelude::*, px, svg, uniform_list,
};
use vector_core::chat::SerializableChat;
use vector_core::types::Message;

use crate::composer::{SendText, TextInput};
use crate::data::{self, Row, Store};
use crate::theme::*;

const CHROME_H: f32 = 32.;
const RAIL_W: f32 = 201.;
const LIST_W: f32 = 239.;
const ROW_PITCH: f32 = 54.;

type Shared = Rc<RefCell<Store>>;

pub struct VectorApp {
    store: Shared,
    open: Option<SharedString>,
    timeline: ListState,
    /// The open chat's rows, rebuilt when it changes rather than per frame.
    rows: Rc<Vec<Row>>,
    read_anchor: Option<String>,
    input: Entity<TextInput>,
}

impl VectorApp {
    pub fn new(store: Store, open: Option<String>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| TextInput::new("Enter message...", cx));
        cx.subscribe(&input, |app, _, SendText(text), cx| app.send(text.clone(), cx)).detach();
        let mut app = Self {
            store: Rc::new(RefCell::new(store)),
            open: None,
            timeline: ListState::new(0, ListAlignment::Top, px(600.)),
            rows: Rc::default(),
            read_anchor: None,
            input,
        };
        if let Some(id) = open {
            app.open_chat(id.into(), window, cx);
        }
        app
    }

    fn rebuild_rows(&mut self) -> usize {
        let store = self.store.borrow();
        let rows = self
            .open
            .as_ref()
            .and_then(|id| store.chat(id))
            .map(|c| data::timeline(c, store.now_ms(), self.read_anchor.as_deref()))
            .unwrap_or_default();
        self.rows = Rc::new(rows);
        self.rows.len()
    }

    fn open_chat(&mut self, id: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        self.read_anchor = self.store.borrow().chat(&id).map(|c| c.last_read.clone());
        self.open = Some(id);
        let count = self.rebuild_rows();
        self.timeline = ListState::new(count, ListAlignment::Top, px(600.));
        window.focus(&self.input.read(cx).focus_handle(cx), cx);
    }

    /// A local echo only: the send pipeline (vector-core `send_dm`) lands with the live account.
    fn send(&mut self, text: String, cx: &mut Context<Self>) {
        let Some(id) = self.open.clone() else { return };
        {
            let mut store = self.store.borrow_mut();
            let now = store.now_ms();
            let me = store.my_npub.clone();
            let Some(chat) = store.chat_mut(&id) else { return };
            let msg = Message { id: format!("local-{now}-{}", chat.messages.len()), content: text, at: now, mine: true, npub: Some(me), ..Default::default() };
            chat.last_read = msg.id.clone();
            chat.messages.push(msg);
            store.resort();
        }
        let old = self.timeline.item_count();
        let new = self.rebuild_rows();
        self.timeline.splice(old..old, new - old);
        self.timeline.scroll_to_end();
        cx.notify();
    }
}

impl Render for VectorApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let store = self.store.clone();
        let store = store.borrow();
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(c(APP_BG))
            .font_family(INTER)
            .text_color(c(TEXT))
            .text_size(px(16.))
            .line_height(px(24.))
            .child(chrome(window))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_h_0()
                    .child(rail(&store, self.open.as_ref()))
                    .child(chat_list(&self.store, &store, self.open.clone(), cx))
                    .child(match self.open.clone() {
                        Some(id) => self.chat_pane(&store, id, cx).into_any_element(),
                        None => empty_pane().into_any_element(),
                    }),
            )
    }
}

// ── #window-chrome ──────────────────────────────────────────────────────────────────────

fn chrome(window: &Window) -> impl IntoElement {
    let maximized = window.is_maximized();
    let ctl = |id: &'static str, icon: &'static str, area: WindowControlArea, hover: u32| {
        div()
            .id(id)
            .w(px(37.))
            .h_full()
            .flex()
            .items_center()
            .justify_center()
            .text_color(white_a(0.7))
            .hover(move |s| s.bg(c(hover)).text_color(c(0xffffff)))
            .window_control_area(area)
            .child(svg().path(icon).size(px(10.)).text_color(white_a(0.7)))
    };
    div()
        .h(px(CHROME_H))
        .flex_shrink_0()
        .flex()
        .items_center()
        .bg(c(CHROME_BG))
        .border_b_1()
        .border_color(c(DIVIDER))
        .child(
            div()
                .id("chrome-drag")
                .flex_1()
                .h_full()
                .window_control_area(WindowControlArea::Drag)
                .on_mouse_down(MouseButton::Left, |e, window, _| {
                    if e.click_count == 2 {
                        window.titlebar_double_click();
                    } else {
                        window.start_window_move();
                    }
                })
                .child(
                    div()
                        .relative()
                        .child(img("ui/logo-mark.svg").absolute().left(px(12.)).top(px(5.5)).w(px(12.)).h(px(20.)))
                        .child(img("ui/logo-typeface.svg").absolute().left(px(32.)).top(px(9.5)).w(px(92.)).h(px(12.))),
                ),
        )
        .child(
            div()
                .flex()
                .h_full()
                .child(
                    div()
                        .w(px(37.))
                        .mr(px(8.))
                        .h_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(svg().path("ui/chrome-help.svg").size(px(16.)).text_color(c(CHROME_HELP))),
                )
                .child(ctl("chrome-min", "ui/chrome-min.svg", WindowControlArea::Min, HOVER))
                .child(ctl(
                    "chrome-max",
                    if maximized { "ui/chrome-restore.svg" } else { "ui/chrome-max.svg" },
                    WindowControlArea::Max,
                    HOVER,
                ))
                .child(ctl("chrome-close", "ui/chrome-close.svg", WindowControlArea::Close, 0xe81123)),
        )
}

// ── #navbar (widescreen rail) ────────────────────────────────────────────────────────────

fn avatar(store: &Store, npub: &str, size: f32) -> AnyElement {
    match store.avatar_path(npub) {
        Some(path) => img(path).size(px(size)).rounded_full().flex_shrink_0().into_any_element(),
        None => img("icons/user-placeholder.svg").size(px(size)).rounded_full().flex_shrink_0().into_any_element(),
    }
}

fn rail_label(text: &'static str) -> impl IntoElement {
    // .ws-rail-group-label
    div()
        .h(px(24.))
        .px(px(16.))
        .font_family(RUBIK)
        .text_size(px(15.))
        .text_color(white_a(0.38))
        .child(text)
}

fn rail(store: &Store, open: Option<&SharedString>) -> impl IntoElement {
    let unread: Vec<&SerializableChat> = store
        .chats
        .iter()
        .filter(|c| data::unread_count(c) > 0 && open.is_none_or(|o| *o != c.id))
        .collect();
    let any_unread = store.chats.iter().any(|c| data::unread_count(c) > 0);

    // .ws-rail-messages.is-home
    let messages = div().pt(px(16.)).px(px(8.)).child(
        div()
            .h(px(40.))
            .rounded(px(6.))
            .bg(c(RAIL_TILE))
            .flex()
            .items_center()
            .justify_center()
            .gap(px(11.))
            .child(svg().path("ui/rail-mail.svg").w(px(25.)).h(px(20.5)).text_color(c(0xffffff)))
            .child(
                div()
                    .relative()
                    .font_family(RUBIK)
                    .font_weight(FontWeight::MEDIUM)
                    .text_size(px(14.))
                    .text_color(c(0xffffff))
                    .child("Messages")
                    .when(any_unread, |d| {
                        d.child(div().absolute().left(px(67.4)).top(px(-1.)).size(px(8.)).rounded_full().bg(c(PRIMARY)))
                    }),
            ),
    );

    // #ws-rail-dms
    let dms = (!unread.is_empty()).then(|| {
        div().mt(px(10.)).child(rail_label("New Messages")).children(unread.iter().map(|chat| {
            div()
                .id(SharedString::from(format!("rail-{}", chat.id)))
                .mx(px(8.))
                .h(px(31.))
                .rounded(px(10.))
                .flex()
                .items_center()
                .px(px(8.))
                .gap(px(10.))
                .child(avatar(store, &chat.id, 26.))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .font_family(RUBIK)
                        .text_size(px(14.))
                        .text_color(c(0xffffff))
                        .child(store.name(&chat.id)),
                )
                .child(div().size(px(7.)).mr(px(5.)).rounded_full().bg(white_a(0.32)))
        }))
    });

    // #ws-rail-spaces
    let spaces = div()
        .mt(px(10.))
        .pt(px(6.))
        .child(div().mx(px(16.)).h(px(1.)).bg(c(DIVIDER)))
        .child(div().pt(px(16.)).child(rail_label("Communities")))
        .child(
            div()
                .mx(px(8.))
                .h(px(31.))
                .rounded(px(10.))
                .flex()
                .items_center()
                .px(px(8.))
                .gap(px(10.))
                .child(
                    div()
                        .size(px(26.))
                        .rounded(px(4.9))
                        .border_1()
                        .border_dashed()
                        .border_color(white_a(0.22))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(svg().path("ui/plus-thin.svg").size(px(14.)).text_color(c(PRIMARY))),
                )
                .child(
                    div()
                        .font_family(RUBIK)
                        .text_size(px(14.))
                        .text_color(white_a(0.55))
                        .child("Create Community"),
                ),
        );

    let me = store.me();
    let my_name = me.map(|p| store.name(&p.id)).unwrap_or_default();
    let status = me.map(|p| p.status.title.clone()).filter(|t| !t.is_empty());

    div()
        .w(px(RAIL_W))
        .flex_shrink_0()
        .h_full()
        .flex()
        .flex_col()
        .bg(c(RAIL_BG))
        .border_r_1()
        .border_color(c(DIVIDER))
        .child(
            // #ws-rail-shortcuts
            div()
                .flex_1()
                .min_h_0()
                .border_b_1()
                .border_color(c(DIVIDER))
                .child(messages)
                .children(dms)
                .child(spaces),
        )
        .child(
            // #ws-rail-collapse
            div()
                .mt(px(10.))
                .mb(px(2.))
                .mx(px(8.))
                .h(px(40.))
                .rounded(px(6.))
                .opacity(0.4)
                .flex()
                .items_center()
                .child(
                    div().ml(px(14.)).size(px(28.)).flex().items_center().justify_center().child(
                        svg().path("icons/chevron-double-left.svg").size(px(28.)).text_color(c(0xffffff)),
                    ),
                )
                .child(
                    div()
                        .ml(px(11.))
                        .font_weight(FontWeight::MEDIUM)
                        .text_size(px(14.))
                        .text_color(c(TEXT))
                        .child("Collapse"),
                ),
        )
        .child(
            // #ws-rail-account .account
            div().p(px(8.)).pt(px(8.)).child(
                div()
                    .h(px(54.))
                    .rounded(px(12.))
                    .bg(c(RAIL_TILE))
                    .flex()
                    .items_center()
                    .px(px(9.))
                    .gap(px(10.))
                    .child(avatar(store, &store.my_npub, 38.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .truncate()
                                    .font_weight(FontWeight::BOLD)
                                    .text_size(px(14.))
                                    .line_height(px(19.))
                                    .text_color(c(0xffffff))
                                    .child(my_name),
                            )
                            .child(
                                div()
                                    .mt(px(1.5))
                                    .truncate()
                                    .text_size(px(12.))
                                    .line_height(px(16.))
                                    .text_color(white_a(0.4))
                                    .child(status.unwrap_or_else(|| "Set a Status".into())),
                            ),
                    ),
            ),
        )
}

// ── #chats ──────────────────────────────────────────────────────────────────────────────

fn chat_list(shared: &Shared, store: &Store, open: Option<SharedString>, cx: &mut Context<VectorApp>) -> impl IntoElement {
    let tab = |label: &'static str, active: bool| {
        div()
            .flex_1()
            .h(px(40.))
            .flex()
            .items_center()
            .justify_center()
            .font_family(RUBIK)
            .text_size(px(14.))
            .font_weight(if active { FontWeight::BOLD } else { FontWeight::MEDIUM })
            .text_color(if active { c(0xffffff) } else { white_a(0.38) })
            .when(active, |d| d.border_b_2().border_color(c(PRIMARY)))
            .child(label)
    };
    let count = store.chats.len();
    let store = shared.clone();
    let view = cx.entity().downgrade();
    div()
        .w(px(LIST_W))
        .flex_shrink_0()
        .h_full()
        .flex()
        .flex_col()
        .bg(c(LIST_BG))
        .border_r_1()
        .border_color(c(DIVIDER))
        .child(
            // .chatlist-head
            div()
                .mt(px(22.))
                .ml(px(15.))
                .mr(px(12.))
                .h(px(36.))
                .flex()
                .items_center()
                .justify_between()
                .child(div().font_weight(FontWeight::SEMIBOLD).text_color(c(0xffffff)).child("Chats"))
                .child(
                    div()
                        .size(px(32.))
                        .rounded(px(8.))
                        .border_1()
                        .border_color(white_a(0.12))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(svg().path("ui/plus-thin.svg").size(px(16.)).text_color(c(ICON_MUTED))),
                ),
        )
        .child(
            // .cc-tabs
            div()
                .mt(px(10.))
                .mx(px(11.))
                .flex()
                .border_b_1()
                .border_color(white_a(0.2))
                .child(tab("Messages", true))
                .child(tab("Requests", false)),
        )
        .child(
            div().flex_1().min_h_0().pt(px(6.)).child(
                uniform_list("chatlist", count, move |range, _, _| {
                    let store = store.borrow();
                    range
                        .map(|i| {
                            let chat = &store.chats[i];
                            let active = open.as_ref().is_some_and(|o| *o == chat.id);
                            let id: SharedString = chat.id.clone().into();
                            let view = view.clone();
                            chat_row(&store, chat, active).on_click(move |_, window, cx| {
                                let id = id.clone();
                                view.update(cx, |app, cx| {
                                    app.open_chat(id, window, cx);
                                    cx.notify();
                                })
                                .ok();
                            })
                        })
                        .collect()
                })
                .size_full(),
            ),
        )
}

/// `.chatlist-contact` (ChatlistRow.svelte).
fn chat_row(store: &Store, chat: &SerializableChat, active: bool) -> gpui::Stateful<gpui::Div> {
    let unread = data::unread_count(chat) > 0 && !active;
    let (name_color, name_weight) = match (unread, active) {
        (true, _) => (c(0xffffff), FontWeight::BOLD),
        (false, true) => (c(0xffffff), FontWeight::NORMAL),
        _ => (white_a(0.75), FontWeight::NORMAL),
    };
    let preview_color = if active {
        white_a(0.85)
    } else if unread {
        c(PREVIEW_UNREAD)
    } else {
        ca(160, 160, 160, 0.5)
    };
    let last = chat.messages.last().map(|m| m.at).unwrap_or(0);
    div()
        .id(SharedString::from(format!("row-{}", chat.id)))
        .h(px(ROW_PITCH))
        .w(px(230.))
        .pb(px(4.))
        .child(
            div()
                .relative()
                .h(px(50.))
                .flex()
                .items_center()
                .pl(px(12.))
                .when(active, |d| {
                    d.child(div().absolute().left(px(-1.)).top(px(5.)).w(px(3.)).h(px(40.)).rounded_r(px(3.)).bg(c(PRIMARY)))
                })
                .child(avatar(store, &chat.id, 40.))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .h(px(40.))
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .h(px(20.))
                                .pl(px(10.))
                                .pr(px(15.))
                                .truncate()
                                .font_family(RUBIK)
                                .text_size(px(15.))
                                .line_height(px(20.))
                                .font_weight(name_weight)
                                .text_color(name_color)
                                .child(store.name(&chat.id)),
                        )
                        .child(
                            div()
                                .mt(px(-4.))
                                .h(px(24.))
                                .pr(px(15.))
                                .flex()
                                .items_center()
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .ml(px(10.))
                                        .mr(px(8.))
                                        .truncate()
                                        .font_family(RUBIK)
                                        .text_size(px(13.))
                                        .line_height(px(18.))
                                        .text_color(preview_color)
                                        .child(data::preview_text(chat)),
                                )
                                .child(
                                    div()
                                        .flex_shrink_0()
                                        .text_size(px(11.))
                                        .text_color(c(LIST_TIME))
                                        .child(data::time_ago(store.now_ms(), last)),
                                ),
                        ),
                ),
        )
}

// ── .chat ───────────────────────────────────────────────────────────────────────────────

fn empty_pane() -> impl IntoElement {
    div()
        .flex_1()
        .h_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .child(img("ui/logo-mark.svg").w(px(48.)).h(px(80.)).opacity(0.25))
        .child(div().mt(px(12.)).text_size(px(14.)).text_color(white_a(0.35)).child("Pick a conversation to get started"))
}

impl VectorApp {
    fn chat_pane(&self, store: &Store, id: SharedString, _cx: &mut Context<Self>) -> impl IntoElement {
        let header = div()
            .h(px(61.))
            .flex_shrink_0()
            .relative()
            .bg(c(CHAT_HEADER_BG))
            .border_b_1()
            .border_color(c(DIVIDER))
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .mt(px(5.))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(avatar(store, &id, 22.))
                    .child(
                        div()
                            .font_family(RUBIK)
                            .font_weight(FontWeight::BOLD)
                            .text_size(px(18.72))
                            .text_color(c(TEXT))
                            .child(store.name(&id)),
                    ),
            )
            .child(
                div()
                    .absolute()
                    .right(px(10.))
                    .top(px(10.))
                    .flex()
                    .child(
                        div()
                            .size(px(40.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(svg().path("ui/call.svg").size(px(22.)).text_color(c(ICON_MUTED))),
                    )
                    .child(
                        div()
                            .size(px(40.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(svg().path("ui/menu-dots.svg").size(px(28.)).text_color(c(ICON_MUTED))),
                    ),
            );

        let rows_store = self.store.clone();
        let rows_id = id.clone();
        let rows = self.rows.clone();
        let messages = list(self.timeline.clone(), move |i, _, _| {
            let rows_store = rows_store.borrow();
            let (Some(chat), Some(row)) = (rows_store.chat(&rows_id), rows.get(i)) else {
                return div().into_any_element();
            };
            // .dmsg carries a 14px top margin, zeroed after a divider; dividers take 10px each side.
            let after_message = i > 0 && matches!(rows[i - 1], Row::Message(_));
            timeline_row(&rows_store, chat, row, after_message)
        })
        .size_full();

        div()
            .flex_1()
            .h_full()
            .min_w_0()
            .flex()
            .flex_col()
            .bg(c(CHAT_BG))
            .child(header)
            .child(div().flex_1().min_h_0().bg(c(MESSAGES_BG)).pt(px(12.)).child(messages))
            .child(composer(self.input.clone()))
    }
}

fn timeline_row(store: &Store, chat: &SerializableChat, row: &Row, after_message: bool) -> AnyElement {
    match row {
        Row::Date { label, time } => divider(
            div()
                .flex()
                .text_size(px(12.))
                .text_color(c(TEXT))
                .child(div().font_weight(FontWeight::BOLD).child(label.clone()))
                .child(format!(", {time}")),
            white_a(0.1),
            0.7,
        ),
        Row::Unread => divider(
            // letter-spacing: 0.08em, which GPUI text has no property for.
            div()
                .flex()
                .gap(px(0.88))
                .font_weight(FontWeight::BOLD)
                .text_size(px(11.))
                .text_color(c(ACCENT))
                .children("NEW".chars().map(|ch| div().child(ch.to_string()))),
            ca(89, 252, 179, 0.6),
            1.,
        ),
        Row::Message(i) => message_row(store, &chat.messages[*i], after_message),
    }
}

/// `.msg-inline-timestamp`: a centred label with a hairline either side.
fn divider(label: impl IntoElement, line: gpui::Hsla, opacity: f32) -> AnyElement {
    let rule = move || div().flex_1().h(px(1.)).bg(line);
    div()
        .px(px(10.))
        .py(px(10.))
        .child(
            div()
                .h(px(24.))
                .flex()
                .items_center()
                .gap(px(12.))
                .font_family(RUBIK)
                .opacity(opacity)
                .child(rule())
                .child(label)
                .child(rule()),
        )
        .into_any_element()
}

/// `.dmsg` (MessageRow.svelte).
fn message_row(store: &Store, m: &Message, after_message: bool) -> AnyElement {
    let author = m.npub.clone().unwrap_or_default();
    div()
        .when(after_message, |d| d.pt(px(14.)))
        .h(px(if after_message { 59. } else { 45. }))
        .child(
            div()
                .flex()
                .pl(px(15.))
                .pr(px(12.))
                .child(div().w(px(56.)).flex_shrink_0().pt(px(2.)).child(avatar(store, &author, 40.)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .font_family(RUBIK)
                        .child(
                            div()
                                .h(px(24.))
                                .flex()
                                .items_center()
                                .gap(px(8.))
                                .child(
                                    div()
                                        .font_weight(FontWeight::BOLD)
                                        .text_size(px(15.))
                                        .line_height(px(19.5))
                                        .text_color(c(0xffffff))
                                        .child(store.name(&author)),
                                )
                                .child(div().text_size(px(12.)).text_color(white_a(0.4)).child(data::clock(m.at))),
                        )
                        .child(
                            div()
                                .mt(px(-2.))
                                .text_size(px(15.))
                                .line_height(px(21.))
                                .text_color(white_a(0.85))
                                .child(m.content.clone()),
                        ),
                ),
        )
        .into_any_element()
}

/// `#chat-box.input-box`: the gradient frame around the composer.
fn composer(input: Entity<TextInput>) -> impl IntoElement {
    let icon = |path: &'static str| {
        div()
            .w(px(38.))
            .h_full()
            .flex()
            .items_center()
            .justify_center()
            .child(svg().path(path).size(px(23.)).text_color(c(ICON_MUTED)))
    };
    div()
        .h(px(55.))
        .flex_shrink_0()
        .px(px(8.))
        .pt(px(5.))
        .pb(px(5.))
        .bg(linear_gradient(
            90.,
            linear_color_stop(c(ACCENT), 0.),
            linear_color_stop(c(ACCENT_DEEP), 1.),
        ))
        .child(
            div()
                .size_full()
                .rounded(px(10.))
                .bg(c(APP_BG))
                .flex()
                .items_center()
                .child(icon("icons/plus.svg"))
                .child(div().flex_1().min_w_0().child(input))
                .child(icon("icons/smile-face.svg"))
                .child(icon("icons/mic-on.svg")),
        )
}
