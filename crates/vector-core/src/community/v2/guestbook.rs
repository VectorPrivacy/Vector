//! CORD-02 §5 Guestbook Plane — membership motion, coalesced flat.
//!
//! One Stream per Community (community_root-keyed,
//! [`super::derive::guestbook_group_key`]), carrying ONLY membership motion:
//! self-signed Joins/Leaves (3306), authorized Kicks (3309), and
//! refounder-signed post-Refounding Snapshots (3312) — never messages, never
//! authority (a Ban lives on the Control Plane). The plane is *off-consensus*:
//! nothing in Control or Chat depends on it, so it loads last and can lag
//! without harm.
//!
//! Everything here is PURE — no DB, no network, no clock reads. The +1h
//! forward-clock rule takes `now_ms` as a parameter, and the two authority
//! questions arrive from the caller's Control Plane fold: `can_kick` (the
//! KICK bit + strict outrank) and `snapshot_authority` (the npub whose
//! Refounding minted the epoch being folded).
//!
//! Guestbook seals are ENCRYPTED (20013) by spec: a plaintext seal would make
//! a member's signed membership record liftable as a standalone public
//! artifact, so [`parse_guestbook_event`] rejects the plaintext form outright.

use std::collections::{BTreeMap, BTreeSet};

use nostr_sdk::prelude::{Event, Keys, PublicKey, Tag, TagKind, Timestamp, UnsignedEvent};

use super::super::edition::AuthorityCitation;
use super::derive::GroupKey;
use super::kind;
use super::stream::{self, OpenedStream, SealForm, StreamError};

/// Entries dated further than this ahead of the receiver's clock are dropped
/// outright (CORD-02 §5) — ample for deep clock skew, and the deterrent
/// against squatting "latest" with a forged future date.
pub const MAX_FUTURE_MS: u64 = 3_600_000;

/// Snapshot chunk size: 400 members per event (CORD-02 §5). 400 hex pubkeys of
/// JSON is ~27 KB — comfortably inside the NIP-44 65,535-byte cap at every
/// nesting layer, with headroom for the envelope.
pub const SNAPSHOT_CHUNK: usize = 400;

const TAG_INVITE: &str = "invite";
const TAG_TARGET: &str = "p";
const TAG_SNAP: &str = "snap";

const VERB_JOIN: &str = "join";
const VERB_LEAVE: &str = "leave";

/// Errors from the guestbook layer (envelope errors ride inside).
#[derive(Debug)]
pub enum GuestbookError {
    Stream(StreamError),
    /// The rumor kind isn't a guestbook kind (3306 / 3309 / 3312).
    NotGuestbook(u16),
    /// A guestbook rumor arrived in a plaintext seal — CORD-02 §5 requires the
    /// encrypted form (a plaintext seal is a liftable, publicly verifiable
    /// membership record), so a strict reader drops it.
    NotEncryptedSealed,
    /// A 3306's content isn't exactly `"join"` or `"leave"` — the verb IS the
    /// state, so anything else is malformed, never interpreted.
    BadVerb,
    MissingTag(&'static str),
    /// A state-bearing tag appears more than once — ambiguous, rejected.
    DuplicateTag(&'static str),
    /// A state-bearing tag is present but unparseable (bad target hex, bad
    /// snap id / chunk indices).
    BadTag(&'static str),
    /// Snapshot content isn't a JSON array.
    BadSnapshotContent,
}

impl std::fmt::Display for GuestbookError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GuestbookError::Stream(e) => write!(f, "stream: {e}"),
            GuestbookError::NotGuestbook(k) => write!(f, "rumor kind {k} is not a guestbook event"),
            GuestbookError::NotEncryptedSealed => write!(f, "guestbook events must ride an encrypted seal"),
            GuestbookError::BadVerb => write!(f, "3306 content is not exactly join/leave"),
            GuestbookError::MissingTag(t) => write!(f, "missing guestbook tag: {t}"),
            GuestbookError::DuplicateTag(t) => write!(f, "duplicate guestbook tag: {t}"),
            GuestbookError::BadTag(t) => write!(f, "unparseable guestbook tag: {t}"),
            GuestbookError::BadSnapshotContent => write!(f, "snapshot content is not a JSON array"),
        }
    }
}

impl std::error::Error for GuestbookError {}

impl From<StreamError> for GuestbookError {
    fn from(e: StreamError) -> Self {
        GuestbookError::Stream(e)
    }
}

// ── Rumor builders ───────────────────────────────────────────────────────────

/// A self-signed Join, optionally echoing the invite attribution from the
/// bundle that admitted the author (`["invite", creator, label]`, CORD-05 §1).
pub fn build_join_rumor(author: PublicKey, invite_attribution: Option<(&str, &str)>, at_ms: u64) -> UnsignedEvent {
    let mut tags = Vec::new();
    if let Some((creator, label)) = invite_attribution {
        tags.push(Tag::custom(
            TagKind::Custom(TAG_INVITE.into()),
            [creator.to_string(), label.to_string()],
        ));
    }
    stream::build_rumor_ms(kind::JOIN_LEAVE, author, VERB_JOIN, tags, at_ms)
}

/// A self-signed Leave.
pub fn build_leave_rumor(author: PublicKey, at_ms: u64) -> UnsignedEvent {
    stream::build_rumor_ms(kind::JOIN_LEAVE, author, VERB_LEAVE, vec![], at_ms)
}

/// An admin-signed Kick naming its target and citing the Grant it acts under
/// (the `vac`, CORD-04 §5) — absent when the owner acts (supreme, no grant to
/// cite). Whether it's *honored* is the reader's call ([`coalesce`]'s
/// `can_kick`), never the writer's.
pub fn build_kick_rumor(
    admin: PublicKey,
    target: PublicKey,
    citation: Option<&AuthorityCitation>,
    at_ms: u64,
) -> UnsignedEvent {
    let mut tags = vec![Tag::public_key(target)];
    if let Some(c) = citation {
        tags.push(c.to_tag());
    }
    stream::build_rumor_ms(kind::KICK, admin, "", tags, at_ms)
}

/// Refounder-signed snapshot rumors seeding a new epoch's Guestbook: present
/// members only, chunked at [`SNAPSHOT_CHUNK`], every chunk carrying
/// `["snap", <id>, <i>, <n>]` (1-based) and ONE shared timestamp — the
/// one-id-one-time invariant is what lets readers reject torn chunk sets.
/// No survivors still yields one empty chunk, so the Refounding's guestbook
/// step is observable either way.
pub fn build_snapshot_rumors(
    refounder: PublicKey,
    members: &[PublicKey],
    snapshot_id: [u8; 32],
    at_ms: u64,
) -> Vec<UnsignedEvent> {
    let id_hex = crate::simd::hex::bytes_to_hex_32(&snapshot_id);
    let chunks: Vec<&[PublicKey]> = if members.is_empty() {
        vec![&[]]
    } else {
        members.chunks(SNAPSHOT_CHUNK).collect()
    };
    let n = chunks.len();
    chunks
        .iter()
        .enumerate()
        .map(|(idx, chunk)| {
            let hexes: Vec<String> = chunk.iter().map(|p| p.to_hex()).collect();
            let content = serde_json::to_string(&hexes).expect("a string array always serializes");
            let tags = vec![Tag::custom(
                TagKind::Custom(TAG_SNAP.into()),
                [id_hex.clone(), (idx + 1).to_string(), n.to_string()],
            )];
            stream::build_rumor_ms(kind::SNAPSHOT, refounder, &content, tags, at_ms)
        })
        .collect()
}

/// Seal a guestbook rumor (encrypted form) into a wrap at `guestbook_pk`.
/// Local-keys convenience; bunker accounts use [`stream::seal_content`] +
/// their remote signer + [`stream::wrap_seal`] for identical wire output.
pub fn seal_guestbook_rumor(
    rumor: &UnsignedEvent,
    group: &GroupKey,
    author_keys: &Keys,
    wrap_at: Timestamp,
) -> Result<(Event, Keys), GuestbookError> {
    let seal = stream::build_seal(rumor, SealForm::Encrypted, group, author_keys)?;
    Ok(stream::wrap_seal(&seal, group, stream::KIND_WRAP, wrap_at)?)
}

// ── Parse ────────────────────────────────────────────────────────────────────

/// One parsed guestbook event: the entry plus the identity the coalesce
/// tie-break runs on — the INNER rumor id, never the wrap's (a wrap id differs
/// per re-wrap, and two clients holding different wraps of one rumor would
/// fork on ties).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestbookEvent {
    /// The verified inner rumor id ([`OpenedStream::rumor_id`]).
    pub rumor_id: [u8; 32],
    pub entry: GuestbookEntry,
}

/// The typed guestbook entries (CORD-02 §5). Authors (`member` / `actor` /
/// `refounder`) are the seal-verified real keys, proven by
/// [`stream::open_wrap`] before parsing ever starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuestbookEntry {
    Join {
        member: PublicKey,
        /// Invite attribution echoed from the bundle: `(creator hex, label)`.
        /// Advisory metadata, carried verbatim — never validated here.
        invited_by: Option<(String, String)>,
        at_ms: u64,
    },
    Leave {
        member: PublicKey,
        at_ms: u64,
    },
    Kick {
        actor: PublicKey,
        target: PublicKey,
        /// The Grant the actor claims to act under; `None` covers both the
        /// owner (no grant to cite) and a corrupt `vac` — the verifier treats
        /// either as uncited, never trusting a malformed citation.
        citation: Option<AuthorityCitation>,
        at_ms: u64,
    },
    Snapshot {
        refounder: PublicKey,
        members: Vec<PublicKey>,
        snapshot_id: [u8; 32],
        /// `(i, n)` — 1-based chunk index over the chunk count.
        chunk: (u32, u32),
        at_ms: u64,
    },
}

impl GuestbookEntry {
    /// The entry's millisecond event time (CORD-02 §4).
    pub fn at_ms(&self) -> u64 {
        match self {
            GuestbookEntry::Join { at_ms, .. }
            | GuestbookEntry::Leave { at_ms, .. }
            | GuestbookEntry::Kick { at_ms, .. }
            | GuestbookEntry::Snapshot { at_ms, .. } => *at_ms,
        }
    }
}

/// Parse a guestbook event from an ALREADY-VERIFIED [`OpenedStream`] (one
/// produced by [`stream::open_wrap`], which proved the seal signature, the
/// author binding, the rumor id, and the strict `ms`). Strict on the seal
/// form: guestbook rumors MUST ride encrypted seals (CORD-02 §5).
pub fn parse_guestbook_event(opened: &OpenedStream) -> Result<GuestbookEvent, GuestbookError> {
    if opened.seal_form != SealForm::Encrypted {
        return Err(GuestbookError::NotEncryptedSealed);
    }
    let rumor = &opened.rumor;
    let at_ms = opened.at_ms;

    let entry = match rumor.kind.as_u16() {
        kind::JOIN_LEAVE => match rumor.content.as_str() {
            VERB_JOIN => {
                let invited_by = rumor.tags.iter().find_map(|t| {
                    let s = t.as_slice();
                    (s.len() >= 3 && s[0] == TAG_INVITE).then(|| (s[1].clone(), s[2].clone()))
                });
                GuestbookEntry::Join { member: opened.author, invited_by, at_ms }
            }
            VERB_LEAVE => GuestbookEntry::Leave { member: opened.author, at_ms },
            _ => return Err(GuestbookError::BadVerb),
        },
        kind::KICK => {
            // The target must come from a UNIQUE p tag: a second one makes
            // "who was kicked" pick-your-favorite — reject, never choose.
            let mut target: Option<PublicKey> = None;
            for t in rumor.tags.iter() {
                let s = t.as_slice();
                if s.len() >= 2 && s[0] == TAG_TARGET {
                    if target.is_some() {
                        return Err(GuestbookError::DuplicateTag(TAG_TARGET));
                    }
                    target = Some(PublicKey::from_hex(&s[1]).map_err(|_| GuestbookError::BadTag(TAG_TARGET))?);
                }
            }
            let target = target.ok_or(GuestbookError::MissingTag(TAG_TARGET))?;
            GuestbookEntry::Kick {
                actor: opened.author,
                target,
                citation: AuthorityCitation::from_tags(&rumor.tags),
                at_ms,
            }
        }
        kind::SNAPSHOT => {
            let mut snap: Option<(String, String, String)> = None;
            for t in rumor.tags.iter() {
                let s = t.as_slice();
                if s.len() >= 2 && s[0] == TAG_SNAP {
                    if snap.is_some() {
                        return Err(GuestbookError::DuplicateTag(TAG_SNAP));
                    }
                    if s.len() < 4 {
                        return Err(GuestbookError::BadTag(TAG_SNAP));
                    }
                    snap = Some((s[1].clone(), s[2].clone(), s[3].clone()));
                }
            }
            // The snap tag is load-bearing (the one-id-one-time consistency
            // rule keys on it), so any malformation rejects the whole event.
            let (id_hex, i_raw, n_raw) = snap.ok_or(GuestbookError::MissingTag(TAG_SNAP))?;
            if id_hex.len() != 64 || !id_hex.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(GuestbookError::BadTag(TAG_SNAP));
            }
            let snapshot_id = crate::simd::hex::hex_to_bytes_32(&id_hex);
            let i: u32 = i_raw.parse().map_err(|_| GuestbookError::BadTag(TAG_SNAP))?;
            let n: u32 = n_raw.parse().map_err(|_| GuestbookError::BadTag(TAG_SNAP))?;
            if i < 1 || i > n {
                return Err(GuestbookError::BadTag(TAG_SNAP));
            }
            let raw: Vec<serde_json::Value> =
                serde_json::from_str(&rumor.content).map_err(|_| GuestbookError::BadSnapshotContent)?;
            // Malformed member entries drop INDIVIDUALLY: a snapshot is
            // secondhand seeding and absence just means "no seed" (§5), so one
            // bad entry shouldn't cost the other 399 theirs — the gap heals by
            // observation or the victim's own fresh Join.
            let members = raw
                .iter()
                .filter_map(|v| v.as_str().and_then(|h| PublicKey::from_hex(h).ok()))
                .collect();
            GuestbookEntry::Snapshot {
                refounder: opened.author,
                members,
                snapshot_id,
                chunk: (i, n),
                at_ms,
            }
        }
        k => return Err(GuestbookError::NotGuestbook(k)),
    };

    Ok(GuestbookEvent { rumor_id: opened.rumor_id.to_bytes(), entry })
}

// ── Coalesce fold (CORD-02 §5) ───────────────────────────────────────────────

/// An npub's final coalesced state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Joined,
    Left,
    Kicked,
}

/// Whether the winning entry was the member's own word or a refounder's
/// secondhand snapshot seed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Firsthand,
    Snapshot,
}

/// One npub's folded guestbook state — the winning entry, flat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberState {
    pub verdict: Verdict,
    /// Millisecond time of the winning entry.
    pub at_ms: u64,
    pub source: Source,
    /// Invite attribution (firsthand Joins only): `(creator hex, label)`.
    pub invited_by: Option<(String, String)>,
    /// The winning entry's inner rumor id — the tie-break identity.
    pub rumor_id: [u8; 32],
}

/// Does `next` beat `prev`? Later ms wins; at a tie a firsthand entry beats a
/// snapshot seed (a member's own word over the refounder's attestation), then
/// the lower rumor id. The id tie-break is author-grindable — an accepted
/// residual: the coalesce is per-npub, so an author only ever grinds ties
/// against their own entries (CORD-02 §5).
fn supersedes(prev: &MemberState, next: &MemberState) -> bool {
    if next.at_ms != prev.at_ms {
        return next.at_ms > prev.at_ms;
    }
    if next.source != prev.source {
        return next.source == Source::Firsthand;
    }
    next.rumor_id < prev.rumor_id
}

fn apply(fold: &mut BTreeMap<PublicKey, MemberState>, member: PublicKey, next: MemberState) {
    match fold.get(&member) {
        Some(prev) if !supersedes(prev, &next) => {}
        _ => {
            fold.insert(member, next);
        }
    }
}

/// Coalesce parsed guestbook events flat: one final [`MemberState`] per npub
/// (CORD-02 §5), order-independent over `events`.
///
///   - entries dated more than [`MAX_FUTURE_MS`] ahead of `now_ms` drop
///     outright (the forged-future "latest" squat);
///   - a Kick is honored only when `can_kick(actor, target)` — the caller
///     closes that over its folded roster (KICK bit + strict outrank);
///   - a Snapshot is honored ONLY from `snapshot_authority`, the npub whose
///     Refounding minted the epoch (`None` — unknown or genesis — honors no
///     snapshots; there is deliberately NO owner fallback, an owner who didn't
///     mint the epoch has no snapshot authority over it);
///   - all chunks of one snapshot id must share one `(created_at, ms)`: the
///     first-seen chunk pins it, disagreeing chunks drop. Pinning `at_ms` pins
///     the pair — it's a bijection of `(created_at, ms)` under the strict
///     `0..=999` ms rule [`stream::open_wrap`] already enforced;
///   - a snapshot merely SEEDS `Joined` at its timestamp: any firsthand entry
///     (or authorized Kick) newer than it — or tying it — supersedes.
pub fn coalesce(
    events: &[GuestbookEvent],
    now_ms: u64,
    snapshot_authority: Option<&PublicKey>,
    can_kick: &dyn Fn(&PublicKey, &PublicKey) -> bool,
) -> BTreeMap<PublicKey, MemberState> {
    let horizon = now_ms.saturating_add(MAX_FUTURE_MS);
    let mut fold: BTreeMap<PublicKey, MemberState> = BTreeMap::new();

    for ev in events {
        if ev.entry.at_ms() > horizon {
            continue;
        }
        match &ev.entry {
            GuestbookEntry::Join { member, invited_by, at_ms } => apply(
                &mut fold,
                *member,
                MemberState {
                    verdict: Verdict::Joined,
                    at_ms: *at_ms,
                    source: Source::Firsthand,
                    invited_by: invited_by.clone(),
                    rumor_id: ev.rumor_id,
                },
            ),
            GuestbookEntry::Leave { member, at_ms } => apply(
                &mut fold,
                *member,
                MemberState {
                    verdict: Verdict::Left,
                    at_ms: *at_ms,
                    source: Source::Firsthand,
                    invited_by: None,
                    rumor_id: ev.rumor_id,
                },
            ),
            GuestbookEntry::Kick { actor, target, at_ms, .. } => {
                if !can_kick(actor, target) {
                    continue;
                }
                apply(
                    &mut fold,
                    *target,
                    MemberState {
                        verdict: Verdict::Kicked,
                        at_ms: *at_ms,
                        source: Source::Firsthand,
                        invited_by: None,
                        rumor_id: ev.rumor_id,
                    },
                );
            }
            GuestbookEntry::Snapshot { refounder, members, at_ms, .. } => {
                if snapshot_authority != Some(refounder) {
                    continue;
                }
                // Each authorized chunk seeds its own members at its own at_ms,
                // with NO cross-chunk consistency gate. CORD-02 §5: "chunks are
                // independently useful ... there is no torn state to defend
                // against." A first-seen timestamp pin would make a maliciously
                // torn snapshot resolve differently by relay delivery order,
                // breaking the deterministic-when-synced guarantee; the per-npub
                // fold below (latest-ms wins, firsthand beats snapshot, lower
                // rumor id ties) is commutative, so seeding every chunk converges.
                for m in members {
                    apply(
                        &mut fold,
                        *m,
                        MemberState {
                            verdict: Verdict::Joined,
                            at_ms: *at_ms,
                            source: Source::Snapshot,
                            invited_by: None,
                            rumor_id: ev.rumor_id,
                        },
                    );
                }
            }
        }
    }

    fold
}

// ── Complete Memberlist (CORD-02 §5) ─────────────────────────────────────────

/// The Complete Memberlist: coalesced `Joined` members ∪ observed authors,
/// minus the Banlist. `observed` maps author → the newest ms they were seen
/// publishing anywhere in the Community (an author seen publishing is
/// *observably present*, included even if their Join never arrived).
/// Observation counts FORWARD only: it re-enters an author whose activity is
/// strictly newer than their latest departure — a departed member's old
/// history can never resurrect them.
pub fn complete_memberlist(
    coalesced: &BTreeMap<PublicKey, MemberState>,
    observed: &BTreeMap<PublicKey, u64>,
    banlist: &BTreeSet<PublicKey>,
) -> BTreeSet<PublicKey> {
    let mut out = BTreeSet::new();
    for (pk, st) in coalesced {
        if st.verdict == Verdict::Joined && !banlist.contains(pk) {
            out.insert(*pk);
        }
    }
    for (pk, seen_ms) in observed {
        if banlist.contains(pk) {
            continue;
        }
        match coalesced.get(pk) {
            Some(st) if st.verdict != Verdict::Joined && *seen_ms <= st.at_ms => {}
            _ => {
                out.insert(*pk);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::super::{CommunityId, Epoch};
    use super::super::derive::guestbook_group_key;
    use super::*;

    /// A stable "receiver clock" for the fold tests, far above every event time.
    const NOW: u64 = 1_722_500_000_000;

    fn cid() -> CommunityId {
        CommunityId([0x33; 32])
    }

    fn group() -> GroupKey {
        guestbook_group_key(&[0x44; 32], &cid(), Epoch(0))
    }

    fn pk() -> PublicKey {
        Keys::generate().public_key()
    }

    fn always(_: &PublicKey, _: &PublicKey) -> bool {
        true
    }

    /// Full wire path: seal → wrap → open → parse.
    fn through(rumor: &UnsignedEvent, author: &Keys) -> GuestbookEvent {
        let g = group();
        let (wrap, _) = seal_guestbook_rumor(rumor, &g, author, Timestamp::from_secs(1_722_400_000)).unwrap();
        parse_guestbook_event(&stream::open_wrap(&wrap, &g).unwrap()).unwrap()
    }

    fn through_err(rumor: &UnsignedEvent, author: &Keys) -> GuestbookError {
        let g = group();
        let (wrap, _) = seal_guestbook_rumor(rumor, &g, author, Timestamp::from_secs(1_722_400_000)).unwrap();
        parse_guestbook_event(&stream::open_wrap(&wrap, &g).unwrap()).unwrap_err()
    }

    // Direct coalesce-input constructors: `id` fills the rumor id, so tie
    // ordering is choosable per test.
    fn join_ev(member: PublicKey, at_ms: u64, id: u8) -> GuestbookEvent {
        GuestbookEvent { rumor_id: [id; 32], entry: GuestbookEntry::Join { member, invited_by: None, at_ms } }
    }

    fn leave_ev(member: PublicKey, at_ms: u64, id: u8) -> GuestbookEvent {
        GuestbookEvent { rumor_id: [id; 32], entry: GuestbookEntry::Leave { member, at_ms } }
    }

    fn kick_ev(actor: PublicKey, target: PublicKey, at_ms: u64, id: u8) -> GuestbookEvent {
        GuestbookEvent { rumor_id: [id; 32], entry: GuestbookEntry::Kick { actor, target, citation: None, at_ms } }
    }

    fn snap_ev(
        refounder: PublicKey,
        members: Vec<PublicKey>,
        snap: u8,
        chunk: (u32, u32),
        at_ms: u64,
        id: u8,
    ) -> GuestbookEvent {
        GuestbookEvent {
            rumor_id: [id; 32],
            entry: GuestbookEntry::Snapshot { refounder, members, snapshot_id: [snap; 32], chunk, at_ms },
        }
    }

    #[test]
    fn join_and_leave_round_trip_through_the_stream() {
        let member = Keys::generate();
        let creator = pk().to_hex();

        let join = build_join_rumor(member.public_key(), Some((&creator, "Reddit")), 1_722_400_000_128);
        let ev = through(&join, &member);
        assert_eq!(
            ev.entry,
            GuestbookEntry::Join {
                member: member.public_key(),
                invited_by: Some((creator, "Reddit".into())),
                at_ms: 1_722_400_000_128,
            }
        );

        // Attribution is optional on a Join, and a Leave never carries any.
        let bare = through(&build_join_rumor(member.public_key(), None, 1_000), &member);
        assert!(matches!(bare.entry, GuestbookEntry::Join { invited_by: None, .. }));
        let leave = through(&build_leave_rumor(member.public_key(), 1_722_400_000_660), &member);
        assert_eq!(leave.entry, GuestbookEntry::Leave { member: member.public_key(), at_ms: 1_722_400_000_660 });
    }

    #[test]
    fn kick_round_trips_with_citation_and_the_inner_rumor_id() {
        let admin = Keys::generate();
        let target = pk();
        let cite = AuthorityCitation { entity_id: [0xab; 32], version: 7, edition_hash: [0xcd; 32] };
        let rumor = build_kick_rumor(admin.public_key(), target, Some(&cite), 1_722_410_000_301);

        let g = group();
        let (wrap, _) = seal_guestbook_rumor(&rumor, &g, &admin, Timestamp::from_secs(1_722_410_000)).unwrap();
        let opened = stream::open_wrap(&wrap, &g).unwrap();
        let ev = parse_guestbook_event(&opened).unwrap();

        assert_eq!(
            ev.entry,
            GuestbookEntry::Kick {
                actor: admin.public_key(),
                target,
                citation: Some(cite),
                at_ms: 1_722_410_000_301,
            }
        );
        // The tie-break identity is the INNER rumor id, never the wrap's
        // (which differs per re-wrap and would fork clients on ties).
        assert_eq!(ev.rumor_id, opened.rumor_id.to_bytes());
        assert_ne!(ev.rumor_id, wrap.id.to_bytes());

        // Uncited kick: the owner acting supreme — citation is simply None.
        let bare = through(&build_kick_rumor(admin.public_key(), target, None, 1_000), &admin);
        assert!(matches!(bare.entry, GuestbookEntry::Kick { citation: None, .. }));
    }

    #[test]
    fn snapshot_chunks_401_members_1_based_sharing_one_timestamp() {
        let refounder = Keys::generate();
        let members: Vec<PublicKey> = (0..401).map(|_| pk()).collect();
        let rumors = build_snapshot_rumors(refounder.public_key(), &members, [0x5a; 32], 1_722_500_000_000);
        assert_eq!(rumors.len(), 2);

        let parsed: Vec<GuestbookEvent> = rumors.iter().map(|r| through(r, &refounder)).collect();
        let (GuestbookEntry::Snapshot { members: m1, snapshot_id: id1, chunk: c1, at_ms: t1, refounder: r1 },
             GuestbookEntry::Snapshot { members: m2, snapshot_id: id2, chunk: c2, at_ms: t2, .. }) =
            (&parsed[0].entry, &parsed[1].entry)
        else {
            panic!("expected two snapshot entries");
        };
        assert_eq!((m1.len(), m2.len()), (400, 1));
        assert_eq!((*c1, *c2), ((1, 2), (2, 2)), "snap indices are 1-based");
        assert_eq!(id1, &[0x5a; 32]);
        assert_eq!(id1, id2);
        assert_eq!(t1, t2, "all chunks share one timestamp");
        assert_eq!(*r1, refounder.public_key());
        let all: Vec<PublicKey> = m1.iter().chain(m2.iter()).copied().collect();
        assert_eq!(all, members, "membership survives the chunking intact");

        // No survivors still publishes one observable (empty) chunk.
        let empty = build_snapshot_rumors(refounder.public_key(), &[], [0x5b; 32], 1_000);
        assert_eq!(empty.len(), 1);
        let ev = through(&empty[0], &refounder);
        assert!(matches!(ev.entry, GuestbookEntry::Snapshot { ref members, chunk: (1, 1), .. } if members.is_empty()));
    }

    #[test]
    fn plaintext_sealed_guestbook_events_are_rejected() {
        // An encrypted seal is what keeps a membership record from being a
        // liftable public artifact — the plaintext form must never be honored.
        let member = Keys::generate();
        let g = group();
        let rumor = build_join_rumor(member.public_key(), None, 1_000);
        let seal = stream::build_seal(&rumor, SealForm::Plaintext, &g, &member).unwrap();
        let (wrap, _) = stream::wrap_seal(&seal, &g, stream::KIND_WRAP, Timestamp::from_secs(1)).unwrap();
        let opened = stream::open_wrap(&wrap, &g).unwrap();
        assert!(matches!(parse_guestbook_event(&opened), Err(GuestbookError::NotEncryptedSealed)));
    }

    #[test]
    fn future_dated_entries_are_dropped_outright() {
        let m = pk();
        // A +2h forged date squatting "latest": dropped, so the honest +59min
        // leave (inside the skew allowance) holds the head.
        let squat = join_ev(m, NOW + 2 * 3_600_000, 1);
        let ok = leave_ev(m, NOW + 59 * 60_000, 2);
        let fold = coalesce(&[squat, ok], NOW, None, &always);
        assert_eq!(fold.get(&m).unwrap().verdict, Verdict::Left);

        // Exactly +1h is still allowed — only strictly-greater drops.
        let edge = join_ev(m, NOW + MAX_FUTURE_MS, 3);
        let fold = coalesce(&[edge], NOW, None, &always);
        assert_eq!(fold.get(&m).unwrap().verdict, Verdict::Joined);
    }

    #[test]
    fn latest_wins_per_npub_join_leave_rejoin() {
        let m = pk();
        let evs = [join_ev(m, 1_000, 1), leave_ev(m, 2_000, 2), join_ev(m, 3_000, 3)];

        let fold = coalesce(&evs, NOW, None, &always);
        let st = fold.get(&m).unwrap();
        assert_eq!(st.verdict, Verdict::Joined, "rejoin-after-leave is Joined");
        assert_eq!(st.at_ms, 3_000);

        // Order-independent: a shuffled delivery folds identically.
        let shuffled = [evs[2].clone(), evs[0].clone(), evs[1].clone()];
        assert_eq!(coalesce(&shuffled, NOW, None, &always), fold);

        // Without the rejoin, leave-after-join is Left.
        let fold = coalesce(&evs[..2], NOW, None, &always);
        assert_eq!(fold.get(&m).unwrap().verdict, Verdict::Left);
    }

    #[test]
    fn tie_firsthand_beats_snapshot_then_lower_rumor_id() {
        let m = pk();
        let auth = pk();

        // Firsthand vs snapshot at one instant: the member's own word wins even
        // though the snapshot holds the lower (would-otherwise-win) rumor id.
        let seed = snap_ev(auth, vec![m], 0x01, (1, 1), 5_000, 0x00);
        let leave = leave_ev(m, 5_000, 0xff);
        for evs in [[seed.clone(), leave.clone()], [leave, seed]] {
            let fold = coalesce(&evs, NOW, Some(&auth), &always);
            let st = fold.get(&m).unwrap();
            assert_eq!(st.verdict, Verdict::Left);
            assert_eq!(st.source, Source::Firsthand);
        }

        // Two firsthand entries at one instant: the lower rumor id wins.
        let join = join_ev(m, 6_000, 0x01);
        let leave = leave_ev(m, 6_000, 0x02);
        for evs in [[join.clone(), leave.clone()], [leave.clone(), join.clone()]] {
            let fold = coalesce(&evs, NOW, None, &always);
            assert_eq!(fold.get(&m).unwrap().verdict, Verdict::Joined, "lower id takes the tie");
        }
        // And symmetrically when the leave holds the lower id.
        let join = join_ev(m, 7_000, 0x02);
        let leave = leave_ev(m, 7_000, 0x01);
        let fold = coalesce(&[join, leave], NOW, None, &always);
        assert_eq!(fold.get(&m).unwrap().verdict, Verdict::Left);
    }

    #[test]
    fn kick_needs_the_callers_authority_verdict() {
        let admin = pk();
        let m = pk();
        let evs = [join_ev(m, 1_000, 1), kick_ev(admin, m, 2_000, 2)];

        let denied = coalesce(&evs, NOW, None, &|_, _| false);
        assert_eq!(denied.get(&m).unwrap().verdict, Verdict::Joined, "unauthorized kick is ignored");

        let granted = coalesce(&evs, NOW, None, &|actor, target| *actor == admin && *target == m);
        let st = granted.get(&m).unwrap();
        assert_eq!(st.verdict, Verdict::Kicked);
        assert_eq!(st.at_ms, 2_000);
    }

    #[test]
    fn snapshots_are_honored_only_from_the_epochs_refounder() {
        let refounder = pk();
        let impostor = pk();
        let m = pk();

        // Wrong npub: ignored entirely — no owner fallback, no partial honor.
        let forged = snap_ev(impostor, vec![m], 0x01, (1, 1), 5_000, 1);
        assert!(coalesce(&[forged], NOW, Some(&refounder), &always).is_empty());

        // Unknown authority: NO snapshots honored.
        let real = snap_ev(refounder, vec![m], 0x02, (1, 1), 5_000, 2);
        assert!(coalesce(&[real.clone()], NOW, None, &always).is_empty());

        // The minting refounder seeds Joined at the snapshot's time.
        let fold = coalesce(&[real], NOW, Some(&refounder), &always);
        let st = fold.get(&m).unwrap();
        assert_eq!(st.verdict, Verdict::Joined);
        assert_eq!(st.source, Source::Snapshot);
        assert_eq!(st.at_ms, 5_000);
    }

    #[test]
    fn snapshot_seeds_yield_to_newer_firsthand_but_not_older() {
        let refounder = pk();
        let m = pk();
        let seed = snap_ev(refounder, vec![m], 0x01, (1, 1), 5_000, 1);

        // A newer firsthand Leave supersedes the secondhand seed.
        let fold = coalesce(&[seed.clone(), leave_ev(m, 6_000, 2)], NOW, Some(&refounder), &always);
        assert_eq!(fold.get(&m).unwrap().verdict, Verdict::Left);

        // An OLDER firsthand Join does not override the newer seed.
        let fold = coalesce(&[join_ev(m, 4_000, 3), seed], NOW, Some(&refounder), &always);
        let st = fold.get(&m).unwrap();
        assert_eq!(st.source, Source::Snapshot);
        assert_eq!(st.at_ms, 5_000);
    }

    #[test]
    fn a_torn_snapshot_coalesces_order_independently() {
        // A maliciously (or buggily) torn snapshot — two chunks of one snap id
        // carrying DIFFERENT timestamps — must fold to the SAME member set
        // regardless of relay delivery order. (A first-seen timestamp pin made
        // the dropped chunk order-dependent, violating determinism; per CORD-02
        // §5 there is no torn state to defend against — every authorized chunk
        // seeds its members.)
        let refounder = pk();
        let (a, b, c) = (pk(), pk(), pk());
        let c1 = snap_ev(refounder, vec![a], 0x01, (1, 2), 5_000, 1);
        let torn = snap_ev(refounder, vec![b], 0x01, (2, 2), 6_000, 2);
        let other = snap_ev(refounder, vec![c], 0x02, (1, 1), 6_000, 3);

        let forward = coalesce(&[c1.clone(), torn.clone(), other.clone()], NOW, Some(&refounder), &always);
        let reverse = coalesce(&[other, torn, c1], NOW, Some(&refounder), &always);
        // Both orders seed all three members, identically.
        for m in [&a, &b, &c] {
            assert!(forward.contains_key(m) && reverse.contains_key(m), "every authorized chunk seeds its members");
        }
        assert_eq!(forward, reverse, "a torn snapshot must coalesce identically regardless of order");
    }

    #[test]
    fn complete_memberlist_merges_observation_forward_only_minus_banlist() {
        let (joined, left, silent, banned_joined, banned_observed) = (pk(), pk(), pk(), pk(), pk());
        let coalesced = coalesce(
            &[
                join_ev(joined, 1_000, 1),
                join_ev(left, 1_000, 2),
                leave_ev(left, 5_000, 3),
                join_ev(banned_joined, 1_000, 4),
            ],
            NOW,
            None,
            &always,
        );
        let banlist: BTreeSet<PublicKey> = [banned_joined, banned_observed].into();

        // Old activity (pre-Leave) never resurrects; silent observed authors
        // ARE members; the banlist subtracts unconditionally.
        let observed: BTreeMap<PublicKey, u64> = [(left, 4_000), (silent, 100), (banned_observed, 9_000)].into();
        let list = complete_memberlist(&coalesced, &observed, &banlist);
        assert!(list.contains(&joined));
        assert!(list.contains(&silent), "an observed author with no guestbook state is present");
        assert!(!list.contains(&left), "activity OLDER than the leave does not resurrect");
        assert!(!list.contains(&banned_joined));
        assert!(!list.contains(&banned_observed));

        // Activity strictly newer than the departure re-enters them.
        let observed: BTreeMap<PublicKey, u64> = [(left, 6_000)].into();
        let list = complete_memberlist(&coalesced, &observed, &banlist);
        assert!(list.contains(&left));
        // Equal-to-departure is not "newer" — still out.
        let observed: BTreeMap<PublicKey, u64> = [(left, 5_000)].into();
        assert!(!complete_memberlist(&coalesced, &observed, &banlist).contains(&left));
    }

    #[test]
    fn malformed_verbs_and_foreign_kinds_are_rejected() {
        let k = Keys::generate();
        // The verb is exact: case variants and paddings are malformed, never
        // normalized (a lenient reader would fold state a strict one dropped).
        for bad in ["JOIN", "Join", " join", "leave ", "", "rejoin"] {
            let rumor = stream::build_rumor_ms(kind::JOIN_LEAVE, k.public_key(), bad, vec![], 1_000);
            assert!(matches!(through_err(&rumor, &k), GuestbookError::BadVerb), "verb {bad:?} must reject");
        }
        let msg = stream::build_rumor_ms(kind::MESSAGE, k.public_key(), "hi", vec![], 1_000);
        assert!(matches!(through_err(&msg, &k), GuestbookError::NotGuestbook(9)));
    }

    #[test]
    fn kick_target_must_be_a_unique_valid_p_tag() {
        let k = Keys::generate();

        let dup = stream::build_rumor_ms(
            kind::KICK,
            k.public_key(),
            "",
            vec![Tag::public_key(pk()), Tag::public_key(pk())],
            1_000,
        );
        assert!(matches!(through_err(&dup, &k), GuestbookError::DuplicateTag("p")));

        let missing = stream::build_rumor_ms(kind::KICK, k.public_key(), "", vec![], 1_000);
        assert!(matches!(through_err(&missing, &k), GuestbookError::MissingTag("p")));

        let bad = stream::build_rumor_ms(
            kind::KICK,
            k.public_key(),
            "",
            vec![Tag::custom(TagKind::Custom("p".into()), ["not-hex".to_string()])],
            1_000,
        );
        assert!(matches!(through_err(&bad, &k), GuestbookError::BadTag("p")));
    }

    #[test]
    fn snapshot_snap_tag_and_content_are_strict() {
        let k = Keys::generate();
        let id_hex = crate::simd::hex::bytes_to_hex_32(&[0x5a; 32]);
        let snap_rumor = |content: &str, id: &str, i: &str, n: &str| {
            stream::build_rumor_ms(
                kind::SNAPSHOT,
                k.public_key(),
                content,
                vec![Tag::custom(
                    TagKind::Custom("snap".into()),
                    [id.to_string(), i.to_string(), n.to_string()],
                )],
                1_000,
            )
        };

        // 0-based, out-of-range, or unparseable indices reject the event.
        for (i, n) in [("0", "2"), ("3", "2"), ("abc", "2"), ("1", "x")] {
            let r = snap_rumor("[]", &id_hex, i, n);
            assert!(matches!(through_err(&r, &k), GuestbookError::BadTag("snap")), "snap {i}/{n} must reject");
        }
        // A bad snapshot id rejects; a missing snap tag rejects; non-array content rejects.
        let bad_id = snap_rumor("[]", "zz", "1", "1");
        assert!(matches!(through_err(&bad_id, &k), GuestbookError::BadTag("snap")));
        let no_tag = stream::build_rumor_ms(kind::SNAPSHOT, k.public_key(), "[]", vec![], 1_000);
        assert!(matches!(through_err(&no_tag, &k), GuestbookError::MissingTag("snap")));
        let not_array = snap_rumor("{}", &id_hex, "1", "1");
        assert!(matches!(through_err(&not_array, &k), GuestbookError::BadSnapshotContent));
    }

    #[test]
    fn snapshot_bad_member_entries_drop_individually() {
        let k = Keys::generate();
        let good = pk();
        // One valid member among garbage hex and a non-string: the good seed
        // survives — secondhand seeding never fails wholesale on one entry.
        let content = format!(r#"["{}","not-a-pubkey",17]"#, good.to_hex());
        let rumor = stream::build_rumor_ms(
            kind::SNAPSHOT,
            k.public_key(),
            &content,
            vec![Tag::custom(
                TagKind::Custom("snap".into()),
                [crate::simd::hex::bytes_to_hex_32(&[0x5a; 32]), "1".to_string(), "1".to_string()],
            )],
            1_000,
        );
        let ev = through(&rumor, &k);
        assert!(matches!(ev.entry, GuestbookEntry::Snapshot { ref members, .. } if members == &vec![good]));
    }
}
