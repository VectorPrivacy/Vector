//! v1 → v2 community migration — the atomic dissolution-carrier wire codec (task #10).
//!
//! The migration rides INSIDE the vsk=10 GroupDissolved tombstone's content: a signpost
//! (where the v2 twin lives) plus `m`, the complete v2 JoinMaterial sealed under the v1
//! server root at publish time. One owner-signed event seals v1, signposts v2, and IS
//! every member's invite. Shipped v0.4.0 never parses tombstone content (validity is
//! vsk + coordinate + signer), so old clients fold this as a plain dissolution — composer
//! lockdown, history intact — and the relays retain the event for any later re-probe.
//!
//! Scope discipline: the signpost is readable by anyone who can open the tombstone's
//! id-derived envelope (it grants nothing); `m` opens only under a held server-root epoch
//! key — exactly v1's confidentiality boundary, so a read-cut member cannot open it. Keys
//! are NEVER placed under the id-derived envelope: the community id rides in every invite
//! bundle ever shared and is not a secret.

use super::cipher;
use super::roster::DissolvedEdition;
use super::transport::Transport;
use super::{Community, CommunityId};
use crate::state::SessionGuard;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::{LazyLock, Mutex as StdMutex};

/// v1 cids with a `drive_migration` in flight — mutual exclusion so the boot maintenance
/// and the live carrier-fold never drive the SAME community concurrently. Without it two
/// drives can interleave a channel-less `save_community_v2` (which prunes) after the other's
/// re-parent commit, deleting a just-adopted row. Cleared on account swap.
static DRIVE_INFLIGHT: LazyLock<StdMutex<HashSet<String>>> =
    LazyLock::new(|| StdMutex::new(HashSet::new()));

/// Clear the drive-in-flight set (account swap — the new account's drives must not be
/// blocked by a stale claim from the old one).
pub fn clear_drive_inflight() {
    DRIVE_INFLIGHT.lock().unwrap_or_else(|e| e.into_inner()).clear();
}

/// RAII claim on [`DRIVE_INFLIGHT`]. `take` returns `None` when the cid is already claimed
/// (drive vs drive, wizard vs drive, wizard vs wizard). Drop is generation-aware: after an
/// account swap clears the set, a stale claim's unwind must not release the claim the NEW
/// account just inserted for the same cid.
struct DriveClaim(String, SessionGuard);
impl DriveClaim {
    fn take(cid: &str) -> Option<Self> {
        let mut inflight = DRIVE_INFLIGHT.lock().unwrap_or_else(|e| e.into_inner());
        if !inflight.insert(cid.to_string()) {
            return None;
        }
        Some(DriveClaim(cid.to_string(), SessionGuard::capture()))
    }
}
impl Drop for DriveClaim {
    fn drop(&mut self) {
        if self.1.is_valid() {
            DRIVE_INFLIGHT.lock().unwrap_or_else(|e| e.into_inner()).remove(&self.0);
        }
    }
}

/// Test hooks: stand in for a concurrent drive holding the claim (the claim is private and
/// RAII-scoped, so a test can't otherwise model "another drive is mid-flight").
#[cfg(test)]
pub fn test_hold_drive_claim(cid: &str) {
    DRIVE_INFLIGHT.lock().unwrap_or_else(|e| e.into_inner()).insert(cid.to_string());
}
#[cfg(test)]
pub fn test_release_drive_claim(cid: &str) {
    DRIVE_INFLIGHT.lock().unwrap_or_else(|e| e.into_inner()).remove(cid);
}

/// When the owner-side migration wizard unlocks: 2026-08-04 00:00:00 UTC. Gates ONLY the
/// wizard (UI row + command entry); the member-side machinery is live from release day, so
/// a migration performed by a lock-bypassing build still carries every member along. A
/// coordination gate, not a security gate.
pub const MIGRATION_UNLOCK_AT: u64 = 1_785_801_600;

/// Bound on the whole tombstone content string before any parse. The outer NIP-44 seal
/// caps its plaintext at 65535 bytes, so anything larger is garbage by construction.
pub const MAX_PAYLOAD_CONTENT: usize = 100_000;
/// Bound on the base64 `m` string inside the payload (checked before decode/open).
pub const MAX_M_B64: usize = 90_000;
/// Display-name cap in the signpost — truncated, not rejected (fail-safe parse).
pub const MAX_SIGNPOST_NAME: usize = 120;
/// Conservative relay max-event-size floor (strfry default is 64 KB): the final sealed
/// OUTER event's JSON must stay under this or common relays will reject the publish.
pub const MAX_WIRE_EVENT: usize = 60_000;

/// The plaintext signpost: where the v2 twin lives. Grants nothing by itself — every field
/// is verified against the member's held v1 owner anchor (triple-bind) before any use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrationSignpost {
    /// The v2 self-certifying community id. Must recompute from `owner_xonly` + `owner_salt`.
    pub v2_community_id: String,
    /// Must equal the member's held v1 owner anchor (owner continuity — the consent basis).
    pub owner_xonly: String,
    /// The v2 owner salt (public input to the self-cert id).
    pub owner_salt: String,
    /// The v2 relay set (capped like every other attacker-influencable relay list).
    /// Absent is empty, not a parse failure — a rejected signpost strands the migration.
    #[serde(default)]
    pub relays: Vec<String>,
    /// Display name at migration time (informational only).
    pub name: String,
    /// Which stitched channel the one-row UI surfaces.
    pub primary_channel: String,
    /// The v1 base epoch at publish — bounds a stale member's catch-up walk before they
    /// conclude `m` is unopenable (a member holding an epoch >= this that still cannot
    /// open `m` is genuinely outside the member set).
    #[serde(default)]
    pub root_epoch: u64,
}

/// The parsed migration payload: signpost + optionally the sealed key material.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationPayload {
    pub signpost: MigrationSignpost,
    /// base64 NIP-44 seal of the v2 JoinMaterial under the v1 server_root at publish.
    /// `None` = signpost-only (still a valid pointer; the member falls back to the
    /// straggler CTA if no key material ever opens).
    pub m: Option<String>,
}

/// Wire shape of the tombstone content. `migrated_to` keys the signpost so a plain `{}`
/// (the pre-migration dissolution) deserializes to "no payload" rather than erroring.
#[derive(Serialize, Deserialize)]
struct WirePayload {
    migrated_to: MigrationSignpost,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    m: Option<String>,
}

fn is_hex64(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Parse a tombstone's content into a migration payload. Fail-SAFE by contract: any bound
/// violation, shape error, or bad field returns `None` — the event remains a plain
/// dissolution and the SEAL is never rejected. Ids are lowercase-normalized; relays are
/// capped by truncation (hostile payloads degrade, never amplify); the name is truncated.
pub fn parse_migration_payload(content: &str) -> Option<MigrationPayload> {
    if content.len() > MAX_PAYLOAD_CONTENT {
        return None;
    }
    let wire: WirePayload = serde_json::from_str(content).ok()?;
    let mut sp = wire.migrated_to;
    if !is_hex64(&sp.v2_community_id)
        || !is_hex64(&sp.owner_xonly)
        || !is_hex64(&sp.owner_salt)
        || !is_hex64(&sp.primary_channel)
    {
        return None;
    }
    sp.v2_community_id = sp.v2_community_id.to_lowercase();
    sp.owner_xonly = sp.owner_xonly.to_lowercase();
    sp.owner_salt = sp.owner_salt.to_lowercase();
    sp.primary_channel = sp.primary_channel.to_lowercase();
    sp.relays = super::cap_relays(sp.relays);
    if sp.name.chars().count() > MAX_SIGNPOST_NAME {
        sp.name = sp.name.chars().take(MAX_SIGNPOST_NAME).collect();
    }
    let m = match wire.m {
        Some(m) if m.len() > MAX_M_B64 => return None,
        other => other,
    };
    Some(MigrationPayload { signpost: sp, m })
}

/// Serialize a payload for the tombstone content (the wizard's side of [`parse_migration_payload`]).
pub fn build_migration_content(signpost: &MigrationSignpost, m: Option<String>) -> Result<String, String> {
    serde_json::to_string(&WirePayload { migrated_to: signpost.clone(), m })
        .map_err(|e| format!("serialize migration payload: {e}"))
}

/// Seal the v2 JoinMaterial under the v1 server root at publish time. Errors past NIP-44's
/// 65535-byte plaintext cap — the wizard surfaces that as a clean abort, never a truncation.
pub fn seal_m(server_root: &[u8; 32], join_material_json: &[u8]) -> Result<String, String> {
    cipher::seal(server_root, join_material_json)
}

/// Try to open `m` under EVERY held server-root epoch key, newest first — absorbs both a
/// stale local head and a concurrent v1 refound that advanced past the publish root.
/// `None` = no held root opens it (caller decides: catch-up walk, then the straggler CTA).
pub fn open_m(held_roots: &[(u64, [u8; 32])], m_b64: &str) -> Option<Vec<u8>> {
    if m_b64.len() > MAX_M_B64 {
        return None;
    }
    let mut roots: Vec<&(u64, [u8; 32])> = held_roots.iter().collect();
    roots.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, key) in roots {
        if let Ok(plain) = cipher::open(key, m_b64) {
            return Some(plain);
        }
    }
    None
}

/// Total, payload-aware pointer selection over the owner's tombstones: the pointer comes
/// from the newest PAYLOAD-CARRYING owner-signed tombstone (tiebreak lowest inner id); a
/// payload-less `{}` tombstone SEALS the community but never shadows a payload-bearing one
/// — otherwise a plain dissolution from a second device would silently shed the keys for
/// every future straggler. (Corollary, deliberate: a NEWER payload-bearing tombstone
/// WITHOUT `m` is the owner's honest-client-scoped retraction lever for the on-ramp.)
pub fn select_pointer(editions: &[DissolvedEdition], owner_hex: &str) -> Option<(MigrationPayload, String)> {
    let mut best: Option<(&DissolvedEdition, MigrationPayload)> = None;
    for e in editions {
        if e.author.to_hex() != owner_hex {
            continue;
        }
        let Some(payload) = parse_migration_payload(&e.content) else { continue };
        best = match best {
            Some((cur, cur_p))
                if (cur.created_at, std::cmp::Reverse(cur.inner_id))
                    >= (e.created_at, std::cmp::Reverse(e.inner_id)) =>
            {
                Some((cur, cur_p))
            }
            _ => Some((e, payload)),
        };
    }
    // The raw winning content rides along so the caller can persist the exact wire form
    // (re-parseable later without a second serializer for the payload).
    best.map(|(e, p)| (p, e.content.clone()))
}

/// Wire-size gate for the final sealed OUTER event — computed on the actual bytes, never
/// estimated. Run by the wizard before publishing; failing is a clean abort.
pub fn check_outer_size(outer: &nostr_sdk::prelude::Event) -> Result<(), String> {
    let len = outer.as_json().len();
    if len > MAX_WIRE_EVENT {
        return Err(format!(
            "migration event is {len} bytes, over the {MAX_WIRE_EVENT}-byte relay ceiling; \
             this community is too large for a single migration event"
        ));
    }
    Ok(())
}

// ── Member flow: fold a migration pointer → open `m` → flip to v2 ─────────────────────────

/// The dissolved-gate exemption: a base rekey may advance a SEALED community's epoch
/// only while a migration pointer is held, the flip hasn't happened, and the target epoch
/// does not exceed the pointer's publish epoch. Lets a stale member walk their held root
/// forward to the one `m` was sealed under, without ever re-opening the seal for anything
/// else. Any read error fails closed (no exemption).
pub fn catchup_exempt(community_id: &str, target_epoch: u64) -> bool {
    if crate::db::community::get_migrated_to(community_id).ok().flatten().is_some() {
        return false; // already flipped — the fence stands
    }
    let Ok(Some(raw)) = crate::db::community::get_migration_pointer(community_id) else {
        return false; // no pointer → a plain dissolution, never advances
    };
    let Some(payload) = parse_migration_payload(&raw) else { return false };
    target_epoch <= payload.signpost.root_epoch
}

/// Held server-root epoch keys for a community, for the multi-root `m` open.
fn held_roots(community_id: &str) -> Vec<(u64, [u8; 32])> {
    crate::db::community::held_epoch_keys(community_id, crate::community::SERVER_ROOT_SCOPE_HEX)
        .unwrap_or_default()
        .into_iter()
        .map(|(e, k)| (e.0, k))
        .collect()
}

/// Drive a held v1 community's migration to completion when a pointer is present and the
/// flip hasn't happened: open `m` (catching a stale root up first, no-erase), join the v2
/// twin through the ban-gated accept path, then run the flip transaction. Idempotent and
/// resumable — safe to call from the boot sweep, the live fold, and the fallback door.
/// Returns `Ok(Some(v2_id))` on a completed flip, `Ok(None)` when nothing was actionable
/// (no pointer, already flipped, or `m` unopenable — the straggler CTA case).
pub async fn drive_migration<T: Transport + ?Sized>(
    transport: &T,
    community: &Community,
) -> Result<Option<String>, String> {
    let session = SessionGuard::capture();
    let cid = community.id.to_hex();

    // Claim the in-flight slot or bail — a concurrent drive (boot maintenance vs the live
    // carrier fold, or the owner's wizard vs their own fold) is already handling this cid.
    // Without this, two channel-less v2 saves can interleave a prune after the other's
    // re-parent. The claim is released on ANY exit (RAII).
    let Some(_claim) = DriveClaim::take(&cid) else {
        return Ok(None);
    };

    // Already flipped? Nothing to do (idempotent double-trigger).
    if crate::db::community::get_migrated_to(&cid).ok().flatten().is_some() {
        return Ok(None);
    }
    let Some(raw) = crate::db::community::get_migration_pointer(&cid)? else {
        return Ok(None); // no pointer → not a migration
    };
    let Some(payload) = parse_migration_payload(&raw) else {
        // A stored-but-unparseable pointer is inert; mark checked so the sweep converges.
        let _ = crate::db::community::set_migration_checked(&cid);
        return Ok(None);
    };

    // Triple-bind check 2 — OWNER CONTINUITY: the v2 owner the signpost claims MUST equal this
    // v1 community's proven owner (the pointer is already owner-signed at the bound coordinate,
    // check 1). A migration that changes the owner identity is NOT eligible for consent-free
    // join — identity continuity is the whole basis for skipping consent. Fail-closed: no
    // proven owner ⇒ no continuity ⇒ no auto-join. (check 3 — the v2 self-cert recompute — runs
    // inside accept_bundle.)
    let Some(owner) = super::service::proven_owner_hex(community) else { return Ok(None) };
    if payload.signpost.owner_xonly != owner {
        return Ok(None); // owner discontinuity → not a consent-free migration
    }
    // Bind the v2 self-cert id to the signpost's own owner/salt before the network join.
    if !super::v2::derive::verify_community_id(
        &CommunityId(crate::simd::hex::hex_to_bytes_32(&payload.signpost.v2_community_id)),
        &crate::simd::hex::hex_to_bytes_32(&payload.signpost.owner_xonly),
        &crate::simd::hex::hex_to_bytes_32(&payload.signpost.owner_salt),
    ) {
        return Ok(None); // the v2 id is not a commitment to this owner+salt
    }

    let Some(m_b64) = payload.m.as_deref() else {
        return Ok(None); // signpost-only pointer → straggler CTA, no keys to open
    };

    // Open `m` under any held root; if none opens and we're stale vs the publish epoch,
    // walk the base root forward (no-erase: the `removed` signal is ignored — the community
    // is terminal either way) and retry. The exemption above lets this walk run despite the
    // seal.
    let mut plain = open_m(&held_roots(&cid), m_b64);
    if plain.is_none() && community.server_root_epoch.0 < payload.signpost.root_epoch {
        let _ = super::service::catch_up_server_root(transport, community).await;
        if !session.is_valid() {
            return Err("account changed during migration catch-up".to_string());
        }
        plain = open_m(&held_roots(&cid), m_b64);
    }
    let Some(plain) = plain else {
        return Ok(None); // unopenable (read-cut or lost-DB) → straggler CTA
    };

    // Verify the JoinMaterial's owner matches the pointer's owner continuity claim before
    // trusting it (the self-cert id recompute is the fail-closed gate inside accept_bundle).
    let jm: super::v2::list::JoinMaterial =
        serde_json::from_slice(&plain).map_err(|e| format!("migration join material parse: {e}"))?;
    if jm.community_id != payload.signpost.v2_community_id || jm.owner != payload.signpost.owner_xonly {
        return Err("migration payload keys disagree with the signpost".to_string());
    }

    // held-v2 dedup: if this account ALREADY holds the v2 twin, do NOT network-join
    // it again — flip only. Covers the idempotent double-trigger, a multi-device peer that
    // synced the twin via the 13302 list first, and the OWNER's own client (the wizard
    // created the twin, so the owner holds it — accept_bundle must never try to "join" it).
    let v2_id = CommunityId(crate::simd::hex::hex_to_bytes_32(&payload.signpost.v2_community_id));
    let v2_hex = payload.signpost.v2_community_id.clone();
    if crate::db::community::load_community_v2(&v2_id)?.is_none() {
        // Join the v2 twin (ban-gated, owner-root-verified). A refusal (banned / forged
        // root) leaves the community sealed — never a half-flip.
        let v2 = super::v2::service::accept_migration_material(transport, &jm).await?;
        if crate::simd::hex::bytes_to_hex_32(&v2.identity.community_id.0) != v2_hex {
            return Err("joined community id disagrees with the migration pointer".to_string());
        }
        if !session.is_valid() {
            return Err("account changed during migration join".to_string());
        }
    }

    // The flip: re-parent the stitched channel rows + stamp the fence (one txn). The v2
    // community ROW already exists (accept saved it, or this account already held it); NO
    // save_community_v2 here — the v2 view is channel-less (its channel ids were v1-owned →
    // skipped by the hijack guard), so a re-save would PRUNE the just-re-parented rows.
    // Public channels fold from the control plane; the re-parented rows carry the history.
    // Under the twin's follow lock: the follow worker's whole-row save deletes channel rows
    // absent from a pre-flip-loaded (channel-less) struct, so the flip must not straddle it.
    // Guard IMMEDIATELY before the most destructive write in the feature — the held-v2
    // path above skips the join branch (and its check), so this is the one that counts.
    let flock = super::v2::realtime::follow_lock(&v2_id);
    let _fguard = flock.lock().await;
    if !session.is_valid() {
        return Err("account changed during migration flip".to_string());
    }
    crate::db::community::reparent_channels_and_fence(&cid, &v2_hex)?;
    Ok(Some(v2_hex))
}

// ── Owner flow: the migration wizard ─────────────────────────────────────────────────────

/// Ledger phases (persisted in `community_migrations.phase`). Each is idempotent + resumable.
/// TWIN_MINTED lands IMMEDIATELY after the twin's genesis returns, BEFORE the sibling-channel
/// and banlist tail — so a crash anywhere in that multi-await tail resumes onto the SAME twin
/// instead of re-minting a fresh identity (which would orphan the first genesis on the relays).
/// Residual window: a crash INSIDE create_migration_twin (after its local save, before return
/// — spanning its genesis + guestbook publishes, so seconds over a slow transport) still
/// re-mints on the next run and leaves a phantom v2 row — bounded: no carrier ever references
/// it, so no member is ever stranded; it is a dead genesis plus one stale local row.
pub const PHASE_TWIN_MINTED: i64 = 1;
pub const PHASE_TWIN_BUILT: i64 = 2;
/// Birth refound 0→1 + Guestbook snapshot of the full v1 roster landed. AFTER this the twin
/// is at epoch 1, so `m` (sealed in the next phase) carries the epoch-1 root — the whole
/// reason for the refound (genesis has no snapshot authority).
pub const PHASE_TWIN_REFOUNDED: i64 = 3;
pub const PHASE_CARRIER_PUBLISHED: i64 = 4;
pub const PHASE_FLIPPED: i64 = 5;

/// The full v1 memberlist to seed into the twin's epoch-1 Guestbook. `community_member_activity`
/// IS v1's single source of truth for "who is a member": it already unions observed authors +
/// the owner + every roster grant-holder, subtracts the banlist, and applies the leave filter
/// — so the seed is exactly that set (fetched UNCAPPED, so a large community seeds every
/// member instead of the top 500-by-recency). Using it verbatim means the v2 seed can NEVER
/// diverge from what v1 itself shows as members.
///
/// note: a v1 admin who LEFT but was never stripped of their grant is re-asserted as a
/// member by that function (v1's rule: a leave must not lock a sitting admin out) — so they
/// ARE seeded. That is v1-faithful (v1 shows them as a member), a deliberate divergence from
/// the design's "− left" line for the admin case; a NON-admin who left is dropped by the leave
/// filter. `owner_hex` is unused now (the DB seeds the owner) but kept for call-site clarity.
fn v1_snapshot_members(v1_cid: &str, _owner_hex: &str) -> Vec<nostr_sdk::prelude::PublicKey> {
    crate::db::community::community_member_activity_capped(v1_cid, false)
        .unwrap_or_default()
        .iter()
        .filter_map(|(npub, _)| nostr_sdk::prelude::PublicKey::parse(npub).ok())
        .collect()
}

/// Whether the owner wizard is unlocked (the timelock is a coordination gate, re-checked at
/// the command entry, not just in the UI). `now_secs` is passed in (the core has no clock).
pub fn wizard_unlocked(now_secs: u64) -> bool {
    now_secs >= MIGRATION_UNLOCK_AT
}

/// The post-timelock door for FRESH v1 joins, probe-first. Pre-unlock, or a community we
/// already hold (re-accept / cross-device rehydrate), passes locally. Post-unlock a fresh
/// join passes ONLY when the rotation-stable dissolved coordinate carries the proven
/// owner's migration pointer — that join is the permanent on-ramp (save → carrier fold
/// seals → drive lands the joiner in the v2 twin). A live v1 community, an unprovable
/// owner, or a relay miss all refuse: fail-closed, a retry beats onboarding a fresh user
/// onto the legacy protocol. Every v1 join door (Tauri direct + public accepts, facade
/// direct + public accepts) must call this before persisting anything.
pub async fn gate_fresh_v1_join<T: Transport + ?Sized>(
    transport: &T,
    community: &Community,
    now_secs: u64,
) -> Result<(), String> {
    if now_secs < MIGRATION_UNLOCK_AT {
        return Ok(());
    }
    if matches!(crate::db::community::load_community(&community.id), Ok(Some(_))) {
        return Ok(());
    }
    if let Some(owner) = super::service::proven_owner_hex(community) {
        let records = super::service::dissolved_tombstone_records(transport, community).await;
        if select_pointer(&records, &owner).is_some() {
            return Ok(());
        }
    }
    Err("This community still uses the legacy protocol and can no longer be joined. Ask the owner to upgrade it to Concord v2 and share a fresh invite.".to_string())
}

/// The UI state ladder for a v1 community's migration row, in priority order. Pure so the
/// ordering is testable: `in_progress` MUST outrank `dissolved`, because the owner's own
/// carrier self-fold seals v1 while the flip is still pending — exactly the window the
/// Resume affordance exists for. A ledger row only ever exists on the wizard's own account.
pub fn migration_state(
    migrated: bool,
    ledger_phase: i64,
    dissolved: bool,
    is_owner: bool,
    unlocked: bool,
) -> &'static str {
    if migrated {
        "migrated"
    } else if ledger_phase > 0 && is_owner {
        "in_progress"
    } else if dissolved {
        "dissolved"
    } else if !is_owner {
        "not_owner"
    } else if unlocked {
        "ready"
    } else {
        "locked"
    }
}

/// Whether the wizard may run for this community (the row's action is armed). A sealed
/// community is eligible ONLY as a resume of its own in-flight migration.
pub fn migration_eligible(migrated: bool, ledger_phase: i64, dissolved: bool, is_owner: bool) -> bool {
    is_owner && !migrated && (!dissolved || ledger_phase > 0)
}

/// Owner-side migration wizard: build the v2 twin (reusing v1 channel ids + cloning the
/// banlist), seal the twin's JoinMaterial into `m`, publish the carrier dissolution on v1,
/// then flip the owner's own client to v2. Resumable via the migration-77 ledger — a re-run
/// after a crash picks up at the recorded phase. Every phase re-checks the `SessionGuard`.
/// `now_secs` gates the timelock. Returns the v2 community id on completion.
/// Emit a wizard progress step to the UI (no-op on headless clients via the unregistered
/// emitter). `pct` is OVERALL progress 0-100 across the whole wizard; `label` is layman-facing.
/// The frontend renders a determinate ring + this label in an unclosable modal (rekey contract).
fn emit_migration_progress(label: &str, pct: u8) {
    crate::emit_event("community_migration_progress", &serde_json::json!({ "label": label, "pct": pct }));
}

pub async fn migrate_community_to_v2<T: Transport + ?Sized>(
    transport: &T,
    v1: &Community,
    now_secs: u64,
) -> Result<String, String> {
    let session = SessionGuard::capture();
    let v1_cid = v1.id.to_hex();
    // One wizard/drive at a time per cid: a double-fired command would race the twin mint
    // pre-ledger (the double-mint orphan window) and its flip against a concurrent drive's.
    let Some(_claim) = DriveClaim::take(&v1_cid) else {
        return Err("this community's upgrade is already in progress".to_string());
    };
    emit_migration_progress("Preparing the upgrade...", 5);

    // Phase 0 — preflight (owner, unlocked, not already dissolved/migrated).
    if !wizard_unlocked(now_secs) {
        return Err("community migration is not unlocked yet".to_string());
    }
    if !super::service::is_proven_owner(v1) {
        return Err("only the community owner can migrate the community".to_string());
    }
    // Already migrated? Check FIRST — a migrated community is ALSO dissolved (the flip seals
    // it), so this must precede the dissolved gate below or a legitimate crash-heal reads as
    // a plain dissolution.
    if let Some(v2) = crate::db::community::get_migrated_to(&v1_cid).ok().flatten() {
        // Crash-heal: the flip LANDED but the FLIPPED ledger write didn't (a crash between
        // the two). This re-run is a COMPLETED migration — heal the ledger and report
        // success instead of erroring on our own success.
        if let Some((ledger_v2, phase, _)) = crate::db::community::get_migration_ledger(&v1_cid).ok().flatten() {
            if ledger_v2 == v2 && phase < PHASE_FLIPPED {
                let _ = crate::db::community::set_migration_ledger(&v1_cid, &v2, PHASE_FLIPPED, "");
                return Ok(v2);
            }
        }
        return Err("this community has already been migrated".to_string());
    }
    // Resume ledger read up-front — a ledger row means THIS owner already began THIS
    // migration, which the dissolved gate below keys off.
    let ledger = crate::db::community::get_migration_ledger(&v1_cid).ok().flatten();
    let resume_phase = ledger.as_ref().map(|(_, p, _)| *p).unwrap_or(0);

    // Not already dissolved: a carrier published for a sealed-but-UNMIGRATED
    // community is undeliverable — members' folds short-circuit on the seal and never see
    // it, so the owner would flip alone. A plain dissolution and a migration are mutually
    // exclusive endings. EXEMPT a resume (`resume_phase > 0`) — a `dissolved=1` flag on
    // a community whose wizard already started is this owner's OWN carrier seal (e.g. a
    // self-fold sealed it after CARRIER_PUBLISHED, then the flip write flaked), not a foreign
    // plain dissolution; without the exemption that transient failure reads as false-terminal.
    if resume_phase == 0 && crate::db::community::get_community_dissolved(&v1_cid).unwrap_or(false) {
        return Err("this community has been dissolved; it cannot be migrated".to_string());
    }
    let Some(owner_hex) = super::service::proven_owner_hex(v1) else {
        return Err("cannot resolve the community owner".to_string());
    };

    // Phase 1a — mint (or reload) the v2 twin: primary channel reuses the v1 primary id.
    emit_migration_progress("Creating the new community...", 15);
    let twin = if resume_phase >= PHASE_TWIN_MINTED {
        let (v2_hex, _, _) = ledger.as_ref().unwrap();
        crate::db::community::load_community_v2(&CommunityId(crate::simd::hex::hex_to_bytes_32(v2_hex)))?
            .ok_or("migration twin missing on resume")?
    } else {
        let primary = v1.channels.first().ok_or("v1 community has no channels")?;
        let twin = super::v2::service::create_migration_twin(
            transport,
            &v1.name,
            v1.relays.clone(),
            v1.description.clone(),
            (primary.id, primary.name.clone()),
        )
        .await?;
        // Ledger the minted identity IMMEDIATELY — before the sibling/banlist tail — so no
        // crash in that tail can re-mint a second twin (the double-mint orphan window).
        let v2_hex = crate::simd::hex::bytes_to_hex_32(&twin.identity.community_id.0);
        if !session.is_valid() {
            return Err("account changed during twin mint".to_string());
        }
        crate::db::community::set_migration_ledger(&v1_cid, &v2_hex, PHASE_TWIN_MINTED, "")?;
        twin
    };
    let v2_hex = crate::simd::hex::bytes_to_hex_32(&twin.identity.community_id.0);

    // Phase 1b — sibling channels + banlist clone. Idempotent, so a resume at TWIN_MINTED
    // re-runs the whole tail: a re-created channel publishes a vsk-2 chain ADVANCE with the
    // same content (readers converge either way) and the local save skips v1-owned rows.
    if resume_phase < PHASE_TWIN_BUILT {
        emit_migration_progress("Copying channels, roles and bans...", 35);
        // Additional v1 channels reuse their ids on the twin (public — v1 has no private
        // channel model in the shipped protocol, so all stitch as public).
        for ch in v1.channels.iter().skip(1) {
            super::v2::service::create_public_channel_with_id(transport, &twin, &ch.name, ch.id).await?;
        }
        // Clone the v1 banlist so the v2 join-time ban gate catches v1-banned members.
        let banlist = crate::db::community::get_community_banlist(&v1_cid).unwrap_or_default();
        super::v2::service::clone_banlist_to_twin(transport, &twin, &banlist).await?;
        if !session.is_valid() {
            return Err("account changed during banlist clone".to_string());
        }
        // Clone governance: every v1 full admin is re-granted @admin on the twin (owner is
        // supreme by identity; banned members skipped). Banlist BEFORE governance so a
        // banned admin never regains authority on v2.
        let v1_roles = crate::db::community::get_community_roles(&v1_cid).unwrap_or_default();
        super::v2::service::clone_governance_to_twin(transport, &twin, &v1_roles, &banlist).await?;
        // The twin persists via save_community_v2 (community row + control plane); its public
        // channel rows stay v1-owned until the flip re-parents them (the hijack guard).
        // The ledger needs only the v2 id — a resume reloads the rest from DB + control plane.
        if !session.is_valid() {
            return Err("account changed during twin build".to_string());
        }
        crate::db::community::set_migration_ledger(&v1_cid, &v2_hex, PHASE_TWIN_BUILT, "")?;
    }

    // Phase 1c — BIRTH REFOUND 0→1 + seed the full v1 roster. Genesis has no snapshot
    // authority, so the twin must roll to epoch 1 (owner-only rekey) carrying an owner-signed
    // Guestbook snapshot of the v1 memberlist. After this the twin is at epoch 1 and `m` (next
    // phase) carries the epoch-1 root. Members join at epoch 1; not-yet-landed seeds show as
    // members (holding no keys) and RECEIVE every future rotation's blob, so a late migrator
    // never misses an epoch. Idempotent: mint_or_reuse re-delivers the same epoch-1 root, the
    // compaction re-wraps the same heads, the snapshot re-publishes (fresh snap_id each run;
    // convergence is coalesce commutativity, safe because it precedes the carrier).
    if resume_phase < PHASE_TWIN_REFOUNDED {
        emit_migration_progress("Securing member access...", 55);
        let members = v1_snapshot_members(&v1_cid, &owner_hex);
        super::v2::service::refound_at_birth(transport, &twin, &members).await?;
        if !session.is_valid() {
            return Err("account changed during birth refound".to_string());
        }
        crate::db::community::set_migration_ledger(&v1_cid, &v2_hex, PHASE_TWIN_REFOUNDED, "")?;
    }
    // Reload the twin at its CURRENT epoch (1 after the refound) so `m` seals the epoch-1
    // root, not the throwaway genesis root. On a resume ≥ TWIN_REFOUNDED the persisted twin
    // is already at epoch 1; this reload makes both paths converge.
    let twin = crate::db::community::load_community_v2(&twin.identity.community_id)?
        .ok_or("migration twin missing after birth refound")?;

    // Phase 2 — seal `m` + publish the carrier dissolution on v1 (idempotent: a re-run
    // re-seals fresh key material under the same root and re-publishes; members dedup).
    if resume_phase < PHASE_CARRIER_PUBLISHED {
        emit_migration_progress("Publishing the upgrade for all members...", 75);
        let jm = super::v2::service::twin_join_material(&twin);
        let m = seal_m(
            v1.server_root_key.as_bytes(),
            &serde_json::to_vec(&jm).map_err(|e| e.to_string())?,
        )?;
        let signpost = MigrationSignpost {
            v2_community_id: v2_hex.clone(),
            owner_xonly: owner_hex.clone(),
            owner_salt: crate::simd::hex::bytes_to_hex_32(&twin.identity.owner_salt),
            relays: twin.relays.clone(),
            name: v1.name.clone(),
            primary_channel: v1.channels.first().map(|c| c.id.to_hex()).unwrap_or_default(),
            root_epoch: v1.server_root_epoch.0,
        };
        let content = build_migration_content(&signpost, Some(m))?;
        super::service::publish_migration_carrier(transport, v1, &content).await?;
        if !session.is_valid() {
            return Err("account changed during carrier publish".to_string());
        }
        crate::db::community::set_migration_ledger(&v1_cid, &v2_hex, PHASE_CARRIER_PUBLISHED, "")?;
    }

    // Phase 3 — the owner's own local flip: re-parent channel rows + stamp the fence. The
    // v2 community ROW already exists (create_migration_twin saved it); NO save_community_v2
    // here — with a channel-less reloaded twin it would PRUNE the just-re-parented rows.
    // Under the twin's follow lock: a concurrent follow-worker save from a pre-flip
    // (channel-less) load would prune the rows this txn re-parents.
    // Straddle the whole wizard's network I/O with the entry guard before the DB write, so
    // a mid-wizard account swap can't land the flip + ledger row in the wrong account's DB.
    // `twin` is intentionally not re-saved here (see the fence contract below).
    emit_migration_progress("Switching you over...", 92);
    {
        // Scoped to the flip transaction alone: the follow lock must never be held across
        // network I/O (the list republish below awaits).
        let flock = super::v2::realtime::follow_lock(&twin.identity.community_id);
        let _fguard = flock.lock().await;
        if !session.is_valid() {
            return Err("account changed during migration".to_string());
        }
        crate::db::community::reparent_channels_and_fence(&v1_cid, &v2_hex)?;
        crate::db::community::set_migration_ledger(&v1_cid, &v2_hex, PHASE_FLIPPED, "")?;
    }

    // Record the twin in the cross-device community list, the same step `create_community`
    // takes for a normal v2 community. Sibling devices usually discover the twin by folding
    // the carrier themselves, but one that no longer holds the v1 community has no carrier to
    // fold and the list is its only route. Runs AFTER the flip so the list never advertises a
    // half-built twin (pre-refound it is epoch 0 with no snapshot). Best-effort.
    // Durable like every other membership record: a twin whose list entry never lands is
    // the same stranded-behind-a-tombstone hazard as a failed join.
    match super::v2::service::republish_community_list(transport, Some(&twin.identity.community_id)).await {
        Ok(true) => {}
        _ => super::v2::service::republish_community_list_durable(Some(twin.identity.community_id)),
    }
    // Stamp the owner's OWN chats as v2 + notify the UI — the wizard doesn't fold its own
    // carrier, so without this the owner's client would show the stale v1 row until a
    // later fold/boot. Same finalize the member path uses (idempotent if a self-fold beat us).
    spawn_finalize_migration(v1_cid, v2_hex.clone());
    Ok(v2_hex)
}

/// Post-flip finalize: stamp the stitched chats as the v2 community (name/metadata,
/// `proto_version` → 2 monotonic, dissolved=false — the ROOM is alive on v2 even though the
/// v1 row is sealed) and tell the UI. Spawned (SessionGuard captured BEFORE the spawn, per
/// the multi-account contract) so no caller's lock context can deadlock the STATE lock.
pub fn spawn_finalize_migration(v1_cid: String, v2_hex: String) {
    let session = SessionGuard::capture();
    tokio::spawn(async move {
        let v2_id = CommunityId(crate::simd::hex::hex_to_bytes_32(&v2_hex));
        let Ok(Some(twin)) = crate::db::community::load_community_v2(&v2_id) else { return };
        if !session.is_valid() {
            return;
        }
        crate::register_v2_chats_inner(&twin, &session).await;
        // Subscribe to the v2 twin's realtime planes — the flip re-pointed the DB but a
        // migrating MEMBER was never live-listening on the new community, so the owner's
        // subsequent messages wouldn't arrive (they can still SEND — that path is stateless).
        // Idempotent for the owner (already following the twin they created). Mirrors the
        // normal v2-join tail (enqueue_follow + refresh_subscription).
        super::v2::realtime::enqueue_follow(&twin.identity.community_id);
        if let Some(client) = crate::state::nostr_client() {
            super::v2::realtime::refresh_subscription(&client).await;
        }
        crate::emit_event(
            "community_migrated",
            &serde_json::json!({ "v1_community_id": v1_cid, "v2_community_id": v2_hex }),
        );
    });
}

/// Boot / account-swap maintenance: (1) re-drive every held pointer whose flip hasn't
/// landed (crash recovery, stale-root retry, unopenable-`m` retry — `drive_migration` is
/// idempotent and its held-v2 dedup makes the "joined but never flipped" crash a pure
/// flip on re-run); (2) probe every sealed pointer-less community for a payload the client
/// missed (the upgrade-lag sweep). Call at boot and after an account swap.
pub async fn run_migration_maintenance<T: Transport + ?Sized>(transport: &T) -> Vec<String> {
    let session = SessionGuard::capture();
    let mut flipped = Vec::new();
    for cid in crate::db::community::migration_flip_candidates().unwrap_or_default() {
        // The candidate list was pre-fetched: after a swap it describes the WRONG account.
        if !session.is_valid() {
            return flipped;
        }
        let Ok(Some(community)) = crate::db::community::load_community(&CommunityId(
            crate::simd::hex::hex_to_bytes_32(&cid),
        )) else {
            continue;
        };
        match drive_migration(transport, &community).await {
            Ok(Some(v2)) => {
                spawn_finalize_migration(cid, v2.clone());
                flipped.push(v2);
            }
            Ok(None) => {}
            Err(e) => crate::log_warn!("migration retry for {cid}: {e}"),
        }
    }
    flipped.extend(sweep_dissolved_for_migration(transport).await);
    flipped
}

/// Boot / account-swap sweep: for every pointer-less, unchecked v1 community — **sealed or
/// not** — re-probe the rotation-stable dissolved coordinate. Extract + persist any migration
/// payload and drive the flip; a plain `{}` dissolution is marked checked so it is never
/// re-probed.
///
/// Unsealed candidates matter most. Sealing happens inside the control fold, which the boot
/// control probe can veto indefinitely: that probe is `since`-windowed over the CONTROL plane
/// while the authoritative tombstone lives at the DISSOLVED coordinate, so once the cursor
/// passes it a migrated-away community reads as quiet forever and never seals. Probing only
/// sealed rows made that state unreachable by every recovery path at once — the community sat
/// on v1 permanently while its v2 twin, adopted from the Community List, stayed empty (the
/// stitched channel id is already owned by the unsealed v1 row).
pub async fn sweep_dissolved_for_migration<T: Transport + ?Sized>(transport: &T) -> Vec<String> {
    let session = SessionGuard::capture();
    let mut flipped = Vec::new();
    let candidates = crate::db::community::migration_sweep_candidates().unwrap_or_default();
    for cid in candidates {
        // Pre-fetched candidates + a network probe per iteration: re-check the session both
        // before each community and again between the probe and any write, so a mid-sweep
        // swap can't persist pointers/checked markers into the new account's rows.
        if !session.is_valid() {
            return flipped;
        }
        let Ok(Some(community)) = crate::db::community::load_community(&CommunityId(
            crate::simd::hex::hex_to_bytes_32(&cid),
        )) else {
            continue;
        };
        let Some(owner) = super::service::proven_owner_hex(&community) else {
            let _ = crate::db::community::set_migration_checked(&cid);
            continue;
        };
        let records = super::service::dissolved_tombstone_records(transport, &community).await;
        if !session.is_valid() {
            return flipped;
        }
        match select_pointer(&records, &owner) {
            Some((_, raw)) => {
                let _ = crate::db::community::set_migration_pointer(&cid, &raw);
                // Re-load so `server_root_epoch` etc. reflect any prior catch-up.
                if let Ok(Some(fresh)) = crate::db::community::load_community(&community.id) {
                    if let Ok(Some(v2)) = drive_migration(transport, &fresh).await {
                        spawn_finalize_migration(cid.clone(), v2.clone());
                        flipped.push(v2);
                    }
                }
            }
            None => {
                // Plain dissolution vs relay-miss vs stranger-only records. Mark checked
                // (stop re-probing) ONLY when the OWNER's own tombstone is present but carries
                // no payload — a genuine plain dissolution. A non-owner tombstone is
                // member-mintable, so a partial-relay probe returning only a stranger's record
                // must NOT converge the sweep (a real owner carrier could still be unfetched).
                let owner_sealed = records.iter().any(|d| d.author.to_hex() == owner);
                if owner_sealed {
                    let _ = crate::db::community::set_migration_checked(&cid);
                }
            }
        }
    }
    flipped
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::community::{roster, CommunityId};
    use nostr_sdk::prelude::*;

    /// `v1_snapshot_members` seeds v1's authoritative memberlist: the owner and a roster
    /// admin are seeded (the DB re-asserts them), a banned member never is. Uses the DB source
    /// of truth so the v2 seed can't diverge from what v1 shows.
    #[test]
    fn snapshot_member_set_is_v1_memberlist_minus_banned() {
        let _g = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        crate::db::close_database();
        crate::db::clear_id_caches();
        let acct = Keys::generate().public_key().to_bech32().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(&acct)).unwrap();
        crate::db::set_app_data_dir(crate::db::shared_test_data_dir().to_path_buf());
        crate::db::set_current_account(acct.clone()).unwrap();
        crate::db::init_database(&acct).unwrap();

        let owner = Keys::generate();
        let admin = Keys::generate();
        let banned = Keys::generate();

        let mut c = crate::community::Community::create("HQ", "general", vec![]);
        let cid = c.id.to_hex();
        {
            c.owner_attestation = Some(crate::community::owner::build_owner_attestation_unsigned(owner.public_key(), &cid)
                .finalize(&owner).unwrap().as_json());
        }
        crate::db::community::save_community(&c).unwrap();
        crate::db::community::set_community_banlist(&cid, &[banned.public_key().to_hex()], 1).unwrap();
        use crate::community::roles::{CommunityRoles, MemberGrant, Role};
        let role = Role::admin("aa".repeat(32));
        crate::db::community::set_community_roles(&cid, &CommunityRoles {
            roles: vec![role.clone()],
            grants: vec![
                MemberGrant { member: admin.public_key().to_hex(), role_ids: vec![role.role_id.clone()] },
                // A banned member with a stale grant must still be excluded.
                MemberGrant { member: banned.public_key().to_hex(), role_ids: vec![role.role_id] },
            ],
        }, 1).unwrap();

        let members = v1_snapshot_members(&cid, &owner.public_key().to_hex());
        let has = |k: &Keys| members.iter().any(|m| *m == k.public_key());
        assert!(has(&owner), "owner is always seeded");
        assert!(has(&admin), "a roster admin is seeded (re-asserted by v1's memberlist)");
        assert!(!has(&banned), "a banned member is never seeded, even with a stale grant");

        crate::db::close_database();
    }

    /// The drive-in-flight claim is exclusive: a second claim on the same cid is refused
    /// while the first is held, released on drop so a later drive can proceed, and
    /// per-cid independent. This claim is what serializes the wizard against the owner's
    /// own carrier self-fold, and boot maintenance against a live fold.
    #[test]
    fn drive_claim_is_exclusive_and_releases_on_drop() {
        // DRIVE_INFLIGHT is process-global: serialize against every other session/DB test.
        let _g = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        clear_drive_inflight();
        let cid = "ab".repeat(32);
        let other = "cd".repeat(32);

        let first = DriveClaim::take(&cid).expect("a free cid claims");
        assert!(DriveClaim::take(&cid).is_none(), "a second claim on the same cid is refused");
        let _independent = DriveClaim::take(&other).expect("a different cid claims freely");

        drop(first);
        let reclaimed = DriveClaim::take(&cid).expect("drop releases the claim for a later drive");
        assert!(DriveClaim::take(&other).is_none(), "the other cid is still independently held");

        drop(reclaimed);
        clear_drive_inflight();
    }

    /// Drop is generation-aware. An account swap clears the set, so a stale drive still
    /// unwinding must NOT remove the claim the NEW account's drive just inserted for the
    /// same cid — that would let a second same-generation drive run concurrently and
    /// re-open the channel-less-save prune race the claim exists to prevent.
    #[test]
    fn drive_claim_drop_is_generation_aware() {
        // Bumps the global session generation — hold the suite guard so no concurrent test
        // sees its own SessionGuard spuriously invalidated.
        let _g = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        clear_drive_inflight();
        let cid = "ef".repeat(32);

        let stale = DriveClaim::take(&cid).expect("old account's drive claims");
        // The swap: generation advances and the set is cleared (production `swap_session`).
        crate::state::bump_session_generation();
        clear_drive_inflight();
        let fresh = DriveClaim::take(&cid).expect("the new account's drive claims the same cid");

        // The old drive finally unwinds. Its Drop must decline to touch the live claim.
        drop(stale);
        assert!(
            DriveClaim::take(&cid).is_none(),
            "a stale generation's Drop must not release the current account's claim"
        );

        // The current claim still releases normally.
        drop(fresh);
        assert!(DriveClaim::take(&cid).is_some(), "a valid-generation Drop releases");
        clear_drive_inflight();
    }

    /// The state ladder, priority-ordered. The load-bearing case is the SELF-SEAL WINDOW:
    /// the owner's own carrier fold sets dissolved=1 while the ledger still says the flip is
    /// pending. A dissolved-first ladder hides the row entirely and strands the owner in the
    /// one state the resumable wizard was built to recover from.
    #[test]
    fn migration_state_ladder_is_priority_ordered() {
        // migrated is terminal and outranks everything, including a stale ledger row.
        assert_eq!(migration_state(true, 0, false, true, true), "migrated");
        assert_eq!(migration_state(true, PHASE_CARRIER_PUBLISHED, true, true, true), "migrated");

        // THE SELF-SEAL WINDOW: sealed v1 + pending ledger + owner ⇒ Resume, never "dissolved".
        assert_eq!(
            migration_state(false, PHASE_CARRIER_PUBLISHED, true, true, true),
            "in_progress",
            "an owner mid-migration must be offered Resume even though their carrier sealed v1"
        );
        assert!(
            migration_eligible(false, PHASE_CARRIER_PUBLISHED, true, true),
            "the sealed-but-resumable community stays eligible so the command isn't refused"
        );

        // A plain dissolution (no ledger) is terminal for the row.
        assert_eq!(migration_state(false, 0, true, true, true), "dissolved");
        assert!(!migration_eligible(false, 0, true, true), "a plainly dissolved community is not migratable");

        // A MEMBER never sees an in-progress row: ledger rows only exist on the wizard's own
        // account, and the is_owner bind makes that structural rather than incidental.
        assert_eq!(migration_state(false, PHASE_TWIN_MINTED, false, false, true), "not_owner");
        assert!(!migration_eligible(false, PHASE_TWIN_MINTED, false, false));

        // Owner, clean community: the timelock decides.
        assert_eq!(migration_state(false, 0, false, true, true), "ready");
        assert_eq!(migration_state(false, 0, false, true, false), "locked");
        assert!(migration_eligible(false, 0, false, true), "eligibility is the ownership+fence question, not the clock");
    }

    /// The timelock is a real gate on the wizard, not just a UI hint: the boundary second
    /// flips it, and every earlier instant is locked.
    #[test]
    fn wizard_timelock_boundary() {
        assert!(!wizard_unlocked(0));
        assert!(!wizard_unlocked(MIGRATION_UNLOCK_AT - 1));
        assert!(wizard_unlocked(MIGRATION_UNLOCK_AT), "unlocks exactly at the boundary");
        assert!(wizard_unlocked(MIGRATION_UNLOCK_AT + 86_400));
    }

    fn signpost() -> MigrationSignpost {
        MigrationSignpost {
            v2_community_id: "aa".repeat(32),
            owner_xonly: "bb".repeat(32),
            owner_salt: "cc".repeat(32),
            relays: vec!["wss://relay.example.com".into()],
            name: "Team Rocket".into(),
            primary_channel: "dd".repeat(32),
            root_epoch: 3,
        }
    }

    #[test]
    fn payload_roundtrip() {
        let content = build_migration_content(&signpost(), Some("bTEyMw==".into())).unwrap();
        let p = parse_migration_payload(&content).unwrap();
        assert_eq!(p.signpost, signpost());
        assert_eq!(p.m.as_deref(), Some("bTEyMw=="));
    }

    #[test]
    fn plain_dissolution_is_no_payload() {
        assert!(parse_migration_payload("{}").is_none());
        assert!(parse_migration_payload("").is_none());
        assert!(parse_migration_payload("not json at all").is_none());
    }

    #[test]
    fn bad_hex_rejected() {
        for field in ["v2_community_id", "owner_xonly", "owner_salt", "primary_channel"] {
            let mut sp = signpost();
            match field {
                "v2_community_id" => sp.v2_community_id = "zz".repeat(32),
                "owner_xonly" => sp.owner_xonly = "short".into(),
                "owner_salt" => sp.owner_salt = String::new(),
                _ => sp.primary_channel = "gg".repeat(32),
            }
            let content = build_migration_content(&sp, None).unwrap();
            assert!(parse_migration_payload(&content).is_none(), "field {field} accepted");
        }
    }

    #[test]
    fn bounds_enforced() {
        // Oversized m → the whole payload is malformed (fail-safe: plain dissolution).
        let content = build_migration_content(&signpost(), Some("A".repeat(MAX_M_B64 + 1))).unwrap();
        assert!(parse_migration_payload(&content).is_none());
        // Oversized content string → None before any parse.
        assert!(parse_migration_payload(&"x".repeat(MAX_PAYLOAD_CONTENT + 1)).is_none());
        // Hostile relay list degrades by truncation, never amplifies.
        let mut sp = signpost();
        sp.relays = (0..40).map(|i| format!("wss://r{i}.example.com")).collect();
        sp.name = "n".repeat(500);
        let p = parse_migration_payload(&build_migration_content(&sp, None).unwrap()).unwrap();
        assert_eq!(p.signpost.relays.len(), crate::community::MAX_COMMUNITY_RELAYS);
        assert_eq!(p.signpost.name.chars().count(), MAX_SIGNPOST_NAME);
    }

    #[test]
    fn m_seal_open_multi_root() {
        let old_root = [7u8; 32];
        let new_root = [8u8; 32];
        let sealed = seal_m(&old_root, b"join material").unwrap();
        // Newest-first try still finds the older archived root.
        let held = vec![(1u64, old_root), (2u64, new_root)];
        assert_eq!(open_m(&held, &sealed).unwrap(), b"join material");
        // No held root opens it → None (the read-cut member's experience).
        assert!(open_m(&[(2u64, new_root)], &sealed).is_none());
    }

    #[test]
    fn seal_errors_past_nip44_cap() {
        assert!(seal_m(&[1u8; 32], &vec![0u8; 70_000]).is_err());
    }

    /// v0.4.0-ACCEPTANCE PIN (the retrofit-certainty condition): an extended tombstone must
    /// be accepted as a PLAIN dissolution by the exact shipped code paths — signer extracted
    /// at the probe, collected by the fold, content untouched — with a fat payload aboard.
    #[test]
    fn v040_accepts_extended_tombstone_as_plain_dissolution() {
        let owner = Keys::generate();
        let cid = CommunityId([0x42u8; 32]);
        // ~10 KB of key material — a realistic large community.
        let m = Some(base64_simd::STANDARD.encode_to_string(vec![0xabu8; 7_500]));
        let content = build_migration_content(&signpost(), m).unwrap();
        let inner = roster::build_group_dissolved_edition_with_content(&owner, &cid, 1_753_000_000, &content).unwrap();

        // Probe path (dissolved coordinate, id-derived envelope) — v0.4.0's cross-epoch open.
        let outer = roster::seal_dissolved_edition(&Keys::generate(), &inner, &cid).unwrap();
        let signer = roster::dissolved_tombstone_signer(&outer, &cid).expect("v0.4.0 probe must accept");
        assert_eq!(signer, owner.public_key());

        // Fold path — dissolved_by (the v0.4.0 seal signal) collects the owner, and the
        // v0.4.1 extension carries the content verbatim.
        let folded = roster::fold_roster(&[inner.clone()], &cid, &std::collections::HashMap::new());
        assert!(folded.dissolved_by.contains(&owner.public_key()));
        let rec = folded.dissolved_editions.iter().find(|d| d.author == owner.public_key()).unwrap();
        assert_eq!(rec.content, content);

        // And the payload-aware probe extracts the same record.
        let opened = roster::dissolved_tombstone_open(&outer, &cid).unwrap();
        assert_eq!(opened.content, content);
        assert_eq!(opened.author, owner.public_key());
    }

    /// pin: a NEWER payload-less `{}` tombstone seals but never shadows the keys.
    #[test]
    fn payloadless_never_shadows_the_pointer() {
        let owner = Keys::generate();
        let stranger = Keys::generate();
        let cid = CommunityId([0x24u8; 32]);
        let content = build_migration_content(&signpost(), Some("bTEyMw==".into())).unwrap();
        let with_payload = roster::build_group_dissolved_edition_with_content(&owner, &cid, 100, &content).unwrap();
        let plain_newer = roster::build_group_dissolved_edition(&owner, &cid, 200).unwrap();
        let forged = roster::build_group_dissolved_edition_with_content(&stranger, &cid, 300, &content).unwrap();

        let folded = roster::fold_roster(&[plain_newer, with_payload, forged], &cid, &std::collections::HashMap::new());
        let (p, raw) = select_pointer(&folded.dissolved_editions, &owner.public_key().to_hex()).expect("payload survives");
        assert_eq!(p.m.as_deref(), Some("bTEyMw=="));
        assert_eq!(parse_migration_payload(&raw).unwrap(), p);
        // A stranger's payload alone is never selected.
        assert!(select_pointer(&folded.dissolved_editions, &Keys::generate().public_key().to_hex()).is_none());
    }

    #[test]
    fn newest_payload_carrier_wins_with_id_tiebreak() {
        let owner = Keys::generate();
        let cid = CommunityId([0x33u8; 32]);
        let mut sp_old = signpost();
        sp_old.name = "old".into();
        let mut sp_new = signpost();
        sp_new.name = "new".into();
        let a = roster::build_group_dissolved_edition_with_content(
            &owner, &cid, 100, &build_migration_content(&sp_old, None).unwrap()).unwrap();
        let b = roster::build_group_dissolved_edition_with_content(
            &owner, &cid, 200, &build_migration_content(&sp_new, None).unwrap()).unwrap();
        let folded = roster::fold_roster(&[a, b], &cid, &std::collections::HashMap::new());
        let (p, _) = select_pointer(&folded.dissolved_editions, &owner.public_key().to_hex()).unwrap();
        assert_eq!(p.signpost.name, "new");
    }
}
