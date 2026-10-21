//! Moving a Community's relays (CORD-02 §6). Every relay the Community will list is first given
//! everything a member needs to fold it, read whole from each old relay on its own, and is read
//! back before anyone is told to move. The move is announced on the old relays first, then the new;
//! message history follows, and a copy cut short resumes on its own.

use super::*;
use nostr_sdk::prelude::RelayUrl;
use std::collections::{BTreeMap, HashSet};

/// How long a member keeps listening on a relay its Community dropped: a member who hasn't
/// folded the move yet still writes there.
pub const RETIRED_RELAY_GRACE_MS: u64 = 7 * 24 * 3600 * 1000;

/// Events published at once while copying.
const COPY_BATCH: usize = 16;

/// Planes read at once.
const READ_BATCH: usize = 8;

/// Probes published at once to see whether a relay throttles: about what a busy minute of a
/// Community sends it.
const BURST: usize = 12;

/// Pauses before each retry of what a relay refused.
const RETRY_BACKOFF_MS: [u64; 3] = [1_000, 3_000, 8_000];

/// The longest pause between batches while a relay is pushing back.
const MAX_PACE_MS: u64 = 8_000;

/// What one second's worth of events is read with when a full page shares a single second.
const WALL_LIMIT: usize = 5_000;

/// One address this device can read a share of the Community at.
struct Plane {
    pk: PublicKey,
    /// The plane's own keys, for relays that only serve a plane to itself.
    keys: Option<Keys>,
    kinds: Vec<u16>,
    /// The current Control Plane: a move never goes ahead without all of it.
    vital: bool,
}

impl Plane {
    fn of(group: &GroupKey) -> Plane {
        Plane { pk: group.pk(), keys: Some(group.keys().clone()), kinds: vec![stream::KIND_WRAP], vital: false }
    }
}

#[derive(Debug, Default, serde::Serialize)]
pub struct RelayMove {
    /// The Community's relays now.
    pub relays: Vec<String>,
    pub added: Vec<String>,
    pub removed: Vec<String>,
    /// Events copied onto the Community's relays.
    pub copied: usize,
    /// The relays the Community left.
    pub previous: Vec<String>,
    /// What the move couldn't fully do, in words.
    pub warnings: Vec<String>,
}

/// One new relay, as the check before a move found it.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RelayCheck {
    pub relay: String,
    /// It stores and serves what a Community needs.
    pub ok: bool,
    /// It pushed back on a modest burst: a Community there can drop posts under load.
    pub rate_limited: bool,
    /// The one network that reaches it ("Tor" for an `.onion`, "I2P" for an `.i2p`): members on
    /// any other can't.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub only_on: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Where the move stands, for the progress ring: `share` of the way through the span
/// `[from, to]` of the whole, under `label`.
fn progress(cid_hex: &str, (from, to): (u32, u32), share: f64, label: &str) {
    let pct = from as f64 + (to - from) as f64 * share.clamp(0.0, 1.0);
    crate::emit_event("community_relay_move", &serde_json::json!({ "community_id": cid_hex, "pct": pct.round() as u32, "label": label }));
}

/// Each phase's span of the progress ring.
const PREFLIGHT: (u32, u32) = (0, 4);
const READING: (u32, u32) = (4, 20);
const COPYING: (u32, u32) = (20, 40);
const CONFIRMING: (u32, u32) = (40, 44);
const ANNOUNCING: (u32, u32) = (44, 48);
const HISTORY: (u32, u32) = (48, 92);
const CATCHING_UP: (u32, u32) = (92, 97);
const LISTING: (u32, u32) = (97, 100);

/// `relays` as a Community lists them, by the app's one relay rule (`wss://`, or plain `ws://` for
/// an `.onion` or `.i2p` host, whose network encrypts the link itself): trimmed, one spelling per
/// relay (an address the list already holds keeps its own), at least one and at most five.
fn clean_relays(held: &[String], relays: &[String]) -> Result<Vec<String>, String> {
    let held_urls: Vec<(RelayUrl, &String)> = held.iter().filter_map(|r| RelayUrl::parse(r).ok().map(|u| (u, r))).collect();
    let mut out: Vec<String> = Vec::new();
    let mut seen: HashSet<RelayUrl> = HashSet::new();
    for raw in relays.iter().map(|r| r.trim()).filter(|r| !r.is_empty()) {
        let raw = match raw.split_once("://") {
            Some((scheme, rest)) => format!("{}://{rest}", scheme.to_ascii_lowercase()),
            None => raw.to_string(),
        };
        let valid = crate::transport::validate_relay_url(&raw).map_err(|e| format!("{raw}: {e}"))?;
        let url = RelayUrl::parse(&valid).map_err(|_| format!("{raw} isn't a relay address"))?;
        if !seen.insert(url.clone()) {
            continue;
        }
        out.push(held_urls.iter().find(|(u, _)| *u == url).map_or(valid, |(_, r)| (*r).clone()));
    }
    if out.is_empty() {
        return Err("a community needs at least one relay".to_string());
    }
    if out.len() > crate::community::MAX_COMMUNITY_RELAYS {
        return Err(format!("a community lists at most {} relays", crate::community::MAX_COMMUNITY_RELAYS));
    }
    Ok(out)
}

fn same_relay(a: &str, b: &str) -> bool {
    match (RelayUrl::parse(a), RelayUrl::parse(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

fn union(first: &[String], then: &[String]) -> Vec<String> {
    let mut out = first.to_vec();
    for r in then {
        if !out.iter().any(|o| same_relay(o, r)) {
            out.push(r.clone());
        }
    }
    out
}

/// Every root this device holds, oldest epoch first, the current one included.
fn held_roots(community: &CommunityV2, cid_hex: &str) -> Vec<(Epoch, [u8; 32])> {
    let mut roots: BTreeMap<u64, [u8; 32]> = crate::db::community::held_epoch_keys(cid_hex, crate::community::SERVER_ROOT_SCOPE_HEX)
        .unwrap_or_default()
        .into_iter()
        .filter(|(e, _)| e.0 < community.root_epoch.0)
        .map(|(e, k)| (e.0, k))
        .collect();
    roots.insert(community.root_epoch.0, community.community_root);
    roots.into_iter().map(|(e, k)| (Epoch(e), k)).collect()
}

/// What a member needs to fold the Community: each epoch's Control Plane and Guestbook, every
/// rotation into a later epoch or a Private Channel's next key, each Private Channel's access
/// plane, and the invite links.
fn structural_planes(community: &CommunityV2, cid_hex: &str) -> Vec<Plane> {
    let id = community.id();
    let mut out = Vec::new();
    let roots = held_roots(community, cid_hex);
    for (epoch, root) in &roots {
        let view = if *epoch == community.root_epoch {
            community.clone()
        } else {
            // The staff key kept for that epoch, so a relay that serves a plane only to itself serves it.
            let mut view = super::audit::epoch_view(community, cid_hex, *epoch, *root);
            view.control_root = crate::db::community::held_epoch_key(cid_hex, crate::community::CONTROL_ROOT_SCOPE, epoch.0).ok().flatten();
            view
        };
        let control = control::ControlPlane::of(&view);
        out.push(Plane { pk: control.pk(), keys: control.signer_keys().cloned(), kinds: vec![stream::KIND_WRAP], vital: *epoch == community.root_epoch });
        out.push(Plane::of(&super::super::derive::guestbook_group_key(root, id, *epoch)));
        out.push(Plane::of(&base_rekey_group_key(root, id, Epoch(epoch.0.saturating_add(1)))));
        for ch in community.channels.iter().filter(|c| c.private) {
            out.push(Plane::of(&super::super::derive::channel_access_group_key(root, &ch.id, *epoch)));
        }
    }
    let rekey_roots = channel_rekey_addressing_roots(community.community_root, cid_hex);
    for ch in community.channels.iter().filter(|c| c.private) {
        let ch_hex = crate::simd::hex::bytes_to_hex_32(&ch.id.0);
        let newest = crate::db::community::held_epoch_keys(cid_hex, &ch_hex).unwrap_or_default().iter().map(|(e, _)| e.0).chain([ch.epoch.0]).max().unwrap_or(0);
        for next in 1..=newest.saturating_add(1) {
            for root in &rekey_roots {
                out.push(Plane::of(&channel_rekey_group_key(root, &ch.id, Epoch(next))));
            }
        }
    }
    for set in &crate::db::community::get_invite_link_sets(cid_hex).unwrap_or_default() {
        if let Ok(pk) = PublicKey::from_hex(&set.creator_hex) {
            out.push(Plane { pk, keys: None, kinds: vec![super::super::kind::INVITE_LIST], vital: false });
        }
        for signer in set.locators.iter().filter_map(|l| PublicKey::from_hex(l).ok()) {
            out.push(Plane { pk: signer, keys: None, kinds: vec![super::super::kind::INVITE_BUNDLE], vital: false });
        }
    }
    out
}

/// Every Channel's posts this device can address: each public Channel under each held root,
/// each Private Channel under each key it holds, and each writer address a ledger names.
fn history_planes(community: &CommunityV2, cid_hex: &str) -> Vec<Plane> {
    let mut out = Vec::new();
    let roots = held_roots(community, cid_hex);
    for ch in &community.channels {
        if ch.private {
            let ch_hex = crate::simd::hex::bytes_to_hex_32(&ch.id.0);
            let mut keys: BTreeMap<u64, [u8; 32]> = crate::db::community::held_epoch_keys(cid_hex, &ch_hex).unwrap_or_default().into_iter().map(|(e, k)| (e.0, k)).collect();
            if let Some(k) = ch.key {
                keys.insert(ch.epoch.0, k);
            }
            for (epoch, key) in keys {
                out.push(Plane::of(&channel_group_key(&key, &ch.id, Epoch(epoch))));
            }
        } else {
            for (epoch, root) in &roots {
                out.push(Plane::of(&channel_group_key(root, &ch.id, *epoch)));
            }
        }
        for entry in ch.writers.iter().flat_map(|l| l.entries.iter()) {
            if let super::super::writers::Spell::Writer { pk, .. } = entry.spell {
                if let Ok(pk) = PublicKey::from_slice(&pk) {
                    out.push(Plane { pk, keys: None, kinds: vec![stream::KIND_WRAP], vital: false });
                }
            }
        }
    }
    out
}

/// One plane as the old relays served it, each read on its own.
#[derive(Default)]
struct PlaneRead {
    events: Vec<Event>,
    /// Relays that answered but couldn't be read to the end.
    partial: Vec<String>,
    /// Relays that couldn't be read at all.
    unreachable: Vec<String>,
}

impl PlaneRead {
    /// Every relay that answered was read to the end, and one did.
    fn whole(&self, asked: usize) -> bool {
        self.partial.is_empty() && self.unreachable.len() < asked
    }
}

async fn fetch_page<T: Transport + ?Sized>(transport: &T, plane: &Plane, query: &Query, relay: &str) -> Result<Vec<Event>, String> {
    let relays = [relay.to_string()];
    match &plane.keys {
        Some(keys) => {
            let page = transport.fetch_plane(keys, query, &relays).await?;
            if !page.is_complete() {
                return Err(format!("{relay} didn't answer"));
            }
            Ok(page.events)
        }
        None => transport.fetch(query, &relays).await,
    }
}

/// Everything `plane` holds on `relay` (since `since`), and whether it was read to the end. Paged
/// on this relay alone: a cursor shared across relays steps past whatever one of them holds and
/// another doesn't. `Err` when the relay couldn't be read at all.
async fn read_plane_on<T: Transport + ?Sized>(transport: &T, plane: &Plane, relay: &str, since: Option<u64>) -> Result<(Vec<Event>, bool), String> {
    let mut out: Vec<Event> = Vec::new();
    let mut seen: HashSet<nostr_sdk::prelude::EventId> = HashSet::new();
    let mut until: Option<u64> = None;
    let mut whole = true;
    for page_n in 0..COMPACT_MAX_PAGES {
        if until.is_some_and(|u| since.is_some_and(|s| u < s)) {
            return Ok((out, whole));
        }
        let query = Query { kinds: plane.kinds.clone(), authors: vec![plane.pk.to_hex()], since, until, limit: Some(FOLLOW_PAGE), evidence: crate::community::transport::Evidence::Full, ..Default::default() };
        let page = match fetch_page(transport, plane, &query, relay).await {
            Ok(page) => page,
            Err(e) if page_n == 0 => return Err(e),
            Err(_) => return Ok((out, false)),
        };
        let full = page.len() >= FOLLOW_PAGE;
        let mut fresh = 0usize;
        let mut oldest: Option<u64> = None;
        for e in page {
            let at = e.created_at.as_secs();
            oldest = Some(oldest.map_or(at, |o| o.min(at)));
            if seen.insert(e.id) {
                fresh += 1;
                out.push(e);
            }
        }
        if !full {
            return Ok((out, whole));
        }
        let Some(oldest) = oldest else { return Ok((out, whole)) };
        if fresh == 0 {
            // A full page sharing one second: read that second on its own with room to spare,
            // then carry on below it.
            let narrow = Query { since: Some(since.map_or(oldest, |s| s.max(oldest))), until: Some(oldest), limit: Some(WALL_LIMIT), ..query.clone() };
            match fetch_page(transport, plane, &narrow, relay).await {
                Ok(second) => {
                    let n = second.len();
                    out.extend(second.into_iter().filter(|e| seen.insert(e.id)));
                    // A relay that capped this read at its own page size may hold more there.
                    if n >= WALL_LIMIT || n <= FOLLOW_PAGE {
                        whole = false;
                    }
                }
                Err(_) => whole = false,
            }
            if oldest == 0 {
                return Ok((out, whole));
            }
            until = Some(oldest - 1);
        } else {
            until = Some(oldest);
        }
    }
    Ok((out, false))
}

/// `plane` from every one of `relays`, each read on its own and merged.
async fn read_plane<T: Transport + ?Sized>(transport: &T, plane: &Plane, relays: &[String], since: Option<u64>) -> PlaneRead {
    let mut reads = Vec::new();
    for relay in relays {
        reads.push(read_plane_on(transport, plane, relay, since));
    }
    let mut out = PlaneRead::default();
    let mut ids = HashSet::new();
    for (relay, read) in relays.iter().zip(futures_util::future::join_all(reads).await) {
        match read {
            Ok((events, whole)) => {
                if !whole {
                    out.partial.push(relay.clone());
                }
                out.events.extend(events.into_iter().filter(|e| ids.insert(e.id)));
            }
            Err(_) => out.unreachable.push(relay.clone()),
        }
    }
    out
}

/// Only the newest version of each replaceable or addressable event: a relay keeps nothing older,
/// and an older copy arriving after a newer one can read as a refusal.
fn newest_versions(mut events: Vec<Event>) -> Vec<Event> {
    events.sort_by(|a, b| b.created_at.cmp(&a.created_at).then_with(|| a.id.cmp(&b.id)));
    let mut coords: HashSet<(u16, PublicKey, String)> = HashSet::new();
    events.retain(|e| {
        let k = e.kind.as_u16();
        let replaceable = k == 0 || k == 3 || (10_000..20_000).contains(&k);
        let addressable = (30_000..40_000).contains(&k);
        if !replaceable && !addressable {
            return true;
        }
        let d = if addressable { e.tags.identifier().unwrap_or_default().to_string() } else { String::new() };
        coords.insert((k, e.pubkey, d))
    });
    events.reverse();
    events
}

/// A relay's reason, without the transport's preamble.
fn reason(why: &str) -> String {
    why.rsplit_once("accepted the event: ").map_or(why, |(_, r)| r).to_ascii_lowercase()
}

/// Whether a relay's refusal reads as "too fast" (NIP-01's `rate-limited:` and its paraphrases).
fn is_rate_limit(why: &str) -> bool {
    let why = reason(why);
    ["rate-limit", "rate limit", "ratelimit", "slow down", "too many", "too fast"].iter().any(|m| why.contains(m))
}

/// Whether a relay already holds the event: some say so with a refusal.
fn is_duplicate(why: &str) -> bool {
    reason(why).starts_with("duplicate")
}

/// Whether a refusal is the relay's policy (NIP-01's machine-readable prefixes), which no retry changes.
fn is_policy(why: &str) -> bool {
    let why = reason(why);
    ["blocked:", "invalid:", "restricted:", "pow:", "auth-required:", "mute:"].iter().any(|p| why.starts_with(p))
}

/// Publish `event` to `relay`. Anything but the relay's policy is tried again after each backoff,
/// so one blip doesn't sink a long copy. Whether it was rate-limited on the way, or the relay's
/// last word if it never took it.
async fn publish_patiently<T: Transport + ?Sized>(transport: &T, event: &Event, relay: &str) -> Result<bool, String> {
    let relays = [relay.to_string()];
    let mut limited = false;
    let mut last = match transport.publish(event, &relays).await {
        Ok(()) => return Ok(false),
        Err(e) if is_duplicate(&e) => return Ok(false),
        Err(e) if is_policy(&e) => return Err(e),
        Err(e) => e,
    };
    for wait in RETRY_BACKOFF_MS {
        limited |= is_rate_limit(&last);
        crate::rt::time::sleep(std::time::Duration::from_millis(wait)).await;
        match transport.publish(event, &relays).await {
            Ok(()) => return Ok(limited),
            Err(e) if is_duplicate(&e) => return Ok(limited),
            Err(e) if is_policy(&e) => return Err(e),
            Err(e) => last = e,
        }
    }
    Err(last)
}

/// Publish `events` to each of `relays` on its own, so every relay's acceptance is known. A batch
/// a relay pushed back on slows the next, and what it refused is retried patiently at the end.
/// Returns each relay that still refused something, with its last word.
async fn copy_events<T: Transport + ?Sized>(transport: &T, events: &[Event], relays: &[String], mut done: impl FnMut(usize)) -> Result<Vec<(String, String)>, String> {
    let mut refused: Vec<(&String, &Event)> = Vec::new();
    let mut pace = 0u64;
    for batch in events.chunks(COPY_BATCH) {
        if !crate::db::session_is_live() {
            return Err("the account changed during the move".to_string());
        }
        let mut sends = Vec::new();
        for relay in relays {
            for event in batch {
                sends.push(async move { (relay, event, transport.publish(event, std::slice::from_ref(relay)).await) });
            }
        }
        let mut pushed_back = false;
        for (relay, event, sent) in futures_util::future::join_all(sends).await {
            match sent {
                Ok(()) => {}
                Err(e) if is_duplicate(&e) => {}
                Err(e) => {
                    pushed_back |= is_rate_limit(&e);
                    refused.push((relay, event));
                }
            }
        }
        done(batch.len());
        pace = if pushed_back { (pace * 2).clamp(500, MAX_PACE_MS) } else { pace / 2 };
        if pace > 0 {
            crate::rt::time::sleep(std::time::Duration::from_millis(pace)).await;
        }
    }
    let mut still: Vec<(String, String)> = Vec::new();
    for (relay, event) in refused {
        if still.iter().any(|(r, _)| r == relay) {
            continue;
        }
        if let Err(e) = publish_patiently(transport, event, relay).await {
            still.push((relay.clone(), e));
        }
    }
    Ok(still)
}

/// Whether `relay` stores and serves what a Community needs: an encrypted wrap (kind 1059) read
/// back by the key that wrote it, an ephemeral wrap (kind 21059) accepted, and an invite bundle
/// (kind 33301) read back. Then a burst, to learn whether it throttles. Returns whether it pushed
/// back anywhere. The probes look like a Community's own, and are deleted after.
async fn preflight<T: Transport + ?Sized>(transport: &T, relay: &str) -> Result<bool, String> {
    use nostr_sdk::prelude::{EventBuilder, EventDeletionRequest, FinalizeEvent, Kind, Tag};
    let probe = Keys::generate();
    let relays = [relay.to_string()];
    // A wrap names a throwaway recipient, never its own author.
    let to = || Tag::public_key(Keys::generate().public_key());
    let sign = |kind: u16, tags: Vec<Tag>| EventBuilder::new(Kind::Custom(kind), "concord relay check").tags(tags).finalize(&probe).map_err(|e| e.to_string());
    let wrap = sign(stream::KIND_WRAP, vec![to()])?;
    let ephemeral = sign(stream::KIND_WRAP_EPHEMERAL, vec![to()])?;
    let bundle = sign(super::super::kind::INVITE_BUNDLE, vec![Tag::identifier("")])?;
    let refused = |what: &str, why: String| format!("{relay} doesn't accept {what}, which a Concord community needs, so nothing was changed. It said: {}", reason(&why));
    let mut pushed_back = publish_patiently(transport, &wrap, relay).await.map_err(|e| refused("encrypted messages (kind 1059)", e))?;
    pushed_back |= publish_patiently(transport, &ephemeral, relay).await.map_err(|e| refused("live events like typing and calls (kind 21059)", e))?;
    pushed_back |= publish_patiently(transport, &bundle, relay).await.map_err(|e| refused("invite links (kind 33301)", e))?;
    let query = Query {
        kinds: vec![stream::KIND_WRAP, super::super::kind::INVITE_BUNDLE],
        authors: vec![probe.public_key().to_hex()],
        evidence: crate::community::transport::Evidence::Full,
        ..Default::default()
    };
    let held: HashSet<_> = transport.fetch_plane(&probe, &query, &relays).await.map(|f| f.events).unwrap_or_default().into_iter().map(|e| e.id).collect();
    // A relay that says it saved an event it then doesn't serve loses a Community's posts
    // silently; under its own rate limit is when that bites, so the reason names it.
    let lost = |what: &str| {
        let load = if pushed_back { " It was rate-limiting the check, and a relay that drops what it accepts under load would lose this community's posts when it's busy." } else { "" };
        format!("{relay} said it saved a test {what} but doesn't serve it back, so nothing was changed.{load}")
    };
    if !held.contains(&wrap.id) {
        return Err(lost("message"));
    }
    if !held.contains(&bundle.id) {
        return Err(lost("invite link"));
    }
    let mut burst = Vec::with_capacity(BURST);
    for _ in 0..BURST {
        burst.push(sign(stream::KIND_WRAP, vec![to()])?);
    }
    let mut sends = Vec::new();
    for event in &burst {
        sends.push(transport.publish(event, &relays));
    }
    pushed_back |= futures_util::future::join_all(sends).await.iter().any(|sent| sent.as_ref().is_err_and(|e| !is_duplicate(e)));
    let probes = burst.iter().map(|e| e.id).chain([wrap.id, bundle.id]);
    if let Ok(cleanup) = EventDeletionRequest::new().ids(probes).finalize(&probe) {
        let _ = transport.publish(&cleanup, &relays).await;
    }
    Ok(pushed_back)
}

/// The network only it is reached on, for a relay whose host is native to one.
fn native_network(relay: &str) -> Option<crate::transport::Kind> {
    let host = url::Url::parse(relay).ok()?.host_str()?.to_ascii_lowercase();
    [crate::transport::Kind::Tor, crate::transport::Kind::I2p].into_iter().find(|k| k.native_suffixes().iter().any(|s| host.ends_with(s)))
}

/// Whether this device can reach `relay` to check it: a network-only relay needs this account on
/// that network.
fn reachable_here(relay: &str) -> Result<(), String> {
    match native_network(relay) {
        Some(kind) if crate::transport::preference() != Some(kind) => Err(format!(
            "{relay} can only be reached over {}, and this device isn't using {} right now, so it couldn't be checked and nothing was changed.",
            kind.label(),
            kind.label()
        )),
        _ => Ok(()),
    }
}

/// Check each relay `relays` would add to `community` before anything moves: whether it holds what
/// a Community needs, whether it throttles, and whether only one network reaches it.
pub async fn check_relays<T: Transport + ?Sized>(transport: &T, community: &CommunityV2, relays: &[String]) -> Result<Vec<RelayCheck>, String> {
    let cid_hex = crate::simd::hex::bytes_to_hex_32(&community.id().0);
    let new = clean_relays(&community.relays, relays)?;
    let added: Vec<String> = new.into_iter().filter(|r| !community.relays.iter().any(|o| same_relay(o, r))).collect();
    progress(&cid_hex, PREFLIGHT, 0.0, &if added.len() == 1 { format!("Checking {}", added[0]) } else { format!("Checking {} relays", added.len()) });
    let mut checks = Vec::new();
    for relay in &added {
        checks.push(async move {
            reachable_here(relay)?;
            preflight(transport, relay).await
        });
    }
    Ok(added
        .iter()
        .zip(futures_util::future::join_all(checks).await)
        .map(|(relay, checked)| {
            let only_on = native_network(relay).map(|k| k.label());
            match checked {
                Ok(rate_limited) => RelayCheck { relay: relay.clone(), ok: true, rate_limited, only_on, reason: None },
                Err(reason) => RelayCheck { relay: relay.clone(), ok: false, rate_limited: false, only_on, reason: Some(reason) },
            }
        })
        .collect())
}

/// Remember the relays a fold or an edit just dropped, so this device keeps listening on them
/// for [`RETIRED_RELAY_GRACE_MS`]; a relay listed again stops being retired.
pub(crate) fn note_retired_relays(cid_hex: &str, before: &[String], after: &[String]) {
    let now = now_ms();
    let mut retired = retired_relay_map(cid_hex);
    retired.retain(|r, until| *until > now && !after.iter().any(|a| same_relay(a, r)));
    for r in before.iter().filter(|r| !after.iter().any(|a| same_relay(a, r))) {
        retired.insert(r.clone(), now + RETIRED_RELAY_GRACE_MS);
    }
    if let Ok(v) = serde_json::to_string(&retired) {
        let _ = crate::db::settings::set_sql_setting(format!("v2_retired_relays:{cid_hex}"), v);
    }
}

fn retired_relay_map(cid_hex: &str) -> BTreeMap<String, u64> {
    crate::db::settings::get_sql_setting(format!("v2_retired_relays:{cid_hex}"))
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_str(&v).ok())
        .unwrap_or_default()
}

/// The relays `cid_hex` dropped that this device still listens on.
pub(crate) fn retired_relays(cid_hex: &str) -> Vec<String> {
    let now = now_ms();
    retired_relay_map(cid_hex).into_iter().filter(|(_, until)| *until > now).map(|(r, _)| r).collect()
}

/// A fold or an edit moved `before`'s relays to `after`'s: listen on the dropped ones a while
/// longer, re-post this device's own invite links where joiners holding either list will look,
/// and carry the new list into this member's Community List.
pub(crate) fn relays_moved(before: &CommunityV2, after: &CommunityV2) {
    let cid_hex = crate::simd::hex::bytes_to_hex_32(&before.id().0);
    note_retired_relays(&cid_hex, &before.relays, &after.relays);
    let mut both = after.clone();
    both.relays = union(&after.relays, &before.relays);
    let Some(client) = crate::state::nostr_client() else { return };
    crate::db::spawn_bound(async move {
        super::super::realtime::refresh_subscription(&client).await;
        let transport = crate::community::transport::LiveTransport::with_timeout(std::time::Duration::from_secs(12));
        let _ = refresh_public_links(&transport, &both).await;
    });
    republish_community_list_durable(None);
}

/// A move's copy still owed: what to copy from, onto which relays, and since when.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct CopyOwed {
    from: Vec<String>,
    to: Vec<String>,
    since: u64,
}

fn copy_owed_key(cid_hex: &str) -> String {
    format!("v2_relay_copy:{cid_hex}")
}

fn copy_owed(cid_hex: &str) -> Option<CopyOwed> {
    crate::db::settings::get_sql_setting(copy_owed_key(cid_hex)).ok().flatten().and_then(|v| serde_json::from_str(&v).ok())
}

/// The communities whose copy is running in this session, so a resume never runs beside it.
struct CopyRunning;
fn copy_running() -> std::sync::Arc<std::sync::Mutex<HashSet<String>>> {
    crate::db::current_session().scoped::<CopyRunning, _>()
}

/// Copy every post this device can address from `owed.from` onto `owed.to`, then anything posted
/// to the old relays since the move began. The marker clears only once both are done, so a copy
/// cut short runs again. Returns what it couldn't do.
async fn copy_history<T: Transport + ?Sized>(transport: &T, community: &CommunityV2, owed: &CopyOwed, report: bool) -> Vec<String> {
    let cid_hex = crate::simd::hex::bytes_to_hex_32(&community.id().0);
    {
        let running = copy_running();
        let mut held = running.lock().unwrap_or_else(|e| e.into_inner());
        if !held.insert(cid_hex.clone()) {
            return Vec::new();
        }
    }
    let mut warnings = Vec::new();
    let tell = |span: (u32, u32), share: f64, label: &str| {
        if report {
            progress(&cid_hex, span, share, label);
        }
    };
    let planes = history_planes(community, &cid_hex);
    let (mut copied, mut partial) = (0usize, 0usize);
    let mut ok = true;
    for (i, plane) in planes.iter().enumerate() {
        if !crate::db::session_is_live() {
            ok = false;
            break;
        }
        // One plane at a time, so a large Community never sits in memory whole.
        let read = read_plane(transport, plane, &owed.from, None).await;
        if !read.whole(owed.from.len()) {
            partial += 1;
        }
        let events = newest_versions(read.events);
        match copy_events(transport, &events, &owed.to, |n| {
            copied += n;
            tell(HISTORY, (i as f64 + 0.5) / planes.len().max(1) as f64, &format!("Copying message history ({copied} so far)"));
        })
        .await
        {
            Ok(refused) => {
                for (relay, why) in refused {
                    warnings.push(format!("{relay} refused some message history ({})", reason(&why)));
                }
            }
            Err(_) => {
                ok = false;
                break;
            }
        }
        tell(HISTORY, (i + 1) as f64 / planes.len().max(1) as f64, &format!("Copying message history ({copied} so far)"));
    }
    if ok {
        tell(CATCHING_UP, 0.0, "Catching up on anything posted meanwhile");
        let mut planes = structural_planes(community, &cid_hex);
        planes.extend(history_planes(community, &cid_hex));
        let since = Some(owed.since.saturating_sub(120));
        for (i, batch) in planes.chunks(READ_BATCH).enumerate() {
            let mut reads = Vec::new();
            for plane in batch {
                reads.push(read_plane(transport, plane, &owed.from, since));
            }
            let late: Vec<Event> = futures_util::future::join_all(reads).await.into_iter().flat_map(|r| r.events).collect();
            if copy_events(transport, &newest_versions(late), &owed.to, |_| {}).await.is_err() {
                ok = false;
                break;
            }
            tell(CATCHING_UP, (i + 1) as f64 * READ_BATCH as f64 / planes.len().max(1) as f64, "Catching up on anything posted meanwhile");
        }
    }
    if ok {
        let _ = crate::db::settings::remove_setting(&copy_owed_key(&cid_hex));
    }
    if partial > 0 {
        warnings.push(format!("{partial} channel histories couldn't be read whole from the old relays, so some old posts may be missing on the new ones"));
    }
    copy_running().lock().unwrap_or_else(|e| e.into_inner()).remove(&cid_hex);
    warnings
}

/// Finish a move's copy a restart cut short, in the background.
pub(crate) fn resume_relay_copy(community: &CommunityV2) {
    let cid_hex = crate::simd::hex::bytes_to_hex_32(&community.id().0);
    let Some(owed) = copy_owed(&cid_hex) else { return };
    if crate::state::nostr_client().is_none() || copy_running().lock().unwrap_or_else(|e| e.into_inner()).contains(&cid_hex) {
        return;
    }
    let community = community.clone();
    crate::db::spawn_bound(async move {
        let transport = crate::community::transport::LiveTransport::with_timeout(std::time::Duration::from_secs(20));
        for warning in copy_history(&transport, &community, &owed, false).await {
            crate::log_warn!("[relays] {warning}");
        }
    });
}

/// Move `community` to `relays`. Every relay it will list is given everything a member needs to
/// fold the Community, read from each old relay on its own, and is read back before anyone is
/// told to move. The move is announced on the old relays first, so it never reaches only the new
/// ones; then message history follows.
pub async fn move_relays<T: Transport + ?Sized>(transport: &T, community: &CommunityV2, relays: &[String]) -> Result<RelayMove, String> {
    crate::db::scoped(async move {
        ensure_folded_permission(community, &me_pk()?, crate::community::roles::Permissions::MANAGE_METADATA, "moving the community's relays")?;
        if !control::ControlPlane::of(community).can_write() {
            return Err("only community staff hold this community's write key; ask an admin to re-send your promotion".to_string());
        }
        let cid_hex = crate::simd::hex::bytes_to_hex_32(&community.id().0);
        let old = community.relays.clone();
        let new = clean_relays(&old, relays)?;
        let added: Vec<String> = new.iter().filter(|r| !old.iter().any(|o| same_relay(o, r))).cloned().collect();
        let removed: Vec<String> = old.iter().filter(|r| !new.iter().any(|n| same_relay(n, r))).cloned().collect();
        if new == old {
            return Ok(RelayMove { relays: old.clone(), previous: old, ..Default::default() });
        }
        let started = now_ms() / 1000;
        let mut warnings = Vec::new();
        let mut copied = 0usize;
        let moving = !added.is_empty() || !removed.is_empty();

        if !added.is_empty() {
            progress(&cid_hex, PREFLIGHT, 0.0, &format!("Checking {}", added.join(", ")));
            let mut checks = Vec::new();
            for relay in &added {
                checks.push(async move {
                    reachable_here(relay)?;
                    preflight(transport, relay).await
                });
            }
            for checked in futures_util::future::join_all(checks).await {
                checked?;
            }
        }

        if moving {
            // Read everything from every old relay, each on its own, before writing anything.
            let planes = structural_planes(community, &cid_hex);
            let mut events: Vec<Event> = Vec::new();
            let mut ids = HashSet::new();
            let mut skipped = 0usize;
            let label = "Reading the community from its relays";
            for (n, batch) in planes.chunks(READ_BATCH).enumerate() {
                progress(&cid_hex, READING, (n * READ_BATCH) as f64 / planes.len().max(1) as f64, label);
                let mut reads = Vec::new();
                for plane in batch {
                    reads.push(read_plane(transport, plane, &old, None));
                }
                for (plane, read) in batch.iter().zip(futures_util::future::join_all(reads).await) {
                    if !read.whole(old.len()) {
                        if plane.vital {
                            let why = match (read.partial.first(), read.unreachable.len() == old.len()) {
                                (_, true) => "none of its relays could be read".to_string(),
                                (Some(relay), _) => format!("{relay} couldn't be read to the end"),
                                (None, _) => "it couldn't be read whole".to_string(),
                            };
                            return Err(format!("This community's rules couldn't be read in full ({why}), so nothing was changed."));
                        }
                        skipped += 1;
                    }
                    events.extend(read.events.into_iter().filter(|e| ids.insert(e.id)));
                }
                if !crate::db::session_is_live() {
                    return Err("the account changed during the move".to_string());
                }
            }
            if skipped > 0 {
                warnings.push(format!("{skipped} parts of the community's history couldn't be read whole from its old relays"));
            }
            // Oldest first, so a relay that stops part-way holds a prefix a reader can fold.
            let mut events = newest_versions(events);
            events.sort_by_key(|e| e.created_at);

            // Onto every relay the Community will list, kept ones included: one of the old relays
            // may hold what another missed.
            let total = events.len();
            let mut done = 0usize;
            let refused = copy_events(transport, &events, &new, |n| {
                done += n;
                progress(&cid_hex, COPYING, done as f64 / total.max(1) as f64, "Copying the community's structure");
            })
            .await?;
            if let Some((relay, why)) = refused.first() {
                return Err(format!("{relay} didn't accept all of the community's events, so nothing was changed. It said: {}", reason(why)));
            }

            // Read the current Control Plane back from each: an accepted write a relay then drops
            // would land members on a Community missing its rules.
            let control = control::ControlPlane::of(community);
            let plane = Plane { pk: control.pk(), keys: control.signer_keys().cloned(), kinds: vec![stream::KIND_WRAP], vital: true };
            let want: HashSet<_> = events.iter().filter(|e| e.pubkey == control.pk()).map(|e| e.id).collect();
            for (i, relay) in new.iter().enumerate() {
                progress(&cid_hex, CONFIRMING, i as f64 / new.len() as f64, &format!("Confirming {relay} holds it"));
                let held: HashSet<_> = read_plane_on(transport, &plane, relay, None).await.map(|r| r.0).unwrap_or_default().into_iter().map(|e| e.id).collect();
                if !want.is_subset(&held) {
                    return Err(format!("{relay} accepted the community's events but doesn't serve them all back, so nothing was changed."));
                }
            }
            copied = total;
        }

        // The announcement, built on what the Community holds now: a rename that landed while the
        // copy ran must not be undone by it.
        progress(&cid_hex, ANNOUNCING, 0.0, "Announcing the move");
        {
            let lock = super::super::realtime::follow_lock(community.id());
            let _guard = lock.lock().await;
            if !crate::db::session_is_live() {
                return Err("the account changed during the move".to_string());
            }
            let mut held = crate::db::community::load_community_v2(community.id())?.ok_or("this community is no longer held")?;
            if held.relays != old {
                return Err("the community's relays changed while this move ran, so nothing was announced; try again".to_string());
            }
            let mut meta = held.metadata();
            meta.relays = new.clone();
            control::validate_community_metadata(&meta).map_err(|e| e.to_string())?;
            let content = serde_json::to_string(&meta).map_err(|e| e.to_string())?;
            // On the old relays first, and at least one must take it: members who only read the
            // old set learn of the move from nowhere else.
            let mut on_old = held.clone();
            on_old.relays = old.clone();
            let wrap = publish_control_edition_wrap_at(transport, &on_old, vsk::COMMUNITY_METADATA, &community.id().0, &content, now_ms() / 1000)
                .await
                .map_err(|e| format!("None of the current relays took the announcement, so members couldn't learn of the move and nothing was changed ({})", reason(&e)))?;
            let everywhere = union(&old, &new);
            let mut sends = Vec::new();
            for relay in &everywhere {
                sends.push(transport.publish_durable(&wrap, std::slice::from_ref(relay)));
            }
            for (relay, sent) in everywhere.iter().zip(futures_util::future::join_all(sends).await) {
                if let Err(e) = sent {
                    warnings.push(format!("{relay} didn't take the announcement ({})", reason(&e)));
                }
            }
            let before = held.clone();
            if apply_community_metadata(&mut held, meta) {
                crate::db::community::save_community_v2(&held)?;
            }
            relays_moved(&before, &held);
            if moving {
                let owed = CopyOwed { from: old.clone(), to: new.clone(), since: started };
                if let Ok(v) = serde_json::to_string(&owed) {
                    let _ = crate::db::settings::set_sql_setting(copy_owed_key(&cid_hex), v);
                }
            }
        }

        if moving {
            let owed = CopyOwed { from: old.clone(), to: new.clone(), since: started };
            warnings.extend(copy_history(transport, community, &owed, true).await);
        }
        progress(&cid_hex, LISTING, 0.0, "Updating your community list");
        if !matches!(republish_community_list(transport, None).await, Ok(true)) {
            republish_community_list_durable(None);
        }
        progress(&cid_hex, LISTING, 1.0, "Done");
        Ok(RelayMove { relays: new, added, removed, copied, previous: old, warnings })
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_relay_list_is_cleaned_and_bounded() {
        let held = vec!["wss://jskitty.com/nostr".to_string()];
        let out = clean_relays(&held, &[" wss://jskitty.com/nostr ".into(), "wss://relay.example.com/".into(), "wss://relay.example.com".into()]).unwrap();
        assert_eq!(out, vec!["wss://jskitty.com/nostr".to_string(), "wss://relay.example.com".to_string()], "one spelling per relay, a held one keeps its own");
        assert!(clean_relays(&held, &[]).is_err(), "a community keeps at least one relay");
        assert!(clean_relays(&held, &["https://relay.example.com".into()]).is_err());
        assert!(clean_relays(&held, &["ws://relay.example.com".into()]).is_err(), "plain text only to a network that encrypts the link");
        let onion = format!("ws://{}.onion", "a".repeat(56));
        assert!(clean_relays(&held, std::slice::from_ref(&onion)).is_ok(), "an onion relay over plain ws");
        assert!(clean_relays(&held, &["ws://relay.example.i2p".into()]).is_ok(), "and an I2P one");
        assert_eq!(native_network(&onion), Some(crate::transport::Kind::Tor));
        assert_eq!(native_network("wss://relay.example.i2p"), Some(crate::transport::Kind::I2p));
        assert_eq!(native_network("wss://jskitty.com/nostr"), None);
        assert!(clean_relays(&held, &["WSS://Relay.Example.com".into()]).is_ok(), "the scheme reads in any case");
        let six: Vec<String> = (0..6).map(|i| format!("wss://r{i}.example.com")).collect();
        assert!(clean_relays(&held, &six).is_err());
    }

    #[test]
    fn a_refusal_is_read_for_what_it_is() {
        assert!(is_rate_limit("no relay accepted the event: rate-limited: slow down matey"));
        assert!(is_rate_limit("Too many requests"));
        assert!(!is_rate_limit("restricted: sign up at https://nostr.wine to write events to this relay"));
        assert!(is_policy("no relay accepted the event: restricted: sign up at https://nostr.wine"));
        assert!(is_policy("blocked: not on the allow list"));
        assert!(!is_policy("timed out"), "a blip is tried again");
        assert!(is_duplicate("no relay accepted the event: duplicate: already have this event"));
    }

    #[test]
    fn only_the_newest_version_of_an_addressable_event_is_copied() {
        use nostr_sdk::prelude::{EventBuilder, FinalizeEvent, Kind, Tag, Timestamp};
        let keys = Keys::generate();
        let at = |s: u64, d: &str| EventBuilder::new(Kind::Custom(super::super::super::kind::INVITE_BUNDLE), "").tags([Tag::identifier(d)]).custom_created_at(Timestamp::from_secs(s)).finalize(&keys).unwrap();
        let wrap = EventBuilder::new(Kind::Custom(stream::KIND_WRAP), "").custom_created_at(Timestamp::from_secs(1)).finalize(&keys).unwrap();
        let (old, new, other) = (at(5, ""), at(9, ""), at(1, "x"));
        let kept: HashSet<_> = newest_versions(vec![old.clone(), wrap.clone(), new.clone(), other.clone()]).into_iter().map(|e| e.id).collect();
        assert_eq!(kept, [wrap.id, new.id, other.id].into_iter().collect());
    }

    #[test]
    fn the_union_keeps_the_new_list_first() {
        let new = vec!["wss://b.example".to_string(), "wss://c.example".to_string()];
        let old = vec!["wss://a.example".to_string(), "wss://b.example/".to_string()];
        assert_eq!(union(&new, &old), vec!["wss://b.example".to_string(), "wss://c.example".to_string(), "wss://a.example".to_string()]);
    }

    use crate::community::transport::memory::MemoryRelay;
    use nostr_sdk::prelude::{EventBuilder, FinalizeEvent, Kind, Timestamp};

    /// Applies `limit` per relay, as real relays do, rather than across the union; `cap` is a
    /// relay's own ceiling on any one read.
    struct PerRelay<'a>(&'a MemoryRelay, Option<usize>);
    #[async_trait::async_trait]
    impl Transport for PerRelay<'_> {
        async fn publish(&self, e: &Event, r: &[String]) -> Result<(), String> {
            self.0.publish(e, r).await
        }
        async fn fetch(&self, query: &Query, relays: &[String]) -> Result<Vec<Event>, String> {
            let mut query = query.clone();
            if let Some(cap) = self.1 {
                query.limit = Some(query.limit.map_or(cap, |l| l.min(cap)));
            }
            let (mut out, mut ids) = (Vec::new(), HashSet::new());
            for relay in relays {
                out.extend(self.0.fetch(&query, std::slice::from_ref(relay)).await?.into_iter().filter(|e| ids.insert(e.id)));
            }
            Ok(out)
        }
        async fn fetch_plane(&self, _plane: &Keys, query: &Query, relays: &[String]) -> Result<crate::community::transport::PlaneFetch, String> {
            Ok(crate::community::transport::PlaneFetch::whole(self.fetch(query, relays).await?))
        }
        async fn publish_durable(&self, e: &Event, r: &[String]) -> Result<(), String> {
            self.0.publish_durable(e, r).await
        }
    }

    fn wraps(author: &Keys, at: impl Iterator<Item = u64>) -> Vec<Event> {
        at.enumerate()
            .map(|(i, s)| EventBuilder::new(Kind::Custom(stream::KIND_WRAP), i.to_string()).custom_created_at(Timestamp::from_secs(s)).finalize(author).unwrap())
            .collect()
    }

    #[tokio::test]
    async fn relays_holding_different_events_are_each_read_whole() {
        let relay = MemoryRelay::new();
        let author = Keys::generate();
        let events = wraps(&author, 1..=1200);
        let (a, b) = ("wss://a".to_string(), "wss://b".to_string());
        for e in &events {
            relay.inject(e, std::slice::from_ref(&a));
        }
        // The second relay missed the newest 600: its newest page (101..=600) reaches below the
        // first's (701..=1200), and a cursor shared between them skips 601..=700.
        for e in &events[..600] {
            relay.inject(e, std::slice::from_ref(&b));
        }
        let plane = Plane { pk: author.public_key(), keys: None, kinds: vec![stream::KIND_WRAP], vital: true };
        let read = read_plane(&PerRelay(&relay, None), &plane, &[a, b], None).await;
        assert_eq!(read.events.len(), 1200, "a cursor shared across relays steps past what only one of them holds");
        assert!(read.whole(2));
    }

    #[tokio::test]
    async fn a_page_within_one_second_is_read_past() {
        let relay = MemoryRelay::new();
        let author = Keys::generate();
        let mut events = wraps(&author, std::iter::repeat_n(50, 600));
        events.extend(wraps(&author, 1..=40));
        let r = "wss://a".to_string();
        for e in &events {
            relay.inject(e, std::slice::from_ref(&r));
        }
        let plane = Plane { pk: author.public_key(), keys: None, kinds: vec![stream::KIND_WRAP], vital: true };
        let read = read_plane(&PerRelay(&relay, None), &plane, std::slice::from_ref(&r), None).await;
        assert_eq!(read.events.len(), 640, "600 wraps in one second, then everything older");
        assert!(read.whole(1));
        // A relay that never serves more than a page at once can't show that second whole.
        let capped = read_plane(&PerRelay(&relay, Some(FOLLOW_PAGE)), &plane, std::slice::from_ref(&r), None).await;
        assert!(!capped.whole(1), "and says so rather than reading short");
        assert!(capped.events.len() >= 540, "what lies past the second is still read");
    }

    /// `VECTOR_RELAYS="wss://a wss://b" cargo test -p vector-core live_preflight -- --ignored --nocapture`
    #[tokio::test]
    #[ignore = "hits real relays over the network"]
    async fn live_preflight() {
        use crate::ClientRelayExt;
        let relays: Vec<String> = std::env::var("VECTOR_RELAYS").unwrap_or_else(|_| "wss://nos.lol".to_string()).split_whitespace().map(str::to_string).collect();
        let client = crate::nostr_client_builder().build();
        for relay in &relays {
            client.add_managed_relay(relay.as_str()).await.ok();
        }
        client.connect().await;
        crate::state::set_nostr_client(client);
        let transport = crate::community::transport::LiveTransport::with_timeout(std::time::Duration::from_secs(15));
        for relay in &relays {
            println!("{relay}: {:?}", preflight(&transport, relay).await);
        }
    }
}
