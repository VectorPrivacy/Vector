//! Per-account RAM state for Community sync: the in-flight page claims. Keyed by this
//! account's channel ids, so it lives on the account's session and goes when that does.

use std::collections::HashSet;
use std::sync::Mutex;

struct InflightKey;

/// Run `f` against this account's in-flight set. A closure, so the lock can never be held
/// across an await; poison-tolerant, so a panicking caller can't wedge every later sync.
fn with_inflight<R>(f: impl FnOnce(&mut HashSet<String>) -> R) -> R {
    let set = crate::db::current_session().scoped::<InflightKey, Mutex<HashSet<String>>>();
    let mut guard = set.lock().unwrap_or_else(|e| e.into_inner());
    f(&mut guard)
}

/// Claim an in-flight page fetch (key `"{channel_id}:{older|latest}"`). Returns `false` if one is
/// already running — the caller should no-op. Pair with [`end_page_fetch`].
pub fn try_begin_page_fetch(key: &str) -> bool {
    with_inflight(|c| c.insert(key.to_string()))
}

/// Release an in-flight page-fetch claim (success or error).
pub fn end_page_fetch(key: &str) {
    with_inflight(|c| c.remove(key));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_page_is_claimed_once_until_released() {
        assert!(try_begin_page_fetch("ch:latest"));
        assert!(!try_begin_page_fetch("ch:latest"));
        assert!(try_begin_page_fetch("ch:older"));
        end_page_fetch("ch:latest");
        assert!(try_begin_page_fetch("ch:latest"));
        end_page_fetch("ch:latest");
        end_page_fetch("ch:older");
    }
}
