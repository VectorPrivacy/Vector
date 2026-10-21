//! A message dated ahead of this device's (corrected) clock waits until its time instead of
//! showing at once: shown early, it would sit below everything sent after it, holding the bottom
//! of the chat. Held in memory and never marked handled, so one cut short by a restart comes back
//! through the next sync or history read, and released through the normal path, which judges it
//! afresh, notifies and counts it unread as if it arrived then.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, RwLock};

use nostr_sdk::prelude::{Event, EventId};

use crate::event_handler::InboundEventHandler;

/// How far ahead of this device a message may be dated and still show at once.
pub const TOLERANCE_MS: u64 = 5_000;

/// How far ahead a message may be dated and still be held. One further out isn't: it stays on
/// the relays for a later read once its date nears (CORD-02 §5's horizon, CORD-03 §1).
pub const HORIZON_MS: u64 = 60 * 60 * 1000;

/// How long a wait sleeps before reading the clock again, which a correction may have moved.
const RECHECK: std::time::Duration = std::time::Duration::from_secs(60);

/// Whether a message dated `at_ms` must wait.
pub fn is_early(at_ms: u64) -> bool {
    at_ms > crate::clock::now_ms().saturating_add(TOLERANCE_MS)
}

/// Which path a held wrap goes back through.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Via {
    /// A NIP-17 gift wrap.
    Dm,
    /// A Concord Chat Plane wrap.
    Community,
}

struct Releaser;

/// The handler of this account's live session, which released wraps go through.
fn releaser() -> Arc<RwLock<Option<Arc<dyn InboundEventHandler>>>> {
    crate::db::current_session().scoped::<Releaser, _>()
}

struct Holding;

/// The wraps this account holds, so one delivered again doesn't wait twice.
fn holding() -> Arc<Mutex<HashSet<EventId>>> {
    crate::db::current_session().scoped::<Holding, _>()
}

/// Start this account's live session: held wraps are released through `handler`, and the clock
/// is checked against the relays now and every [`crate::clock::RESYNC`].
pub fn start(handler: Arc<dyn InboundEventHandler>) {
    let first = releaser().write().unwrap_or_else(|e| e.into_inner()).replace(handler).is_none();
    if !first {
        return;
    }
    crate::db::spawn_bound(async move {
        loop {
            let Some(client) = crate::state::nostr_client() else { return };
            if !crate::db::current_session().is_live() {
                return;
            }
            let relays: Vec<String> = client.relays().await.keys().map(|u| u.to_string()).collect();
            crate::clock::sync_with(&relays).await;
            crate::rt::time::sleep(crate::clock::RESYNC).await;
        }
    });
}

/// Hold `wrap`, a message dated `at_ms`, until it is due, then release it through `via`. One dated
/// past [`HORIZON_MS`] is let go, to be read again once its date nears.
pub fn hold(wrap: Event, via: Via, at_ms: u64) {
    if at_ms > crate::clock::now_ms().saturating_add(HORIZON_MS) || !holding().lock().unwrap_or_else(|e| e.into_inner()).insert(wrap.id) {
        return;
    }
    crate::db::spawn_bound(async move {
        loop {
            let (due, now) = (at_ms.saturating_sub(TOLERANCE_MS), crate::clock::now_ms());
            if due <= now {
                break;
            }
            crate::rt::time::sleep(std::time::Duration::from_millis(due - now).min(RECHECK)).await;
        }
        while !crate::state::is_processing_allowed() {
            crate::rt::time::sleep(std::time::Duration::from_secs(1)).await;
        }
        holding().lock().unwrap_or_else(|e| e.into_inner()).remove(&wrap.id);
        if !crate::db::current_session().is_live() {
            return;
        }
        // No live session to show it in: a later sync or history read finds it.
        let Some(handler) = releaser().read().unwrap_or_else(|e| e.into_inner()).clone() else { return };
        match via {
            Via::Dm => {
                let _ = crate::event_handler::process_event(wrap, true, &*handler).await;
            }
            Via::Community => crate::community::v2::realtime::dispatch_event(wrap, handler).await,
        }
    });
}

#[cfg(test)]
pub(crate) fn held(id: &EventId) -> bool {
    holding().lock().unwrap_or_else(|e| e.into_inner()).contains(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_message_waits_only_past_the_tolerance() {
        let now = crate::clock::now_ms();
        assert!(!is_early(now));
        assert!(!is_early(now + TOLERANCE_MS - 1_000), "a few seconds fast still shows at once");
        assert!(is_early(now + TOLERANCE_MS + 2_000));
        assert!(is_early(now + 4 * 60 * 1000), "minutes ahead waits");
    }

    #[test]
    fn a_message_past_the_horizon_is_never_held() {
        use nostr_sdk::prelude::{EventBuilder, FinalizeEvent, Keys, Kind};
        let wrap = EventBuilder::new(Kind::GiftWrap, "").finalize(&Keys::generate()).unwrap();
        hold(wrap.clone(), Via::Dm, crate::clock::now_ms() + HORIZON_MS + 60_000);
        assert!(!held(&wrap.id), "left for a read once its date nears");
    }
}
