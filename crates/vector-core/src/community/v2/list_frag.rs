//! CORD-02 §8 fragmented Community List (kind 33302).
//!
//! The List is addressable, one event per fragment at `d` = the fragment index,
//! so it shards past the ~64KB an event may be and a membership set has no cap.
//! [`fragment`] packs a list into as few fragments as fit; [`defragment`] unions
//! whatever fragments a reader holds.
//!
//! Three shape changes against the retired single-event form: every 32-byte
//! value is unpadded base64url at any depth; a snapshot embedded in an entry
//! drops the `community_id` it inherits; `seed` is absent whenever it equals
//! `current`.

use crate::event_ext::FinalizeUnsignedWithId;
use nostr_sdk::prelude::nip44::{self, Version};
use nostr_sdk::prelude::{Event, EventBuilder, FinalizeEvent, Keys, Kind, PublicKey, Tag, Timestamp};
use serde::{Deserialize, Serialize};

use super::list::{ChannelKeyRef, CommunityList, JoinMaterial};

/// The relay ceiling the List actually has to fit. The binding limit is the
/// encoded EVENT, never the NIP-44 plaintext: content is base64 ciphertext at
/// ~4/3, so a plaintext-only check mints events every relay refuses.
pub const MAX_EVENT_BYTES: usize = 65_536;

/// Everything in the signed event that isn't `content` — id, pubkey, sig, kind,
/// created_at, the `d` tag, JSON scaffolding. Deliberately generous.
const EVENT_ENVELOPE_BYTES: usize = 320;

// ── wire ─────────────────────────────────────────────────────────────────────

/// One fragment. `frags` is the total, declared in every fragment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FragList {
    pub frags: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entries: Vec<FragEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tombstones: Vec<FragTombstone>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FragEntry {
    pub community_id: String,
    /// Absent when it equals `current` — which is what absence means.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<FragMaterial>,
    pub current: FragMaterial,
    pub added_at: u64,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// Join material as embedded in an entry: no `community_id`, it inherits the
/// entry's. A standalone snapshot (a CORD-06 §1 dissolution payload) keeps its.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FragMaterial {
    pub owner: String,
    pub owner_salt: String,
    pub community_root: String,
    pub root_epoch: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control_pk: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control_root: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub channels: Vec<FragChannel>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relays: Vec<String>,
    pub name: String,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FragChannel {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    pub epoch: u64,
    pub name: String,
    /// Armada's `priors` ride here. Without it a republish takes every other
    /// channel's pre-rotation history dark.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FragTombstone {
    pub community_id: String,
    pub removed_at: u64,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

// ── encoding ─────────────────────────────────────────────────────────────────

/// 32 bytes of hex to unpadded base64url (43 chars). Anything that is not
/// exactly 32 bytes of hex passes through untouched: the amendment re-encodes
/// KEYS, and an unknown field from a peer may hold anything at all.
fn b64(value: &str) -> String {
    match crate::simd::hex::hex_to_bytes_32_checked(value) {
        Some(bytes) => base64_simd::URL_SAFE_NO_PAD.encode_to_string(bytes),
        None => value.to_string(),
    }
}

fn material(src: &JoinMaterial) -> FragMaterial {
    FragMaterial {
        owner: b64(&src.owner),
        owner_salt: b64(&src.owner_salt),
        community_root: b64(&src.community_root),
        root_epoch: src.root_epoch,
        control_pk: src.control_pk.as_deref().map(b64),
        control_root: src.control_root.as_deref().map(b64),
        channels: src.channels.iter().map(channel).collect(),
        relays: src.relays.clone(),
        name: src.name.clone(),
        extra: src.extra.clone(),
    }
}

fn channel(src: &ChannelKeyRef) -> FragChannel {
    FragChannel {
        id: b64(&src.id),
        key: src.key.as_deref().map(b64),
        epoch: src.epoch,
        name: src.name.clone(),
        extra: src.extra.clone(),
    }
}

// ── decoding ─────────────────────────────────────────────────────────────────

/// Unpadded base64url back to 32 bytes of hex. Anything that isn't a 43-char
/// base64 value passes through untouched — the same tolerance as [`b64`], and
/// what lets a document carrying both encodings round-trip either way.
fn unb64(value: &str) -> String {
    if value.len() != 43 {
        return value.to_string();
    }
    match base64_simd::URL_SAFE_NO_PAD.decode_to_vec(value) {
        Ok(bytes) if bytes.len() == 32 => {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&bytes);
            crate::simd::hex::bytes_to_hex_32(&arr)
        }
        _ => value.to_string(),
    }
}

fn unmaterial(src: &FragMaterial, community_id: &str) -> JoinMaterial {
    JoinMaterial {
        // Inherited from the entry — the embedded form never carries it.
        community_id: community_id.to_string(),
        owner: unb64(&src.owner),
        owner_salt: unb64(&src.owner_salt),
        community_root: unb64(&src.community_root),
        root_epoch: src.root_epoch,
        control_pk: src.control_pk.as_deref().map(unb64),
        control_root: src.control_root.as_deref().map(unb64),
        channels: src
            .channels
            .iter()
            .map(|c| ChannelKeyRef {
                id: unb64(&c.id),
                key: c.key.as_deref().map(unb64),
                epoch: c.epoch,
                name: c.name.clone(),
                extra: c.extra.clone(),
            })
            .collect(),
        relays: src.relays.clone(),
        name: src.name.clone(),
        extra: src.extra.clone(),
    }
}

/// Union a fragment set back into one list. Entries and tombstones from every
/// fragment are concatenated; a `community_id` appearing in more than one
/// fragment is merged by the caller's own merge, never duplicated here — an
/// interrupted repack legitimately leaves one in two places.
pub fn defragment(frags: &[FragList]) -> CommunityList {
    let mut out = CommunityList::default();
    for f in frags {
        for e in &f.entries {
            let cid = unb64(&e.community_id);
            let current = unmaterial(&e.current, &cid);
            // Absent seed means "equal to current" — that is what absence means.
            let seed = e.seed.as_ref().map(|s| unmaterial(s, &cid)).unwrap_or_else(|| current.clone());
            out.entries.push(super::list::CommunityListEntry {
                community_id: cid,
                seed,
                current,
                added_at: e.added_at,
                extra: e.extra.clone(),
            });
        }
        for t in &f.tombstones {
            out.tombstones.push(super::list::Tombstone {
                community_id: unb64(&t.community_id),
                removed_at: t.removed_at,
                extra: t.extra.clone(),
            });
        }
        for (k, v) in &f.extra {
            out.extra.entry(k.clone()).or_insert_with(|| v.clone());
        }
    }
    out
}

// ── sizing ───────────────────────────────────────────────────────────────────

/// NIP-44 v2 padded plaintext length.
fn nip44_padded_len(unpadded: usize) -> usize {
    if unpadded <= 32 {
        return 32;
    }
    let next_power = 1usize << (usize::BITS - (unpadded - 1).leading_zeros()) as usize;
    let chunk = if next_power <= 256 { 32 } else { next_power / 8 };
    chunk * ((unpadded - 1) / chunk + 1)
}

/// The encoded event a plaintext of `n` bytes becomes: NIP-44 v2 is
/// `version ‖ nonce ‖ len ‖ padded ‖ mac`, base64'd into `content`.
pub fn projected_event_bytes(plaintext: usize) -> usize {
    let raw = 1 + 32 + 2 + nip44_padded_len(plaintext) + 32;
    raw.div_ceil(3) * 4 + EVENT_ENVELOPE_BYTES
}

fn json_len<T: Serialize>(value: &T) -> usize {
    serde_json::to_string(value).map(|s| s.len()).unwrap_or(0)
}

// ── fragmentation ────────────────────────────────────────────────────────────

/// Pack the list into fragments, each projected to fit [`MAX_EVENT_BYTES`].
///
/// Greedy and order-preserving: an entry lands in the first fragment with room.
/// Placement is arbitrary by design — a `community_id` in two fragments merges,
/// so guessing wrong costs a duplicate, never a loss.
///
/// An entry its tombstone outranks is dropped (CORD-02 §8) — a membership is live
/// only while its entry beats its removal, so a stale fragment re-unioning the
/// retired entry still reads as left.
pub fn fragment(list: &CommunityList) -> Vec<FragList> {
    let entries: Vec<FragEntry> = list
        .entries
        .iter()
        .filter(|e| list.is_live(&e.community_id))
        .map(|e| {
            let current = material(&e.current);
            let seed = material(&e.seed);
            FragEntry {
                community_id: b64(&e.community_id),
                // The omission that pays for itself: identical snapshots are the
                // common case, and every unrefounded membership has them.
                seed: (seed != current).then_some(seed),
                current,
                added_at: e.added_at,
                extra: e.extra.clone(),
            }
        })
        .collect();
    let tombs: Vec<FragTombstone> = list
        .tombstones
        .iter()
        .map(|t| FragTombstone {
            community_id: b64(&t.community_id),
            removed_at: t.removed_at,
            extra: t.extra.clone(),
        })
        .collect();

    let mut frags: Vec<FragList> = vec![FragList {
        frags: 1,
        entries: vec![],
        tombstones: vec![],
        extra: list.extra.clone(),
    }];
    // Room left in the current fragment, measured against the projected event.
    let fits = |f: &FragList, add: usize| projected_event_bytes(json_len(f) + add) <= MAX_EVENT_BYTES;

    for e in entries {
        let cost = json_len(&e) + 1;
        let last = frags.last_mut().expect("seeded above");
        if last.entries.is_empty() || fits(last, cost) {
            last.entries.push(e);
        } else {
            frags.push(FragList { frags: 1, entries: vec![e], tombstones: vec![], extra: Default::default() });
        }
    }
    for t in tombs {
        let cost = json_len(&t) + 1;
        let last = frags.last_mut().expect("seeded above");
        if last.tombstones.is_empty() || fits(last, cost) {
            last.tombstones.push(t);
        } else {
            frags.push(FragList { frags: 1, entries: vec![], tombstones: vec![t], extra: Default::default() });
        }
    }

    let total = frags.len();
    for f in &mut frags {
        f.frags = total;
    }
    frags
}

// ── events ───────────────────────────────────────────────────────────────────

/// Build one signed fragment at `d` = `index`.
///
/// `created_at` is the caller's to choose and MUST exceed this fragment's
/// previous value: relays resolve an addressable event on `created_at` alone and
/// break ties on the lowest event id, so two writes sharing a second can discard
/// the later one.
pub async fn build_fragment_event<S: crate::signer::VectorSigner + ?Sized>(
    signer: &S,
    my_pk: PublicKey,
    frag: &FragList,
    index: usize,
    created_at: u64,
) -> Result<Event, String> {
    let json = serde_json::to_string(frag).map_err(|e| format!("fragment json: {e}"))?;
    let content = signer
        .nip44_encrypt_async(&my_pk, &json)
        .await
        .map_err(|e| format!("fragment nip44: {e}"))?;
    let unsigned = EventBuilder::new(Kind::Custom(super::kind::COMMUNITY_LIST_FRAG), content)
        .tag(Tag::identifier(index.to_string()))
        .custom_created_at(Timestamp::from_secs(created_at))
        .finalize_unsigned_with_id(my_pk);
    signer.sign_event_async(unsigned).await.map_err(|e| format!("fragment sign: {e}"))
}

/// [`build_fragment_event`] with raw keys — the local-signer path, and what tests
/// use to stand in for a sibling device.
pub fn build_fragment_event_keys(
    my_keys: &Keys,
    frag: &FragList,
    index: usize,
    created_at: u64,
) -> Result<Event, String> {
    let json = serde_json::to_string(frag).map_err(|e| format!("fragment json: {e}"))?;
    let content = nip44::encrypt(my_keys.secret_key(), &my_keys.public_key(), json.as_bytes(), Version::V2)
        .map_err(|e| format!("fragment nip44: {e}"))?;
    EventBuilder::new(Kind::Custom(super::kind::COMMUNITY_LIST_FRAG), content)
        .tag(Tag::identifier(index.to_string()))
        .custom_created_at(Timestamp::from_secs(created_at))
        .finalize(my_keys)
        .map_err(|e| format!("fragment sign: {e}"))
}

/// Parse a fragment event back, returning its `d` index alongside the document.
pub async fn parse_fragment_event<S: crate::signer::VectorSigner + ?Sized>(
    signer: &S,
    my_pk: PublicKey,
    event: &Event,
) -> Result<(usize, FragList), String> {
    if event.kind.as_u16() != super::kind::COMMUNITY_LIST_FRAG {
        return Err(format!("wrong kind {}", event.kind.as_u16()));
    }
    let index: usize = event
        .tags
        .identifier()
        .ok_or("fragment carries no d tag")?
        .parse()
        .map_err(|_| "fragment d tag is not an index".to_string())?;
    let json = signer
        .nip44_decrypt_async(&my_pk, &event.content)
        .await
        .map_err(|e| format!("fragment decrypt: {e}"))?;
    let frag: FragList = serde_json::from_str(&json).map_err(|e| format!("fragment parse: {e}"))?;
    Ok((index, frag))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex32(byte: u8) -> String {
        crate::simd::hex::bytes_to_hex_32(&[byte; 32])
    }

    fn jm(name: &str) -> JoinMaterial {
        JoinMaterial {
            community_id: hex32(0x11),
            owner: hex32(0x22),
            owner_salt: hex32(0x33),
            community_root: hex32(0x44),
            root_epoch: 3,
            control_pk: Some(hex32(0x55)),
            control_root: None,
            channels: vec![],
            relays: vec!["wss://relay.example.com".into()],
            name: name.into(),
            extra: Default::default(),
        }
    }

    fn entry(seed: JoinMaterial, current: JoinMaterial) -> super::super::list::CommunityListEntry {
        super::super::list::CommunityListEntry {
            community_id: hex32(0x11),
            seed,
            current,
            added_at: 1_719_800_000_000,
            extra: Default::default(),
        }
    }

    fn list_of(entries: Vec<super::super::list::CommunityListEntry>) -> CommunityList {
        CommunityList { entries, tombstones: vec![], extra: Default::default() }
    }

    #[test]
    fn thirty_two_byte_values_become_43_char_base64() {
        let f = fragment(&list_of(vec![entry(jm("x"), jm("x"))]));
        let m = &f[0].entries[0].current;
        assert_eq!(m.owner.len(), 43);
        assert_eq!(f[0].entries[0].community_id.len(), 43);
        assert!(!m.owner.contains('='), "padding would give one value two spellings");
    }

    #[test]
    fn seed_is_omitted_when_it_equals_current() {
        let f = fragment(&list_of(vec![entry(jm("x"), jm("x"))]));
        assert!(f[0].entries[0].seed.is_none());
        // ...and kept when it genuinely differs.
        let mut older = jm("x");
        older.root_epoch = 1;
        let g = fragment(&list_of(vec![entry(older, jm("x"))]));
        assert!(g[0].entries[0].seed.is_some());
    }

    #[test]
    fn a_tombstoned_entry_is_dropped_but_its_tombstone_is_not() {
        let mut l = list_of(vec![entry(jm("x"), jm("x"))]);
        l.tombstones.push(super::super::list::Tombstone {
            community_id: hex32(0x11),
            removed_at: l.entries[0].added_at + 1,
            extra: Default::default(),
        });
        let f = fragment(&l);
        assert!(f[0].entries.is_empty(), "the retired entry's snapshots and keys leave the wire");
        assert_eq!(f[0].tombstones.len(), 1, "the removal itself is permanent");
        assert!(!defragment(&f).is_live(&hex32(0x11)));

        // A re-join out-ranks the tombstone, so its entry rides again.
        l.entries[0].added_at = l.tombstones[0].removed_at + 1;
        let g = fragment(&l);
        assert_eq!(g[0].entries.len(), 1);
        assert!(defragment(&g).is_live(&hex32(0x11)));
    }

    #[test]
    fn embedded_snapshots_carry_no_community_id() {
        let f = fragment(&list_of(vec![entry(jm("x"), jm("x"))]));
        let json = serde_json::to_value(&f[0].entries[0].current).unwrap();
        assert!(json.get("community_id").is_none(), "the entry already keys it");
    }

    #[test]
    fn unknown_fields_survive_at_every_level() {
        let mut m = jm("x");
        m.extra.insert("held_roots".into(), serde_json::json!([{"epoch": 1}]));
        m.channels.push(ChannelKeyRef {
            id: hex32(0x66),
            key: Some(hex32(0x77)),
            epoch: 2,
            name: "staff".into(),
            extra: [("priors".to_string(), serde_json::json!([{"epoch": 1}]))].into_iter().collect(),
        });
        let f = fragment(&list_of(vec![entry(m.clone(), m)]));
        let cur = &f[0].entries[0].current;
        assert!(cur.extra.contains_key("held_roots"));
        assert!(cur.channels[0].extra.contains_key("priors"), "priors must not be dropped");
    }

    #[test]
    fn a_list_survives_fragment_then_defragment_unchanged() {
        let mut refounded_seed = jm("Refounded");
        refounded_seed.root_epoch = 1;
        let mut staff = jm("Staffed");
        staff.control_root = Some(hex32(0x99));
        staff.channels.push(ChannelKeyRef {
            id: hex32(0x66),
            key: Some(hex32(0x77)),
            epoch: 2,
            name: "staff".into(),
            extra: [("priors".to_string(), serde_json::json!([{"epoch": 1, "key": "ab".repeat(32)}]))]
                .into_iter()
                .collect(),
        });
        let mut unkeyed = jm("Unkeyed");
        unkeyed.channels.push(ChannelKeyRef { id: hex32(0x44), key: None, epoch: 0, name: "locked".into(), extra: Default::default() });

        let before = CommunityList {
            entries: vec![
                entry(jm("Plain"), jm("Plain")),                    // seed == current
                entry(refounded_seed, jm("Refounded")),             // seed differs
                entry(staff.clone(), staff),                        // control_root + priors
                entry(unkeyed.clone(), unkeyed),                    // a channel with no key
            ],
            tombstones: vec![super::super::list::Tombstone {
                community_id: hex32(0xAA),
                removed_at: 1_722_400_000_000,
                extra: Default::default(),
            }],
            extra: Default::default(),
        };

        let after = defragment(&fragment(&before));
        assert_eq!(before, after, "fragmenting and defragmenting must be lossless");
    }

    #[test]
    fn unknown_encodings_round_trip_untouched() {
        // An unknown field holding 32 bytes of hex must come back as it went in:
        // a client cannot re-encode what it cannot interpret.
        let mut m = jm("x");
        m.extra.insert("refounder".into(), serde_json::json!("cd".repeat(32)));
        let before = list_of(vec![entry(m.clone(), m)]);
        let after = defragment(&fragment(&before));
        assert_eq!(
            after.entries[0].current.extra.get("refounder").and_then(|v| v.as_str()),
            Some("cd".repeat(32).as_str())
        );
        assert_eq!(before, after);
    }

    #[test]
    fn a_multi_fragment_list_defragments_whole() {
        let entries: Vec<_> = (0..400)
            .map(|i| {
                let mut m = jm(&format!("community {i}"));
                m.owner = hex32((i % 251) as u8);
                let mut e = entry(m.clone(), m);
                e.community_id = format!("{i:064x}");
                e
            })
            .collect();
        let before = list_of(entries);
        let frags = fragment(&before);
        assert!(frags.len() > 1, "400 memberships must span fragments");
        assert_eq!(defragment(&frags).entries.len(), before.entries.len(), "no entry lost across the split");
    }

    #[test]
    fn nip44_padding_matches_the_spec() {
        assert_eq!(nip44_padded_len(1), 32);
        assert_eq!(nip44_padded_len(32), 32);
        assert_eq!(nip44_padded_len(33), 64);
        assert_eq!(nip44_padded_len(100), 128);
    }

    #[test]
    fn fragments_split_before_the_event_ceiling() {
        let entries: Vec<_> = (0..400).map(|_| entry(jm("a community with a name"), jm("a community with a name"))).collect();
        let f = fragment(&list_of(entries));
        assert!(f.len() > 1, "400 memberships must not fit one event");
        for frag in &f {
            assert!(projected_event_bytes(json_len(frag)) <= MAX_EVENT_BYTES);
            assert_eq!(frag.frags, f.len(), "every fragment declares the total");
        }
    }
}
