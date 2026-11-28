//! `profilewatch.js`: the people drawn on screen, reported in batches so the backend refreshes
//! the profiles whose last check has gone stale. A person is reported again only after a while,
//! so scrolling back and forth doesn't re-ask; the backend dedupes too.

use std::cell::RefCell;
use std::collections::HashMap;
use std::time::{Duration, Instant};

const RESEND: Duration = Duration::from_secs(10 * 60);

#[derive(Default)]
struct Watch {
    sent: HashMap<String, Instant>,
    /// In the order drawn: top of the screen first.
    pending: Vec<String>,
}

/// The most one batch reports; the rest wait for the next.
const BATCH: usize = 200;

thread_local! {
    static WATCH: RefCell<Watch> = RefCell::default();
}

/// A person drawn this frame.
pub fn seen(npub: &str) {
    if !npub.starts_with("npub1") {
        return;
    }
    WATCH.with(|w| {
        let mut w = w.borrow_mut();
        if w.sent.get(npub).is_some_and(|at| at.elapsed() < RESEND) {
            return;
        }
        w.sent.insert(npub.to_string(), Instant::now());
        w.pending.push(npub.to_string());
    });
}

pub fn has_pending() -> bool {
    WATCH.with(|w| !w.borrow().pending.is_empty())
}

/// The people seen since the last batch, first drawn first.
pub fn take() -> Vec<String> {
    WATCH.with(|w| {
        let mut w = w.borrow_mut();
        let n = w.pending.len().min(BATCH);
        w.pending.drain(..n).collect()
    })
}
