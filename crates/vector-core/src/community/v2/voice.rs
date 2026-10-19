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

/// A moderator's directive in a call (CORD-07 §7).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Directive {
    Mute,
    Unmute,
    Deafen,
    Undeafen,
    Move,
    Disconnect,
}

impl Directive {
    pub fn as_str(self) -> &'static str {
        match self {
            Directive::Mute => "mute",
            Directive::Unmute => "unmute",
            Directive::Deafen => "deafen",
            Directive::Undeafen => "undeafen",
            Directive::Move => "move",
            Directive::Disconnect => "disconnect",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "mute" => Directive::Mute,
            "unmute" => Directive::Unmute,
            "deafen" => Directive::Deafen,
            "undeafen" => Directive::Undeafen,
            "move" => Directive::Move,
            "disconnect" => Directive::Disconnect,
            _ => return None,
        })
    }

    /// The bit its signer holds over the target.
    pub fn permission(self) -> u64 {
        use crate::community::roles::Permissions;
        match self {
            Directive::Mute | Directive::Unmute => Permissions::MUTE_MEMBERS,
            Directive::Deafen | Directive::Undeafen => Permissions::DEAFEN_MEMBERS,
            Directive::Move | Directive::Disconnect => Permissions::MOVE_MEMBERS,
        }
    }
}

/// What one presence says.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Heard {
    pub joined: bool,
    pub transport: Option<String>,
    pub endpoint: Option<[u8; 32]>,
    pub relay: Option<String>,
    pub muted: bool,
    pub deafened: bool,
}

/// One member in a call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Present {
    pub author: PublicKey,
    pub at_ms: u64,
    /// The transport they named; anything but iroh is a call this client can't join them in.
    pub transport: Option<String>,
    /// Their iroh endpoint, when they named one and no one else claims it too.
    pub endpoint: Option<[u8; 32]>,
    /// The relay that endpoint is reached through.
    pub relay: Option<String>,
    pub muted: bool,
    pub deafened: bool,
    /// A moderator's mute or deafen, for as long as they stay in the call.
    pub server_muted: bool,
    pub server_deafened: bool,
}

#[derive(Clone)]
struct Entry {
    at_ms: u64,
    heard: Heard,
    server_muted: bool,
    server_deafened: bool,
}

/// What folding a presence did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Noted {
    /// Who is in the call, or how, changed.
    pub changed: bool,
    /// They are reachable anew: in from outside the call, or back on a new endpoint. Who is
    /// already in it tells them so, and any standing moderation comes back with them.
    pub reached: bool,
}

struct Calls;

fn calls() -> Arc<Mutex<HashMap<String, HashMap<PublicKey, Entry>>>> {
    crate::db::current_session().scoped::<Calls, _>()
}

/// Fold one presence for `channel` (hex); the latest per author wins. Returns whether it changed
/// who is in the call.
pub fn note(channel: &str, author: PublicKey, at_ms: u64, heard: Heard, now_ms: u64) -> Noted {
    if at_ms > now_ms.saturating_add(AHEAD_MS) {
        return Noted::default();
    }
    let store = calls();
    let mut all = store.lock().unwrap_or_else(|e| e.into_inner());
    let room = all.entry(channel.to_ascii_lowercase()).or_default();
    if room.get(&author).is_some_and(|e| e.at_ms >= at_ms) {
        return Noted::default();
    }
    let fresh = |e: &Entry| now_ms.saturating_sub(e.at_ms) < FRESH_MS && e.heard.joined;
    let was = room.get(&author).filter(|e| fresh(e)).cloned();
    let reached = heard.joined && was.as_ref().is_none_or(|e| e.heard.endpoint != heard.endpoint);
    let changed = was.as_ref().map(|e| &e.heard) != Some(&heard);
    // A moderator's mute lasts while they stay: leaving or arriving anew clears it.
    let (server_muted, server_deafened) = match &was {
        Some(e) if heard.joined => (e.server_muted, e.server_deafened),
        _ => (false, false),
    };
    room.insert(author, Entry { at_ms, heard, server_muted, server_deafened });
    Noted { changed, reached }
}

/// Apply a moderator's mute or deafen to `target`, if they're in the call. Returns whether it changed.
pub fn apply_directive(channel: &str, target: &PublicKey, directive: Directive, now_ms: u64) -> bool {
    let store = calls();
    let mut all = store.lock().unwrap_or_else(|e| e.into_inner());
    let Some(e) = all.get_mut(&channel.to_ascii_lowercase()).and_then(|r| r.get_mut(target)) else { return false };
    if !e.heard.joined || now_ms.saturating_sub(e.at_ms) >= FRESH_MS {
        return false;
    }
    let before = (e.server_muted, e.server_deafened);
    match directive {
        Directive::Mute => e.server_muted = true,
        Directive::Unmute => e.server_muted = false,
        Directive::Deafen => e.server_deafened = true,
        Directive::Undeafen => e.server_deafened = false,
        Directive::Move | Directive::Disconnect => {}
    }
    before != (e.server_muted, e.server_deafened)
}

/// Whether a moderator has muted `member` in `channel`'s call: (muted, deafened).
pub fn server_state(channel: &str, member: &PublicKey) -> (bool, bool) {
    let store = calls();
    let all = store.lock().unwrap_or_else(|e| e.into_inner());
    all.get(&channel.to_ascii_lowercase()).and_then(|r| r.get(member)).map_or((false, false), |e| (e.server_muted, e.server_deafened))
}

/// Who is in `channel`'s call now. An endpoint two authors both claim proves nothing about
/// either, so neither is given one (CORD-07 §4).
pub fn present(channel: &str, now_ms: u64) -> Vec<Present> {
    let store = calls();
    let all = store.lock().unwrap_or_else(|e| e.into_inner());
    let Some(room) = all.get(&channel.to_ascii_lowercase()) else { return Vec::new() };
    let live: Vec<(&PublicKey, &Entry)> = room.iter().filter(|(_, e)| e.heard.joined && now_ms.saturating_sub(e.at_ms) < FRESH_MS).collect();
    let claims = |ep: &[u8; 32]| live.iter().filter(|(_, e)| e.heard.endpoint.as_ref() == Some(ep)).count();
    let mut out: Vec<Present> = live
        .iter()
        .map(|(a, e)| {
            let h = &e.heard;
            let endpoint = h.endpoint.filter(|ep| claims(ep) == 1);
            Present {
                author: **a,
                at_ms: e.at_ms,
                transport: h.transport.clone(),
                endpoint,
                relay: endpoint.and(h.relay.clone()),
                muted: h.muted,
                deafened: h.deafened,
                server_muted: e.server_muted,
                server_deafened: e.server_deafened,
            }
        })
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
        // The store is the session\'s: a bed test swapping sessions must not run beside this.
        let _db = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let (ch, a) = ("aa".repeat(32), Keys::generate().public_key());
        let ep = [1u8; 32];
        assert!(note(&ch, a, 10_000, Heard { joined: true, transport: Some("iroh".into()), endpoint: Some(ep), ..Default::default() }, 10_000).reached);
        assert_eq!(present(&ch, 10_000).len(), 1);
        assert!(!note(&ch, a, 9_000, Heard { joined: false, transport: None, endpoint: None, ..Default::default() }, 10_000).changed, "an older left changes nothing");
        assert_eq!(present(&ch, 10_000 + FRESH_MS - 1).len(), 1, "fresh until three heartbeats go missing");
        assert!(present(&ch, 10_000 + FRESH_MS).is_empty(), "then absent");
        let muted = Heard { joined: true, transport: Some("iroh".into()), endpoint: Some(ep), muted: true, ..Default::default() };
        let n = note(&ch, a, 15_000, muted.clone(), 15_000);
        assert!(n.changed && !n.reached, "a mute alone is news, not an arrival");
        assert!(present(&ch, 15_000)[0].muted);
        assert!(!note(&ch, a, 16_000, muted, 16_000).changed, "a heartbeat that says the same is not");
        assert!(note(&ch, a, 20_000, Heard { joined: false, transport: None, endpoint: None, ..Default::default() }, 20_000).changed);
        assert!(present(&ch, 20_000).is_empty(), "a newer left ends it");
        assert!(!note(&ch, a, 20_000 + AHEAD_MS + 1_000, Heard { joined: true, transport: None, endpoint: None, ..Default::default() }, 20_000).changed, "nor can a joined dated far ahead stick");
    }

    #[test]
    fn a_moderators_mute_lasts_while_they_stay_and_ends_when_they_leave() {
        // The store is the session\'s: a bed test swapping sessions must not run beside this.
        let _db = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let (ch, a) = ("cc".repeat(32), Keys::generate().public_key());
        let joined = |_at: u64| Heard { joined: true, transport: Some("iroh".into()), endpoint: Some([3u8; 32]), ..Default::default() };
        assert!(!apply_directive(&ch, &a, Directive::Mute, 1_000), "nobody to mute yet");
        note(&ch, a, 1_000, joined(1_000), 1_000);
        assert!(apply_directive(&ch, &a, Directive::Mute, 1_000));
        assert!(!apply_directive(&ch, &a, Directive::Mute, 1_000), "once");
        note(&ch, a, 31_000, joined(31_000), 31_000);
        assert_eq!(server_state(&ch, &a), (true, false), "a heartbeat keeps it");
        assert!(present(&ch, 31_000)[0].server_muted);
        let back = Heard { joined: true, transport: Some("iroh".into()), endpoint: Some([4u8; 32]), ..Default::default() };
        assert!(note(&ch, a, 32_000, back, 32_000).reached, "back on a new endpoint: reachable anew, to be told who is here");
        assert_eq!(server_state(&ch, &a), (true, false), "and still muted: they never left");
        note(&ch, a, 40_000, Heard::default(), 40_000);
        note(&ch, a, 41_000, joined(41_000), 41_000);
        assert_eq!(server_state(&ch, &a), (false, false), "leaving ends it: a moderator re-asserts on return");
        assert!(apply_directive(&ch, &a, Directive::Deafen, 41_000) && apply_directive(&ch, &a, Directive::Undeafen, 41_000));
    }

    #[test]
    fn an_endpoint_two_authors_claim_is_given_to_neither() {
        // The store is the session\'s: a bed test swapping sessions must not run beside this.
        let _db = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let ch = "bb".repeat(32);
        let (a, b, c) = (Keys::generate().public_key(), Keys::generate().public_key(), Keys::generate().public_key());
        note(&ch, a, 1_000, Heard { joined: true, transport: Some("iroh".into()), endpoint: Some([7u8; 32]), ..Default::default() }, 1_000);
        note(&ch, b, 1_001, Heard { joined: true, transport: Some("iroh".into()), endpoint: Some([7u8; 32]), ..Default::default() }, 1_001);
        note(&ch, c, 1_002, Heard { joined: true, transport: Some("iroh".into()), endpoint: Some([8u8; 32]), ..Default::default() }, 1_002);
        let now = present(&ch, 1_002);
        assert_eq!(now.len(), 3, "all three are in the call");
        assert!(now.iter().filter(|p| p.author == a || p.author == b).all(|p| p.endpoint.is_none()), "the contested endpoint dials no one");
        assert_eq!(author_of(&ch, &[8u8; 32], 1_002), Some(c));
        assert_eq!(author_of(&ch, &[7u8; 32], 1_002), None);
    }
}
