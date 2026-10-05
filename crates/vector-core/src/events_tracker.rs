//! Seen-event tracker for short-lived relay clients. nostr-sdk's default allocates its whole
//! 35k-entry table up front for every client; this one keeps the same bound but grows with
//! what the client actually sees, so a client that fetches a page and goes holds a page.

use std::collections::{BTreeSet, HashSet, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;

use nostr_database::error::Error;
use nostr_sdk::prelude::{
    DatabaseEventStatus, Event, EventId, Features, Filter, NostrDatabase, SaveEventStatus, Timestamp,
};

/// The default tracker's bound, so dedup behaves the same however long a client lives.
const MAX_EVENTS: usize = 35_000;

#[derive(Debug, Default)]
pub struct LazyEventsTracker {
    seen: Mutex<Seen>,
}

#[derive(Debug, Default)]
struct Seen {
    ids: HashSet<EventId>,
    order: VecDeque<EventId>,
}

impl Seen {
    fn insert(&mut self, id: EventId) {
        if !self.ids.insert(id) {
            return;
        }
        self.order.push_back(id);
        if self.order.len() > MAX_EVENTS {
            if let Some(old) = self.order.pop_front() {
                self.ids.remove(&old);
            }
        }
    }
}

type Fut<'a, T> = Pin<Box<dyn Future<Output = Result<T, Error>> + Send + 'a>>;

impl NostrDatabase for LazyEventsTracker {
    fn backend(&self) -> &'static str {
        "lazy-events-tracker"
    }

    fn features(&self) -> Features {
        Features { persistent: false, event_expiration: false, full_text_search: false, request_to_vanish: false }
    }

    fn save_event<'a>(&'a self, event: &'a Event) -> Fut<'a, SaveEventStatus> {
        Box::pin(async move {
            self.seen.lock().unwrap_or_else(|e| e.into_inner()).insert(event.id);
            Ok(SaveEventStatus::Success)
        })
    }

    fn check_id<'a>(&'a self, event_id: &'a EventId) -> Fut<'a, DatabaseEventStatus> {
        Box::pin(async move {
            let seen = self.seen.lock().unwrap_or_else(|e| e.into_inner()).ids.contains(event_id);
            Ok(if seen { DatabaseEventStatus::Saved } else { DatabaseEventStatus::NotExistent })
        })
    }

    fn event_by_id<'a>(&'a self, _event_id: &'a EventId) -> Fut<'a, Option<Event>> {
        Box::pin(async move { Ok(None) })
    }

    fn count(&self, _filter: Filter) -> Fut<'_, usize> {
        Box::pin(async move { Ok(0) })
    }

    fn query(&self, _filter: Filter) -> Fut<'_, BTreeSet<Event>> {
        Box::pin(async move { Ok(BTreeSet::new()) })
    }

    fn negentropy_items(&self, _filter: Filter) -> Fut<'_, Vec<(EventId, Timestamp)>> {
        Box::pin(async move { Ok(Vec::new()) })
    }

    fn delete(&self, _filter: Filter) -> Fut<'_, ()> {
        Box::pin(async move { Err(Error::unsupported("delete is not supported for the events tracker")) })
    }

    fn wipe(&self) -> Fut<'_, ()> {
        Box::pin(async move {
            *self.seen.lock().unwrap_or_else(|e| e.into_inner()) = Seen::default();
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_forgets_the_oldest_past_its_bound() {
        let mut seen = Seen::default();
        let id = |n: u32| {
            let mut b = [0u8; 32];
            b[..4].copy_from_slice(&n.to_le_bytes());
            EventId::from_byte_array(b)
        };
        for n in 0..(MAX_EVENTS as u32 + 3) {
            seen.insert(id(n));
        }
        seen.insert(id(MAX_EVENTS as u32 + 2));
        assert_eq!(seen.ids.len(), MAX_EVENTS);
        assert_eq!(seen.order.len(), MAX_EVENTS);
        assert!(!seen.ids.contains(&id(2)) && seen.ids.contains(&id(3)));
    }
}
