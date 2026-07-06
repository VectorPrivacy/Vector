//! CORD-02 §8: the Community List — a member's memberships, synced across
//! devices and clients as one self-encrypted kind 13302 replaceable.
//!
//! Two snapshots per membership solve opposite problems: `seed` holds the
//! *earliest* epoch you ever held (the anchor for full-history backfill) and
//! only ever moves backward on merge; `current` holds the *latest* (a fresh
//! device reconstructs instantly and just pages) and only ever moves forward.
//! Tombstones are permanent — liveness is derived, never deletion.

use nostr_sdk::prelude::*;
use serde::{Deserialize, Serialize};

use super::invite::{ChannelGrant, CommunityInvite, InviteError};
use super::{kind, Epoch, COMMUNITY_LIST_MAX_MEMBERSHIPS, NIP44_MAX_PLAINTEXT};

/// Join material: the bundle's *membership* subset — never the icon (a
/// rehydrating device folds it from the Control Plane) and never the link
/// fields (expiry and attribution belong to the invite, not the membership).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JoinMaterial {
    pub community_id: String,
    pub owner: String,
    pub owner_salt: String,
    pub community_root: String,
    pub root_epoch: u64,
    #[serde(default)]
    pub channels: Vec<ChannelGrant>,
    #[serde(default)]
    pub relays: Vec<String>,
    #[serde(default)]
    pub name: String,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl JoinMaterial {
    /// Extract the membership subset from a validated bundle.
    pub fn from_bundle(bundle: &CommunityInvite) -> Self {
        JoinMaterial {
            community_id: bundle.community_id.clone(),
            owner: bundle.owner.clone(),
            owner_salt: bundle.owner_salt.clone(),
            community_root: bundle.community_root.clone(),
            root_epoch: bundle.root_epoch,
            channels: bundle.channels.clone(),
            relays: bundle.relays.clone(),
            name: bundle.name.clone(),
            extra: Default::default(),
        }
    }

    /// The canonical bytes an epoch tie breaks on — a total order, so a
    /// same-epoch rename can't leave two devices flapping.
    fn canonical(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommunityListEntry {
    pub community_id: String,
    /// The earliest epoch held: only ever moves BACKward on merge.
    pub seed: JoinMaterial,
    /// The freshest snapshot: replaced on every Refounding or rename.
    pub current: JoinMaterial,
    /// Unix ms; tiebreaks against a tombstone (newest wins).
    pub added_at: u64,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ListTombstone {
    pub community_id: String,
    pub removed_at: u64,
}

/// The kind 13302 document: every Community you're in and every one you've
/// left. A tombstoned entry stays *in* the document, or merges would depend
/// on gossip order.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CommunityList {
    #[serde(default)]
    pub entries: Vec<CommunityListEntry>,
    #[serde(default)]
    pub tombstones: Vec<ListTombstone>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// Merge direction for one snapshot slot.
fn merge_snapshot(mine: &mut JoinMaterial, theirs: &JoinMaterial, keep_lower_epoch: bool) {
    let replace = match theirs.root_epoch.cmp(&mine.root_epoch) {
        std::cmp::Ordering::Less => keep_lower_epoch,
        std::cmp::Ordering::Greater => !keep_lower_epoch,
        // Epoch tie: the lexicographically lowest canonical bytes win.
        std::cmp::Ordering::Equal => theirs.canonical() < mine.canonical(),
    };
    if replace {
        *mine = theirs.clone();
    }
}

impl CommunityList {
    /// Whether a membership is live: the newest of `added_at` and
    /// `removed_at` wins, so a re-join legitimately resurrects while a
    /// backfill can never re-add a tombstoned id.
    pub fn is_live(&self, community_id: &str) -> bool {
        let Some(entry) = self.entries.iter().find(|e| e.community_id == community_id) else {
            return false;
        };
        match self.tombstones.iter().find(|t| t.community_id == community_id) {
            Some(tomb) => entry.added_at > tomb.removed_at,
            None => true,
        }
    }

    pub fn live_entries(&self) -> impl Iterator<Item = &CommunityListEntry> {
        self.entries.iter().filter(|e| self.is_live(&e.community_id))
    }

    /// Record a (re-)join. Seed keeps the earliest epoch, current the
    /// freshest; `added_at` moves forward only.
    pub fn add_membership(&mut self, material: JoinMaterial, added_at_ms: u64) {
        match self.entries.iter_mut().find(|e| e.community_id == material.community_id) {
            Some(entry) => {
                merge_snapshot(&mut entry.seed, &material, true);
                merge_snapshot(&mut entry.current, &material, false);
                entry.added_at = entry.added_at.max(added_at_ms);
            }
            None => self.entries.push(CommunityListEntry {
                community_id: material.community_id.clone(),
                seed: material.clone(),
                current: material,
                added_at: added_at_ms,
                extra: Default::default(),
            }),
        }
        self.entries.sort_by(|a, b| a.community_id.cmp(&b.community_id));
    }

    /// Record a leave. Permanent: pruning would let a long-offline device
    /// resurrect a Community you left.
    pub fn tombstone(&mut self, community_id: &str, removed_at_ms: u64) {
        match self.tombstones.iter_mut().find(|t| t.community_id == community_id) {
            Some(tomb) => tomb.removed_at = tomb.removed_at.max(removed_at_ms),
            None => self.tombstones.push(ListTombstone {
                community_id: community_id.to_string(),
                removed_at: removed_at_ms,
            }),
        }
        self.tombstones.sort_by(|a, b| a.community_id.cmp(&b.community_id));
    }

    /// Merge another device's copy. Commutative, associative, idempotent —
    /// two clients serving one npub converge without coordination.
    pub fn merge(&mut self, other: &CommunityList) {
        for theirs in &other.entries {
            match self.entries.iter_mut().find(|e| e.community_id == theirs.community_id) {
                Some(mine) => {
                    merge_snapshot(&mut mine.seed, &theirs.seed, true);
                    merge_snapshot(&mut mine.current, &theirs.current, false);
                    mine.added_at = mine.added_at.max(theirs.added_at);
                }
                None => self.entries.push(theirs.clone()),
            }
        }
        for tomb in &other.tombstones {
            self.tombstone(&tomb.community_id, tomb.removed_at);
        }
        self.entries.sort_by(|a, b| a.community_id.cmp(&b.community_id));
    }

    /// The publish gate: the membership cap bounds the common case, the byte
    /// cap is the law — a client MUST verify the serialized List fits (join
    /// material carrying private-channel keys can overflow well below 50).
    pub fn validate_for_publish(&self) -> Result<String, InviteError> {
        if self.entries.len() > COMMUNITY_LIST_MAX_MEMBERSHIPS {
            return Err(InviteError::Malformed(format!(
                "community list holds {} memberships (cap {COMMUNITY_LIST_MAX_MEMBERSHIPS})",
                self.entries.len()
            )));
        }
        let json = serde_json::to_string(self).map_err(|e| InviteError::Malformed(e.to_string()))?;
        if json.len() > NIP44_MAX_PLAINTEXT {
            return Err(InviteError::Malformed("community list exceeds the NIP-44 plaintext cap".into()));
        }
        Ok(json)
    }

    pub fn seed_epoch(&self, community_id: &str) -> Option<Epoch> {
        self.entries
            .iter()
            .find(|e| e.community_id == community_id)
            .map(|e| Epoch(e.seed.root_epoch))
    }

    pub fn current_epoch(&self, community_id: &str) -> Option<Epoch> {
        self.entries
            .iter()
            .find(|e| e.community_id == community_id)
            .map(|e| Epoch(e.current.root_epoch))
    }
}

/// Publish form: kind 13302, replaceable, signed by the real key,
/// NIP-44-encrypted to self.
pub fn build_community_list_event(
    keys: &Keys,
    list: &CommunityList,
    created_at_secs: u64,
) -> Result<Event, InviteError> {
    let json = list.validate_for_publish()?;
    let ct = nostr_sdk::nips::nip44::encrypt(
        keys.secret_key(),
        &keys.public_key(),
        &json,
        nostr_sdk::nips::nip44::Version::V2,
    )
    .map_err(|e| InviteError::Crypto(e.to_string()))?;
    EventBuilder::new(Kind::Custom(kind::COMMUNITY_LIST), ct)
        .custom_created_at(Timestamp::from_secs(created_at_secs))
        .sign_with_keys(keys)
        .map_err(|e| InviteError::Crypto(e.to_string()))
}

pub fn open_community_list_event(keys: &Keys, event: &Event) -> Result<CommunityList, InviteError> {
    let json = nostr_sdk::nips::nip44::decrypt(keys.secret_key(), &keys.public_key(), &event.content)
        .map_err(|e| InviteError::Crypto(e.to_string()))?;
    serde_json::from_str(&json).map_err(|e| InviteError::Malformed(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn material(cid: &str, epoch: u64, name: &str) -> JoinMaterial {
        JoinMaterial {
            community_id: cid.into(),
            owner: "owner".into(),
            owner_salt: "salt".into(),
            community_root: format!("root-{epoch}"),
            root_epoch: epoch,
            channels: vec![],
            relays: vec!["wss://a".into()],
            name: name.into(),
            extra: Default::default(),
        }
    }

    #[test]
    fn seed_moves_backward_current_moves_forward() {
        let mut list = CommunityList::default();
        list.add_membership(material("c1", 3, "V"), 1_000);
        // A refounding hands epoch 4: current advances, seed stays.
        list.add_membership(material("c1", 4, "V"), 2_000);
        assert_eq!(list.seed_epoch("c1"), Some(Epoch(3)));
        assert_eq!(list.current_epoch("c1"), Some(Epoch(4)));
        // A backfilled older epoch: seed rewinds, current stays.
        list.add_membership(material("c1", 1, "V"), 500);
        assert_eq!(list.seed_epoch("c1"), Some(Epoch(1)));
        assert_eq!(list.current_epoch("c1"), Some(Epoch(4)));
        assert_eq!(list.entries[0].added_at, 2_000, "added_at moves forward only");
    }

    #[test]
    fn same_epoch_tie_breaks_on_canonical_bytes() {
        let a = material("c1", 2, "Alpha");
        let b = material("c1", 2, "Beta");
        let winner = if a.canonical() < b.canonical() { "Alpha" } else { "Beta" };

        // Both merge orders land every device on the same current.
        let mut dev1 = CommunityList::default();
        dev1.add_membership(a.clone(), 1);
        dev1.add_membership(b.clone(), 2);
        let mut dev2 = CommunityList::default();
        dev2.add_membership(b, 1);
        dev2.add_membership(a, 2);
        assert_eq!(dev1.entries[0].current.name, winner);
        assert_eq!(dev2.entries[0].current.name, winner);
    }

    #[test]
    fn tombstones_are_permanent_and_rejoin_resurrects() {
        let mut list = CommunityList::default();
        list.add_membership(material("c1", 0, "V"), 1_000);
        assert!(list.is_live("c1"));

        list.tombstone("c1", 2_000);
        assert!(!list.is_live("c1"));
        assert_eq!(list.entries.len(), 1, "a tombstoned entry stays in the document");

        // A backfill replaying the old add can never re-add it.
        list.add_membership(material("c1", 0, "V"), 1_000);
        assert!(!list.is_live("c1"));

        // A genuine re-join (newer added_at) resurrects.
        list.add_membership(material("c1", 0, "V"), 3_000);
        assert!(list.is_live("c1"));
    }

    #[test]
    fn device_merge_converges_regardless_of_order() {
        let mut a = CommunityList::default();
        a.add_membership(material("c1", 2, "V"), 1_000);
        a.add_membership(material("c2", 0, "W"), 1_500);
        let mut b = CommunityList::default();
        b.add_membership(material("c1", 5, "V-renamed"), 3_000);
        b.tombstone("c2", 2_000);

        let mut ab = a.clone();
        ab.merge(&b);
        let mut ba = b.clone();
        ba.merge(&a);
        assert_eq!(ab, ba, "merge is commutative");
        assert_eq!(ab.seed_epoch("c1"), Some(Epoch(2)));
        assert_eq!(ab.current_epoch("c1"), Some(Epoch(5)));
        assert!(!ab.is_live("c2"));
        // Idempotent.
        let snapshot = ab.clone();
        ab.merge(&snapshot.clone());
        assert_eq!(ab, snapshot);
    }

    #[test]
    fn publish_gates_membership_and_byte_caps() {
        let mut list = CommunityList::default();
        for i in 0..COMMUNITY_LIST_MAX_MEMBERSHIPS {
            list.add_membership(material(&format!("c{i:03}"), 0, "V"), 1);
        }
        assert!(list.validate_for_publish().is_ok());
        list.add_membership(material("c-one-more", 0, "V"), 1);
        assert!(list.validate_for_publish().is_err(), "51st membership refused");

        // The byte cap binds below the count cap when entries are heavy.
        let mut heavy = CommunityList::default();
        let mut m = material("c-heavy", 0, "V");
        m.channels = (0..200)
            .map(|i| ChannelGrant {
                id: crate::simd::hex::bytes_to_hex_32(&[i as u8; 32]),
                key: Some(crate::simd::hex::bytes_to_hex_32(&[i as u8; 32])),
                epoch: 1,
                name: "x".repeat(60),
            })
            .collect();
        for i in 0..3 {
            let mut mi = m.clone();
            mi.community_id = format!("c-heavy-{i}");
            heavy.add_membership(mi, 1);
        }
        assert!(heavy.validate_for_publish().is_err(), "the byte cap is the law");
    }

    #[test]
    fn event_roundtrip_and_unknown_fields_survive() {
        let keys = Keys::generate();
        let mut list = CommunityList::default();
        list.add_membership(material("c1", 0, "V"), 1_000);
        list.extra.insert("future_field".into(), serde_json::json!(42));
        let event = build_community_list_event(&keys, &list, 1_722_400_000).unwrap();
        assert_eq!(event.kind.as_u16(), kind::COMMUNITY_LIST);
        let back = open_community_list_event(&keys, &event).unwrap();
        assert_eq!(back, list);
        assert_eq!(back.extra["future_field"], 42);
        assert!(open_community_list_event(&Keys::generate(), &event).is_err());
    }
}
