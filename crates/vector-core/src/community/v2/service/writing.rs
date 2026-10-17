//! Restricted Channels (CORD-03 §1): the writer key, the Writers ledger, and who holds it.

use super::*;
use crate::community::roles::{CommunityRoles, Permissions};
use crate::community::v2::derive::channel_writer_group_key;
use crate::community::v2::writers::{self, Ledger, Spell};
use sha2::Digest;

/// How long a manager waits before cutting or re-minting for a writer again in one Channel.
const BACKSTOP_DAMP_MS: u64 = 6 * 3600 * 1000;

/// How long a manager who may post goes unserved before it mints a key it can use.
const STARVED_MS: u64 = 10 * 60 * 1000;

/// Each rank step down waits this much longer before a backstop mint, so one manager acts.
const RANK_STAGGER_MS: u64 = 2_000;

fn hex(b: &[u8; 32]) -> String {
    crate::simd::hex::bytes_to_hex_32(b)
}

/// Whether `member_hex` may post in `ch` by `roster`.
pub(crate) fn may_write(roster: &CommunityRoles, owner_hex: &str, member_hex: &str, ch: &ChannelV2) -> bool {
    roster.channel_permissions(Some(owner_hex), member_hex, &hex(&ch.id.0), ch.private).contains(Permissions::SEND_MESSAGES)
}

/// Whether `roster` restricts `ch`.
pub(crate) fn restricts(roster: &CommunityRoles, ch: &ChannelV2) -> bool {
    !roster.everyone_sends(&hex(&ch.id.0))
}

/// Who may post in a Channel by a Roster: members it hasn't banned who hold Send there.
struct Posters<'a> {
    roster: &'a CommunityRoles,
    owner_hex: String,
    members: std::collections::HashSet<PublicKey>,
    banned: &'a std::collections::BTreeSet<String>,
}

impl<'a> Posters<'a> {
    fn of(community: &CommunityV2, roster: &'a CommunityRoles, banned: &'a std::collections::BTreeSet<String>) -> Result<Self, String> {
        let mut members: std::collections::HashSet<PublicKey> = stored_memberlist(community)?.into_iter().collect();
        members.insert(community.owner()?);
        Ok(Posters { roster, owner_hex: community.owner()?.to_hex(), members, banned })
    }

    fn writes(&self, p: &PublicKey, ch: &ChannelV2) -> bool {
        self.members.contains(p) && !self.banned.contains(&p.to_hex()) && may_write(self.roster, &self.owner_hex, &p.to_hex(), ch)
    }
}

/// The writer secret this device holds for `ch`'s live writer epoch, checked against the ledger.
pub(crate) fn live_writer(community: &CommunityV2, ch: &ChannelV2) -> Option<(Epoch, [u8; 32], GroupKey)> {
    let (epoch, pk) = ch.writers.as_ref()?.current_writer()?;
    let secret = crate::db::community::held_epoch_key(&hex(&community.id().0), &writers::secret_scope(&ch.id), epoch.0).ok()??;
    let group = channel_writer_group_key(&secret, &ch.id, epoch);
    (group.pk().to_bytes() == pk).then_some((epoch, secret, group))
}

/// The live writer key, when the stored Roster lets this device post: a key a manager minted or
/// a cut writer kept is no licence.
fn usable_writer(community: &CommunityV2, ch: &ChannelV2) -> Option<(Epoch, [u8; 32], GroupKey)> {
    let roster = crate::db::community::get_community_roles(&hex(&community.id().0)).ok()?;
    let me = me_pk().ok()?.to_hex();
    may_write(&roster, &community.owner().ok()?.to_hex(), &me, ch).then(|| live_writer(community, ch)).flatten()
}

/// Where a send of `kind` lands in `ch`: the read group, or the live writer key signing with the
/// read key encrypting, and the writer epoch the rumor commits.
pub(crate) fn send_group(community: &CommunityV2, ch: &ChannelV2, read: &GroupKey, kind: u16) -> Result<(GroupKey, Option<Epoch>), String> {
    if !ch.restricted() || !chat::rides_writer_address(kind) {
        return Ok((read.clone(), None));
    }
    let (wepoch, _, writer) = usable_writer(community, ch).ok_or("only this channel's writers can post here")?;
    Ok((GroupKey::from_parts(writer.keys().clone(), *read.conv_key()), Some(wepoch)))
}

/// `rumor` as it is sent in `channel`, committing the live writer epoch when it lands at a writer
/// address. A client precomputing a rumor id builds it through this.
pub fn outbound_rumor(community: &CommunityV2, channel_id: &ChannelId, rumor: nostr_sdk::prelude::UnsignedEvent) -> nostr_sdk::prelude::UnsignedEvent {
    let Some(ch) = community.channel(channel_id) else { return rumor };
    if !ch.restricted() || !chat::rides_writer_address(rumor.kind.as_u16()) {
        return rumor;
    }
    match usable_writer(community, ch) {
        Some((wepoch, ..)) => chat::with_writer_epoch(rumor, wepoch),
        None => rumor,
    }
}

/// Whether this device may post in `channel` now: a restricted Channel takes only its writers.
pub fn can_post(community: &CommunityV2, channel_id: &ChannelId) -> bool {
    community.channel(channel_id).is_some_and(|ch| !ch.restricted() || usable_writer(community, ch).is_some())
}

/// Every member a writer secret could be owed to, the owner and this device included.
fn writer_candidates(community: &CommunityV2, roster: &CommunityRoles) -> Result<Vec<PublicKey>, String> {
    let mut out: std::collections::BTreeSet<PublicKey> = access_candidates(community, roster)?.into_iter().collect();
    out.insert(community.owner()?);
    out.insert(me_pk()?);
    Ok(out.into_iter().collect())
}

/// The writer-key deliveries on `ch`'s access plane whose claimed address is the one the ledger
/// names for their epoch: a losing fork's or a stranger's blobs serve nobody.
async fn named_deliveries<T: Transport + ?Sized>(transport: &T, community: &CommunityV2, ch: &ChannelV2, ledger: &Ledger) -> Result<Vec<rekey::AccessDelivery>, String> {
    let names: std::collections::HashMap<u64, [u8; 32]> = ledger.writers().map(|(e, pk, _)| (e.0, pk)).collect();
    Ok(fetch_plane_deliveries(transport, community, &ch.id, writers::writer_rekey_scope(&ch.id))
        .await?
        .into_iter()
        .filter(|d| d.writer.is_some() && d.writer == names.get(&d.epoch.0).copied())
        .collect())
}

/// Whether `ch`'s ledger has a head this device couldn't read: minting over it would erase
/// every writer address it names.
fn ledger_unread(community: &CommunityV2, ch: &ChannelV2) -> bool {
    let eid = super::super::derive::writers_locator(community.id(), &ch.id);
    ch.writers.is_none() && crate::db::community::get_edition_head(&hex(&community.id().0), &hex(&eid)).ok().flatten().is_some()
}

/// Mint writer epoch `ledger.next_epoch()` for `ch`, deliver it to every writer `posters` lets
/// post there but `cut`, and append it to the ledger. A retry after a failed publish reuses the
/// secret it already minted, so the blobs it left still serve.
async fn mint<T: Transport + ?Sized>(transport: &T, community: &CommunityV2, ch: &ChannelV2, posters: &Posters<'_>, cut: &[PublicKey]) -> Result<(), String> {
    if ledger_unread(community, ch) {
        return Err("this channel's writer ledger can't be read; nothing minted over it".to_string());
    }
    let ledger = ch.writers.clone().unwrap_or_else(|| Ledger::new(&ch.id));
    let me = me_pk()?;
    let epoch = ledger.next_epoch().ok_or("this channel has no writer epochs left")?;
    let (cid_hex, scope_hex) = (hex(&community.id().0), writers::secret_scope(&ch.id));
    let secret = match crate::db::community::held_epoch_key(&cid_hex, &scope_hex, epoch.0)? {
        Some(minted) => minted,
        None => {
            let fresh = crate::community::random_32();
            crate::db::community::store_epoch_key(&cid_hex, &scope_hex, epoch.0, &fresh)?;
            fresh
        }
    };
    let pk = writers::writer_address(&secret, &ch.id, epoch);
    let next = ledger
        .appended(now_ms(), Spell::Writer { epoch, pk, root: community.root_epoch, by: me.to_bytes() })
        .ok_or("this channel's writer ledger can't take another epoch")?;
    let recipients: Vec<PublicKey> = writer_candidates(community, posters.roster)?.into_iter().filter(|p| !cut.contains(p) && posters.writes(p, ch)).collect();
    deliver_on_plane(transport, community, &ch.id, writers::writer_rekey_scope(&ch.id), epoch, Some(pk), &secret, &recipients).await?;
    publish_ledger(transport, community, ch, &next).await
}

/// Append `open` to `ch`'s ledger: posts move back to the read address.
async fn reopen<T: Transport + ?Sized>(transport: &T, community: &CommunityV2, ch: &ChannelV2) -> Result<(), String> {
    if ledger_unread(community, ch) {
        return Err("this channel's writer ledger can't be read; nothing appended to it".to_string());
    }
    let ledger = ch.writers.clone().unwrap_or_else(|| Ledger::new(&ch.id));
    let next = ledger.appended(now_ms(), Spell::Open).ok_or("this channel's writer ledger can't take that")?;
    publish_ledger(transport, community, ch, &next).await
}

async fn publish_ledger<T: Transport + ?Sized>(transport: &T, community: &CommunityV2, ch: &ChannelV2, next: &Ledger) -> Result<(), String> {
    let eid = super::super::derive::writers_locator(community.id(), &ch.id);
    let content = next.to_json();
    publish_control_edition(transport, community, vsk::WRITERS, &eid, &content).await?;
    let cid_hex = hex(&community.id().0);
    let version = crate::db::community::get_edition_head(&cid_hex, &hex(&eid))?.map_or(1, |(v, _)| v);
    persist_writers(&cid_hex, &hex(&ch.id.0), &content, version, false);
    Ok(())
}

/// Read the Control Plane again and return the Channel as it stands, `None` when its ledger moved
/// from `seen`: someone else acted, and a mint on the old head would fork it.
async fn ledger_still<T: Transport + ?Sized>(transport: &T, community: &CommunityV2, channel: &ChannelId, seen: &Option<Ledger>) -> Result<Option<(CommunityV2, ChannelV2)>, String> {
    follow_control(transport, community).await?;
    let fresh = crate::db::community::load_community_v2(community.id())?.ok_or("v2 community not found")?;
    let Some(ch) = fresh.channel(channel).cloned() else { return Ok(None) };
    Ok((ch.writers == *seen).then_some((fresh, ch)))
}

/// Bring every Channel's writers from what `before` let post to what `after` does (CORD-03 §1):
/// a manager restricts or reopens what this change moved and cuts the writers who lost the bit,
/// and a writer hands the live key to those who gained it. A ledger already out of line is the
/// duty's.
pub(super) async fn settle_writers<T: Transport + ?Sized>(transport: &T, community: &CommunityV2, before: &CommunityRoles, after: &CommunityRoles) -> Result<(), String> {
    let owner_hex = community.owner()?.to_hex();
    let me_hex = me_pk()?.to_hex();
    let may_manage = me_hex == owner_hex || after.is_authorized(&me_hex, Some(&owner_hex), Permissions::MANAGE_CHANNELS);
    let held = crate::db::community::load_community_v2(community.id())?.ok_or("v2 community not found")?;
    if !held.channels.iter().any(|ch| restricts(after, ch) || restricts(before, ch) || ch.restricted()) {
        return Ok(());
    }
    let banned: std::collections::BTreeSet<String> = crate::db::community::get_community_banlist(&hex(&held.id().0)).unwrap_or_default().into_iter().collect();
    let (was, now) = (Posters::of(&held, before, &banned)?, Posters::of(&held, after, &banned)?);
    let mut candidates = writer_candidates(&held, after)?;
    for pk in writer_candidates(&held, before)? {
        if !candidates.contains(&pk) {
            candidates.push(pk);
        }
    }
    for ch in &held.channels {
        let moved = restricts(before, ch) != restricts(after, ch);
        let lost: Vec<PublicKey> = candidates.iter().filter(|p| was.writes(p, ch) && !now.writes(p, ch)).copied().collect();
        let acts = match (restricts(after, ch), ch.restricted()) {
            (true, false) => may_manage && moved,
            (false, true) => may_manage && moved,
            (true, true) => may_manage && !lost.is_empty(),
            (false, false) => false,
        };
        if acts {
            let Some((fresh, ch)) = ledger_still(transport, &held, &ch.id, &ch.writers).await? else { continue };
            match ch.restricted() {
                false => mint(transport, &fresh, &ch, &now, &lost).await?,
                true if restricts(after, &ch) => mint(transport, &fresh, &ch, &now, &lost).await?,
                true => reopen(transport, &fresh, &ch).await?,
            }
            continue;
        }
        if !ch.restricted() || !restricts(after, ch) {
            continue;
        }
        let gained: Vec<PublicKey> = candidates.iter().filter(|p| !was.writes(p, ch) && now.writes(p, ch)).copied().collect();
        if let (false, Some((epoch, secret, writer))) = (gained.is_empty(), live_writer(&held, ch)) {
            deliver_on_plane(transport, &held, &ch.id, writers::writer_rekey_scope(&ch.id), epoch, Some(writer.pk().to_bytes()), &secret, &gained).await?;
        }
    }
    Ok(())
}

/// What one run of the writer duty did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct WriterDuty {
    /// Writer secrets delivered.
    pub delivered: usize,
    /// Writers cut by a fresh writer epoch.
    pub cut: usize,
    /// Ledgers brought in line with the Channel's permissions, or re-minted for a Refounding or
    /// a manager left without the key.
    pub moved: usize,
}

struct WriterSent;
struct BackstopClock;

/// Whether this device may cut or re-mint for a writer in `channel` now. Marks it.
fn backstop_due(channel: &ChannelId) -> bool {
    let clock = crate::db::current_session().scoped::<BackstopClock, std::sync::Mutex<std::collections::HashMap<[u8; 32], u64>>>();
    let mut last = clock.lock().unwrap_or_else(|e| e.into_inner());
    let now = now_ms();
    let due = last.get(&channel.0).is_none_or(|t| now.saturating_sub(*t) > BACKSTOP_DAMP_MS);
    if due {
        last.insert(channel.0, now);
    }
    due
}

/// The writer duty (CORD-03 §1), judged on the whole-read `roster`. A manager brings a ledger
/// that disagrees with the Channel's permissions in line, re-mints a writer epoch a Refounding
/// retired, cuts a writer the Channel no longer lets post who is still served and whom it
/// outranks, and mints for itself when it may post but nobody served it. A writer holding the live
/// secret delivers it to each writer nobody has served, but never to one that epoch's mint left out
/// after they held the one before. One Channel's failure never stops the rest.
pub(super) async fn writer_duty<T: Transport + ?Sized>(transport: &T, community: &CommunityV2, roster: &CommunityRoles, banned: &std::collections::BTreeSet<String>) -> Result<WriterDuty, String> {
    let held = crate::db::community::load_community_v2(community.id())?.ok_or("v2 community not found")?;
    if !held.channels.iter().any(|ch| restricts(roster, ch) || ch.restricted()) {
        return Ok(WriterDuty::default());
    }
    let posters = Posters::of(&held, roster, banned)?;
    let candidates = writer_candidates(&held, roster)?;
    let mut done = WriterDuty::default();
    for ch in &held.channels {
        if let Err(e) = duty_for(transport, &held, ch, &posters, &candidates, &mut done).await {
            crate::log_warn!("[v2:writers] duty for channel {} failed: {e}", &hex(&ch.id.0)[..8]);
        }
    }
    Ok(done)
}

async fn duty_for<T: Transport + ?Sized>(transport: &T, held: &CommunityV2, ch: &ChannelV2, posters: &Posters<'_>, candidates: &[PublicKey], done: &mut WriterDuty) -> Result<(), String> {
    let roster = posters.roster;
    let owner_hex = posters.owner_hex.as_str();
    let me = me_pk()?;
    let me_hex = me.to_hex();
    let manager = |h: &str| h == owner_hex || (!posters.banned.contains(h) && roster.is_authorized(h, Some(owner_hex), Permissions::MANAGE_CHANNELS));
    let may_manage = manager(&me_hex);
    let restrict = restricts(roster, ch);
    let live = ch.writers.as_ref().and_then(|l| l.live());
    // A Refounding retires the live writer epoch: the members it removed may hold it.
    let retired = live.is_some_and(|(root, ..)| root < held.root_epoch);
    if may_manage && ((restrict && retired) || restrict != ch.restricted()) {
        backstop(transport, held, ch, posters, &[]).await?;
        done.moved += 1;
        return Ok(());
    }
    let (Some(ledger), Some((root_ok, minter, began))) = (ch.writers.as_ref(), live.map(|(r, by, at)| (r >= held.root_epoch, by, at))) else { return Ok(()) };
    let Some((epoch, pk)) = ledger.current_writer() else { return Ok(()) };
    if !restrict || !root_ok {
        return Ok(());
    }
    let scope = writers::writer_rekey_scope(&ch.id);
    let deliveries = named_deliveries(transport, held, ch, ledger).await?;
    let minter = PublicKey::from_slice(&minter).ok();
    let trusted = |d: &PublicKey| Some(*d) == minter || posters.writes(d, ch) || manager(&d.to_hex());
    let served = |p: &PublicKey, e: Epoch| served_at(&deliveries, p, scope, e, &trusted);
    if may_manage {
        let banned_pks: Vec<PublicKey> = posters.banned.iter().filter_map(|h| PublicKey::from_hex(h).ok()).collect();
        let out: Vec<PublicKey> = candidates
            .iter()
            .chain(banned_pks.iter())
            .filter(|p| **p != me && !posters.writes(p, ch) && served(p, epoch))
            .filter(|p| me_hex == owner_hex || roster.can_act_on_member(&me_hex, Some(owner_hex), &p.to_hex(), Permissions::MANAGE_CHANNELS))
            .copied()
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        if !out.is_empty() && backstop_due(&ch.id) {
            backstop(transport, held, ch, posters, &out).await?;
            done.cut += out.len();
            return Ok(());
        }
        let starved = posters.writes(&me, ch) && live_writer(held, ch).is_none() && !served(&me, epoch) && now_ms().saturating_sub(began) > STARVED_MS;
        if starved && backstop_due(&ch.id) {
            backstop(transport, held, ch, posters, &[]).await?;
            done.moved += 1;
            return Ok(());
        }
    }
    let Some((_, secret, _)) = live_writer(held, ch) else { return Ok(()) };
    if !trusted(&me) {
        return Ok(());
    }
    // The epoch's mint is the minter's own delivery: whoever held the epoch before and it left
    // out was cut, whatever a lagging Roster says.
    let minting: Vec<&rekey::AccessDelivery> = deliveries.iter().filter(|d| Some(d.deliverer) == minter && d.epoch == epoch).collect();
    let minted_to = |p: &PublicKey| minting.iter().any(|d| rekey::find_my_blob(&d.blobs, &d.deliverer.to_bytes(), &p.to_bytes(), scope, epoch).is_some());
    let previous = ledger.previous_writer();
    let cut = |p: &PublicKey| !minting.is_empty() && !minted_to(p) && previous.is_some_and(|e| served(p, e));
    let sent = crate::db::current_session().scoped::<WriterSent, std::sync::Mutex<std::collections::HashSet<([u8; 32], u64, [u8; 32])>>>();
    let already = sent.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let unserved: Vec<PublicKey> = candidates
        .iter()
        .filter(|p| posters.writes(p, ch) && !already.contains(&(ch.id.0, epoch.0, p.to_bytes())) && !served(p, epoch) && !cut(p))
        .copied()
        .collect();
    if unserved.is_empty() {
        return Ok(());
    }
    deliver_on_plane(transport, held, &ch.id, scope, epoch, Some(pk), &secret, &unserved).await?;
    sent.lock().unwrap_or_else(|e| e.into_inner()).extend(unserved.iter().map(|p| (ch.id.0, epoch.0, p.to_bytes())));
    done.delivered += unserved.len();
    Ok(())
}

/// A manager's mint or reopen on another's behalf: wait by rank so the highest acts first, read
/// the Control Plane again, and act only if the ledger is where this pass saw it.
async fn backstop<T: Transport + ?Sized>(transport: &T, held: &CommunityV2, ch: &ChannelV2, posters: &Posters<'_>, cut: &[PublicKey]) -> Result<(), String> {
    let me_hex = me_pk()?.to_hex();
    let steps = if me_hex == posters.owner_hex { 0 } else { posters.roster.highest_position(&me_hex).map_or(10, |p| p.saturating_add(1).min(10)) };
    if steps > 0 {
        crate::rt::time::sleep(std::time::Duration::from_millis(RANK_STAGGER_MS * steps as u64)).await;
    }
    let Some((fresh, ch)) = ledger_still(transport, held, &ch.id, &ch.writers).await? else { return Ok(()) };
    match (restricts(posters.roster, &ch), ch.restricted()) {
        (false, true) => reopen(transport, &fresh, &ch).await,
        (false, false) => Ok(()),
        _ => mint(transport, &fresh, &ch, posters, cut).await,
    }
}

struct OpenedWriterBlobs;

/// Fetch the live writer secret of every restricted Channel this member may post in but can't
/// yet, adopting one only if it derives to the address the ledger names. Returns how many.
pub async fn collect_writers<T: Transport + ?Sized>(transport: &T, community: &CommunityV2) -> Result<usize, String> {
    crate::db::scoped(collect_writers_bound(transport, community)).await
}

async fn collect_writers_bound<T: Transport + ?Sized>(transport: &T, community: &CommunityV2) -> Result<usize, String> {
    let cid_hex = hex(&community.id().0);
    let roster = crate::db::community::get_community_roles(&cid_hex)?;
    if roster.everyone.is_none() {
        return Ok(0);
    }
    let owner_hex = community.owner()?.to_hex();
    let me = me_pk()?;
    let signer = crate::signer::active_signer()?;
    let opened = crate::db::current_session().scoped::<OpenedWriterBlobs, std::sync::Mutex<std::collections::HashSet<([u8; 32], [u8; 32], [u8; 32], [u8; 32])>>>();
    let mut adopted = 0;
    for ch in community.channels.iter().filter(|c| c.restricted()) {
        if live_writer(community, ch).is_some() || !may_write(&roster, &owner_hex, &me.to_hex(), ch) {
            continue;
        }
        let Some(ledger) = ch.writers.as_ref() else { continue };
        let Some((epoch, pk)) = ledger.current_writer() else { continue };
        let scope = writers::writer_rekey_scope(&ch.id);
        'deliveries: for d in named_deliveries(transport, community, ch, ledger).await?.into_iter().filter(|d| d.epoch == epoch) {
            for blob in rekey::find_my_blobs(&d.blobs, &d.deliverer.to_bytes(), &me.to_bytes(), scope, epoch) {
                // Each blob is opened once a session; a junk one never hides a later real one.
                let seen: [u8; 32] = sha2::Sha256::digest(blob.wrapped.as_bytes()).into();
                if !opened.lock().unwrap_or_else(|e| e.into_inner()).insert((ch.id.0, d.deliverer.to_bytes(), me.to_bytes(), seen)) {
                    continue;
                }
                let Ok(secret) = rekey::open_blob(&signer, &d.deliverer, scope, epoch, blob).await else { continue };
                if writers::writer_address(&secret, &ch.id, epoch) == pk {
                    crate::db::community::store_epoch_key(&cid_hex, &writers::secret_scope(&ch.id), epoch.0, &secret)?;
                    adopted += 1;
                    break 'deliveries;
                }
            }
        }
    }
    Ok(adopted)
}

/// After a Refounding: re-mint every restricted Channel's writer epoch under the new root, so
/// the members it removed keep no way to post. Best-effort; the duty backs it up.
pub(super) async fn retire_after_refounding<T: Transport + ?Sized>(transport: &T, community: &CommunityV2) {
    let cid_hex = hex(&community.id().0);
    let Ok(roster) = crate::db::community::get_community_roles(&cid_hex) else { return };
    let (Ok(owner), Ok(me)) = (community.owner(), me_pk()) else { return };
    if me != owner && !roster.is_authorized(&me.to_hex(), Some(&owner.to_hex()), Permissions::MANAGE_CHANNELS) {
        return;
    }
    let banned: std::collections::BTreeSet<String> = crate::db::community::get_community_banlist(&cid_hex).unwrap_or_default().into_iter().collect();
    let Ok(Some(held)) = crate::db::community::load_community_v2(community.id()) else { return };
    let Ok(posters) = Posters::of(&held, &roster, &banned) else { return };
    for ch in held.channels.iter().filter(|c| c.restricted() && restricts(&roster, c)) {
        if ch.writers.as_ref().and_then(|l| l.live()).is_some_and(|(root, ..)| root < held.root_epoch) {
            if let Err(e) = mint(transport, &held, ch, &posters, &[]).await {
                crate::log_warn!("[v2:writers] re-mint after the Refounding failed (the duty retries): {e}");
            }
        }
    }
}
