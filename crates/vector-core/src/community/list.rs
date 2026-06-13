//! Cross-device joined-communities sync — the encrypted Community List.
//!
//! Joins don't reconstruct from the network (a DM bundle reaching device B doesn't mean device A
//! accepted it; a URL join leaves no on-relay trace), so memberships are synced explicitly via a
//! self-encrypted, replaceable per-user list (modeled on the emoji-pack list). Each entry carries a
//! HYBRID seed: `current_root` for instant-latest rehydration on a fresh device, and `seed_root` (the
//! stable earliest/join root) for the background full-history backfill. ADD on join, REMOVE on
//! self-removal; concurrent cross-device edits resolve by per-community latest-action-wins.
//!
//! This module owns the data model + the merge (conflict resolution). Transport (encrypt/publish/fetch),
//! reconcile, and rehydrate live alongside it (added in later increments).

use nostr_sdk::prelude::{Client, EventBuilder, Filter, Kind, NostrSigner, PublicKey, Tag, Timestamp};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::stored_event::event_kind;

/// The synced list rides a NIP-78 parameterized-replaceable event (kind 30078) addressed to ourselves,
/// distinguished from the wallpaper/badge 30078s by this `d`-tag. NIP-44-self-encrypted content — the
/// membership graph is private.
pub const COMMUNITY_LIST_D_TAG: &str = "vector/communities";
/// Local mirror of the (merged) list JSON — survives a teardown that deletes the community row, so a
/// tombstone outlives the community it buried. Account-scoped (settings live in the per-account DB).
const LOCAL_LIST_KEY: &str = "community_list_json";
/// UNIX-seconds of our most recent local mutation. A fetch ignores any relay copy older than this so a
/// just-changed (not-yet-propagated) list can't be clobbered by a stale relay — same guard as the emoji list.
const LIST_PUBLISHED_AT_KEY: &str = "community_list_published_at";
const FETCH_TIMEOUT_SECS: u64 = 20;

/// Milliseconds since the epoch — the clock the merge tiebreaks on (`added_at`/`removed_at`).
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// One joined community in the synced list. Carries TWO full bundles: the stable `seed` (earliest/join
/// — the full-history backfill anchor) and a refreshed `current` snapshot (latest root + latest channel
/// keys + latest name). A fresh device reconstructs DIRECTLY from `current` → instant-latest with NO
/// rekey walk; the seed is only for the (deferred) background full-history backfill.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq, Debug)]
pub struct CommunityListEntry {
    pub community_id: String,
    /// Full join material captured at join (the invite bundle): server root + per-channel keys + owner
    /// attestation + relays. STABLE (set once). The full-history backfill anchor; its
    /// `server_root_key`/`server_root_epoch` ARE the seed root/epoch.
    pub seed: super::invite::CommunityInvite,
    /// Freshest full snapshot — current root + CURRENT channel keys + current name — refreshed on every
    /// re-founding (rekey) and rename. Drives instant-latest rehydration: reconstruct straight from this and
    /// page, no walk. `None` only on a legacy entry (pre-`current`) → falls back to `seed` (one walk until
    /// the next refresh). Channel pseudonyms derive from the channel key, so this is the ONLY source of the
    /// current channel keys a fresh device needs to fetch the channel at the latest epoch.
    #[serde(default)]
    pub current: Option<super::invite::CommunityInvite>,
    /// When this membership was (re-)added, in ms. Tiebreaks against a removal tombstone.
    pub added_at: u64,
}

impl CommunityListEntry {
    /// The freshest snapshot to rehydrate from — `current` when present, else the seed (legacy entry).
    pub fn current(&self) -> &super::invite::CommunityInvite {
        self.current.as_ref().unwrap_or(&self.seed)
    }
    /// The seed (earliest/join) server-root epoch — the merge keeps the lowest (widest backfill reach).
    pub fn seed_epoch(&self) -> u64 {
        self.seed.server_root_epoch
    }
    /// The current (freshest) server-root epoch — the merge keeps the highest (instant-latest).
    pub fn current_epoch(&self) -> u64 {
        self.current().server_root_epoch
    }
    /// Freshest relay set known for this community (the current snapshot's set).
    pub fn relays(&self) -> &[String] {
        &self.current().relays
    }
}

/// A removal tombstone — keeps a stale device from resurrecting a community you left/were-removed-from.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq, Debug)]
pub struct CommunityRemoval {
    pub community_id: String,
    pub removed_at: u64,
}

/// The whole synced list: present memberships + removal tombstones.
#[derive(Clone, Serialize, Deserialize, Default, Debug, PartialEq, Eq)]
pub struct CommunityList {
    #[serde(default)]
    pub entries: Vec<CommunityListEntry>,
    #[serde(default)]
    pub tombstones: Vec<CommunityRemoval>,
}

/// Stable canonical bytes of a snapshot, for a deterministic cross-device tiebreak (serde struct field
/// order is fixed, so this is reproducible on every device).
fn canonical(inv: &super::invite::CommunityInvite) -> String {
    serde_json::to_string(inv).unwrap_or_default()
}

/// Does `a` supersede `b` as the CURRENT (freshest) snapshot? Higher epoch wins; on an epoch TIE the
/// lexicographically-lowest canonical bytes win. This is a TOTAL order over the whole bundle (not just the
/// root), so a same-epoch change (e.g. a rename, same root) can't leave two devices disagreeing or flapping
/// competing republishes. The displayed name stays authoritative via the control-plane fold; this only
/// makes the cached `current` converge.
fn current_supersedes(a: &super::invite::CommunityInvite, b: &super::invite::CommunityInvite) -> bool {
    use std::cmp::Ordering::*;
    match a.server_root_epoch.cmp(&b.server_root_epoch) {
        Greater => true,
        Less => false,
        Equal => canonical(a) < canonical(b),
    }
}

/// Does `a` supersede `b` as the SEED (earliest / widest-backfill) bundle? LOWER epoch wins; on a tie the
/// lowest canonical bytes win — also a total order, so the seed converges identically across devices.
fn seed_supersedes(a: &super::invite::CommunityInvite, b: &super::invite::CommunityInvite) -> bool {
    use std::cmp::Ordering::*;
    match a.server_root_epoch.cmp(&b.server_root_epoch) {
        Less => true,
        Greater => false,
        Equal => canonical(a) < canonical(b),
    }
}

impl CommunityList {
    /// Parse from the decrypted JSON content of the list event. A malformed/empty payload is an empty list
    /// (never an error that aborts a refresh — same posture as the emoji list).
    pub fn from_json(s: &str) -> Self {
        serde_json::from_str(s).unwrap_or_default()
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".to_string())
    }

    /// True iff `community_id` is a present membership (an entry whose add is newer than any tombstone).
    pub fn contains(&self, community_id: &str) -> bool {
        self.entries.iter().any(|e| e.community_id == community_id)
    }

    /// Timestamp-supersession test (pure): is a decline/leave tombstone for `community_id` at least as
    /// new as an invite whose outer `created_at` is `invite_created_at_secs`? `true` = suppress (the
    /// invite is a re-delivery or older); a strictly-newer invite returns `false` and resurfaces.
    /// `removed_at` is ms, invite `created_at` is seconds — compared in ms.
    pub fn tombstone_suppresses_at(&self, community_id: &str, invite_created_at_secs: u64) -> bool {
        let invite_ms = invite_created_at_secs.saturating_mul(1000);
        self.tombstones
            .iter()
            .any(|t| t.community_id == community_id && t.removed_at >= invite_ms)
    }

    /// ADD/refresh a membership locally (call before republishing). If a newer entry already exists it
    /// keeps the earliest seed (stable) and freshest current_root; a re-add after a tombstone resurrects.
    pub fn upsert(&mut self, entry: CommunityListEntry) {
        // A fresh add dominates any prior tombstone for this community (re-join) as long as its added_at is
        // newer — drop stale tombstones for it; the merge below re-derives the rest.
        self.tombstones.retain(|t| !(t.community_id == entry.community_id && t.removed_at <= entry.added_at));
        if let Some(cur) = self.entries.iter_mut().find(|e| e.community_id == entry.community_id) {
            // Decide before moving any field out of `entry` (a method call borrows the whole value).
            // TOTAL-order tiebreaks (epoch, then canonical bytes) so every device converges identically.
            let take_current = current_supersedes(entry.current(), cur.current());
            let take_seed = seed_supersedes(&entry.seed, &cur.seed);
            // Keep the freshest full snapshot (latest root + channel keys + name) for instant-latest.
            if take_current {
                cur.current = Some(entry.current().clone());
            }
            // Keep the earliest join material (widest backfill reach + the channel keys it carries).
            if take_seed {
                cur.seed = entry.seed;
            }
            cur.added_at = cur.added_at.max(entry.added_at);
        } else {
            self.entries.push(entry);
        }
    }

    /// REMOVE a membership locally (self-removal teardown): drop the entry + leave a tombstone so a
    /// stale device can't resurrect it.
    pub fn remove(&mut self, community_id: &str, removed_at: u64) {
        self.entries.retain(|e| e.community_id != community_id);
        match self.tombstones.iter_mut().find(|t| t.community_id == community_id) {
            Some(t) => t.removed_at = t.removed_at.max(removed_at),
            None => self.tombstones.push(CommunityRemoval { community_id: community_id.to_string(), removed_at }),
        }
    }

    /// Refresh the `current` snapshot (latest root + channel keys + name) of an existing entry — the rekey /
    /// rename follow. Accepts a newer epoch OR a same-epoch change (e.g. a rename); never regresses to
    /// an older epoch. No-op if the community isn't present. Returns true if it changed anything.
    pub fn refresh_current(&mut self, community_id: &str, current: super::invite::CommunityInvite) -> bool {
        if let Some(e) = self.entries.iter_mut().find(|e| e.community_id == community_id) {
            // Use the SAME total order as `merge`, so a local refresh only installs a snapshot that would
            // also WIN the merge — otherwise two devices ping-pong competing republishes (a refresh that
            // loses the next merge would re-assert forever). A higher epoch always wins (the rekey follow).
            if current_supersedes(&current, e.current()) {
                e.current = Some(current);
                return true;
            }
        }
        false
    }

    /// True iff publishing `self` would add something the `relay` copy doesn't already reflect — i.e. we
    /// hold a membership/tombstone the relay lacks (a backfill discovery, or a local edit that never
    /// propagated before the app closed). Boot uses this to write ONLY when genuinely ahead: a read-only
    /// sync otherwise, so two devices booting don't clobber each other's just-published joins by timestamp.
    pub fn is_ahead_of(&self, relay: &CommunityList) -> bool {
        let empty = CommunityList::default();
        relay.merge(self) != relay.merge(&empty)
    }

    /// READ-MERGE-WRITE conflict resolution across devices: fold `other` (e.g. the relay's copy)
    /// into `self` (local), per-community LATEST-ACTION-WINS. Among surviving entries the freshest
    /// `current_root` (highest `current_epoch`) wins and the earliest seed (lowest `seed_epoch`) is kept;
    /// a removal newer than the add buries it (you left); an add newer than the removal resurrects it
    /// (re-join). Deterministic — every device computes the same merged list.
    pub fn merge(&self, other: &CommunityList) -> CommunityList {
        // Fold all adds → newest-wins-per-field per community.
        let mut adds: HashMap<String, CommunityListEntry> = HashMap::new();
        for e in self.entries.iter().chain(other.entries.iter()) {
            match adds.get_mut(&e.community_id) {
                Some(cur) => {
                    // Freshest current + earliest seed, each via a TOTAL order (epoch, then canonical bytes)
                    // so the merged bytes are identical on every device regardless of fold order — even when
                    // two snapshots share an epoch AND a root (e.g. a same-epoch rename).
                    if current_supersedes(e.current(), cur.current()) {
                        cur.current = Some(e.current().clone());
                    }
                    if seed_supersedes(&e.seed, &cur.seed) {
                        cur.seed = e.seed.clone();
                    }
                    cur.added_at = cur.added_at.max(e.added_at);
                }
                None => { adds.insert(e.community_id.clone(), e.clone()); }
            }
        }
        // Newest removal per community.
        let mut rms: HashMap<String, u64> = HashMap::new();
        for t in self.tombstones.iter().chain(other.tombstones.iter()) {
            let slot = rms.entry(t.community_id.clone()).or_insert(0);
            *slot = (*slot).max(t.removed_at);
        }
        let mut entries: Vec<CommunityListEntry> = Vec::new();
        let mut tombstones: Vec<CommunityRemoval> = Vec::new();
        for (cid, e) in adds {
            let removed_at = rms.get(&cid).copied().unwrap_or(0);
            if e.added_at >= removed_at {
                entries.push(e); // present: never removed, or re-joined after a tombstone
            } else {
                tombstones.push(CommunityRemoval { community_id: cid, removed_at });
            }
        }
        for (cid, removed_at) in rms {
            if !entries.iter().any(|e| e.community_id == cid)
                && !tombstones.iter().any(|t| t.community_id == cid)
            {
                tombstones.push(CommunityRemoval { community_id: cid, removed_at });
            }
        }
        // Stable order so the serialized bytes (and the published event) are deterministic across devices.
        entries.sort_by(|a, b| a.community_id.cmp(&b.community_id));
        tombstones.sort_by(|a, b| a.community_id.cmp(&b.community_id));
        CommunityList { entries, tombstones }
    }
}

impl CommunityListEntry {
    /// Build a membership entry from a held community at join/create time. The held bundle IS both the
    /// stable `seed` (backfill anchor) and the `current` snapshot (instant-latest); they diverge later only
    /// as `refresh_current` follows re-foundings + renames forward.
    pub fn from_community(community: &super::Community, added_at_ms: u64) -> Self {
        // The invite bundle IS the join material — server root + channel keys + owner attestation + name.
        let bundle = super::invite::build_invite(community);
        CommunityListEntry {
            community_id: community.id.to_hex(),
            current: Some(bundle.clone()),
            seed: bundle,
            added_at: added_at_ms,
        }
    }
}

// ============================================================================
// Local mirror (account-scoped settings)
// ============================================================================

/// The device's own view of the list — what it knows it joined + the tombstones it must honor. Distinct
/// from the communities DB because a tombstone has to outlive the community row that teardown deletes.
pub fn load_local_list() -> CommunityList {
    crate::db::settings::get_sql_setting(LOCAL_LIST_KEY.to_string())
        .ok()
        .flatten()
        .map(|s| CommunityList::from_json(&s))
        .unwrap_or_default()
}

pub fn save_local_list(list: &CommunityList) -> Result<(), String> {
    crate::db::settings::set_sql_setting(LOCAL_LIST_KEY.to_string(), list.to_json())
}

/// Stamp the local-mutation clock NOW (synchronously, before any debounced publish fires) so a refresh
/// racing the not-yet-published change still treats the local copy as newer than a stale relay's.
fn stamp_published_now() {
    let _ = crate::db::settings::set_sql_setting(
        LIST_PUBLISHED_AT_KEY.to_string(),
        Timestamp::now().as_secs().to_string(),
    );
}

// ============================================================================
// Transport (NIP-44-self-encrypted kind 30078, parameterized-replaceable)
// ============================================================================

/// Fetch our list event from relays and decrypt it. Returns the relay's `CommunityList`, or the LOCAL
/// mirror when relays return nothing (a transient sync gap) or a copy that predates our last publish (our
/// republish hasn't propagated — trusting it would clobber a just-made change). Never errors on a missing
/// list; an empty list is a valid state. Session-guarded against an account swap mid-fetch.
pub async fn fetch_community_list(
    client: &Client,
    my_pubkey: PublicKey,
    session: crate::state::SessionGuard,
) -> Result<CommunityList, String> {
    let filter = Filter::new()
        .author(my_pubkey)
        .kind(Kind::Custom(event_kind::APPLICATION_SPECIFIC))
        .identifier(COMMUNITY_LIST_D_TAG)
        .limit(1);

    let events = client
        .fetch_events(filter, std::time::Duration::from_secs(FETCH_TIMEOUT_SECS))
        .await
        .map_err(|e| format!("fetch community list (kind 30078): {}", e))?;

    if !session.is_valid() {
        return Ok(load_local_list());
    }

    let our_last_publish: u64 = crate::db::settings::get_sql_setting(LIST_PUBLISHED_AT_KEY.to_string())
        .ok()
        .flatten()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    let latest = events.into_iter().max_by_key(|e| e.created_at);
    let list = match latest {
        Some(ev) if ev.created_at.as_secs() < our_last_publish => {
            crate::log_debug!(
                "[CommunityList] relay copy (created_at {}) predates our publish ({}) — keeping local",
                ev.created_at.as_secs(), our_last_publish,
            );
            load_local_list()
        }
        Some(ev) => decrypt_list_event(client, &my_pubkey, &ev).await,
        None => {
            crate::log_debug!("[CommunityList] no list on relays — using local mirror");
            load_local_list()
        }
    };
    Ok(list)
}

/// Decrypt a fetched list event; a malformed/undecryptable payload degrades to an empty list (never an
/// error that aborts a reconcile), matching the emoji list's posture.
async fn decrypt_list_event(client: &Client, my_pk: &PublicKey, event: &nostr_sdk::prelude::Event) -> CommunityList {
    if event.content.is_empty() {
        return CommunityList::default();
    }
    let signer = match client.signer().await {
        Ok(s) => s,
        Err(e) => {
            crate::log_warn!("[CommunityList] signer unavailable for decrypt: {}", e);
            return CommunityList::default();
        }
    };
    match signer.nip44_decrypt(my_pk, &event.content).await {
        Ok(plaintext) => CommunityList::from_json(&plaintext),
        Err(e) => {
            crate::log_warn!("[CommunityList] decrypt failed: {}", e);
            CommunityList::default()
        }
    }
}

/// READ-MERGE-WRITE publish: fold the relay's copy into our local mirror (so a concurrent edit from
/// another device survives), persist the merged result locally, then publish it self-encrypted. The merge
/// is deterministic, so every device converges on identical bytes regardless of publish order.
pub async fn publish_community_list(
    client: &Client,
    session: crate::state::SessionGuard,
) -> Result<(), String> {
    let my_pk = crate::state::my_public_key().ok_or_else(|| "Not logged in".to_string())?;

    // Fold the relay's copy first so we don't drop a sibling device's change.
    let relay = fetch_community_list(client, my_pk, session.clone()).await.unwrap_or_default();
    if !session.is_valid() {
        return Ok(());
    }
    let merged = load_local_list().merge(&relay);
    save_local_list(&merged)?;

    let signer = client.signer().await.map_err(|e| format!("Signer unavailable: {}", e))?;
    let content = signer
        .nip44_encrypt(&my_pk, &merged.to_json())
        .await
        .map_err(|e| format!("nip44 encrypt community list: {}", e))?;

    let builder = EventBuilder::new(Kind::Custom(event_kind::APPLICATION_SPECIFIC), content)
        .tag(Tag::identifier(COMMUNITY_LIST_D_TAG));
    client
        .send_event_builder(builder)
        .await
        .map_err(|e| format!("Failed to publish community list (kind 30078): {}", e))?;

    crate::log_info!(
        "[CommunityList] Published encrypted list: {} membership(s), {} tombstone(s)",
        merged.entries.len(), merged.tombstones.len(),
    );
    Ok(())
}

/// Consume a remotely-received list event (the live cross-device path): decrypt it, fold it into the local
/// mirror, persist. Deliberately does NOT republish — the relay echoes our OWN publishes back on the live
/// subscription too, so republishing here would loop forever. A local-only entry not in this event simply
/// waits for the next local mutation / boot to propagate. Returns the merged list so the caller can
/// rehydrate any newly-present community.
pub async fn ingest_remote_list_event(
    client: &Client,
    my_pk: &PublicKey,
    event: &nostr_sdk::prelude::Event,
    session: crate::state::SessionGuard,
) -> Result<CommunityList, String> {
    let incoming = decrypt_list_event(client, my_pk, event).await;
    if !session.is_valid() {
        return Ok(load_local_list());
    }
    let merged = load_local_list().merge(&incoming);
    save_local_list(&merged)?;
    Ok(merged)
}

static REPUBLISH_GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Coalesce rapid join/leave/refresh mutations into one network publish. The local mirror is already
/// updated by the caller; this only pushes it out. Stamps the publish clock + captures `SessionGuard`
/// BEFORE the debounce sleep, mirroring `republish_emoji_list_debounced`.
pub fn republish_community_list_debounced() {
    use std::sync::atomic::Ordering;
    stamp_published_now();
    let gen = REPUBLISH_GEN.fetch_add(1, Ordering::SeqCst) + 1;
    let session = crate::state::SessionGuard::capture();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(800)).await;
        if REPUBLISH_GEN.load(Ordering::SeqCst) != gen { return; }
        if !session.is_valid() { return; }
        let client = match crate::state::nostr_client() {
            Some(c) => c,
            None => return,
        };
        if let Err(e) = publish_community_list(&client, session.clone()).await {
            crate::log_warn!("[CommunityList] Republish failed: {} (retrying in 5s)", e);
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            if REPUBLISH_GEN.load(Ordering::SeqCst) != gen { return; }
            if !session.is_valid() { return; }
            if let Err(e) = publish_community_list(&client, session).await {
                crate::log_warn!("[CommunityList] Republish retry failed: {}", e);
            }
        }
    });
}

// ============================================================================
// Local mutations (the hooks call these, then debounce-republish)
// ============================================================================

/// Seed the local mirror from communities ALREADY persisted in the DB but missing from the list — the ones
/// joined BEFORE this feature shipped (so they never hit `add_membership`), or on a device that predates
/// it. Idempotent: a community already listed is skipped; a tombstoned one is resurrected (we still hold it
/// in the DB, so we ARE a member). Run at boot before the publish so existing memberships sync too. Does NOT
/// republish itself — the caller's `publish_community_list` pushes the seeded list out.
pub fn backfill_from_db() {
    let ids = match crate::db::community::list_community_ids() {
        Ok(ids) => ids,
        Err(e) => {
            crate::log_warn!("[CommunityList] backfill: list_community_ids failed: {}", e);
            return;
        }
    };
    let mut list = load_local_list();
    let mut changed = false;
    for id in ids {
        let hex = id.to_hex();
        if list.contains(&hex) {
            continue; // already listed
        }
        // Don't RESURRECT a community we left: a tombstone means we removed it, but a DB row may linger (a
        // teardown that didn't fully run, or a sibling's leave we folded but haven't torn down yet). Re-adding
        // it here (with `added_at = now`, which beats the tombstone) would silently undo the leave on every
        // device. The boot reconcile tears the stale row down separately.
        if list.tombstones.iter().any(|t| t.community_id == hex) {
            continue;
        }
        if let Ok(Some(community)) = crate::db::community::load_community(&id) {
            list.upsert(CommunityListEntry::from_community(&community, now_ms()));
            changed = true;
        }
    }
    if changed {
        if let Err(e) = save_local_list(&list) {
            crate::log_warn!("[CommunityList] backfill save failed: {}", e);
        }
    }
}

/// The join/awareness time (ms) for a listed community — when WE (or the device that published the entry)
/// joined. Drives chatlist ordering for a not-yet-active community: an empty community sorts by when it was
/// joined, never to the bottom (no messages means newest, not oldest). `None` if not in the list.
pub fn membership_added_at(community_id: &str) -> Option<u64> {
    load_local_list()
        .entries
        .iter()
        .find(|e| e.community_id == community_id)
        .map(|e| e.added_at)
}

/// ADD/refresh a membership on join/create: upsert into the local mirror, stamp, and schedule a publish.
pub fn add_membership(community: &super::Community) {
    let mut list = load_local_list();
    list.upsert(CommunityListEntry::from_community(community, now_ms()));
    if let Err(e) = save_local_list(&list) {
        crate::log_warn!("[CommunityList] save after add failed: {}", e);
        return;
    }
    republish_community_list_debounced();
}

/// REMOVE a membership on a LOCAL self-removal (teardown): tombstone it locally, stamp, schedule a
/// publish so our other devices tear it down too.
pub fn remove_membership(community_id: &str) {
    let mut list = load_local_list();
    list.remove(community_id, now_ms());
    if let Err(e) = save_local_list(&list) {
        crate::log_warn!("[CommunityList] save after remove failed: {}", e);
        return;
    }
    republish_community_list_debounced();
}

/// Tombstone a membership locally WITHOUT republishing — the receive path (a sibling device already
/// published the removal). Republishing here would just re-echo our own event over the live self-sync
/// subscription. The local mirror is updated so a later local mutation/boot still carries the tombstone.
pub fn tombstone_local_only(community_id: &str) {
    let mut list = load_local_list();
    list.remove(community_id, now_ms());
    if let Err(e) = save_local_list(&list) {
        crate::log_warn!("[CommunityList] local-only tombstone save failed: {}", e);
    }
}

/// True if a decline/leave tombstone for `community_id` is at least as new as an incoming invite
/// (whose outer `created_at` is in SECONDS) — i.e. the invite is a re-delivery of, or older than, the
/// decision to suppress this community. A STRICTLY-NEWER invite returns false so it resurfaces for an
/// explicit Accept/Decline (the timestamp-supersession rule). This is what lets the un-deletable
/// gift-wrapped invite stop re-nagging after a decline/leave — including on a fresh device, which folds
/// the tombstone from the synced list before it parks the re-fetched bundle.
pub fn tombstone_suppresses(community_id: &str, invite_created_at_secs: u64) -> bool {
    load_local_list().tombstone_suppresses_at(community_id, invite_created_at_secs)
}

/// Follow a re-founding OR a rename forward: refresh an entry's `current` snapshot (latest root + channel
/// keys + name) from the freshest community state, so a fresh device jumps STRAIGHT to the latest epoch with
/// the current keys (no walk) and shows the current name. No-op if the community isn't in our list or the
/// snapshot is unchanged — so it republishes only on a real rekey/rename, never on a quiet sync.
pub fn refresh_membership_current(community: &super::Community) {
    let cid = community.id.to_hex();
    let mut list = load_local_list();
    if !list.contains(&cid) {
        return;
    }
    let snapshot = super::invite::build_invite(community);
    if !list.refresh_current(&cid, snapshot) {
        return; // nothing changed → no pointless republish
    }
    if let Err(e) = save_local_list(&list) {
        crate::log_warn!("[CommunityList] save after refresh failed: {}", e);
        return;
    }
    republish_community_list_debounced();
}

// ============================================================================
// Rehydrate — reconstruct a listed community on a device that doesn't hold it
// ============================================================================

/// Outcome of rehydrating one listed community on a fresh device.
pub enum RehydrateOutcome {
    /// Reconstructed + fast-forwarded to current; ready to page + subscribe.
    Rehydrated(super::Community),
    /// Already held locally — nothing to do (keeps boot reconcile idempotent).
    AlreadyHeld(super::Community),
    /// The fast-forward revealed an authorized rotation that excluded us (private ban / read-cut). Local
    /// data was erased; the caller tombstones it out of the list so no device resurrects it.
    Removed,
}

/// Reconstruct a joined community for an INSTANT latest-epoch view (Discord-style cross-device bootstrap).
/// Reconstructs from the `current` snapshot (latest root + channel keys + name) and confirms the head — so a
/// re-founded community renders its newest messages immediately, with NO upfront rekey walk. Prior-epoch keys
/// are archived separately + quietly by [`backfill_history_from_seed`], spawned by the caller, so older
/// history fills in on scroll-back without blocking first paint. For a never-re-founded community (or a legacy
/// entry with no `current`) `current()` == `seed`, so `catch_up_server_root` is a single cheap probe and the
/// backfill is a no-op.
pub async fn rehydrate_community_from_seed<T: super::transport::Transport + ?Sized>(
    transport: &T,
    entry: &CommunityListEntry,
    session: crate::state::SessionGuard,
) -> Result<RehydrateOutcome, String> {
    // Validate the id is real 64-char hex — `hex_to_bytes_32` is lenient (zero-pads/zero-decodes), so a
    // malformed entry would silently key the lookup to a bogus id. (Self-authored, so defense-in-depth.)
    if entry.community_id.len() != 64 || !entry.community_id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!("invalid community id in list entry: {}", entry.community_id));
    }
    let id = super::CommunityId(crate::simd::hex::hex_to_bytes_32(&entry.community_id));

    // Idempotent: if we already hold it (a concurrent join, or a prior reconcile), do nothing.
    if let Some(existing) = crate::db::community::load_community(&id)? {
        return Ok(RehydrateOutcome::AlreadyHeld(existing));
    }

    // Reconstruct + persist from the CURRENT snapshot (latest root + channel keys) for an INSTANT latest view.
    // `catch_up_server_root` below is then one cheap probe (or walks forward if `current` is itself stale);
    // prior-epoch keys are archived quietly by `backfill_history_from_seed`. For a legacy/never-re-founded entry
    // `current()` == `seed`, so this is the full from-genesis walk and the backfill is a no-op.
    let community = super::service::accept_invite(entry.current())?;
    if !session.is_valid() {
        // Session swapped: the DB pool may now be another account's — do NOT delete (it'd hit the wrong DB).
        return Err("account changed during rehydrate".to_string());
    }

    // Fast-forward the base. An authorized rotation that excluded us == removal: leave the (saved) community
    // in place and signal the caller to tear it down fully (DB + STATE + routes); we can't read the new
    // control plane to learn the ban the normal way. A transient fetch error cleans up the partial save so
    // the next boot retries from scratch instead of stranding a seed-epoch community.
    match super::service::catch_up_server_root(transport, &community).await {
        Ok(c) if c.removed => return Ok(RehydrateOutcome::Removed),
        Ok(_) => {}
        Err(e) => {
            if session.is_valid() {
                let _ = crate::db::community::delete_community_retain_keys(&entry.community_id);
            }
            return Err(e);
        }
    }
    let community = crate::db::community::load_community(&id)?.unwrap_or(community);

    // Fold the whole control plane (roster / mode / metadata / channel names). A dissolution tombstone
    // present here seals it (read-only) — still a valid rehydrate.
    let _ = super::service::fetch_and_apply_control(transport, &community).await;
    let community = crate::db::community::load_community(&id)?.unwrap_or(community);

    // Walk each channel's rekey chain so we hold the CURRENT channel key before the caller pages it.
    for ch in &community.channels {
        let _ = super::service::catch_up_channel_rekeys(transport, &community, &ch.id).await;
    }
    if !session.is_valid() {
        return Err("account changed during rehydrate".to_string());
    }
    let community = crate::db::community::load_community(&id)?.unwrap_or(community);
    Ok(RehydrateOutcome::Rehydrated(community))
}

/// Quietly archive the PRIOR epochs' keys for a community that was rehydrated instant-from-`current`, so its
/// older-epoch history becomes decryptable (loads on scroll-back via the multi-epoch read). Spawn this AFTER
/// the instant `rehydrate_community_from_seed` so it never blocks first paint.
///
/// Safe by construction: it drives the proven `catch_up_*` walk from an IN-MEMORY view at the SEED epoch and
/// never persists that view. Both epoch advances (`advance_server_root_epoch`/`advance_channel_epoch`) are
/// MONOTONIC — they always archive the walked epoch's key but only move the head FORWARD — so re-walking
/// seed..head re-archives the missing prior keys without ever regressing the live (current-epoch) head.
/// No-op when there's nothing below `current` to fill (never re-founded, or a legacy entry).
/// Returns `Ok(true)` if it actually walked (there were prior epochs to archive), so the caller can re-open
/// scroll-back; `Ok(false)` when there was nothing below `current` to fill.
pub async fn backfill_history_from_seed<T: super::transport::Transport + ?Sized>(
    transport: &T,
    entry: &CommunityListEntry,
    session: crate::state::SessionGuard,
) -> Result<bool, String> {
    // Only meaningful when `current` sits above `seed` — i.e. the instant view jumped past earlier epochs.
    if entry.current().server_root_epoch <= entry.seed.server_root_epoch {
        return Ok(false);
    }
    let id = super::CommunityId(crate::simd::hex::hex_to_bytes_32(&entry.community_id));
    // Must already be held (the foreground rehydrate saved it); otherwise there's nothing to backfill into.
    if crate::db::community::load_community(&id)?.is_none() {
        return Ok(false);
    }
    // In-memory view at the seed epoch — drives the walk from the bottom; NEVER saved.
    let seed_view = super::invite::accept_invite(&entry.seed)?;
    if !session.is_valid() {
        return Ok(false);
    }
    // Archive the SEED epoch's own keys. The catch_up walk archives only the epochs it walks TO
    // (seed+1..head), never its STARTING epoch — and we never `save_community(seed_view)` (which is what
    // normally archives a bundle's keys), so without this the seed/join epoch's channel key is absent from
    // the archive and its messages stay unreadable (`read_epoch_keys` returns the archive exclusively once
    // it's non-empty). Idempotent (PK includes epoch).
    let _ = crate::db::community::store_epoch_key(
        &entry.community_id, crate::community::SERVER_ROOT_SCOPE_HEX,
        seed_view.server_root_epoch.0, seed_view.server_root_key.as_bytes(),
    );
    for ch in &seed_view.channels {
        let _ = crate::db::community::store_epoch_key(
            &entry.community_id, &ch.id.to_hex(), ch.epoch.0, ch.key.as_bytes(),
        );
    }
    // Base first: archives every prior server root (the channel-rekey walk opens rekeys under held roots).
    let _ = super::service::catch_up_server_root(transport, &seed_view).await;
    if !session.is_valid() {
        return Ok(false);
    }
    // Then each channel: archives every prior channel key. Monotonic advance keeps the live head intact.
    for ch in &seed_view.channels {
        if !session.is_valid() {
            return Ok(false);
        }
        let _ = super::service::catch_up_channel_rekeys(transport, &seed_view, &ch.id).await;
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bundle(cid: &str, epoch: u64, root: &str) -> crate::community::invite::CommunityInvite {
        crate::community::invite::CommunityInvite {
            community_id: cid.into(),
            name: "T".into(),
            server_root_key: root.into(),
            server_root_epoch: epoch,
            relays: vec!["r1".into()],
            channels: vec![],
            owner_attestation: None,
        }
    }

    fn entry(cid: &str, seed_epoch: u64, current_epoch: u64, added_at: u64) -> CommunityListEntry {
        CommunityListEntry {
            community_id: cid.into(),
            seed: bundle(cid, seed_epoch, &format!("seed{seed_epoch}")),
            current: Some(bundle(cid, current_epoch, &format!("cur{current_epoch}"))),
            added_at,
        }
    }

    #[test]
    fn merge_keeps_freshest_current_root_and_earliest_seed() {
        // Device A holds an older current_root; device B a newer one. Merge → newest current, earliest seed.
        let a = CommunityList { entries: vec![entry("X", 5, 8, 100)], tombstones: vec![] };
        let b = CommunityList { entries: vec![entry("X", 5, 11, 100)], tombstones: vec![] };
        let m = a.merge(&b);
        assert_eq!(m.entries.len(), 1);
        assert_eq!(m.entries[0].current_epoch(), 11, "freshest current_root wins");
        assert_eq!(m.entries[0].seed_epoch(), 5, "seed stays the earliest/join root");
    }

    #[test]
    fn remove_newer_than_add_buries_membership() {
        // Joined on A (t=100), left on B (t=200). Merge → removed (tombstone), no entry.
        let a = CommunityList { entries: vec![entry("X", 5, 8, 100)], tombstones: vec![] };
        let mut b = CommunityList::default();
        b.remove("X", 200);
        let m = a.merge(&b);
        assert!(!m.contains("X"), "a removal newer than the add buries the community");
        assert!(m.tombstones.iter().any(|t| t.community_id == "X"));
    }

    #[test]
    fn decline_tombstone_suppresses_old_invite_not_newer() {
        // Decline recorded at 5_000_000 ms (= 5000 s). removed_at is ms; invite created_at is secs.
        let list = CommunityList { entries: vec![], tombstones: vec![CommunityRemoval {
            community_id: "X".into(), removed_at: 5_000_000,
        }] };
        // A re-delivery of the same/older invite (≤ 5000 s) is suppressed...
        assert!(list.tombstone_suppresses_at("X", 5000), "same-time re-delivery suppressed");
        assert!(list.tombstone_suppresses_at("X", 4999), "older invite suppressed");
        // ...a strictly-newer invite (deliberate re-invite) resurfaces.
        assert!(!list.tombstone_suppresses_at("X", 5001), "newer invite resurfaces");
        // An unrelated community is never suppressed.
        assert!(!list.tombstone_suppresses_at("Y", 1), "other community unaffected");
    }

    #[test]
    fn add_newer_than_removal_resurrects_on_rejoin() {
        // Left at t=100, re-joined at t=200. Merge → present (re-join wins), tombstone dropped.
        let mut a = CommunityList::default();
        a.remove("X", 100);
        let b = CommunityList { entries: vec![entry("X", 5, 9, 200)], tombstones: vec![] };
        let m = a.merge(&b);
        assert!(m.contains("X"), "a re-join newer than the tombstone resurrects the community");
        assert!(!m.tombstones.iter().any(|t| t.community_id == "X"), "stale tombstone dropped");
    }

    #[test]
    fn merge_is_deterministic_and_order_independent() {
        let a = CommunityList { entries: vec![entry("B", 1, 3, 50), entry("A", 2, 4, 60)], tombstones: vec![] };
        let b = CommunityList { entries: vec![entry("A", 2, 5, 60)], tombstones: vec![] };
        assert_eq!(a.merge(&b), b.merge(&a), "merge is commutative");
        assert_eq!(a.merge(&b).entries[0].community_id, "A", "entries sorted deterministically");
    }

    #[test]
    fn upsert_resurrects_over_old_tombstone_keeps_stable_seed() {
        let mut list = CommunityList::default();
        list.remove("X", 100);
        list.upsert(entry("X", 5, 5, 200)); // re-join newer than the tombstone
        assert!(list.contains("X"));
        assert!(!list.tombstones.iter().any(|t| t.community_id == "X"));
        // A later current refresh keeps the join seed.
        list.refresh_current("X", bundle("X", 12, "cur12"));
        let e = list.entries.iter().find(|e| e.community_id == "X").unwrap();
        assert_eq!(e.current_epoch(), 12);
        assert_eq!(e.seed_epoch(), 5, "seed root stays the join root through refreshes");
    }

    #[test]
    fn round_trips_through_json() {
        let list = CommunityList { entries: vec![entry("X", 5, 9, 100)], tombstones: vec![CommunityRemoval { community_id: "Y".into(), removed_at: 50 }] };
        assert_eq!(CommunityList::from_json(&list.to_json()), list);
        assert_eq!(CommunityList::from_json("garbage"), CommunityList::default());
    }

    #[test]
    fn merge_is_deterministic_on_equal_epoch_ties() {
        // Two devices joined the SAME community at epoch 0 via different invite links (different seed bytes),
        // and saw the same current epoch under different roots. Without a total tiebreak the two devices
        // would compute different merged bytes and never converge. Assert commutativity + a fixed winner.
        let mut a_entry = entry("X", 0, 0, 100);
        a_entry.seed.server_root_key = "bbbb".into();
        a_entry.current = Some(bundle("X", 0, "rootB"));
        let mut b_entry = entry("X", 0, 0, 100);
        b_entry.seed.server_root_key = "aaaa".into();
        b_entry.current = Some(bundle("X", 0, "rootA"));
        let a = CommunityList { entries: vec![a_entry], tombstones: vec![] };
        let b = CommunityList { entries: vec![b_entry], tombstones: vec![] };
        assert_eq!(a.merge(&b), b.merge(&a), "equal-epoch merge must be commutative");
        let m = a.merge(&b);
        assert_eq!(m.entries[0].seed.server_root_key, "aaaa", "tie → lowest seed root wins");
        assert_eq!(m.entries[0].current().server_root_key, "rootA", "tie → lowest current root wins");
    }

    #[test]
    fn merge_converges_on_same_epoch_same_root_rename() {
        // Two devices at the SAME epoch AND SAME root but DIFFERENT names (a rename). The old root-only
        // tiebreak was a no-op here → non-deterministic (first-seen won) → divergence + republish flap.
        // The full-canonical total order must converge identically both fold orders.
        let mut a = entry("X", 0, 0, 100);
        a.current = Some({ let mut b = bundle("X", 0, "root"); b.name = "Bbb".into(); b });
        let mut b = entry("X", 0, 0, 100);
        b.current = Some({ let mut x = bundle("X", 0, "root"); x.name = "Aaa".into(); x });
        let la = CommunityList { entries: vec![a], tombstones: vec![] };
        let lb = CommunityList { entries: vec![b], tombstones: vec![] };
        assert_eq!(la.merge(&lb), lb.merge(&la), "same-epoch+root rename must converge (commutative)");
        assert_eq!(la.merge(&lb).entries[0].current().name, "Aaa", "tie → lowest canonical wins deterministically");
    }

    #[test]
    fn refresh_current_agrees_with_merge_no_flap() {
        // refresh must only install a snapshot that would also WIN the merge — else two devices ping-pong.
        let mut list = CommunityList { entries: vec![entry("X", 0, 0, 100)], tombstones: vec![] };
        // current starts at bundle("X",0,"cur0"). A higher epoch always wins (rekey follow).
        assert!(list.refresh_current("X", bundle("X", 1, "cur1")));
        // A same-epoch snapshot that LOSES the canonical tiebreak must be rejected (it would flap otherwise).
        let mut lower = bundle("X", 1, "cur1"); lower.name = "zzz_loses".into();
        assert!(!list.refresh_current("X", lower), "a same-epoch snapshot that loses the merge order is not installed");
    }

    #[test]
    fn is_ahead_of_only_when_local_has_more() {
        let relay = CommunityList { entries: vec![entry("A", 0, 0, 100)], tombstones: vec![] };
        // Identical → not ahead → boot stays read-only (no pointless write).
        let same = CommunityList { entries: vec![entry("A", 0, 0, 100)], tombstones: vec![] };
        assert!(!same.is_ahead_of(&relay), "in-sync local must not trigger a boot publish");
        // Local holds a community the relay lacks (a backfill discovery / unpropagated join) → ahead.
        let more = CommunityList { entries: vec![entry("A", 0, 0, 100), entry("B", 0, 0, 100)], tombstones: vec![] };
        assert!(more.is_ahead_of(&relay), "a local-only membership must trigger a publish");
        // Local holds a tombstone the relay lacks (an unpropagated leave) → ahead.
        let mut tomb = same.clone();
        tomb.remove("A", 200);
        assert!(tomb.is_ahead_of(&relay), "a local-only removal must trigger a publish");
        // Relay strictly ahead of local (it knows a community we don't yet) → we are NOT ahead.
        assert!(!relay.is_ahead_of(&more), "when the relay knows more, boot must not write");
    }

    #[test]
    fn entry_seed_carries_channel_keys_and_survives_json() {
        // The crux of cross-device rehydration: the seed must carry the channel keys (channel pseudonyms
        // derive from them, so a fresh device can't even fetch a channel without them).
        let community = super::super::Community::create("HQ", "general", vec!["wss://r1".into()]);
        let want_key = crate::simd::hex::bytes_to_hex_32(community.channels[0].key.as_bytes());
        let e = CommunityListEntry::from_community(&community, 1234);
        assert_eq!(e.seed.channels.len(), 1, "seed carries the channel");
        assert_eq!(e.seed.channels[0].key, want_key, "seed carries the channel KEY, not just the id");

        // It must survive the wire (NIP-44 content is just this JSON).
        let list = CommunityList { entries: vec![e.clone()], tombstones: vec![] };
        let round = CommunityList::from_json(&list.to_json());
        assert_eq!(round.entries[0].seed.channels[0].key, want_key);
    }

    #[test]
    fn merge_keeps_the_earliest_seed_bundle_with_its_channel_keys() {
        // Device A joined at epoch 2 (channel keys for epoch 2), device B at epoch 5. The merge must keep
        // A's earlier bundle — it reaches strictly more history (and B can fast-forward from it).
        let mut a_entry = entry("X", 2, 2, 100);
        a_entry.seed.channels = vec![crate::community::invite::InviteChannel {
            id: "chan".into(), key: "earlykey".into(), epoch: 2, name: "general".into(),
        }];
        let mut b_entry = entry("X", 5, 9, 100);
        b_entry.seed.channels = vec![crate::community::invite::InviteChannel {
            id: "chan".into(), key: "latekey".into(), epoch: 5, name: "general".into(),
        }];
        let a = CommunityList { entries: vec![a_entry], tombstones: vec![] };
        let b = CommunityList { entries: vec![b_entry], tombstones: vec![] };
        let m = a.merge(&b);
        assert_eq!(m.entries[0].seed_epoch(), 2, "earliest seed bundle wins");
        assert_eq!(m.entries[0].seed.channels[0].key, "earlykey", "earliest bundle's channel keys kept");
        assert_eq!(m.entries[0].current_epoch(), 9, "but the freshest current snapshot still wins");
    }

    #[test]
    fn refresh_current_updates_snapshot_on_rekey_and_rename() {
        // The current snapshot drives instant-latest rehydration — it must follow rekeys AND renames.
        let mut list = CommunityList { entries: vec![entry("X", 0, 0, 100)], tombstones: vec![] };
        // Rekey: epoch advances → snapshot updates, returns true.
        assert!(list.refresh_current("X", bundle("X", 1, "epoch1root")));
        assert_eq!(list.entries[0].current_epoch(), 1);
        assert_eq!(list.entries[0].current().server_root_key, "epoch1root");
        // Rename at the SAME epoch: name differs → still updates (the name rides current), returns true.
        let mut renamed = bundle("X", 1, "epoch1root");
        renamed.name = "Renamed".into();
        assert!(list.refresh_current("X", renamed));
        assert_eq!(list.entries[0].current().name, "Renamed");
        // No-op: identical snapshot → returns false (no pointless republish).
        let same = { let mut b = bundle("X", 1, "epoch1root"); b.name = "Renamed".into(); b };
        assert!(!list.refresh_current("X", same));
        // Never regress to an older epoch.
        assert!(!list.refresh_current("X", bundle("X", 0, "oldroot")));
        assert_eq!(list.entries[0].current_epoch(), 1, "a stale lower-epoch snapshot is rejected");
    }
}
