//! `refreshTimeoutLock`: a community's running Timeouts, and which authors in the open channel
//! can't send there. Both collapse posts and the first can lock the composer. Also
//! `loadChannelReactDenied`: which reactors there can't react, whose reactions don't count.

use std::collections::{HashMap, HashSet};

use gpui::Context;

use super::VectorApp;
use crate::backend;
use crate::data::Timeout;

impl VectorApp {
    pub(super) fn refresh_timeouts(&mut self, community: &str, cx: &mut Context<Self>) {
        if !self.live {
            return;
        }
        let running: HashMap<String, Timeout> = serde_json::from_value(backend::community::timeouts(community)).unwrap_or_default();
        let open = self.open.as_ref().map(|s| s.to_string()).filter(|o| self.store.borrow().community_of(o).is_some_and(|c| c.community_id == community));
        let denied = open.as_ref().map(|ch| {
            let store = self.store.borrow();
            let authors: Vec<String> = store
                .chat(ch)
                .map(|c| c.messages.iter().filter(|m| !m.mine).filter_map(|m| m.npub.clone()).collect::<HashSet<_>>().into_iter().collect())
                .unwrap_or_default();
            (ch.clone(), backend::community::send_denied(community, ch, &authors))
        });
        let soonest = running.values().map(|t| t.until).min();
        let mut store = self.store.borrow_mut();
        let mut changed = store.timeouts.get(community) != Some(&running);
        store.timeouts.insert(community.to_string(), running);
        if let Some((ch, denied)) = denied {
            changed |= store.send_denied.get(&ch) != Some(&denied);
            store.send_denied.insert(ch, denied);
        }
        let now = store.now_ms() / 1000;
        drop(store);
        if changed && open.is_some() {
            self.refresh_rows(false);
        }
        if let Some(ch) = &open {
            self.load_react_denied(community, ch);
        }
        cx.notify();
        self.timeout_expiry = soonest.map(|until| {
            let community = community.to_string();
            let wait = until.saturating_sub(now) + 1;
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(std::time::Duration::from_secs(wait.min(28 * 86_400))).await;
                this.update(cx, |app, cx| app.refresh_timeouts(&community, cx)).ok();
            })
        });
    }

    /// Ask again which of the open channel's reactors can't react there. Says whether that moved.
    fn load_react_denied(&mut self, community: &str, channel: &str) -> bool {
        let reactors: HashSet<String> = {
            let store = self.store.borrow();
            store
                .chat(channel)
                .map(|c| c.messages.iter().flat_map(|m| m.reactions.iter().map(|r| r.author_id.clone())).collect())
                .unwrap_or_default()
        };
        let list: Vec<String> = reactors.iter().cloned().collect();
        let denied = backend::community::react_denied(community, channel, &list);
        let mut store = self.store.borrow_mut();
        store.react_checked.insert(channel.to_string(), reactors);
        let moved = store.react_denied.get(channel) != Some(&denied);
        store.react_denied.insert(channel.to_string(), denied);
        drop(store);
        if moved {
            self.refresh_rows(false);
        }
        moved
    }

    /// A reactor in the open channel nobody has asked about counts until the next read.
    pub(super) fn check_reactors(&mut self, cx: &mut Context<Self>) {
        if !self.live || self.react_recheck.is_some() {
            return;
        }
        let Some(channel) = self.open.as_ref().map(|s| s.to_string()) else { return };
        let fresh = {
            let store = self.store.borrow();
            let Some(checked) = store.react_checked.get(&channel) else { return };
            store.chat(&channel).is_some_and(|c| c.messages.iter().flat_map(|m| m.reactions.iter()).any(|r| !checked.contains(&r.author_id)))
        };
        if !fresh {
            return;
        }
        self.react_recheck = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(std::time::Duration::from_millis(250)).await;
            this.update(cx, |app, cx| {
                app.react_recheck = None;
                if app.open.as_deref() != Some(channel.as_str()) {
                    return;
                }
                let community = app.store.borrow().community_of(&channel).map(|c| c.community_id.clone());
                if let Some(community) = community
                    && app.load_react_denied(&community, &channel)
                {
                    crate::pane::notify(&app.panes.timeline, cx);
                }
            })
            .ok();
        }));
    }
}
