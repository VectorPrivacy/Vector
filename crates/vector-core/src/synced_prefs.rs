//! Account preferences that follow you between your own devices: the block
//! list, the mute list, and nicknames.
//!
//! Each is a private, parameterized-replaceable kind 30078 with its own d tag,
//! NIP-44 self-encrypted, riding the SAME self-sync subscription as the
//! Community, Invite and Pinned lists — so each inherits boot sync, reconnect
//! re-sync and live cross-device edits with no new plumbing.
//!
//! **Vector's own lists, deliberately not NIP-51.** Social clients disagree on
//! what a mute is — several treat it as a soft block — so round-tripping
//! through the shared kind-10000 would blur the mute/block separation Vector
//! draws on purpose. Isolation costs interop and buys exactness.
//!
//! **Newest wins, whole list.** These are one person's settings edited from one
//! device at a time, so the replaceable event's own last-write-wins is the
//! merge. A device that was offline can therefore republish over a change it
//! never saw; the pre-publish fetch below shrinks that window, and these are
//! deliberate, infrequent actions rather than latency-sensitive ones.

use nostr_sdk::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::stored_event::event_kind;

pub const BLOCKS_D_TAG: &str = "vector/blocks";
pub const MUTES_D_TAG: &str = "vector/mutes";
pub const NICKNAMES_D_TAG: &str = "vector/nicknames";

const BLOCKS_LOCAL_KEY: &str = "synced_blocks_local";
const MUTES_LOCAL_KEY: &str = "synced_mutes_local";
const NICKNAMES_LOCAL_KEY: &str = "synced_nicknames_local";

/// One NIP-44 event holds the whole list, so it inherits the same ~65KB
/// plaintext ceiling as the Community List. Blocks and nicknames scale with
/// contacts rather than being capped like pins, so the write path refuses to
/// grow a list past this rather than publishing something no reader can open.
const MAX_ENTRIES: usize = 2048;

const FETCH_TIMEOUT_SECS: u64 = 10;

/// A set of ids (npubs for blocks, chat ids for mutes).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdList {
    #[serde(default = "one")]
    pub v: u32,
    #[serde(default)]
    pub ids: Vec<String>,
}

/// npub → nickname. A map rather than a list so a rename replaces rather than
/// duplicates, and so the wire form stays stable under reordering.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NicknameMap {
    #[serde(default = "one")]
    pub v: u32,
    #[serde(default)]
    pub names: BTreeMap<String, String>,
}

fn one() -> u32 {
    1
}

impl IdList {
    /// Tolerant parse: a malformed payload degrades to empty rather than
    /// erroring, so one bad event can never wedge a sync.
    pub fn from_json(s: &str) -> Self {
        serde_json::from_str(s).unwrap_or_default()
    }
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{\"v\":1,\"ids\":[]}".to_string())
    }
    pub fn contains(&self, id: &str) -> bool {
        self.ids.iter().any(|i| i == id)
    }
    pub fn add(&mut self, id: &str) -> Result<(), String> {
        if id.trim().is_empty() {
            return Err("empty id".to_string());
        }
        if self.contains(id) {
            return Ok(());
        }
        if self.ids.len() >= MAX_ENTRIES {
            return Err(format!("this list is full ({MAX_ENTRIES} entries)"));
        }
        self.ids.push(id.to_string());
        Ok(())
    }
    pub fn remove(&mut self, id: &str) {
        self.ids.retain(|i| i != id);
    }
}

impl NicknameMap {
    pub fn from_json(s: &str) -> Self {
        serde_json::from_str(s).unwrap_or_default()
    }
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{\"v\":1,\"names\":{}}".to_string())
    }
    /// An empty nickname CLEARS the entry — that is how the UI expresses
    /// "remove this nickname", and keeping a blank would republish it forever.
    pub fn set(&mut self, npub: &str, name: &str) -> Result<(), String> {
        if npub.trim().is_empty() {
            return Err("empty npub".to_string());
        }
        if name.trim().is_empty() {
            self.names.remove(npub);
            return Ok(());
        }
        if !self.names.contains_key(npub) && self.names.len() >= MAX_ENTRIES {
            return Err(format!("nickname list is full ({MAX_ENTRIES} entries)"));
        }
        self.names.insert(npub.to_string(), name.to_string());
        Ok(())
    }
}

/// Which list a call refers to. Keeps one set of network/storage plumbing for
/// all three rather than three near-identical copies that can drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pref {
    Blocks,
    Mutes,
    Nicknames,
}

impl Pref {
    pub fn d_tag(self) -> &'static str {
        match self {
            Pref::Blocks => BLOCKS_D_TAG,
            Pref::Mutes => MUTES_D_TAG,
            Pref::Nicknames => NICKNAMES_D_TAG,
        }
    }
    fn local_key(self) -> &'static str {
        match self {
            Pref::Blocks => BLOCKS_LOCAL_KEY,
            Pref::Mutes => MUTES_LOCAL_KEY,
            Pref::Nicknames => NICKNAMES_LOCAL_KEY,
        }
    }
    /// The d-tag → list routing used by the self-sync handler.
    pub fn from_d_tag(d: &str) -> Option<Self> {
        match d {
            BLOCKS_D_TAG => Some(Pref::Blocks),
            MUTES_D_TAG => Some(Pref::Mutes),
            NICKNAMES_D_TAG => Some(Pref::Nicknames),
            _ => None,
        }
    }
}

/// Lists this account has reconciled with the relays this session, keyed by
/// d-tag. Per-account by construction: it lives on the Session, so a swap drops
/// it and the next account re-hydrates rather than inheriting this one's.
struct Hydrated;

fn hydrated_set() -> std::sync::Arc<std::sync::Mutex<std::collections::HashSet<&'static str>>> {
    crate::db::current_session().scoped::<Hydrated, _>()
}

/// Has `pref` been reconciled with the relays yet this session?
///
/// **The publish gate.** These lists are whole-list newest-wins projections of
/// local state, so publishing one before the relay copy has been applied would
/// overwrite another device's prefs with this device's emptier view — a fresh
/// login that mutes one chat before the subscription replay lands would erase
/// every block, mute and nickname set elsewhere. Reconcile, THEN publish.
pub fn is_hydrated(pref: Pref) -> bool {
    hydrated_set().lock().map(|h| h.contains(pref.d_tag())).unwrap_or(false)
}

/// Mark `pref` reconciled. Called when a copy is applied AND when the relays
/// confirm none exists — "there is nothing to preserve" is just as reconciled
/// as having read it, and without that a first-ever account could never publish.
pub fn mark_hydrated(pref: Pref) {
    if let Ok(mut h) = hydrated_set().lock() {
        h.insert(pref.d_tag());
    }
}

/// Pull every list once at login and apply it, so this device is reconciled
/// before the user can touch anything. The live subscription also delivers
/// these, but it races the user; this does not.
///
/// A list whose fetch FAILS stays un-hydrated, so it stays unpublishable — far
/// better to leave prefs un-synced for a session than to overwrite prefs we
/// could not read.
pub async fn hydrate_all(client: &Client) -> Vec<(Pref, String)> {
    let Some(my_pk) = crate::state::my_public_key() else { return Vec::new() };
    let mut applied = Vec::new();
    for pref in [Pref::Blocks, Pref::Mutes, Pref::Nicknames] {
        match fetch_raw(client, my_pk, pref).await {
            Some(json) => {
                if save_local_raw(pref, &json).is_ok() {
                    mark_hydrated(pref);
                    applied.push((pref, json));
                }
            }
            // No stored copy: nothing to preserve, so this device may publish.
            None => mark_hydrated(pref),
        }
    }
    applied
}

/// Raw JSON of a list's local mirror. Callers parse into whichever shape the
/// list uses; the storage layer stays shape-agnostic.
pub fn load_local_raw(pref: Pref) -> Option<String> {
    crate::db::settings::get_sql_setting(pref.local_key().to_string())
        .ok()
        .flatten()
}

pub fn save_local_raw(pref: Pref, json: &str) -> Result<(), String> {
    crate::db::settings::set_sql_setting(pref.local_key().to_string(), json.to_string())
}

pub fn load_blocks() -> IdList {
    load_local_raw(Pref::Blocks).map(|s| IdList::from_json(&s)).unwrap_or_default()
}
pub fn load_mutes() -> IdList {
    load_local_raw(Pref::Mutes).map(|s| IdList::from_json(&s)).unwrap_or_default()
}
pub fn load_nicknames() -> NicknameMap {
    load_local_raw(Pref::Nicknames).map(|s| NicknameMap::from_json(&s)).unwrap_or_default()
}

async fn decrypt_event(my_pk: &PublicKey, event: &Event) -> Option<String> {
    if event.content.is_empty() {
        return None;
    }
    let signer = crate::signer::active_signer().ok()?;
    match signer.nip44_decrypt_async(my_pk, &event.content).await {
        Ok(plaintext) => Some(plaintext),
        Err(e) => {
            crate::log_warn!("[SyncedPrefs] decrypt {} failed: {}", event.kind.as_u16(), e);
            None
        }
    }
}

/// Fetch a list's relay copy as raw JSON, or `None` when the relays hold none.
pub async fn fetch_raw(client: &Client, my_pk: PublicKey, pref: Pref) -> Option<String> {
    let filter = Filter::new()
        .author(my_pk)
        .kind(Kind::Custom(event_kind::APPLICATION_SPECIFIC))
        .identifier(pref.d_tag())
        .limit(1);
    let events = client
        .fetch_events(filter)
        .timeout(std::time::Duration::from_secs(FETCH_TIMEOUT_SECS))
        .await
        .ok()?;
    let event = events.into_iter().next()?;
    decrypt_event(&my_pk, &event).await
}

/// Persist locally, then publish self-encrypted.
pub async fn publish_raw(client: &Client, pref: Pref, json: &str) -> Result<(), String> {
    let my_pk = crate::state::my_public_key().ok_or_else(|| "Not logged in".to_string())?;
    save_local_raw(pref, json)?;

    let signer = crate::signer::active_signer().map_err(|e| format!("Signer unavailable: {e}"))?;
    let content = signer
        .nip44_encrypt_async(&my_pk, json)
        .await
        .map_err(|e| format!("nip44 encrypt {}: {e}", pref.d_tag()))?;
    let builder = EventBuilder::new(Kind::Custom(event_kind::APPLICATION_SPECIFIC), content)
        .tag(Tag::identifier(pref.d_tag()));
    crate::sign_and_send(client, builder)
        .await
        .map_err(|e| format!("publish {}: {e}", pref.d_tag()))?;
    Ok(())
}

/// Consume a sibling device's update. Never republishes — the relay echoes our
/// own publishes back on this same subscription, and answering an echo with a
/// publish loops forever.
pub async fn ingest_remote(my_pk: &PublicKey, event: &Event) -> Option<(Pref, String)> {
    let d = event.tags.identifier().unwrap_or_default().to_string();
    let pref = Pref::from_d_tag(&d)?;
    let json = decrypt_event(my_pk, event).await?;
    if let Err(e) = save_local_raw(pref, &json) {
        crate::log_warn!("[SyncedPrefs] persisting {} failed: {e}", pref.d_tag());
        return None;
    }
    mark_hydrated(pref);
    Some((pref, json))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn d_tags_round_trip_and_are_distinct() {
        for p in [Pref::Blocks, Pref::Mutes, Pref::Nicknames] {
            assert_eq!(Pref::from_d_tag(p.d_tag()), Some(p));
        }
        // A tag belonging to another 30078 list must not resolve here, or the
        // self-sync router would hand a Community List to the block ingest.
        assert_eq!(Pref::from_d_tag("vector/communities"), None);
        assert_eq!(Pref::from_d_tag("vector/pinned"), None);
        assert_eq!(Pref::from_d_tag(""), None);
    }

    #[test]
    fn id_lists_add_idempotently_and_remove_tolerantly() {
        let mut l = IdList::default();
        l.add("npub1a").unwrap();
        l.add("npub1a").unwrap();
        assert_eq!(l.ids.len(), 1, "a second add is not a second entry");
        l.remove("never-present");
        l.remove("npub1a");
        assert!(l.ids.is_empty());
        assert!(l.add("  ").is_err(), "an empty id is refused, not stored");
    }

    #[test]
    fn an_empty_nickname_clears_rather_than_storing_a_blank() {
        let mut n = NicknameMap::default();
        n.set("npub1a", "Landlord").unwrap();
        assert_eq!(n.names.get("npub1a").map(String::as_str), Some("Landlord"));
        n.set("npub1a", "").unwrap();
        assert!(!n.names.contains_key("npub1a"), "clearing removes the key, not blanks it");
    }

    #[test]
    fn malformed_payloads_degrade_to_empty_instead_of_erroring() {
        assert!(IdList::from_json("not json").ids.is_empty());
        assert!(IdList::from_json("{}").ids.is_empty());
        assert!(NicknameMap::from_json("[]").names.is_empty());
        // An unknown version still yields its entries rather than being dropped.
        let future = IdList::from_json("{\"v\":99,\"ids\":[\"a\"],\"extra\":1}");
        assert_eq!(future.ids, vec!["a".to_string()]);
    }

    #[test]
    fn lists_refuse_to_grow_past_the_event_ceiling() {
        let mut l = IdList::default();
        for i in 0..MAX_ENTRIES {
            l.add(&format!("id{i}")).unwrap();
        }
        assert!(l.add("one-too-many").is_err(), "a list that cannot be opened is worse than a refusal");
        // Removing frees a slot again.
        l.remove("id0");
        assert!(l.add("one-too-many").is_ok());
    }

    #[test]
    fn nickname_order_is_stable_across_a_round_trip() {
        // BTreeMap, so two devices building the same set emit identical bytes —
        // no spurious republish churn from map iteration order.
        let mut a = NicknameMap::default();
        a.set("npub1z", "Zed").unwrap();
        a.set("npub1a", "Ann").unwrap();
        let mut b = NicknameMap::default();
        b.set("npub1a", "Ann").unwrap();
        b.set("npub1z", "Zed").unwrap();
        assert_eq!(a.to_json(), b.to_json(), "insertion order must not change the wire form");
    }
}
