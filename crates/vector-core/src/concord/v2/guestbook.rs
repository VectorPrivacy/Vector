//! CORD-02 §5: the Guestbook Plane — membership motion, coalesced flat.
//!
//! Self-signed Joins and Leaves, authorized Kicks, refounder snapshots; never
//! messages, never authority. Off-consensus: nothing in Control or Chat
//! depends on it, so it loads last and lags without harm. The coalesce keeps
//! one final state per npub (latest wins by millisecond time, ties by lower
//! rumor id), merges observably-present authors forward, and subtracts the
//! Banlist: the Complete Memberlist, deterministic when synced, self-healing
//! when not.

use std::collections::{HashMap, HashSet};

use nostr_sdk::prelude::*;

use super::edition::Citation;
use super::stream::TAG_MS;
use super::{kind, split_ms, MAX_FUTURE_SKEW_MS, SNAPSHOT_CHUNK_MEMBERS};

/// Optional Join attribution echoed from an invite bundle (CORD-05 §1) — what
/// makes per-link usage counters possible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InviteAttribution {
    pub creator_hex: String,
    pub label: String,
}

/// A parsed Guestbook rumor.
#[derive(Debug, Clone)]
pub enum GuestbookEvent {
    Join { attribution: Option<InviteAttribution> },
    Leave,
    Kick { target: PublicKey, citation: Option<Citation> },
    /// One chunk of a refounder snapshot: `chunk` is (i, n), 1-based.
    Snapshot { snapshot_id: String, chunk: (u32, u32), members: Vec<PublicKey> },
}

/// One npub's coalesced final state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberState {
    Present,
    Departed,
}

#[derive(Debug, Clone)]
struct EntryState {
    state: MemberState,
    ms: u64,
    rumor_id: EventId,
    /// A snapshot seed is *secondhand* — the refounder's attestation, not the
    /// member's own word; any self-signed entry or authorized Kick strictly
    /// newer supersedes it.
    secondhand: bool,
}

/// The Guestbook coalesce plus forward-only observation.
#[derive(Debug, Default)]
pub struct GuestbookFold {
    entries: HashMap<PublicKey, EntryState>,
    /// Latest observed activity per author (any valid decrypted event
    /// anywhere in the Community).
    observed: HashMap<PublicKey, u64>,
}

impl GuestbookFold {
    pub fn new() -> Self {
        Self::default()
    }

    /// The strict fold gate (CORD-02 §5): an entry dated over an hour ahead
    /// of the receiver's clock is dropped outright.
    fn future_gate(ms: u64, now_ms: u64) -> bool {
        ms <= now_ms.saturating_add(MAX_FUTURE_SKEW_MS)
    }

    fn coalesce(&mut self, who: PublicKey, state: MemberState, ms: u64, rumor_id: EventId, secondhand: bool) {
        let incoming = EntryState { state, ms, rumor_id, secondhand };
        match self.entries.get(&who) {
            None => {
                self.entries.insert(who, incoming);
            }
            Some(held) => {
                let wins = match incoming.ms.cmp(&held.ms) {
                    std::cmp::Ordering::Greater => true,
                    std::cmp::Ordering::Less => false,
                    // Tie: firsthand beats secondhand (a member's own word over
                    // the refounder's attestation), then the lower rumor id
                    // (the inner event's, never the wrap's, which differs per
                    // re-wrap). Per-npub, so an author only ever grinds ties
                    // against themselves.
                    std::cmp::Ordering::Equal => {
                        if incoming.secondhand != held.secondhand {
                            held.secondhand
                        } else {
                            incoming.rumor_id.as_bytes() < held.rumor_id.as_bytes()
                        }
                    }
                };
                if wins {
                    self.entries.insert(who, incoming);
                }
            }
        }
    }

    /// Apply a self-signed Join or Leave. `author` is seal-verified; `ms` is
    /// the rumor's reconstructed millisecond time (a malformed `ms` tag means
    /// the caller already dropped the entry).
    pub fn apply_join_leave(&mut self, author: PublicKey, join: bool, ms: u64, rumor_id: EventId, now_ms: u64) {
        if !Self::future_gate(ms, now_ms) {
            return;
        }
        let state = if join { MemberState::Present } else { MemberState::Departed };
        self.coalesce(author, state, ms, rumor_id, false);
    }

    /// Apply a Kick. `authorized` is the caller's `ControlFold::may_kick`
    /// verdict — an unauthorized Kick is dropped, degrading the removal, never
    /// breaking it.
    pub fn apply_kick(&mut self, target: PublicKey, authorized: bool, ms: u64, rumor_id: EventId, now_ms: u64) {
        if !authorized || !Self::future_gate(ms, now_ms) {
            return;
        }
        self.coalesce(target, MemberState::Departed, ms, rumor_id, false);
    }

    /// Apply one snapshot chunk (CORD-02 §5). Honored only from the npub
    /// whose Refounding minted the epoch — the caller verifies `author`
    /// before calling. Chunks are independently useful; a partial snapshot
    /// seeds whoever arrived and the rest heal by observation.
    pub fn apply_snapshot_chunk(&mut self, members: &[PublicKey], ms: u64, rumor_id: EventId, now_ms: u64) {
        if !Self::future_gate(ms, now_ms) {
            return;
        }
        for member in members {
            // Secondhand: merely seeds state at the snapshot's timestamp; a
            // firsthand entry newer (or tying) supersedes it in the coalesce.
            self.coalesce(*member, MemberState::Present, ms, rumor_id, true);
        }
    }

    /// Record observed presence: every valid decrypted event names its real
    /// author, and an author seen publishing is observably present — counted
    /// *forward only* (an author re-enters on activity newer than their
    /// latest Leave or Kick; departed history can never resurrect them).
    pub fn observe(&mut self, author: PublicKey, ms: u64, now_ms: u64) {
        if !Self::future_gate(ms, now_ms) {
            return;
        }
        let slot = self.observed.entry(author).or_insert(0);
        if ms > *slot {
            *slot = ms;
        }
    }

    /// One npub's coalesced state, observation merged.
    pub fn state(&self, who: &PublicKey) -> Option<MemberState> {
        let entry = self.entries.get(who);
        let observed = self.observed.get(who).copied();
        match (entry, observed) {
            (None, None) => None,
            (None, Some(_)) => Some(MemberState::Present),
            (Some(e), None) => Some(e.state),
            (Some(e), Some(obs_ms)) => {
                if e.state == MemberState::Departed && obs_ms > e.ms {
                    Some(MemberState::Present)
                } else {
                    Some(e.state)
                }
            }
        }
    }

    /// The Complete Memberlist: coalesced Guestbook, merged with observed
    /// authors, minus the Banlist. Sorted for determinism.
    pub fn members(&self, banlist: &HashSet<PublicKey>) -> Vec<PublicKey> {
        let mut all: HashSet<PublicKey> = self.entries.keys().copied().collect();
        all.extend(self.observed.keys().copied());
        let mut out: Vec<PublicKey> = all
            .into_iter()
            .filter(|pk| !banlist.contains(pk))
            .filter(|pk| self.state(pk) == Some(MemberState::Present))
            .collect();
        out.sort();
        out
    }
}

// ============================================================================
// Rumor build & parse
// ============================================================================

/// Build a Join (or Leave) rumor. Attribution rides Joins only (CORD-05 §1).
pub fn build_join_leave(
    author: PublicKey,
    join: bool,
    ms: u64,
    attribution: Option<&InviteAttribution>,
) -> UnsignedEvent {
    let mut tags = Vec::new();
    if join {
        if let Some(a) = attribution {
            tags.push(Tag::custom(
                TagKind::Custom("invite".into()),
                [a.creator_hex.clone(), a.label.clone()],
            ));
        }
    }
    super::stream::build_plane_rumor(author, kind::JOIN_LEAVE, if join { "join" } else { "leave" }, ms, tags)
}

/// Build a Kick rumor: names its target, cites the Grant it acts under.
pub fn build_kick(admin: PublicKey, target: &PublicKey, citation: &Citation, ms: u64) -> UnsignedEvent {
    let tags = vec![
        Tag::public_key(*target),
        Tag::custom(
            TagKind::Custom(super::edition::TAG_VAC.into()),
            [
                crate::simd::hex::bytes_to_hex_32(&citation.grant_eid),
                citation.grant_version.to_string(),
                crate::simd::hex::bytes_to_hex_32(&citation.grant_hash),
            ],
        ),
    ];
    super::stream::build_plane_rumor(admin, kind::KICK, "", ms, tags)
}

/// Build a snapshot as its chunk rumors: present members only, 400 per event,
/// one snapshot id and one timestamp across all `n` chunks.
pub fn build_snapshot(refounder: PublicKey, members: &[PublicKey], snapshot_id: &str, ms: u64) -> Vec<UnsignedEvent> {
    let chunks: Vec<&[PublicKey]> = members.chunks(SNAPSHOT_CHUNK_MEMBERS).collect();
    let n = chunks.len().max(1);
    let (secs, remainder) = split_ms(ms);
    chunks
        .iter()
        .enumerate()
        .map(|(i, chunk)| {
            let hexes: Vec<String> = chunk.iter().map(|m| m.to_hex()).collect();
            let content = serde_json::to_string(&hexes).expect("string array");
            let tags = vec![
                Tag::custom(TagKind::Custom(TAG_MS.into()), [remainder.to_string()]),
                Tag::custom(
                    TagKind::Custom("snap".into()),
                    [snapshot_id.to_string(), (i + 1).to_string(), n.to_string()],
                ),
            ];
            let mut rumor = EventBuilder::new(Kind::Custom(kind::SNAPSHOT), content)
                .tags(tags)
                .custom_created_at(Timestamp::from_secs(secs))
                .build(refounder);
            rumor.ensure_id();
            rumor
        })
        .collect()
}

fn tag_parts<'a>(tags: &'a Tags, name: &str) -> Option<Vec<&'a str>> {
    tags.iter()
        .find(|t| t.kind() == TagKind::Custom(name.into()))
        .map(|t| t.as_slice().iter().skip(1).map(|s| s.as_str()).collect())
}

/// Parse a Guestbook rumor (kinds 3306 / 3309 / 3312).
pub fn parse(rumor: &UnsignedEvent) -> Option<GuestbookEvent> {
    match rumor.kind.as_u16() {
        kind::JOIN_LEAVE => match rumor.content.as_str() {
            "join" => {
                let attribution = tag_parts(&rumor.tags, "invite").and_then(|p| {
                    Some(InviteAttribution {
                        creator_hex: p.first()?.to_string(),
                        label: p.get(1).unwrap_or(&"").to_string(),
                    })
                });
                Some(GuestbookEvent::Join { attribution })
            }
            "leave" => Some(GuestbookEvent::Leave),
            _ => None,
        },
        kind::KICK => {
            let target_hex = tag_parts(&rumor.tags, "p")?.first()?.to_string();
            let target = PublicKey::from_hex(&target_hex).ok()?;
            let citation = tag_parts(&rumor.tags, super::edition::TAG_VAC).and_then(|p| {
                Some(Citation {
                    grant_eid: crate::simd::hex::hex_to_bytes_32_checked(p.first()?)?,
                    grant_version: p.get(1)?.parse().ok()?,
                    grant_hash: crate::simd::hex::hex_to_bytes_32_checked(p.get(2)?)?,
                })
            });
            Some(GuestbookEvent::Kick { target, citation })
        }
        kind::SNAPSHOT => {
            let snap = tag_parts(&rumor.tags, "snap")?;
            let snapshot_id = snap.first()?.to_string();
            let chunk = (snap.get(1)?.parse().ok()?, snap.get(2)?.parse().ok()?);
            let hexes: Vec<String> = serde_json::from_str(&rumor.content).ok()?;
            let members: Vec<PublicKey> = hexes.iter().filter_map(|h| PublicKey::from_hex(h).ok()).collect();
            Some(GuestbookEvent::Snapshot { snapshot_id, chunk, members })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_722_500_000_000;

    fn pk() -> PublicKey {
        Keys::generate().public_key()
    }

    fn rid(seed: u8) -> EventId {
        EventId::from_slice(&[seed; 32]).unwrap()
    }

    #[test]
    fn latest_state_wins_per_npub() {
        let mut g = GuestbookFold::new();
        let alice = pk();
        g.apply_join_leave(alice, true, 1_000, rid(1), NOW);
        g.apply_join_leave(alice, false, 2_000, rid(2), NOW);
        assert_eq!(g.state(&alice), Some(MemberState::Departed));
        // An older join arriving late never resurrects.
        g.apply_join_leave(alice, true, 1_500, rid(3), NOW);
        assert_eq!(g.state(&alice), Some(MemberState::Departed));
        // A genuinely newer re-join does.
        g.apply_join_leave(alice, true, 3_000, rid(4), NOW);
        assert_eq!(g.state(&alice), Some(MemberState::Present));
    }

    #[test]
    fn ties_break_by_lower_rumor_id() {
        let mut g = GuestbookFold::new();
        let alice = pk();
        g.apply_join_leave(alice, true, 1_000, rid(9), NOW);
        g.apply_join_leave(alice, false, 1_000, rid(1), NOW);
        assert_eq!(g.state(&alice), Some(MemberState::Departed), "lower id wins the tie");
        // Order independence.
        let mut g2 = GuestbookFold::new();
        g2.apply_join_leave(alice, false, 1_000, rid(1), NOW);
        g2.apply_join_leave(alice, true, 1_000, rid(9), NOW);
        assert_eq!(g2.state(&alice), Some(MemberState::Departed));
    }

    #[test]
    fn far_future_entries_are_dropped_outright() {
        let mut g = GuestbookFold::new();
        let alice = pk();
        // A forged "latest forever" squat from the year 3000.
        g.apply_join_leave(alice, false, NOW + MAX_FUTURE_SKEW_MS + 1, rid(1), NOW);
        assert_eq!(g.state(&alice), None);
        // Within the hour of skew: accepted.
        g.apply_join_leave(alice, true, NOW + MAX_FUTURE_SKEW_MS, rid(2), NOW);
        assert_eq!(g.state(&alice), Some(MemberState::Present));
    }

    #[test]
    fn kick_departs_only_when_authorized() {
        let mut g = GuestbookFold::new();
        let target = pk();
        g.apply_join_leave(target, true, 1_000, rid(1), NOW);
        g.apply_kick(target, false, 2_000, rid(2), NOW);
        assert_eq!(g.state(&target), Some(MemberState::Present), "unauthorized Kick is dropped");
        g.apply_kick(target, true, 2_000, rid(2), NOW);
        assert_eq!(g.state(&target), Some(MemberState::Departed));
    }

    #[test]
    fn snapshot_seeds_and_firsthand_supersedes() {
        let mut g = GuestbookFold::new();
        let alice = pk();
        let bob = pk();
        g.apply_snapshot_chunk(&[alice, bob], 5_000, rid(1), NOW);
        assert_eq!(g.state(&alice), Some(MemberState::Present));

        // A member's own Leave strictly newer than the seed supersedes it.
        g.apply_join_leave(alice, false, 5_001, rid(2), NOW);
        assert_eq!(g.state(&alice), Some(MemberState::Departed));

        // A stale snapshot never overrides newer firsthand state.
        g.apply_snapshot_chunk(&[alice], 5_000, rid(3), NOW);
        assert_eq!(g.state(&alice), Some(MemberState::Departed));

        // An omitted member publishes a fresh Join: unsuppressable heal.
        let omitted = pk();
        g.apply_join_leave(omitted, true, 6_000, rid(4), NOW);
        assert_eq!(g.state(&omitted), Some(MemberState::Present));
    }

    #[test]
    fn observation_counts_forward_only() {
        let mut g = GuestbookFold::new();
        let alice = pk();
        g.apply_join_leave(alice, false, 5_000, rid(1), NOW);
        // Old history can never resurrect a departed member.
        g.observe(alice, 4_000, NOW);
        assert_eq!(g.state(&alice), Some(MemberState::Departed));
        // Activity newer than the departure re-enters them.
        g.observe(alice, 6_000, NOW);
        assert_eq!(g.state(&alice), Some(MemberState::Present));
        // An author never Guestbooked but seen publishing is present.
        let lurker = pk();
        g.observe(lurker, 1_000, NOW);
        assert_eq!(g.state(&lurker), Some(MemberState::Present));
    }

    #[test]
    fn memberlist_subtracts_banlist_and_sorts() {
        let mut g = GuestbookFold::new();
        let a = pk();
        let b = pk();
        let banned = pk();
        g.apply_join_leave(a, true, 1, rid(1), NOW);
        g.apply_join_leave(b, true, 2, rid(2), NOW);
        g.apply_join_leave(banned, true, 3, rid(3), NOW);
        let banlist: HashSet<PublicKey> = [banned].into_iter().collect();
        let members = g.members(&banlist);
        assert_eq!(members.len(), 2);
        assert!(!members.contains(&banned));
        let mut expect = vec![a, b];
        expect.sort();
        assert_eq!(members, expect);
    }

    #[test]
    fn rumor_build_parse_roundtrip() {
        let author = pk();
        let join = build_join_leave(author, true, NOW, Some(&InviteAttribution { creator_hex: "aa".into(), label: "Reddit".into() }));
        match parse(&join).unwrap() {
            GuestbookEvent::Join { attribution } => {
                let a = attribution.unwrap();
                assert_eq!(a.label, "Reddit");
            }
            _ => panic!("expected join"),
        }
        let leave = build_join_leave(author, false, NOW, None);
        assert!(matches!(parse(&leave).unwrap(), GuestbookEvent::Leave));

        let target = pk();
        let citation = Citation { grant_eid: [1; 32], grant_version: 3, grant_hash: [2; 32] };
        let kick = build_kick(author, &target, &citation, NOW);
        match parse(&kick).unwrap() {
            GuestbookEvent::Kick { target: t, citation: c } => {
                assert_eq!(t, target);
                assert_eq!(c, Some(citation));
            }
            _ => panic!("expected kick"),
        }
    }

    #[test]
    fn snapshot_chunks_share_id_and_timestamp() {
        let refounder = pk();
        let members: Vec<PublicKey> = (0..900).map(|_| pk()).collect();
        let chunks = build_snapshot(refounder, &members, "snap-1", NOW);
        assert_eq!(chunks.len(), 3, "900 members chunk at 400");
        let ts = chunks[0].created_at;
        for (i, chunk) in chunks.iter().enumerate() {
            assert_eq!(chunk.created_at, ts);
            match parse(chunk).unwrap() {
                GuestbookEvent::Snapshot { snapshot_id, chunk: (ci, cn), members: m } => {
                    assert_eq!(snapshot_id, "snap-1");
                    assert_eq!((ci as usize, cn as usize), (i + 1, 3));
                    assert_eq!(m.len(), if i < 2 { 400 } else { 100 });
                }
                _ => panic!("expected snapshot"),
            }
        }
    }
}
