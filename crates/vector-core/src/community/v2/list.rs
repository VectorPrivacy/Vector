//! The Community List — CORD-02 §8 (kind 13302).
//!
//! A member's own memberships sync across their devices *and* their clients as
//! one self-encrypted, replaceable event: every Community they're in and every
//! one they've left, in a single NIP-44-to-self document. Liveness is DERIVED,
//! never deletion — a tombstoned entry stays IN the document, or two devices'
//! merges would depend on gossip order.
//!
//! Per entry two snapshots solve opposite problems. `seed` holds the EARLIEST
//! epoch ever held (the full-history backfill anchor; only ever moves BACKWARD
//! on merge) and `current` the LATEST (instant reconstruction on a fresh
//! device; replaced on every Refounding or rename). The merges mirror each
//! other — `seed` keeps the lower epoch, `current` the higher — and an epoch
//! TIE breaks on the lexicographically lowest [`canonical_json`] bytes of the
//! whole join material, a total order so two devices can't flap competing
//! same-epoch republishes.
//!
//! Tombstones are per-Community, timestamped, and PERMANENT (pruning would let
//! a long-offline device resurrect a Community you left). The newest of
//! `added_at` / `removed_at` decides liveness: a re-join legitimately
//! resurrects a membership, while a backfill can never re-add a tombstoned id.
//!
//! This module is pure merge algebra — no DB, no network. Two on-read rules the
//! caller owns: (1) on receiving a remote list you MERGE, never replace, or a
//! stale device wipes a sibling's change; (2) a decrypt failure must never
//! clobber a populated local list — treat an unreadable event as "no news".

use nostr_sdk::nips::nip44::{self, Version};
use nostr_sdk::prelude::{Event, EventBuilder, Keys, Kind, PublicKey};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use super::kind;
use super::stream;

/// The membership cap (CORD-02 §8). Bounds the common case; the NIP-44 byte cap
/// ([`stream::NIP44_MAX_PLAINTEXT`]) is the actual law — join material carrying
/// private-channel keys can overflow the event well below 50.
pub const MAX_MEMBERSHIPS: usize = 50;

/// Join material — the invite bundle's MEMBERSHIP subset (CORD-02 §8): never
/// the icon (a rehydrating device folds it from the Control Plane), never the
/// link fields (expiry/attribution belong to the invite, not the membership).
///
/// Field names are the cross-client wire contract — they must match every other
/// Concord client byte-for-byte or a rehydrate silently drops keys. `extra`
/// round-trips armada's `held_roots` / `refounder` extensions and any future
/// unknown field (CORD-02 §6 round-trip discipline).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinMaterial {
    pub community_id: String,
    pub owner: String,
    pub owner_salt: String,
    pub community_root: String,
    pub root_epoch: u64,
    /// The PRIVATE channels held (public ones derive from the root — CORD-03).
    pub channels: Vec<ChannelKeyRef>,
    pub relays: Vec<String>,
    pub name: String,
    /// Round-tripped verbatim: `held_roots`, `refounder`, and anything a peer
    /// client added that this one doesn't model.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// A private Channel's key reference inside join material (CORD-03). Shape is
/// pinned to armada's inline `{id,key,epoch,name}` — a mismatch breaks the
/// cross-client rehydrate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelKeyRef {
    pub id: String,
    pub key: String,
    pub epoch: u64,
    pub name: String,
}

/// One membership: the community id plus its two snapshots and the add time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommunityListEntry {
    pub community_id: String,
    /// Earliest epoch held — only ever moves BACKWARD on merge.
    pub seed: JoinMaterial,
    /// Freshest snapshot — replaced on every Refounding or rename.
    pub current: JoinMaterial,
    /// ms; tiebreaks against a tombstone (newest of add / removal wins).
    pub added_at: u64,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// A permanent per-Community tombstone. Stays in the document forever.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tombstone {
    pub community_id: String,
    /// ms. Permanent — pruning would let a long-offline device resurrect a leave.
    pub removed_at: u64,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// The whole document: memberships and tombstones, both kept (liveness derived).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct CommunityList {
    #[serde(default)]
    pub entries: Vec<CommunityListEntry>,
    #[serde(default)]
    pub tombstones: Vec<Tombstone>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// Errors from building or parsing the 13302 event / enforcing the caps.
#[derive(Debug)]
pub enum ListError {
    Json(String),
    Nip44(String),
    Sign(String),
    /// The event isn't kind 13302.
    WrongKind(u16),
    /// The live membership set exceeds [`MAX_MEMBERSHIPS`].
    TooManyMemberships(usize),
    /// The serialized (pre-encryption) list exceeds the NIP-44 plaintext cap.
    Oversize(usize),
}

impl std::fmt::Display for ListError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ListError::Json(e) => write!(f, "json: {e}"),
            ListError::Nip44(e) => write!(f, "nip44: {e}"),
            ListError::Sign(e) => write!(f, "sign: {e}"),
            ListError::WrongKind(k) => write!(f, "not a community-list event: kind {k}"),
            ListError::TooManyMemberships(n) => write!(f, "{n} memberships exceeds the {MAX_MEMBERSHIPS} cap"),
            ListError::Oversize(n) => write!(f, "serialized list {n} bytes exceeds the NIP-44 plaintext cap"),
        }
    }
}

impl std::error::Error for ListError {}

// ── Canonical JSON — the epoch-tie total order ───────────────────────────────

/// Serialize a value with recursively lexicographically-sorted object keys,
/// arrays in order, and no insignificant whitespace — the deterministic byte
/// string that breaks equal-epoch merge ties identically on every client.
///
/// Keys sort by their UTF-8 bytes, which coincides with JavaScript's default
/// (UTF-16 code-unit) string order for every Basic-Multilingual-Plane key — and
/// Concord field names are ASCII — so the output matches armada's `canonicalJson`
/// byte-for-byte. Scalars and key strings reuse serde_json's own formatting so
/// number rendering and string escaping stay identical to a plain serialize.
pub fn canonical_json(value: &serde_json::Value) -> String {
    let mut out = String::new();
    canonicalize_into(value, &mut out);
    out
}

fn canonicalize_into(value: &serde_json::Value, out: &mut String) {
    use serde_json::Value;
    match value {
        Value::Object(map) => {
            out.push('{');
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_unstable();
            for (i, key) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::to_string(key).expect("json string key is infallible"));
                out.push(':');
                canonicalize_into(map.get(key).expect("key from map.keys() is present"), out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                canonicalize_into(item, out);
            }
            out.push(']');
        }
        scalar => out.push_str(&scalar.to_string()),
    }
}

fn material_canonical(jm: &JoinMaterial) -> String {
    canonical_json(&serde_json::to_value(jm).expect("JoinMaterial serializes"))
}

/// Higher epoch wins; on a tie, the lexicographically lowest canonical bytes.
fn freshest<'a>(a: &'a JoinMaterial, b: &'a JoinMaterial) -> &'a JoinMaterial {
    if a.root_epoch != b.root_epoch {
        return if a.root_epoch > b.root_epoch { a } else { b };
    }
    if material_canonical(a) <= material_canonical(b) {
        a
    } else {
        b
    }
}

/// Lower epoch wins; on a tie, the lexicographically lowest canonical bytes.
fn earliest<'a>(a: &'a JoinMaterial, b: &'a JoinMaterial) -> &'a JoinMaterial {
    if a.root_epoch != b.root_epoch {
        return if a.root_epoch < b.root_epoch { a } else { b };
    }
    if material_canonical(a) <= material_canonical(b) {
        a
    } else {
        b
    }
}

fn merge_extra(
    a: &serde_json::Map<String, serde_json::Value>,
    b: &serde_json::Map<String, serde_json::Value>,
) -> serde_json::Map<String, serde_json::Value> {
    let mut out = a.clone();
    for (k, v) in b.iter() {
        out.insert(k.clone(), v.clone());
    }
    out
}

fn merge_entry(x: &CommunityListEntry, y: &CommunityListEntry) -> CommunityListEntry {
    CommunityListEntry {
        community_id: x.community_id.clone(),
        seed: earliest(&x.seed, &y.seed).clone(),
        current: freshest(&x.current, &y.current).clone(),
        // Newest add wins the liveness race against a tombstone.
        added_at: x.added_at.max(y.added_at),
        extra: merge_extra(&x.extra, &y.extra),
    }
}

impl CommunityList {
    /// Deterministically fold `other` into `self`: commutative, idempotent, and
    /// order-independent. Entries and tombstones both stay in the document;
    /// nothing is deleted (liveness is derived). Unknown top-level fields
    /// round-trip (last writer wins, matching armada's object spread).
    pub fn merge(&self, other: &CommunityList) -> CommunityList {
        let mut entries: BTreeMap<String, CommunityListEntry> = BTreeMap::new();
        for e in self.entries.iter().chain(other.entries.iter()) {
            entries
                .entry(e.community_id.clone())
                .and_modify(|prev| *prev = merge_entry(prev, e))
                .or_insert_with(|| e.clone());
        }

        let mut tombstones: BTreeMap<String, Tombstone> = BTreeMap::new();
        for t in self.tombstones.iter().chain(other.tombstones.iter()) {
            match tombstones.get(&t.community_id) {
                // A tombstone is permanent; the latest removal time survives.
                Some(prev) if prev.removed_at >= t.removed_at => {}
                _ => {
                    tombstones.insert(t.community_id.clone(), t.clone());
                }
            }
        }

        CommunityList {
            entries: entries.into_values().collect(),
            tombstones: tombstones.into_values().collect(),
            extra: merge_extra(&self.extra, &other.extra),
        }
    }

    /// Whether a membership is live: it has an entry and no tombstone newer than
    /// (or equal to) its add.
    pub fn is_live(&self, community_id: &str) -> bool {
        let Some(entry) = self.entries.iter().find(|e| e.community_id == community_id) else {
            return false;
        };
        match self.tombstones.iter().find(|t| t.community_id == community_id) {
            None => true,
            Some(tomb) => entry.added_at > tomb.removed_at,
        }
    }

    /// The live memberships, derived.
    pub fn live_entries(&self) -> Vec<&CommunityListEntry> {
        self.entries.iter().filter(|e| self.is_live(&e.community_id)).collect()
    }

    /// Verify the list is publishable: within the membership cap AND under the
    /// NIP-44 plaintext byte cap (the law — private-channel keys can overflow
    /// the event well below 50 memberships). Call before every publish.
    pub fn assert_fits(&self) -> Result<(), ListError> {
        let live = self.live_entries().len();
        if live > MAX_MEMBERSHIPS {
            return Err(ListError::TooManyMemberships(live));
        }
        let bytes = serde_json::to_string(self).map_err(|e| ListError::Json(e.to_string()))?.len();
        if bytes > stream::NIP44_MAX_PLAINTEXT {
            return Err(ListError::Oversize(bytes));
        }
        Ok(())
    }
}

// ── The 13302 event (self-encrypted, replaceable) ────────────────────────────

/// Build the member's kind-13302 Community List: the document NIP-44-encrypted
/// to SELF and signed by the member's real key. Refuses to build an event that
/// violates a cap (a strict reader would drop an oversize one).
///
/// On READ the caller merges into the local mirror, never replaces — see the
/// module doc.
pub fn build_list_event(my_keys: &Keys, list: &CommunityList) -> Result<Event, ListError> {
    list.assert_fits()?;
    let json = serde_json::to_string(list).map_err(|e| ListError::Json(e.to_string()))?;
    let content = nip44::encrypt(my_keys.secret_key(), &my_keys.public_key(), json.as_bytes(), Version::V2)
        .map_err(|e| ListError::Nip44(e.to_string()))?;
    EventBuilder::new(Kind::Custom(kind::COMMUNITY_LIST), content)
        .sign_with_keys(my_keys)
        .map_err(|e| ListError::Sign(e.to_string()))
}

/// Decrypt + parse a kind-13302 event with the member's own keys. A decrypt
/// failure surfaces as an error the caller MUST treat as "no news" — never let
/// it clobber a populated local list.
pub fn parse_list_event(event: &Event, my_keys: &Keys) -> Result<CommunityList, ListError> {
    if event.kind.as_u16() != kind::COMMUNITY_LIST {
        return Err(ListError::WrongKind(event.kind.as_u16()));
    }
    let json = nip44::decrypt(my_keys.secret_key(), &my_keys.public_key(), &event.content)
        .map_err(|e| ListError::Nip44(e.to_string()))?;
    serde_json::from_str(&json).map_err(|e| ListError::Json(e.to_string()))
}

/// [`build_list_event`] via a [`NostrSigner`]: self-encrypts to `my_pk` and signs
/// the 13302 through the signer. `my_pk` must equal `my_public_key()`.
pub async fn build_list_event_signed<S: nostr_sdk::prelude::NostrSigner + ?Sized>(
    signer: &S,
    my_pk: PublicKey,
    list: &CommunityList,
) -> Result<Event, ListError> {
    list.assert_fits()?;
    let json = serde_json::to_string(list).map_err(|e| ListError::Json(e.to_string()))?;
    let content = signer.nip44_encrypt(&my_pk, &json).await.map_err(|e| ListError::Nip44(e.to_string()))?;
    let unsigned = EventBuilder::new(Kind::Custom(kind::COMMUNITY_LIST), content).build(my_pk);
    signer.sign_event(unsigned).await.map_err(|e| ListError::Sign(e.to_string()))
}

/// [`parse_list_event`] via a [`NostrSigner`] (self-decrypt to `my_pk`).
pub async fn parse_list_event_signed<S: nostr_sdk::prelude::NostrSigner + ?Sized>(
    signer: &S,
    my_pk: PublicKey,
    event: &Event,
) -> Result<CommunityList, ListError> {
    if event.kind.as_u16() != kind::COMMUNITY_LIST {
        return Err(ListError::WrongKind(event.kind.as_u16()));
    }
    let json = signer.nip44_decrypt(&my_pk, &event.content).await.map_err(|e| ListError::Nip44(e.to_string()))?;
    serde_json::from_str(&json).map_err(|e| ListError::Json(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostr_sdk::prelude::Keys;

    fn material(root_epoch: u64, name: &str) -> JoinMaterial {
        JoinMaterial {
            community_id: "c".repeat(64),
            owner: "a".repeat(64),
            owner_salt: "b".repeat(64),
            community_root: "d".repeat(64),
            root_epoch,
            channels: vec![],
            relays: vec![],
            name: name.to_string(),
            extra: serde_json::Map::new(),
        }
    }

    fn entry(cid: &str, added_at: u64, seed: JoinMaterial, current: JoinMaterial) -> CommunityListEntry {
        CommunityListEntry {
            community_id: cid.to_string(),
            seed,
            current,
            added_at,
            extra: serde_json::Map::new(),
        }
    }

    fn tomb(cid: &str, removed_at: u64) -> Tombstone {
        Tombstone { community_id: cid.to_string(), removed_at, extra: serde_json::Map::new() }
    }

    fn list(entries: Vec<CommunityListEntry>, tombstones: Vec<Tombstone>) -> CommunityList {
        CommunityList { entries, tombstones, extra: serde_json::Map::new() }
    }

    #[test]
    fn canonical_json_sorts_nested_objects_and_keeps_array_order() {
        // GOLDEN: unsorted input with a nested object and arrays → one
        // deterministic byte string. Cross-client ties break on exactly this.
        let v: serde_json::Value =
            serde_json::from_str(r#"{"b":1,"a":{"z":[3,2,1],"y":"x"},"arr":[{"k":2,"j":1}]}"#).unwrap();
        assert_eq!(
            canonical_json(&v),
            r#"{"a":{"y":"x","z":[3,2,1]},"arr":[{"j":1,"k":2}],"b":1}"#
        );
    }

    #[test]
    fn canonical_json_is_key_order_independent() {
        let a: serde_json::Value =
            serde_json::from_str(r#"{"name":"x","owner":"o","community_id":"c"}"#).unwrap();
        let b: serde_json::Value =
            serde_json::from_str(r#"{"community_id":"c","owner":"o","name":"x"}"#).unwrap();
        assert_eq!(canonical_json(&a), canonical_json(&b));
        assert_eq!(canonical_json(&a), r#"{"community_id":"c","name":"x","owner":"o"}"#);
    }

    #[test]
    fn equal_epoch_materials_differing_only_in_key_order_canonicalize_identically() {
        // Two logically-equal materials whose serde_json::Value keys differ in
        // insertion order must produce identical canonical bytes.
        let jm = material(5, "Vector");
        let value = serde_json::to_value(&jm).unwrap();
        let shuffled: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&value).unwrap()).unwrap();
        assert_eq!(canonical_json(&value), canonical_json(&shuffled));
    }

    #[test]
    fn merge_seed_keeps_lower_epoch_current_keeps_higher() {
        let a = list(vec![entry("aa", 100, material(2, "x"), material(5, "x"))], vec![]);
        let b = list(vec![entry("aa", 100, material(1, "x"), material(8, "x"))], vec![]);
        let merged = a.merge(&b);
        assert_eq!(merged.entries.len(), 1);
        assert_eq!(merged.entries[0].seed.root_epoch, 1, "seed keeps the lower epoch");
        assert_eq!(merged.entries[0].current.root_epoch, 8, "current keeps the higher epoch");
    }

    #[test]
    fn merge_equal_epoch_tie_breaks_on_lowest_canonical_bytes_and_is_commutative() {
        let a = list(vec![entry("aa", 100, material(5, "aaa"), material(5, "aaa"))], vec![]);
        let b = list(vec![entry("aa", 100, material(5, "bbb"), material(5, "bbb"))], vec![]);
        // "aaa" < "bbb" → the aaa material wins both seed and current, either way.
        let ab = a.merge(&b);
        assert_eq!(ab.entries[0].current.name, "aaa");
        assert_eq!(ab.entries[0].seed.name, "aaa");
        let ba = b.merge(&a);
        assert_eq!(ba.entries[0].current.name, "aaa");
        assert_eq!(ba.entries[0].seed.name, "aaa");
    }

    #[test]
    fn tombstone_beats_a_stale_entry_but_a_newer_rejoin_beats_the_tombstone() {
        let stale = list(vec![entry("aa", 100, material(0, "x"), material(0, "x"))], vec![tomb("aa", 200)]);
        assert!(!stale.is_live("aa"), "a leave newer than the add wins");
        assert!(stale.live_entries().is_empty());

        let rejoined = list(vec![entry("aa", 300, material(0, "x"), material(0, "x"))], vec![tomb("aa", 200)]);
        assert!(rejoined.is_live("aa"), "a re-join newer than the removal resurrects");
    }

    #[test]
    fn a_backfill_never_re_adds_a_tombstoned_id_but_the_entry_stays_in_the_doc() {
        let base = list(vec![entry("aa", 100, material(0, "x"), material(0, "x"))], vec![tomb("aa", 200)]);
        // An older backfill add cannot cross the removal time.
        let backfill = list(vec![entry("aa", 50, material(0, "x"), material(0, "x"))], vec![]);
        let merged = base.merge(&backfill);
        assert!(merged.entries.iter().any(|e| e.community_id == "aa"), "tombstoned entry stays in the doc");
        assert!(!merged.is_live("aa"), "the backfill cannot resurrect it");

        // A genuine re-join (newer add) still resurrects.
        let rejoin = list(vec![entry("aa", 300, material(0, "x"), material(0, "x"))], vec![]);
        assert!(merged.merge(&rejoin).is_live("aa"));
    }

    #[test]
    fn tombstones_are_permanent_and_union_with_newest_removal() {
        let a = list(vec![], vec![tomb("aa", 100)]);
        let b = list(vec![], vec![tomb("aa", 300), tomb("bb", 50)]);
        let merged = a.merge(&b);
        assert_eq!(merged.tombstones.len(), 2);
        let aa = merged.tombstones.iter().find(|t| t.community_id == "aa").unwrap();
        assert_eq!(aa.removed_at, 300, "the newest removal survives the union");
    }

    #[test]
    fn unknown_fields_round_trip_at_every_level() {
        let wire = r#"{
            "entries": [{
                "community_id": "aa",
                "seed": {"community_id":"aa","owner":"o","owner_salt":"s","community_root":"r","root_epoch":1,
                         "channels":[],"relays":[],"name":"n",
                         "held_roots":[{"epoch":1,"key":"kk"}],"refounder":"rr","weird":123},
                "current": {"community_id":"aa","owner":"o","owner_salt":"s","community_root":"r","root_epoch":1,
                            "channels":[],"relays":[],"name":"n"},
                "added_at": 5,
                "entry_extra": true
            }],
            "tombstones": [],
            "list_extra": {"deep": [1,2]}
        }"#;
        let parsed: CommunityList = serde_json::from_str(wire).unwrap();
        let out = serde_json::to_string(&parsed).unwrap();
        let reparsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(reparsed["list_extra"]["deep"][1], 2);
        assert_eq!(reparsed["entries"][0]["entry_extra"], true);
        assert_eq!(reparsed["entries"][0]["seed"]["held_roots"][0]["epoch"], 1);
        assert_eq!(reparsed["entries"][0]["seed"]["refounder"], "rr");
        assert_eq!(reparsed["entries"][0]["seed"]["weird"], 123);
    }

    #[test]
    fn assert_fits_enforces_the_membership_cap_on_the_live_set() {
        let mut entries = vec![];
        for i in 0..(MAX_MEMBERSHIPS + 1) {
            entries.push(entry(&format!("{i:064x}"), 100, material(0, "x"), material(0, "x")));
        }
        let l = list(entries, vec![]);
        assert_eq!(l.live_entries().len(), MAX_MEMBERSHIPS + 1);
        assert!(matches!(l.assert_fits(), Err(ListError::TooManyMemberships(n)) if n == MAX_MEMBERSHIPS + 1));
    }

    #[test]
    fn assert_fits_catches_an_oversize_list_well_under_the_membership_cap() {
        // One membership whose join material carries enough private-channel keys
        // to blow the NIP-44 plaintext cap.
        let mut jm = material(0, "x");
        for i in 0..600u64 {
            jm.channels.push(ChannelKeyRef {
                id: format!("{i:064x}"),
                key: "e".repeat(64),
                epoch: 0,
                name: "c".into(),
            });
        }
        let l = list(vec![entry(&"f".repeat(64), 100, jm.clone(), jm)], vec![]);
        assert_eq!(l.live_entries().len(), 1);
        assert!(matches!(l.assert_fits(), Err(ListError::Oversize(_))));
        // build_list_event refuses it too — never mint an event a strict reader drops.
        let keys = Keys::generate();
        assert!(matches!(build_list_event(&keys, &l), Err(ListError::Oversize(_))));
    }

    #[test]
    fn list_event_round_trips_and_rejects_wrong_kind_or_recipient() {
        let keys = Keys::generate();
        let l = list(
            vec![entry(&"a".repeat(64), 5, material(1, "seed"), material(3, "current"))],
            vec![tomb(&"b".repeat(64), 9)],
        );
        let ev = build_list_event(&keys, &l).unwrap();
        assert_eq!(ev.kind.as_u16(), kind::COMMUNITY_LIST);
        assert_eq!(parse_list_event(&ev, &keys).unwrap(), l);

        // A non-13302 event is rejected outright.
        let wrong = EventBuilder::new(Kind::Custom(1), "x").sign_with_keys(&keys).unwrap();
        assert!(matches!(parse_list_event(&wrong, &keys), Err(ListError::WrongKind(1))));

        // Another account can't decrypt it (fail-closed — caller keeps its list).
        let other = Keys::generate();
        assert!(parse_list_event(&ev, &other).is_err());
    }

    #[test]
    fn merge_is_idempotent() {
        let l = list(
            vec![entry("aa", 100, material(1, "x"), material(4, "x"))],
            vec![tomb("bb", 7)],
        );
        assert_eq!(l.merge(&l), l.merge(&l).merge(&l));
        assert_eq!(l.merge(&l).entries.len(), 1);
        assert_eq!(l.merge(&l).tombstones.len(), 1);
    }
}
