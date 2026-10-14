//! Per-account RAM state for Community sync: the in-flight page claims. Keyed by this
//! account's channel ids, so it lives on the account's session and goes when that does.

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

#[cfg(test)]
mod tests {
    use super::*;

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
