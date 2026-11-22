//! Per-account RAM state for Community sync: the in-flight page claims and the foreground gate.
//! Keyed by this account's channel ids, so it lives on the account's session and goes when that
//! does.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

struct InflightKey;

/// A channel's page direction: `true` for an older page, `false` for the latest.
type PageKey = ([u8; 32], bool);
type Inflight = Mutex<HashSet<PageKey>>;

/// One running page fetch, released when dropped — on return or panic alike, so a
/// panicking fetch can't wedge every later sync of its channel. Releases into the set
/// it claimed from, whichever account is live by then.
pub struct PageFetchClaim {
    set: Arc<Inflight>,
    key: Option<PageKey>,
}

impl Drop for PageFetchClaim {
    fn drop(&mut self) {
        if let Some(key) = self.key {
            self.set.lock().unwrap_or_else(|e| e.into_inner()).remove(&key);
        }
    }
}

/// Claim the fetch of one channel page. `None` if one is already running — the caller
/// should no-op. A channel id that is not canonical hex names no channel; its claim
/// always succeeds and guards nothing.
pub fn claim_page_fetch(channel_id: &str, older: bool) -> Option<PageFetchClaim> {
    let set = crate::db::current_session().scoped::<InflightKey, Inflight>();
    let Some(channel) = super::id32_from_hex(channel_id) else {
        return Some(PageFetchClaim { set, key: None });
    };
    let key = (channel, older);
    if !set.lock().unwrap_or_else(|e| e.into_inner()).insert(key) {
        return None;
    }
    Some(PageFetchClaim { set, key: Some(key) })
}

struct ForegroundKey;

#[derive(Default)]
struct ForegroundGate {
    running: std::sync::atomic::AtomicUsize,
    done: tokio::sync::Notify,
}

fn foreground_gate() -> Arc<ForegroundGate> {
    crate::db::current_session().scoped::<ForegroundKey, ForegroundGate>()
}

/// A fetch the reader is waiting on (the newest page of the channel on screen): background
/// channel walks hold off until it ends. Released when dropped.
pub struct Foreground(Arc<ForegroundGate>);

impl Drop for Foreground {
    fn drop(&mut self) {
        if self.0.running.fetch_sub(1, std::sync::atomic::Ordering::AcqRel) == 1 {
            self.0.done.notify_waiters();
        }
    }
}

/// Marks a foreground fetch running until the guard drops.
pub fn foreground() -> Foreground {
    let gate = foreground_gate();
    gate.running.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    Foreground(gate)
}

/// The longest a background walk waits on foreground fetches: one that hangs can't stall the rest.
const FOREGROUND_WAIT: std::time::Duration = std::time::Duration::from_secs(15);

/// Waits, before a background channel walk, until no foreground fetch is running.
pub async fn after_foreground() {
    let gate = foreground_gate();
    let _ = crate::rt::time::timeout(FOREGROUND_WAIT, async {
        loop {
            let done = gate.done.notified();
            if gate.running.load(std::sync::atomic::Ordering::Acquire) == 0 {
                return;
            }
            done.await;
        }
    })
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_background_walk_waits_out_the_foreground_fetch() {
        crate::db::with_session(crate::db::Session::empty(), async {
            after_foreground().await;
            let fg = foreground();
            let second = foreground();
            let waiting = crate::db::spawn_bound(async {
                let at = std::time::Instant::now();
                after_foreground().await;
                at.elapsed()
            });
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            drop(fg);
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            assert!(!waiting.is_finished(), "one foreground fetch still runs");
            drop(second);
            let waited = waiting.await.unwrap();
            assert!(waited >= std::time::Duration::from_millis(100) && waited < FOREGROUND_WAIT);
        })
        .await;
    }

    #[tokio::test]
    async fn a_page_is_claimed_once_until_released() {
        // A private session: the live one is swapped by tests running alongside.
        crate::db::with_session(crate::db::Session::empty(), async {
            let ch = "ab".repeat(32);
            let latest = claim_page_fetch(&ch, false).expect("first claim");
            assert!(claim_page_fetch(&ch, false).is_none(), "a running fetch refuses a second");
            let older = claim_page_fetch(&ch, true).expect("the other direction is its own claim");
            drop(latest);
            let again = claim_page_fetch(&ch, false).expect("released on drop");
            drop((again, older));
            assert!(claim_page_fetch(&ch, true).is_some(), "both released");
        })
        .await;
    }
}
