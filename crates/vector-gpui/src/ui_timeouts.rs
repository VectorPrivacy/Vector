//! `refreshTimeoutLock`: a community's running Timeouts, and which authors in the open channel
//! can't send there. Both collapse posts and the first can lock the composer.

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
}
