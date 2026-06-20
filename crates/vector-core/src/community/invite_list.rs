//! Cross-device public-invite sync — the encrypted Invite List.
//!
//! A public invite link is a secret capability: the clickable URL embeds a random 32-byte token, and
//! only the minting device retains it (in `community_public_invites`). Other devices fold the community's
//! per-creator locator set (so the mode shows Public) but hold only a ONE-WAY locator, never the token —
//! so a link minted on the PC is invisible on the phone. This module syncs the tokens themselves over a
//! self-encrypted, replaceable per-user list, a sibling to the Community List.
//!
//! The merge is simpler than the Community List's: a token is minted ONCE and never reused, so revocation
//! is terminal (no leave/re-join race). The merged list is the UNION of every token each device minted,
//! minus the union of revocations.

use nostr_sdk::prelude::{Client, EventBuilder, Filter, Kind, NostrSigner, PublicKey, Tag};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use super::list::{CommunityList, COMMUNITY_LIST_D_TAG};
use crate::stored_event::event_kind;

/// Rides the same NIP-78 (kind 30078) parameterized-replaceable, NIP-44-self-encrypted machinery as the
/// Community List, distinguished only by this `d`-tag — so a single REQ fetches both.
pub const INVITE_LIST_D_TAG: &str = "vector/invites";
/// Local mirror of the (merged) list JSON. Holds tombstones too, which must outlive the
/// `community_public_invites` row a revoke deletes (else a stale device resurrects the token).
const LOCAL_INVITE_LIST_KEY: &str = "invite_list_json";
/// UNIX-seconds of our most recent local mutation; a fetch ignores any relay copy older than this so a
/// just-minted (not-yet-propagated) link can't be clobbered by a stale relay. Same guard as the other lists.
const INVITE_LIST_PUBLISHED_AT_KEY: &str = "invite_list_published_at";
const FETCH_TIMEOUT_SECS: u64 = 20;

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// One minted public-invite link. Immutable once minted (the token is the whole secret), so the merge can
/// treat any copy as interchangeable. Timestamps are UNIX seconds, matching `community_public_invites`.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq, Debug)]
pub struct InviteEntry {
    /// Hex token — the link's secret AND the merge key (globally unique, never reused).
    pub token: String,
    pub community_id: String,
    pub url: String,
    #[serde(default)]
    pub label: Option<String>,
    pub created_at: u64,
    #[serde(default)]
    pub expires_at: Option<u64>,
}

/// A revocation tombstone — keeps a stale device from resurrecting a link you revoked. Terminal: a token is
/// minted once, so a tombstone is permanent (no re-mint can outdate it). Carries `community_id` so a remote
/// revocation can refresh the right open panel even when it removed the community's last link.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq, Debug)]
pub struct InviteRevocation {
    pub token: String,
    #[serde(default)]
    pub community_id: String,
    pub revoked_at: u64,
}

/// The whole synced list: live links + revocation tombstones.
#[derive(Clone, Serialize, Deserialize, Default, Debug, PartialEq, Eq)]
pub struct InviteList {
    #[serde(default)]
    pub entries: Vec<InviteEntry>,
    #[serde(default)]
    pub tombstones: Vec<InviteRevocation>,
}

impl InviteList {
    pub fn from_json(s: &str) -> Self {
        serde_json::from_str(s).unwrap_or_default()
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".to_string())
    }

    pub fn contains(&self, token: &str) -> bool {
        self.entries.iter().any(|e| e.token == token)
    }

    /// ADD a freshly-minted link locally (call before republishing). A token is minted once, so a
    /// re-`upsert` of the same token is a no-op (the entry is immutable). A token already tombstoned can't
    /// be resurrected — revocation is terminal.
    pub fn upsert(&mut self, entry: InviteEntry) {
        if self.tombstones.iter().any(|t| t.token == entry.token) {
            return;
        }
        if !self.entries.iter().any(|e| e.token == entry.token) {
            self.entries.push(entry);
        }
    }

    /// REVOKE a link locally: drop the entry + leave a tombstone so a stale device can't resurrect it.
    pub fn revoke(&mut self, token: &str, community_id: &str, revoked_at: u64) {
        self.entries.retain(|e| e.token != token);
        match self.tombstones.iter_mut().find(|t| t.token == token) {
            Some(t) => t.revoked_at = t.revoked_at.max(revoked_at),
            None => self.tombstones.push(InviteRevocation {
                token: token.to_string(),
                community_id: community_id.to_string(),
                revoked_at,
            }),
        }
    }

    /// True iff publishing `self` would add something the `relay` copy lacks — a token it doesn't list, or a
    /// revocation it hasn't seen. Boot writes ONLY when genuinely ahead, so two devices booting don't clobber
    /// each other's just-minted links by timestamp.
    pub fn is_ahead_of(&self, relay: &InviteList) -> bool {
        relay.merge(self) != relay.merge(&InviteList::default())
    }

    /// READ-MERGE-WRITE conflict resolution across devices. Tokens are mint-once, so this is a clean union:
    /// every distinct token from either side survives, every revocation is unioned (newest `revoked_at`
    /// wins, though the value is cosmetic), and a tombstoned token is dropped from the live set — terminally,
    /// since no re-mint can ever outdate the revocation. Deterministic: identical bytes on every device.
    pub fn merge(&self, other: &InviteList) -> InviteList {
        // Union live links by token (immutable, so first-seen wins; collisions are impossible).
        let mut adds: HashMap<String, InviteEntry> = HashMap::new();
        for e in self.entries.iter().chain(other.entries.iter()) {
            adds.entry(e.token.clone()).or_insert_with(|| e.clone());
        }
        // Union revocations by token (keep the newest stamp; the community_id is stable per token).
        let mut rms: HashMap<String, (String, u64)> = HashMap::new();
        for t in self.tombstones.iter().chain(other.tombstones.iter()) {
            rms.entry(t.token.clone())
                .and_modify(|(_cid, ra)| *ra = (*ra).max(t.revoked_at))
                .or_insert_with(|| (t.community_id.clone(), t.revoked_at));
        }
        // A revoked token is dropped from the live set (terminal).
        let mut entries: Vec<InviteEntry> =
            adds.into_values().filter(|e| !rms.contains_key(&e.token)).collect();
        let mut tombstones: Vec<InviteRevocation> = rms
            .into_iter()
            .map(|(token, (community_id, revoked_at))| InviteRevocation { token, community_id, revoked_at })
            .collect();
        // Stable order → deterministic serialized bytes across devices.
        entries.sort_by(|a, b| a.token.cmp(&b.token));
        tombstones.sort_by(|a, b| a.token.cmp(&b.token));
        InviteList { entries, tombstones }
    }

    /// Live links for one community (drives hydration of `community_public_invites`).
    pub fn for_community<'a>(&'a self, community_id: &'a str) -> impl Iterator<Item = &'a InviteEntry> {
        self.entries.iter().filter(move |e| e.community_id == community_id)
    }
}

// ============================================================================
// Local mirror (account-scoped settings)
// ============================================================================

pub fn load_local_invite_list() -> InviteList {
    crate::db::settings::get_sql_setting(LOCAL_INVITE_LIST_KEY.to_string())
        .ok()
        .flatten()
        .map(|s| InviteList::from_json(&s))
        .unwrap_or_default()
}

pub fn save_local_invite_list(list: &InviteList) -> Result<(), String> {
    crate::db::settings::set_sql_setting(LOCAL_INVITE_LIST_KEY.to_string(), list.to_json())
}

fn stamp_published_now() {
    let _ = crate::db::settings::set_sql_setting(
        INVITE_LIST_PUBLISHED_AT_KEY.to_string(),
        now_secs().to_string(),
    );
}

fn our_last_publish() -> u64 {
    crate::db::settings::get_sql_setting(INVITE_LIST_PUBLISHED_AT_KEY.to_string())
        .ok()
        .flatten()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

// ============================================================================
// Transport (NIP-44-self-encrypted kind 30078, parameterized-replaceable)
// ============================================================================

/// Decrypt a fetched self-list event to its JSON plaintext; a malformed/undecryptable payload degrades to
/// empty (never an error that aborts a reconcile), matching the Community List's posture.
async fn decrypt_self_event(client: &Client, my_pk: &PublicKey, event: &nostr_sdk::prelude::Event) -> String {
    if event.content.is_empty() {
        return String::new();
    }
    let signer = match client.signer().await {
        Ok(s) => s,
        Err(e) => {
            crate::log_warn!("[InviteList] signer unavailable for decrypt: {}", e);
            return String::new();
        }
    };
    signer.nip44_decrypt(my_pk, &event.content).await.unwrap_or_else(|e| {
        crate::log_warn!("[InviteList] decrypt failed: {}", e);
        String::new()
    })
}

/// Fetch BOTH self-lists (Community + Invite) in a SINGLE REQ — they're kind-30078 self-events differing
/// only by `d`-tag, so one filter matches both. Each half degrades to its local mirror on a miss or a relay
/// copy that predates our last publish (our republish hasn't propagated). Session-guarded.
pub async fn fetch_self_lists(
    client: &Client,
    my_pubkey: PublicKey,
    session: crate::state::SessionGuard,
) -> (CommunityList, InviteList) {
    let filter = Filter::new()
        .author(my_pubkey)
        .kind(Kind::Custom(event_kind::APPLICATION_SPECIFIC))
        .identifiers([COMMUNITY_LIST_D_TAG.to_string(), INVITE_LIST_D_TAG.to_string()])
        .limit(2);

    let events = client
        .fetch_events(filter, std::time::Duration::from_secs(FETCH_TIMEOUT_SECS))
        .await
        .unwrap_or_default();

    if !session.is_valid() {
        return (super::list::load_local_list(), load_local_invite_list());
    }

    // Latest event per d-tag.
    let mut community_ev: Option<nostr_sdk::prelude::Event> = None;
    let mut invite_ev: Option<nostr_sdk::prelude::Event> = None;
    for ev in events {
        let slot = match ev.tags.identifier() {
            Some(COMMUNITY_LIST_D_TAG) => &mut community_ev,
            Some(INVITE_LIST_D_TAG) => &mut invite_ev,
            _ => continue,
        };
        if slot.as_ref().map_or(true, |cur| ev.created_at > cur.created_at) {
            *slot = Some(ev);
        }
    }

    let community = match community_ev {
        Some(ev) if ev.created_at.as_secs() < super::list::our_last_community_publish() => {
            super::list::load_local_list()
        }
        Some(ev) => CommunityList::from_json(&decrypt_self_event(client, &my_pubkey, &ev).await),
        None => super::list::load_local_list(),
    };
    let invite = match invite_ev {
        Some(ev) if ev.created_at.as_secs() < our_last_publish() => load_local_invite_list(),
        Some(ev) => InviteList::from_json(&decrypt_self_event(client, &my_pubkey, &ev).await),
        None => load_local_invite_list(),
    };
    (community, invite)
}

/// Fetch only our Invite List (single d-tag) — the read half of a publish read-merge-write.
pub async fn fetch_invite_list(
    client: &Client,
    my_pubkey: PublicKey,
    session: crate::state::SessionGuard,
) -> InviteList {
    let filter = Filter::new()
        .author(my_pubkey)
        .kind(Kind::Custom(event_kind::APPLICATION_SPECIFIC))
        .identifier(INVITE_LIST_D_TAG)
        .limit(1);
    let events = client
        .fetch_events(filter, std::time::Duration::from_secs(FETCH_TIMEOUT_SECS))
        .await
        .unwrap_or_default();
    if !session.is_valid() {
        return load_local_invite_list();
    }
    match events.into_iter().max_by_key(|e| e.created_at) {
        Some(ev) if ev.created_at.as_secs() < our_last_publish() => load_local_invite_list(),
        Some(ev) => InviteList::from_json(&decrypt_self_event(client, &my_pubkey, &ev).await),
        None => load_local_invite_list(),
    }
}

/// READ-MERGE-WRITE publish: fold the relay's copy into our local mirror (so a concurrent mint from another
/// device survives), persist, hydrate the read model, then publish self-encrypted.
pub async fn publish_invite_list(
    client: &Client,
    session: crate::state::SessionGuard,
) -> Result<(), String> {
    let my_pk = crate::state::my_public_key().ok_or_else(|| "Not logged in".to_string())?;

    let relay = fetch_invite_list(client, my_pk, session.clone()).await;
    if !session.is_valid() {
        return Ok(());
    }
    let merged = load_local_invite_list().merge(&relay);
    save_local_invite_list(&merged)?;
    hydrate_read_model(&merged);

    let signer = client.signer().await.map_err(|e| format!("Signer unavailable: {}", e))?;
    let content = signer
        .nip44_encrypt(&my_pk, &merged.to_json())
        .await
        .map_err(|e| format!("nip44 encrypt invite list: {}", e))?;
    let builder = EventBuilder::new(Kind::Custom(event_kind::APPLICATION_SPECIFIC), content)
        .tag(Tag::identifier(INVITE_LIST_D_TAG));
    client
        .send_event_builder(builder)
        .await
        .map_err(|e| format!("Failed to publish invite list (kind 30078): {}", e))?;
    crate::log_info!(
        "[InviteList] Published encrypted list: {} link(s), {} tombstone(s)",
        merged.entries.len(), merged.tombstones.len(),
    );
    Ok(())
}

/// Consume a remotely-received invite-list event (live cross-device path): decrypt, fold into the local
/// mirror, persist, hydrate the read model. Does NOT republish (the relay echoes our own publishes back).
pub async fn ingest_remote_invite_list_event(
    client: &Client,
    my_pk: &PublicKey,
    event: &nostr_sdk::prelude::Event,
    session: crate::state::SessionGuard,
) -> Result<InviteList, String> {
    let incoming = InviteList::from_json(&decrypt_self_event(client, my_pk, event).await);
    if !session.is_valid() {
        return Ok(load_local_invite_list());
    }
    let merged = load_local_invite_list().merge(&incoming);
    save_local_invite_list(&merged)?;
    hydrate_read_model(&merged);
    Ok(merged)
}

static REPUBLISH_GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Coalesce rapid mint/revoke mutations into one network publish. Stamps the publish clock + captures the
/// `SessionGuard` BEFORE the debounce sleep, mirroring the Community List.
pub fn republish_invite_list_debounced() {
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
        if let Err(e) = publish_invite_list(&client, session.clone()).await {
            crate::log_warn!("[InviteList] Republish failed: {} (retrying in 5s)", e);
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            if REPUBLISH_GEN.load(Ordering::SeqCst) != gen { return; }
            if !session.is_valid() { return; }
            if let Err(e) = publish_invite_list(&client, session).await {
                crate::log_warn!("[InviteList] Republish retry failed: {}", e);
            }
        }
    });
}

// ============================================================================
// Local mutations (the create/revoke hooks call these, then debounce-republish)
// ============================================================================

/// Record a freshly-minted link in the synced list + schedule a publish.
pub fn add_invite(entry: InviteEntry) {
    let mut list = load_local_invite_list();
    list.upsert(entry);
    if let Err(e) = save_local_invite_list(&list) {
        crate::log_warn!("[InviteList] save after add failed: {}", e);
        return;
    }
    republish_invite_list_debounced();
}

/// Tombstone a revoked link in the synced list + schedule a publish so our other devices drop it too.
pub fn revoke_invite(token: &str, community_id: &str) {
    let mut list = load_local_invite_list();
    list.revoke(token, community_id, now_secs());
    if let Err(e) = save_local_invite_list(&list) {
        crate::log_warn!("[InviteList] save after revoke failed: {}", e);
        return;
    }
    republish_invite_list_debounced();
}

// ============================================================================
// Read model <-> synced list bridge
// ============================================================================

/// Push the merged list into `community_public_invites` (the table the GUI lists from): insert tokens we
/// didn't have, delete tokens that were revoked elsewhere. Idempotent.
pub fn hydrate_read_model(list: &InviteList) {
    for e in &list.entries {
        if let Err(err) = crate::db::community::upsert_public_invite(
            &e.token, &e.community_id, &e.url, e.expires_at.map(|x| x as i64), e.created_at as i64, e.label.as_deref(),
        ) {
            crate::log_warn!("[InviteList] hydrate insert failed for {}: {}", e.token, err);
        }
    }
    for t in &list.tombstones {
        if let Err(err) = crate::db::community::delete_public_invite(&t.token) {
            crate::log_warn!("[InviteList] hydrate delete failed for {}: {}", t.token, err);
        }
    }
}

/// Seed the synced list from links ALREADY in the local table but missing from it — links minted before
/// this feature shipped, or on a device that predates it. Skips tombstoned tokens (don't resurrect a
/// revoked link). Returns true if anything changed. Does NOT republish itself (the boot publish carries it).
pub fn backfill_from_db() -> bool {
    let records = match crate::db::community::list_all_public_invites() {
        Ok(r) => r,
        Err(e) => {
            crate::log_warn!("[InviteList] backfill: list_all_public_invites failed: {}", e);
            return false;
        }
    };
    let mut list = load_local_invite_list();
    let mut changed = false;
    for r in records {
        if list.contains(&r.token) || list.tombstones.iter().any(|t| t.token == r.token) {
            continue;
        }
        list.upsert(InviteEntry {
            token: r.token,
            community_id: r.community_id,
            url: r.url,
            label: r.label,
            created_at: r.created_at.max(0) as u64,
            expires_at: r.expires_at.map(|x| x.max(0) as u64),
        });
        changed = true;
    }
    if changed {
        if let Err(e) = save_local_invite_list(&list) {
            crate::log_warn!("[InviteList] backfill save failed: {}", e);
            return false;
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(token: &str, cid: &str) -> InviteEntry {
        InviteEntry {
            token: token.to_string(),
            community_id: cid.to_string(),
            url: format!("https://vector/{token}"),
            label: None,
            created_at: 100,
            expires_at: None,
        }
    }

    #[test]
    fn merge_unions_links_from_two_devices() {
        let mut a = InviteList::default();
        a.upsert(entry("aa", "c1"));
        let mut b = InviteList::default();
        b.upsert(entry("bb", "c1"));
        let merged = a.merge(&b);
        assert_eq!(merged.entries.len(), 2);
        assert!(merged.contains("aa") && merged.contains("bb"));
    }

    #[test]
    fn merge_dedups_same_token() {
        let mut a = InviteList::default();
        a.upsert(entry("aa", "c1"));
        let b = a.clone();
        let merged = a.merge(&b);
        assert_eq!(merged.entries.len(), 1);
    }

    #[test]
    fn revocation_is_terminal_across_merge() {
        let mut a = InviteList::default();
        a.upsert(entry("aa", "c1"));
        // Device B revokes the same token.
        let mut b = InviteList::default();
        b.revoke("aa", "c1", 200);
        let merged = a.merge(&b);
        assert!(!merged.contains("aa"), "revoked link must not survive the merge");
        assert_eq!(merged.tombstones.len(), 1);
        // A re-upsert of a tombstoned token can't resurrect it.
        let mut m = merged.clone();
        m.upsert(entry("aa", "c1"));
        assert!(!m.contains("aa"));
    }

    #[test]
    fn merge_is_order_independent_and_idempotent() {
        let mut a = InviteList::default();
        a.upsert(entry("aa", "c1"));
        a.revoke("zz", "c1", 5);
        let mut b = InviteList::default();
        b.upsert(entry("bb", "c2"));
        let ab = a.merge(&b);
        let ba = b.merge(&a);
        assert_eq!(ab, ba, "merge must be commutative (deterministic bytes)");
        assert_eq!(ab, ab.merge(&a), "merge must be idempotent");
        assert_eq!(ab.to_json(), ba.to_json());
    }

    #[test]
    fn revoke_drops_entry_and_tombstones() {
        let mut a = InviteList::default();
        a.upsert(entry("aa", "c1"));
        a.revoke("aa", "c1", 200);
        assert!(!a.contains("aa"));
        assert_eq!(a.tombstones.len(), 1);
    }

    #[test]
    fn is_ahead_detects_new_token() {
        let mut local = InviteList::default();
        local.upsert(entry("aa", "c1"));
        assert!(local.is_ahead_of(&InviteList::default()));
        assert!(!InviteList::default().is_ahead_of(&local));
    }
}
