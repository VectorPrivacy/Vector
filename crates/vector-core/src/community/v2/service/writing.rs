//! Restricted Channels (CORD-03 §1): the writer key, the Writers ledger, and who holds it.

use super::*;
use crate::community::roles::{CommunityRoles, Permissions};
use sha2::Digest;
use crate::community::v2::derive::channel_writer_group_key;
use crate::community::v2::writers::{self, Ledger, Spell};

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

/// The writer secret this device holds for `ch`'s live writer epoch, checked against the ledger.
pub(crate) fn live_writer(community: &CommunityV2, ch: &ChannelV2) -> Option<(Epoch, [u8; 32], GroupKey)> {
    let (epoch, pk) = ch.writers.as_ref()?.current_writer()?;
    let secret = crate::db::community::held_epoch_key(&hex(&community.id().0), &writers::secret_scope(&ch.id), epoch.0).ok()??;
    let group = channel_writer_group_key(&secret, &ch.id, epoch);
    (group.pk().to_bytes() == pk).then_some((epoch, secret, group))
}

/// Where a send of `kind` lands in `ch`: the read group, or the live writer key signing with the
/// read key encrypting, and the writer epoch the rumor commits.
pub(crate) fn send_group(community: &CommunityV2, ch: &ChannelV2, read: &GroupKey, kind: u16) -> Result<(GroupKey, Option<Epoch>), String> {
    if !ch.restricted() || !chat::rides_writer_address(kind) {
        return Ok((read.clone(), None));
    }
    let (wepoch, _, writer) = live_writer(community, ch).ok_or("only this channel's writers can post here")?;
    Ok((GroupKey::from_parts(writer.keys().clone(), *read.conv_key()), Some(wepoch)))
}

/// `rumor` as it is sent in `channel`, committing the live writer epoch when it lands at a writer
/// address. A client precomputing a rumor id builds it through this.
pub fn outbound_rumor(community: &CommunityV2, channel_id: &ChannelId, rumor: nostr_sdk::prelude::UnsignedEvent) -> nostr_sdk::prelude::UnsignedEvent {
    let Some(ch) = community.channel(channel_id) else { return rumor };
    if !ch.restricted() || !chat::rides_writer_address(rumor.kind.as_u16()) {
        return rumor;
    }
    match live_writer(community, ch) {
        Some((wepoch, ..)) => chat::with_writer_epoch(rumor, wepoch),
        None => rumor,
    }
}

/// Whether this device may post in `channel` now: a restricted Channel takes only its writers.
pub fn can_post(community: &CommunityV2, channel_id: &ChannelId) -> bool {
    community.channel(channel_id).is_some_and(|ch| !ch.restricted() || live_writer(community, ch).is_some())
}

/// Every member a writer secret could be owed to, the owner and this device included.
fn writer_candidates(community: &CommunityV2, roster: &CommunityRoles) -> Result<Vec<PublicKey>, String> {
    let mut out: std::collections::BTreeSet<PublicKey> = access_candidates(community, roster)?.into_iter().collect();
    out.insert(community.owner()?);
    out.insert(me_pk()?);
    Ok(out.into_iter().collect())
}

/// Mint writer epoch `ledger.next_epoch()` for `ch`, deliver it to every writer `roster` lets
/// post there but `cut`, and append it to the ledger.
async fn mint<T: Transport + ?Sized>(transport: &T, community: &CommunityV2, ch: &ChannelV2, roster: &CommunityRoles, banned: &std::collections::BTreeSet<String>, cut: &[PublicKey]) -> Result<(), String> {
    let ledger = ch.writers.clone().unwrap_or_else(|| Ledger::new(&ch.id));
    let owner_hex = community.owner()?.to_hex();
    let me = me_pk()?;
    let epoch = ledger.next_epoch();
    let secret = crate::community::random_32();
    let pk = writers::writer_address(&secret, &ch.id, epoch);
    let next = ledger.appended(now_ms(), Spell::Writer { epoch, pk, root: community.root_epoch }).ok_or("this channel's writer ledger can't take another epoch")?;
    let recipients: Vec<PublicKey> = writer_candidates(community, roster)?
        .into_iter()
        .filter(|p| !cut.contains(p) && !banned.contains(&p.to_hex()) && may_write(roster, &owner_hex, &p.to_hex(), ch))
        .collect();
    deliver_on_plane(transport, community, &ch.id, writers::writer_rekey_scope(&ch.id), epoch, &secret, &recipients).await?;
    if recipients.contains(&me) {
        crate::db::community::store_epoch_key(&hex(&community.id().0), &writers::secret_scope(&ch.id), epoch.0, &secret)?;
    }
    publish_ledger(transport, community, ch, &next).await
}

/// Append `open` to `ch`'s ledger: posts move back to the read address.
async fn reopen<T: Transport + ?Sized>(transport: &T, community: &CommunityV2, ch: &ChannelV2) -> Result<(), String> {
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
    persist_writers(&cid_hex, &hex(&ch.id.0), &content, version);
    Ok(())
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
    let candidates = writer_candidates(&held, after)?;
    for ch in &held.channels {
        let moved = restricts(before, ch) != restricts(after, ch);
        match (restricts(after, ch), ch.restricted()) {
            (true, false) if may_manage && moved => mint(transport, &held, ch, after, &banned, &[]).await?,
            (false, true) if may_manage && moved => reopen(transport, &held, ch).await?,
            (true, true) => {
                let lost: Vec<PublicKey> = candidates
                    .iter()
                    .filter(|p| may_write(before, &owner_hex, &p.to_hex(), ch) && !may_write(after, &owner_hex, &p.to_hex(), ch))
                    .copied()
                    .collect();
                if !lost.is_empty() && may_manage {
                    mint(transport, &held, ch, after, &banned, &lost).await?;
                    continue;
                }
                let gained: Vec<PublicKey> = candidates
                    .iter()
                    .filter(|p| !banned.contains(&p.to_hex()) && !may_write(before, &owner_hex, &p.to_hex(), ch) && may_write(after, &owner_hex, &p.to_hex(), ch))
                    .copied()
                    .collect();
                if let (false, Some((epoch, secret, _))) = (gained.is_empty(), live_writer(&held, ch)) {
                    deliver_on_plane(transport, &held, &ch.id, writers::writer_rekey_scope(&ch.id), epoch, &secret, &gained).await?;
                }
            }
            _ => {}
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
    /// Ledgers brought in line with the Channel's permissions.
    pub moved: usize,
}

struct WriterSent;

/// The writer duty (CORD-03 §1), judged on the whole-read `roster`: a manager brings a ledger
/// that disagrees with its Channel's permissions in line, and cuts a writer the Channel no longer
/// lets post who is still served on the access plane and whom it outranks; a writer holding the
/// live secret delivers it to each writer nobody has served.
pub(super) async fn writer_duty<T: Transport + ?Sized>(transport: &T, community: &CommunityV2, roster: &CommunityRoles, banned: &std::collections::BTreeSet<String>) -> Result<WriterDuty, String> {
    let owner_hex = community.owner()?.to_hex();
    let me = me_pk()?;
    let me_hex = me.to_hex();
    let manager = |h: &str| h == owner_hex || (!banned.contains(h) && roster.is_authorized(h, Some(&owner_hex), Permissions::MANAGE_CHANNELS));
    let may_manage = manager(&me_hex);
    let held = crate::db::community::load_community_v2(community.id())?.ok_or("v2 community not found")?;
    if !held.channels.iter().any(|ch| restricts(roster, ch) || ch.restricted()) {
        return Ok(WriterDuty::default());
    }
    let candidates = writer_candidates(&held, roster)?;
    let banned_pks: Vec<PublicKey> = banned.iter().filter_map(|h| PublicKey::from_hex(h).ok()).collect();
    let sent = crate::db::current_session().scoped::<WriterSent, std::sync::Mutex<std::collections::HashSet<([u8; 32], u64, [u8; 32])>>>();
    let mut done = WriterDuty::default();
    for ch in &held.channels {
        let restrict = restricts(roster, ch);
        // A Refounding retires the live writer epoch: the members it removed may hold it.
        let retired = ch.writers.as_ref().and_then(|l| l.live_root()).is_some_and(|r| r < held.root_epoch);
        if may_manage && restrict && retired {
            mint(transport, &held, ch, roster, banned, &[]).await?;
            done.moved += 1;
            continue;
        }
        if may_manage && restrict != ch.restricted() {
            match restrict {
                true => mint(transport, &held, ch, roster, banned, &[]).await?,
                false => reopen(transport, &held, ch).await?,
            }
            done.moved += 1;
            continue;
        }
        let Some((epoch, _)) = ch.writers.as_ref().and_then(|l| l.current_writer()) else { continue };
        if !restrict {
            continue;
        }
        let writes = |p: &PublicKey| !banned.contains(&p.to_hex()) && may_write(roster, &owner_hex, &p.to_hex(), ch);
        let trusted = |d: &PublicKey| writes(d) || manager(&d.to_hex());
        let scope = writers::writer_rekey_scope(&ch.id);
        let deliveries: Vec<rekey::AccessDelivery> = fetch_plane_deliveries(transport, &held, &ch.id, scope).await?.into_iter().filter(|d| d.epoch == epoch).collect();
        if may_manage {
            let out: Vec<PublicKey> = candidates
                .iter()
                .chain(banned_pks.iter())
                .filter(|p| **p != me && !writes(p))
                .filter(|p| served_at(&deliveries, p, scope, epoch, &trusted))
                .filter(|p| me_hex == owner_hex || roster.can_act_on_member(&me_hex, Some(&owner_hex), &p.to_hex(), Permissions::MANAGE_CHANNELS))
                .copied()
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect();
            if !out.is_empty() {
                mint(transport, &held, ch, roster, banned, &out).await?;
                done.cut += out.len();
                continue;
            }
        }
        let Some((_, secret, _)) = live_writer(&held, ch) else { continue };
        let already = sent.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let unserved: Vec<PublicKey> = candidates
            .iter()
            .filter(|p| writes(p) && !already.contains(&(ch.id.0, epoch.0, p.to_bytes())) && !served_at(&deliveries, p, scope, epoch, &trusted))
            .copied()
            .collect();
        if unserved.is_empty() {
            continue;
        }
        deliver_on_plane(transport, &held, &ch.id, scope, epoch, &secret, &unserved).await?;
        sent.lock().unwrap_or_else(|e| e.into_inner()).extend(unserved.iter().map(|p| (ch.id.0, epoch.0, p.to_bytes())));
        done.delivered += unserved.len();
    }
    Ok(done)
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
        let Some((epoch, pk)) = ch.writers.as_ref().and_then(|l| l.current_writer()) else { continue };
        let scope = writers::writer_rekey_scope(&ch.id);
        'deliveries: for d in fetch_plane_deliveries(transport, community, &ch.id, scope).await?.into_iter().filter(|d| d.epoch == epoch) {
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
