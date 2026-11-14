//! The chat list's pins, its Archived Chats row and each row's context menu
//! (`render/chatlist/row.js`, `ArchiveRow.svelte`).

use std::time::{Duration, Instant};

use gpui::{AnyElement, Context, FontWeight, ListOffset, Pixels, Point, Task, div, prelude::*, px, svg};

use super::{MenuAct, ROW_PITCH, VectorApp, backend, kit};
use crate::data::Store;
use crate::theme::*;

/// Wheel travel past the top that commits a reveal.
const WHEEL_PX: f32 = 160.;
/// Wheel silence that ends a gesture, so the momentum that reached the top never counts.
const GESTURE_GAP: Duration = Duration::from_millis(220);
const SETTLE: f32 = 0.2;

#[derive(Clone, Copy, PartialEq)]
pub(super) enum Row {
    Archive,
    Chat { idx: usize, archived: bool, last: bool },
}

#[derive(Default)]
pub(super) struct Fold {
    revealed: bool,
    pub expanded: bool,
    pull: f32,
    /// Where the slot's height eases from, and since when.
    settle: Option<(f32, Instant)>,
    wheel_at: Option<Instant>,
    from_top: bool,
    travel: f32,
    timer: Option<Task<()>>,
    /// Laid out in the slim list, whose rows are taller.
    pub slim: bool,
}

impl Fold {
    pub(super) fn pitch(&self) -> f32 {
        if self.slim { super::slim_ops::ROW_PITCH } else { ROW_PITCH }
    }

    pub(super) fn shown(&self, forced: bool) -> bool {
        forced || self.revealed
    }

    /// The row fades in with the pull that draws it down.
    pub(super) fn opacity(&self, shown: bool) -> f32 {
        if shown { 1. } else { (self.pull / self.pitch()).min(1.) }
    }

    pub(super) fn height(&self, shown: bool) -> f32 {
        let pitch = self.pitch();
        let target = if shown { pitch } else { self.pull.min(pitch) };
        match self.settle {
            Some((from, at)) => {
                let t = (at.elapsed().as_secs_f32() / SETTLE).clamp(0., 1.);
                from + (target - from) * (1. - (1. - t).powi(3))
            }
            None => target,
        }
    }

    pub fn animating(&self) -> bool {
        self.settle.is_some_and(|(_, at)| at.elapsed().as_secs_f32() < SETTLE)
    }

    fn ease_from(&mut self, from: f32) {
        self.settle = Some((from, Instant::now()));
    }
}

/// The rows the list shows: the archive row and what it unfolds, then the rest with pins first.
pub(super) fn rows(store: &Store, ids: &[usize], fold: &Fold, others: bool) -> (Vec<Row>, bool) {
    let (mut archived, mut rest): (Vec<usize>, Vec<usize>) = ids.iter().partition(|&&i| store.is_archived(&store.chats[i]));
    rest.sort_by_key(|&i| store.pin_rank(&store.chats[i].id).unwrap_or(usize::MAX));
    archived.sort_by_key(|&i| store.pin_rank(&store.chats[i].id).unwrap_or(usize::MAX));
    let forced = rest.is_empty() && !others;
    let shown = forced || fold.revealed;
    let mut out = Vec::with_capacity(ids.len() + 1);
    if !archived.is_empty() && (shown || fold.height(false) > 0.5) {
        out.push(Row::Archive);
        if shown && fold.expanded {
            let n = archived.len();
            out.extend(archived.iter().enumerate().map(|(k, &idx)| Row::Chat { idx, archived: true, last: k + 1 == n }));
        }
    }
    out.extend(rest.into_iter().map(|idx| Row::Chat { idx, archived: false, last: false }));
    (out, forced)
}

impl VectorApp {
    /// The list the archive row sits atop: the slim one while that layout is up.
    fn fold_list(&self) -> gpui::ListState {
        if self.archive.slim { self.slim.list.clone() } else { self.chatlist.clone() }
    }

    fn at_top(&self) -> bool {
        let top = self.fold_list().logical_scroll_top();
        top.item_ix == 0 && top.offset_in_item <= px(0.5)
    }

    /// A pull past the top draws the archive down; a scroll back tucks it away.
    pub(super) fn archive_wheel(&mut self, e: &gpui::ScrollEvent, forced: bool, has: bool, cx: &mut Context<Self>) {
        if !has {
            return;
        }
        let dy = -f32::from(e.delta.pixel_delta(px(20.)).y);
        let now = Instant::now();
        let list = self.fold_list();
        let f = &mut self.archive;
        if f.wheel_at.is_none_or(|t| now - t > GESTURE_GAP) {
            let top = list.logical_scroll_top();
            f.from_top = top.item_ix == 0 && top.offset_in_item <= px(0.5);
            f.travel = 0.;
        }
        f.wheel_at = Some(now);
        let shown = forced || f.revealed;
        if dy > 0. && f.revealed && !f.expanded && !forced {
            let pitch = f.pitch();
            if f32::from(list.max_offset_for_scrollbar().y) < pitch {
                f.ease_from(pitch);
                f.revealed = false;
                cx.notify();
            }
            return;
        }
        if shown || !f.from_top {
            return;
        }
        if dy > 0. || !self.at_top() {
            let f = &mut self.archive;
            f.from_top = false;
            if f.pull > 0. {
                f.ease_from(f.pull);
                f.pull = 0.;
                cx.notify();
            }
            return;
        }
        let f = &mut self.archive;
        f.travel -= dy;
        f.settle = None;
        f.pull = f.pitch() * (f.travel / WHEEL_PX).min(1.) * 0.85;
        if f.travel >= WHEEL_PX {
            let from = f.pull;
            f.revealed = true;
            f.pull = 0.;
            f.ease_from(from);
            f.timer = None;
        } else {
            f.timer = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(GESTURE_GAP).await;
                this.update(cx, |this, cx| {
                    let f = &mut this.archive;
                    if !f.revealed && f.pull > 0. {
                        f.ease_from(f.pull);
                        f.pull = 0.;
                        cx.notify();
                    }
                })
                .ok();
            }));
        }
        cx.notify();
    }

    /// Scrolled past while folded: tuck it away without moving the rows on screen.
    pub(super) fn archive_scrolled(&mut self, forced: bool) {
        let f = &mut self.archive;
        if !f.revealed || f.expanded || forced {
            return;
        }
        let list = if f.slim { self.slim.list.clone() } else { self.chatlist.clone() };
        let top = list.logical_scroll_top();
        if top.item_ix >= 1 {
            f.revealed = false;
            f.settle = None;
            list.scroll_to(ListOffset { item_ix: top.item_ix - 1, offset_in_item: top.offset_in_item });
        }
    }

    /// The model's rows into the list state: reset when they changed, the slot remeasured as it moves.
    pub(super) fn sync_chatlist(&self, rows: &[Row], store: &Store) {
        let keys: Vec<(u8, String)> = rows
            .iter()
            .map(|r| match r {
                Row::Archive => (0, String::new()),
                Row::Chat { idx, archived, .. } => (1 + *archived as u8, store.chats[*idx].id.clone()),
            })
            .collect();
        let mut last = self.chatlist_keys.borrow_mut();
        if *last != keys {
            let top = self.chatlist.logical_scroll_top();
            self.chatlist.reset(keys.len());
            self.chatlist.scroll_to(top);
            *last = keys;
        } else if rows.first() == Some(&Row::Archive) {
            self.chatlist.remeasure_items(0..1);
        }
    }

    /// `ArchiveRow.svelte`: the folded chats' names and count, tucked above the list.
    pub(super) fn archive_row(&self, store: &Store, ids: &[usize], forced: bool, cx: &mut Context<Self>) -> AnyElement {
        let f = &self.archive;
        let shown = forced || f.revealed;
        let archived: Vec<&vector_core::chat::SerializableChat> =
            ids.iter().map(|&i| &store.chats[i]).filter(|c| store.is_archived(c)).collect();
        let names = archived.iter().map(|c| store.name(&c.id)).collect::<Vec<_>>().join(", ");
        let unread = archived.iter().any(|c| store.unread_count(c) > 0);
        let height = f.height(shown);
        let opacity = if shown { 1. } else { (f.pull / ROW_PITCH).min(1.) };
        let expanded = f.expanded;
        div()
            .h(px(height))
            .overflow_hidden()
            .flex()
            .flex_col()
            .justify_end()
            .child(
                div()
                    .id("chatlist-archive")
                    .h(px(ROW_PITCH))
                    .w(px(230.))
                    .pb(px(4.))
                    .flex_shrink_0()
                    .opacity(opacity)
                    .when(shown, |d| {
                        d.cursor_pointer().on_click(cx.listener(|app, _, _, cx| {
                            app.archive.expanded = !app.archive.expanded;
                            cx.notify();
                        }))
                    })
                    .child(
                        div()
                            .h(px(50.))
                            .flex()
                            .items_center()
                            .pl(px(12.))
                            .child(
                                div()
                                    .size(px(40.))
                                    .flex_shrink_0()
                                    .rounded_full()
                                    .bg(white_a(0.06))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .child(svg().path("icons/archive.svg").size(px(18.)).text_color(c(ACCENT))),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .h(px(34.))
                                    .flex()
                                    .flex_col()
                                    .child(
                                        div()
                                            .h(px(20.))
                                            .pl(px(10.))
                                            .pr(px(10.))
                                            .flex()
                                            .items_center()
                                            .gap(px(6.))
                                            .child(
                                                div()
                                                    .min_w_0()
                                                    .truncate()
                                                    .font_family(RUBIK)
                                                    .text_size(px(15.))
                                                    .line_height(px(20.))
                                                    .text_color(white_a(0.75))
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
                                            .mt(px(-4.))
                                            .h(px(18.))
                                            .ml(px(10.))
                                            .mr(px(8.))
                                            .flex()
                                            .items_center()
                                            .child(
                                                div()
                                                    .min_w_0()
                                                    .truncate()
                                                    .font_family(RUBIK)
                                                    .text_size(px(13.))
                                                    .line_height(px(18.))
                                                    .text_color(ca(160, 160, 160, 0.5))
                                                    .child(names),
                                            ),
                                    ),
                            )
                            .child(
                                div()
                                    .id("chatlist-expander")
                                    .w(px(26.))
                                    .ml(px(2.))
                                    .mr(px(5.))
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

    pub(super) fn start_menu_on(&self) -> bool {
        backend::device::get("ws_nav").as_deref() != Some("navbar")
    }

    /// `startPinItems`: the Main Menu toggle for a pinnable place, while the Main Menu is in use.
    pub(super) fn start_pin_row(&self, key: String) -> Option<(kit::MenuItem, MenuAct)> {
        self.start_menu_on().then(|| {
            let item = if self.start_pinned(&key) {
                kit::MenuItem::new("Unpin", "pin").danger()
            } else {
                kit::MenuItem::new("Pin to Main Menu", "pin")
            };
            (item, MenuAct::StartTogglePin(key))
        })
    }

    /// `_showChatRowContextMenu` for a DM row.
    pub(super) fn chat_row_menu(&mut self, id: String, at: Point<Pixels>, cx: &mut Context<Self>) {
        let (unread, muted, can_unread, pinned, archived, me) = {
            let store = self.store.borrow();
            let Some(chat) = store.chat(&id) else { return };
            let can = chat.messages.last().is_some_and(|m| !m.mine);
            (
                store.unread_count(chat) > 0,
                chat.muted,
                can,
                store.pin_rank(&id).is_some(),
                store.is_archived(chat),
                store.my_npub == id,
            )
        };
        let mut rows: Vec<(kit::MenuItem, MenuAct)> = Vec::new();
        if unread {
            rows.push((kit::MenuItem::new("Mark as Read", "check"), MenuAct::ChatRead(id.clone())));
        } else if !muted && can_unread {
            rows.push((kit::MenuItem::new("Mark as Unread", "eye-off"), MenuAct::ChatUnread(id.clone())));
        }
        if !(self.live && self.syncing) {
            rows.extend(self.notify_rows(&id, None));
        }
        rows.push((kit::MenuItem::new(if pinned { "Unpin" } else { "Pin" }, "pin"), MenuAct::ChatPin(id.clone(), !pinned)));
        rows.push((
            kit::MenuItem::new(if archived { "Unarchive" } else { "Archive" }, "archive"),
            MenuAct::ChatArchive(id.clone(), !archived),
        ));
        if !me {
            rows.extend(self.start_pin_row(format!("dm/{id}")));
        }
        if self.advanced() && !self.store.borrow().stream_hidden(&id) {
            rows.push((kit::MenuItem::new("Copy User ID", "channel-hash"), MenuAct::CopyId(id.clone(), "User")));
        }
        rows.push((kit::MenuItem::divider(), MenuAct::Nothing));
        rows.push((kit::MenuItem::new("Block", "x-user").danger(), MenuAct::ChatBlock(id)));
        self.show_menu(at, rows, cx);
    }

    pub(super) fn chat_mark_read(&mut self, id: String, cx: &mut Context<Self>) {
        {
            let mut store = self.store.borrow_mut();
            if let Some(chat) = store.chat_mut(&id) {
                chat.last_read = chat.messages.last().map(|m| m.id.clone()).unwrap_or_default();
            }
            if let Some(u) = store.unread.as_mut() {
                u.remove(&id);
            }
        }
        cx.notify();
        if self.live {
            let rx = backend::run(backend::messaging::mark_as_read(id, None));
            cx.spawn(async move |this, cx| {
                let _ = rx.await;
                this.update(cx, |this, cx| this.refresh_unread(cx)).ok();
            })
            .detach();
        }
    }

    /// `markChatUnread`: the newest message from them counts again.
    pub(super) fn chat_mark_unread(&mut self, id: String, cx: &mut Context<Self>) {
        if !self.live {
            let mut store = self.store.borrow_mut();
            let Some(chat) = store.chat_mut(&id) else { return };
            let Some(i) = chat.messages.iter().rposition(|m| !m.mine) else { return };
            chat.last_read = if i == 0 { String::new() } else { chat.messages[i - 1].id.clone() };
            drop(store);
            return cx.notify();
        }
        let rx = backend::run(backend::chatlist::mark_unread(id.clone()));
        cx.spawn(async move |this, cx| {
            let Ok(Some(last)) = rx.await else { return };
            this.update(cx, |this, cx| {
                if let Some(chat) = this.store.borrow_mut().chat_mut(&id) {
                    chat.last_read = last;
                }
                this.refresh_unread(cx);
            })
            .ok();
        })
        .detach();
    }

    /// `chatPinMenuItem`: the backend holds the tier's cap and says so.
    pub(super) fn chat_pin(&mut self, id: String, pin: bool, cx: &mut Context<Self>) {
        if self.blocked_by_sync(cx) {
            return;
        }
        if !self.live {
            let mut store = self.store.borrow_mut();
            store.pinned.retain(|p| *p != id);
            if pin {
                store.pinned.push(id);
            }
            drop(store);
            return cx.notify();
        }
        let rx = backend::run(backend::chatlist::set_pinned(id, pin));
        cx.spawn(async move |this, cx| {
            let res = rx.await.unwrap_or_else(|_| Err("cancelled".into()));
            this.update(cx, |this, cx| {
                match res {
                    Ok(list) => this.store.borrow_mut().pinned = list,
                    Err(e) => this.show_toast(&e, cx),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Move a pin to just before `before`, or to the end.
    pub(super) fn chat_move_pin(&mut self, id: String, before: Option<String>, cx: &mut Context<Self>) {
        if self.blocked_by_sync(cx) {
            return;
        }
        if !self.live {
            let mut store = self.store.borrow_mut();
            store.pinned.retain(|p| *p != id);
            let at = before
                .and_then(|b| store.pinned.iter().position(|p| *p == b))
                .unwrap_or(store.pinned.len());
            store.pinned.insert(at, id);
            drop(store);
            return cx.notify();
        }
        let rx = backend::run(backend::chatlist::move_pinned(id, before));
        cx.spawn(async move |this, cx| {
            let res = rx.await.unwrap_or_else(|_| Err("cancelled".into()));
            this.update(cx, |this, cx| {
                match res {
                    Ok(list) => this.store.borrow_mut().pinned = list,
                    Err(e) => this.show_toast(&e, cx),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// `setChatArchived`: archiving reads it; the archive keeps the newest message seen here.
    pub(super) fn chat_archive(&mut self, id: String, archive: bool, cx: &mut Context<Self>) {
        if self.blocked_by_sync(cx) {
            return;
        }
        let at = archive.then(|| self.store.borrow().chat(&id).map(crate::data::last_activity).unwrap_or(0));
        if archive {
            self.chat_mark_read(id.clone(), cx);
        }
        if self.live {
            match backend::chatlist::set_archived(&id, at) {
                Ok(map) => self.store.borrow_mut().archived = map,
                Err(e) => return self.show_toast(&e, cx),
            }
        } else {
            let mut store = self.store.borrow_mut();
            match at {
                Some(at) => store.archived.insert(id.clone(), at),
                None => store.archived.remove(&id),
            };
        }
        if self.open.as_deref() == Some(id.as_str()) && archive {
            self.archive.revealed = true;
        }
        if !archive {
            return self.show_toast("Chat Unarchived", cx);
        }
        let hinted = backend::device::get("archive_hint_shown").is_some();
        backend::device::set("archive_hint_shown", "1");
        self.show_toast(
            if hinted { "Chat Archived" } else { "Chat Archived. Pull or scroll past the top of your chats to find it." },
            cx,
        );
    }

    /// `revokeOutlivedArchives`: a newer message brings a chat back on every device.
    pub(super) fn revoke_outlived_archives(&mut self) {
        if !self.live {
            return;
        }
        let outlived: Vec<(String, u64)> = {
            let store = self.store.borrow();
            store
                .archived
                .iter()
                .filter_map(|(id, &at)| {
                    let active = store.chat(id).map(crate::data::last_activity)?;
                    (active > at).then(|| (id.clone(), active))
                })
                .collect()
        };
        for (id, active) in outlived {
            if let Ok(map) = backend::chatlist::revoke_archive(&id, active) {
                self.store.borrow_mut().archived = map;
            }
        }
    }

    pub(super) fn chatlist_event(&mut self, name: &str, payload: &serde_json::Value, cx: &mut Context<Self>) {
        match name {
            "pinned_chats_updated" => {
                if let Ok(list) = serde_json::from_value(payload.clone()) {
                    self.store.borrow_mut().pinned = list;
                }
            }
            "archived_chats_updated" => {
                if let Ok(map) = serde_json::from_value(payload.clone()) {
                    self.store.borrow_mut().archived = map;
                }
            }
            _ => {}
        }
        cx.notify();
    }

    pub(super) fn load_chatlist_prefs(&mut self) {
        if self.live {
            let mut store = self.store.borrow_mut();
            store.pinned = backend::chatlist::pinned();
            store.archived = backend::chatlist::archived();
        }
    }
}

#[derive(Clone)]
pub(super) struct PinDrag {
    key: String,
    name: gpui::SharedString,
}

struct PinGhost(gpui::SharedString);

impl gpui::Render for PinGhost {
    fn render(&mut self, _: &mut gpui::Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .w(px(220.))
            .h(px(50.))
            .px(px(12.))
            .flex()
            .items_center()
            .rounded(px(10.))
            .bg(c(0x161616))
            .opacity(0.92)
            .shadow_lg()
            .font_family(RUBIK)
            .text_size(px(15.))
            .text_color(c(0xffffff))
            .child(div().truncate().child(self.0.clone()))
    }
}

/// A pinned row lifts to move among the pins shown (`keys`, in order); its top half drops
/// above it, its bottom half below. Pins this tab hides keep their places.
pub(super) fn pin_draggable(
    row: gpui::Stateful<gpui::Div>,
    key: String,
    name: gpui::SharedString,
    keys: std::rc::Rc<Vec<String>>,
    view: gpui::WeakEntity<VectorApp>,
) -> gpui::Stateful<gpui::Div> {
    let mut row = row.relative().on_drag(PinDrag { key: key.clone(), name }, |d: &PinDrag, _, _, cx| {
        cx.new(|_| PinGhost(d.name.clone()))
    });
    for top in [true, false] {
        let (key, keys, view) = (key.clone(), keys.clone(), view.clone());
        let zone = div().absolute().left(px(12.)).right(px(12.)).h(gpui::relative(0.5));
        let zone = if top { zone.top_0() } else { zone.bottom_0() };
        row = row.child(
            zone.drag_over::<PinDrag>(move |d, _, _, _| {
                if top { d.border_t_2() } else { d.border_b_2() }.border_color(c(PRIMARY))
            })
            .on_drop(move |d: &PinDrag, _, cx| {
                if d.key == key {
                    return;
                }
                let rest: Vec<&String> = keys.iter().filter(|k| **k != d.key).collect();
                let Some(t) = rest.iter().position(|k| **k == key) else { return };
                let at = t + usize::from(!top);
                if keys.iter().position(|k| *k == d.key) == Some(at) {
                    return;
                }
                let before = rest.get(at).map(|k| k.to_string());
                let moved = d.key.clone();
                view.update(cx, |app, cx| app.chat_move_pin(moved, before, cx)).ok();
            }),
        );
    }
    row
}
