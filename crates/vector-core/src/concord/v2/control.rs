//! CORD-04: the Control Plane fold — convergent, owner-rooted, refuse-downgrade.
//!
//! Clients fold every edition they hold into current state per entity, taking
//! the highest version whose chain is intact, judging each edition's signer
//! against the roster settled by the chain behind it. The fold is a
//! deterministic function of the accumulated candidate pool, so two clients
//! holding the same editions land on the same heads regardless of arrival
//! order; a persistent accepted floor refuses downgrades (a replayed stale
//! Grant or lifted Ban is rejected).

use std::collections::{BTreeMap, HashMap, HashSet};

use nostr_sdk::prelude::PublicKey;
use serde::{Deserialize, Serialize};

use super::derive::{banlist_locator, grant_locator, invite_links_locator, verify_owner};
use super::edition::{Edition, EditionError};
use super::roster::{Grant, Rank, Role, Roster};
use super::{
    perm, vsk, ChannelId, CommunityId, OwnerSalt, RoleId, DESCRIPTION_MAX_BYTES, NAME_MAX_BYTES,
    RELAYS_RECOMMENDED_MAX,
};

// ============================================================================
// Entity payloads
// ============================================================================

/// An encrypted-blob pointer: images never touch a media server in plaintext
/// (CORD-02 §6). Fetch, decrypt with `key`/`nonce`, verify `hash`; a swapped
/// blob fails closed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageRef {
    pub url: String,
    pub key: String,
    pub nonce: String,
    pub hash: String,
}

/// Community metadata (vsk 0), gated by `MANAGE_METADATA`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommunityMetadata {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub relays: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<ImageRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub banner: Option<ImageRef>,
    /// Client-extensible, folded atomically with the entity. Round-trip
    /// discipline: preserve what you don't understand.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom: Option<serde_json::Value>,
    /// Additive protocol fields ride unknown-field round-tripping.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl CommunityMetadata {
    pub fn validate(&self) -> Result<(), String> {
        if self.name.len() > NAME_MAX_BYTES {
            return Err(format!("community name exceeds {NAME_MAX_BYTES} bytes"));
        }
        if let Some(d) = &self.description {
            if d.len() > DESCRIPTION_MAX_BYTES {
                return Err(format!("description exceeds {DESCRIPTION_MAX_BYTES} bytes"));
            }
        }
        Ok(())
    }

    /// The relay set a client actually connects: truncated to the recommended
    /// cap, so an entity MUST stay usable when trimmed (CORD-02 §6).
    pub fn effective_relays(&self) -> &[String] {
        &self.relays[..self.relays.len().min(RELAYS_RECOMMENDED_MAX)]
    }
}

/// Channel metadata (vsk 2), gated by `MANAGE_CHANNELS`. The `channel_id` is
/// the edition's `eid`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChannelMetadata {
    pub name: String,
    pub private: bool,
    /// Terminal: the id is never reused and no later edition resurrects it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub deleted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom: Option<serde_json::Value>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl ChannelMetadata {
    pub fn validate(&self) -> Result<(), String> {
        if self.name.len() > NAME_MAX_BYTES {
            return Err(format!("channel name exceeds {NAME_MAX_BYTES} bytes"));
        }
        Ok(())
    }
}

fn parse_pubkey_list(content: &str) -> Option<Vec<PublicKey>> {
    let hexes: Vec<String> = serde_json::from_str(content).ok()?;
    hexes
        .iter()
        .map(|h| {
            crate::simd::hex::hex_to_bytes_32_checked(h)
                .and_then(|b| PublicKey::from_slice(&b).ok())
        })
        .collect()
}

// ============================================================================
// The fold
// ============================================================================

/// How a fold treats a chain that starts on a dangling `prev` (CORD-04 §1,
/// "Folding across a Refounding").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FoldMode {
    /// A client that held the prior chain: an unresolvable `prev` is a gap —
    /// fail closed for that entity, suspend, refetch.
    Tracking,
    /// A fresh joiner at a new epoch: accept the highest authority-verified
    /// head as baseline despite the dangling `prev` (there is nothing behind
    /// it to verify; signature + current-authority is the whole test).
    FreshJoin,
}

#[derive(Debug, Clone)]
struct AcceptedChain {
    /// Version → accepted edition, ascending; contiguous from its first key.
    editions: BTreeMap<u64, Edition>,
    /// Version → edition hash, mirror of `editions`.
    hashes: BTreeMap<u64, [u8; 32]>,
}

impl AcceptedChain {
    fn head(&self) -> Option<&Edition> {
        self.editions.values().next_back()
    }

    fn head_version(&self) -> u64 {
        self.editions.keys().next_back().copied().unwrap_or(0)
    }
}

/// The folded Control Plane of one Community.
pub struct ControlFold {
    community_id: CommunityId,
    owner: PublicKey,
    mode: FoldMode,
    /// Every candidate ever ingested: eid → version → candidates (deduped by
    /// edition hash). The fold is a deterministic function of this pool.
    pool: HashMap<[u8; 32], BTreeMap<u64, Vec<Edition>>>,
    /// The accepted floor — refuse-downgrade lives here.
    accepted: HashMap<[u8; 32], AcceptedChain>,
    /// Derived caches, rebuilt on every fold.
    roster: Roster,
    banned: HashSet<PublicKey>,
    dissolved: bool,
}

impl ControlFold {
    /// `owner` MUST already be verified against the `community_id` commitment
    /// (`derive::verify_owner`) — the fold roots every chain at it.
    pub fn new(community_id: CommunityId, owner: PublicKey, mode: FoldMode) -> Self {
        ControlFold {
            community_id,
            owner,
            mode,
            pool: HashMap::new(),
            accepted: HashMap::new(),
            roster: Roster::new(owner),
            banned: HashSet::new(),
            dissolved: false,
        }
    }

    /// Verify the owner commitment and construct. The entry point for a
    /// bundle-holding joiner.
    pub fn verified(
        community_id: CommunityId,
        owner: PublicKey,
        salt: &OwnerSalt,
        mode: FoldMode,
    ) -> Result<Self, String> {
        if !verify_owner(&community_id, &owner.to_bytes(), salt) {
            return Err("owner/salt do not reproduce the community_id".into());
        }
        Ok(Self::new(community_id, owner, mode))
    }

    // --- ingest ---

    /// Add seal-verified editions to the pool and refold. Returns the number
    /// of entities whose head changed.
    pub fn ingest(&mut self, editions: impl IntoIterator<Item = Edition>) -> usize {
        for ed in editions {
            if ed.vsk == vsk::DISSOLVED {
                // Wrong plane: the tombstone lives at the dissolved address,
                // ingested via `ingest_dissolution`.
                continue;
            }
            let versions = self.pool.entry(ed.eid).or_default();
            let slot = versions.entry(ed.version).or_default();
            if !slot.iter().any(|e| e.hash() == ed.hash()) {
                slot.push(ed);
            }
        }
        self.refold()
    }

    /// Honor an owner-signed dissolution tombstone (CORD-02 §9). Anything
    /// else at the coordinate is noise. Terminal: there is no un-dissolve.
    pub fn ingest_dissolution(&mut self, ed: &Edition) -> bool {
        if ed.vsk == vsk::DISSOLVED && ed.author == self.owner {
            self.dissolved = true;
        }
        self.dissolved
    }

    // --- accessors ---

    pub fn community_id(&self) -> &CommunityId {
        &self.community_id
    }

    pub fn owner(&self) -> &PublicKey {
        &self.owner
    }

    pub fn is_dissolved(&self) -> bool {
        self.dissolved
    }

    pub fn roster(&self) -> &Roster {
        &self.roster
    }

    /// Every honest client drops **every** event from a banned npub.
    pub fn is_banned(&self, pk: &PublicKey) -> bool {
        self.banned.contains(pk)
    }

    pub fn banlist(&self) -> &HashSet<PublicKey> {
        &self.banned
    }

    /// The folded Community metadata head, if any.
    pub fn metadata(&self) -> Option<CommunityMetadata> {
        let head = self.accepted.get(&self.community_id.0)?.head()?;
        serde_json::from_str(&head.content).ok()
    }

    /// Every defined Channel (deleted ones included — the flag is terminal).
    pub fn channels(&self) -> Vec<(ChannelId, ChannelMetadata)> {
        let mut out: Vec<(ChannelId, ChannelMetadata)> = self
            .accepted
            .values()
            .filter_map(|chain| {
                let head = chain.head()?;
                if head.vsk != vsk::CHANNEL_METADATA {
                    return None;
                }
                let meta: ChannelMetadata = serde_json::from_str(&head.content).ok()?;
                Some((ChannelId(head.eid), meta))
            })
            .collect();
        out.sort_by_key(|(id, _)| id.0);
        out
    }

    pub fn channel(&self, id: &ChannelId) -> Option<ChannelMetadata> {
        let head = self.accepted.get(&id.0)?.head()?;
        if head.vsk != vsk::CHANNEL_METADATA {
            return None;
        }
        serde_json::from_str(&head.content).ok()
    }

    /// The aggregate active public-invite set: every creator's Registry head,
    /// honored only while its author holds `CREATE_INVITE` (CORD-05 §5).
    pub fn registry_active_set(&self) -> Vec<PublicKey> {
        let mut set: Vec<PublicKey> = self
            .accepted
            .values()
            .filter_map(|chain| {
                let head = chain.head()?;
                if head.vsk != vsk::INVITE_REGISTRY {
                    return None;
                }
                if self.roster.permissions(&head.author) & perm::CREATE_INVITE == 0 {
                    return None;
                }
                parse_pubkey_list(&head.content)
            })
            .flatten()
            .collect();
        set.sort();
        set.dedup();
        set
    }

    /// Non-empty active set = a live link exists = the Community is Public.
    pub fn is_public(&self) -> bool {
        !self.registry_active_set().is_empty()
    }

    /// The current head edition of an entity, for publishing the next version.
    pub fn head(&self, eid: &[u8; 32]) -> Option<&Edition> {
        self.accepted.get(eid)?.head()
    }

    /// The accepted hash at `(eid, version)` — what a `vac` citation pins.
    pub fn accepted_hash(&self, eid: &[u8; 32], version: u64) -> Option<[u8; 32]> {
        self.accepted.get(eid)?.hashes.get(&version).copied()
    }

    /// Whether a Guestbook Kick is honored (CORD-02 §5 / CORD-04 §6): banlist
    /// drop, citation sync floor (owner exempt), `KICK` bit, strict outrank.
    pub fn may_kick(
        &self,
        actor: &PublicKey,
        citation: Option<&super::edition::Citation>,
        target: &PublicKey,
    ) -> bool {
        if self.banned.contains(actor) {
            return false;
        }
        if *actor != self.owner {
            let Some(c) = citation else { return false };
            match self.accepted_hash(&c.grant_eid, c.grant_version) {
                Some(h) if h == c.grant_hash => {}
                _ => return false,
            }
        }
        self.roster.can_act_on(actor, perm::KICK, target)
    }

    // --- the fold itself ---

    fn refold(&mut self) -> usize {
        if self.dissolved {
            return 0; // Death wins every race: nothing new is honored.
        }
        // Fixpoint: roster/banlist feed authorization, authorization feeds the
        // roster. Each pass is a deterministic function of (pool, previous
        // pass), starting from the accepted floor; the dependency depth is the
        // delegation depth, so a small bound converges.
        let mut changed_total: HashSet<[u8; 32]> = HashSet::new();
        for _ in 0..8 {
            let mut changed_this_pass = false;
            let eids: Vec<[u8; 32]> = self.pool.keys().copied().collect();
            for eid in eids {
                let proposal = self.walk_entity(&eid);
                let adopt = match (self.accepted.get(&eid), &proposal) {
                    (_, None) => false,
                    (None, Some(_)) => true,
                    (Some(old), Some(new)) => Self::chain_supersedes(&self.roster, old, new),
                };
                if adopt {
                    let new_chain = proposal.expect("checked");
                    let old_head = self.accepted.get(&eid).map(|c| c.head_version());
                    let differs = self
                        .accepted
                        .get(&eid)
                        .map(|c| c.hashes != new_chain.hashes)
                        .unwrap_or(true);
                    if differs {
                        self.accepted.insert(eid, new_chain);
                        changed_this_pass = true;
                        changed_total.insert(eid);
                        let _ = old_head;
                    }
                }
            }
            self.rebuild_roster();
            if !changed_this_pass {
                break;
            }
        }
        changed_total.len()
    }

    /// Refuse-downgrade with a deterministic same-version arbiter: a higher
    /// head wins outright; equal heads compare at the earliest divergence by
    /// authority, then the lower rumor id.
    fn chain_supersedes(roster: &Roster, old: &AcceptedChain, new: &AcceptedChain) -> bool {
        let (ov, nv) = (old.head_version(), new.head_version());
        if nv != ov {
            return nv > ov;
        }
        for (version, new_hash) in &new.hashes {
            match old.hashes.get(version) {
                Some(old_hash) if old_hash == new_hash => continue,
                Some(_) => {
                    let old_ed = &old.editions[version];
                    let new_ed = &new.editions[version];
                    return Self::candidate_beats(roster, new_ed, old_ed);
                }
                // Different starting floors (e.g. a compaction baseline vs a
                // full chain): keep what we hold.
                None => return false,
            }
        }
        false
    }

    /// The same-version tiebreak (CORD-04 §1): authority first, then the
    /// lower rumor id — never the author-settable timestamp.
    fn candidate_beats(roster: &Roster, a: &Edition, b: &Edition) -> bool {
        let (ra, rb) = (roster.rank(&a.author), roster.rank(&b.author));
        if ra.outranks(&rb) {
            return true;
        }
        if rb.outranks(&ra) {
            return false;
        }
        a.rumor_id.as_bytes() < b.rumor_id.as_bytes()
    }

    /// Recompute one entity's best intact chain from the pool: the highest
    /// version reachable, tie-broken at each step by authority then lower id.
    fn walk_entity(&self, eid: &[u8; 32]) -> Option<AcceptedChain> {
        let versions = self.pool.get(eid)?;
        let first_version = *versions.keys().next()?;

        // Anchor: version 1 (prev absent), or — fresh join after a compaction
        // — the pool's lowest version accepted baseline-style on a dangling
        // prev. A tracking client refuses the dangling start (a gap).
        let mut starts: Vec<(&Edition, u64)> = Vec::new();
        if let Some(v1) = versions.get(&1) {
            starts.extend(v1.iter().filter(|e| e.prev.is_none()).map(|e| (e, 1u64)));
        }
        if starts.is_empty() && self.mode == FoldMode::FreshJoin {
            if let Some(cands) = versions.get(&first_version) {
                starts.extend(cands.iter().map(|e| (e, first_version)));
            }
        }
        // If we already accepted a floor for this entity, its own start is
        // always a valid anchor (it may have been a baseline).
        if let Some(existing) = self.accepted.get(eid) {
            if let Some((v, ed)) = existing.editions.iter().next() {
                if !starts.iter().any(|(e, _)| e.hash() == ed.hash()) {
                    starts.push((ed, *v));
                }
            }
        }

        let mut best: Option<AcceptedChain> = None;
        for (start, start_version) in starts {
            if !self.authorize(start, None) {
                continue;
            }
            let mut chain = AcceptedChain { editions: BTreeMap::new(), hashes: BTreeMap::new() };
            chain.editions.insert(start_version, start.clone());
            chain.hashes.insert(start_version, start.hash());
            self.extend_chain(versions, &mut chain, start_version);
            best = match best {
                None => Some(chain),
                Some(current) => {
                    if Self::chain_supersedes(&self.roster, &current, &chain) {
                        Some(chain)
                    } else {
                        Some(current)
                    }
                }
            };
        }
        best
    }

    /// Greedy forward walk: at each next version pick the deterministic winner
    /// among chain-intact, authorized candidates. Greedy is convergent here
    /// because honest children only ever cite the winner's hash — a losing
    /// same-version sibling has no valid descendants to out-reach it.
    fn extend_chain(
        &self,
        versions: &BTreeMap<u64, Vec<Edition>>,
        chain: &mut AcceptedChain,
        mut at: u64,
    ) {
        loop {
            let head_hash = chain.hashes[&at];
            let prior = chain.editions.get(&at).cloned();
            let next = at + 1;
            let Some(candidates) = versions.get(&next) else { break };
            let mut eligible: Vec<&Edition> = candidates
                .iter()
                .filter(|e| e.prev == Some(head_hash) && self.authorize(e, prior.as_ref()))
                .collect();
            if eligible.is_empty() {
                break;
            }
            eligible.sort_by(|a, b| {
                if Self::candidate_beats(&self.roster, a, b) {
                    std::cmp::Ordering::Less
                } else {
                    std::cmp::Ordering::Greater
                }
            });
            let winner = eligible[0].clone();
            chain.hashes.insert(next, winner.hash());
            chain.editions.insert(next, winner);
            at = next;
        }
    }

    /// CORD-04 §5, judged against the *current* fixpoint state: seal-verified
    /// actor (already done upstream), banlist drop, citation sync floor, the
    /// required bit, and strict outranking per entity type.
    fn authorize(&self, ed: &Edition, prior: Option<&Edition>) -> bool {
        let author = ed.author;
        if self.banned.contains(&author) {
            return false;
        }

        // Citation: absent when the owner acts; required from everyone else,
        // and it must resolve (block-until-synced) before the action is
        // honored. A hash mismatch parks exactly like an unsynced one.
        if author != self.owner {
            let Some(citation) = &ed.citation else { return false };
            match self.accepted_hash(&citation.grant_eid, citation.grant_version) {
                Some(h) if h == citation.grant_hash => {}
                _ => return false, // parked: the pool retains it for later folds
            }
        }

        match ed.vsk {
            vsk::COMMUNITY_METADATA => {
                if ed.eid != self.community_id.0 {
                    return false;
                }
                if self.roster.permissions(&author) & perm::MANAGE_METADATA == 0 {
                    return false;
                }
                serde_json::from_str::<CommunityMetadata>(&ed.content)
                    .map(|m| m.validate().is_ok())
                    .unwrap_or(false)
            }
            vsk::ROLE => {
                let Ok(role) = serde_json::from_str::<Role>(&ed.content) else { return false };
                if role.role_id != ed.eid || role.validate().is_err() {
                    return false;
                }
                if !self.roster.may_place_role(&author, role.position) {
                    return false;
                }
                // Editing an existing Role: the actor must also outrank what
                // it currently is, or a junior could rewrite a senior Role.
                if let Some(prev) = prior {
                    if let Ok(existing) = serde_json::from_str::<Role>(&prev.content) {
                        if !self.roster.rank(&author).outranks(&Rank::Position(existing.position)) {
                            return false;
                        }
                    }
                }
                true
            }
            vsk::CHANNEL_METADATA => {
                let Ok(meta) = serde_json::from_str::<ChannelMetadata>(&ed.content) else {
                    return false;
                };
                if meta.validate().is_err() {
                    return false;
                }
                if self.roster.permissions(&author) & perm::MANAGE_CHANNELS == 0 {
                    return false;
                }
                // Deletion is terminal: nothing follows a deleted state.
                if let Some(prev) = prior {
                    if let Ok(existing) = serde_json::from_str::<ChannelMetadata>(&prev.content) {
                        if existing.deleted {
                            return false;
                        }
                    }
                }
                true
            }
            vsk::GRANT => {
                let Ok(grant) = serde_json::from_str::<Grant>(&ed.content) else { return false };
                let Ok(member) = PublicKey::from_slice(&grant.member) else { return false };
                if ed.eid != grant_locator(&self.community_id, &grant.member) {
                    return false;
                }
                let role_ids: Vec<RoleId> = grant.role_ids.iter().map(|r| RoleId(*r)).collect();
                self.roster.may_grant(&author, &member, &role_ids)
            }
            vsk::BANLIST => {
                if ed.eid != banlist_locator(&self.community_id) {
                    return false;
                }
                if self.roster.permissions(&author) & perm::BAN == 0 {
                    return false;
                }
                let Some(new_list) = parse_pubkey_list(&ed.content) else { return false };
                // The owner is unbannable, and every newly added target must
                // be strictly outranked.
                let prev_list: HashSet<PublicKey> = prior
                    .and_then(|p| parse_pubkey_list(&p.content))
                    .map(|v| v.into_iter().collect())
                    .unwrap_or_default();
                let actor_rank = self.roster.rank(&author);
                new_list.iter().all(|target| {
                    *target != self.owner
                        && (prev_list.contains(target) || actor_rank.outranks(&self.roster.rank(target)))
                })
            }
            vsk::INVITE_REGISTRY => {
                // Coordinate bound to the creator: nobody forges entries into
                // anyone else's Registry.
                if ed.eid != invite_links_locator(&self.community_id, &author.to_bytes()) {
                    return false;
                }
                if self.roster.permissions(&author) & perm::CREATE_INVITE == 0 {
                    return false;
                }
                parse_pubkey_list(&ed.content).is_some()
            }
            _ => false,
        }
    }

    fn rebuild_roster(&mut self) {
        let mut roster = Roster::new(self.owner);
        // Roles first (BTreeMap order via sorted eids keeps the cap
        // deterministic), then Grants.
        let mut role_heads: Vec<&Edition> = Vec::new();
        let mut grant_heads: Vec<&Edition> = Vec::new();
        let mut banlist_head: Option<&Edition> = None;
        for chain in self.accepted.values() {
            let Some(head) = chain.head() else { continue };
            match head.vsk {
                vsk::ROLE => role_heads.push(head),
                vsk::GRANT => grant_heads.push(head),
                vsk::BANLIST => banlist_head = Some(head),
                _ => {}
            }
        }
        role_heads.sort_by_key(|e| e.eid);
        for head in role_heads {
            if let Ok(role) = serde_json::from_str::<Role>(&head.content) {
                roster.insert_role(role);
            }
        }
        for head in grant_heads {
            if let Ok(grant) = serde_json::from_str::<Grant>(&head.content) {
                if let Ok(member) = PublicKey::from_slice(&grant.member) {
                    roster.apply_grant(member, grant.role_ids.iter().map(|r| RoleId(*r)).collect());
                }
            }
        }
        self.banned = banlist_head
            .and_then(|h| parse_pubkey_list(&h.content))
            .map(|v| v.into_iter().collect())
            .unwrap_or_default();
        self.roster = roster;
    }
}

/// Convenience: parse + fold errors surfaced together for transports.
pub fn parse_and_note(rumor: &nostr_sdk::prelude::UnsignedEvent) -> Result<Edition, EditionError> {
    super::edition::parse_edition(rumor)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::concord::v2::edition::{build_edition_rumor, parse_edition, Citation};
    use crate::concord::v2::roster::RoleScope;
    use nostr_sdk::prelude::Keys;

    struct TestCtx {
        cid: CommunityId,
        owner: Keys,
        fold: ControlFold,
    }

    fn ctx() -> TestCtx {
        let owner = Keys::generate();
        let salt = OwnerSalt([0x33; 32]);
        let cid = crate::concord::v2::derive::community_id(&owner.public_key().to_bytes(), &salt);
        let fold = ControlFold::verified(cid, owner.public_key(), &salt, FoldMode::Tracking).unwrap();
        TestCtx { cid, owner, fold }
    }

    fn edition(
        author: &Keys,
        entity_vsk: u8,
        eid: &[u8; 32],
        version: u64,
        prev: Option<&[u8; 32]>,
        content: &str,
        citation: Option<&Citation>,
    ) -> Edition {
        let rumor = build_edition_rumor(author.public_key(), entity_vsk, eid, version, prev, content, 1_700_000_000, citation);
        parse_edition(&rumor).unwrap()
    }

    fn role_json(id: u8, position: u32, permissions: u64) -> String {
        serde_json::to_string(&Role {
            role_id: [id; 32],
            name: "role".into(),
            position,
            permissions,
            scope: RoleScope::Server,
            color: 0,
        })
        .unwrap()
    }

    fn grant_json(member: &Keys, roles: &[u8]) -> String {
        serde_json::to_string(&Grant {
            member: member.public_key().to_bytes(),
            role_ids: roles.iter().map(|i| [*i; 32]).collect(),
        })
        .unwrap()
    }

    fn metadata_json(name: &str) -> String {
        format!("{{\"name\":\"{name}\",\"relays\":[\"wss://a\"]}}")
    }

    /// Owner founds: metadata v1 + #general v1 (genesis, CORD-02 §1).
    fn genesis(t: &mut TestCtx) -> ChannelId {
        let chan = ChannelId([0x77; 32]);
        let meta = edition(&t.owner, vsk::COMMUNITY_METADATA, &t.cid.0, 1, None, &metadata_json("Vector"), None);
        let general = edition(
            &t.owner,
            vsk::CHANNEL_METADATA,
            &chan.0,
            1,
            None,
            "{\"name\":\"general\",\"private\":false}",
            None,
        );
        t.fold.ingest([meta, general]);
        chan
    }

    #[test]
    fn genesis_folds() {
        let mut t = ctx();
        let chan = genesis(&mut t);
        assert_eq!(t.fold.metadata().unwrap().name, "Vector");
        assert_eq!(t.fold.channel(&chan).unwrap().name, "general");
        assert!(!t.fold.is_public(), "no live links at genesis: Private");
    }

    #[test]
    fn owner_verification_is_enforced() {
        let owner = Keys::generate();
        let salt = OwnerSalt([0x33; 32]);
        let cid = crate::concord::v2::derive::community_id(&owner.public_key().to_bytes(), &salt);
        let impostor = Keys::generate();
        assert!(ControlFold::verified(cid, impostor.public_key(), &salt, FoldMode::Tracking).is_err());
    }

    #[test]
    fn version_chain_folds_to_highest_intact() {
        let mut t = ctx();
        genesis(&mut t);
        let v1_hash = t.fold.head(&t.cid.0).unwrap().hash();
        let v2 = edition(&t.owner, vsk::COMMUNITY_METADATA, &t.cid.0, 2, Some(&v1_hash), &metadata_json("Renamed"), None);
        let v2_hash = v2.hash();
        let v3 = edition(&t.owner, vsk::COMMUNITY_METADATA, &t.cid.0, 3, Some(&v2_hash), &metadata_json("Renamed again"), None);
        // Arrival order must not matter.
        t.fold.ingest([v3.clone()]);
        assert_eq!(t.fold.metadata().unwrap().name, "Vector", "v3 dangles until v2 arrives");
        t.fold.ingest([v2]);
        assert_eq!(t.fold.metadata().unwrap().name, "Renamed again");
    }

    #[test]
    fn refuse_downgrade_ignores_replayed_stale_state() {
        let mut t = ctx();
        genesis(&mut t);
        let v1 = t.fold.head(&t.cid.0).unwrap().clone();
        let v2 = edition(&t.owner, vsk::COMMUNITY_METADATA, &t.cid.0, 2, Some(&v1.hash()), &metadata_json("Renamed"), None);
        t.fold.ingest([v2]);
        assert_eq!(t.fold.metadata().unwrap().name, "Renamed");
        // A relay replaying v1 must not move the head back.
        t.fold.ingest([v1]);
        assert_eq!(t.fold.metadata().unwrap().name, "Renamed");
    }

    #[test]
    fn unauthorized_editor_is_dropped() {
        let mut t = ctx();
        genesis(&mut t);
        let rando = Keys::generate();
        let v1_hash = t.fold.head(&t.cid.0).unwrap().hash();
        // Perfectly shaped, correctly chained, validly signed — and dropped:
        // the signer maps to no qualifying rank.
        let citation = Citation { grant_eid: [0; 32], grant_version: 1, grant_hash: [0; 32] };
        let forged = edition(&rando, vsk::COMMUNITY_METADATA, &t.cid.0, 2, Some(&v1_hash), &metadata_json("Owned"), Some(&citation));
        t.fold.ingest([forged]);
        assert_eq!(t.fold.metadata().unwrap().name, "Vector");
    }

    /// The full delegation flow: owner mints a Role, Grants it, and the
    /// grantee acts under a resolving citation.
    fn delegate_admin(t: &mut TestCtx, admin: &Keys, position: u32, permissions: u64) -> Citation {
        let role_ed = edition(&t.owner, vsk::ROLE, &[0x01; 32], 1, None, &role_json(0x01, position, permissions), None);
        let grant_eid = grant_locator(&t.cid, &admin.public_key().to_bytes());
        let grant_ed = edition(&t.owner, vsk::GRANT, &grant_eid, 1, None, &grant_json(admin, &[0x01]), None);
        let grant_hash = grant_ed.hash();
        t.fold.ingest([role_ed, grant_ed]);
        Citation { grant_eid, grant_version: 1, grant_hash }
    }

    #[test]
    fn delegated_admin_can_act_with_citation() {
        let mut t = ctx();
        genesis(&mut t);
        let admin = Keys::generate();
        let vac = delegate_admin(&mut t, &admin, 1, perm::MANAGE_METADATA);
        assert_eq!(t.fold.roster().rank(&admin.public_key()), Rank::Position(1));

        let v1_hash = t.fold.head(&t.cid.0).unwrap().hash();
        let rename = edition(&admin, vsk::COMMUNITY_METADATA, &t.cid.0, 2, Some(&v1_hash), &metadata_json("ByAdmin"), Some(&vac));
        t.fold.ingest([rename]);
        assert_eq!(t.fold.metadata().unwrap().name, "ByAdmin");
    }

    #[test]
    fn action_without_citation_is_dropped_unless_owner() {
        let mut t = ctx();
        genesis(&mut t);
        let admin = Keys::generate();
        delegate_admin(&mut t, &admin, 1, perm::MANAGE_METADATA);
        let v1_hash = t.fold.head(&t.cid.0).unwrap().hash();
        let rename = edition(&admin, vsk::COMMUNITY_METADATA, &t.cid.0, 2, Some(&v1_hash), &metadata_json("NoVac"), None);
        t.fold.ingest([rename]);
        assert_eq!(t.fold.metadata().unwrap().name, "Vector");
    }

    #[test]
    fn citation_parks_until_the_grant_syncs() {
        let mut t = ctx();
        genesis(&mut t);
        let admin = Keys::generate();

        // Build the grant material but DON'T ingest it yet.
        let role_ed = edition(&t.owner, vsk::ROLE, &[0x01; 32], 1, None, &role_json(0x01, 1, perm::MANAGE_METADATA), None);
        let grant_eid = grant_locator(&t.cid, &admin.public_key().to_bytes());
        let grant_ed = edition(&t.owner, vsk::GRANT, &grant_eid, 1, None, &grant_json(&admin, &[0x01]), None);
        let vac = Citation { grant_eid, grant_version: 1, grant_hash: grant_ed.hash() };

        let v1_hash = t.fold.head(&t.cid.0).unwrap().hash();
        let rename = edition(&admin, vsk::COMMUNITY_METADATA, &t.cid.0, 2, Some(&v1_hash), &metadata_json("Parked"), Some(&vac));
        t.fold.ingest([rename]);
        assert_eq!(t.fold.metadata().unwrap().name, "Vector", "block-until-synced");

        // The grant arrives: the parked action unparks on the next fold.
        t.fold.ingest([role_ed, grant_ed]);
        assert_eq!(t.fold.metadata().unwrap().name, "Parked");
    }

    #[test]
    fn forged_citation_hash_never_resolves() {
        let mut t = ctx();
        genesis(&mut t);
        let admin = Keys::generate();
        let real_vac = delegate_admin(&mut t, &admin, 1, perm::MANAGE_METADATA);
        let forged = Citation { grant_hash: [0xEE; 32], ..real_vac };
        let v1_hash = t.fold.head(&t.cid.0).unwrap().hash();
        let rename = edition(&admin, vsk::COMMUNITY_METADATA, &t.cid.0, 2, Some(&v1_hash), &metadata_json("Forged"), Some(&forged));
        t.fold.ingest([rename]);
        assert_eq!(t.fold.metadata().unwrap().name, "Vector");
    }

    #[test]
    fn demotion_is_never_grandfathered() {
        let mut t = ctx();
        genesis(&mut t);
        let admin = Keys::generate();
        let vac = delegate_admin(&mut t, &admin, 1, perm::MANAGE_METADATA);

        // Owner revokes (grant v2, empty roles).
        let grant_eid = vac.grant_eid;
        let g1_hash = t.fold.head(&grant_eid).unwrap().hash();
        let revoke = edition(&t.owner, vsk::GRANT, &grant_eid, 2, Some(&g1_hash), &grant_json(&admin, &[]), None);
        t.fold.ingest([revoke]);

        // The demoted admin's action citing the OLD (once-valid) grant is
        // dropped: rank resolves against the current refuse-downgrade roster.
        let v1_hash = t.fold.head(&t.cid.0).unwrap().hash();
        let rename = edition(&admin, vsk::COMMUNITY_METADATA, &t.cid.0, 2, Some(&v1_hash), &metadata_json("Stale"), Some(&vac));
        t.fold.ingest([rename]);
        assert_eq!(t.fold.metadata().unwrap().name, "Vector");
    }

    #[test]
    fn same_version_tie_breaks_by_authority_then_lower_id() {
        let mut t = ctx();
        genesis(&mut t);
        let admin = Keys::generate();
        let vac = delegate_admin(&mut t, &admin, 1, perm::MANAGE_METADATA);
        let v1_hash = t.fold.head(&t.cid.0).unwrap().hash();

        // Owner and admin race the same version: authority (owner) wins,
        // regardless of ingest order.
        let by_admin = edition(&admin, vsk::COMMUNITY_METADATA, &t.cid.0, 2, Some(&v1_hash), &metadata_json("AdminWins"), Some(&vac));
        let by_owner = edition(&t.owner, vsk::COMMUNITY_METADATA, &t.cid.0, 2, Some(&v1_hash), &metadata_json("OwnerWins"), None);
        t.fold.ingest([by_admin.clone()]);
        t.fold.ingest([by_owner.clone()]);
        assert_eq!(t.fold.metadata().unwrap().name, "OwnerWins");

        let mut t2 = ctx();
        // Rebuild the same community in the other ingest order. (Fresh keys
        // per ctx, so rebuild the delegation too.)
        genesis(&mut t2);
        let admin2 = Keys::generate();
        let vac2 = delegate_admin(&mut t2, &admin2, 1, perm::MANAGE_METADATA);
        let v1h2 = t2.fold.head(&t2.cid.0).unwrap().hash();
        let a = edition(&t2.owner, vsk::COMMUNITY_METADATA, &t2.cid.0, 2, Some(&v1h2), &metadata_json("OwnerWins"), None);
        let b = edition(&admin2, vsk::COMMUNITY_METADATA, &t2.cid.0, 2, Some(&v1h2), &metadata_json("AdminWins"), Some(&vac2));
        t2.fold.ingest([a]);
        t2.fold.ingest([b]);
        assert_eq!(t2.fold.metadata().unwrap().name, "OwnerWins");
    }

    #[test]
    fn equal_authority_tie_breaks_by_lower_rumor_id() {
        let mut t = ctx();
        genesis(&mut t);
        let v1_hash = t.fold.head(&t.cid.0).unwrap().hash();
        let a = edition(&t.owner, vsk::COMMUNITY_METADATA, &t.cid.0, 2, Some(&v1_hash), &metadata_json("A"), None);
        let b = edition(&t.owner, vsk::COMMUNITY_METADATA, &t.cid.0, 2, Some(&v1_hash), &metadata_json("B"), None);
        let expect = if a.rumor_id.as_bytes() < b.rumor_id.as_bytes() { "A" } else { "B" };
        // Both orders converge on the lower id.
        t.fold.ingest([a.clone(), b.clone()]);
        assert_eq!(t.fold.metadata().unwrap().name, expect);
        // Same community, reverse arrival order.
        let mut fold = ControlFold::new(t.cid, t.owner.public_key(), FoldMode::Tracking);
        let meta1 = edition(&t.owner, vsk::COMMUNITY_METADATA, &t.cid.0, 1, None, &metadata_json("Vector"), None);
        fold.ingest([meta1]);
        fold.ingest([b, a]);
        assert_eq!(fold.metadata().unwrap().name, expect);
    }

    #[test]
    fn no_self_promotion_and_no_position_zero() {
        let mut t = ctx();
        genesis(&mut t);
        let admin = Keys::generate();
        let vac = delegate_admin(&mut t, &admin, 2, perm::MANAGE_ROLES);
        // A role at the actor's own position (or above) is refused.
        let peer_role = edition(&admin, vsk::ROLE, &[0x05; 32], 1, None, &role_json(0x05, 2, perm::BAN), Some(&vac));
        let above_role = edition(&admin, vsk::ROLE, &[0x06; 32], 1, None, &role_json(0x06, 1, perm::BAN), Some(&vac));
        t.fold.ingest([peer_role, above_role]);
        assert!(t.fold.roster().roles.get(&RoleId([0x05; 32])).is_none());
        assert!(t.fold.roster().roles.get(&RoleId([0x06; 32])).is_none());
        // Below: honored.
        let below = edition(&admin, vsk::ROLE, &[0x07; 32], 1, None, &role_json(0x07, 3, perm::BAN), Some(&vac));
        t.fold.ingest([below]);
        assert!(t.fold.roster().roles.get(&RoleId([0x07; 32])).is_some());
        // Position 0 is unmintable, the owner included.
        let zero = edition(&t.owner, vsk::ROLE, &[0x08; 32], 1, None, &role_json(0x08, 0, 0), None);
        t.fold.ingest([zero]);
        assert!(t.fold.roster().roles.get(&RoleId([0x08; 32])).is_none());
    }

    #[test]
    fn banlist_gating_and_banned_author_drop() {
        let mut t = ctx();
        genesis(&mut t);
        let admin = Keys::generate();
        let troll = Keys::generate();
        let vac = delegate_admin(&mut t, &admin, 1, perm::BAN | perm::MANAGE_METADATA);

        let ban_eid = banlist_locator(&t.cid);
        let ban = edition(
            &admin,
            vsk::BANLIST,
            &ban_eid,
            1,
            None,
            &format!("[\"{}\"]", troll.public_key().to_hex()),
            Some(&vac),
        );
        t.fold.ingest([ban]);
        assert!(t.fold.is_banned(&troll.public_key()));

        // A banned npub's editions are dropped entirely — even well-formed
        // ones with a stolen-looking citation.
        let v1_hash = t.fold.head(&t.cid.0).unwrap().hash();
        let from_troll = edition(&troll, vsk::COMMUNITY_METADATA, &t.cid.0, 2, Some(&v1_hash), &metadata_json("Troll"), Some(&vac));
        t.fold.ingest([from_troll]);
        assert_eq!(t.fold.metadata().unwrap().name, "Vector");
    }

    #[test]
    fn banning_the_owner_or_a_peer_is_refused() {
        let mut t = ctx();
        genesis(&mut t);
        let admin = Keys::generate();
        let peer = Keys::generate();
        let vac_a = delegate_admin(&mut t, &admin, 1, perm::BAN);
        // Give peer the same role (same rank).
        let peer_grant_eid = grant_locator(&t.cid, &peer.public_key().to_bytes());
        let peer_grant = edition(&t.owner, vsk::GRANT, &peer_grant_eid, 1, None, &grant_json(&peer, &[0x01]), None);
        t.fold.ingest([peer_grant]);

        let ban_eid = banlist_locator(&t.cid);
        // Owner ban: refused outright.
        let ban_owner = edition(&admin, vsk::BANLIST, &ban_eid, 1, None, &format!("[\"{}\"]", t.owner.public_key().to_hex()), Some(&vac_a));
        // Peer ban: equal cannot act on equal.
        let ban_peer = edition(&admin, vsk::BANLIST, &ban_eid, 1, None, &format!("[\"{}\"]", peer.public_key().to_hex()), Some(&vac_a));
        t.fold.ingest([ban_owner, ban_peer]);
        assert!(t.fold.banlist().is_empty());
    }

    #[test]
    fn channel_deletion_is_terminal() {
        let mut t = ctx();
        let chan = genesis(&mut t);
        let v1_hash = t.fold.head(&chan.0).unwrap().hash();
        let delete = edition(&t.owner, vsk::CHANNEL_METADATA, &chan.0, 2, Some(&v1_hash), "{\"name\":\"general\",\"private\":false,\"deleted\":true}", None);
        let d_hash = delete.hash();
        t.fold.ingest([delete]);
        assert!(t.fold.channel(&chan).unwrap().deleted);
        // Even the owner cannot resurrect a deleted Channel.
        let resurrect = edition(&t.owner, vsk::CHANNEL_METADATA, &chan.0, 3, Some(&d_hash), "{\"name\":\"general\",\"private\":false}", None);
        t.fold.ingest([resurrect]);
        assert!(t.fold.channel(&chan).unwrap().deleted);
    }

    #[test]
    fn registry_binds_to_its_creator_and_gates_public() {
        let mut t = ctx();
        genesis(&mut t);
        let creator = Keys::generate();
        let vac = delegate_admin(&mut t, &creator, 1, perm::CREATE_INVITE);
        let link_signer = Keys::generate().public_key();

        // Registry at someone ELSE's coordinate: refused.
        let other_eid = invite_links_locator(&t.cid, &Keys::generate().public_key().to_bytes());
        let forged = edition(&creator, vsk::INVITE_REGISTRY, &other_eid, 1, None, &format!("[\"{}\"]", link_signer.to_hex()), Some(&vac));
        t.fold.ingest([forged]);
        assert!(!t.fold.is_public());

        // Own coordinate: honored; the Community flips Public.
        let own_eid = invite_links_locator(&t.cid, &creator.public_key().to_bytes());
        let registry = edition(&creator, vsk::INVITE_REGISTRY, &own_eid, 1, None, &format!("[\"{}\"]", link_signer.to_hex()), Some(&vac));
        let r_hash = registry.hash();
        t.fold.ingest([registry]);
        assert!(t.fold.is_public());
        assert_eq!(t.fold.registry_active_set(), vec![link_signer]);

        // Retiring the last live link empties the set: Private again.
        let empty = edition(&creator, vsk::INVITE_REGISTRY, &own_eid, 2, Some(&r_hash), "[]", Some(&vac));
        t.fold.ingest([empty]);
        assert!(!t.fold.is_public());
    }

    #[test]
    fn fresh_join_accepts_a_compaction_baseline_tracking_fails_closed() {
        let mut t = ctx();
        genesis(&mut t);
        // Simulate a post-Refounding head: metadata v5 whose prev cites an
        // edition that no longer exists.
        let head = edition(&t.owner, vsk::COMMUNITY_METADATA, &t.cid.0, 5, Some(&[0xAA; 32]), &metadata_json("Refounded"), None);

        // Tracking client that held v1: the dangling prev is a gap — fail
        // closed for that entity, keep the old state.
        t.fold.ingest([head.clone()]);
        assert_eq!(t.fold.metadata().unwrap().name, "Vector");

        // Fresh joiner from nothing: signature + current-authority is the
        // whole test; the head is the baseline.
        let mut fresh = ControlFold::new(t.cid, t.owner.public_key(), FoldMode::FreshJoin);
        fresh.ingest([head]);
        assert_eq!(fresh.metadata().unwrap().name, "Refounded");
        assert_eq!(fresh.head(&t.cid.0).unwrap().version, 5);

        // And the baseline extends normally afterward.
        let h5 = fresh.head(&t.cid.0).unwrap().hash();
        let v6 = edition(&t.owner, vsk::COMMUNITY_METADATA, &t.cid.0, 6, Some(&h5), &metadata_json("Onward"), None);
        fresh.ingest([v6]);
        assert_eq!(fresh.metadata().unwrap().name, "Onward");
    }

    #[test]
    fn dissolution_is_terminal_and_owner_only() {
        let mut t = ctx();
        genesis(&mut t);
        let impostor = Keys::generate();
        let fake = parse_edition(&crate::concord::v2::edition::build_dissolved_rumor(impostor.public_key(), 1_725_000_000)).unwrap();
        assert!(!t.fold.ingest_dissolution(&fake), "an impostor's tombstone is noise");

        let real = parse_edition(&crate::concord::v2::edition::build_dissolved_rumor(t.owner.public_key(), 1_725_000_000)).unwrap();
        assert!(t.fold.ingest_dissolution(&real));
        assert!(t.fold.is_dissolved());

        // Nothing new is honored post-seal.
        let v1_hash = t.fold.head(&t.cid.0).unwrap().hash();
        let rename = edition(&t.owner, vsk::COMMUNITY_METADATA, &t.cid.0, 2, Some(&v1_hash), &metadata_json("PostMortem"), None);
        t.fold.ingest([rename]);
        assert_eq!(t.fold.metadata().unwrap().name, "Vector");
    }

    #[test]
    fn metadata_custom_fields_round_trip() {
        let json = "{\"name\":\"V\",\"relays\":[],\"custom\":{\"rules\":\"Be excellent.\",\"vector/theme\":\"dark\"},\"future_field\":42}";
        let meta: CommunityMetadata = serde_json::from_str(json).unwrap();
        assert_eq!(meta.custom.as_ref().unwrap()["rules"], "Be excellent.");
        // Unknown top-level fields survive a round trip.
        let back = serde_json::to_value(&meta).unwrap();
        assert_eq!(back["future_field"], 42);
        assert_eq!(back["custom"]["vector/theme"], "dark");
    }

    #[test]
    fn oversized_names_are_dropped_at_fold() {
        let mut t = ctx();
        let long_name = "x".repeat(NAME_MAX_BYTES + 1);
        let meta = edition(&t.owner, vsk::COMMUNITY_METADATA, &t.cid.0, 1, None, &metadata_json(&long_name), None);
        t.fold.ingest([meta]);
        assert!(t.fold.metadata().is_none());
    }
}
