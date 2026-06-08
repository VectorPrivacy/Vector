//! Per-entity version chain for authority editions.
//!
//! Every authority record (a Grant, RoleMetadata, RoleOrder, Banlist, the OwnerAttestation) is a
//! sequence of **editions**. Each edition carries a monotonic `version` and the hash of its
//! predecessor (`prev_hash`), and the actor's real-npub signature covers both — so the chain is over
//! signed content, not the (ephemeral) outer wrapper. Clients fold the fetched set into the current
//! head by the rules:
//!   - **refuse-downgrade** on the version integer (never accept a version below the floor already held);
//!   - **equal-version fork** resolves by a deterministic tiebreak: the lower **inner edition id** (a
//!     commitment hash over author+content+tags+time, NOT the author-settable `created_at`, so it can't
//!     be cheaply biased — the authority-first lens is layered on by the caller via the roster);
//!   - a **gap** (a higher version whose `prev_hash` doesn't link contiguously to what we hold) leaves
//!     the head at the highest *contiguous* version and is reported, so the caller can fail closed for
//! that entity and refetch the missing prereqs from the quorum (H1/M8) rather than fail open.

use sha2::{Digest, Sha256};

/// Frozen domain-separation label for the edition canonicalization (never change).
const EDITION_LABEL: &[u8] = b"vector-community/v1/edition";

/// Domain-separated, length-prefixed canonical bytes an authority edition commits to.
///
/// Layout (FROZEN — interop + no-migration depend on it):
/// `u64_be(label.len) ‖ label ‖ entity_id[32] ‖ u64_be(version) ‖ has_prev(1) ‖ prev_hash[32 or zero]
///  ‖ u64_be(content.len) ‖ content`.
/// Every field is fixed-width or length-prefixed so distinct inputs can never collide.
pub fn edition_signing_bytes(
    entity_id: &[u8; 32],
    version: u64,
    prev_hash: Option<&[u8; 32]>,
    content: &[u8],
) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + EDITION_LABEL.len() + 32 + 8 + 1 + 32 + 8 + content.len());
    out.extend_from_slice(&(EDITION_LABEL.len() as u64).to_be_bytes());
    out.extend_from_slice(EDITION_LABEL);
    out.extend_from_slice(entity_id);
    out.extend_from_slice(&version.to_be_bytes());
    match prev_hash {
        Some(h) => {
            out.push(1);
            out.extend_from_slice(h);
        }
        None => {
            out.push(0);
            out.extend_from_slice(&[0u8; 32]);
        }
    }
    out.extend_from_slice(&(content.len() as u64).to_be_bytes());
    out.extend_from_slice(content);
    out
}

/// SHA-256 of [`edition_signing_bytes`] — the edition's identity in the chain. The next edition's
/// `prev_hash` cites this value.
pub fn edition_hash(
    entity_id: &[u8; 32],
    version: u64,
    prev_hash: Option<&[u8; 32]>,
    content: &[u8],
) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(edition_signing_bytes(entity_id, version, prev_hash, content));
    h.finalize().into()
}

/// One fetched edition of an entity, reduced to what the fold needs. (Signature/authority validation
/// happens before this — only editions whose real-npub signature verified are folded.)
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edition {
    pub version: u64,
    pub prev_hash: Option<[u8; 32]>,
    /// `edition_hash` of THIS edition (what the next edition's `prev_hash` must cite).
    pub self_hash: [u8; 32],
    /// Inner authored timestamp (secs); the first tiebreak at equal version.
    pub created_at: u64,
    /// Inner event id; the deterministic final tiebreak (same for every member).
    pub tiebreak_id: [u8; 32],
}

/// The outcome of folding one entity's editions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FoldResult {
    /// Index (into the input slice) of the chosen head edition, or `None` if nothing ≥ floor.
    pub head: Option<usize>,
    /// A higher version exists but doesn't link contiguously to the head — withheld prereqs. The
    /// caller fails CLOSED for the entity (suspends its authority) and refetches (H1/M8).
    pub gap: bool,
}

/// Fold a set of editions for **one** entity into its current head.
///
/// `floor` is the highest version the client has already accepted (0 = none yet) and `floor_hash` is
/// that held edition's [`Edition::self_hash`] (so a new edition can be proven to link to it).
/// Editions below the floor are ignored (refuse-downgrade), equal-version forks pick the
/// deterministic tiebreak winner, and the head walks the contiguous `prev_hash` chain upward.
///
/// **`gap` is the safety signal.** It is set whenever the head is NOT chain-anchored — either the
/// lowest edition isn't a genesis / doesn't link to `floor_hash`, or a link breaks mid-chain. A
/// **tracking** client (one that already holds the floor) MUST fail closed on `gap` (suspend the
/// entity and refetch the missing prereqs from the quorum — H1/M8), since an unanchored head can
/// be a forged or rolled-back edition (a hostile relay serving only a high version). A
/// **bootstrapping** client (a new joiner, `floor == 0`, who legitimately lacks history because the
/// state was re-anchored under a later epoch) may accept the head despite `gap` *only* after
/// independently verifying its author's current authority against the roster + owner attestation.
pub fn fold(editions: &[Edition], floor: u64, floor_hash: Option<&[u8; 32]>) -> FoldResult {
    use std::collections::BTreeMap;
    // Per-version winner (equal-version fork → lower tiebreak_id). Skip anything below the floor:
    // refuse-downgrade.
    let mut by_version: BTreeMap<u64, usize> = BTreeMap::new();
    for (i, e) in editions.iter().enumerate() {
        if e.version < floor {
            continue;
        }
        match by_version.get(&e.version) {
            Some(&j) => {
                let cur = &editions[j];
                // Equal-version fork → lower inner edition id wins. The id is a commitment hash over
                // (author, content, tags, time), NOT the author-settable `created_at`, so the winner is
                // deterministic for every client and can't be cheaply gamed (no `created_at=0` always-win).
                if e.tiebreak_id < cur.tiebreak_id {
                    by_version.insert(e.version, i);
                }
            }
            None => {
                by_version.insert(e.version, i);
            }
        }
    }
    let versions: Vec<u64> = by_version.keys().copied().collect();
    if versions.is_empty() {
        return FoldResult { head: None, gap: false };
    }
    // Anchor the lowest edition — the chain must be rooted, not merely internally linked. Without
    // this a lone high-version edition with a forged prev_hash would be trusted as a contiguous head.
    let lo = &editions[by_version[&versions[0]]];
    let anchored = if floor == 0 {
        // No prior head held → only a genuine genesis (v1, no predecessor) anchors the chain.
        versions[0] == 1 && lo.prev_hash.is_none()
    } else if versions[0] == floor {
        // Re-presenting the held edition (e.g. re-anchored under a new epoch — which re-seals the SAME
        // inner edition, so its self_hash is identical). It MUST be the exact edition we committed to:
        // otherwise a relay that withholds ours and serves a DIFFERENT, same-version fork would silently
        // replace our floor. The hash check rejects the fork → gap → fail closed → refetch.
        floor_hash == Some(&lo.self_hash)
    } else if versions[0] == floor + 1 {
        floor_hash.is_some() && lo.prev_hash.as_ref() == floor_hash
    } else {
        false // a jump past the floor with the linking edition(s) missing
    };
    let mut gap = !anchored;
    // Walk upward; advance only across a contiguous link (version == prev+1 AND prev_hash matches).
    let mut head_idx = by_version[&versions[0]];
    for pair in versions.windows(2) {
        let lo_idx = by_version[&pair[0]];
        let hi_idx = by_version[&pair[1]];
        let linked = pair[1] == pair[0] + 1
            && editions[hi_idx].prev_hash == Some(editions[lo_idx].self_hash);
        if linked {
            head_idx = hi_idx;
        } else {
            gap = true; // a higher version exists but isn't contiguously linked
            break;
        }
    }
    FoldResult { head: Some(head_idx), gap }
}

/// The head a **bootstrapping** client accepts: the per-version winner at the HIGHEST present
/// version ≥ `floor`, **ignoring chain contiguity**. A fresh joiner whose genesis was re-anchored away
/// cannot verify lineage at all, so contiguity is the wrong test for it — the real gate is the
/// edition's signature (verified before folding) plus the author's CURRENT authority, which the caller
/// resolves against the roster + owner attestation. A relay cannot forge a higher version (no valid
/// signature), so the worst case is a stale-but-valid head that the union + ratchet later upgrade.
/// Returns `None` if no edition is ≥ `floor`. Equal-version forks use the same deterministic tiebreak
/// as [`fold`] (lower inner edition id) so every client converges on one head.
pub fn bootstrap_head(editions: &[Edition], floor: u64) -> Option<usize> {
    let mut best: Option<usize> = None;
    for (i, e) in editions.iter().enumerate() {
        if e.version < floor {
            continue; // refuse-downgrade still applies
        }
        match best {
            Some(b) => {
                let cur = &editions[b];
                // Higher version wins; at equal version, the lower inner edition id (see `fold`).
                let take = e.version > cur.version
                    || (e.version == cur.version && e.tiebreak_id < cur.tiebreak_id);
                if take {
                    best = Some(i);
                }
            }
            None => best = Some(i),
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(b: u8) -> [u8; 32] {
        [b; 32]
    }

    /// Golden vector — the frozen edition canonicalization must never drift (a change reshuffles
    /// every chain link and forces a migration).
    #[test]
    fn edition_hash_golden_vector() {
        let h = edition_hash(&id(0x11), 1, None, b"hello");
        assert_eq!(
            crate::simd::hex::bytes_to_hex_32(&h),
            "2daf42e65a6bc259a4c99fac6df754a5d3d92310607cf13e2a1e8c94d42f6303"
        );
    }

    /// Distinct fields never collide (length-prefixing): same bytes split differently differ.
    #[test]
    fn edition_hash_is_field_unambiguous() {
        // version vs content boundary can't be confused.
        assert_ne!(
            edition_hash(&id(1), 2, None, b"x"),
            edition_hash(&id(1), 0, None, b"x"),
        );
        let with_prev = edition_hash(&id(1), 2, Some(&id(9)), b"x");
        let without = edition_hash(&id(1), 2, None, b"x");
        assert_ne!(with_prev, without, "prev presence changes the hash");
    }

    /// A linked v1→v2→v3 chain folds to v3 with no gap.
    #[test]
    fn contiguous_chain_folds_to_latest() {
        let e1 = Edition { version: 1, prev_hash: None, self_hash: id(1), created_at: 100, tiebreak_id: id(0xa1) };
        let e2 = Edition { version: 2, prev_hash: Some(id(1)), self_hash: id(2), created_at: 101, tiebreak_id: id(0xa2) };
        let e3 = Edition { version: 3, prev_hash: Some(id(2)), self_hash: id(3), created_at: 102, tiebreak_id: id(0xa3) };
        let r = fold(&[e1, e2, e3], 0, None);
        assert_eq!(r, FoldResult { head: Some(2), gap: false });
    }

    #[test]
    fn bootstrap_head_takes_highest_version_across_gaps() {
        let ed = |v: u64| Edition {
            version: v,
            prev_hash: if v == 1 { None } else { Some(id(v as u8 - 1)) },
            self_hash: id(v as u8),
            created_at: 100 + v,
            tiebreak_id: id(0xa0 + v as u8),
        };
        // GroupRoot shape: 1,2,3,4,(no 5),6..11 — strict fold stops at v4; bootstrap takes v11.
        let groot: Vec<Edition> = [1u64, 2, 3, 4, 6, 7, 8, 9, 10, 11].iter().map(|&v| ed(v)).collect();
        assert_eq!(fold(&groot, 0, None).head.map(|i| groot[i].version), Some(4), "strict head stops at the gap");
        assert!(fold(&groot, 0, None).gap);
        assert_eq!(bootstrap_head(&groot, 0).map(|i| groot[i].version), Some(11), "bootstrap takes the latest across the gap");

        // Grant shape: 2,3,4 with no v1 to anchor — strict can't anchor; bootstrap takes v4.
        let grant: Vec<Edition> = [2u64, 3, 4].iter().map(|&v| ed(v)).collect();
        assert!(fold(&grant, 0, None).gap, "no v1 → strict is unanchored");
        assert_eq!(bootstrap_head(&grant, 0).map(|i| grant[i].version), Some(4));

        // Refuse-downgrade still holds: nothing below the floor.
        assert_eq!(bootstrap_head(&grant, 9), None, "all below floor → no head");

        // Equal-version fork resolves to the deterministic tiebreak winner: lower inner id (a: 0xa1 < b: 0xb1).
        let a = Edition { version: 5, prev_hash: None, self_hash: id(0xAA), created_at: 200, tiebreak_id: id(0xa1) };
        let b = Edition { version: 5, prev_hash: None, self_hash: id(0xBB), created_at: 100, tiebreak_id: id(0xb1) };
        assert_eq!(bootstrap_head(&[a, b], 0), Some(0), "lower inner id wins at equal version (not created_at)");
    }

    // A properly-linked edition: self_hash=id(v), prev=id(v-1) (genesis v1 has no prev).
    fn linked(v: u64) -> Edition {
        Edition {
            version: v,
            prev_hash: if v == 1 { None } else { Some(id((v - 1) as u8)) },
            self_hash: id(v as u8),
            created_at: 100 + v,
            tiebreak_id: id(0xc0u8.wrapping_add(v as u8)),
        }
    }

    /// AGGREGATE LINEARITY — no single relay has the whole chain (relay A: v1,3,5; relay B: v2,4), but the
    /// UNION computes the full contiguous chain → head v5, no gap. This is the property the whole
    /// "bad-relay resilient" design rests on: gaps in any one source are filled by the others. `fold_roster`
    /// inherits this since it folds the per-entity union.
    #[test]
    fn union_of_split_relays_folds_contiguously() {
        let mut union = vec![linked(1), linked(3), linked(5)];
        union.extend(vec![linked(2), linked(4)]);
        let r = fold(&union, 0, None);
        assert_eq!(r.head.map(|i| union[i].version), Some(5));
        assert!(!r.gap, "the union is contiguous v1..v5 even though neither relay had it alone");
    }

    /// Arrival order is irrelevant — the fold is a pure function of the SET (so two clients merging the same
    /// editions in different orders converge identically).
    #[test]
    fn fold_is_order_independent_under_scrambled_arrival() {
        let scrambled = vec![linked(3), linked(1), linked(5), linked(2), linked(4)];
        let r = fold(&scrambled, 0, None);
        assert_eq!(r.head.map(|i| scrambled[i].version), Some(5));
        assert!(!r.gap);
    }

    /// Multiple holes: strict stops at the FIRST gap (fail-closed prefix), bootstrap takes the highest.
    #[test]
    fn multiple_gaps_strict_stops_at_first_bootstrap_takes_highest() {
        let eds = vec![linked(1), linked(2), linked(4), linked(6)]; // holes at v3 and v5
        let r = fold(&eds, 0, None);
        assert_eq!(r.head.map(|i| eds[i].version), Some(2), "strict stops at the first gap");
        assert!(r.gap);
        assert_eq!(bootstrap_head(&eds, 0).map(|i| eds[i].version), Some(6), "bootstrap takes the highest");
    }

    /// The RATCHET click: a gap leaves the head behind; when the missing version streams in from the union,
    /// the chain advances. Convergence is monotonic and order-free.
    #[test]
    fn ratchet_advances_when_the_missing_version_arrives() {
        let before = vec![linked(1), linked(3)]; // v2 missing
        let r1 = fold(&before, 0, None);
        assert_eq!(r1.head.map(|i| before[i].version), Some(1));
        assert!(r1.gap, "v3 can't link without v2");
        let after = vec![linked(1), linked(2), linked(3)]; // v2 arrives
        let r2 = fold(&after, 0, None);
        assert_eq!(r2.head.map(|i| after[i].version), Some(3));
        assert!(!r2.gap, "the gap filled → ratchets to v3");
    }

    /// Duplicate editions (a relay echo, or the same edition from two relays) don't double-count or break
    /// the chain — dedup by version, fold proceeds cleanly.
    #[test]
    fn duplicate_editions_do_not_break_the_fold() {
        let eds = vec![linked(1), linked(2), linked(2), linked(3)];
        let r = fold(&eds, 0, None);
        assert_eq!(r.head.map(|i| eds[i].version), Some(3));
        assert!(!r.gap);
    }

    /// A forged MIDDLE edition (wrong prev) must not let a later "linked" edition advance the head —
    /// the walk breaks at the bad link and the head stays at the last contiguous version.
    #[test]
    fn forged_middle_edition_does_not_advance_the_head() {
        let e1 = linked(1);
        let e2_bad = Edition { version: 2, prev_hash: Some(id(0xFF)), self_hash: id(2), created_at: 102, tiebreak_id: id(0xc2) };
        let e3 = linked(3); // links to id(2) — but v2's link to v1 is forged
        let r = fold(&[e1, e2_bad, e3], 0, None);
        assert_eq!(r.head.map(|i| [1u64, 2, 3][i]), Some(1), "head stays at v1 — the v1→v2 link is broken");
        assert!(r.gap, "a forged middle edition is a gap, not a silent advance");
    }

    /// A held floor whose hash we've lost (`floor_hash = None`) must FAIL CLOSED at floor+1, not blindly
    /// anchor — without the hash we can't prove the incoming edition links to what we hold.
    #[test]
    fn floor_plus_one_without_a_floor_hash_is_a_gap() {
        let e6 = Edition { version: 6, prev_hash: Some(id(5)), self_hash: id(6), created_at: 600, tiebreak_id: id(0xa6) };
        assert!(fold(&[e6], 5, None).gap, "floor+1 with no floor_hash can't be anchored → gap (fail closed)");
    }

    /// A relay serving ONLY stale editions (all below floor) yields "no change" — `head: None, gap: false`
    /// — distinct from a gap. The caller keeps its floor; it neither quarantines nor re-authorizes.
    #[test]
    fn all_below_floor_is_no_change_not_a_gap() {
        let r = fold(&[linked(1), linked(2)], 5, Some(&id(5)));
        assert_eq!(r, FoldResult { head: None, gap: false }, "everything below floor → no candidate, no gap");
    }

    // ===== Weird / absurd / malformed input — fold must degrade gracefully, NEVER panic =====

    #[test]
    fn fold_version_zero_does_not_panic() {
        // v0 is invalid (chains start at v1) but a relay could serve one. Unanchored → gap, no panic.
        let e0 = Edition { version: 0, prev_hash: None, self_hash: id(0), created_at: 1, tiebreak_id: id(0xe0) };
        assert!(fold(&[e0], 0, None).gap, "a v0 'genesis' is not a valid anchor → gap, no panic");
    }

    #[test]
    fn fold_genesis_with_a_spurious_prev_is_unanchored() {
        // A v1 carrying a prev_hash (a real genesis has none) is a forged "genesis" → unanchored.
        let e1 = Edition { version: 1, prev_hash: Some(id(0xFF)), self_hash: id(1), created_at: 100, tiebreak_id: id(0xa1) };
        assert!(fold(&[e1], 0, None).gap, "v1 with a prev is not a real genesis → gap");
    }

    #[test]
    fn fold_near_u64_max_version_does_not_panic() {
        // A relay serves a wildly high version. fold uses it as a key; the +1 increment lives in the
        // producer, not here, so no overflow. Must not panic from any floor.
        let big = Edition { version: u64::MAX, prev_hash: None, self_hash: id(9), created_at: 1, tiebreak_id: id(0xff) };
        assert!(fold(&[big.clone()], 0, None).gap, "u64::MAX is not a genesis → gap, no overflow");
        let r = fold(&[big], u64::MAX, Some(&id(9)));
        assert!(!r.gap, "re-presenting the held u64::MAX floor is anchored, no overflow in the walk");
    }

    #[test]
    fn fold_a_million_version_gap_holds_at_the_prefix() {
        let far = Edition { version: 1_000_000, prev_hash: Some(id(2)), self_hash: id(0x77), created_at: 200, tiebreak_id: id(0xb7) };
        let r = fold(&[linked(1), far], 0, None);
        assert_eq!(r.head.map(|i| [1u64, 1_000_000][i]), Some(1), "head stays at the genesis prefix");
        assert!(r.gap, "a million-version jump is a gap, not a silent advance");
    }

    #[test]
    fn fold_mass_identical_duplicates_collapse() {
        let e = linked(1);
        let r = fold(&[e.clone(), e.clone(), e.clone(), e], 0, None);
        assert_eq!(r.head, Some(0));
        assert!(!r.gap, "N identical genesis copies collapse to one, no gap");
    }

    #[test]
    fn fold_a_large_noisy_scrambled_input_does_not_panic() {
        // A contiguous v1..v50 chain buried in duplicates and reversed arrival order.
        let mut eds: Vec<Edition> = (1..=50).map(linked).collect();
        eds.extend((1..=50).map(linked)); // every edition twice
        eds.reverse();
        let r = fold(&eds, 0, None);
        assert_eq!(r.head.map(|i| eds[i].version), Some(50), "folds to v50 through all the noise");
        assert!(!r.gap);
    }

    #[test]
    fn bootstrap_head_on_pathological_inputs() {
        assert_eq!(bootstrap_head(&[], 0), None, "empty → None");
        assert_eq!(bootstrap_head(&[linked(1), linked(2)], 999), None, "all below floor → None");
        let v0 = Edition { version: 0, prev_hash: None, self_hash: id(0), created_at: 1, tiebreak_id: id(0xe0) };
        assert!(bootstrap_head(&[v0], 0).is_some(), "a v0 edition still surfaces (≥ floor 0), no panic");
    }

    #[test]
    fn unanchored_head_is_flagged_as_gap() {
        // A hostile relay serves only v5 with a forged prev_hash, withholding v1..v4. The head is
        // returned but MUST be flagged gap=true — it isn't anchored to genesis, so a tracking client
        // fails closed instead of installing a possibly forged/rolled-back edition.
        let e5 = Edition { version: 5, prev_hash: Some(id(0xFF)), self_hash: id(5), created_at: 500, tiebreak_id: id(0xa5) };
        assert!(fold(&[e5], 0, None).gap, "a lone non-genesis edition is unanchored → gap");

        // With the held floor + its hash, a genuine v6 linking to v5 IS anchored (no gap).
        let floor_hash = id(0x55);
        let e6 = Edition { version: 6, prev_hash: Some(floor_hash), self_hash: id(6), created_at: 600, tiebreak_id: id(0xa6) };
        let r = fold(&[e6], 5, Some(&floor_hash));
        assert_eq!(r, FoldResult { head: Some(0), gap: false }, "v6 linking to the held v5 hash is anchored");

        // A v6 whose prev_hash does NOT match the held floor is unanchored → gap.
        let e6_bad = Edition { version: 6, prev_hash: Some(id(0xAB)), self_hash: id(6), created_at: 600, tiebreak_id: id(0xa6) };
        assert!(fold(&[e6_bad], 5, Some(&floor_hash)).gap, "v6 not linking to the floor is a gap");
    }

    /// At the FLOOR version, a re-presented edition must be the exact one we hold.
    /// A relay that withholds our floor edition A and serves a DIFFERENT same-version fork B must be
    /// rejected (gap → fail closed) so it can't silently swap our committed head. The genuine re-anchor
    /// case (same inner edition → identical self_hash) still anchors cleanly.
    #[test]
    fn tracking_rejects_a_forked_floor_edition() {
        let a_hash = id(0xAA);
        // Re-presenting OUR floor edition (same self_hash) → anchored, no gap (the legit re-anchor path).
        let a = Edition { version: 5, prev_hash: Some(id(4)), self_hash: a_hash, created_at: 500, tiebreak_id: id(0xa5) };
        assert!(!fold(&[a], 5, Some(&a_hash)).gap, "re-presenting our own floor edition is anchored");
        // A DIFFERENT edition at the floor version (a withheld-original fork) → gap, fail closed.
        let b = Edition { version: 5, prev_hash: Some(id(4)), self_hash: id(0xBB), created_at: 600, tiebreak_id: id(0xb5) };
        assert!(fold(&[b], 5, Some(&a_hash)).gap, "a different same-version edition is a fork → rejected, not anchored");
    }

    /// Refuse-downgrade: an edition below the floor is ignored entirely.
    #[test]
    fn refuses_to_downgrade_below_floor() {
        let e1 = Edition { version: 1, prev_hash: None, self_hash: id(1), created_at: 100, tiebreak_id: id(0xa1) };
        let e2 = Edition { version: 2, prev_hash: Some(id(1)), self_hash: id(2), created_at: 101, tiebreak_id: id(0xa2) };
        // Floor already at 2 (we hold v2 = self_hash id(2)) → only v2 is a candidate; v1 is a downgrade and
        // dropped. Pass the held floor hash, as production does — the ==floor anchor now verifies it.
        let r = fold(&[e1, e2], 2, Some(&id(2)));
        assert_eq!(r.head, Some(1));
        assert!(!r.gap);
    }

    /// Equal-version fork resolves to the deterministic tiebreak winner (lower created_at, then id),
    /// not to arrival order — so every client converges on the same head.
    #[test]
    fn equal_version_fork_resolves_by_lower_inner_id_not_created_at() {
        // Two distinct v1 editions. Winner = lower inner edition id; `created_at` is IGNORED, so an author
        // can't set created_at=0 to force a win. `a` has the LATER created_at but the LOWER id → `a` wins,
        // proving created_at is not the lever (the anti-gaming fix).
        let a = Edition { version: 1, prev_hash: None, self_hash: id(0xAA), created_at: 999, tiebreak_id: id(0x01) };
        let b = Edition { version: 1, prev_hash: None, self_hash: id(0xBB), created_at: 0, tiebreak_id: id(0x02) };
        assert_eq!(fold(&[a.clone(), b.clone()], 0, None).head, Some(0), "lower id wins even though `a` has the later created_at");
        assert_eq!(fold(&[b, a], 0, None).head, Some(1), "and it's independent of arrival order");
    }

    /// A missing or mismatched predecessor is a gap: the head stays at the highest CONTIGUOUS version
    /// and `gap` flags the break (caller fails closed + refetches).
    #[test]
    fn detects_a_gap_in_the_chain() {
        let e1 = Edition { version: 1, prev_hash: None, self_hash: id(1), created_at: 100, tiebreak_id: id(0xa1) };
        // v3 present but v2 missing → not contiguous from v1.
        let e3 = Edition { version: 3, prev_hash: Some(id(2)), self_hash: id(3), created_at: 102, tiebreak_id: id(0xa3) };
        let r = fold(&[e1.clone(), e3], 0, None);
        assert_eq!(r.head, Some(0), "head stays at the highest contiguous version (v1)");
        assert!(r.gap, "the v2 gap is reported");

        // Present-but-wrong prev_hash is also a gap (a forked/forged link).
        let e2_bad = Edition { version: 2, prev_hash: Some(id(0xFF)), self_hash: id(2), created_at: 101, tiebreak_id: id(0xa2) };
        let r2 = fold(&[e1.clone(), e2_bad], 0, None);
        assert_eq!(r2.head, Some(0));
        assert!(r2.gap, "a wrong prev_hash link does not advance the head");
    }
}
