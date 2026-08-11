//! The Community List document — CORD-02 §8. The wire form it serializes
//! into lives in [`super::list_frag`].
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

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

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
    /// The current epoch's Control Plane signer pubkey (CORD-02 §2/§8) — read
    /// access to the plane, never write. Absent = a legacy pre-split epoch,
    /// whose Control folds at the member-derivable legacy address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control_pk: Option<String>,
    /// STAFF ONLY: the current epoch's `control_root` write secret (hex). The
    /// list is NIP-44-encrypted to self and already carries the
    /// `community_root`, so this is the same trust class — it is how a
    /// staffer's write key survives across their own devices (CORD-02 §8).
    /// Delivered by a staff-making Grant's `control_wrap` (CORD-04 §3) or a
    /// 136-byte base blob (CORD-06 §1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control_root: Option<String>,
    /// The PRIVATE channels held (public ones derive from the root — CORD-03).
    /// ABSENT, not empty, when the writer holds no keys — armada omits the field.
    /// Required once, which rejected the whole vault for the commonest case there
    /// is: a membership with no private channels.
    #[serde(default)]
    pub channels: Vec<ChannelKeyRef>,
    #[serde(default)]
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
    /// ABSENT when the writer knows the channel but holds no key for it — armada
    /// lists private channels the account has not been granted. Required here
    /// once, which made ONE unkeyed channel reject the whole document and strand
    /// the account on a stale copy, so it must stay optional.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    pub epoch: u64,
    pub name: String,
    /// Round-tripped verbatim, like every other level of the document. armada
    /// carries `priors` here — a channel's retired keys, which is how history
    /// spanning a rekey stays readable; dropping them on republish takes that
    /// history dark for every device the account owns.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
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

/// Errors from serializing or encrypting the List document.
#[derive(Debug)]
pub enum ListError {
    Json(String),
    Nip44(String),
    Sign(String),
}

impl std::fmt::Display for ListError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ListError::Json(e) => write!(f, "json: {e}"),
            ListError::Nip44(e) => write!(f, "nip44: {e}"),
            ListError::Sign(e) => write!(f, "sign: {e}"),
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

}

#[cfg(test)]
mod tests {
    use super::*;

    fn material(root_epoch: u64, name: &str) -> JoinMaterial {
        JoinMaterial {
            community_id: "c".repeat(64),
            owner: "a".repeat(64),
            owner_salt: "b".repeat(64),
            community_root: "d".repeat(64),
            root_epoch,
            control_pk: None,
            control_root: None,
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
    fn a_channel_listed_without_a_key_does_not_reject_the_whole_document() {
        // GOLDEN, armada-shaped: it lists private channels the account holds NO key
        // for, omitting `key` entirely. `key` was a required String, so ONE such
        // channel failed the whole parse and pinned the account to a stale copy —
        // cross-device sync stopped dead the day private channels shipped.
        let json = r#"{
          "entries": [{
            "community_id": "aa",
            "seed":    {"community_id":"aa","owner":"o","owner_salt":"s","community_root":"r","root_epoch":1,"channels":[],"relays":[],"name":"n"},
            "current": {"community_id":"aa","owner":"o","owner_salt":"s","community_root":"r","root_epoch":1,
                        "channels":[
                          {"id":"c1","key":"kk","epoch":2,"name":"keyed"},
                          {"id":"c2","epoch":0,"name":"not-granted"}
                        ],
                        "relays":[],"name":"n"},
            "added_at": 5
          }],
          "tombstones": []
        }"#;
        let parsed: CommunityList = serde_json::from_str(json).expect("a keyless channel must parse");
        let chans = &parsed.entries[0].current.channels;
        assert_eq!(chans.len(), 2, "both channels survive");
        assert_eq!(chans[0].key.as_deref(), Some("kk"));
        assert_eq!(chans[1].key, None, "an unheld channel is known but keyless");
        assert_eq!(chans[1].name, "not-granted", "and still carries its identity");
    }

    #[test]
    fn join_material_that_vends_no_keys_omits_channels_entirely() {
        // armada drops `channels` when it holds no keys ("the type promises an
        // array while the wire promises nothing"). A required Vec would reject the
        // whole vault document for the commonest case of all: a public community.
        let json = r#"{
          "entries": [{
            "community_id": "aa",
            "seed":    {"community_id":"aa","owner":"o","owner_salt":"s","community_root":"r","root_epoch":1,"relays":[],"name":"n"},
            "current": {"community_id":"aa","owner":"o","owner_salt":"s","community_root":"r","root_epoch":1,"relays":[],"name":"n"},
            "added_at": 5
          }],
          "tombstones": []
        }"#;
        let parsed: CommunityList = serde_json::from_str(json).expect("absent `channels` must parse");
        assert!(parsed.entries[0].current.channels.is_empty(), "no keys held");
        assert!(parsed.is_live("aa"), "and the membership still counts");
    }

    #[test]
    fn a_keyless_channel_is_never_written_back() {
        // Shipped builds require `key`, so emitting a keyless entry would inflict
        // this very bug on them. Absent, not null.
        let c = ChannelKeyRef { id: "c".into(), key: None, epoch: 0, name: "n".into(), extra: Default::default() };
        let s = serde_json::to_string(&c).unwrap();
        assert!(!s.contains("key"), "no `key` field at all, got {s}");
        let keyed = ChannelKeyRef { id: "c".into(), key: Some("kk".into()), epoch: 1, name: "n".into(), extra: Default::default() };
        assert!(serde_json::to_string(&keyed).unwrap().contains(r#""key":"kk""#), "keyed stays byte-identical");
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
