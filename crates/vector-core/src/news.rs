//! What a listening client is told as news: everything sent since it began listening, once,
//! however it reached this device (live, a catch-up, a replay). What came before is history:
//! stored, never announced.
//!
//! Once is anchored on the store: an event is news only the first time this device keeps it,
//! so the paths that can carry it (the live feed, every catch-up, a replay) race harmlessly.

use std::sync::{Arc, Mutex};

use crate::event_handler::InboundEventHandler;

/// How long before listening began an event still counts: senders' clocks drift, and what was
/// sent while the client started reached nobody else.
pub const ALLOWANCE_MS: u64 = crate::community::REALTIME_FRESH_WINDOW_MS;

struct Listening {
    session: u64,
    since_ms: u64,
    handler: Arc<dyn InboundEventHandler>,
}

// Keyed by session id rather than held on the Session: two Session objects can share an id,
// and a catch-up running on either must find the listener.
static LISTENING: Mutex<Option<Listening>> = Mutex::new(None);

/// `handler` hears what is sent from `since_ms` (unix ms) on, for the live session.
pub(crate) fn begin(handler: Arc<dyn InboundEventHandler>, since_ms: u64) {
    let session = crate::db::current_session_id();
    *LISTENING.lock().unwrap_or_else(|e| e.into_inner()) = Some(Listening { session, since_ms, handler });
}

fn current<T>(f: impl FnOnce(&Listening) -> T) -> Option<T> {
    let held = LISTENING.lock().unwrap_or_else(|e| e.into_inner());
    held.as_ref().filter(|l| l.session == crate::db::current_session_id()).map(f)
}

/// This session's listener ends: catch-ups stop telling it anything.
pub(crate) fn end() {
    let mut held = LISTENING.lock().unwrap_or_else(|e| e.into_inner());
    if held.as_ref().is_some_and(|l| l.session == crate::db::current_session_id()) {
        *held = None;
    }
    *TOLD.lock().unwrap_or_else(|e| e.into_inner()) = Told::default();
}

/// What a listener has been told, by event id: the newest [`TOLD_GEN`] to two generations of the
/// ids' first 8 bytes. Two paths that store the same event a moment apart (overlapping catch-ups,
/// a page and the live feed) can each think they stored it first.
#[derive(Default)]
struct Told {
    session: u64,
    cur: std::collections::HashSet<u64>,
    prev: std::collections::HashSet<u64>,
}

const TOLD_GEN: usize = 32_768;

static TOLD: std::sync::LazyLock<Mutex<Told>> = std::sync::LazyLock::new(Mutex::default);

/// Whether to tell this session's listener about `id` (an event id, or any 32 bytes naming one
/// thing that happened): true the first time, and always when nobody is listening.
pub fn first_telling(id: &[u8; 32]) -> bool {
    let session = crate::db::current_session_id();
    if listening_since().is_none() {
        return true;
    }
    let key = u64::from_le_bytes(id[..8].try_into().expect("8 bytes"));
    let mut told = TOLD.lock().unwrap_or_else(|e| e.into_inner());
    if told.session != session {
        *told = Told { session, ..Told::default() };
    }
    if told.cur.contains(&key) || told.prev.contains(&key) {
        return false;
    }
    if told.cur.len() >= TOLD_GEN {
        let Told { cur, prev, .. } = &mut *told;
        std::mem::swap(cur, prev);
        cur.clear();
    }
    told.cur.insert(key);
    true
}

/// [`first_telling`] for a hex id; anything else names itself through a hash.
pub fn first_telling_hex(id: &str) -> bool {
    match crate::simd::hex::hex_to_bytes_32_checked(id) {
        Some(bytes) => first_telling(&bytes),
        None => {
            use sha2::Digest;
            first_telling(&sha2::Sha256::digest(id.as_bytes()).into())
        }
    }
}

/// The handler listening for this session, if one is.
pub(crate) fn listener() -> Option<Arc<dyn InboundEventHandler>> {
    current(|l| l.handler.clone())
}

/// When this session's listener began (unix ms), if one is listening.
pub fn listening_since() -> Option<u64> {
    current(|l| l.since_ms)
}

/// The earliest send time (unix ms) still news to this session's listener.
pub fn cutoff() -> Option<u64> {
    listening_since().map(|since| since.saturating_sub(ALLOWANCE_MS))
}

/// Whether something sent at `at_ms` is news. With a listener: sent since it began. Without one
/// (a UI paints history anyway), only what is fresh now.
pub fn is_news(at_ms: u64) -> bool {
    match cutoff() {
        Some(from) => at_ms >= from,
        None => crate::community::is_realtime_fresh(at_ms),
    }
}

/// A listener for a test's length: it ends when this drops.
#[cfg(test)]
pub(crate) struct TestListener;

#[cfg(test)]
pub(crate) fn listen_for_test(handler: Arc<dyn InboundEventHandler>, since_ms: u64) -> TestListener {
    begin(handler, since_ms);
    TestListener
}

#[cfg(test)]
impl Drop for TestListener {
    fn drop(&mut self) {
        *LISTENING.lock().unwrap_or_else(|e| e.into_inner()) = None;
        *TOLD.lock().unwrap_or_else(|e| e.into_inner()) = Told::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_listener_hears_what_was_sent_since_it_began_however_late_it_arrives() {
        let _g = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let now = crate::clock::now_ms();
        *LISTENING.lock().unwrap() = None;
        assert!(!is_news(now - 3_600_000), "an hour old, nobody listening: history");
        assert!(is_news(now), "fresh, nobody listening: news");

        begin(Arc::new(crate::event_handler::NoOpEventHandler), now - 3_600_000);
        assert!(is_news(now - 3_000_000), "sent after listening began, arriving 50 minutes late");
        assert!(is_news(now - 3_600_000 - ALLOWANCE_MS + 1), "within the allowance before it");
        assert!(!is_news(now - 3_600_000 - ALLOWANCE_MS - 1), "before it: history");
        end();
        assert!(listening_since().is_none());
    }

    #[test]
    fn a_listener_is_told_each_thing_once() {
        let _g = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        assert!(first_telling(&[1; 32]) && first_telling(&[1; 32]), "nobody listening: nothing to remember");
        begin(Arc::new(crate::event_handler::NoOpEventHandler), crate::clock::now_ms());
        assert!(first_telling(&[2; 32]));
        assert!(!first_telling(&[2; 32]), "a second path carrying it says nothing");
        assert!(first_telling_hex("not hex") && !first_telling_hex("not hex"));
        end();
    }
}
