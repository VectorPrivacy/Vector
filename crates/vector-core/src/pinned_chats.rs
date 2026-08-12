//! Pinned Chats — the account's favourite conversations, synced across its own
//! devices.
//!
//! A pin is just an id: a DM's npub, or a Community's id (pinning a Community
//! hoists the chat row of its primary channel). The list is ORDERED, and that
//! order is the display order.
//!
//! On the wire: a parameterized-replaceable kind 30078 with the d tag
//! `vector/pinned`, content NIP-44 self-encrypted — the same shape and the same
//! self-sync subscription as the Community and Invite lists, so it inherits boot
//! sync, reconnect re-sync and live cross-device edits without new plumbing.
//!
//! ```text
//!   kind:    30078 (APPLICATION_SPECIFIC)
//!   tags:    ["d", "vector/pinned"]
//!   content: nip44(self, {"v":1,"chats":["npub1…","<community-id-hex>"]})
//! ```
//!
//! **Ids are opaque here.** A pin for a chat this device has not synced yet is
//! carried through every read and republish untouched: dropping "unknown" ids
//! would let the device that knows least erase the others' pins. Nothing
//! resolves an id to a chat in this module — [`is_pinned`] is consulted at
//! render time, so a chat that syncs in later is pinned the moment it paints.

use nostr_sdk::prelude::*;
use serde::{Deserialize, Serialize};

use crate::stored_event::event_kind;

/// The d tag identifying this list among our other kind-30078 self-lists.
pub const PINNED_D_TAG: &str = "vector/pinned";

/// Settings key for the local mirror — the list paints from here at boot,
/// before any relay answers, and keeps working offline.
const LOCAL_PINNED_KEY: &str = "pinned_chats_local";

/// UNIX-seconds of our most recent publish. An arriving copy older than this is
/// our own echo racing a newer local edit, so it must not overwrite it.
const PINNED_PUBLISHED_AT_KEY: &str = "pinned_chats_published_at";

/// Per-effective-tier pin caps (index = `badges::effective_tier()`, 0-3). Tier 3
/// is the Bug Hunter badge's full-premium grade, which unlocks unlimited pins.
///
/// Enforced on WRITE only: a list that already exceeds the cap — a premium
/// account's list read on a free one, or another client's — is read and
/// republished intact rather than truncated. Losing a premium user's pins
/// because they opened a second account would be the worst possible reading.
const PINNED_BY_TIER: [usize; 4] = [3, 6, 9, usize::MAX];

/// Base (free, tier-0) pin cap. Named const for the frontend mirror.
pub const MAX_PINNED: usize = PINNED_BY_TIER[0];

/// Pin cap for the current account, scaled by effective tier. Gate the pin
/// ACTION on this — never the read or render path.
pub fn effective_max_pinned() -> usize {
    PINNED_BY_TIER[crate::badges::effective_tier() as usize]
}

const FETCH_TIMEOUT_SECS: u64 = 10;

/// The synced list. `v` is a forward-compat marker, not a gate: an unknown
/// version still round-trips its ids rather than being discarded.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PinnedChats {
    #[serde(default = "one")]
    pub v: u32,
    /// Ordered ids. Order IS the display order.
    #[serde(default)]
    pub chats: Vec<String>,
}

fn one() -> u32 {
    1
}

impl PinnedChats {
    /// Tolerant parse: a malformed payload degrades to an empty list, never an
    /// error that aborts a sync.
    pub fn from_json(s: &str) -> Self {
        serde_json::from_str(s).unwrap_or_default()
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{\"v\":1,\"chats\":[]}".to_string())
    }

    pub fn contains(&self, id: &str) -> bool {
        self.chats.iter().any(|c| c == id)
    }

    /// Append `id` if absent, up to `max`. `Err` when the cap is already met —
    /// the caller is a user action, so a refusal is a message, not a silent
    /// no-op. `max` is passed rather than read so this stays pure and testable;
    /// callers use [`effective_max_pinned`].
    pub fn pin(&mut self, id: &str, max: usize) -> Result<(), String> {
        if id.trim().is_empty() {
            return Err("cannot pin a chat with no id".to_string());
        }
        if self.contains(id) {
            return Ok(());
        }
        if self.chats.len() >= max {
            return Err(format!("you can pin up to {max} chats — unpin one first"));
        }
        self.chats.push(id.to_string());
        Ok(())
    }

    /// Drop `id`. Absent is success: unpinning something already gone is the
    /// state the caller asked for.
    pub fn unpin(&mut self, id: &str) {
        self.chats.retain(|c| c != id);
    }
}

/// The local mirror. Every read path goes through here so the UI never waits on
/// a relay to know what is pinned.
pub fn load_local() -> PinnedChats {
    crate::db::settings::get_sql_setting(LOCAL_PINNED_KEY.to_string())
        .ok()
        .flatten()
        .map(|s| PinnedChats::from_json(&s))
        .unwrap_or_default()
}

pub fn save_local(list: &PinnedChats) -> Result<(), String> {
    crate::db::settings::set_sql_setting(LOCAL_PINNED_KEY.to_string(), list.to_json())
}

/// Is this chat id pinned? The render-time question — deliberately a lookup
/// against the list rather than a flag stamped onto chats when they sync, so a
/// chat that arrives after its pin is pinned on its first paint.
pub fn is_pinned(id: &str) -> bool {
    load_local().contains(id)
}

/// Pin order for `id`, or `None` when unpinned. Sorts the chat list.
pub fn pin_position(id: &str) -> Option<usize> {
    load_local().chats.iter().position(|c| c == id)
}

fn our_last_publish() -> u64 {
    crate::db::settings::get_sql_setting(PINNED_PUBLISHED_AT_KEY.to_string())
        .ok()
        .flatten()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

fn mark_published() {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let _ = crate::db::settings::set_sql_setting(PINNED_PUBLISHED_AT_KEY.to_string(), now.to_string());
}

async fn decrypt_event(my_pk: &PublicKey, event: &Event) -> PinnedChats {
    if event.content.is_empty() {
        return PinnedChats::default();
    }
    let signer = match crate::signer::active_signer() {
        Ok(s) => s,
        Err(e) => {
            crate::log_warn!("[PinnedChats] signer unavailable for decrypt: {}", e);
            return PinnedChats::default();
        }
    };
    match signer.nip44_decrypt_async(my_pk, &event.content).await {
        Ok(plaintext) => PinnedChats::from_json(&plaintext),
        Err(e) => {
            crate::log_warn!("[PinnedChats] decrypt failed: {}", e);
            PinnedChats::default()
        }
    }
}

/// Fetch the relay copy. A copy older than our last publish loses to the local
/// mirror — otherwise a relay still serving the pre-edit event would undo the
/// pin the user just made.
pub async fn fetch_pinned(client: &Client, my_pk: PublicKey) -> Result<PinnedChats, String> {
    crate::db::scoped(async move {
        let filter = Filter::new()
            .author(my_pk)
            .kind(Kind::Custom(event_kind::APPLICATION_SPECIFIC))
            .identifier(PINNED_D_TAG)
            .limit(1);
        let events = client
            .fetch_events(filter)
            .timeout(std::time::Duration::from_secs(FETCH_TIMEOUT_SECS))
            .await
            .map_err(|e| format!("fetch pinned chats (kind 30078): {}", e))?;

        Ok(match events.into_iter().next() {
            Some(ev) if ev.created_at.as_secs() < our_last_publish() => load_local(),
            Some(ev) => decrypt_event(&my_pk, &ev).await,
            None => load_local(),
        })
    })
    .await
}

/// Persist `list` locally and publish it self-encrypted.
pub async fn publish(client: &Client, list: &PinnedChats) -> Result<(), String> {
    save_local(list)?;
    publish_only(client, list).await
}

/// The network half of [`publish`], for callers that already committed locally.
async fn publish_only(client: &Client, list: &PinnedChats) -> Result<(), String> {
    let my_pk = crate::state::my_public_key().ok_or_else(|| "Not logged in".to_string())?;

    let signer = crate::signer::active_signer().map_err(|e| format!("Signer unavailable: {}", e))?;
    let content = signer
        .nip44_encrypt_async(&my_pk, &list.to_json())
        .await
        .map_err(|e| format!("nip44 encrypt pinned chats: {}", e))?;

    let builder = EventBuilder::new(Kind::Custom(event_kind::APPLICATION_SPECIFIC), content)
        .tag(Tag::identifier(PINNED_D_TAG));
    crate::sign_and_send(client, builder)
        .await
        .map_err(|e| format!("Failed to publish pinned chats (kind 30078): {}", e))?;

    mark_published();
    crate::log_info!("[PinnedChats] published {} pin(s)", list.chats.len());
    Ok(())
}

/// Pin a chat: commit locally and RETURN, then sync in the background.
///
/// A pin is a UI gesture, so it must land at click speed. The local mirror is
/// already current — the self-sync subscription streams sibling-device edits
/// into it — so there is nothing to re-read from a relay first, and the cap
/// check is local anyway. Publishing behind the return keeps a slow or
/// unreachable relay from stalling the list; the next mutation or boot
/// republishes if it failed.
pub async fn pin_chat(client: &Client, id: &str) -> Result<PinnedChats, String> {
    let mut list = load_local();
    list.pin(id, effective_max_pinned())?;
    save_local(&list)?;
    publish_in_background(client, &list);
    Ok(list)
}

/// Unpin a chat. Same commit-then-sync shape as [`pin_chat`].
pub async fn unpin_chat(client: &Client, id: &str) -> Result<PinnedChats, String> {
    let mut list = load_local();
    list.unpin(id);
    save_local(&list)?;
    publish_in_background(client, &list);
    Ok(list)
}

/// Publish off the caller's path. Bound to the account it started under, so a
/// swap mid-publish cannot write this list into the next account's storage.
fn publish_in_background(client: &Client, list: &PinnedChats) {
    let client = client.clone();
    let list = list.clone();
    crate::db::spawn_bound(async move {
        if let Err(e) = publish_only(&client, &list).await {
            crate::log_warn!("[PinnedChats] background publish failed: {e}");
        }
    });
}

/// Consume a remotely-received list event (the live cross-device path). Does
/// NOT republish: the relay echoes our own publishes back on the same
/// subscription, and answering an echo with a publish loops forever.
pub async fn ingest_remote_event(my_pk: &PublicKey, event: &Event) -> Result<PinnedChats, String> {
    crate::db::scoped(async move {
        // Our own newer edit is still in flight to this relay; its older stored
        // copy must not roll the user's pin back.
        if event.created_at.as_secs() < our_last_publish() {
            return Ok(load_local());
        }
        let incoming = decrypt_event(my_pk, event).await;
        save_local(&incoming)?;
        Ok(incoming)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pinning_is_capped_but_reading_over_the_cap_is_not() {
        let mut l = PinnedChats::default();
        for i in 0..MAX_PINNED {
            l.pin(&format!("id{i}"), MAX_PINNED).unwrap();
        }
        let err = l.pin("one-too-many", MAX_PINNED).unwrap_err();
        assert!(err.contains(&MAX_PINNED.to_string()), "the refusal names the cap: {err}");
        assert_eq!(l.chats.len(), MAX_PINNED);

        // A list that arrives OVER the cap is kept whole: truncating here would
        // silently drop a pin made by a build whose cap is higher.
        let over = PinnedChats::from_json("{\"v\":1,\"chats\":[\"a\",\"b\",\"c\",\"d\",\"e\"]}");
        assert_eq!(over.chats.len(), 5, "read tolerates what write refuses");
    }

    #[test]
    fn the_badge_tiers_step_the_cap_up_to_unlimited() {
        // Free pins 3, Bug Hunter grades step 6 -> 9, and the top grade IS
        // full premium: unlimited.
        assert_eq!(PINNED_BY_TIER, [3, 6, 9, usize::MAX]);
        assert_eq!(MAX_PINNED, 3, "the frontend mirror is the free-tier cap");

        // The cap is a parameter, so the premium path is exercised directly
        // rather than by faking a badge.
        let mut premium = PinnedChats::default();
        for i in 0..MAX_PINNED + 5 {
            premium
                .pin(&format!("id{i}"), PINNED_BY_TIER[3])
                .unwrap_or_else(|e| panic!("full premium refused pin {i}: {e}"));
        }
        assert_eq!(premium.chats.len(), MAX_PINNED + 5, "no ceiling at the top tier");

        // Downgrading is not destructive: a free-tier read keeps every pin, and
        // only the next ADD is refused.
        let mut free = PinnedChats::from_json(&premium.to_json());
        assert_eq!(free.chats.len(), MAX_PINNED + 5, "a premium list survives a free-tier read");
        assert!(free.pin("one-more", MAX_PINNED).is_err(), "but adding past the free cap is refused");
        assert_eq!(free.chats.len(), MAX_PINNED + 5, "and the refusal changed nothing");
    }

    #[test]
    fn pinning_is_idempotent_and_unpinning_an_absent_id_is_success() {
        let mut l = PinnedChats::default();
        l.pin("a", MAX_PINNED).unwrap();
        l.pin("a", MAX_PINNED).unwrap();
        assert_eq!(l.chats, vec!["a".to_string()], "a second pin is not a second entry");
        l.unpin("never-pinned");
        l.unpin("a");
        assert!(l.chats.is_empty());
    }

    #[test]
    fn order_is_preserved_across_a_round_trip() {
        let mut l = PinnedChats::default();
        l.pin("first", MAX_PINNED).unwrap();
        l.pin("second", MAX_PINNED).unwrap();
        let back = PinnedChats::from_json(&l.to_json());
        assert_eq!(back.chats, vec!["first".to_string(), "second".to_string()], "order IS the display order");
    }

    #[test]
    fn an_unknown_id_survives_a_parse_and_republish() {
        // The whole point of opaque ids: a device that has never synced the chat
        // behind "stranger" must still carry its pin forward.
        let json = "{\"v\":1,\"chats\":[\"stranger\"]}";
        let mut l = PinnedChats::from_json(json);
        l.pin("mine", usize::MAX).unwrap();
        let out = PinnedChats::from_json(&l.to_json());
        assert!(out.contains("stranger"), "an id we cannot resolve is never dropped");
        assert!(out.contains("mine"));
    }

    #[test]
    fn a_malformed_or_future_payload_never_errors() {
        assert!(PinnedChats::from_json("not json").chats.is_empty());
        assert!(PinnedChats::from_json("{}").chats.is_empty());
        // An unknown version still yields its ids rather than being discarded.
        let future = PinnedChats::from_json("{\"v\":99,\"chats\":[\"a\"],\"unknown\":true}");
        assert_eq!(future.chats, vec!["a".to_string()]);
    }
}
