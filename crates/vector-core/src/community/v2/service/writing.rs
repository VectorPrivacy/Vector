//! Restricted Channels (CORD-03 §1): the writer key, the Writers ledger, and who holds it.

use super::*;
use crate::community::roles::{CommunityRoles, Permissions};
use crate::community::v2::derive::channel_writer_group_key;
use crate::community::v2::writers::{self, Ledger, Spell};
use sha2::Digest;

/// How long a manager waits before cutting one writer, or minting for itself, again in a Channel.
const BACKSTOP_DAMP_MS: u64 = 6 * 3600 * 1000;

/// How long a manager waits before moving a Channel's ledger in line with its permissions again.
const BACKSTOP_MOVE_MS: u64 = 3600 * 1000;

/// How long a backstop that read the Control Plane short waits before reading it again.
const BACKSTOP_RETRY_MS: u64 = 10 * 60 * 1000;

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
        let mut members: std::collections::HashSet<PublicKey> = stored_memberlist_strict(community)?.into_iter().collect();
        members.insert(community.owner()?);
        Ok(Posters { roster, owner_hex: community.owner()?.to_hex(), members, banned })
    }

    fn writes(&self, p: &PublicKey, ch: &ChannelV2) -> bool {
        self.members.contains(p) && !self.banned.contains(&p.to_hex()) && may_write(self.roster, &self.owner_hex, &p.to_hex(), ch)
    }

    /// Whether `p` manages the Channel's writers: the owner, or a member it hasn't banned holding
    /// Manage Channels.
    fn manages(&self, p: &PublicKey) -> bool {
        self.members.contains(p) && self.vouches_as_manager(p) || p.to_hex() == self.owner_hex
    }

    /// Whether the Roster lets `p` manage, here or not: whom they certified, they vouched for.
    fn vouches_as_manager(&self, p: &PublicKey) -> bool {
        let h = p.to_hex();
        h == self.owner_hex || (!self.banned.contains(&h) && self.roster.is_authorized(&h, Some(&self.owner_hex), Permissions::MANAGE_CHANNELS))
    }
}


/// Whether `p` may no longer post in `ch`: the Roster takes Send, or a ban. One who left the
/// Community needs no stop line: the Guestbook every reader holds already stops them.
fn lost_send(roster: &CommunityRoles, owner_hex: &str, banned: &std::collections::BTreeSet<String>, p: &PublicKey, ch: &ChannelV2) -> bool {
    !may_write(roster, owner_hex, &p.to_hex(), ch) || banned.contains(&p.to_hex())
}

/// The writer secret this device holds for `ch`'s live writer epoch, given or minted here,
/// checked against the ledger.
pub(crate) fn live_writer(community: &CommunityV2, ch: &ChannelV2) -> Option<(Epoch, [u8; 32], GroupKey)> {
    let (epoch, pk) = ch.writers.as_ref()?.current_writer()?;
    let cid_hex = hex(&community.id().0);
    [writers::secret_scope(&ch.id), writers::minted_scope(&ch.id)].iter().find_map(|scope| {
        let secret = crate::db::community::held_epoch_key(&cid_hex, scope, epoch.0).ok()??;
        let group = channel_writer_group_key(&secret, &ch.id, epoch);
        (group.pk().to_bytes() == pk).then_some((epoch, secret, group))
    })
}

/// The live writer key, when the stored Roster lets this device post and it holds the epoch's
/// certificate: a key a manager minted or a cut writer kept is no licence, and a post without
/// its proof would never stand.
fn usable_writer(community: &CommunityV2, ch: &ChannelV2) -> Option<(Epoch, [u8; 32], GroupKey)> {
    let roster = crate::db::community::get_community_roles(&hex(&community.id().0)).ok()?;
    let me = me_pk().ok()?.to_hex();
    if !may_write(&roster, &community.owner().ok()?.to_hex(), &me, ch) {
        return None;
    }
    let live = live_writer(community, ch)?;
    let ledger = ch.writers.as_ref()?;
    let certified = ledger.certifier(live.0).is_none() || my_cert(community, ch, live.0).is_some();
    let me = me_pk().ok()?;
    // Kicked while this epoch held, what this device posts at it stands nowhere: it waits for a fresh
    // one. A manager's posts stand on the Roster, so it never waits.
    let banned: std::collections::BTreeSet<String> = crate::db::community::get_community_banlist(&hex(&community.id().0)).unwrap_or_default().into_iter().collect();
    let manages = Posters::of(community, &roster, &banned).is_ok_and(|p| p.manages(&me));
    let kicked = !manages && stored_departure_windows(community).get(&me).is_some_and(|spans| kicked_in(spans, ledger.writer_start(live.0).unwrap_or(0), u64::MAX));
    (certified && !kicked && !ledger.stopped(live.0, &me.to_bytes(), u64::MAX)).then_some(live)
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
        Some((wepoch, ..)) => as_writer(community, ch, rumor, wepoch),
        None => rumor,
    }
}

/// `rumor` as a writer sends it at `wepoch`: committing the epoch, and carrying this device's
/// certificate for it when it holds one.
pub(crate) fn as_writer(community: &CommunityV2, ch: &ChannelV2, rumor: nostr_sdk::prelude::UnsignedEvent, wepoch: Epoch) -> nostr_sdk::prelude::UnsignedEvent {
    chat::with_writer(rumor, wepoch, my_cert(community, ch, wepoch).as_ref())
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

/// Whether `ch`'s ledger has a head this device couldn't read: minting over it would erase every
/// writer address it names.
fn ledger_unread(community: &CommunityV2, ch: &ChannelV2) -> bool {
    let eid = super::super::derive::writers_locator(community.id(), &ch.id);
    ch.writers.is_none() && crate::db::community::get_edition_head(&hex(&community.id().0), &hex(&eid)).ok().flatten().is_some()
}

/// Mint writer epoch `ledger.next_epoch()` for `ch`, deliver it to every writer `posters` lets
/// post there but `cut`, and append it to the ledger. A retry after a failed publish reuses the
/// secret it already minted, so the blobs it left still serve.
async fn mint<T: Transport + ?Sized>(transport: &T, community: &CommunityV2, ch: &ChannelV2, posters: &Posters<'_>, cut: &[PublicKey]) -> Result<(), String> {
    read_whole(transport, community).await?;
    let away = stored_departure_windows(community);
    mint_carrying(transport, community, ch, posters, cut, &away).await
}

/// Read `community`'s Guestbook whole this session, as whom a mint leaves out or stops is final.
pub(super) async fn read_whole<T: Transport + ?Sized>(transport: &T, community: &CommunityV2) -> Result<(), String> {
    let mut passes = 0;
    while !guestbook_walked_this_session(community) {
        passes += 1;
        if passes > 3 || sync_guestbook(transport, community).await.is_err() {
            return Err(NOT_WALKED.to_string());
        }
    }
    Ok(())
}

/// [`mint`], first stopping in the epoch it closes each member `away` says left while it held:
/// a reader who never reads the Guestbook that says so, as one who joins after a Refounding,
/// learns it from the ledger. A stop that fails leaves the mint for the duty's retry.
async fn mint_carrying<T: Transport + ?Sized>(transport: &T, community: &CommunityV2, ch: &ChannelV2, posters: &Posters<'_>, cut: &[PublicKey], away: &super::DepartureSpans) -> Result<(), String> {
    if carry_departures(transport, community, &ch.id, away).await? {
        let fresh = crate::db::community::load_community_v2(community.id())?.ok_or("this community is gone")?;
        let ch = fresh.channel(&ch.id).cloned().ok_or("no such channel")?;
        return mint_over(transport, &fresh, &ch, posters, cut).await;
    }
    mint_over(transport, community, ch, posters, cut).await
}

/// Stop in `channel`'s live epoch, from when they left, each member away while it held whose
/// posts there this device has seen: anyone else has nothing a later reader could show.
async fn carry_departures<T: Transport + ?Sized>(transport: &T, community: &CommunityV2, channel: &ChannelId, away: &super::DepartureSpans) -> Result<bool, String> {
    let (cid, chan) = (hex(&community.id().0), hex(&channel.0));
    let mut carried = false;
    let mut fresh = community.clone();
    for (writer, spans) in away {
        let Some(ch) = fresh.channel(channel) else { return Ok(carried) };
        let Some(ledger) = ch.writers.as_ref() else { return Ok(carried) };
        let Some((epoch, address)) = ledger.current_writer() else { return Ok(carried) };
        let start = ledger.writer_start(epoch).unwrap_or(0);
        let Some(from) = spans.iter().filter(|(_, until, kicked)| *kicked && until.is_none_or(|u| u > start)).map(|(from, ..)| (*from).max(start)).min() else { continue };
        if ledger.stopped(epoch, &writer.to_bytes(), from) {
            continue;
        }
        let Ok(npub) = nostr_sdk::prelude::ToBech32::to_bech32(writer);
        // Their writer posts this device stored, or holds sent with the key this epoch closes.
        let posted = crate::db::events::posts_since_by(&chan, from, Some(&npub))?.iter().any(|p| p.writer) || crate::db::community::holds_wraps_at(&cid, &chan, &writer.to_hex(), &address);
        if !posted {
            continue;
        }
        // At the cap a stop would retire the live entry: the epoch's window is what holds then.
        if ledger.appended_from(now_ms(), Spell::Stop { writer: writer.to_bytes() }, Some(from)).is_none() {
            return Ok(carried);
        }
        stop_from(transport, &fresh, ch, writer, from).await?;
        carried = true;
        fresh = crate::db::community::load_community_v2(community.id())?.ok_or("this community is gone")?;
    }
    Ok(carried)
}

async fn mint_over<T: Transport + ?Sized>(transport: &T, community: &CommunityV2, ch: &ChannelV2, posters: &Posters<'_>, cut: &[PublicKey]) -> Result<(), String> {
    if ledger_unread(community, ch) {
        return Err("this channel's writer ledger can't be read; nothing minted over it".to_string());
    }
    let ledger = ch.writers.clone().unwrap_or_else(|| Ledger::new(&ch.id));
    let me = me_pk()?;
    let epoch = ledger.next_epoch().ok_or("this channel has no writer epochs left")?;
    let (cid_hex, minted_hex) = (hex(&community.id().0), writers::minted_scope(&ch.id));
    let secret = match crate::db::community::held_epoch_key(&cid_hex, &minted_hex, epoch.0)? {
        Some(minted) => minted,
        None => {
            let fresh = crate::community::random_32();
            crate::db::community::store_epoch_key(&cid_hex, &minted_hex, epoch.0, &fresh)?;
            fresh
        }
    };
    let pk = writers::writer_address(&secret, &ch.id, epoch);
    let certifying = match crate::db::community::held_epoch_key(&cid_hex, &writers::certifier_scope(&ch.id), epoch.0)? {
        Some(held) => held,
        None => {
            let fresh = crate::community::random_32();
            crate::db::community::store_epoch_key(&cid_hex, &writers::certifier_scope(&ch.id), epoch.0, &fresh)?;
            fresh
        }
    };
    let certifier = writers::certifier_of(&certifying).ok_or("couldn't make a writer certifier")?;
    let spell = Spell::Writer { epoch, pk, root: community.root_epoch, by: me.to_bytes(), certifier: Some(certifier) };
    ledger.appended(now_ms(), spell).ok_or("this channel's writer ledger can't take another epoch")?;
    // Nothing goes out for a ledger this device couldn't publish over.
    if !chains_from_head(community, ch, &ledger)? {
        return Err(LEDGER_MOVED.to_string());
    }
    // Who a mint leaves out is final, so it reads the memberlist whole first.
    read_whole(transport, community).await?;
    let read = Posters::of(community, posters.roster, posters.banned)?;
    let posters = &read;
    // Managers hold the key and its certifier too, so any of them can let a writer in.
    let candidates = writer_candidates(community, posters.roster)?;
    let recipients: Vec<PublicKey> = candidates.iter().filter(|p| posters.manages(p) || (!cut.contains(p) && posters.writes(p, ch))).copied().collect();
    let managers: Vec<PublicKey> = recipients.iter().filter(|p| **p != me && posters.manages(p)).copied().collect();
    let certs = certify(community, ch, epoch, &certifying, &recipients);
    if recipients.contains(&me) {
        crate::db::community::store_epoch_key(&cid_hex, &writers::secret_scope(&ch.id), epoch.0, &secret)?;
        if let Some(mine) = certs.get(&me) {
            store_my_cert(community, ch, epoch, mine);
        }
    }
    // Every delivery is built first, the signer's slow part. Then the ledger goes, so a manager
    // that sees it moved stands down and a mint that lost its race sends nothing, then the
    // deliveries at once; a part that fails after is the duty's, and leaves nobody out.
    let mut sends = build_deliveries(community, &ch.id, writers::writer_rekey_scope(&ch.id), epoch, Some(pk), &secret, &recipients, &certs, true).await?;
    if !managers.is_empty() {
        sends.extend(build_deliveries(community, &ch.id, writers::certifier_rekey_scope(&ch.id), epoch, Some(certifier), &certifying, &managers, &Default::default(), false).await?);
    }
    if !crate::db::current_session().is_live() {
        return Err("account changed while minting a writer key".to_string());
    }
    // Building takes a signer round trip per writer: another manager may have moved the ledger.
    if ledger_moved_since(transport, community, ch).await? {
        return Err(LEDGER_MOVED.to_string());
    }
    publish_ledger(transport, community, ch, &ledger, spell, None).await?;
    // Every part matters: a mint missing one leaves nobody out, but serves nobody it carried.
    for wrap in &sends {
        let mut tries = 0;
        while let Err(e) = transport.publish(wrap, &community.relays).await {
            tries += 1;
            if tries >= 3 {
                return Err(e);
            }
            crate::rt::time::sleep(std::time::Duration::from_millis(500 * tries)).await;
        }
    }
    Ok(())
}

/// Whether a reader folding what the relays serve now lands past the head this device holds: the
/// fold's own rules decide, so an edition every reader refuses moves nothing. Read without
/// persisting, so it never waits on a follow the caller may be inside.
async fn ledger_moved_since<T: Transport + ?Sized>(transport: &T, community: &CommunityV2, ch: &ChannelV2) -> Result<bool, String> {
    let eid = super::super::derive::writers_locator(community.id(), &ch.id);
    let held = crate::db::community::get_edition_head(&hex(&community.id().0), &hex(&eid))?;
    Ok(match (held, super::writers_head_on_relays(transport, community, &eid).await?) {
        (_, None) => false,
        (None, Some(_)) => true,
        // A head the relays lost is no move: it stays this device's to build on.
        (Some((held_v, held_hash)), Some((hash, version))) => hash != held_hash && version >= held_v,
    })
}

/// Who holds `ch`'s certifier at writer `epoch`: its minter, and whom a delivery at the
/// certifier scope from the minter or a manager served.
async fn certifier_holders<T: Transport + ?Sized>(transport: &T, held: &CommunityV2, ch: &ChannelV2, epoch: Epoch, pool: &[PublicKey], manages: &(dyn Fn(&PublicKey) -> bool + Sync)) -> Result<std::collections::BTreeSet<PublicKey>, String> {
    let Some(ledger) = ch.writers.as_ref() else { return Ok(Default::default()) };
    let minter = ledger.minter(epoch).and_then(|m| PublicKey::from_slice(&m).ok());
    let cscope = writers::certifier_rekey_scope(&ch.id);
    let named = ledger.certifier(epoch);
    let offered: Vec<rekey::AccessDelivery> = fetch_plane_deliveries(transport, held, &ch.id, cscope).await?.into_iter().filter(|d| d.epoch == epoch && d.writer == named && (Some(d.deliverer) == minter || manages(&d.deliverer))).collect();
    Ok(minter
        .into_iter()
        .chain(pool.iter().filter(|p| offered.iter().any(|d| rekey::find_my_blob(&d.blobs, &d.deliverer.to_bytes(), &p.to_bytes(), cscope, epoch).is_some())).copied())
        .collect())
}

/// Writer certificates from `certifying` for `writers` at `epoch` of `ch` (CORD-03 §1).
fn certify(community: &CommunityV2, ch: &ChannelV2, epoch: Epoch, certifying: &[u8; 32], writers: &[PublicKey]) -> std::collections::HashMap<PublicKey, [u8; 64]> {
    writers
        .iter()
        .filter_map(|w| writers::sign_writer_cert(certifying, &writers::cert_digest(&community.id().0, &ch.id.0, epoch, &w.to_bytes())).map(|c| (*w, c)))
        .collect()
}

/// The certifier secret this device holds for `ch`'s writer `epoch`, minted or handed to it,
/// when it is the one the ledger names.
pub(super) fn certifier_held(community: &CommunityV2, ch: &ChannelV2, epoch: Epoch) -> Option<[u8; 32]> {
    let held = crate::db::community::held_epoch_key(&hex(&community.id().0), &writers::certifier_scope(&ch.id), epoch.0).ok()??;
    let named = ch.writers.as_ref()?.certifier(epoch)?;
    (writers::certifier_of(&held) == Some(named)).then_some(held)
}

fn cert_key(community: &CommunityV2, ch: &ChannelV2, epoch: Epoch) -> String {
    format!("writer_cert:{}:{}:{}", hex(&community.id().0), hex(&ch.id.0), epoch.0)
}

fn store_my_cert(community: &CommunityV2, ch: &ChannelV2, epoch: Epoch, cert: &[u8; 64]) {
    let _ = crate::db::settings::set_sql_setting(cert_key(community, ch, epoch), crate::simd::hex::bytes_to_hex_string(cert));
}

/// This device's writer certificate for `ch`'s writer `epoch`, when it holds one the ledger's
/// certifier signed. One it didn't, from a mint that lost its race, is dropped for a fresh one.
pub(crate) fn my_cert(community: &CommunityV2, ch: &ChannelV2, epoch: Epoch) -> Option<[u8; 64]> {
    let key = cert_key(community, ch, epoch);
    let held = crate::db::settings::get_sql_setting(key.clone()).ok()??;
    let cert: [u8; 64] = crate::simd::hex::hex_string_to_bytes_checked(&held)?.try_into().ok()?;
    let ledger = ch.writers.as_ref()?;
    ledger.certifier(epoch)?;
    if ledger.certifies(&community.id().0, epoch, &me_pk().ok()?.to_bytes(), &cert) {
        return Some(cert);
    }
    let _ = crate::db::settings::remove_setting(&key);
    None
}

/// When a new entry starts: now, but never before the newest post this device holds in the
/// Channel, so a slow clock can't close a window on posts already made. A post dated further
/// ahead than any edition may be is its author's say, not a clock.
async fn entry_time(ch: &ChannelV2) -> u64 {
    let now = now_ms();
    // What the rules held is no post made, whatever date it claims.
    let newest = crate::state::STATE
        .lock()
        .await
        .get_chat(&hex(&ch.id.0))
        .and_then(|c| c.messages.iter().rev().find(|m| !m.flags.is_unauthorized()).map(|m| crate::compact::timestamp_from_compact(m.at)))
        .filter(|t| *t < now + writers::ENTRY_SKEW_MS);
    now.max(newest.map_or(0, |t| t.saturating_add(1)))
}

/// Append `open` to `ch`'s ledger: posts move back to the read address.
async fn reopen<T: Transport + ?Sized>(transport: &T, community: &CommunityV2, ch: &ChannelV2) -> Result<(), String> {
    if ledger_unread(community, ch) {
        return Err("this channel's writer ledger can't be read; nothing appended to it".to_string());
    }
    // An open closes the writer window as a mint does: it carries who was kicked in it first.
    read_whole(transport, community).await?;
    if carry_departures(transport, community, &ch.id, &stored_departure_windows(community)).await? {
        let fresh = crate::db::community::load_community_v2(community.id())?.ok_or("this community is gone")?;
        let ch = fresh.channel(&ch.id).cloned().ok_or("no such channel")?;
        let ledger = ch.writers.clone().unwrap_or_else(|| Ledger::new(&ch.id));
        return publish_ledger(transport, &fresh, &ch, &ledger, Spell::Open, None).await;
    }
    let ledger = ch.writers.clone().unwrap_or_else(|| Ledger::new(&ch.id));
    publish_ledger(transport, community, ch, &ledger, Spell::Open, None).await
}

/// Append a stop for `writer` to `ch`'s ledger: their certificate at the live epoch vouches for
/// nothing they post from now on.
async fn stop<T: Transport + ?Sized>(transport: &T, community: &CommunityV2, ch: &ChannelV2, writer: &PublicKey) -> Result<(), String> {
    if ledger_unread(community, ch) {
        return Err("this channel's writer ledger can't be read; nothing appended to it".to_string());
    }
    let ledger = ch.writers.clone().ok_or("this channel keeps no writer ledger")?;
    publish_ledger(transport, community, ch, &ledger, Spell::Stop { writer: writer.to_bytes() }, None).await
}

/// A stop for `writer` recording that they left at `from`: what a reader who never read the
/// Guestbook that says so needs, once a Refounding moves it.
async fn stop_from<T: Transport + ?Sized>(transport: &T, community: &CommunityV2, ch: &ChannelV2, writer: &PublicKey, from: u64) -> Result<(), String> {
    let ledger = ch.writers.clone().ok_or("this channel keeps no writer ledger")?;
    publish_ledger(transport, community, ch, &ledger, Spell::Stop { writer: writer.to_bytes() }, Some(from)).await
}

/// Whether a stop for `writer` fits `ch`'s ledger (a ledger refuses one that would retire its
/// live writer entry), and isn't one already standing.
pub(super) fn stop_fits(ch: &ChannelV2, writer: &PublicKey) -> bool {
    let Some(ledger) = ch.writers.as_ref() else { return false };
    let Some((epoch, _)) = ledger.current_writer() else { return false };
    !ledger.stopped(epoch, &writer.to_bytes(), u64::MAX) && ledger.appended(now_ms(), Spell::Stop { writer: writer.to_bytes() }).is_some()
}

/// How every refusal that is one Channel's ledger's own begins.
const LEDGER_OWN: &str = "this channel's writer ledger";
const LEDGER_MOVED: &str = "this channel's writer ledger moved; nothing published over it";
const NOT_WALKED: &str = "this community's memberlist isn't read whole yet; nothing minted";

/// Whether an edition appending to `base` would chain from the head this device holds: the
/// stored ledger is `base` at the head's version, or there is neither.
fn chains_from_head(community: &CommunityV2, ch: &ChannelV2, base: &Ledger) -> Result<bool, String> {
    let eid = super::super::derive::writers_locator(community.id(), &ch.id);
    let cid_hex = hex(&community.id().0);
    let held = crate::db::community::get_community_writers(&cid_hex, &hex(&ch.id.0))?.and_then(|(c, v)| Ledger::parse(&c).map(|l| (l, v as u64)));
    Ok(match (crate::db::community::get_edition_head(&cid_hex, &hex(&eid))?, held) {
        (None, _) => base.entries.is_empty() && base.retired.is_empty(),
        (Some((v, _)), Some((held, held_v))) => v == held_v && held == *base,
        (Some(_), None) => false,
    })
}

/// Append `spell` to `base` as `ch`'s ledger, dated now, once the edition passes the rules every
/// reader holds it to: an edition they'd refuse would leave this device alone on it.
async fn publish_ledger<T: Transport + ?Sized>(transport: &T, community: &CommunityV2, ch: &ChannelV2, base: &Ledger, spell: Spell, from: Option<u64>) -> Result<(), String> {
    let next = base.appended_from(entry_time(ch).await, spell, from).ok_or("this channel's writer ledger can't take that")?;
    if !crate::db::current_session().is_live() {
        return Err("account changed while moving a channel's writers".to_string());
    }
    let eid = super::super::derive::writers_locator(community.id(), &ch.id);
    let cid_hex = hex(&community.id().0);
    let now = now_ms();
    let at = now.max(next.entries.last().map_or(0, |e| e.at)) / 1000;
    if !chains_from_head(community, ch, base)? || !next.tail_fits(at, &me_pk()?.to_bytes()) || !next.not_ahead(now) {
        return Err(LEDGER_MOVED.to_string());
    }
    let content = next.to_json();
    publish_control_edition_at(transport, community, vsk::WRITERS, &eid, &content, at).await?;
    let version = crate::db::community::get_edition_head(&cid_hex, &hex(&eid))?.map_or(1, |(v, _)| v);
    persist_writers(&cid_hex, &hex(&ch.id.0), &content, version, false);
    Ok(())
}

/// Read the Control Plane again and return the Channel as it stands, `None` when its ledger moved
/// from `seen`: someone else acted, and a mint on the old head would fork it.
async fn ledger_still<T: Transport + ?Sized>(transport: &T, community: &CommunityV2, channel: &ChannelId, seen: &Option<Ledger>) -> Result<Option<(CommunityV2, ChannelV2)>, String> {
    let fresh = follow_fresh(transport, community.id()).await?.1;
    let Some(ch) = fresh.channel(channel).cloned() else { return Ok(None) };
    Ok((ch.writers == *seen).then_some((fresh, ch)))
}

/// Bring every Channel's writers from what `before` let post to what `after` does (CORD-03 §1):
/// a manager restricts or reopens what this change moved and cuts the writers who lost the bit,
/// and a writer hands the live key to those who gained it. A ledger already out of line is the
/// duty's.
pub(super) async fn settle_writers<T: Transport + ?Sized>(transport: &T, community: &CommunityV2, before: &CommunityRoles, after: &CommunityRoles) -> Result<(), String> {
    let settled = settle_writers_moved(transport, community, before, after).await;
    // The change already stands, so making it again moves nothing: the duty finishes it, soon.
    if settled.is_err() {
        access_duty_now(community.id());
        super::super::realtime::enqueue_follow(community.id());
    }
    settled
}

async fn settle_writers_moved<T: Transport + ?Sized>(transport: &T, community: &CommunityV2, before: &CommunityRoles, after: &CommunityRoles) -> Result<(), String> {
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
    // A Channel whose ledger won't take this never stops the rest; anything else, a relay that
    // isn't answering, stops them all rather than waiting it out once per Channel.
    let mut failed: Option<String> = None;
    for ch in &held.channels {
        if let Err(e) = settle_one(transport, &held, ch, before, after, (&was, &now), &candidates, may_manage).await {
            let theirs = e.starts_with(LEDGER_OWN);
            failed.get_or_insert(e);
            if !theirs {
                break;
            }
        }
    }
    failed.map_or(Ok(()), Err)
}

#[allow(clippy::too_many_arguments)]
async fn settle_one<T: Transport + ?Sized>(
    transport: &T,
    held: &CommunityV2,
    ch: &ChannelV2,
    before: &CommunityRoles,
    after: &CommunityRoles,
    (was, now): (&Posters<'_>, &Posters<'_>),
    candidates: &[PublicKey],
    may_manage: bool,
) -> Result<(), String> {
    let moved = restricts(before, ch) != restricts(after, ch);
    let live = ch.writers.as_ref().and_then(|l| l.current_writer().map(|(e, _)| (l, e)));
    // A manager keeps the key by design, so losing Send stops nothing; one the ledger already
    // stopped is nothing to act on.
    let lost: Vec<PublicKey> = candidates
        .iter()
        .filter(|p| was.writes(p, ch) && !now.writes(p, ch) && !now.manages(p))
        .filter(|p| !live.is_some_and(|(l, e)| l.stopped(e, &p.to_bytes(), u64::MAX)))
        .copied()
        .collect();
    // A manager who lost the bit, or left, and holds this Channel's certifier: only a fresh epoch
    // takes it back.
    let dropped: Vec<PublicKey> = candidates.iter().filter(|p| (p.to_hex() == was.owner_hex || before.is_authorized(&p.to_hex(), Some(&was.owner_hex), Permissions::MANAGE_CHANNELS)) && !now.manages(p)).copied().collect();
    let demoted = match live {
        Some((_, e)) if !dropped.is_empty() && ch.restricted() => certifier_holders(transport, held, ch, e, &dropped, &|d| now.manages(d) || was.manages(d)).await?.iter().any(|h| dropped.contains(h)),
        _ => false,
    };
    let acts = match (restricts(after, ch), ch.restricted()) {
        (true, false) => may_manage && moved,
        (false, true) => may_manage && moved,
        (true, true) => may_manage && (!lost.is_empty() || demoted),
        (false, false) => false,
    };
    if acts {
        let Some((fresh, ch)) = ledger_still(transport, held, &ch.id, &ch.writers).await? else { return Ok(()) };
        return match ch.restricted() {
            false => mint(transport, &fresh, &ch, now, &lost).await,
            true if restricts(after, &ch) => match (demoted, lost.as_slice()) {
                (false, [one]) if stop_fits(&ch, one) => stop(transport, &fresh, &ch, one).await,
                _ => mint(transport, &fresh, &ch, now, &lost).await,
            },
            true => reopen(transport, &fresh, &ch).await,
        };
    }
    if !ch.restricted() || !restricts(after, ch) {
        return Ok(());
    }
    let gained: Vec<PublicKey> = candidates.iter().filter(|p| !was.writes(p, ch) && now.writes(p, ch)).copied().collect();
    if gained.is_empty() {
        return Ok(());
    }
    let live = live_writer(held, ch);
    let certifying = live.as_ref().and_then(|(e, ..)| certifier_held(held, ch, *e));
    // A writer stopped at this epoch, or left out of its whole mint, needs a fresh one: nobody
    // serves them this one.
    let back: std::collections::BTreeSet<PublicKey> = match ch.writers.as_ref().and_then(|l| l.current_writer().map(|(e, _)| (l, e))) {
        Some((ledger, epoch)) => {
            let deliveries = named_deliveries(transport, held, ch, ledger).await?;
            let vouching = |d: &PublicKey, e: Epoch| Some(*d) == ledger.minter(e).and_then(|m| PublicKey::from_slice(&m).ok()) || now.vouches_as_manager(d);
            let left = left_out(&held.id().0, ledger, &deliveries, writers::writer_rekey_scope(&ch.id), &vouching);
            gained.iter().filter(|p| ledger.stopped(epoch, &p.to_bytes(), u64::MAX) || left.contains(p)).copied().collect()
        }
        None => Default::default(),
    };
    let served: Vec<PublicKey> = gained.iter().filter(|p| !back.contains(p)).copied().collect();
    match (live, certifying) {
        _ if !back.is_empty() && may_manage => match ledger_still(transport, held, &ch.id, &ch.writers).await? {
            Some((fresh, ch)) => mint(transport, &fresh, &ch, now, &[]).await,
            None => Ok(()),
        },
        _ if served.is_empty() => Ok(()),
        (Some((epoch, secret, writer)), Some(certifying)) => {
            let certs = certify(held, ch, epoch, &certifying, &served);
            deliver_certified(transport, held, &ch.id, writers::writer_rekey_scope(&ch.id), epoch, Some(writer.pk().to_bytes()), &secret, &served, &certs).await
        }
        // Only an epoch's minter can certify a writer: a manager mints one that includes them.
        _ if may_manage => match ledger_still(transport, held, &ch.id, &ch.writers).await? {
            Some((fresh, ch)) => mint(transport, &fresh, &ch, now, &[]).await,
            None => Ok(()),
        },
        _ => Ok(()),
    }
}

/// Whether a Kick in `spans` reaches into a writer window that began at `start`, by `at_ms`.
fn kicked_in(spans: &[super::super::guestbook::Span], start: u64, at_ms: u64) -> bool {
    spans.iter().any(|(from, until, kicked)| *kicked && *from <= at_ms && until.is_none_or(|u| u > start))
}

/// Who epoch `e` vouches for: its minter, and each writer a `trusted` delivery certified.
fn vouched_at(cid: &[u8; 32], ledger: &Ledger, deliveries: &[rekey::AccessDelivery], trusted: &dyn Fn(&PublicKey, Epoch) -> bool, e: Epoch) -> std::collections::BTreeSet<PublicKey> {
    ledger
        .minter(e)
        .and_then(|m| PublicKey::from_slice(&m).ok())
        .into_iter()
        .chain(deliveries.iter().filter(|d| d.epoch == e && trusted(&d.deliverer, e)).flat_map(|d| d.certs.iter().filter(|(w, c)| ledger.certifies(cid, e, w, c)).filter_map(|(w, _)| PublicKey::from_slice(w).ok())))
        .collect()
}

/// Whom the live epoch's minting delivery left out after the epoch before vouched for them,
/// judged only once every part of it is in: nobody serves them this epoch, whatever a lagging
/// Roster says. A minter's later top-ups are untagged and serve a few: they are never the mint.
fn left_out(cid: &[u8; 32], ledger: &Ledger, deliveries: &[rekey::AccessDelivery], scope: rekey::RekeyScope, trusted: &dyn Fn(&PublicKey, Epoch) -> bool) -> std::collections::BTreeSet<PublicKey> {
    let Some((epoch, _)) = ledger.current_writer() else { return Default::default() };
    let minter = ledger.minter(epoch).and_then(|m| PublicKey::from_slice(&m).ok());
    let minting: Vec<&rekey::AccessDelivery> = deliveries.iter().filter(|d| Some(d.deliverer) == minter && d.epoch == epoch && d.mint.is_some()).collect();
    let parts = minting.iter().filter_map(|d| d.mint.map(|m| m.1)).max().unwrap_or(0);
    if parts == 0 || !(1..=parts).all(|i| minting.iter().any(|d| d.mint.map(|m| m.0) == Some(i))) {
        return Default::default();
    }
    let minted_to = |p: &PublicKey| minting.iter().any(|d| rekey::find_my_blob(&d.blobs, &d.deliverer.to_bytes(), &p.to_bytes(), scope, epoch).is_some());
    ledger.previous_writer().map(|e| vouched_at(cid, ledger, deliveries, trusted, e)).unwrap_or_default().into_iter().filter(|p| !minted_to(p)).collect()
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
struct CertifierSent;
struct BackstopClock;

/// A backstop's lane in one Channel: cutting one member, minting for itself, or moving the ledger
/// in line with the Channel's permissions. Each is damped on its own once it tried.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Lane {
    Cut(PublicKey),
    Starved,
    Move,
    /// After a read that came back short: try again in a while, not every pass.
    Retry,
    /// Minting for writers nobody who can certify them has served.
    Unserved,
    /// Minting to take the certifier back from someone who no longer manages.
    Certifier,
}

impl Lane {
    fn key(self) -> ([u8; 32], u64) {
        match self {
            Lane::Cut(m) => (m.to_bytes(), BACKSTOP_DAMP_MS),
            Lane::Starved => ([0; 32], BACKSTOP_DAMP_MS),
            Lane::Move => ([1; 32], BACKSTOP_MOVE_MS),
            Lane::Certifier => ([2; 32], BACKSTOP_MOVE_MS),
            Lane::Retry => ([3; 32], BACKSTOP_RETRY_MS),
            Lane::Unserved => ([4; 32], BACKSTOP_MOVE_MS),
        }
    }
}

type Clock = std::sync::Mutex<std::collections::HashMap<([u8; 32], [u8; 32]), u64>>;

fn backstop_clock() -> std::sync::Arc<Clock> {
    crate::db::current_session().scoped::<BackstopClock, Clock>()
}

/// Whether this device's backstop may act in `lane` of `channel` now.
fn due(channel: &ChannelId, lane: Lane) -> bool {
    let (lane, period) = lane.key();
    backstop_clock().lock().unwrap_or_else(|e| e.into_inner()).get(&(channel.0, lane)).is_none_or(|t| now_ms().saturating_sub(*t) > period)
}

/// Mark `lanes` of `channel` as tried now.
fn acted(channel: &ChannelId, lanes: &[Lane]) {
    let clock = backstop_clock();
    let mut last = clock.lock().unwrap_or_else(|e| e.into_inner());
    for lane in lanes {
        last.insert((channel.0, lane.key().0), now_ms());
    }
}

struct PostableSince;

fn postable_key(community: &CommunityV2, channel: &ChannelId) -> String {
    format!("writer_postable:{}:{}", hex(&community.id().0), hex(&channel.0))
}

/// Since when this device has seen itself able to post in `ch` without a key: a manager given
/// Send a while into an epoch waits for the key its granter delivers. Kept across restarts.
fn postable_since(community: &CommunityV2, ch: &ChannelV2, postable: bool) -> u64 {
    let key = postable_key(community, &ch.id);
    let cache = crate::db::current_session().scoped::<PostableSince, std::sync::Mutex<std::collections::HashMap<String, u64>>>();
    let mut cache = cache.lock().unwrap_or_else(|e| e.into_inner());
    if !postable {
        // u64::MAX marks one known absent, so a pass writes only when something changed.
        if cache.insert(key.clone(), u64::MAX) != Some(u64::MAX) {
            let _ = crate::db::settings::remove_setting(&key);
        }
        return u64::MAX;
    }
    if let Some(&t) = cache.get(&key).filter(|t| **t != u64::MAX) {
        return t;
    }
    let t = crate::db::settings::get_sql_setting(key.clone()).ok().flatten().and_then(|v| v.parse().ok()).unwrap_or_else(|| {
        let now = now_ms();
        let _ = crate::db::settings::set_sql_setting(key.clone(), now.to_string());
        now
    });
    cache.insert(key, t);
    t
}

#[cfg(test)]
pub(super) fn backdate_postable(community: &CommunityV2, channel: &ChannelId, ms: u64) {
    let key = postable_key(community, channel);
    crate::db::settings::set_sql_setting(key.clone(), ms.to_string()).unwrap();
    crate::db::current_session().scoped::<PostableSince, std::sync::Mutex<std::collections::HashMap<String, u64>>>().lock().unwrap().insert(key, ms);
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
    let keyless = may_manage && restrict && ch.restricted() && posters.writes(&me, ch) && live_writer(held, ch).is_none();
    let since = postable_since(held, ch, keyless);
    let live = ch.writers.as_ref().and_then(|l| l.live());
    // A Refounding retires the live writer epoch: the members it removed may hold it. One whose
    // entry named no certifier predates certificates, so anyone handed its key could post.
    let retired = live.is_some_and(|(root, ..)| root < held.root_epoch) || ch.writers.as_ref().and_then(|l| l.current_writer()).is_some_and(|(e, _)| ch.writers.as_ref().is_some_and(|l| l.certifier(e).is_none()));
    if may_manage && due(&ch.id, Lane::Move) && ((restrict && retired) || restrict != ch.restricted()) {
        if backstop_damped(transport, held, ch, posters, Act::Mint(&[]), &[Lane::Move]).await? {
            done.moved += 1;
        }
        return Ok(());
    }
    let (Some(ledger), Some((root_ok, began))) = (ch.writers.as_ref(), live.map(|(r, _, at)| (r >= held.root_epoch, at))) else { return Ok(()) };
    let Some((epoch, pk)) = ledger.current_writer() else { return Ok(()) };
    if !restrict || !root_ok {
        return Ok(());
    }
    let cid = held.id().0;
    let scope = writers::writer_rekey_scope(&ch.id);
    let deliveries = named_deliveries(transport, held, ch, ledger).await?;
    // Only an epoch's minter and the managers serve someone: a writer's blob can't vouch that
    // they were served. Whom one who left certified, they still vouched for.
    let minted = |d: &PublicKey, e: Epoch| Some(*d) == ledger.minter(e).and_then(|m| PublicKey::from_slice(&m).ok());
    let trusted = |d: &PublicKey, e: Epoch| minted(d, e) || posters.manages(d);
    let vouching = |d: &PublicKey, e: Epoch| minted(d, e) || posters.vouches_as_manager(d);
    // At a certified epoch, served means handed a certificate too.
    let certified = |p: &PublicKey, e: Epoch| {
        ledger.certifier(e).is_none() || deliveries.iter().any(|d| d.epoch == e && trusted(&d.deliverer, e) && d.certs.iter().any(|(w, c)| *w == p.to_bytes() && ledger.certifies(&cid, e, w, c)))
    };
    let served = |p: &PublicKey, e: Epoch| served_at(&deliveries, p, scope, e, &|d| trusted(d, e)) && certified(p, e);
    let vouched = |e: Epoch| vouched_at(&cid, ledger, &deliveries, &vouching, e);
    let certifying = certifier_held(held, ch, epoch);
    if may_manage {
        let now = now_ms();
        // Whoever holds the certifier and no longer manages could vouch for anyone: a fresh epoch
        // takes it back.
        let cscope = writers::certifier_rekey_scope(&ch.id);
        let named = ledger.certifier(epoch);
        let pool: Vec<PublicKey> = candidates.iter().copied().chain(posters.banned.iter().filter_map(|h| PublicKey::from_hex(h).ok())).collect();
        let holding = certifier_holders(transport, held, ch, epoch, &pool, &|d| manager(&d.to_hex())).await?;
        let holds_certifier = |p: &PublicKey| holding.contains(p);
        // Who left is an absence: judged only on a memberlist this session has read whole.
        let walked = guestbook_walked_this_session(held);
        if walked && holding.iter().any(|p| *p != me && !posters.manages(p)) && due(&ch.id, Lane::Certifier) {
            if backstop_damped(transport, held, ch, posters, Act::Mint(&[]), &[Lane::Certifier]).await? {
                done.moved += 1;
            }
            return Ok(());
        }
        // A manager may hold the key by design; a writer the Channel no longer lets post is stopped,
        // on evidence rather than an absence.
        let out: Vec<PublicKey> = vouched(epoch)
            .into_iter()
            .filter(|p| walked && *p != me && !posters.manages(p) && !ledger.stopped(epoch, &p.to_bytes(), now))
            .filter(|p| lost_send(roster, owner_hex, posters.banned, p, ch))
            .filter(|p| me_hex == owner_hex || roster.can_act_on_member(&me_hex, Some(owner_hex), &p.to_hex(), Permissions::MANAGE_CHANNELS))
            .filter(|p| due(&ch.id, Lane::Cut(*p)))
            .collect();
        if !out.is_empty() {
            let lanes: Vec<Lane> = out.iter().map(|p| Lane::Cut(*p)).collect();
            let act = match out.as_slice() {
                [one] => Act::Stop(*one),
                many => Act::Mint(many),
            };
            if backstop_damped(transport, held, ch, posters, act, &lanes).await? {
                done.cut += out.len();
            }
            return Ok(());
        }
        // A manager promoted since the mint is handed the certifier, so it never mints to let a writer in.
        if let Some(certifying) = certifying {
            let sent = crate::db::current_session().scoped::<CertifierSent, std::sync::Mutex<std::collections::HashSet<([u8; 32], u64, [u8; 32])>>>();
            let already = sent.lock().unwrap_or_else(|e| e.into_inner()).clone();
            let owed: Vec<PublicKey> = candidates.iter().filter(|p| **p != me && posters.manages(p) && !holds_certifier(p) && !already.contains(&(ch.id.0, epoch.0, p.to_bytes()))).copied().collect();
            if !owed.is_empty() {
                deliver_on_plane(transport, held, &ch.id, cscope, epoch, named, &certifying, &owed).await?;
                sent.lock().unwrap_or_else(|e| e.into_inner()).extend(owed.iter().map(|p| (ch.id.0, epoch.0, p.to_bytes())));
            }
        }
        // A key it can open, not a blob at its locator: anyone can leave a blob.
        let starved = now.saturating_sub(began.max(since)) > STARVED_MS;
        if keyless && starved && due(&ch.id, Lane::Starved) {
            if backstop_damped(transport, held, ch, posters, Act::Mint(&[]), &[Lane::Starved]).await? {
                done.moved += 1;
            }
            return Ok(());
        }
    }
    // Only a holder of the epoch's certifier can let a writer in; a manager mints the rest one that includes them.
    let live = live_writer(held, ch);
    if certifying.is_none() && !may_manage {
        return Ok(());
    }
    let left = left_out(&cid, ledger, &deliveries, scope, &vouching);
    let cut = |p: &PublicKey| left.contains(p);
    let sent = crate::db::current_session().scoped::<WriterSent, std::sync::Mutex<std::collections::HashSet<([u8; 32], u64, [u8; 32])>>>();
    let already = sent.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let now = now_ms();
    let stopped = |p: &PublicKey| ledger.stopped(epoch, &p.to_bytes(), now);
    // Away from the Community while this epoch held: back, they need a fresh one, as if stopped.
    let away = stored_departure_windows(held);
    let start = ledger.writer_start(epoch).unwrap_or(0);
    let was_away = |p: &PublicKey| away.get(p).is_some_and(|spans| kicked_in(spans, start, now));
    let owed = |p: &PublicKey| (posters.writes(p, ch) || posters.manages(p)) && !stopped(p) && !was_away(p) && !already.contains(&(ch.id.0, epoch.0, p.to_bytes())) && !served(p, epoch) && !cut(p);
    let unserved: Vec<PublicKey> = candidates.iter().filter(|p| **p != me && owed(p)).copied().collect();
    // One this epoch stopped who may post again is owed a fresh epoch, not this one's certificate:
    // judged on a memberlist read whole, and never for a manager, whose posts stand regardless.
    let walked = guestbook_walked_this_session(held);
    // So is one a whole mint left out who, on that memberlist, may post again.
    let back = walked.then(|| candidates.iter().find(|p| posters.writes(p, ch) && !posters.manages(p) && (stopped(p) || cut(p) || was_away(p)))).flatten();
    crate::log_debug!("[v2:writers {}] epoch {} walked={walked} back={:?} unserved={}", &hex(&ch.id.0)[..8], epoch.0, back.map(|p| p.to_hex()[..8].to_string()), unserved.len());
    if let Some(back) = back {
        if may_manage && due(&ch.id, Lane::Unserved) && backstop_damped(transport, held, ch, posters, Act::Restart(*back), &[Lane::Unserved]).await? {
            done.moved += 1;
            return Ok(());
        }
    }
    if unserved.is_empty() {
        return Ok(());
    }
    match (live, certifying) {
        (Some((_, secret, _)), Some(certifying)) => {
            let certs = certify(held, ch, epoch, &certifying, &unserved);
            deliver_certified(transport, held, &ch.id, scope, epoch, Some(pk), &secret, &unserved, &certs).await?;
            sent.lock().unwrap_or_else(|e| e.into_inner()).extend(unserved.iter().map(|p| (ch.id.0, epoch.0, p.to_bytes())));
            done.delivered += unserved.len();
        }
        // A fresh epoch's own deliveries get a while to land before anyone mints over it.
        _ if may_manage && now.saturating_sub(began) > STARVED_MS && unserved.iter().any(|p| posters.writes(p, ch)) && due(&ch.id, Lane::Unserved) && backstop_damped(transport, held, ch, posters, Act::Mint(&[]), &[Lane::Unserved]).await? => done.moved += 1,
        _ => {}
    }
    Ok(())
}

/// [`backstop`], then damping `lanes` unless it found the ledger moved: a failure counts as an
/// attempt, or every pass would mint and deliver again.
async fn backstop_damped<T: Transport + ?Sized>(transport: &T, held: &CommunityV2, ch: &ChannelV2, posters: &Posters<'_>, act: Act<'_>, lanes: &[Lane]) -> Result<bool, String> {
    if !due(&ch.id, Lane::Retry) {
        return Ok(false);
    }
    let result = backstop(transport, held, ch, posters, act).await;
    crate::log_debug!("[v2:writers {}] backstop: {:?}", &hex(&ch.id.0)[..8], result);
    match result {
        Ok(Backstop::Moved) => {}
        Ok(Backstop::Short) => acted(&ch.id, &[Lane::Retry]),
        _ => acted(&ch.id, lanes),
    }
    result.map(|b| b == Backstop::Acted)
}

/// What a backstop does to a restricted Channel's ledger.
#[derive(Clone, Copy)]
pub(super) enum Act<'a> {
    /// Mint a fresh epoch for the writers who remain, leaving these out.
    Mint(&'a [PublicKey]),
    /// Stop one writer, or mint without them when a stop doesn't fit.
    Stop(PublicKey),
    /// Mint a fresh epoch for a stopped writer the Channel lets post again.
    Restart(PublicKey),
}

/// How a backstop ended.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Backstop {
    Acted,
    /// The ledger moved: nothing to damp.
    Moved,
    /// A read came back short.
    Short,
    /// A fresh read of the roster disagrees with the pass's.
    StoodDown,
}

/// A manager's mint or reopen on another's behalf: wait by rank so the highest acts first, read
/// the Control Plane again, and act only if the ledger is where this pass saw it and the roster,
/// read whole again, agrees with the one the pass judged by.
async fn backstop<T: Transport + ?Sized>(transport: &T, held: &CommunityV2, ch: &ChannelV2, posters: &Posters<'_>, act: Act<'_>) -> Result<Backstop, String> {
    let me_hex = me_pk()?.to_hex();
    let steps = if me_hex == posters.owner_hex { 0 } else { posters.roster.highest_position(&me_hex).map_or(10, |p| p.saturating_add(1).min(10)) };
    if steps > 0 {
        // Managers of one rank spread out too.
        let spread = rand::random::<u64>() % RANK_STAGGER_MS;
        crate::rt::time::sleep(std::time::Duration::from_millis(RANK_STAGGER_MS * steps as u64 + spread)).await;
    }
    if !crate::db::current_session().is_live() {
        return Ok(Backstop::Moved);
    }
    let Some((fresh, ch)) = ledger_still(transport, held, &ch.id, &ch.writers).await? else { return Ok(Backstop::Moved) };
    // The roster the pass read may predate the change that moved the ledger.
    let now = fetch_authority(transport, &fresh).await;
    if !now.everyone_served || now.gapped {
        return Ok(Backstop::Short);
    }
    if restricts(&now.roles, &ch) != restricts(posters.roster, &ch) {
        return Ok(Backstop::StoodDown);
    }
    // What justified acting on one writer must hold on what was just read too: the roster, and
    // the Guestbook caught up.
    let mut kept_out: Vec<PublicKey> = Vec::new();
    let act = match act {
        Act::Stop(_) | Act::Restart(_) | Act::Mint([_, ..]) => {
            if sync_guestbook(transport, &fresh).await.is_err() {
                return Ok(Backstop::Short);
            }
            let lost = |p: &PublicKey| lost_send(&now.roles, &posters.owner_hex, posters.banned, p, &ch);
            match act {
                Act::Stop(p) if !lost(&p) => return Ok(Backstop::StoodDown),
                Act::Restart(p) if lost(&p) => return Ok(Backstop::StoodDown),
                Act::Mint(cut) => {
                    kept_out = cut.iter().filter(|p| lost(p)).copied().collect();
                    match kept_out.as_slice() {
                        [] => return Ok(Backstop::StoodDown),
                        [one] => Act::Stop(*one),
                        _ => Act::Mint(&[]),
                    }
                }
                act => act,
            }
        }
        act => act,
    };
    let done = match (restricts(posters.roster, &ch), ch.restricted()) {
        (false, true) => reopen(transport, &fresh, &ch).await,
        (false, false) => return Ok(Backstop::Moved),
        (true, true) => match act {
            Act::Stop(writer) if stop_fits(&ch, &writer) => stop(transport, &fresh, &ch, &writer).await,
            Act::Stop(writer) => mint(transport, &fresh, &ch, posters, &[writer]).await,
            Act::Mint(_) if !kept_out.is_empty() => mint(transport, &fresh, &ch, posters, &kept_out).await,
            Act::Mint(cut) => mint(transport, &fresh, &ch, posters, cut).await,
            Act::Restart(_) => mint(transport, &fresh, &ch, posters, &[]).await,
        },
        (true, false) => mint(transport, &fresh, &ch, posters, &[]).await,
    };
    // Someone else moved the ledger first: nothing was tried, so nothing is damped.
    match done {
        Err(e) if e == LEDGER_MOVED => Ok(Backstop::Moved),
        Err(e) if e == NOT_WALKED || e == super::CONTROL_SHORT => Ok(Backstop::Short),
        Err(e) => Err(e),
        Ok(()) => Ok(Backstop::Acted),
    }
}

#[cfg(test)]
pub(super) async fn settle_for_test<T: Transport + ?Sized>(transport: &T, community: &CommunityV2, before: &CommunityRoles, after: &CommunityRoles) -> Result<(), String> {
    settle_writers_moved(transport, community, before, after).await
}

#[cfg(test)]
pub(super) async fn backstop_act_on<T: Transport + ?Sized>(transport: &T, community: &CommunityV2, channel: &ChannelId, roster: &CommunityRoles, act: Act<'_>) -> Result<Backstop, String> {
    let banned = Default::default();
    let posters = Posters::of(community, roster, &banned)?;
    let ch = community.channel(channel).cloned().ok_or("no such channel")?;
    backstop(transport, community, &ch, &posters, act).await
}

#[cfg(test)]
pub(super) async fn mint_for_test<T: Transport + ?Sized>(transport: &T, community: &CommunityV2, channel: &ChannelId, roster: &CommunityRoles) -> Result<(), String> {
    let banned = Default::default();
    let posters = Posters::of(community, roster, &banned)?;
    let ch = community.channel(channel).cloned().ok_or("no such channel")?;
    mint(transport, community, &ch, &posters, &[]).await
}

#[cfg(test)]
pub(super) const LEDGER_MOVED_FOR_TEST: &str = LEDGER_MOVED;

#[cfg(test)]
pub(super) async fn backstop_on<T: Transport + ?Sized>(transport: &T, community: &CommunityV2, channel: &ChannelId, roster: &CommunityRoles) -> Result<Backstop, String> {
    let banned = Default::default();
    let posters = Posters::of(community, roster, &banned)?;
    let ch = community.channel(channel).cloned().ok_or("no such channel")?;
    backstop(transport, community, &ch, &posters, Act::Mint(&[])).await
}

/// How a restricted Channel's writers' rules judge what reaches a reader (CORD-03 §1).
pub(crate) struct Judge {
    ledger: Ledger,
    community: CommunityV2,
    channel: ChannelV2,
    roster: CommunityRoles,
    owner_hex: String,
    banned: Vec<String>,
    members: std::sync::OnceLock<std::collections::HashSet<String>>,
    departures: std::sync::OnceLock<std::sync::Arc<super::DepartureSpans>>,
    me_manages: bool,
}

/// What a reader does with a post on arrival (CORD-03 §1).
pub(crate) enum Arrival {
    Stands(chat::ChatEvent),
    /// Its wrap waits on this device for a later judgment; a reader who manages the Channel
    /// keeps this marked copy meanwhile.
    Held(Option<chat::ChatEvent>),
    /// A signal of the moment, typing or a Mini App's: nothing to judge later.
    Dropped,
}

fn fleeting(event: &chat::ChatEvent) -> bool {
    matches!(event, chat::ChatEvent::Typing { .. } | chat::ChatEvent::Webxdc { .. })
}

impl Judge {
    /// `None` when the Channel keeps no ledger. A Roster it can't read judges as an empty one:
    /// what that holds waits for the next judgment rather than standing.
    pub(crate) fn of(community: &CommunityV2, ch: &ChannelV2) -> Option<Judge> {
        let ledger = ch.writers.clone()?;
        let cid_hex = hex(&community.id().0);
        let roster = crate::db::community::get_community_roles(&cid_hex).unwrap_or_default();
        let owner_hex = community.owner().map(|o| o.to_hex()).unwrap_or_default();
        let banned = crate::db::community::get_community_banlist(&cid_hex).unwrap_or_default();
        let me = me_pk().map(|p| p.to_hex()).unwrap_or_default();
        let me_manages = !me.is_empty() && (me == owner_hex || (!banned.contains(&me) && roster.is_authorized(&me, Some(&owner_hex), Permissions::MANAGE_CHANNELS)));
        Some(Judge { ledger, community: community.clone(), channel: ch.clone(), roster, owner_hex, banned, members: Default::default(), departures: Default::default(), me_manages })
    }

    fn spans(&self, author: &PublicKey) -> Option<&Vec<super::super::guestbook::Span>> {
        self.departures.get_or_init(|| stored_departure_windows(&self.community)).get(author)
    }

    /// Whether the Guestbook records `author` away from the Community at `at_ms`.
    fn departed(&self, author: &PublicKey, at_ms: u64) -> bool {
        self.spans(author).is_some_and(|spans| spans.iter().any(|(from, until, _)| at_ms >= *from && until.is_none_or(|u| at_ms < u)))
    }

    /// Whether `author` was kicked while writer epoch `e` held, at or before `at_ms`: from then that
    /// epoch's certificate vouches for nothing they post, whatever Join follows. Back, they post on a
    /// fresh epoch, as after a stop. A Leave of their own ends at their next Join.
    fn away_in(&self, e: Epoch, author: &PublicKey, at_ms: u64) -> bool {
        let start = self.ledger.writer_start(e).unwrap_or(0);
        self.spans(author).is_some_and(|spans| kicked_in(spans, start, at_ms))
    }

    fn member(&self, author: &str) -> bool {
        self.members
            .get_or_init(|| stored_memberlist_strict(&self.community).unwrap_or_default().iter().map(|p| p.to_hex()).collect())
            .contains(author)
    }

    /// A post by a member who manages the Channel stands. So does one by a member who may post
    /// there now, sent across a move of the ledger it hadn't read yet: dated within
    /// [`writers::ENTRY_SKEW_MS`] past the entry that moved its address.
    fn vouched_by_roster(&self, author: &str, at_ms: u64, wepoch: Option<Epoch>) -> bool {
        self.vouches(author, at_ms, wepoch, true)
    }

    /// [`Self::vouched_by_roster`]; `gated` reads today's Roster for writers only while the
    /// Channel is restricted, since once open it lets everyone.
    fn vouches(&self, author: &str, at_ms: u64, wepoch: Option<Epoch>, gated: bool) -> bool {
        if author == self.owner_hex {
            return true;
        }
        let manages = self.roster.is_authorized(author, Some(&self.owner_hex), Permissions::MANAGE_CHANNELS);
        let writes = || (!gated || self.channel.restricted()) && self.crossed_a_move(at_ms, wepoch) && may_write(&self.roster, &self.owner_hex, author, &self.channel);
        // The memberlist leaves out the banned; nobody vouches for what they posted while away.
        let away = || PublicKey::from_hex(author).is_ok_and(|p| self.departed(&p, at_ms));
        (manages || writes()) && self.member(author) && !away()
    }

    /// Whether a post dated `at_ms` at writer `wepoch`'s address, or the read address, sits just
    /// past the entry that closed that address to it.
    fn crossed_a_move(&self, at_ms: u64, wepoch: Option<Epoch>) -> bool {
        let just_past = |edge: u64| edge <= at_ms && at_ms - edge < writers::ENTRY_SKEW_MS;
        self.ledger.writers().any(|(e, _, end)| match wepoch {
            None => self.ledger.writer_start(e).is_some_and(just_past),
            Some(w) => e == w && end.is_some_and(just_past),
        })
    }

    /// Whether `event`, carried at writer `wepoch`'s address or the read address, stands by the
    /// ledger alone: an open window takes anyone's, a writer window its certified writers'.
    fn stands_by_ledger(&self, event: &chat::ChatEvent, wepoch: Option<Epoch>) -> bool {
        let o = event.opened();
        // Once the Channel is open, anyone may post at its read address: a writer still posting
        // at its own takes nothing new.
        let open = self.ledger.read_address_takes(o.at_ms);
        match wepoch {
            None => !chat::rides_writer_address(o.rumor.kind.as_u16()) || open,
            Some(e) => {
                let certified = match self.ledger.certifier(e) {
                    Some(_) => chat::writer_cert_of(&o.rumor).is_some_and(|c| self.ledger.certifies(&self.community.id().0, e, &o.author.to_bytes(), &c)),
                    None => true,
                };
                open || (certified && self.ledger.writer_address_takes(e, o.at_ms) && !self.ledger.stopped(e, &o.author.to_bytes(), o.at_ms) && !self.away_in(e, &o.author, o.at_ms) && !self.departed(&o.author, o.at_ms))
            }
        }
    }

    pub(crate) fn sort(&self, mut event: chat::ChatEvent, wepoch: Option<Epoch>) -> Arrival {
        let o = event.opened();
        // A banned author's post never stands, so it isn't worth holding.
        if self.banned.contains(&o.author.to_hex()) {
            return Arrival::Dropped;
        }
        if self.stands_by_ledger(&event, wepoch) || self.vouched_by_roster(&o.author.to_hex(), o.at_ms, wepoch) {
            return Arrival::Stands(event);
        }
        if fleeting(&event) {
            return Arrival::Dropped;
        }
        if self.me_manages && matches!(event, chat::ChatEvent::Message { .. }) {
            event.mark_held();
            return Arrival::Held(Some(event));
        }
        Arrival::Held(None)
    }
}

/// [`Judge::sort`] for a Channel that may keep no ledger.
pub(crate) fn sort_arrival(judge: Option<&Judge>, event: chat::ChatEvent, wepoch: Option<Epoch>) -> Arrival {
    match judge {
        Some(j) => j.sort(event, wepoch),
        None if wepoch.is_none() => Arrival::Stands(event),
        None if fleeting(&event) => Arrival::Dropped,
        None => Arrival::Held(None),
    }
}

/// [`sort_arrival`] for `wrap` as it arrives: what stands, or a manager's marked copy of what
/// doesn't, whose wrap waits for a later judgment.
pub(crate) fn judge_arrival(judge: Option<&Judge>, community: &[u8; 32], channel: &ChannelId, wrap: &nostr_sdk::prelude::Event, event: chat::ChatEvent, wepoch: Option<Epoch>) -> Option<chat::ChatEvent> {
    let (at_ms, author) = (event.opened().at_ms, event.opened().author);
    match sort_arrival(judge, event, wepoch) {
        Arrival::Stands(event) => Some(event),
        // One that couldn't be held is left on the relay for a later read.
        Arrival::Held(marked) => hold(community, channel, wrap, &author, at_ms).then_some(marked).flatten(),
        Arrival::Dropped => None,
    }
}

/// `page`, fetched for `channel_hex` under a Judge built when it was read, judged again as it
/// lands: what the Channel's rules no longer stand behind meanwhile isn't kept (a manager's
/// copy marked), and a later read finds its wrap unsettled.
pub(crate) fn rejudge_page(channel_hex: &str, page: Vec<FetchedEvent>) -> Vec<FetchedEvent> {
    let judged = crate::community::id32_from_hex(channel_hex).and_then(|ch| {
        let cid = crate::db::community::community_id_for_channel(channel_hex).ok().flatten()?;
        let community = crate::db::community::load_community_v2(&crate::community::CommunityId(crate::community::id32_from_hex(&cid)?)).ok().flatten()?;
        let channel = community.channel(&ChannelId(ch))?.clone();
        Some((community.id().0, channel.id, Judge::of(&community, &channel)?))
    });
    let Some((community, channel, judge)) = judged else { return page };
    page.into_iter()
        .filter_map(|f| {
            let (author, at_ms) = (f.event.opened().author, f.event.opened().at_ms);
            let wepoch = chat::writer_epoch_of(&f.event.opened().rumor).ok().flatten();
            match judge.sort(f.event, wepoch) {
                Arrival::Stands(event) => Some(FetchedEvent { event, ..f }),
                // Held to be judged again; a manager's marked copy only with its wrap held.
                Arrival::Held(marked) => {
                    let held = f.wrap.as_ref().is_some_and(|w| hold(&community, &channel, w, &author, at_ms));
                    marked.filter(|_| held).map(|event| FetchedEvent { event, ..f })
                }
                Arrival::Dropped => None,
            }
        })
        .collect()
}

/// The stored Writers ledger of `channel_hex`, as a page is judged on it.
pub(crate) fn writers_head(channel_hex: &str) -> Option<String> {
    let cid = crate::db::community::community_id_for_channel(channel_hex).ok().flatten()?;
    crate::db::community::get_community_writers(&cid, channel_hex).ok().flatten().map(|(c, _)| c)
}

/// A page judged on `judged_on` and stored after the ledger moved owes a retraction from the
/// first entry the move added (or the page's oldest post, if later): the one the move set off may
/// have read the history before it landed. A move that only reopens owes nothing.
pub(crate) fn owe_if_moved(channel_hex: &str, judged_on: Option<String>, since: u64) {
    let Some(head) = writers_head(channel_hex) else { return };
    let Some(cid) = crate::db::community::community_id_for_channel(channel_hex).ok().flatten() else { return };
    let mut owed = false;
    for (writer, from) in crate::db::community::stop_owes(judged_on.as_deref(), &head) {
        owed |= crate::db::community::owe_author_retraction(&cid, channel_hex, &hex(&writer), since.max(from), None).is_ok();
    }
    let everyone = crate::db::community::retraction_since(judged_on.as_deref(), &head);
    if everyone.is_some_and(|from| crate::db::community::owe_writer_retraction(&cid, channel_hex, since.max(from)).is_ok()) || owed {
        if let Some(id) = crate::community::id32_from_hex(&cid) {
            crate::community::v2::realtime::enqueue_follow(&crate::community::CommunityId(id));
        }
    }
}

/// A page stored after a departure it was judged before owes that member's posts a take-back:
/// the drain the departure set off may have read the history before it landed.
pub(crate) fn owe_if_departed(channel_hex: &str, posts: &[(PublicKey, u64)]) {
    if posts.is_empty() || writers_head(channel_hex).is_none() {
        return;
    }
    let Some(cid) = crate::db::community::community_id_for_channel(channel_hex).ok().flatten() else { return };
    let Some(id) = crate::community::id32_from_hex(&cid) else { return };
    let Ok(Some(community)) = crate::db::community::load_community_v2(&crate::community::CommunityId(id)) else { return };
    let away = stored_departure_windows(&community);
    let mut owed = false;
    for (author, at) in posts {
        if away.get(author).is_some_and(|w| w.iter().any(|(from, until, _)| *at >= *from && until.is_none_or(|u| *at < u))) {
            owed |= crate::db::community::owe_author_retraction(&cid, channel_hex, &author.to_hex(), *at, None).is_ok();
        }
    }
    if owed {
        crate::community::v2::realtime::enqueue_follow(community.id());
    }
}

/// Keep `wrap` for a later judgment of `channel`. Whether it is kept.
pub(crate) fn hold(community: &[u8; 32], channel: &ChannelId, wrap: &nostr_sdk::prelude::Event, author: &PublicKey, at_ms: u64) -> bool {
    match crate::db::community::hold_wrap(&hex(community), &hex(&channel.0), &wrap.id.to_hex(), &wrap.as_json(), &author.to_hex(), at_ms) {
        Ok(held) => held,
        Err(e) => {
            crate::log_warn!("[v2:writers] couldn't hold a post for later: {e}");
            false
        }
    }
}

struct Rejudged;

/// What a held post is judged on: once none of it moves, judging it again changes nothing.
fn judged_on(ledger: &Option<Ledger>, roster: &str, members: &std::collections::HashSet<String>, away: &super::DepartureSpans, banned: &[String], me_manages: bool, planes: usize) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    ledger.as_ref().map(|l| l.to_json()).hash(&mut h);
    roster.hash(&mut h);
    away.iter().map(|(p, spans)| (p.to_bytes(), spans)).collect::<Vec<_>>().hash(&mut h);
    let mut m: Vec<&String> = members.iter().collect();
    m.sort();
    m.hash(&mut h);
    banned.hash(&mut h);
    (me_manages, planes).hash(&mut h);
    h.finish()
}

/// Judge again every post `community` holds (CORD-03 §1). What its rules now stand behind joins
/// its Channel unrung, or loses its mark on a manager's device; a reader who now manages the
/// Channel sees what still waits, marked, and one who no longer does stops seeing it. Returns
/// how many posts it let go.
pub async fn rejudge_held(community: &CommunityV2, handler: &dyn crate::event_handler::InboundEventHandler) -> usize {
    crate::db::scoped(rejudge_held_bound(community, handler)).await
}

async fn rejudge_held_bound(community: &CommunityV2, handler: &dyn crate::event_handler::InboundEventHandler) -> usize {
    let cid_hex = hex(&community.id().0);
    if !crate::db::community::holds_wraps(&cid_hex) {
        return 0;
    }
    let (Ok(channels), Ok(me)) = (crate::db::community::held_wrap_channels(&cid_hex), me_pk()) else { return 0 };
    let session = crate::db::current_session();
    let members: std::collections::HashSet<String> = stored_memberlist_strict(community).unwrap_or_default().iter().map(|p| p.to_hex()).collect();
    let roster = crate::db::community::get_community_roles(&cid_hex).ok().and_then(|r| serde_json::to_string(&r).ok()).unwrap_or_default();
    let away = stored_departure_windows(community);
    // By wrap: one held after the last pass, on whatever state its arrival read, is judged again.
    let last = session.scoped::<Rejudged, std::sync::Mutex<std::collections::HashMap<([u8; 32], String), u64>>>();
    let cid = community.id().0;
    let mut released = 0;
    for channel_hex in channels {
        let Some(ch) = crate::community::id32_from_hex(&channel_hex).and_then(|id| community.channel(&ChannelId(id))) else { continue };
        let judge = Judge::of(community, ch);
        let planes = read_planes(community, ch);
        if let Some(j) = &judge {
            let _ = j.members.set(members.clone());
        }
        let print = judged_on(&ch.writers, &roster, &members, &away, judge.as_ref().map_or(&[][..], |j| &j.banned[..]), judge.as_ref().is_some_and(|j| j.me_manages), planes.len());
        let judged = |id: &str| last.lock().unwrap_or_else(|e| e.into_inner()).get(&(cid, id.to_string())) == Some(&print);
        if crate::db::community::held_wrap_ids_in(&cid_hex, &channel_hex).unwrap_or_default().iter().all(|id| judged(id)) {
            continue;
        }
        let Ok(wraps) = crate::db::community::held_wraps_in(&cid_hex, &channel_hex) else { continue };
        for (wrap_id, json) in wraps {
            if !session.is_live() {
                return released;
            }
            if judged(&wrap_id) {
                continue;
            }
            last.lock().unwrap_or_else(|e| e.into_inner()).insert((cid, wrap_id.clone()), print);
            let Ok(wrap) = nostr_sdk::prelude::Event::from_json(&json) else {
                let _ = crate::db::community::release_held_wrap(&wrap_id);
                continue;
            };
            // A key this device doesn't hold yet may come: the wrap waits for it.
            let Some((event, wepoch)) = open_on_planes(ch, &planes, &wrap) else { continue };
            let id = event.opened().rumor_id.to_hex();
            let marked = crate::db::events::get_message_by_id(&id).await.ok().flatten().is_some_and(|(_, m)| m.unauthorized);
            match sort_arrival(judge.as_ref(), event, wepoch) {
                Arrival::Stands(event) => {
                    if marked {
                        let _ = crate::db::events::set_event_unauthorized(&id, false);
                        let shown = crate::state::STATE.lock().await.update_message(&id, |m| m.flags.set_unauthorized(false));
                        if let Some((chat, message)) = shown {
                            handler.on_community_update(&chat, &id, &message);
                        }
                    } else {
                        admit(&event, &channel_hex, community, &me, handler).await;
                    }
                    let _ = crate::db::community::release_held_wrap(&wrap_id);
                    released += 1;
                }
                Arrival::Held(Some(copy)) if !marked => admit(&copy, &channel_hex, community, &me, handler).await,
                Arrival::Held(None) if marked => {
                    let _ = crate::state::STATE.lock().await.remove_message(&id);
                    let _ = crate::db::events::delete_event(&id).await;
                    handler.on_community_removed(&channel_hex, &id);
                }
                // Its author is banned since: nothing to wait for.
                Arrival::Dropped => {
                    let _ = crate::db::community::release_held_wrap(&wrap_id);
                    if marked {
                        let _ = crate::state::STATE.lock().await.remove_message(&id);
                        let _ = crate::db::events::delete_event(&id).await;
                        handler.on_community_removed(&channel_hex, &id);
                    }
                }
                Arrival::Held(_) => {}
            }
        }
    }
    // Forget what no longer waits.
    let waiting: std::collections::HashSet<String> = crate::db::community::held_wraps(&cid_hex).unwrap_or_default().into_iter().map(|(id, ..)| id).collect();
    last.lock().unwrap_or_else(|e| e.into_inner()).retain(|(c, id), _| *c != cid || waiting.contains(id));
    released
}

/// Apply a judged post to its Channel as a late arrival: shown, never rung.
async fn admit(event: &chat::ChatEvent, channel_hex: &str, community: &CommunityV2, me: &PublicKey, handler: &dyn crate::event_handler::InboundEventHandler) {
    use super::super::inbound::ChatPersist;
    match super::super::inbound::persist_routed_chat_event(event, channel_hex, &community.id().0, me).await {
        Some(ChatPersist::New(message)) => handler.on_community_message(channel_hex, &message, false),
        Some(ChatPersist::Updated { message, .. }) | Some(ChatPersist::ReactionRemoved { message, .. }) => handler.on_community_update(channel_hex, &message.id, &message),
        Some(ChatPersist::Removed(target)) => handler.on_community_removed(channel_hex, &target),
        None => {}
    }
}

/// Take back what `ch` shows that the ledger entries this device folded, dated from `since`, no
/// longer stand behind (CORD-03 §1): each post and edit is fetched again and judged as it would be
/// on arrival, except that today's Roster vouches for a writer in flight whatever the Channel is
/// now. One that doesn't stand is held for a later judgment (or, dated past the hour, left on the
/// relays), and only then does a manager mark it, anyone else let it go, or an edit come undone.
/// Returns how many it took back, or an error when a read failed and the rest is still owed.
pub async fn retract_since<T: Transport + ?Sized>(transport: &T, community: &CommunityV2, ch: &ChannelV2, since: u64, handler: &dyn crate::event_handler::InboundEventHandler) -> Result<usize, String> {
    crate::db::scoped(retract_since_bound(transport, community, ch, since, None, None, handler)).await
}

/// [`retract_since`] over one author's posts only: a departure changes nobody else's verdict.
pub async fn retract_by<T: Transport + ?Sized>(transport: &T, community: &CommunityV2, ch: &ChannelV2, author: &PublicKey, since: u64, until: Option<u64>, handler: &dyn crate::event_handler::InboundEventHandler) -> Result<usize, String> {
    let npub = nostr_sdk::prelude::ToBech32::to_bech32(author).map_err(|e| e.to_string())?;
    crate::db::scoped(retract_since_bound(transport, community, ch, since, until, Some(npub), handler)).await
}

/// [`retract_since`] from the earliest of `added`.
#[cfg(test)]
pub(super) async fn retract_after<T: Transport + ?Sized>(transport: &T, community: &CommunityV2, ch: &ChannelV2, added: &[writers::Entry], handler: &dyn crate::event_handler::InboundEventHandler) -> usize {
    match added.iter().map(|e| e.at).min() {
        Some(since) => retract_since(transport, community, ch, since, handler).await.unwrap_or(0),
        None => 0,
    }
}

async fn retract_since_bound<T: Transport + ?Sized>(transport: &T, community: &CommunityV2, ch: &ChannelV2, since: u64, until: Option<u64>, author: Option<String>, handler: &dyn crate::event_handler::InboundEventHandler) -> Result<usize, String> {
    let Some(judge) = Judge::of(community, ch) else { return Ok(0) };
    let chan = hex(&ch.id.0);
    let me = me_pk().ok().and_then(|p| nostr_sdk::prelude::ToBech32::to_bech32(&p).ok());
    let posts = crate::db::events::posts_since_by(&chan, since, author.as_deref())?;
    let posts: Vec<crate::db::events::StoredPost> = posts.into_iter().filter(|p| !p.unauthorized && p.wrap_id.is_some() && p.npub != me && until.is_none_or(|u| p.at_ms < u)).collect();
    // Asked as the reader and, for what a gated relay serves only to its plane, as each read plane.
    let planes = read_planes(community, ch);
    let mut taken = 0;
    // A chunk at a time, so a long history never sits in memory at once.
    for chunk in posts.chunks(100) {
        let ids: Vec<String> = chunk.iter().filter_map(|p| p.wrap_id.clone()).collect();
        let mut wraps: std::collections::HashMap<String, nostr_sdk::prelude::Event> = Default::default();
        // Absence is a verdict: every relay asked must answer, or the rest stays owed.
        let q = crate::community::transport::Query { kinds: vec![stream::KIND_WRAP], ids: ids.clone(), evidence: crate::community::transport::Evidence::Full, ..Default::default() };
        let mut answered = match transport.fetch(&q, &community.relays).await {
            Ok(found) => {
                wraps.extend(found.into_iter().map(|w| (w.id.to_hex(), w)));
                true
            }
            Err(_) => false,
        };
        let missing: Vec<String> = ids.iter().filter(|id| !wraps.contains_key(*id)).cloned().collect();
        for (plane, _) in planes.iter().filter(|_| !missing.is_empty()) {
            if let Some(group) = plane.group() {
                let q = crate::community::transport::Query { kinds: vec![stream::KIND_WRAP], ids: missing.clone(), evidence: crate::community::transport::Evidence::Full, ..Default::default() };
                if let Ok(found) = transport.fetch_plane(group.keys(), &q, &community.relays).await {
                    answered |= found.is_complete();
                    wraps.extend(found.events.into_iter().map(|w| (w.id.to_hex(), w)));
                }
            }
        }
        if !answered {
            return Err("couldn't read the posts to take back".to_string());
        }
        if !crate::db::current_session().is_live() {
            return Err("account changed while taking posts back".to_string());
        }
        for post in chunk {
            let Some(wrap) = post.wrap_id.as_ref().and_then(|w| wraps.get(w)) else { continue };
            let (author, at_ms) = match open_on_planes(ch, &planes, wrap) {
                Some((event, wepoch)) => {
                    let o = event.opened();
                    if o.rumor_id.to_hex() != post.id || judge.banned.contains(&o.author.to_hex()) {
                        continue;
                    }
                    if judge.stands_by_ledger(&event, wepoch) || judge.vouches(&o.author.to_hex(), o.at_ms, wepoch, false) {
                        continue;
                    }
                    (o.author, o.at_ms)
                }
                // A writer-address post whose address the held ledger no longer names stands nowhere;
                // one this device simply holds no key for stays.
                None if post.writer && !ch.writers.as_ref().is_some_and(|l| l.writers().any(|(_, pk, _)| pk == wrap.pubkey.to_bytes())) => match post.npub.as_deref().and_then(|n| PublicKey::parse(n).ok()) {
                    Some(author) => (author, post.at_ms),
                    None => continue,
                },
                None => continue,
            };
            // Nothing goes that a later judgment couldn't reach: it is held here, or, dated past the
            // hour, left on the relays until its date nears.
            let deferred = at_ms > now_ms().saturating_add(crate::db::community::HELD_HORIZON_MS);
            if !(hold(&community.id().0, &ch.id, wrap, &author, at_ms) || deferred) {
                continue;
            }
            taken += 1;
            match &post.edit_of {
                Some(target) => {
                    let _ = crate::db::events::delete_event(&post.id).await;
                    if let Ok(Some((chat, message))) = crate::db::events::get_message_by_id(target).await {
                        let shown = crate::state::STATE.lock().await.update_message_with(target, |m, i| *m = crate::compact::CompactMessage::from_message(&message, i));
                        handler.on_community_update(&chat, target, &shown.map_or(message, |(_, m)| m));
                    }
                }
                // A manager's flag needs a held wrap: one left on the relays is read again later.
                None if judge.me_manages && !deferred => {
                    let _ = crate::db::events::set_event_unauthorized(&post.id, true);
                    let resident = crate::state::STATE.lock().await.update_message(&post.id, |m| m.flags.set_unauthorized(true));
                    let shown = match resident {
                        Some(shown) => Some(shown),
                        None => crate::db::events::get_message_by_id(&post.id).await.ok().flatten(),
                    };
                    if let Some((chat, message)) = shown {
                        handler.on_community_update(&chat, &post.id, &message);
                    }
                }
                None => {
                    let _ = crate::state::STATE.lock().await.remove_message(&post.id);
                    let _ = crate::db::events::delete_event(&post.id).await;
                    handler.on_community_removed(&chan, &post.id);
                }
            }
        }
    }
    Ok(taken)
}

struct OpenedWriterBlobs;

/// Fetch the live writer secret of every restricted Channel this member may post in, or manages,
/// but can't yet, adopting one only if it derives to the address the ledger names; with it, this
/// member's certificate, and for a manager the certifier. Returns how many secrets it took.
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
    let me_hex = me.to_hex();
    let banned = crate::db::community::get_community_banlist(&cid_hex).unwrap_or_default();
    let manages = me_hex == owner_hex || (!banned.contains(&me_hex) && roster.is_authorized(&me_hex, Some(&owner_hex), Permissions::MANAGE_CHANNELS));
    let signer = crate::signer::active_signer()?;
    let opened = crate::db::current_session().scoped::<OpenedWriterBlobs, std::sync::Mutex<std::collections::HashSet<([u8; 32], [u8; 32], [u8; 32], [u8; 32])>>>();
    let mut adopted = 0;
    for ch in community.channels.iter().filter(|c| c.restricted()) {
        let writes = may_write(&roster, &owner_hex, &me_hex, ch);
        let Some(ledger) = ch.writers.as_ref() else { continue };
        let Some((epoch, pk)) = ledger.current_writer() else { continue };
        let need_key = (writes || manages) && live_writer(community, ch).is_none();
        let need_cert = writes && ledger.certifier(epoch).is_some() && my_cert(community, ch, epoch).is_none();
        let need_certifier = manages && certifier_held(community, ch, epoch).is_none() && ledger.certifier(epoch).is_some();
        if !need_key && !need_cert && !need_certifier {
            continue;
        }
        let scope = writers::writer_rekey_scope(&ch.id);
        let deliveries: Vec<rekey::AccessDelivery> = named_deliveries(transport, community, ch, ledger).await?.into_iter().filter(|d| d.epoch == epoch).collect();
        // A certificate is taken only if the ledger's certifier signed it, whoever carried it.
        if need_cert {
            if let Some(cert) = deliveries.iter().flat_map(|d| d.certs.iter()).find(|(who, cert)| *who == me.to_bytes() && ledger.certifies(&community.id().0, epoch, &me.to_bytes(), cert)) {
                store_my_cert(community, ch, epoch, &cert.1);
            }
        }
        if need_key {
            if let Some(secret) = open_first(&signer, &opened, ch, &deliveries, scope, epoch, me, |secret| writers::writer_address(secret, &ch.id, epoch) == pk).await {
                crate::db::community::store_epoch_key(&cid_hex, &writers::secret_scope(&ch.id), epoch.0, &secret)?;
                adopted += 1;
            }
        }
        if need_certifier {
            let named = ledger.certifier(epoch);
            let scope = writers::certifier_rekey_scope(&ch.id);
            let offered: Vec<rekey::AccessDelivery> = fetch_plane_deliveries(transport, community, &ch.id, scope).await?.into_iter().filter(|d| d.epoch == epoch && d.writer == named).collect();
            if let Some(secret) = open_first(&signer, &opened, ch, &offered, scope, epoch, me, |secret| writers::certifier_of(secret) == named).await {
                crate::db::community::store_epoch_key(&cid_hex, &writers::certifier_scope(&ch.id), epoch.0, &secret)?;
            }
        }
    }
    Ok(adopted)
}

type OpenedSet = std::sync::Mutex<std::collections::HashSet<([u8; 32], [u8; 32], [u8; 32], [u8; 32])>>;

/// The first of `deliveries`' blobs for `me` that opens to a secret `fits` takes. Each blob is
/// opened once a session; a junk one never hides a later real one.
#[allow(clippy::too_many_arguments)]
async fn open_first(
    signer: &crate::signer::ActiveSigner,
    opened: &std::sync::Arc<OpenedSet>,
    ch: &ChannelV2,
    deliveries: &[rekey::AccessDelivery],
    scope: rekey::RekeyScope,
    epoch: Epoch,
    me: PublicKey,
    fits: impl Fn(&[u8; 32]) -> bool,
) -> Option<[u8; 32]> {
    for d in deliveries {
        for blob in rekey::find_my_blobs(&d.blobs, &d.deliverer.to_bytes(), &me.to_bytes(), scope, epoch) {
            let seen: [u8; 32] = sha2::Sha256::digest(blob.wrapped.as_bytes()).into();
            if !opened.lock().unwrap_or_else(|e| e.into_inner()).insert((ch.id.0, d.deliverer.to_bytes(), me.to_bytes(), seen)) {
                continue;
            }
            let Ok(secret) = rekey::open_blob(signer, &d.deliverer, scope, epoch, blob).await else { continue };
            if fits(&secret) {
                return Some(secret);
            }
        }
    }
    None
}

/// After a Refounding: re-mint every restricted Channel's writer epoch under the new root, so
/// the members it removed keep no way to post. Best-effort; the duty backs it up.
pub(super) async fn retire_after_refounding<T: Transport + ?Sized>(transport: &T, community: &CommunityV2, away: &super::DepartureSpans) {
    let cid_hex = hex(&community.id().0);
    let Ok(roster) = crate::db::community::get_community_roles(&cid_hex) else { return };
    let (Ok(owner), Ok(me)) = (community.owner(), me_pk()) else { return };
    if me != owner && !roster.is_authorized(&me.to_hex(), Some(&owner.to_hex()), Permissions::MANAGE_CHANNELS) {
        return;
    }
    let banned: std::collections::BTreeSet<String> = crate::db::community::get_community_banlist(&cid_hex).unwrap_or_default().into_iter().collect();
    let Ok(Some(held)) = crate::db::community::load_community_v2(community.id()) else { return };
    let Ok(posters) = Posters::of(&held, &roster, &banned) else { return };
    for id in held.channels.iter().filter(|c| c.restricted() && restricts(&roster, c)).map(|c| c.id).collect::<Vec<_>>() {
        let Some(ch) = held.channel(&id) else { continue };
        if ch.writers.as_ref().and_then(|l| l.live()).is_some_and(|(root, ..)| root < held.root_epoch) {
            if let Err(e) = mint_carrying(transport, &held, ch, &posters, &[], away).await {
                crate::log_warn!("[v2:writers] re-mint after the Refounding failed (the duty retries): {e}");
            }
        }
    }
}
