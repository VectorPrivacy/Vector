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

/// Ids are held by their first 8 bytes. They are SHA-256, so a false "seen" is about 2e-15
/// per event and costs one live delivery, which negentropy recovers.
#[derive(Debug, Default)]
struct Seen {
    ids: HashSet<u64>,
    /// `ids` oldest first. Grown by hand to exactly the bound: doubling would end near 2x it.
    order: VecDeque<u64>,
}

fn short(id: &EventId) -> u64 {
    let b = id.as_bytes();
    u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]])
}

impl Seen {
    fn contains(&self, id: &EventId) -> bool {
        self.ids.contains(&short(id))
    }

    /// The id forgotten is nearly always one just marked, so it is sought from the newest end.
    fn remove(&mut self, id: &EventId) {
        let key = short(id);
        if self.ids.remove(&key) {
            if let Some(at) = self.order.iter().rposition(|&o| o == key) {
                self.order.remove(at);
            }
        }
    }

    fn insert(&mut self, id: EventId) {
        let key = short(&id);
        if !self.ids.insert(key) {
            return;
        }
        if self.order.len() == MAX_EVENTS {
            if let Some(old) = self.order.pop_front() {
                self.ids.remove(&old);
            }
        } else if self.order.len() == self.order.capacity() {
            let grow = self.order.capacity().max(64).min(MAX_EVENTS - self.order.len());
            self.order.reserve_exact(grow);
        }
        self.order.push_back(key);
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
            let seen = self.seen.lock().unwrap_or_else(|e| e.into_inner()).contains(event_id);
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

    /// Forget events by id, so a later copy of one counts as new. Only by id: the tracker
    /// keeps nothing else to match a filter against.
    fn delete(&self, filter: Filter) -> Fut<'_, ()> {
        Box::pin(async move {
            let Some(ids) = filter.ids else {
                return Err(Error::unsupported("the events tracker forgets by id only"));
            };
            let mut seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
            for id in ids {
                seen.remove(&id);
            }
            Ok(())
        })
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

    fn id(n: u32) -> EventId {
        let mut b = [0u8; 32];
        b[..4].copy_from_slice(&n.to_le_bytes());
        EventId::from_byte_array(b)
    }

    #[test]
    fn it_forgets_the_oldest_past_its_bound() {
        let mut seen = Seen::default();
        for n in 0..(MAX_EVENTS as u32 + 3) {
            seen.insert(id(n));
        }
        seen.insert(id(MAX_EVENTS as u32 + 2));
        assert_eq!(seen.ids.len(), MAX_EVENTS);
        assert_eq!(seen.order.len(), MAX_EVENTS);
        assert!(!seen.contains(&id(2)) && seen.contains(&id(3)));
        assert!(seen.contains(&id(MAX_EVENTS as u32 + 2)), "the newest stays");
        assert_eq!(seen.order.capacity(), MAX_EVENTS, "the ring never grows past its bound");
    }

    #[test]
    fn it_evicts_in_the_order_ids_arrived() {
        let mut seen = Seen::default();
        for n in 0..MAX_EVENTS as u32 {
            seen.insert(id(n));
        }
        seen.insert(id(0));
        for k in 0..5u32 {
            seen.insert(id(1_000_000 + k));
            assert!(!seen.contains(&id(k)), "id {k} was the oldest");
            assert!(seen.contains(&id(k + 1)), "id {} is not yet", k + 1);
        }
    }

    #[test]
    fn a_forgotten_id_frees_its_place_in_the_order() {
        let mut seen = Seen::default();
        for n in 0..10 {
            seen.insert(id(n));
        }
        seen.remove(&id(4));
        seen.remove(&id(9));
        seen.remove(&id(77));
        assert_eq!(seen.ids.len(), 8);
        assert_eq!(seen.order.len(), 8);
        assert!(!seen.contains(&id(4)) && !seen.contains(&id(9)) && seen.contains(&id(5)));
        seen.insert(id(4));
        assert_eq!(seen.order.back(), Some(&short(&id(4))), "a returning id is the newest");
    }

    #[test]
    fn a_saturated_tracker_holds_under_a_quarter_of_the_old_bytes() {
        let mut seen = Seen::default();
        for n in 0..MAX_EVENTS as u32 {
            seen.insert(id(n));
        }
        // hashbrown keeps 7/8 of its buckets usable and one control byte per bucket.
        let set = (seen.ids.capacity() * 8 / 7).next_power_of_two() * 9;
        let ring = seen.order.capacity() * 8;
        // Full 32-byte ids at the same cap: 65,536 buckets and a doubled ring.
        let full_ids = 65_536 * 33 + 65_536 * 32;
        assert!((set + ring) * 4 < full_ids, "set {set} B + ring {ring} B vs {full_ids} B");
    }

    #[tokio::test]
    async fn a_forgotten_event_counts_as_new_again() {
        use nostr_sdk::prelude::*;
        let tracker = LazyEventsTracker::default();
        let ev = EventBuilder::new(Kind::TextNote, "mine").finalize(&Keys::generate()).unwrap();
        tracker.save_event(&ev).await.unwrap();
        assert_eq!(tracker.check_id(&ev.id).await.unwrap(), DatabaseEventStatus::Saved);
        tracker.delete(Filter::new().id(ev.id)).await.unwrap();
        assert_eq!(tracker.check_id(&ev.id).await.unwrap(), DatabaseEventStatus::NotExistent);
        assert!(tracker.seen.lock().unwrap().order.is_empty(), "no stale entry left to evict it early later");
        assert!(tracker.delete(Filter::new().kind(Kind::TextNote)).await.is_err(), "only ids can be forgotten");
    }
}
