//! Nostr content referenced from a chat message (a post, an article, a video),
//! fetched from relays and shaped for an inline card.
//!
//! A reference is a `note1`/`nevent1`/`naddr1` entity, bare, as a `nostr:` URI, or
//! as the last path segment of a web link. The web site itself is never contacted:
//! the event comes from relays and is verified before it is shown.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};

use nostr_sdk::prelude::*;
use serde::Serialize;

use crate::ClientRelayExt;

pub const KIND_POST: u16 = 1;
pub const KIND_ARTICLE: u16 = 30023;
pub const KIND_VIDEO: u16 = 21;
pub const KIND_SHORT: u16 = 22;
pub const KIND_VIDEO_ADDRESSABLE: u16 = 34235;
pub const KIND_SHORT_ADDRESSABLE: u16 = 34236;
pub const KIND_REPOST: u16 = 6;
pub const KIND_GENERIC_REPOST: u16 = 16;

/// Asked when neither our relays, the reference's hints nor the author's outbox
/// have it: the large general-purpose relays most public content reaches.
const PUBLIC_RELAYS: &[&str] = &["wss://relay.damus.io", "wss://relay.primal.net", "wss://nos.lol"];
const MAX_REMOTE_RELAYS: usize = 8;
const POOL_TIMEOUT_SECS: u64 = 6;
const REMOTE_TIMEOUT_SECS: u64 = 8;
/// A miss is retried after this long, so a relay blip doesn't blank a card all session.
const MISS_TTL_SECS: u64 = 30;
const CACHE_CAP: usize = 512;
/// Longest a second ask waits on a first still fetching before fetching itself: past every
/// relay budget, so it only fires when the first was lost.
const WAIT_DEADLINE_SECS: u64 = 150;
/// A post's files beyond these are never shown, so they are never worked on either.
const MAX_MEDIA: usize = 16;
const MAX_CONTENT_URLS: usize = 32;
const MAX_FALLBACKS: usize = 3;
/// Bounds what a card or modal is handed: a post or video description is never
/// this long, an article rarely is.
const MAX_TEXT_CHARS: usize = 100_000;

fn supported(kind: u16) -> bool {
    shown(kind) || is_repost_kind(kind)
}

/// What a card shows itself; a repost shows the event it carries.
fn shown(kind: u16) -> bool {
    matches!(kind, KIND_POST | KIND_ARTICLE | KIND_VIDEO | KIND_SHORT | KIND_VIDEO_ADDRESSABLE | KIND_SHORT_ADDRESSABLE)
}

fn is_repost_kind(kind: u16) -> bool {
    matches!(kind, KIND_REPOST | KIND_GENERIC_REPOST)
}

/// What a reference points at, as decoded from its bech32.
#[derive(Debug, Clone, PartialEq)]
pub enum EmbedRef {
    Event { id: EventId, author: Option<PublicKey>, kind: Option<u16>, relays: Vec<RelayUrl> },
    Address { coordinate: Coordinate, relays: Vec<RelayUrl> },
}

impl EmbedRef {
    /// A bare entity or a `nostr:` URI. `None` for anything that isn't a post,
    /// article or video reference, so an emoji pack or invite naddr is never one.
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        let bech = s.strip_prefix("nostr:").unwrap_or(s);
        let r = match Nip19::from_bech32(bech).ok()? {
            Nip19::EventId(id) => EmbedRef::Event { id, author: None, kind: None, relays: Vec::new() },
            Nip19::Event(e) => EmbedRef::Event {
                id: e.event_id,
                author: e.author,
                kind: e.kind.map(|k| k.as_u16()),
                relays: e.relays,
            },
            Nip19::Coordinate(c) => EmbedRef::Address { coordinate: c.coordinate, relays: c.relays },
            _ => return None,
        };
        match &r {
            EmbedRef::Event { kind: Some(k), .. } if !supported(*k) => None,
            EmbedRef::Address { coordinate, .. } if !supported(coordinate.kind.as_u16()) => None,
            _ => Some(r),
        }
    }

    /// An https link whose last path segment is a reference (njump, primal, habla
    /// and the like all put it there).
    pub fn from_url(url: &str) -> Option<Self> {
        let rest = url.strip_prefix("https://")?;
        let path = rest.split(['?', '#']).next()?;
        let (_host, path) = path.split_once('/')?;
        let last = path.trim_end_matches('/').rsplit('/').next()?;
        let last = last.trim_end_matches(['!', '\'', '"', '.', ',', ';', ':', '?', ')', ']']);
        let last = last.strip_suffix(".html").unwrap_or(last);
        let entity = last.strip_prefix("nostr:").unwrap_or(last);
        if !(entity.starts_with("note1") || entity.starts_with("nevent1") || entity.starts_with("naddr1")) {
            return None;
        }
        Self::parse(entity)
    }

    fn key(&self) -> String {
        match self {
            EmbedRef::Event { id, .. } => format!("e:{}", id.to_hex()),
            EmbedRef::Address { coordinate, .. } => format!(
                "a:{}:{}:{}",
                coordinate.kind.as_u16(),
                coordinate.public_key.to_hex(),
                coordinate.identifier
            ),
        }
    }

    fn author(&self) -> Option<PublicKey> {
        match self {
            EmbedRef::Event { author, .. } => *author,
            EmbedRef::Address { coordinate, .. } => Some(coordinate.public_key),
        }
    }

    fn hints(&self) -> &[RelayUrl] {
        match self {
            EmbedRef::Event { relays, .. } | EmbedRef::Address { relays, .. } => relays,
        }
    }

    fn filter(&self) -> Filter {
        match self {
            EmbedRef::Event { id, .. } => Filter::new().id(*id).limit(1),
            EmbedRef::Address { coordinate, .. } => Filter::new()
                .author(coordinate.public_key)
                .kind(coordinate.kind)
                .identifier(coordinate.identifier.clone())
                .limit(1),
        }
    }

    /// A relay's answer is only believed when it is exactly what was asked for.
    fn matches(&self, ev: &Event) -> bool {
        if ev.verify().is_err() {
            return false;
        }
        match self {
            EmbedRef::Event { id, author, kind, .. } => {
                ev.id == *id && author.is_none_or(|a| a == ev.pubkey) && kind.is_none_or(|k| k == ev.kind.as_u16())
            }
            EmbedRef::Address { coordinate, .. } => {
                ev.kind == coordinate.kind
                    && ev.pubkey == coordinate.public_key
                    && ev.tags.identifier().as_deref().unwrap_or("") == coordinate.identifier
            }
        }
    }
}

/// A playable or viewable file a post or video carries.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct EmbedMedia {
    pub url: String,
    pub is_video: bool,
    pub mime: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub size: Option<u64>,
    pub duration: Option<f64>,
    pub poster: Option<String>,
    pub sha256: Option<String>,
    pub fallbacks: Vec<String>,
}

/// One referenced event, shaped for its card.
#[derive(Debug, Clone, Serialize)]
pub struct Embed {
    /// "post", "article", "video" or "short".
    pub class: &'static str,
    pub kind: u16,
    pub id: String,
    pub author: String,
    pub created_at: u64,
    pub published_at: Option<u64>,
    pub title: Option<String>,
    pub summary: Option<String>,
    /// Article cover or video poster.
    pub image: Option<String>,
    /// A post's text without its media links, an article's markdown, or a video's description.
    pub text: String,
    pub media: Vec<EmbedMedia>,
    pub video: Option<EmbedMedia>,
    /// A video event's page when it names no file that can be fetched (a YouTube link).
    pub link: Option<String>,
    pub content_warning: Option<String>,
    /// NIP-30 custom emoji the text's `:shortcode:`s name.
    pub emoji: Vec<crate::types::EmojiTag>,
    /// The canonical nevent/naddr, relay hints included, for copying.
    pub bech32: String,
    /// Who reposted it, when the reference named a repost (NIP-18).
    pub reposted_by: Option<String>,
}

/// Fetch and shape the event `reference` (a bare entity, a `nostr:` URI, or a web
/// link ending in one). Repeated asks within a session are answered from memory.
pub async fn fetch(reference: &str) -> Result<Embed, String> {
    let r = EmbedRef::parse(reference)
        .or_else(|| EmbedRef::from_url(reference))
        .ok_or("Not a post, article or video reference")?;
    let key = r.key();
    // Held for the whole ask: an account swap mid-fetch must not file it under the next one.
    let owner = cache();
    let started = web_time::Instant::now();
    let _claim = loop {
        match cache_lookup(&owner, &key) {
            Lookup::Hit(result) => return result,
            Lookup::Fetch => break InFlight { owner: owner.clone(), key: key.clone() },
            Lookup::Wait if started.elapsed().as_secs() >= WAIT_DEADLINE_SECS => {
                break InFlight { owner: owner.clone(), key: key.clone() };
            }
            Lookup::Wait => crate::rt::time::sleep(std::time::Duration::from_millis(100)).await,
        }
    };
    let result = match fetch_event(&r).await {
        Some(ev) if is_repost_kind(ev.kind.as_u16()) => shape_repost(&ev).await,
        Some(ev) => to_embed(&ev, r.hints()),
        None => Err("Not found on any relay".to_string()),
    };
    cache_store(&owner, &key, &result);
    result
}

/// Clears a key's in-flight mark however its fetch ends, a dropped future or a panic included,
/// or every later ask for it would wait on nothing.
struct InFlight {
    owner: Arc<Mutex<EmbedCache>>,
    key: String,
}

impl Drop for InFlight {
    fn drop(&mut self) {
        self.owner.lock().unwrap_or_else(|e| e.into_inner()).in_flight.remove(&self.key);
    }
}

struct EmbedCacheKey;

#[derive(Default)]
struct EmbedCache {
    entries: HashMap<String, Cached>,
    order: VecDeque<String>,
    in_flight: HashSet<String>,
}

enum Cached {
    Hit(Embed),
    Miss { error: String, at: web_time::Instant },
}

enum Lookup {
    Hit(Result<Embed, String>),
    Wait,
    Fetch,
}

/// Which posts this account looked at is its own business, so the cache lives on its session.
fn cache() -> Arc<Mutex<EmbedCache>> {
    crate::db::current_session().scoped::<EmbedCacheKey, _>()
}

fn cache_lookup(owner: &Mutex<EmbedCache>, key: &str) -> Lookup {
    let mut c = owner.lock().unwrap_or_else(|e| e.into_inner());
    match c.entries.get(key) {
        Some(Cached::Hit(embed)) => return Lookup::Hit(Ok(embed.clone())),
        Some(Cached::Miss { error, at }) if at.elapsed().as_secs() < MISS_TTL_SECS => {
            return Lookup::Hit(Err(error.clone()));
        }
        _ => {}
    }
    if c.in_flight.contains(key) {
        return Lookup::Wait;
    }
    c.in_flight.insert(key.to_string());
    Lookup::Fetch
}

fn cache_store(owner: &Mutex<EmbedCache>, key: &str, result: &Result<Embed, String>) {
    let mut c = owner.lock().unwrap_or_else(|e| e.into_inner());
    let entry = match result {
        Ok(embed) => Cached::Hit(embed.clone()),
        Err(error) => Cached::Miss { error: error.clone(), at: web_time::Instant::now() },
    };
    if c.entries.insert(key.to_string(), entry).is_none() {
        c.order.push_back(key.to_string());
    }
    while c.order.len() > CACHE_CAP {
        if let Some(old) = c.order.pop_front() {
            c.entries.remove(&old);
        }
    }
}

/// Our own relays first, then the reference's hints, the author's outbox and the
/// big public relays through a throwaway client.
async fn fetch_event(r: &EmbedRef) -> Option<Event> {
    let filter = r.filter();
    let client = crate::state::nostr_client();
    if let Some(client) = &client {
        let timeout = crate::relay_request_timeout(std::time::Duration::from_secs(POOL_TIMEOUT_SECS));
        match client.fetch_events(filter.clone()).timeout(timeout).await {
            Ok(events) => {
                if let Some(ev) = best(r, events) {
                    return Some(ev);
                }
            }
            Err(e) => crate::log_debug!("[Embeds] pool fetch failed: {e}"),
        }
    }

    // Hints and the author's outbox are chosen by whoever sent the reference: off Tor with the
    // privacy proxy on, dialing them would hand that sender this device's address.
    let mut relays: Vec<RelayUrl> = Vec::new();
    if may_dial_strangers() {
        relays.extend(r.hints().iter().filter(|u| dialable(u)).take(4).cloned());
        if let (Some(client), Some(author)) = (&client, r.author()) {
            let outbox = crate::emoji_packs::fetch_author_write_relays(client, author).await;
            for url in outbox.into_iter().filter(dialable).take(4) {
                if !relays.contains(&url) {
                    relays.push(url);
                }
            }
        }
    }
    for url in PUBLIC_RELAYS.iter().filter_map(|u| RelayUrl::parse(u).ok()) {
        if !relays.contains(&url) {
            relays.push(url);
        }
    }
    relays.truncate(MAX_REMOTE_RELAYS);

    let _lane = REMOTE_LANE.acquire().await.ok();
    // No authenticator: a relay that asks who is reading learns nothing about the user.
    let scratch = crate::apply_tor_proxy(ClientBuilder::new()).build();
    for url in &relays {
        let _ = scratch.add_managed_relay(url.as_str()).await;
    }
    scratch.connect().await;
    let timeout = crate::relay_request_timeout(std::time::Duration::from_secs(REMOTE_TIMEOUT_SECS));
    let result = scratch.fetch_events(filter).timeout(timeout).await;
    scratch.shutdown().await;
    match result {
        Ok(events) => best(r, events),
        Err(e) => {
            crate::log_debug!("[Embeds] remote fetch failed: {e}");
            None
        }
    }
}

/// A repost shows what it reposted, under the reposter's name. Only one level: a repost of a
/// repost is refused, so a chain can't send the fetch round in circles.
async fn shape_repost(ev: &Event) -> Result<Embed, String> {
    let target = repost_target(ev);
    let inner = match reposted_inline(ev, target.as_ref()) {
        Some(inner) => inner,
        None => fetch_event(target.as_ref().ok_or("This repost doesn't say what it reposted")?)
            .await
            .ok_or("The reposted post wasn't found")?,
    };
    if !shown(inner.kind.as_u16()) {
        return Err(format!("Unsupported Nostr event (kind {})", inner.kind.as_u16()));
    }
    let hints: Vec<RelayUrl> = target.as_ref().map(|t| t.hints().to_vec()).unwrap_or_default();
    let mut embed = to_embed(&inner, &hints)?;
    embed.reposted_by = Some(ev.pubkey.to_bech32().map_err(|e| e.to_string())?);
    Ok(embed)
}

/// The event a repost names: its `a` coordinate, else its `e` id with the relay and kind it gives.
fn repost_target(ev: &Event) -> Option<EmbedRef> {
    let tag = |name: &str| {
        ev.tags.iter().find_map(|t| {
            let s = t.as_slice();
            (s.first().map(String::as_str) == Some(name)).then(|| s.to_vec())
        })
    };
    let hint = |s: &[String]| s.get(2).and_then(|u| RelayUrl::parse(u).ok()).into_iter().collect::<Vec<_>>();
    if let Some(a) = tag("a") {
        let coordinate = Coordinate::parse(a.get(1)?).ok()?;
        return Some(EmbedRef::Address { coordinate, relays: hint(&a) });
    }
    let e = tag("e")?;
    Some(EmbedRef::Event {
        id: EventId::from_hex(e.get(1)?).ok()?,
        author: tag("p").and_then(|p| PublicKey::from_hex(p.get(1)?).ok()),
        kind: tag("k").and_then(|k| k.get(1)?.parse().ok()),
        relays: hint(&e),
    })
}

/// The reposted event carried in the repost's content, believed only if it is signed and is
/// the event the repost's tags name.
fn reposted_inline(ev: &Event, target: Option<&EmbedRef>) -> Option<Event> {
    let inner = Event::from_json(ev.content.trim()).ok()?;
    match target {
        Some(t) => t.matches(&inner).then_some(inner),
        None => inner.verify().is_ok().then_some(inner),
    }
}

/// Scratch clients open at once; each dials up to MAX_REMOTE_RELAYS sockets.
static REMOTE_LANE: std::sync::LazyLock<tokio::sync::Semaphore> = std::sync::LazyLock::new(|| tokio::sync::Semaphore::new(4));

/// Whether relays named by a stranger may be dialed: through Tor, or when the user has
/// chosen to connect directly anyway (the privacy proxy off).
fn may_dial_strangers() -> bool {
    #[cfg(all(feature = "tor", not(target_arch = "wasm32")))]
    if !matches!(crate::tor::transport_state(), crate::tor::TorTransportState::Disabled) {
        return true;
    }
    !crate::proxy::enabled()
}

/// A stranger's relay is dialed only over TLS and only on the public internet.
fn dialable(url: &RelayUrl) -> bool {
    let s = url.as_str();
    s.starts_with("wss://") && crate::net::validate_url_not_private(&s.replacen("wss://", "https://", 1)).is_ok()
}

/// The newest event that is exactly what `r` names.
fn best(r: &EmbedRef, events: impl IntoIterator<Item = Event>) -> Option<Event> {
    events.into_iter().filter(|ev| r.matches(ev)).max_by_key(|ev| ev.created_at)
}

fn tag_value<'a>(ev: &'a Event, name: &str) -> Option<&'a str> {
    ev.tags.iter().find_map(|t| {
        let s = t.as_slice();
        (s.first().map(String::as_str) == Some(name)).then(|| s.get(1).map(String::as_str)).flatten()
    })
}

fn non_empty(s: Option<&str>) -> Option<String> {
    s.map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

fn cap_text(mut s: String) -> String {
    if let Some((i, _)) = s.char_indices().nth(MAX_TEXT_CHARS) {
        s.truncate(i);
    }
    s
}

pub(crate) fn to_embed(ev: &Event, hints: &[RelayUrl]) -> Result<Embed, String> {
    let kind = ev.kind.as_u16();
    let class = match kind {
        KIND_POST => "post",
        KIND_ARTICLE => "article",
        KIND_VIDEO | KIND_VIDEO_ADDRESSABLE => "video",
        KIND_SHORT | KIND_SHORT_ADDRESSABLE => "short",
        other => return Err(format!("Unsupported Nostr event (kind {other})")),
    };
    let author = ev.pubkey.to_bech32().map_err(|e| e.to_string())?;
    let hints: Vec<RelayUrl> = hints.iter().take(3).cloned().collect();
    let bech32 = if ev.kind.is_addressable() {
        let coordinate = Coordinate::new(ev.kind, ev.pubkey).identifier(ev.tags.identifier().unwrap_or_default());
        Nip19Coordinate::new(coordinate, hints).to_bech32()
    } else {
        Nip19Event::new(ev.id).author(ev.pubkey).kind(ev.kind).relays(hints).to_bech32()
    }
    .map_err(|e| e.to_string())?;
    let published_at = tag_value(ev, "published_at").and_then(|v| v.trim().parse::<u64>().ok());
    let content_warning = ev.tags.iter().find_map(|t| {
        let s = t.as_slice();
        (s.first().map(String::as_str) == Some("content-warning")).then(|| s.get(1).cloned().unwrap_or_default())
    });

    let content = cap_text(ev.content.clone());
    let imeta = imeta_media(ev);
    let mut embed = Embed {
        class,
        kind,
        id: ev.id.to_hex(),
        author,
        created_at: ev.created_at.as_secs(),
        published_at,
        title: None,
        summary: None,
        image: None,
        text: String::new(),
        media: Vec::new(),
        video: None,
        link: None,
        content_warning,
        emoji: crate::types::EmojiTag::extract_from_tags(ev.tags.iter()),
        bech32,
        reposted_by: None,
    };
    match class {
        "post" => {
            let mut media = imeta;
            let mut seen: HashSet<String> = media.iter().map(|m| m.url.clone()).collect();
            for url in content_urls(&content).into_iter().take(MAX_CONTENT_URLS) {
                if media.len() >= MAX_MEDIA {
                    break;
                }
                if let Some(is_video) = media_kind_by_extension(&url) {
                    if seen.insert(url.clone()) {
                        media.push(EmbedMedia::bare(url, is_video));
                    }
                }
            }
            let mut text = content;
            for m in &media {
                text = text.replace(&m.url, "");
            }
            embed.text = npubs_for_nprofiles(text.trim());
            embed.media = media;
        }
        "article" => {
            embed.title = non_empty(tag_value(ev, "title"));
            embed.summary = non_empty(tag_value(ev, "summary"));
            embed.image = non_empty(tag_value(ev, "image"));
            embed.text = npubs_for_nprofiles(&content);
        }
        _ => {
            embed.title = non_empty(tag_value(ev, "title")).or_else(|| non_empty(tag_value(ev, "alt")));
            embed.text = npubs_for_nprofiles(content.trim());
            let mut variants: Vec<EmbedMedia> = imeta.iter().filter(|m| m.is_video).cloned().collect();
            if variants.is_empty() {
                variants.extend(legacy_video(ev));
            }
            let video = best_variant(variants);
            embed.image = video
                .as_ref()
                .and_then(|v| v.poster.clone())
                .or_else(|| imeta.iter().find_map(|m| m.poster.clone()))
                .or_else(|| non_empty(tag_value(ev, "thumb")))
                .or_else(|| non_empty(tag_value(ev, "image")));
            if video.is_none() {
                // The page, never its thumbnail: an image link is not where the video plays.
                embed.link = imeta
                    .iter()
                    .filter(|m| media_kind_by_extension(&m.url) != Some(false))
                    .map(|m| m.url.clone())
                    .chain(tag_value(ev, "url").filter(|u| is_https(u)).map(str::to_string))
                    .next();
            }
            embed.video = video;
        }
    }
    Ok(embed)
}

impl EmbedMedia {
    fn bare(url: String, is_video: bool) -> Self {
        EmbedMedia {
            url,
            is_video,
            mime: None,
            width: None,
            height: None,
            size: None,
            duration: None,
            poster: None,
            sha256: None,
            fallbacks: Vec::new(),
        }
    }
}

fn is_https(url: &str) -> bool {
    url.starts_with("https://")
}

/// Image or video, judged from the link's extension; `None` for anything else.
fn media_kind_by_extension(url: &str) -> Option<bool> {
    let path = url.split(['?', '#']).next()?.to_ascii_lowercase();
    let ext = path.rsplit_once('.')?.1;
    match ext {
        "jpg" | "jpeg" | "png" | "gif" | "webp" | "avif" | "svg" => Some(false),
        "mp4" | "webm" | "mov" | "m4v" => Some(true),
        _ => None,
    }
}

fn mime_is_video(mime: &str) -> bool {
    matches!(mime, "video/mp4" | "video/webm" | "video/quicktime" | "video/x-m4v")
}

/// The https links in a post's text, with trailing punctuation dropped.
fn content_urls(content: &str) -> Vec<String> {
    content
        .split_whitespace()
        .filter_map(|word| {
            let start = word.find("https://")?;
            let url = word[start..].trim_end_matches(['.', ',', ';', ':', '!', '?', ')', ']', '}', '"', '\'']);
            (url.len() > "https://".len()).then(|| url.to_string())
        })
        .collect()
}

/// NIP-92 `imeta` entries: one `key value` pair per element after the tag name.
fn imeta_media(ev: &Event) -> Vec<EmbedMedia> {
    let mut out = Vec::new();
    for tag in ev.tags.iter() {
        let s = tag.as_slice();
        if s.first().map(String::as_str) != Some("imeta") {
            continue;
        }
        let mut fields: HashMap<&str, Vec<&str>> = HashMap::new();
        for entry in &s[1..] {
            if let Some((k, v)) = entry.split_once(' ') {
                fields.entry(k).or_default().push(v.trim());
            }
        }
        let first = |k: &str| fields.get(k).and_then(|v| v.first()).copied();
        let Some(url) = first("url").filter(|u| is_https(u)) else { continue };
        if out.iter().any(|m: &EmbedMedia| m.url == url) {
            continue;
        }
        let mime = first("m").map(str::to_ascii_lowercase);
        let is_video = match mime.as_deref() {
            Some(m) if m.starts_with("video/") => {
                if !mime_is_video(m) {
                    continue; // a stream manifest (HLS, DASH) can't be fetched as one file
                }
                true
            }
            Some(m) if m.starts_with("image/") => false,
            _ => match media_kind_by_extension(url) {
                Some(v) => v,
                None => continue,
            },
        };
        let (width, height) = first("dim").and_then(parse_dim).map_or((None, None), |(w, h)| (Some(w), Some(h)));
        out.push(EmbedMedia {
            url: url.to_string(),
            is_video,
            mime,
            width,
            height,
            size: first("size").and_then(|v| v.parse().ok()),
            duration: first("duration").and_then(|v| v.parse().ok()).filter(|d: &f64| d.is_finite() && *d > 0.0),
            poster: first("image").filter(|u| is_https(u)).map(str::to_string),
            sha256: first("x").filter(|x| x.len() == 64).map(str::to_ascii_lowercase),
            fallbacks: fields
                .get("fallback")
                .map(|v| v.iter().filter(|u| is_https(u)).take(MAX_FALLBACKS).map(|u| u.to_string()).collect())
                .unwrap_or_default(),
        });
        if out.len() >= MAX_MEDIA {
            break;
        }
    }
    out
}

/// The pre-imeta NIP-71 shape: the file described by top-level tags.
fn legacy_video(ev: &Event) -> Option<EmbedMedia> {
    let url = tag_value(ev, "url").filter(|u| is_https(u))?;
    let mime = tag_value(ev, "m").map(str::to_ascii_lowercase);
    if mime.as_deref().is_some_and(|m| m.starts_with("video/") && !mime_is_video(m)) {
        return None;
    }
    let (width, height) = tag_value(ev, "dim").and_then(parse_dim).map_or((None, None), |(w, h)| (Some(w), Some(h)));
    Some(EmbedMedia {
        url: url.to_string(),
        is_video: true,
        mime,
        width,
        height,
        size: tag_value(ev, "size").and_then(|v| v.parse().ok()),
        duration: tag_value(ev, "duration").and_then(|v| v.parse().ok()).filter(|d: &f64| d.is_finite() && *d > 0.0),
        poster: tag_value(ev, "thumb").or_else(|| tag_value(ev, "image")).filter(|u| is_https(u)).map(str::to_string),
        sha256: tag_value(ev, "x").filter(|x| x.len() == 64).map(str::to_ascii_lowercase),
        fallbacks: Vec::new(),
    })
}

fn parse_dim(dim: &str) -> Option<(u32, u32)> {
    let (w, h) = dim.trim().split_once('x')?;
    Some((w.parse().ok()?, h.parse().ok()?))
}

/// The variant a chat bubble should play: the sharpest one up to 1080p, else the smallest.
fn best_variant(variants: Vec<EmbedMedia>) -> Option<EmbedMedia> {
    let short_side = |m: &EmbedMedia| match (m.width, m.height) {
        (Some(w), Some(h)) => Some(w.min(h)),
        _ => None,
    };
    let mut fitting: Vec<&EmbedMedia> = variants.iter().filter(|m| short_side(m).is_none_or(|s| s <= 1080)).collect();
    fitting.sort_by_key(|m| short_side(m).unwrap_or(0));
    if let Some(m) = fitting.last() {
        return Some((*m).clone());
    }
    variants.into_iter().min_by_key(|m| short_side(m).unwrap_or(u32::MAX))
}

/// `nostr:nprofile1…` mentions become `nostr:npub1…`, which chat already renders as a name.
fn npubs_for_nprofiles(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find("nostr:nprofile1") {
        out.push_str(&rest[..i]);
        let tail = &rest[i + "nostr:".len()..];
        let end = tail.find(|c: char| !c.is_ascii_alphanumeric()).unwrap_or(tail.len());
        let entity = &tail[..end];
        match Nip19Profile::from_bech32(entity).ok().and_then(|p| p.public_key.to_bech32().ok()) {
            Some(npub) => {
                out.push_str("nostr:");
                out.push_str(&npub);
            }
            None => out.push_str(&rest[i..i + "nostr:".len() + end]),
        }
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signed(kind: u16, content: &str, tags: Vec<Tag>) -> (Keys, Event) {
        let keys = Keys::generate();
        let ev = EventBuilder::new(Kind::from(kind), content).tags(tags).finalize(&keys).unwrap();
        (keys, ev)
    }

    fn raw(parts: &[&str]) -> Tag {
        Tag::parse(parts.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap()
    }

    #[test]
    fn references_parse_from_every_form_and_only_for_supported_kinds() {
        let keys = Keys::generate();
        let id = EventId::from_byte_array([0u8; 32]);
        let note = id.to_bech32().unwrap();
        assert!(matches!(EmbedRef::parse(&note), Some(EmbedRef::Event { kind: None, .. })));
        assert!(EmbedRef::parse(&format!("nostr:{note}")).is_some());

        let nevent = Nip19Event::new(id).kind(Kind::from(KIND_SHORT)).to_bech32().unwrap();
        assert!(EmbedRef::parse(&nevent).is_some());
        let reaction = Nip19Event::new(id).kind(Kind::from(7)).to_bech32().unwrap();
        assert_eq!(EmbedRef::parse(&reaction), None, "a known unsupported kind is no embed");

        let article = Nip19Coordinate::new(Coordinate::new(Kind::from(KIND_ARTICLE), keys.public_key()).identifier("essay"), Vec::<RelayUrl>::new())
            .to_bech32()
            .unwrap();
        assert!(matches!(EmbedRef::parse(&article), Some(EmbedRef::Address { .. })));
        let pack = Nip19Coordinate::new(Coordinate::new(Kind::from(30030), keys.public_key()).identifier("pack"), Vec::<RelayUrl>::new())
            .to_bech32()
            .unwrap();
        assert_eq!(EmbedRef::parse(&pack), None, "an emoji pack keeps its own card");
        assert_eq!(EmbedRef::parse(&keys.public_key().to_bech32().unwrap()), None);
        assert_eq!(EmbedRef::parse("nevent1garbage"), None);
    }

    #[test]
    fn web_links_carry_a_reference_only_in_their_last_segment() {
        let keys = Keys::generate();
        let nevent = Nip19Event::new(EventId::from_byte_array([0u8; 32])).to_bech32().unwrap();
        let naddr = Nip19Coordinate::new(Coordinate::new(Kind::from(KIND_ARTICLE), keys.public_key()).identifier("x"), Vec::<RelayUrl>::new())
            .to_bech32()
            .unwrap();
        assert!(EmbedRef::from_url(&format!("https://njump.me/{nevent}")).is_some());
        assert!(EmbedRef::from_url(&format!("https://primal.net/e/{nevent}/?ref=1#top")).is_some());
        assert!(EmbedRef::from_url(&format!("https://habla.news/a/{naddr}")).is_some());
        assert!(EmbedRef::from_url(&format!("https://example.com/nostr:{nevent}")).is_some());
        assert_eq!(EmbedRef::from_url(&format!("https://example.com/{nevent}/comments")), None);
        assert_eq!(EmbedRef::from_url(&format!("https://example.com/?id={nevent}")), None);
        assert_eq!(EmbedRef::from_url(&format!("http://njump.me/{nevent}")), None);

        let pack = Nip19Coordinate::new(Coordinate::new(Kind::from(30030), keys.public_key()).identifier("p"), Vec::<RelayUrl>::new())
            .to_bech32()
            .unwrap();
        assert_eq!(EmbedRef::from_url(&format!("https://vectorapp.io/emojis/pack/{pack}")), None);
    }

    #[test]
    fn only_the_exact_signed_event_is_believed() {
        let (keys, ev) = signed(KIND_POST, "hello", vec![]);
        let r = EmbedRef::Event { id: ev.id, author: Some(keys.public_key()), kind: Some(KIND_POST), relays: vec![] };
        assert!(r.matches(&ev));
        let wrong_author = EmbedRef::Event { id: ev.id, author: Some(Keys::generate().public_key()), kind: None, relays: vec![] };
        assert!(!wrong_author.matches(&ev));
        let (_, other) = signed(KIND_POST, "other", vec![]);
        assert!(!r.matches(&other), "a different id is not the event");

        let mut forged: serde_json::Value = serde_json::from_str(&ev.as_json()).unwrap();
        forged["content"] = "tampered".into();
        let forged = Event::from_json(forged.to_string()).unwrap();
        assert!(!r.matches(&forged), "a bad signature is never believed");

        let (keys, article) = signed(KIND_ARTICLE, "# Hi", vec![Tag::identifier("essay")]);
        let addr = EmbedRef::Address { coordinate: Coordinate::new(Kind::from(KIND_ARTICLE), keys.public_key()).identifier("essay"), relays: vec![] };
        assert!(addr.matches(&article));
        let elsewhere = EmbedRef::Address { coordinate: Coordinate::new(Kind::from(KIND_ARTICLE), keys.public_key()).identifier("other"), relays: vec![] };
        assert!(!elsewhere.matches(&article));
    }

    #[test]
    fn a_post_splits_its_media_from_its_text() {
        let mention = Nip19Profile::new(Keys::generate().public_key(), Vec::<RelayUrl>::new()).to_bech32().unwrap();
        let content = format!(
            "Look at this https://cdn.example/a.jpg and https://cdn.example/clip.mp4?x=1, says nostr:{mention}. More: https://example.com/page"
        );
        let (_, ev) = signed(
            KIND_POST,
            &content,
            vec![
                raw(&["imeta", "url https://cdn.example/a.jpg", "m image/jpeg", "dim 800x600"]),
                raw(&["content-warning", "spoilers"]),
            ],
        );
        let e = to_embed(&ev, &[]).unwrap();
        assert_eq!(e.class, "post");
        assert_eq!(e.media.len(), 2);
        assert_eq!((e.media[0].url.as_str(), e.media[0].is_video, e.media[0].width), ("https://cdn.example/a.jpg", false, Some(800)));
        assert!(e.media[1].is_video && e.media[1].url == "https://cdn.example/clip.mp4?x=1");
        assert!(!e.text.contains("cdn.example"), "media links leave the text: {}", e.text);
        assert!(e.text.contains("https://example.com/page"), "other links stay");
        assert!(e.text.contains("nostr:npub1") && !e.text.contains("nprofile"), "{}", e.text);
        assert_eq!(e.content_warning.as_deref(), Some("spoilers"));
        assert!(e.bech32.starts_with("nevent1"));
    }

    #[test]
    fn an_article_keeps_its_markdown_and_metadata() {
        let (_, ev) = signed(
            KIND_ARTICLE,
            "# Title\n\nBody ![pic](https://cdn.example/p.png)",
            vec![
                Tag::identifier("essay"),
                raw(&["title", "On Embeds"]),
                raw(&["summary", "Why cards"]),
                raw(&["image", "https://cdn.example/cover.jpg"]),
                raw(&["published_at", "1700000000"]),
            ],
        );
        let e = to_embed(&ev, &[]).unwrap();
        assert_eq!(e.class, "article");
        assert_eq!(e.title.as_deref(), Some("On Embeds"));
        assert_eq!(e.summary.as_deref(), Some("Why cards"));
        assert_eq!(e.image.as_deref(), Some("https://cdn.example/cover.jpg"));
        assert_eq!(e.published_at, Some(1_700_000_000));
        assert!(e.text.contains("![pic]"), "the markdown is passed whole");
        assert!(e.bech32.starts_with("naddr1"));
    }

    #[test]
    fn a_video_picks_a_playable_variant_and_its_poster() {
        let (_, ev) = signed(
            KIND_VIDEO,
            "A description",
            vec![
                raw(&["title", "Clip"]),
                raw(&["imeta", "url https://v.example/master.m3u8", "m application/x-mpegURL"]),
                raw(&["imeta", "url https://v.example/hls.m3u8", "m video/mp2t"]),
                raw(&["imeta", "url https://v.example/4k.mp4", "m video/mp4", "dim 3840x2160", "image https://v.example/4k.jpg"]),
                raw(&["imeta", "url https://v.example/720.mp4", "m video/mp4", "dim 1280x720", "image https://v.example/p.jpg", "duration 29.5", "size 4000000", "fallback https://m.example/720.mp4"]),
                raw(&["imeta", "url https://v.example/480.mp4", "m video/mp4", "dim 854x480"]),
            ],
        );
        let e = to_embed(&ev, &[]).unwrap();
        assert_eq!(e.class, "video");
        let v = e.video.unwrap();
        assert_eq!(v.url, "https://v.example/720.mp4", "the sharpest up to 1080p");
        assert_eq!((v.duration, v.size), (Some(29.5), Some(4_000_000)));
        assert_eq!(v.fallbacks, vec!["https://m.example/720.mp4".to_string()]);
        assert_eq!(e.image.as_deref(), Some("https://v.example/p.jpg"));
        assert_eq!(e.title.as_deref(), Some("Clip"));

        let (_, short) = signed(
            KIND_SHORT_ADDRESSABLE,
            "",
            vec![Tag::identifier("s"), raw(&["url", "https://v.example/s.mp4"]), raw(&["thumb", "https://v.example/s.jpg"]), raw(&["dim", "1080x1920"])],
        );
        let e = to_embed(&short, &[]).unwrap();
        assert_eq!(e.class, "short");
        assert_eq!(e.video.as_ref().map(|v| v.url.as_str()), Some("https://v.example/s.mp4"), "the pre-imeta shape still plays");
        assert_eq!(e.image.as_deref(), Some("https://v.example/s.jpg"));
    }

    #[test]
    fn a_video_page_without_a_file_links_out_with_its_poster() {
        let (_, ev) = signed(
            KIND_VIDEO,
            "Watch this",
            vec![raw(&["title", "Talk"]), raw(&["imeta", "url https://www.youtube.com/watch?v=abc", "image https://i.ytimg.com/vi/abc/hq.jpg", "m image/jpeg"])],
        );
        let e = to_embed(&ev, &[]).unwrap();
        assert!(e.video.is_none(), "a web page is not a file to fetch");
        assert_eq!(e.image.as_deref(), Some("https://i.ytimg.com/vi/abc/hq.jpg"));
        assert_eq!(e.link.as_deref(), Some("https://www.youtube.com/watch?v=abc"));
    }

    #[test]
    fn a_huge_post_is_shaped_in_bounded_time() {
        let words: Vec<String> = (0..60_000).map(|i| format!("https://a.b/{i}.png")).collect();
        let (_, ev) = signed(KIND_POST, &words.join(" "), vec![]);
        let started = std::time::Instant::now();
        let e = to_embed(&ev, &[]).unwrap();
        assert!(started.elapsed() < std::time::Duration::from_secs(2), "took {:?}", started.elapsed());
        assert_eq!(e.media.len(), MAX_MEDIA);
        assert!(e.text.chars().count() <= MAX_TEXT_CHARS);
    }

    #[test]
    fn a_repeated_file_is_listed_once_with_few_mirrors() {
        let mirrors: Vec<String> = (0..50).map(|i| format!("fallback https://m{i}.example/a.mp4")).collect();
        let mut imeta = vec!["imeta".to_string(), "url https://v.example/a.mp4".to_string(), "m video/mp4".to_string()];
        imeta.extend(mirrors);
        let imeta: Vec<&str> = imeta.iter().map(String::as_str).collect();
        let (_, ev) = signed(KIND_POST, "clip", vec![raw(&imeta), raw(&["imeta", "url https://v.example/a.mp4", "m video/mp4"])]);
        let e = to_embed(&ev, &[]).unwrap();
        assert_eq!(e.media.len(), 1, "one card per file, or the list's keys collide");
        assert_eq!(e.media[0].fallbacks.len(), MAX_FALLBACKS);
    }

    #[test]
    fn a_link_ending_in_punctuation_still_names_its_event() {
        let nevent = Nip19Event::new(EventId::from_byte_array([0u8; 32])).to_bech32().unwrap();
        assert!(EmbedRef::from_url(&format!("https://njump.me/{nevent}!")).is_some());
        assert!(EmbedRef::from_url(&format!("https://njump.me/{nevent}'")).is_some());
    }

    #[test]
    fn a_strangers_relay_must_be_public_and_encrypted() {
        let ok = |u: &str| dialable(&RelayUrl::parse(u).unwrap());
        assert!(ok("wss://relay.damus.io"));
        assert!(!ok("ws://relay.damus.io"), "plain text");
        assert!(!ok("wss://127.0.0.1:631"), "loopback");
        assert!(!ok("wss://192.168.1.1"), "LAN");
        assert!(!ok("wss://localhost"));
    }

    #[test]
    fn an_abandoned_fetch_releases_its_key() {
        let owner: Arc<Mutex<EmbedCache>> = Arc::default();
        assert!(matches!(cache_lookup(&owner, "k"), Lookup::Fetch));
        assert!(matches!(cache_lookup(&owner, "k"), Lookup::Wait), "a second ask waits on the first");
        drop(InFlight { owner: owner.clone(), key: "k".into() });
        assert!(matches!(cache_lookup(&owner, "k"), Lookup::Fetch), "the dropped fetch no longer holds it");
    }

    #[test]
    fn a_repost_carries_the_event_it_names() {
        let (author, post) = signed(KIND_POST, "the original", vec![]);
        let (_, repost) = signed(
            KIND_REPOST,
            &post.as_json(),
            vec![Tag::parse(["e".to_string(), post.id.to_hex(), "wss://r.example".to_string()]).unwrap(), Tag::public_key(author.public_key())],
        );
        let target = repost_target(&repost).unwrap();
        assert!(matches!(&target, EmbedRef::Event { id, relays, .. } if *id == post.id && relays.len() == 1));
        assert_eq!(reposted_inline(&repost, Some(&target)).map(|e| e.id), Some(post.id));

        let (_, other) = signed(KIND_POST, "not what the tag names", vec![]);
        let (_, lying) = signed(KIND_REPOST, &other.as_json(), vec![Tag::parse(["e".to_string(), post.id.to_hex()]).unwrap()]);
        assert!(reposted_inline(&lying, repost_target(&lying).as_ref()).is_none(), "content must be the tagged event");

        let mut forged: serde_json::Value = serde_json::from_str(&post.as_json()).unwrap();
        forged["content"] = "tampered".into();
        let (_, forged_repost) = signed(KIND_REPOST, &forged.to_string(), vec![]);
        assert!(reposted_inline(&forged_repost, None).is_none(), "an unsigned copy is never believed");
    }

    #[test]
    fn a_generic_repost_of_an_article_names_its_coordinate() {
        let (keys, _) = signed(KIND_ARTICLE, "", vec![]);
        let a = format!("{KIND_ARTICLE}:{}:essay", keys.public_key().to_hex());
        let (_, repost) = signed(KIND_GENERIC_REPOST, "", vec![Tag::parse(["a".to_string(), a, "wss://r.example".to_string()]).unwrap()]);
        assert!(matches!(repost_target(&repost), Some(EmbedRef::Address { coordinate, .. }) if coordinate.identifier == "essay"));
    }

    #[test]
    fn an_unsupported_kind_is_reported_not_shown() {
        let (_, ev) = signed(6, "", vec![]);
        assert!(to_embed(&ev, &[]).unwrap_err().contains("kind 6"));
    }
}
