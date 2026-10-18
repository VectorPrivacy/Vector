//! Who is in each voice Channel's call (CORD-07 §4): the latest presence per author, live while
//! fresh. Presence is ephemeral and never stored, so this lives on the session alone.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use nostr_sdk::prelude::PublicKey;

/// A `joined` is re-sent this often while in the call.
pub const HEARTBEAT_MS: u64 = 30_000;
/// Three missed heartbeats and a `joined` counts as absent.
pub const FRESH_MS: u64 = 90_000;
/// A presence dated further ahead than this would outlive its author's leave; it's refused.
const AHEAD_MS: u64 = 60_000;

/// One member in a call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Present {
    pub author: PublicKey,
    pub at_ms: u64,
    /// The transport they named; anything but iroh is a call this client can't join them in.
    pub transport: Option<String>,
    /// Their iroh endpoint, when they named one and no one else claims it too.
    pub endpoint: Option<[u8; 32]>,
}

#[derive(Clone)]
struct Entry {
    at_ms: u64,
    joined: bool,
    transport: Option<String>,
    endpoint: Option<[u8; 32]>,
}

struct Calls;

fn calls() -> Arc<Mutex<HashMap<String, HashMap<PublicKey, Entry>>>> {
    crate::db::current_session().scoped::<Calls, _>()
}

/// Fold one presence for `channel` (hex); the latest per author wins. Returns whether it changed
/// who is in the call.
pub fn note(channel: &str, author: PublicKey, at_ms: u64, joined: bool, transport: Option<String>, endpoint: Option<[u8; 32]>, now_ms: u64) -> bool {
    if at_ms > now_ms.saturating_add(AHEAD_MS) {
        return false;
    }
    let store = calls();
    let mut all = store.lock().unwrap_or_else(|e| e.into_inner());
    let room = all.entry(channel.to_ascii_lowercase()).or_default();
    if room.get(&author).is_some_and(|e| e.at_ms >= at_ms) {
        return false;
    }
    let was = room.get(&author).map(|e| (e.joined, e.endpoint, now_ms.saturating_sub(e.at_ms) < FRESH_MS));
    room.insert(author, Entry { at_ms, joined, transport, endpoint });
    was != Some((joined, endpoint, true))
}

/// Who is in `channel`'s call now. An endpoint two authors both claim proves nothing about
/// either, so neither is given one (CORD-07 §4).
pub fn present(channel: &str, now_ms: u64) -> Vec<Present> {
    let store = calls();
    let all = store.lock().unwrap_or_else(|e| e.into_inner());
    let Some(room) = all.get(&channel.to_ascii_lowercase()) else { return Vec::new() };
    let live: Vec<(&PublicKey, &Entry)> = room.iter().filter(|(_, e)| e.joined && now_ms.saturating_sub(e.at_ms) < FRESH_MS).collect();
    let claims = |ep: &[u8; 32]| live.iter().filter(|(_, e)| e.endpoint.as_ref() == Some(ep)).count();
    let mut out: Vec<Present> = live
        .iter()
        .map(|(a, e)| Present { author: **a, at_ms: e.at_ms, transport: e.transport.clone(), endpoint: e.endpoint.filter(|ep| claims(ep) == 1) })
        .collect();
    out.sort_by_key(|p| p.at_ms);
    out
}

/// The member a live presence binds `endpoint` to, if exactly one does.
pub fn author_of(channel: &str, endpoint: &[u8; 32], now_ms: u64) -> Option<PublicKey> {
    present(channel, now_ms).into_iter().find(|p| p.endpoint.as_ref() == Some(endpoint)).map(|p| p.author)
}

/// Every Channel with someone in its call, for a sidebar that lists them.
pub fn occupied(now_ms: u64) -> Vec<String> {
    let store = calls();
    let rooms: Vec<String> = store.lock().unwrap_or_else(|e| e.into_inner()).keys().cloned().collect();
    rooms.into_iter().filter(|r| !present(r, now_ms).is_empty()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostr_sdk::prelude::Keys;

    #[test]
    fn the_latest_presence_per_author_stands_while_fresh() {
        let (ch, a) = ("aa".repeat(32), Keys::generate().public_key());
        let ep = [1u8; 32];
        assert!(note(&ch, a, 10_000, true, Some("iroh".into()), Some(ep), 10_000));
        assert_eq!(present(&ch, 10_000).len(), 1);
        assert!(!note(&ch, a, 9_000, false, None, None, 10_000), "an older left changes nothing");
        assert_eq!(present(&ch, 10_000 + FRESH_MS - 1).len(), 1, "fresh until three heartbeats go missing");
        assert!(present(&ch, 10_000 + FRESH_MS).is_empty(), "then absent");
        assert!(note(&ch, a, 20_000, false, None, None, 20_000));
        assert!(present(&ch, 20_000).is_empty(), "a newer left ends it");
        assert!(!note(&ch, a, 20_000 + AHEAD_MS + 1_000, true, None, None, 20_000), "nor can a joined dated far ahead stick");
    }

    #[test]
    fn an_endpoint_two_authors_claim_is_given_to_neither() {
        let ch = "bb".repeat(32);
        let (a, b, c) = (Keys::generate().public_key(), Keys::generate().public_key(), Keys::generate().public_key());
        note(&ch, a, 1_000, true, Some("iroh".into()), Some([7u8; 32]), 1_000);
        note(&ch, b, 1_001, true, Some("iroh".into()), Some([7u8; 32]), 1_001);
        note(&ch, c, 1_002, true, Some("iroh".into()), Some([8u8; 32]), 1_002);
        let now = present(&ch, 1_002);
        assert_eq!(now.len(), 3, "all three are in the call");
        assert!(now.iter().filter(|p| p.author == a || p.author == b).all(|p| p.endpoint.is_none()), "the contested endpoint dials no one");
        assert_eq!(author_of(&ch, &[8u8; 32], 1_002), Some(c));
        assert_eq!(author_of(&ch, &[7u8; 32], 1_002), None);
    }
}
