//! `chat-scroll.js`'s scrollbar half: where the view sits in the whole chat, not in the run the
//! timeline holds. A row's place is its rank among the chat's messages, and a seek past what is
//! held loads the page there, leaving a gap before the held tail that scrolling down fills.

use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{Context, Window};
use vector_core::Message;

use super::VectorApp;
use crate::backend::{self, messaging};
use crate::data::Row;
use crate::scrollbar::{Ev, OnEv, date};

const RANK_TTL: Duration = Duration::from_secs(2);
const PAGE: usize = 80;
const STEP: usize = 20;

fn counted(m: &Message) -> bool {
    !m.pending && !m.failed
}

impl VectorApp {
    pub(super) fn bar_on(&self, cx: &mut Context<Self>) -> OnEv {
        let app = cx.entity().downgrade();
        Rc::new(move |ev, window, cx| {
            app.update(cx, |app, cx| app.bar_event(ev, window, cx)).ok();
        })
    }

    fn bar_event(&mut self, ev: Ev, _window: &mut Window, cx: &mut Context<Self>) {
        let bar = &mut self.bar;
        let th = bar.track_h();
        match ev {
            Ev::Down(y) => {
                let (h, top) = bar.thumb();
                // On the thumb: hold it where it was taken. On the track: centre it under the pointer.
                bar.grab = if bar.on_thumb(y) { y - top } else { h / 2. };
                let p = bar.at(y, bar.grab);
                self.bar_seek(p, false, cx);
            }
            Ev::Move(y) => {
                if bar.dragging {
                    let p = bar.at(y, bar.grab);
                    self.bar_seek(p, false, cx);
                } else {
                    bar.hover = Some(y.clamp(0., th));
                    if !bar.on_thumb(y) {
                        let (h, _) = bar.thumb();
                        let p = bar.at(y, h / 2.);
                        self.bar_point(p, cx);
                    }
                }
            }
            Ev::Up(y) => {
                let p = bar.at(y, bar.grab);
                // Let go over the track: still on it.
                let inside = (0. ..=th).contains(&y);
                bar.hover = inside.then_some(y);
                self.bar_seek(p, true, cx);
            }
            Ev::Leave => bar.hover = None,
            Ev::Wheel(dy) => self.timeline.scroll_by(gpui::px(-dy)),
        }
        cx.notify();
    }

    /// `updateChatScrollbar`: the view's place and size in the chat, measured off the last layout.
    pub(super) fn bar_update(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(chat_id) = self.open.clone() else { return };
        let list = self.timeline.clone();
        let view = list.viewport_bounds();
        let (edge, bottom) = (view.top(), view.bottom());
        let store = self.store.borrow();
        let Some(chat) = store.chat(&chat_id) else { return };
        let msgs = &chat.messages;
        if !msgs.iter().any(counted) || view.size.height <= gpui::px(0.) {
            self.bar.shown = false;
            return;
        }
        let mut top = None;
        let mut visible = 0.;
        let mut last_seen = 0;
        for ix in list.logical_scroll_top().item_ix..self.rows.len() {
            let Some(b) = list.bounds_for_item(ix) else { break };
            if b.bottom() <= edge {
                continue;
            }
            if b.top() >= bottom {
                break;
            }
            last_seen = ix;
            let Row::Message { index, .. } = self.rows[ix] else { continue };
            let h = f32::from(b.size.height);
            if h <= 0. {
                continue;
            }
            if top.is_none() {
                top = Some((index, (f32::from(edge - b.top()) / h).clamp(0., 1.)));
            }
            visible += f32::from(b.bottom().min(bottom) - b.top().max(edge)) / h;
        }
        let from_end = f32::from(list.max_offset_for_scrollbar().y + list.scroll_px_offset_for_scrollbar().y);
        let following = list.is_following_tail() || from_end < 2.;
        if following && self.gap.is_none() {
            self.unread_below = 0;
        }
        self.return_shown = self.gap.is_some() || (!following && from_end > 500.);
        let Some((i, into)) = top else { return };
        let gap = self.gap_index(&store);
        let mut j = match gap {
            Some(g) if i <= g => g,
            _ => msgs.len() - 1,
        };
        while j > i && !counted(&msgs[j]) {
            j -= 1;
        }
        self.bar.here = date(msgs[i].at);
        let sig = (i, msgs[j].id.clone(), msgs.len());
        let fresh = self.bar.sig.as_ref() != Some(&sig);
        let near_end = gap.is_some() && last_seen + 4 >= self.rows.len();
        let rank = if self.live {
            let id = msgs[j].id.clone();
            drop(store);
            self.bar_rank(chat_id.to_string(), id, fresh, cx)
        } else {
            let newer = msgs[j + 1..].iter().filter(|m| counted(m)).count();
            let total = msgs.iter().filter(|m| counted(m)).count();
            drop(store);
            Some((newer, total))
        };
        if near_end {
            self.load_newer(cx);
        }
        // A seek lands without a scroll event: the history above it pages in from here.
        if fresh && list.logical_scroll_top().item_ix < 4 {
            self.load_older(cx);
        }
        if fresh {
            // Bounds lag the layout by a frame: measure again once it settles.
            window.request_animation_frame();
        }
        self.bar.sig = Some(sig.clone());
        let Some((newer_j, total)) = rank else { return };
        let store = self.store.borrow();
        let Some(msgs) = store.chat(&chat_id).map(|c| &c.messages) else { return };
        let newer = newer_j + msgs[i + 1..=j].iter().filter(|m| counted(m)).count();
        let total = total.max(1);
        let at = (total as f32 - 1. - newer as f32).max(0.) + into;
        self.bar.anchor = Some((sig.1, newer_j));
        self.bar.total = total;
        self.bar.visible = visible;
        let overflows = list.max_offset_for_scrollbar().y > gpui::px(1.);
        self.bar.shown = overflows && total as f32 > visible + 0.5;
        self.bar.size = (visible / total as f32).min(1.);
        if !self.bar.dragging {
            self.bar.top = (at / (total as f32 - visible).max(1.)).clamp(0., 1.);
        }
    }

    /// A held message's rank, asked of the database at most once at a time; a stale answer
    /// serves until the fresh one lands.
    fn bar_rank(&mut self, chat: String, id: String, moved: bool, cx: &mut Context<Self>) -> Option<(usize, usize)> {
        let hit = self.bar.ranks.get(&id).copied();
        let stale = hit.is_none_or(|(_, at)| moved && at.elapsed() >= RANK_TTL);
        if stale && self.bar.asking.as_ref() != Some(&id) {
            self.bar.asking = Some(id.clone());
            if self.bar.ranks.len() > 200 {
                self.bar.ranks.clear();
            }
            let (c, m) = (chat.clone(), id.clone());
            let rx = backend::run(async move { messaging::message_position(&c, &m) });
            cx.spawn(async move |this, cx| {
                let pos = rx.await.ok().flatten();
                this.update(cx, |this, cx| {
                    this.bar.ranks.insert(id.clone(), (pos, Instant::now()));
                    if this.bar.asking.as_ref() == Some(&id) {
                        this.bar.asking = None;
                    }
                    this.bar.sig = None;
                    if this.open.as_deref() == Some(chat.as_str()) {
                        cx.notify();
                    }
                })
                .ok();
            })
            .detach();
        }
        hit.and_then(|(p, _)| p)
    }

    /// The seeked run's newest message, while a gap lies between it and the held tail.
    pub(super) fn gap_index(&self, store: &crate::data::Store) -> Option<usize> {
        let (chat, id) = self.gap.as_ref()?;
        if self.open.as_deref() != Some(chat.as_str()) {
            return None;
        }
        store.chat(chat)?.messages.iter().position(|m| &m.id == id)
    }

    /// The held message `newer` places from the newest, when the view's run reaches it.
    fn bar_held_at(&self, newer: usize) -> Option<String> {
        let (anchor, newer_j) = self.bar.anchor.clone()?;
        let store = self.store.borrow();
        let msgs = &store.chat(self.open.as_deref()?)?.messages;
        let j = msgs.iter().position(|m| m.id == anchor)?;
        let start = match self.gap_index(&store) {
            Some(g) if j > g => g + 1,
            _ => 0,
        };
        let mut left = newer.checked_sub(newer_j)?;
        for m in msgs[start..=j].iter().rev() {
            if !counted(m) {
                continue;
            }
            if left == 0 {
                return Some(m.id.clone());
            }
            left -= 1;
        }
        None
    }

    /// A drag or click on the track: `p` is the view's place, 0 (the start) to 1 (the newest).
    fn bar_seek(&mut self, p: f32, done: bool, cx: &mut Context<Self>) {
        self.bar.dragging = !done;
        self.bar.top = p;
        self.bar_point(p, cx);
        self.bar.seek_want = Some(p);
        if !self.bar.seeking {
            self.bar_run_seeks(cx);
        }
    }

    /// The date at a place on the track, read from the database: the latest place wins.
    fn bar_point(&mut self, p: f32, cx: &mut Context<Self>) {
        let Some(chat) = self.open.clone().map(|c| c.to_string()) else { return };
        if self.bar.total == 0 {
            return;
        }
        let newer = self.bar.newer_at(p);
        if !self.live {
            let store = self.store.borrow();
            let at = store.chat(&chat).and_then(|c| c.messages.iter().rev().filter(|m| counted(m)).nth(newer)).map(|m| m.at);
            if let Some(at) = at {
                self.bar.tip = date(at);
            }
            return;
        }
        let key = (chat.clone(), newer);
        if let Some(at) = self.bar.times.get(&key) {
            if let Some(at) = at {
                self.bar.tip = date(*at);
            }
            return;
        }
        self.bar.point_want = Some(p);
        if self.bar.pointing {
            return;
        }
        self.bar.pointing = true;
        let rx = backend::run(async move { messaging::message_time_at(&chat, newer) });
        cx.spawn(async move |this, cx| {
            let at = rx.await.ok().flatten();
            this.update(cx, |this, cx| {
                this.bar.pointing = false;
                if this.bar.times.len() > 500 {
                    this.bar.times.clear();
                }
                this.bar.times.insert(key, at);
                if let Some(p) = this.bar.point_want.take() {
                    this.bar_point(p, cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn bar_run_seeks(&mut self, cx: &mut Context<Self>) {
        while let Some(p) = self.bar.seek_want.take() {
            if !self.bar_seek_to(p, cx) {
                return;
            }
        }
    }

    /// One seek; false while a page it asked for is on its way.
    fn bar_seek_to(&mut self, p: f32, cx: &mut Context<Self>) -> bool {
        let Some(chat) = self.open.clone() else { return true };
        if self.bar.total == 0 {
            return true;
        }
        let newer = self.bar.newer_at(p);
        if newer as f32 <= self.bar.visible {
            self.to_tail();
            return true;
        }
        if let Some(id) = self.bar_held_at(newer) {
            self.bar_land(&id, p);
            return true;
        }
        if !self.live {
            return true;
        }
        self.bar.seeking = true;
        let offset = newer.saturating_sub(STEP * 2);
        let rx = backend::run(messaging::message_views(chat.to_string(), PAGE, offset));
        cx.spawn(async move |this, cx| {
            let page = rx.await.ok().and_then(Result::ok).unwrap_or_default();
            this.update(cx, |this, cx| {
                this.bar.seeking = false;
                if this.open.as_ref() == Some(&chat) && !page.is_empty() {
                    let target = page[(newer - offset).min(page.len() - 1)].id.clone();
                    this.bar_place_page(&chat, page, offset);
                    this.bar_land(&target, p);
                }
                this.bar.sig = None;
                this.bar_run_seeks(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
        false
    }

    /// A page fetched for a seek joins the held tail, or sits before it across a gap.
    fn bar_place_page(&mut self, chat: &str, mut page: Vec<Message>, offset: usize) {
        page.reverse();
        let gap = {
            let store = self.store.borrow();
            self.gap_index(&store)
        };
        let mut store = self.store.borrow_mut();
        let Some(c) = store.chat_mut(chat) else { return };
        let tail: Vec<Message> = match gap {
            Some(g) => c.messages.split_off(g + 1),
            None => std::mem::take(&mut c.messages),
        };
        let held = tail.iter().filter(|m| counted(m)).count();
        if offset <= held {
            let have: std::collections::HashSet<&str> = tail.iter().map(|m| m.id.as_str()).collect();
            let fresh: Vec<Message> = page.into_iter().filter(|m| !have.contains(m.id.as_str())).collect();
            c.messages = tail;
            c.messages.extend(fresh);
            c.messages.sort_by_key(|m| m.at);
            self.gap = None;
        } else {
            self.gap = page.last().map(|m| (chat.to_string(), m.id.clone()));
            c.messages = page;
            c.messages.extend(tail);
        }
        drop(store);
        self.history_done.remove(chat);
        let old = self.timeline.item_count();
        let new = self.rebuild_rows();
        self.timeline.splice(0..old, new);
    }

    /// The seek's row at the top of the view, with the day divider above it.
    fn bar_land(&mut self, id: &str, p: f32) {
        let Some(ix) = self.row_of(id) else { return };
        let mut lead = ix;
        let mut k = ix;
        while k > 0 && !matches!(self.rows[k - 1], Row::Message { .. }) {
            k -= 1;
            if matches!(self.rows[k], Row::Date { .. }) {
                lead = k;
            }
        }
        self.timeline.pause_following_tail();
        let ix = if p <= 0. { 0 } else { lead };
        self.timeline.scroll_to(gpui::ListOffset { item_ix: ix, offset_in_item: gpui::px(0.) });
    }

    /// The live tail, whatever the view had seeked to: the seeked run goes.
    pub(super) fn to_tail(&mut self) {
        self.drop_gap();
        self.timeline.scroll_to_end();
    }

    /// The seeked run and its gap dropped, leaving the held tail.
    pub(super) fn drop_gap(&mut self) {
        let Some((chat, id)) = self.gap.take() else { return };
        {
            let mut store = self.store.borrow_mut();
            let Some(c) = store.chat_mut(&chat) else { return };
            let Some(g) = c.messages.iter().position(|m| m.id == id) else { return };
            c.messages.drain(..=g);
        }
        self.history_done.remove(&chat);
        if self.open.as_deref() == Some(chat.as_str()) {
            let old = self.timeline.item_count();
            let new = self.rebuild_rows();
            self.timeline.splice(0..old, new);
        }
    }

    /// The page after the gap, joined to the run before it; the gap closes once it meets the tail.
    fn load_newer(&mut self, cx: &mut Context<Self>) {
        let Some((chat, id)) = self.gap.clone() else { return };
        if self.paging_newer || !self.live {
            return;
        }
        let tail_first = {
            let store = self.store.borrow();
            let Some(g) = self.gap_index(&store) else { return };
            store.chat(&chat).and_then(|c| c.messages.get(g + 1)).map(|m| m.id.clone())
        };
        self.paging_newer = true;
        let rx = backend::run(messaging::messages_around(chat.clone(), id, 1, 100));
        cx.spawn(async move |this, cx| {
            let res = rx.await;
            this.update(cx, |this, cx| {
                this.paging_newer = false;
                if this.gap.as_ref().map(|g| &g.0) != Some(&chat) {
                    return;
                }
                let Ok(Ok(views)) = res else { return };
                let met = views.len() < 101 || tail_first.as_ref().is_some_and(|t| views.iter().any(|m| &m.id == t));
                let newest = views.last().map(|m| m.id.clone());
                this.merge_messages(&chat, views);
                this.gap = match (met, newest) {
                    (false, Some(n)) => Some((chat.clone(), n)),
                    _ => None,
                };
                this.relayout_keeping_top();
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}
