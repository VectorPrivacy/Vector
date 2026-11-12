//! A Locked Channel's sealed history (CORD-03 §1, [`super::super::seal`]): building it once the
//! lock lands, and walking it as a reader scrolls back past the lock.

use super::*;
use crate::community::v2::seal::{self, SealPage};
use crate::community::v2::{kind, writers};
use crate::community::v2::writers::{Sealed, Spell};

/// How many wraps one read of a read address asks for while sealing.
const SEAL_READ_PAGE: usize = 500;

/// Most ids one fetch names: well inside what relays take in an `ids` filter.
const IDS_PER_FETCH: usize = 200;

fn hex(b: &[u8; 32]) -> String {
    crate::simd::hex::bytes_to_hex_32(b)
}

/// Seal what stood at `ch`'s read addresses before its live Locked window: every read address is
/// read whole up to the lock, each wrap judged as it would be on arrival, and the ids that stand go
/// out as pages at the window's writer address, then the ledger entry naming the first. Returns how
/// many ids the seal names.
pub(crate) async fn seal_history<T: Transport + ?Sized>(transport: &T, community: &CommunityV2, ch: &ChannelV2) -> Result<usize, String> {
    use nostr_sdk::prelude::AsyncGetPublicKey;
    let ledger = ch.writers.clone().ok_or("this channel keeps no writer ledger")?;
    if !ledger.locked() {
        return Err("this channel isn't locked".to_string());
    }
    let (window, _) = ledger.current_writer().ok_or("this channel has no live writer epoch")?;
    if ledger.sealed().is_some_and(|s| s.window == window) {
        return Ok(0);
    }
    // What the seal holds ends where this read begins: anything later stays the read address's.
    let before = ledger.writer_start(window).unwrap_or(0).saturating_add(writers::ENTRY_SKEW_MS).min(now_ms());
    let me = me_pk()?;
    // The hash chain from the ledger entry vouches for the pages; a certificate only names the sealer.
    let cert = writing::my_cert_for(community, ch, window, kind::SEAL_PAGE);
    let (_, _, writer) = writing::live_writer(community, ch).ok_or("this device holds no key to this channel's writer address")?;
    let signer = crate::signer::active_signer()?;
    if signer.get_public_key_async().await.map_err(|e| e.to_string())? != me {
        return Err("the active signer isn't this account".to_string());
    }

    let planes = read_planes(community, ch);
    let judge = writing::Judge::of(community, ch);
    let mut ids: Vec<([u8; 32], u64)> = Vec::new();
    for (plane, _) in &planes {
        let mut until = Some(before / 1000 + 1);
        loop {
            let q = Query { evidence: crate::community::transport::Evidence::Full, paged: false, ..plane_page(plane.address().to_hex(), SEAL_READ_PAGE, until) };
            let evs = match plane.group() {
                Some(group) => transport.fetch_plane(group.keys(), &q, &community.relays).await?.events,
                None => transport.fetch(&q, &community.relays).await?,
            };
            for wrap in &evs {
                let Some((event, None)) = open_on_planes(ch, &planes, wrap) else { continue };
                let at = event.opened().at_ms;
                if at >= before {
                    continue;
                }
                if matches!(writing::sort_arrival(judge.as_ref(), event, None), writing::Arrival::Stands(_)) {
                    ids.push((wrap.id.to_bytes(), at));
                }
            }
            if evs.len() < SEAL_READ_PAGE {
                break;
            }
            // Inclusive, so the next read starts at the page's oldest second; one that holds more
            // than a page is stepped past.
            let oldest = evs.iter().map(|e| e.created_at.as_secs()).min().unwrap_or(0);
            until = Some(if Some(oldest) == until { oldest.saturating_sub(1) } else { oldest });
            if oldest == 0 {
                break;
            }
        }
    }
    let mut pages = seal::paginate(ids);
    let count: usize = pages.iter().map(|p| p.ids.len()).sum();
    let (read_secret, read) = community.channel_secret(ch);
    let at_writer = GroupKey::from_parts(writer.keys().clone(), *channel_group_key(&read_secret, &ch.id, read).conv_key());
    // Built oldest first, so each names the one after it.
    let mut next: Option<([u8; 32], [u8; 32])> = None;
    let mut wraps = Vec::with_capacity(pages.len());
    for page in pages.iter_mut().rev() {
        page.next = next;
        let now = now_ms();
        let rumor = seal::page_rumor(me, &ch.id, read, window, cert, page, now);
        let (wrap, _) = stream::seal_and_wrap_signed(&signer, me, &rumor, stream::SealForm::Encrypted, &at_writer, stream::KIND_WRAP, Timestamp::from_secs(now / 1000), &[])
            .await
            .map_err(|e| e.to_string())?;
        next = Some((wrap.id.to_bytes(), seal::content_hash(&page.content())));
        wraps.push(wrap);
    }
    for wrap in &wraps {
        let mut tries = 0;
        while let Err(e) = transport.publish(wrap, &community.relays).await {
            tries += 1;
            if tries >= 3 {
                return Err(e);
            }
            crate::rt::time::sleep(std::time::Duration::from_millis(500 * tries)).await;
        }
    }
    if !crate::db::current_session().is_live() {
        return Err("account changed while sealing a channel".to_string());
    }
    let fresh = crate::db::community::load_community_v2(community.id())?.ok_or("this community is gone")?;
    let ch = fresh.channel(&ch.id).cloned().ok_or("no such channel")?;
    let base = ch.writers.clone().ok_or("this channel keeps no writer ledger")?;
    if base.current_writer().map(|(e, _)| e) != Some(window) {
        return Err("this channel's writer ledger moved; nothing sealed".to_string());
    }
    let (page, hash) = next.unwrap_or(([0; 32], [0; 32]));
    writing::publish_ledger(transport, &fresh, &ch, &base, Spell::Seal { page, hash, read, window, until: before, count: count as u64 }, None, false).await?;
    Ok(count)
}

struct SealPages;
type PageCache = std::sync::Mutex<std::collections::HashMap<[u8; 32], SealPage>>;

/// A page of `sealed`, by the link that names it, from this session's cache or the relays.
/// `None` when no relay serves the page it names.
async fn load_page<T: Transport + ?Sized>(transport: &T, community: &CommunityV2, ch: &ChannelV2, sealed: &Sealed, link: ([u8; 32], [u8; 32])) -> Result<Option<SealPage>, String> {
    let cache = crate::db::current_session().scoped::<SealPages, PageCache>();
    if let Some(p) = cache.lock().unwrap_or_else(|e| e.into_inner()).get(&link.0) {
        return Ok(Some(p.clone()));
    }
    let Some((plane, _)) = read_planes(community, ch).into_iter().find(|(_, e)| *e == sealed.read) else { return Ok(None) };
    let writer = PublicKey::from_slice(&sealed.writer).map_err(|e| e.to_string())?;
    let q = Query { ids: vec![hex(&link.0)], evidence: crate::community::transport::Evidence::Quorum, ..Default::default() };
    let page = transport.fetch(&q, &community.relays).await?.iter().find_map(|w| seal::open_page(w, &writer, sealed.window, plane.conv(), &ch.id, sealed.read, &link.1).map(|(p, _)| p));
    if let Some(p) = &page {
        cache.lock().unwrap_or_else(|e| e.into_inner()).insert(link.0, p.clone());
    }
    Ok(page)
}

/// A reader's place in a seal: the ids it handed out, newest first.
pub(crate) struct SealWalk {
    sealed: Sealed,
    /// Pages opened so far, newest first.
    pages: Vec<SealPage>,
    given: usize,
    /// Where the walk starts: pages wholly newer than this (unix ms) are passed over.
    until_ms: u64,
    done: bool,
}

impl SealWalk {
    pub(crate) fn new(sealed: Sealed, until_ms: u64) -> Self {
        SealWalk { sealed, pages: Vec::new(), given: 0, until_ms, done: sealed.page == [0; 32] }
    }

    pub(crate) fn done(&self) -> bool {
        self.done
    }

    /// The next `n` sealed ids, newest first, opening pages only as far as they reach.
    pub(crate) async fn next<T: Transport + ?Sized>(&mut self, transport: &T, community: &CommunityV2, ch: &ChannelV2, n: usize) -> Result<Vec<[u8; 32]>, String> {
        let mut out = Vec::with_capacity(n);
        while out.len() < n && !self.done {
            let loaded: usize = self.pages.iter().map(|p| p.ids.len()).sum();
            if self.given < loaded {
                let flat = self.pages.iter().flat_map(|p| p.ids.iter()).skip(self.given).take(n - out.len());
                let before = out.len();
                out.extend(flat.copied());
                self.given += out.len() - before;
                continue;
            }
            let link = match self.pages.last() {
                None => (self.sealed.page, self.sealed.hash),
                Some(p) => match p.next {
                    Some(link) => link,
                    None => {
                        self.done = true;
                        break;
                    }
                },
            };
            match load_page(transport, community, ch, &self.sealed, link).await? {
                Some(page) => {
                    // A page wholly newer than where the walk starts holds nothing it needs.
                    if page.oldest > self.until_ms {
                        self.given += page.ids.len();
                    }
                    self.pages.push(page);
                }
                None => {
                    self.done = true;
                    break;
                }
            }
        }
        Ok(out)
    }
}

/// Fetch `ids` from `community`'s relays, a batch at a time.
pub(crate) async fn fetch_ids<T: Transport + ?Sized>(transport: &T, community: &CommunityV2, ids: &[[u8; 32]], evidence: crate::community::transport::Evidence) -> Result<Vec<Event>, String> {
    let mut out = Vec::with_capacity(ids.len());
    for batch in ids.chunks(IDS_PER_FETCH) {
        let q = Query { ids: batch.iter().map(hex).collect(), evidence, ..Default::default() };
        out.extend(transport.fetch(&q, &community.relays).await?);
    }
    Ok(out)
}
