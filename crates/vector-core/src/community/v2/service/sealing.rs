//! A Locked Channel's sealed history (CORD-03 §1, [`super::super::seal`]): building it as a run of
//! Locked windows begins and once its first minutes pass, and walking it as a reader scrolls back.

use super::*;
use crate::community::roles::Permissions;
use crate::community::v2::seal::{self, SealPage};
use crate::community::v2::writers::SealRef;

/// How many wraps one read of a read address asks for while sealing.
const SEAL_READ_PAGE: usize = 200;

/// What one second is read with while sealing when a page of it shares that second.
const SEAL_WALL_LIMIT: usize = 5_000;

/// Most ids one fetch names: well inside what relays take in an `ids` filter.
const IDS_PER_FETCH: usize = 200;

/// Pages a session keeps opened; past it they are read again.
const PAGE_CACHE_MAX: usize = 4096;

fn hex(b: &[u8; 32]) -> String {
    crate::simd::hex::bytes_to_hex_32(b)
}

/// Whether this device holds the key to every plane `ch`'s history was written on: a seal it made
/// would otherwise leave out what it couldn't open, for every reader, and a page rides its plane.
pub(super) fn reads_whole_history(community: &CommunityV2, ch: &ChannelV2, planes: &[(ReadPlane, Epoch)]) -> bool {
    let has = |e: u64, public: bool| planes.iter().any(|(p, ep)| ep.0 == e && p.group().is_some() && matches!(p, ReadPlane::Public(..)) == public);
    let roots = |public: bool| (0..=community.root_epoch.0).all(|e| has(e, public));
    if ch.private {
        (1..=ch.epoch.0).all(|e| has(e, false)) && (ch.public_until().is_none() || roots(true))
    } else {
        roots(false)
    }
}

/// Seal `ch` as far as its live run of Locked windows is owed now. See [`seal_history_at`].
pub(crate) async fn seal_history<T: Transport + ?Sized>(transport: &T, community: &CommunityV2, ch: &ChannelV2) -> Result<Option<usize>, String> {
    seal_history_at(transport, community, ch, now_ms()).await
}

/// Seal `ch` as far as its live run of Locked windows is owed at `now`: read what its read
/// addresses hold by date since the last seal, skipping every quiet span, judge each wrap as on
/// arrival, and publish the ids that stand as pages whose oldest names the last seal's first, then
/// the ledger edition naming the newest. Returns how many ids it added, `None` when nothing is owed.
pub(crate) async fn seal_history_at<T: Transport + ?Sized>(transport: &T, community: &CommunityV2, ch: &ChannelV2, now: u64) -> Result<Option<usize>, String> {
    let ledger = ch.writers.clone().ok_or("this channel keeps no writer ledger")?;
    let (Some(until), Some(run)) = (ledger.seal_target(now), ledger.seal_run()) else { return Ok(None) };
    let planes = read_planes(community, ch);
    // Another manager holding every key seals it instead.
    if !reads_whole_history(community, ch, &planes) {
        crate::log_debug!("[v2:writers {}] can't read all of its history, so not sealing it", &hex(&ch.id.0)[..8]);
        return Ok(None);
    }
    // Who may post decides what stands, so the memberlist is read whole first.
    writing::read_whole(transport, community).await?;
    let cid_hex = hex(&community.id().0);
    let held = crate::db::community::load_community_v2(community.id())?.ok_or("this community is gone")?;
    let judge = writing::Judge::of(&held, ch);
    let roster = crate::db::community::get_community_roles(&cid_hex).unwrap_or_default();
    let owner = community.owner()?.to_hex();
    let prev = ledger.seal.clone();

    let mut ids: Vec<([u8; 32], u64, usize)> = Vec::new();
    // Each stretch the read address took things in, newest first, down to the last seal.
    let mut top = until.saturating_sub(1);
    while let Some((floor, range_top)) = ledger.read_range(top) {
        for (i, (plane, epoch)) in planes.iter().enumerate() {
            read_whole_range(transport, community, plane, floor, range_top, |wrap| {
                let Some(event) = plane.open(&wrap, &ch.id, *epoch) else { return };
                let o = event.opened();
                let (at, author, kind) = (o.at_ms, o.author.to_hex(), o.rumor.kind.as_u16());
                if at < floor || at > range_top {
                    return;
                }
                // A reaction its author couldn't make no reader counts, so none is kept.
                if kind == super::super::kind::REACTION && !roster.channel_permissions(Some(&owner), &author, &hex(&ch.id.0), ch.private).contains(Permissions::ADD_REACTIONS) {
                    return;
                }
                if matches!(writing::sort_arrival(judge.as_ref(), event, None), writing::Arrival::Stands(_)) {
                    ids.push((wrap.id.to_bytes(), at, i));
                }
            })
            .await?;
        }
        let Some(below) = floor.checked_sub(1) else { break };
        top = below;
    }

    let mut pages = seal::paginate(ids);
    let added: usize = pages.iter().map(|(p, _)| p.ids.len()).sum();
    // Dated inside what the seal covers, so no read by date ever meets them.
    let dated = until.saturating_sub(1);
    // Built oldest first, so each names the one after it; the oldest names the last seal's first.
    let mut next: Option<([u8; 32], Epoch)> = prev.as_ref().and_then(|s| s.page.map(|p| (p, s.read)));
    let mut wraps = Vec::with_capacity(pages.len());
    for (page, i) in pages.iter_mut().rev() {
        let (plane, epoch) = &planes[*i];
        let group = plane.group().ok_or("a page's read plane has no key here")?;
        page.next = next;
        let wrap = seal::page_wrap(page, &ch.id, *epoch, group, dated)?;
        next = Some((wrap.id.to_bytes(), *epoch));
        wraps.push(wrap);
    }
    for wrap in &wraps {
        transport.publish_durable(wrap, &community.relays).await?;
    }
    if !crate::db::current_session().is_live() {
        return Err("account changed while sealing a channel".to_string());
    }
    let fresh = crate::db::community::load_community_v2(community.id())?.ok_or("this community is gone")?;
    let ch = fresh.channel(&ch.id).cloned().ok_or("no such channel")?;
    let base = ch.writers.clone().ok_or("this channel keeps no writer ledger")?;
    // Another manager, or a lock moving under the read, makes this seal no longer the one owed.
    if base.seal_target(now) != Some(until) || writing::ledger_moved_since(transport, &fresh, &ch).await? {
        return Err("this channel's writer ledger moved; nothing sealed".to_string());
    }
    let sealed = SealRef {
        page: next.map(|(p, _)| p),
        read: next.map_or(Epoch(0), |(_, r)| r),
        until,
        run,
        count: prev.map_or(0, |s| s.count) + added as u64,
        extra: Default::default(),
    };
    writing::publish_seal(transport, &fresh, &ch, &base, sealed).await?;
    Ok(Some(added))
}

/// Each wrap `plane` holds dated from `floor` to `top` (unix ms), handed to `each` a page at a
/// time, read whole: every page answered by every relay, or an error. A seal can't leave out what
/// a slow relay held.
async fn read_whole_range<T: Transport + ?Sized>(transport: &T, community: &CommunityV2, plane: &ReadPlane, floor: u64, top: u64, mut each: impl FnMut(Event)) -> Result<(), String> {
    let mut seen: std::collections::HashSet<nostr_sdk::prelude::EventId> = std::collections::HashSet::new();
    let mut cursor = top / 1000;
    let read = |q: Query| async move {
        match plane.group() {
            Some(group) => {
                let page = transport.fetch_plane(group.keys(), &q, &community.relays).await?;
                if !page.is_complete() {
                    return Err("a relay didn't answer the whole read; nothing sealed".to_string());
                }
                Ok(page.events)
            }
            None => transport.fetch(&q, &community.relays).await,
        }
    };
    loop {
        let q = Query { since: Some(floor / 1000), evidence: crate::community::transport::Evidence::Full, ..plane_page(plane.address().to_hex(), SEAL_READ_PAGE, Some(cursor)) };
        let evs = read(q.clone()).await?;
        let full = evs.len() >= SEAL_READ_PAGE;
        let oldest = evs.iter().map(|e| e.created_at.as_secs()).min();
        let mut fresh = false;
        for e in evs {
            if seen.insert(e.id) {
                fresh = true;
                each(e);
            }
        }
        let Some(oldest) = oldest else { break };
        // Inclusive, so the next read starts at the page's oldest second. A page all in one
        // second, or one that brought nothing new from it, is that second read on its own with
        // room to spare, then stepped past.
        let next = if oldest == cursor && (full || !fresh) {
            for e in read(Query { since: Some(oldest), limit: Some(SEAL_WALL_LIMIT), ..q }).await? {
                if seen.insert(e.id) {
                    each(e);
                }
            }
            oldest.saturating_sub(1)
        } else if fresh {
            oldest
        } else {
            break;
        };
        if next < floor / 1000 || oldest == 0 {
            break;
        }
        cursor = next;
    }
    Ok(())
}

struct SealPages;
type PageCache = std::sync::Mutex<std::collections::HashMap<[u8; 32], (SealPage, PublicKey)>>;

/// The page `link` names, with the address of the plane it rides, from this session's cache or the
/// relays. `None` when no relay serves it, or this device holds no plane of the read epoch it
/// opens under. A Channel made private has public epochs alongside its own, so each plane of that
/// epoch is tried.
async fn load_page<T: Transport + ?Sized>(transport: &T, community: &CommunityV2, ch: &ChannelV2, link: ([u8; 32], Epoch)) -> Result<Option<(SealPage, PublicKey)>, String> {
    let cache = crate::db::current_session().scoped::<SealPages, PageCache>();
    if let Some(p) = cache.lock().unwrap_or_else(|e| e.into_inner()).get(&link.0) {
        return Ok(Some(p.clone()));
    }
    let q = Query { ids: vec![hex(&link.0)], evidence: crate::community::transport::Evidence::Quorum, ..Default::default() };
    for (plane, _) in read_planes(community, ch).into_iter().filter(|(_, e)| *e == link.1) {
        // Asked as the plane's key where this device holds it: a gating relay serves a plane only so.
        let fetched = match plane.group() {
            Some(group) => transport.fetch_plane(group.keys(), &q, &community.relays).await?.events,
            None => transport.fetch(&q, &community.relays).await?,
        };
        if let Some(page) = fetched.iter().find_map(|w| seal::open_page(w, &link.0, &ch.id, link.1, &plane.address(), plane.conv())) {
            let mut cache = cache.lock().unwrap_or_else(|e| e.into_inner());
            if cache.len() >= PAGE_CACHE_MAX {
                cache.clear();
            }
            cache.insert(link.0, (page.clone(), plane.address()));
            return Ok(Some((page, plane.address())));
        }
    }
    Ok(None)
}

/// A reader's place in a seal: the ids it handed out, newest first.
pub(crate) struct SealWalk {
    next: Option<([u8; 32], Epoch)>,
    /// The page being handed out, the address of the plane its ids ride, and how far into it.
    page: Option<(SealPage, PublicKey)>,
    given: usize,
    /// Where the walk starts: pages wholly newer than this (unix ms) are passed over.
    until_ms: u64,
    /// A page it was owed that no relay served or that wouldn't open: what lies past it is
    /// unread, not absent.
    broken: bool,
}

/// What one step of a walk handed out: the ids with the address of the plane each rides, and the
/// oldest date the pages they came from reach, which is where a reader goes on from whether or
/// not the ids are still served.
pub(crate) struct SealStep {
    pub ids: Vec<([u8; 32], PublicKey)>,
    pub bottom_ms: Option<u64>,
}

impl SealWalk {
    pub(crate) fn new(seal: &SealRef, until_ms: u64) -> Self {
        SealWalk { next: seal.page.map(|p| (p, seal.read)), page: None, given: 0, until_ms, broken: false }
    }

    pub(crate) fn done(&self) -> bool {
        self.next.is_none() && self.page.as_ref().is_none_or(|(p, _)| self.given >= p.ids.len())
    }

    pub(crate) fn broken(&self) -> bool {
        self.broken
    }

    /// The next `n` sealed ids, newest first, opening pages only as far as they reach.
    pub(crate) async fn next<T: Transport + ?Sized>(&mut self, transport: &T, community: &CommunityV2, ch: &ChannelV2, n: usize) -> Result<SealStep, String> {
        let mut step = SealStep { ids: Vec::with_capacity(n), bottom_ms: None };
        while step.ids.len() < n {
            if let Some((page, address)) = &self.page {
                if self.given < page.ids.len() {
                    let take = (n - step.ids.len()).min(page.ids.len() - self.given);
                    step.ids.extend(page.ids[self.given..self.given + take].iter().map(|id| (*id, *address)));
                    self.given += take;
                    step.bottom_ms = Some(page.oldest);
                    continue;
                }
            }
            let Some(link) = self.next else { break };
            // A failed read keeps the walk's place for the next.
            let Some((page, address)) = load_page(transport, community, ch, link).await? else {
                self.next = None;
                self.broken = true;
                break;
            };
            self.next = page.next;
            // A page wholly newer than where the walk starts holds nothing it needs.
            self.given = if page.oldest > self.until_ms { page.ids.len() } else { 0 };
            self.page = Some((page, address));
        }
        Ok(step)
    }
}

/// Fetch `ids` from `community`'s relays, a batch at a time, asked as `plane` where this device
/// holds its key.
pub(crate) async fn fetch_ids<T: Transport + ?Sized>(transport: &T, community: &CommunityV2, plane: Option<&GroupKey>, ids: &[[u8; 32]], evidence: crate::community::transport::Evidence) -> Result<Vec<Event>, String> {
    let mut out = Vec::with_capacity(ids.len());
    for batch in ids.chunks(IDS_PER_FETCH) {
        let q = Query { ids: batch.iter().map(hex).collect(), evidence, ..Default::default() };
        out.extend(match plane {
            Some(group) => transport.fetch_plane(group.keys(), &q, &community.relays).await?.events,
            None => transport.fetch(&q, &community.relays).await?,
        });
    }
    Ok(out)
}
