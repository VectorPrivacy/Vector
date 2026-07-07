//! NIP-30 / NIP-51 custom emoji + pack support.
//!
//! Phase 1 (read path):
//! - Parse kind 30030 emoji sets and kind 10030 user emoji lists.
//! - Fetch a user's subscribed packs from relays, persist them locally.
//! - Expose a flat `EmojiPack` API to the frontend for picker rendering.
//!
//! Spec: <https://nips.nostr.com/30>, <https://nips.nostr.com/51>.
//!
//! Metadata interop: NIP-51 standardises `title` / `image` / `description`
//! tags on kind 30030. Ditto and Nostria emit non-standard `name` /
//! `picture` / `about` instead. Vector reads both with spec preference
//! and (eventually) dual-writes both on publish.

use std::collections::HashMap;

use nostr_sdk::prelude::*;
use serde::{Deserialize, Serialize};

use crate::state::nostr_client;

/// NIP-51 kind for a user's "Emojis" list (replaceable, per-user).
const KIND_EMOJI_LIST: u16 = 10030;

/// NIP-51 kind for an "Emoji set" (parameterised replaceable).
pub const KIND_EMOJI_SET: u16 = 30030;

/// Wire-form prefix for kind 30030 `a` tags / DB primary keys.
/// `format!("{}:…", KIND_EMOJI_SET)` is the obvious construction but
/// `format!` is heavy (Display dispatch, intermediate capacity probing);
/// for an addr that's read on every pack save / load / subscribe, hold
/// it as a literal and assert at compile time that the kind matches.
const KIND_EMOJI_SET_ADDR_PREFIX: &str = "30030:";
const _: () = assert!(
    KIND_EMOJI_SET == 30030,
    "KIND_EMOJI_SET_ADDR_PREFIX literal must match KIND_EMOJI_SET"
);

/// Build a canonical `kind:pubkey:identifier` addr string with a single
/// allocation sized exactly to the result. ~3× faster than the
/// equivalent `format!` in microbenchmarks and has no fmt machinery on
/// the hot path (load, save, subscribe all hit this).
fn build_pack_addr(pubkey: &str, identifier: &str) -> String {
    let mut s = String::with_capacity(
        KIND_EMOJI_SET_ADDR_PREFIX.len() + pubkey.len() + 1 + identifier.len(),
    );
    s.push_str(KIND_EMOJI_SET_ADDR_PREFIX);
    s.push_str(pubkey);
    s.push(':');
    s.push_str(identifier);
    s
}

/// Network fetch budget for resolving the user's pack list + each pack.
/// 8s was too tight in practice — a single slow relay handshake would
/// flash "Pack Unavailable" at the user when the event was still on its
/// way. 20s comfortably covers a cold Tor circuit / sleepy relay
/// without making a genuinely-missing pack feel sluggish.
const FETCH_TIMEOUT_SECS: u64 = 20;

/// Per-effective-tier caps (index = `badges::effective_tier()`, 0-3). These gate
/// only the in-app add action; packs subscribed via other clients always load in
/// full and are never sliced. Tier 3 (full premium) is effectively unlimited for
/// the count cap.
const EQUIPPED_PACKS_BY_TIER: [usize; 4] = [3, 6, 9, usize::MAX];
/// Per-pack emoji cap by tier (authoring + display). Full premium tops out at 90.
const EMOJIS_PER_PACK_BY_TIER: [usize; 4] = [30, 30, 60, 90];

/// Base (free, tier-0) equipped-pack cap. Named const for the frontend mirror.
pub const MAX_EQUIPPED_PACKS: usize = EQUIPPED_PACKS_BY_TIER[0];

/// Base (free, tier-0) emojis-per-own-pack cap. Shared packs received from the
/// network may exceed this — only what *we* author is capped, and the frontend
/// truncates oversized received packs at display time.
pub const MAX_EMOJIS_PER_PACK: usize = EMOJIS_PER_PACK_BY_TIER[0];

/// In-app equipped-pack cap for the current account, scaled by effective tier.
/// Gate the in-app subscribe/create action on this — never the load/display path.
pub fn effective_max_equipped_packs() -> usize {
    EQUIPPED_PACKS_BY_TIER[crate::badges::effective_tier() as usize]
}

/// Per-pack emoji authoring cap for the current account, scaled by effective tier.
pub fn effective_max_emojis_per_pack() -> usize {
    EMOJIS_PER_PACK_BY_TIER[crate::badges::effective_tier() as usize]
}

// ============================================================================
// Types
// ============================================================================

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct PackEmoji {
    pub shortcode: String,
    pub url: String,
    pub sha256: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct EmojiPack {
    /// Canonical NIP-19 `naddr1...` for this pack (no relay hints).
    /// Mirrors the `profile.id == npub` pattern — frontend / IPC code
    /// only ever speaks bech32. Internal storage + kind 10030 `a` tags
    /// still use the raw `kind:pubkey:identifier` coordinate (DB
    /// columns named `addr`, helpers exposed via `parse_pack_address`
    /// / `naddr_from_addr`).
    pub id: String,
    /// Author of the kind 30030 event (hex).
    pub pubkey: String,
    /// `d` tag identifier.
    pub identifier: String,
    /// NIP-51 `title` with Ditto `name` fallback.
    pub title: String,
    /// NIP-51 `image` with Ditto `picture` fallback. Empty if neither.
    pub image_url: String,
    /// NIP-51 `description` with Ditto `about` fallback.
    pub description: String,
    pub emojis: Vec<PackEmoji>,
    /// Owned packs surface a different UI affordance (edit pencil).
    pub is_own: bool,
    /// Event `created_at` — fed back into the relay filter so re-fetches
    /// don't process older events on top of a newer cached pack.
    pub updated_at: u64,
    /// Health verdict: [`PACK_STATUS_ACTIVE`] / [`PACK_STATUS_REVOKED`] /
    /// [`PACK_STATUS_MISSING`]. Dead packs stay in the picker payload so the
    /// UI can render the section greyed with an explanation + remove button,
    /// but their emojis are excluded from sending and suggestions.
    pub status: u8,
}

/// Pack is live on its relays (or hasn't been judged otherwise).
pub const PACK_STATUS_ACTIVE: u8 = 0;
/// A deterministic tombstone was seen: the author replaced the pack with an
/// EMPTY kind 30030 (Vector's own delete flow) or published a kind-5
/// deletion naming it.
pub const PACK_STATUS_REVOKED: u8 = 1;
/// No tombstone, but the pack has been absent across enough clean sweeps of
/// live relays (see the gauntlet in [`apply_pack_health`]) that it's
/// considered gone.
pub const PACK_STATUS_MISSING: u8 = 2;

/// What one refresh sweep learned about a subscribed pack.
#[derive(Debug, Clone, PartialEq)]
pub enum PackFetchOutcome {
    /// A live (non-empty) pack event resolved.
    Found,
    /// Deterministic deletion evidence — no gauntlet needed.
    Tombstoned,
    /// Enough live relays answered EOSE and none had the pack.
    CleanMiss,
    /// Not enough connectivity to judge; counters must not move.
    Unreachable,
}

/// Absence promotes to `missing` only after this many rate-limited clean
/// misses AND [`PACK_MISS_PROMOTE_SECS`] since the first — strict enough
/// that a weekend relay outage never flags a healthy pack.
const PACK_MISS_PROMOTE_COUNT: i64 = 3;
const PACK_MISS_PROMOTE_SECS: i64 = 48 * 3600;
/// A clean miss moves the counter at most once per this window, so rapid
/// app restarts can't fabricate the miss count.
const PACK_MISS_RATELIMIT_SECS: i64 = 12 * 3600;

impl EmojiPack {
    /// Raw NIP-51 coordinate (`kind:pubkey:identifier`). Used for kind
    /// 10030 `a` tags + DB keying. One pre-sized allocation, no fmt
    /// machinery (see `build_pack_addr`).
    pub fn addr(&self) -> String {
        build_pack_addr(&self.pubkey, &self.identifier)
    }
}

// ============================================================================
// Parsing
// ============================================================================

/// Validate a NIP-30 shortcode: `[a-zA-Z0-9_-]+`, non-empty. Anything
/// else would render brokenly against the `:[\w-]+:` regex our renderer
/// uses, so we drop invalid items at parse time.
fn is_valid_shortcode(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Active theme pack's `(shortcode, url)` pairs, registered from the frontend.
/// The theme pack is shown in the picker without being a real subscription, so
/// its shortcodes never land in `emoji_pack_items`; this lets the send resolver
/// still attach NIP-30 tags for them. Replaced wholesale on theme change.
static THEME_EMOJI_TAGS: std::sync::OnceLock<std::sync::Mutex<Vec<(String, String)>>> =
    std::sync::OnceLock::new();

fn theme_emoji_tags() -> &'static std::sync::Mutex<Vec<(String, String)>> {
    THEME_EMOJI_TAGS.get_or_init(|| std::sync::Mutex::new(Vec::new()))
}

/// Register (or clear, with an empty vec) the active theme pack's emoji so the
/// send resolver can tag them even though they aren't a DB subscription.
pub fn set_theme_emoji_tags(tags: Vec<(String, String)>) {
    if let Ok(mut g) = theme_emoji_tags().lock() {
        *g = tags;
    }
}

/// Per-pack emoji cap mirrored from the frontend's `MAX_DISPLAY_EMOJIS_PER_PACK`
/// (`applyBadgeLimits`). The send resolver caps each subscribed pack to this
/// many items so its `~N` disambiguation of duplicate shortcodes matches exactly
/// what the picker displayed — otherwise a large pack's hidden tail (the picker
/// only shows the first N) would enter the candidate set and shift the indices.
pub fn effective_display_cap() -> usize {
    EMOJIS_PER_PACK_BY_TIER[crate::badges::effective_tier() as usize]
}

/// Resolve a (possibly `~N`-suffixed) shortcode token against the candidate map.
/// Plain `base` → the first candidate; `base~N` → the N-th (1-based) candidate
/// in the same URL-sorted order the picker assigns. A `~N` whose base is unknown
/// or whose index is out of range yields nothing (renders as literal text).
fn resolve_emoji_token(by_code: &HashMap<String, Vec<String>>, token: &str) -> Option<String> {
    if let Some((base, suffix)) = token.rsplit_once('~') {
        let n: usize = suffix.parse().ok()?;
        if n == 0 { return None; }
        return by_code.get(base).and_then(|urls| urls.get(n - 1).cloned());
    }
    by_code.get(token).and_then(|urls| urls.first().cloned())
}

/// Scan `content` for `:shortcode:` patterns and resolve them against the user's
/// currently-subscribed packs (plus the active theme pack). Returns deduped emoji
/// tags in first-match order. Used by the send pipeline to attach NIP-30 emoji
/// tags so recipients without the pack subscribed still render.
///
/// Duplicate shortcodes across packs are disambiguated `base~N` (1-based) by a
/// lexicographic URL sort — the SAME ordering the frontend's `_assignEmojiDisambig`
/// uses — so a `:love~2:` the picker inserted resolves here to the same image.
pub fn resolve_outbound_emoji_tags(content: &str) -> Vec<crate::types::EmojiTag> {
    if content.is_empty() || !content.contains(':') {
        return Vec::new();
    }

    // base shortcode -> ordered candidate URLs (one per distinct image).
    let cap = effective_display_cap();
    let mut by_code: HashMap<String, Vec<String>> = HashMap::new();

    // Subscribed packs. INNER JOIN matches `load_all_packs` — soft-removed own
    // packs shouldn't leak their Blossom URLs through outbound tags when the
    // user types a shortcode they thought was hidden. Dead packs (revoked /
    // missing) are excluded too: their emojis are retired from sending, not
    // just from the picker.
    if let Ok(conn) = crate::db::get_db_connection_guard_static() {
        if let Ok(mut stmt) = conn.prepare(
            "SELECT p.addr, i.shortcode, i.url
             FROM emoji_pack_items i
             INNER JOIN emoji_packs p ON p.addr = i.pack_addr
             INNER JOIN emoji_pack_subscriptions s ON s.addr = p.addr
             WHERE p.status = 0
             ORDER BY p.is_own DESC, p.updated_at DESC, i.position ASC"
        ) {
            if let Ok(rows) = stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?))
            }) {
                // Mirror the picker's per-pack display cap so hidden tail emojis
                // don't enter (and skew) disambiguation.
                let mut per_pack: HashMap<String, usize> = HashMap::new();
                for (addr, code, url) in rows.flatten() {
                    let n = per_pack.entry(addr).or_insert(0);
                    if *n >= cap { continue; }
                    *n += 1;
                    by_code.entry(code).or_default().push(url);
                }
            }
        }
    }

    // Active theme pack fills any gaps. It's shown in the picker without being
    // a real subscription, so its shortcodes aren't in the DB — registered
    // from the frontend via `set_theme_emoji_tags` (already capped there).
    if let Ok(theme) = theme_emoji_tags().lock() {
        for (code, url) in theme.iter() {
            by_code.entry(code.clone()).or_default().push(url.clone());
        }
    }

    if by_code.is_empty() {
        return Vec::new();
    }

    // De-dup identical images + lexicographic sort so `~N` is stable and matches
    // the frontend (two packs carrying the same URL collapse to one candidate).
    for urls in by_code.values_mut() {
        urls.sort();
        urls.dedup();
    }

    let mut out: Vec<crate::types::EmojiTag> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let bytes = content.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b':' {
            let start = i + 1;
            let mut j = start;
            while j < bytes.len() {
                let c = bytes[j];
                // `~` is the reserved disambiguation separator (`:love~2:`).
                let ok = c.is_ascii_alphanumeric() || c == b'_' || c == b'-' || c == b'~';
                if !ok { break; }
                j += 1;
            }
            if j > start && j < bytes.len() && bytes[j] == b':' {
                if let Ok(token) = std::str::from_utf8(&bytes[start..j]) {
                    if !seen.contains(token) {
                        if let Some(url) = resolve_emoji_token(&by_code, token) {
                            out.push(crate::types::EmojiTag {
                                shortcode: token.to_string(),
                                url,
                            });
                            seen.insert(token.to_string());
                        }
                    }
                }
                i = j + 1;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// Fetch the first single-string tag whose key matches any of `keys`,
/// in order. Used for the dual NIP-51 / Ditto metadata lookup.
fn first_tag(tags: &Tags, keys: &[&str]) -> Option<String> {
    for key in keys {
        for tag in tags.iter() {
            let parts: Vec<&str> = tag.as_slice().iter().map(|s| s.as_str()).collect();
            if parts.len() >= 2 && parts[0] == *key {
                return Some(parts[1].to_string());
            }
        }
    }
    None
}

/// Parse a kind 30030 event into an EmojiPack. Returns `None` if the
/// event is missing a `d` tag or has zero valid emoji rows.
pub fn parse_pack_from_event(event: &Event, my_pubkey_hex: Option<&str>) -> Option<EmojiPack> {
    if event.kind.as_u16() != KIND_EMOJI_SET {
        return None;
    }

    let identifier = first_tag(&event.tags, &["d"])?;
    let pubkey = event.pubkey.to_hex();
    let addr = build_pack_addr(&pubkey, &identifier);
    let id = match naddr_from_addr(&addr) {
        Ok(s) => s,
        Err(e) => {
            crate::log_warn!(
                "[EmojiPacks] naddr encode failed for `{}`: {} — pack dropped",
                addr, e,
            );
            return None;
        }
    };

    let title = first_tag(&event.tags, &["title", "name"]).unwrap_or_default();
    let image_url = first_tag(&event.tags, &["image", "picture"]).unwrap_or_default();
    let description = first_tag(&event.tags, &["description", "about"]).unwrap_or_default();

    let mut emojis = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for tag in event.tags.iter() {
        let parts: Vec<&str> = tag.as_slice().iter().map(|s| s.as_str()).collect();
        if parts.len() >= 3 && parts[0] == "emoji" {
            let shortcode = parts[1];
            if !is_valid_shortcode(shortcode) { continue; }
            // Trim URL whitespace: some packs in the wild carry a stray leading/trailing space
            // (e.g. "…/x.gif "), which the WebView strips for a raw <img> but the cache fetch does
            // NOT — leaving the emoji blank everywhere it's served from cache. Skip if empty after.
            let url = parts[2].trim();
            if url.is_empty() { continue; }
            if !seen.insert(shortcode.to_string()) { continue; }
            emojis.push(PackEmoji {
                shortcode: shortcode.to_string(),
                url: url.to_string(),
                sha256: None,
            });
        }
    }

    if emojis.is_empty() {
        return None;
    }

    let is_own = my_pubkey_hex.map_or(false, |me| me == pubkey);

    Some(EmojiPack {
        id,
        pubkey,
        identifier,
        title,
        image_url,
        description,
        emojis,
        is_own,
        updated_at: event.created_at.as_secs(),
        status: PACK_STATUS_ACTIVE,
    })
}

/// Parsed NIP-19 / NIP-51 set address.
#[derive(Debug, Clone, PartialEq)]
pub struct PackAddress {
    pub kind: u16,
    pub pubkey: PublicKey,
    pub identifier: String,
}

/// Parse a `kind:pubkey-hex:d-tag` address as found in kind 10030 `a` tags.
/// Rejects anything that isn't kind 30030 — we don't want a malformed list
/// pulling in random replaceable events.
pub fn parse_pack_address(addr: &str) -> Result<PackAddress, String> {
    let mut parts = addr.splitn(3, ':');
    let kind_str = parts.next().ok_or_else(|| "missing kind".to_string())?;
    let pubkey_str = parts.next().ok_or_else(|| "missing pubkey".to_string())?;
    let identifier = parts.next().ok_or_else(|| "missing identifier".to_string())?;

    let kind: u16 = kind_str.parse()
        .map_err(|_| format!("invalid kind: {}", kind_str))?;
    if kind != KIND_EMOJI_SET {
        return Err(format!("expected kind {}, got {}", KIND_EMOJI_SET, kind));
    }
    let pubkey = PublicKey::from_hex(pubkey_str)
        .map_err(|e| format!("invalid pubkey: {}", e))?;

    Ok(PackAddress { kind, pubkey, identifier: identifier.to_string() })
}

impl PackAddress {
    /// Serialise back to the wire form used in kind 10030 `a` tags.
    /// `parse_pack_address` is the only constructor and it rejects any
    /// kind ≠ KIND_EMOJI_SET, so we can route through the optimised
    /// `build_pack_addr` and skip `format!` entirely.
    pub fn to_addr_string(&self) -> String {
        debug_assert_eq!(self.kind, KIND_EMOJI_SET,
            "PackAddress kind mismatch — was it constructed bypassing parse_pack_address?");
        build_pack_addr(&self.pubkey.to_hex(), &self.identifier)
    }
}

/// Encode a pack `addr` (`kind:pubkey:identifier`) into a NIP-19
/// `naddr1...` bech32 string. Used by the share-pack flow to put a
/// portable reference on the user's clipboard.
pub fn naddr_from_addr(addr: &str) -> Result<String, String> {
    let parsed = parse_pack_address(addr)?;
    let coord = nostr_sdk::nips::nip01::Coordinate {
        kind: Kind::Custom(parsed.kind),
        public_key: parsed.pubkey,
        identifier: parsed.identifier,
    };
    let n19 = nostr_sdk::nips::nip19::Nip19Coordinate {
        coordinate: coord,
        relays: Vec::new(),
    };
    nostr_sdk::nips::nip19::Nip19::Coordinate(n19)
        .to_bech32()
        .map_err(|e| format!("encode naddr: {}", e))
}

/// Decode a NIP-19 `naddr1...` into a `PackAddress`. Rejects coordinates
/// that don't point at kind 30030 so a malformed paste can't pull in
/// an unrelated replaceable event.
pub fn parse_naddr(naddr: &str) -> Result<PackAddress, String> {
    let trimmed = naddr.trim().trim_start_matches("nostr:");
    let parsed = nostr_sdk::nips::nip19::Nip19::from_bech32(trimmed)
        .map_err(|e| format!("invalid naddr: {}", e))?;
    let coord = match parsed {
        nostr_sdk::nips::nip19::Nip19::Coordinate(c) => c,
        _ => return Err("naddr expected (Nip19 was not a coordinate)".to_string()),
    };
    let kind = coord.kind.as_u16();
    if kind != KIND_EMOJI_SET {
        return Err(format!(
            "expected kind {} (emoji set), got {}",
            KIND_EMOJI_SET, kind,
        ));
    }
    Ok(PackAddress {
        kind,
        pubkey: coord.public_key,
        identifier: coord.identifier.clone(),
    })
}

/// Parse a NIP-51 inner tag list (the JSON array of tag tuples that
/// lives inside the NIP-44-encrypted `content` of an encrypted-items
/// list). Pulls out `a` tags as pack addresses; malformed inner
/// entries are dropped silently so one bad row doesn't nuke the list.
fn parse_inner_tag_list(plaintext: &str) -> Vec<PackAddress> {
    let inner: Vec<Vec<String>> = match serde_json::from_str(plaintext) {
        Ok(v) => v,
        Err(e) => {
            crate::log_warn!("[EmojiPacks] emoji list JSON parse failed: {}", e);
            return Vec::new();
        }
    };
    inner.into_iter()
        .filter_map(|tup| {
            if tup.len() >= 2 && tup[0] == "a" {
                parse_pack_address(&tup[1]).ok()
            } else {
                None
            }
        })
        .collect()
}

/// Decrypt + parse a kind 10030 event's encrypted subscription list.
///
/// Vector's emoji list is fully private by design — every `a` tag is
/// carried inside the NIP-44-self-encrypted `content`, never in the
/// public `tags` field. A list event with empty / undecryptable /
/// malformed content is treated as "no subscriptions" rather than
/// failing the whole refresh. Spec: NIP-51 "encrypted items" section.
pub async fn decrypt_subscribed_addresses(
    client: &Client,
    my_pk: &PublicKey,
    event: &Event,
) -> Vec<PackAddress> {
    if event.content.is_empty() {
        return Vec::new();
    }
    let signer = match client.signer().await {
        Ok(s) => s,
        Err(e) => {
            crate::log_warn!("[EmojiPacks] signer unavailable for emoji list decrypt: {}", e);
            return Vec::new();
        }
    };
    let plaintext = match signer.nip44_decrypt(my_pk, &event.content).await {
        Ok(p) => p,
        Err(e) => {
            crate::log_warn!("[EmojiPacks] emoji list decrypt failed: {}", e);
            return Vec::new();
        }
    };
    parse_inner_tag_list(&plaintext)
}

// ============================================================================
// DB persistence
// ============================================================================

pub fn save_pack(pack: &EmojiPack) -> Result<(), String> {
    let mut conn = crate::db::get_write_connection_guard_static()?;
    let tx = conn.transaction()
        .map_err(|e| format!("Failed to start tx: {}", e))?;

    let addr = pack.addr();
    tx.execute(
        "INSERT OR REPLACE INTO emoji_packs
            (addr, pubkey, identifier, title, image_url, description, is_own, updated_at, raw_event)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, '')",
        rusqlite::params![
            addr, pack.pubkey, pack.identifier,
            pack.title, pack.image_url, pack.description,
            pack.is_own as i32, pack.updated_at as i64,
        ],
    ).map_err(|e| format!("Failed to upsert pack: {}", e))?;

    // Replace the item set wholesale — kind 30030 is a replaceable event,
    // older shortcodes that disappeared from the new version must not
    // linger in our local mirror.
    tx.execute(
        "DELETE FROM emoji_pack_items WHERE pack_addr = ?1",
        rusqlite::params![addr],
    ).map_err(|e| format!("Failed to clear pack items: {}", e))?;

    for (pos, emoji) in pack.emojis.iter().enumerate() {
        tx.execute(
            "INSERT INTO emoji_pack_items (pack_addr, shortcode, url, sha256, position)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                addr, emoji.shortcode, emoji.url, emoji.sha256, pos as i64,
            ],
        ).map_err(|e| format!("Failed to insert pack item: {}", e))?;
    }

    tx.commit().map_err(|e| format!("Failed to commit pack: {}", e))?;
    Ok(())
}

/// Apply one refresh sweep's verdict to a pack's persisted health. Returns
/// `true` when the pack's STATUS changed (caller emits a UI refresh).
///
/// Rules:
/// - `Found` resets everything to active — self-healing from any state, even
///   revoked (the author republished, or a relay restored from backup).
/// - `Tombstoned` is deterministic: revoked immediately, no gauntlet.
/// - `CleanMiss` runs the gauntlet: the counter moves at most once per
///   [`PACK_MISS_RATELIMIT_SECS`], and promotion to missing requires BOTH
///   [`PACK_MISS_PROMOTE_COUNT`] misses and [`PACK_MISS_PROMOTE_SECS`] since
///   the first — so neither rapid restarts nor one long offline stretch can
///   false-positive. A revoked pack ignores misses (stronger evidence holds).
/// - `Unreachable` never moves anything in either direction.
///
/// On a transition INTO revoked/missing, the pack's emojis are purged from
/// the frequently-used engine so dead emojis stop being suggested.
pub fn apply_pack_health(addr: &str, outcome: &PackFetchOutcome, now: i64) -> Result<bool, String> {
    use rusqlite::OptionalExtension;
    if matches!(outcome, PackFetchOutcome::Unreachable) {
        return Ok(false);
    }
    // IMMEDIATE transaction: concurrent sweeps (boot trigger + panel refresh)
    // each get their own pooled connection, so an unserialized read-modify-write
    // could let a stale CleanMiss overwrite a just-written tombstone.
    let mut conn = crate::db::get_write_connection_guard_static()?;
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| format!("begin pack health tx: {}", e))?;

    let row = tx
        .query_row(
            "SELECT status, miss_count, first_missed_at, last_miss_counted_at
             FROM emoji_packs WHERE addr = ?1",
            rusqlite::params![addr],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            },
        )
        .optional()
        .map_err(|e| format!("read pack health: {}", e))?;
    let Some((status, miss_count, first_missed_at, last_miss_counted_at)) = row else {
        return Ok(false);
    };

    let (new_status, new_miss, new_first, new_last) = match outcome {
        PackFetchOutcome::Found => (PACK_STATUS_ACTIVE as i64, 0, 0, 0),
        PackFetchOutcome::Tombstoned => (PACK_STATUS_REVOKED as i64, 0, 0, 0),
        PackFetchOutcome::CleanMiss => {
            if status == PACK_STATUS_REVOKED as i64 {
                (status, miss_count, first_missed_at, last_miss_counted_at)
            } else if now - last_miss_counted_at < PACK_MISS_RATELIMIT_SECS {
                (status, miss_count, first_missed_at, last_miss_counted_at)
            } else {
                let first = if miss_count == 0 { now } else { first_missed_at };
                let count = miss_count + 1;
                let promoted = count >= PACK_MISS_PROMOTE_COUNT
                    && now - first >= PACK_MISS_PROMOTE_SECS;
                (
                    if promoted { PACK_STATUS_MISSING as i64 } else { status },
                    count,
                    first,
                    now,
                )
            }
        }
        PackFetchOutcome::Unreachable => unreachable!(),
    };

    let changed = new_status != status;
    tx.execute(
        "UPDATE emoji_packs SET status = ?2, miss_count = ?3, first_missed_at = ?4,
             last_miss_counted_at = ?5,
             status_changed_at = CASE WHEN status != ?2 THEN ?6 ELSE status_changed_at END
         WHERE addr = ?1",
        rusqlite::params![addr, new_status, new_miss, new_first, new_last, now],
    )
    .map_err(|e| format!("write pack health: {}", e))?;

    // Dead pack: retire its emojis from the frequently-used engine (kind 1 =
    // custom emoji rows, matched by image URL — shortcodes collide across packs).
    if changed && new_status != PACK_STATUS_ACTIVE as i64 {
        tx.execute(
            "DELETE FROM emoji_usage WHERE kind = 1 AND url IN
                 (SELECT url FROM emoji_pack_items WHERE pack_addr = ?1)",
            rusqlite::params![addr],
        )
        .map_err(|e| format!("purge dead pack usage: {}", e))?;
    }

    tx.commit().map_err(|e| format!("commit pack health: {}", e))?;
    Ok(changed)
}

pub fn save_subscriptions(addrs: &[String]) -> Result<(), String> {
    let mut conn = crate::db::get_write_connection_guard_static()?;
    let tx = conn.transaction()
        .map_err(|e| format!("Failed to start tx: {}", e))?;

    tx.execute("DELETE FROM emoji_pack_subscriptions", [])
        .map_err(|e| format!("Failed to clear subscriptions: {}", e))?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
    for addr in addrs {
        tx.execute(
            "INSERT OR REPLACE INTO emoji_pack_subscriptions (addr, subscribed_at)
             VALUES (?1, ?2)",
            rusqlite::params![addr, now],
        ).map_err(|e| format!("Failed to insert subscription: {}", e))?;
    }

    tx.commit().map_err(|e| format!("Failed to commit subscriptions: {}", e))?;
    Ok(())
}

pub fn load_subscriptions() -> Result<Vec<String>, String> {
    let conn = crate::db::get_db_connection_guard_static()?;
    let mut stmt = conn.prepare("SELECT addr FROM emoji_pack_subscriptions ORDER BY subscribed_at ASC")
        .map_err(|e| format!("prepare: {}", e))?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))
        .map_err(|e| format!("query: {}", e))?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r.map_err(|e| format!("row: {}", e))?);
    }
    Ok(out)
}

/// Load a single cached pack by its raw `kind:pubkey:identifier` addr,
/// regardless of subscription status. Used by the theme-pack path: a theme
/// pack is persisted via `save_pack` (so it loads instantly across sessions)
/// but never gets a subscription row, so `load_all_packs` rightly hides it.
pub fn load_cached_pack(addr: &str) -> Result<Option<EmojiPack>, String> {
    let conn = crate::db::get_db_connection_guard_static()?;

    let mut pack = match conn.query_row(
        "SELECT pubkey, identifier, title, image_url, description, is_own, updated_at, status
         FROM emoji_packs WHERE addr = ?1",
        rusqlite::params![addr],
        |row| {
            Ok(EmojiPack {
                id: naddr_from_addr(addr).unwrap_or_else(|_| addr.to_string()),
                pubkey: row.get(0)?,
                identifier: row.get(1)?,
                title: row.get(2)?,
                image_url: row.get(3)?,
                description: row.get(4)?,
                is_own: row.get::<_, i32>(5)? != 0,
                updated_at: row.get::<_, i64>(6)? as u64,
                emojis: Vec::new(),
                status: row.get::<_, i64>(7)? as u8,
            })
        },
    ) {
        Ok(p) => p,
        Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
        Err(e) => return Err(format!("query cached pack: {}", e)),
    };

    let mut stmt = conn.prepare(
        "SELECT shortcode, url, sha256 FROM emoji_pack_items
         WHERE pack_addr = ?1 ORDER BY position ASC"
    ).map_err(|e| format!("prepare items: {}", e))?;
    let rows = stmt.query_map(rusqlite::params![addr], |row| {
        Ok(PackEmoji {
            shortcode: row.get(0)?,
            url: row.get(1)?,
            sha256: row.get(2)?,
        })
    }).map_err(|e| format!("query items: {}", e))?;
    for r in rows {
        pack.emojis.push(r.map_err(|e| format!("row item: {}", e))?);
    }

    Ok(Some(pack))
}

/// Load every locally-cached pack the user is currently subscribed to
/// (plus their own packs, which always count). Hydrated with items.
/// Cached non-subscribed pack rows stay in the DB so historic reactions
/// still resolve their image URLs — they're just hidden from the picker.
pub fn load_all_packs() -> Result<Vec<EmojiPack>, String> {
    let conn = crate::db::get_db_connection_guard_static()?;

    let mut packs: HashMap<String, EmojiPack> = HashMap::new();
    let mut order: Vec<String> = Vec::new();

    {
        // INNER JOIN — only show subscribed packs. Own packs are
        // auto-subscribed when published (see `publish_pack`), so this
        // surfaces them by default; if the user explicitly unsubscribes
        // their own pack via the right-click "Remove" path, it drops out
        // of the picker but stays on Nostr and in `emoji_packs` so a
        // later re-subscribe (paste naddr) restores it with `is_own` set.
        let mut stmt = conn.prepare(
            "SELECT p.addr, p.pubkey, p.identifier, p.title, p.image_url, p.description, p.is_own, p.updated_at, p.status
             FROM emoji_packs p
             INNER JOIN emoji_pack_subscriptions s ON s.addr = p.addr
             ORDER BY p.is_own DESC, p.updated_at DESC"
        ).map_err(|e| format!("prepare packs: {}", e))?;

        let rows = stmt.query_map([], |row| {
            // Row col 0 is the raw addr (kind:pubkey:identifier). Encode
            // to naddr here so the public `id` field is consistent with
            // what `parse_pack_from_event` produces.
            let raw_addr: String = row.get(0)?;
            let id = naddr_from_addr(&raw_addr).unwrap_or(raw_addr.clone());
            Ok((raw_addr, EmojiPack {
                id,
                pubkey: row.get(1)?,
                identifier: row.get(2)?,
                title: row.get(3)?,
                image_url: row.get(4)?,
                description: row.get(5)?,
                is_own: row.get::<_, i32>(6)? != 0,
                updated_at: row.get::<_, i64>(7)? as u64,
                emojis: Vec::new(),
                status: row.get::<_, i64>(8)? as u8,
            }))
        }).map_err(|e| format!("query packs: {}", e))?;

        for r in rows {
            let (raw_addr, pack) = r.map_err(|e| format!("row pack: {}", e))?;
            order.push(raw_addr.clone());
            packs.insert(raw_addr, pack);
        }
    }

    {
        let mut stmt = conn.prepare(
            "SELECT pack_addr, shortcode, url, sha256
             FROM emoji_pack_items
             ORDER BY pack_addr, position ASC"
        ).map_err(|e| format!("prepare items: {}", e))?;

        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                PackEmoji {
                    shortcode: row.get(1)?,
                    url: row.get(2)?,
                    sha256: row.get(3)?,
                },
            ))
        }).map_err(|e| format!("query items: {}", e))?;

        for r in rows {
            let (addr, emoji) = r.map_err(|e| format!("row item: {}", e))?;
            if let Some(p) = packs.get_mut(&addr) {
                p.emojis.push(emoji);
            }
        }
    }

    Ok(order.into_iter().filter_map(|a| packs.remove(&a)).collect())
}

// ============================================================================
// Author outbox (NIP-65)
// ============================================================================

/// How long a cached author relay list stays valid before re-fetching.
/// NIP-65 lists rarely change (relay-set edits are a deliberate user act);
/// an hour amortises the overhead without serving routing that's egregiously
/// stale. Mirrors the TTL the NIP-17 inbox cache uses.
const NIP65_CACHE_TTL_SECS: u64 = 3600;
/// Shorter TTL after an empty/failed lookup so a transient relay blip doesn't
/// suppress outbox routing for a whole hour.
const NIP65_CACHE_TTL_ERROR_SECS: u64 = 60;
const NIP65_FETCH_TIMEOUT_SECS: u64 = 10;

#[derive(Clone)]
struct CachedRelayList {
    relays: Vec<RelayUrl>,
    fetched_at: std::time::Instant,
    /// Empty fetches use the shorter TTL so transient outages recover fast.
    empty: bool,
    /// True when the entry came from a SUCCESSFUL kind-10002 fetch. An
    /// error-cached placeholder (kept so previews don't refetch-storm) must
    /// never read as "author verifiably has no relays" to the health sweep.
    verified: bool,
}

static NIP65_CACHE: std::sync::OnceLock<std::sync::RwLock<HashMap<PublicKey, CachedRelayList>>> =
    std::sync::OnceLock::new();

fn nip65_cache() -> &'static std::sync::RwLock<HashMap<PublicKey, CachedRelayList>> {
    NIP65_CACHE.get_or_init(|| std::sync::RwLock::new(HashMap::new()))
}

/// Read fresh write relays for `pubkey` from the cache, or `None` if absent /
/// expired. Honours the dual TTL (short for empty entries).
fn cached_write_relays(pubkey: &PublicKey) -> Option<Vec<RelayUrl>> {
    let cache = nip65_cache().read().ok()?;
    let entry = cache.get(pubkey)?;
    let ttl = if entry.empty { NIP65_CACHE_TTL_ERROR_SECS } else { NIP65_CACHE_TTL_SECS };
    if entry.fetched_at.elapsed() < std::time::Duration::from_secs(ttl) {
        Some(entry.relays.clone())
    } else {
        None
    }
}

/// Like [`cached_write_relays`], but only entries learned from a SUCCESSFUL
/// kind-10002 fetch. Judging a pack's absence needs its canonical home to
/// have really been resolved; an error placeholder must read as unknown.
fn cached_write_relays_verified(pubkey: &PublicKey) -> Option<Vec<RelayUrl>> {
    let cache = nip65_cache().read().ok()?;
    let entry = cache.get(pubkey)?;
    if !entry.verified {
        return None;
    }
    let ttl = if entry.empty { NIP65_CACHE_TTL_ERROR_SECS } else { NIP65_CACHE_TTL_SECS };
    if entry.fetched_at.elapsed() < std::time::Duration::from_secs(ttl) {
        Some(entry.relays.clone())
    } else {
        None
    }
}

/// Drop every cached relay list (account swap — entries hold contact-graph metadata
/// from the prior session, mirroring the inbox-relay cache's swap hygiene).
pub fn clear_nip65_cache() {
    if let Ok(mut cache) = nip65_cache().write() {
        cache.clear();
    }
}

/// Store a freshly-resolved write-relay list for `pubkey` in the cache.
fn cache_write_relays(pubkey: PublicKey, relays: Vec<RelayUrl>, verified: bool) {
    if let Ok(mut cache) = nip65_cache().write() {
        let empty = relays.is_empty();
        cache.insert(pubkey, CachedRelayList {
            relays,
            fetched_at: std::time::Instant::now(),
            empty,
            verified,
        });
    }
}

/// Extract the write relays from a kind-10002 event. NIP-65: marker absent =
/// read+write (both), "write" = author publishes here, "read"-only = consumes
/// only (useless for finding their packs), so we keep both/write and drop read.
fn extract_write_relays(ev: &Event) -> Vec<RelayUrl> {
    let mut relays: Vec<RelayUrl> = Vec::new();
    for (url, marker) in nostr_sdk::nips::nip65::extract_relay_list(ev) {
        match marker {
            None | Some(nostr_sdk::nips::nip65::RelayMetadata::Write) => {
                if !relays.contains(url) {
                    relays.push(url.clone());
                }
            }
            Some(nostr_sdk::nips::nip65::RelayMetadata::Read) => {}
        }
    }
    relays
}

/// Resolve the author's NIP-65 (kind-10002) write relays — where they publish.
/// Returns an empty Vec on absence or fetch error; callers must treat absence
/// as "no extra hints," not a failure. Cached per-pubkey. Used by the
/// single-pack path; the batched list path uses `prefetch_author_write_relays`.
async fn fetch_author_write_relays(client: &Client, pubkey: PublicKey) -> Vec<RelayUrl> {
    if let Some(relays) = cached_write_relays(&pubkey) {
        return relays;
    }

    let filter = Filter::new()
        .author(pubkey)
        .kind(Kind::RelayList)
        .limit(1);
    let events = match client
        .fetch_events(filter, std::time::Duration::from_secs(NIP65_FETCH_TIMEOUT_SECS))
        .await
    {
        Ok(evs) => evs,
        Err(_) => {
            cache_write_relays(pubkey, Vec::new(), false);
            return Vec::new();
        }
    };

    let relays = events.into_iter()
        .max_by_key(|e| e.created_at)
        .map(|ev| extract_write_relays(&ev))
        .unwrap_or_default();
    cache_write_relays(pubkey, relays.clone(), true);
    relays
}

/// Warm the NIP-65 cache for many authors in ONE request. Used by the batched
/// subscribed-list path so a boot with N federated packs pays a single
/// kind-10002 fetch instead of N. Authors already cached-fresh are skipped;
/// authors with no kind-10002 are cached empty (short TTL) so we don't refetch
/// them every pass.
async fn prefetch_author_write_relays(client: &Client, authors: &[PublicKey]) {
    let uncached: Vec<PublicKey> = authors.iter()
        .filter(|pk| cached_write_relays(pk).is_none())
        .copied()
        .collect();
    if uncached.is_empty() {
        return;
    }

    let filter = Filter::new()
        .authors(uncached.iter().copied())
        .kind(Kind::RelayList);
    let events = match client
        .fetch_events(filter, std::time::Duration::from_secs(NIP65_FETCH_TIMEOUT_SECS))
        .await
    {
        Ok(evs) => evs,
        // On error, leave the cache cold — the next pass retries rather than
        // poisoning every author with an empty entry off one failed batch.
        Err(_) => return,
    };

    // Keep the newest kind-10002 per author, then cache each.
    let mut newest: HashMap<PublicKey, Event> = HashMap::new();
    for ev in events {
        match newest.get(&ev.pubkey) {
            Some(existing) if existing.created_at >= ev.created_at => {}
            _ => { newest.insert(ev.pubkey, ev); }
        }
    }
    for pk in uncached {
        let relays = newest.get(&pk).map(extract_write_relays).unwrap_or_default();
        cache_write_relays(pk, relays, true);
    }
}

// ============================================================================
// Relay fetch
// ============================================================================

/// Resolve a single pack from relays by its parsed address. Returns
/// `None` when no matching event is found within the timeout — callers
/// distinguish "unknown" from "fetch error" by the caller's own error
/// pathway (every relay call here that errors logs and proceeds).
async fn fetch_pack_from_relays(client: &Client, addr: &PackAddress) -> Option<EmojiPack> {
    let filter = Filter::new()
        .author(addr.pubkey)
        .kind(Kind::Custom(KIND_EMOJI_SET))
        .identifier(&addr.identifier)
        .limit(1);
    let timeout = std::time::Duration::from_secs(FETCH_TIMEOUT_SECS);
    let me = crate::state::my_public_key().map(|pk| pk.to_hex());

    // 1) Home relays first (the shared pool). Covers our own packs and any
    //    pack that's on Vector's default relays — the common, fast case.
    match client.fetch_events(filter.clone(), timeout).await {
        Ok(events) => {
            if let Some(ev) = events.into_iter().max_by_key(|e| e.created_at) {
                if let Some(pack) = parse_pack_from_event(&ev, me.as_deref()) {
                    return Some(pack);
                }
            }
        }
        Err(e) => crate::log_warn!("[EmojiPacks] home fetch {} failed: {}", &addr.identifier, e),
    }

    // 2) Outbox fallback (NIP-65): the pack lives wherever the creator
    //    publishes, which may sit outside our relays. Fetch through an
    //    ISOLATED throwaway client so these third-party relays never enter
    //    the shared pool — the DM/community sync loops enumerate that pool and
    //    would otherwise reconcile against every pack author's relays.
    let outbox = fetch_author_write_relays(client, addr.pubkey).await;
    if outbox.is_empty() {
        return None;
    }
    fetch_pack_via_isolated_client(&outbox, filter, timeout, me.as_deref()).await
}

/// Fetch a kind-30030 pack through a dedicated, short-lived client connected
/// only to the given relays. Built with Tor-aware options; fully torn down
/// before returning so nothing leaks into the app's relay pool or sync loops.
async fn fetch_pack_via_isolated_client(
    relays: &[RelayUrl],
    filter: Filter,
    timeout: std::time::Duration,
    my_pubkey_hex: Option<&str>,
) -> Option<EmojiPack> {
    let scratch = ClientBuilder::new()
        .opts(crate::nostr_client_options())
        .build();
    for r in relays {
        let opts = crate::tor_aware_relay_options(RelayOptions::new().reconnect(false));
        let _ = scratch.pool().add_relay(r.as_str(), opts).await;
    }
    scratch.connect().await;

    let result = scratch.fetch_events(filter, timeout).await;
    // Tear the scratch client down regardless of outcome.
    scratch.shutdown().await;

    let events = match result {
        Ok(events) => events,
        Err(e) => {
            crate::log_warn!("[EmojiPacks] outbox fetch failed: {}", e);
            return None;
        }
    };
    let event = events.into_iter().max_by_key(|e| e.created_at)?;
    parse_pack_from_event(&event, my_pubkey_hex)
}

// ============================================================================
// Batched relay fetch (subscribed-list path ONLY)
// ============================================================================
//
// `fetch_pack_from_relays` (above) resolves ONE pack and is used by the
// per-pack flows: in-chat preview cards and the pinned theme pack, which
// arrive as independent render events and must stay independent.
//
// `fetch_packs_from_relays` (below) resolves MANY packs whose coordinates are
// all known up front — i.e. the user's own subscribed list. It collapses what
// used to be N requests into one batched home request plus, for any packs not
// on our relays, one batched NIP-65 prefetch and one batched outbox request.
// These two paths intentionally do NOT share fetch logic: the single path is
// kept byte-stable so the preview/theme behaviour can't regress.

/// Coordinate key for matching a kind-30030 event back to a requested pack:
/// `pubkey_hex:identifier`. (Not the `30030:`-prefixed addr — just the parts a
/// fetched event exposes via its author + `d` tag.)
fn event_coord(ev: &Event) -> Option<String> {
    let d = first_tag(&ev.tags, &["d"])?;
    Some(format!("{}:{}", ev.pubkey.to_hex(), d))
}
fn addr_coord(addr: &PackAddress) -> String {
    format!("{}:{}", addr.pubkey.to_hex(), addr.identifier)
}

/// One batched filter matches the cross-product of authors × identifiers, so it
/// can return events we didn't ask for (author A's `d` that belongs to author
/// B's pack). Match strictly by exact coordinate and keep the newest event per
/// coordinate; strays are dropped. Returns raw EVENTS (not parsed packs): an
/// empty kind 30030 is a deletion tombstone, and parsing would erase exactly
/// that evidence.
fn newest_events_by_coord(
    events: impl IntoIterator<Item = Event>,
    wanted: &std::collections::HashSet<String>,
) -> HashMap<String, Event> {
    let mut newest: HashMap<String, Event> = HashMap::new();
    for ev in events {
        if ev.kind.as_u16() != KIND_EMOJI_SET { continue; }
        let Some(coord) = event_coord(&ev) else { continue; };
        if !wanted.contains(&coord) { continue; }
        match newest.get(&coord) {
            Some(existing) if existing.created_at >= ev.created_at => {}
            _ => { newest.insert(coord, ev); }
        }
    }
    newest
}

/// Merge a phase's newest-events into the accumulator, keeping the newer of
/// the two per coordinate.
fn merge_newest(acc: &mut HashMap<String, Event>, phase: HashMap<String, Event>) {
    for (coord, ev) in phase {
        match acc.get(&coord) {
            Some(existing) if existing.created_at >= ev.created_at => {}
            _ => { acc.insert(coord, ev); }
        }
    }
}

/// Batched NIP-09 deletion filter for the given packs: author-signed kind 5s
/// that cite a pack coordinate in an `a` tag.
fn deletion_filter(addrs: &[&PackAddress]) -> Filter {
    Filter::new()
        .authors(addrs.iter().map(|a| a.pubkey))
        .kind(Kind::EventDeletion)
        .custom_tags(
            SingleLetterTag::lowercase(Alphabet::A),
            addrs.iter().map(|a| a.to_addr_string()),
        )
}

/// Does this kind-5, signed by the pack's author, cite the pack's coordinate?
/// Third-party deletions never count — only the author may revoke.
fn deletion_matches(ev: &Event, author: &PublicKey, raw_addr: &str) -> bool {
    ev.kind == Kind::EventDeletion
        && ev.pubkey == *author
        && ev.tags.iter().any(|t| {
            let s = t.as_slice();
            s.len() >= 2 && s[0] == "a" && s[1] == raw_addr
        })
}

/// URLs of relays that are CONNECTED and readable right now — the basis for
/// judging whether an absence was observed against live relays or thin air.
/// READ-flagged only for the main pool (fetches never touch write-only or
/// GOSSIP-isolated community relays, so those must not inflate the count).
async fn connected_read_relays(client: &Client, read_only: bool) -> std::collections::HashSet<String> {
    client
        .relays()
        .await
        .iter()
        .filter(|(_, r)| r.status() == RelayStatus::Connected && (!read_only || r.flags().has_read()))
        .map(|(url, _)| url.to_string())
        .collect()
}

/// Is a pack's ABSENCE from this sweep judgeable as a clean miss? Pure so the
/// gate combinations are table-testable.
///
/// - The home fetch must have completed against relays that were connected
///   both before AND after it ran (`connect()` is non-blocking, so an
///   after-only sample would bless a fetch that raced an empty pool).
/// - At least two distinct live relays across the phases.
/// - The author's NIP-65 write relays are the pack's canonical home:
///   unknown (cache cold / fetch failed) means it was never really checked —
///   not clean. Verifiably EMPTY means home evidence alone is the best
///   anyone can do. Known means the outbox phase must have completed there.
fn absence_is_clean(
    home_ok: bool,
    live_relays: usize,
    author_writes: Option<&Vec<RelayUrl>>,
    outbox_ok: bool,
    outbox_live: usize,
) -> bool {
    if !home_ok || live_relays < 2 {
        return false;
    }
    match author_writes {
        None => false,
        Some(w) if w.is_empty() => true,
        Some(_) => outbox_ok && outbox_live >= 1,
    }
}

/// Classify one pack from a sweep's evidence. Pure so the verdict table is
/// unit-testable. Returns the parsed pack alongside `Found`.
fn classify_pack(
    newest_ev: Option<&Event>,
    newest_deletion: Option<&Event>,
    me: Option<&str>,
    absence_clean: bool,
) -> (PackFetchOutcome, Option<EmojiPack>) {
    match newest_ev {
        Some(ev) => match parse_pack_from_event(ev, me) {
            Some(pack) => {
                // A republish NEWER than the deletion revives the pack; on an
                // equal timestamp the pack wins (fail-safe toward alive).
                match newest_deletion {
                    Some(del) if del.created_at > ev.created_at => {
                        (PackFetchOutcome::Tombstoned, None)
                    }
                    _ => (PackFetchOutcome::Found, Some(pack)),
                }
            }
            None => {
                // No `emoji` tags at all: the author replaced the pack with an
                // empty one — Vector's own delete flow publishes exactly this
                // tombstone. An event that HAS emoji tags Vector merely can't
                // validate is not a deletion; judge nothing from it.
                let has_emoji_tags = ev.tags.iter().any(|t| {
                    t.as_slice().first().map(|k| k == "emoji").unwrap_or(false)
                });
                if has_emoji_tags {
                    (PackFetchOutcome::Unreachable, None)
                } else {
                    (PackFetchOutcome::Tombstoned, None)
                }
            }
        },
        None if newest_deletion.is_some() => (PackFetchOutcome::Tombstoned, None),
        None if absence_clean => (PackFetchOutcome::CleanMiss, None),
        None => (PackFetchOutcome::Unreachable, None),
    }
}

/// Everything one refresh sweep learned: resolved packs (in `addrs` order)
/// plus a per-addr verdict for the health engine.
struct PackSweep {
    packs: Vec<EmojiPack>,
    /// raw `kind:pubkey:identifier` addr → verdict.
    outcomes: HashMap<String, PackFetchOutcome>,
}

/// Resolve MANY packs in a batch and judge the ones that didn't resolve.
/// Home relays in one request; any unresolved packs then get one batched
/// NIP-65 prefetch + one batched outbox request via an isolated client, with
/// author-signed kind-5 deletions fetched alongside each phase. Callers keep
/// cached copies for anything that isn't `Found`.
async fn sweep_packs_from_relays(client: &Client, addrs: &[PackAddress]) -> PackSweep {
    if addrs.is_empty() {
        return PackSweep { packs: Vec::new(), outcomes: HashMap::new() };
    }
    let timeout = std::time::Duration::from_secs(FETCH_TIMEOUT_SECS);
    let me = crate::state::my_public_key().map(|pk| pk.to_hex());
    let wanted: std::collections::HashSet<String> = addrs.iter().map(addr_coord).collect();
    let all_refs: Vec<&PackAddress> = addrs.iter().collect();

    // 1) One batched home request for every subscribed pack (+ deletions).
    // Live-relay evidence is the INTERSECTION of connected READ relays before
    // and after the fetch: `connect()` is non-blocking, so an after-only
    // sample would bless a fetch that actually ran against an empty pool
    // (boot, Tor cold start) — the exact false-positive the gauntlet must
    // never produce.
    let home_before = connected_read_relays(client, true).await;
    let home_filter = Filter::new()
        .authors(addrs.iter().map(|a| a.pubkey))
        .kind(Kind::Custom(KIND_EMOJI_SET))
        .identifiers(addrs.iter().map(|a| a.identifier.clone()));
    let mut newest: HashMap<String, Event> = HashMap::new();
    let mut deletions: Vec<Event> = Vec::new();
    let mut home_ok = false;
    match client.fetch_events(home_filter, timeout).await {
        Ok(events) => {
            home_ok = true;
            merge_newest(&mut newest, newest_events_by_coord(events, &wanted));
        }
        Err(e) => {
            crate::log_warn!("[EmojiPacks] batched home fetch failed: {}", e);
        }
    }
    if home_ok {
        match client.fetch_events(deletion_filter(&all_refs), timeout).await {
            Ok(events) => deletions.extend(events),
            Err(e) => crate::log_warn!("[EmojiPacks] home deletion fetch failed: {}", e),
        }
    }
    let home_after = connected_read_relays(client, true).await;
    let mut live_relays: std::collections::HashSet<String> =
        home_before.intersection(&home_after).cloned().collect();

    // 2) Outbox fallback for the misses, all in one shot. A pack's canonical
    // home is its author's NIP-65 write relays, so absence THERE is the
    // signal that matters for the health verdict.
    let misses: Vec<&PackAddress> = addrs.iter()
        .filter(|a| !newest.contains_key(&addr_coord(a)))
        .collect();
    let mut outbox_ok = false;
    let mut outbox_live = 0usize;
    if !misses.is_empty() {
        let miss_authors: Vec<PublicKey> = {
            let mut v: Vec<PublicKey> = misses.iter().map(|a| a.pubkey).collect();
            v.sort(); v.dedup();
            v
        };
        // Warm NIP-65 for all missed authors in one request, then union their
        // write relays into a single isolated client + one batched request.
        prefetch_author_write_relays(client, &miss_authors).await;
        let mut outbox: Vec<RelayUrl> = Vec::new();
        for pk in &miss_authors {
            for r in cached_write_relays(pk).unwrap_or_default() {
                if !outbox.contains(&r) { outbox.push(r); }
            }
        }
        if !outbox.is_empty() {
            let miss_filter = Filter::new()
                .authors(misses.iter().map(|a| a.pubkey))
                .kind(Kind::Custom(KIND_EMOJI_SET))
                .identifiers(misses.iter().map(|a| a.identifier.clone()));
            let wanted_misses: std::collections::HashSet<String> =
                misses.iter().map(|a| addr_coord(a)).collect();
            if let Some((events, dels, live)) = sweep_via_isolated_client(
                &outbox, miss_filter, deletion_filter(&misses), timeout,
            ).await {
                outbox_ok = true;
                outbox_live = live.len();
                live_relays.extend(live);
                merge_newest(&mut newest, newest_events_by_coord(events, &wanted_misses));
                deletions.extend(dels);
            }
        }
    }

    // 3) Classify every pack (pure helpers; see their docs for the rules).
    let mut packs: Vec<EmojiPack> = Vec::new();
    let mut outcomes: HashMap<String, PackFetchOutcome> = HashMap::new();
    for addr in addrs {
        let coord = addr_coord(addr);
        let raw_addr = addr.to_addr_string();
        let newest_deletion = deletions.iter()
            .filter(|d| deletion_matches(d, &addr.pubkey, &raw_addr))
            .max_by_key(|d| d.created_at);
        let author_writes = cached_write_relays_verified(&addr.pubkey);
        let clean = absence_is_clean(
            home_ok,
            live_relays.len(),
            author_writes.as_ref(),
            outbox_ok,
            outbox_live,
        );
        let (outcome, pack) = classify_pack(
            newest.get(&coord),
            newest_deletion,
            me.as_deref(),
            clean,
        );
        if let Some(p) = pack {
            packs.push(p);
        }
        outcomes.insert(raw_addr, outcome);
    }

    PackSweep { packs, outcomes }
}

/// Batched sibling of `fetch_pack_via_isolated_client`: fetch pack events and
/// their kind-5 deletions from a throwaway client connected to the given
/// relays. Returns `None` when the pack fetch itself errored (the sweep must
/// not judge absences it never observed), plus the URLs of relays that were
/// connected across the fetch (before ∩ after — `connect()` is non-blocking,
/// so a fetch can race an empty pool) for the connectivity gate.
async fn sweep_via_isolated_client(
    relays: &[RelayUrl],
    pack_filter: Filter,
    del_filter: Filter,
    timeout: std::time::Duration,
) -> Option<(Vec<Event>, Vec<Event>, std::collections::HashSet<String>)> {
    let scratch = ClientBuilder::new()
        .opts(crate::nostr_client_options())
        .build();
    for r in relays {
        let opts = crate::tor_aware_relay_options(RelayOptions::new().reconnect(false));
        let _ = scratch.pool().add_relay(r.as_str(), opts).await;
    }
    scratch.connect().await;
    // `connect()` is non-blocking and this client is brand new, so wait for at
    // least one handshake to complete (bounded) before sampling connectivity —
    // an instant sample reads empty on every run, which would make before ∩
    // after a tautological zero and outbox absences permanently unjudgeable.
    let connect_deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
    let before = loop {
        let connected = connected_read_relays(&scratch, false).await;
        if !connected.is_empty() || std::time::Instant::now() >= connect_deadline {
            break connected;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    };
    let result = scratch.fetch_events(pack_filter, timeout).await;
    let dels = match &result {
        Ok(_) => scratch.fetch_events(del_filter, timeout).await.ok(),
        Err(_) => None,
    };
    let after = connected_read_relays(&scratch, false).await;
    scratch.shutdown().await;

    match result {
        Ok(events) => Some((
            events.into_iter().collect(),
            dels.map(|d| d.into_iter().collect()).unwrap_or_default(),
            before.intersection(&after).cloned().collect(),
        )),
        Err(e) => {
            crate::log_warn!("[EmojiPacks] batched outbox fetch failed: {}", e);
            None
        }
    }
}

/// Fetch the user's kind 10030 list, resolve every referenced pack, and
/// persist the result locally. Session-guarded against an account swap
/// landing the new account's pack list in account A's DB.
///
/// Non-destructive: a missing kind 10030 event or a transient per-pack
/// fetch failure must NOT nuke the user's local subscription list — that
/// would wipe their picker on every relay blip. Cached pack data is the
/// fallback whenever a fresh fetch fails.
pub async fn fetch_subscribed_packs(
    client: &Client,
    my_pubkey: PublicKey,
    session: crate::state::SessionGuard,
) -> Result<Vec<EmojiPack>, String> {
    let list_filter = Filter::new()
        .author(my_pubkey)
        .kind(Kind::Custom(KIND_EMOJI_LIST))
        .limit(1);

    let list_events = client
        .fetch_events(list_filter, std::time::Duration::from_secs(FETCH_TIMEOUT_SECS))
        .await
        .map_err(|e| format!("fetch kind 10030: {}", e))?;

    if !session.is_valid() {
        return Ok(Vec::new());
    }

    // Source-of-truth selection. If relays returned a kind 10030, trust
    // its `a` tags as the canonical subscription set — UNLESS it predates
    // our own last publish, which means our latest republish hasn't
    // propagated yet and the relay is still serving a stale list. Trusting
    // a stale list would clobber a just-added pack (last-write-wins by
    // created_at). If relays returned *nothing*, that's a transient sync
    // gap — fall back to the local mirror either way.
    let local_addrs = || -> Vec<PackAddress> {
        load_subscriptions()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|s| parse_pack_address(&s).ok())
            .collect()
    };
    let our_last_publish: u64 = crate::db::settings::get_sql_setting(EMOJI_LIST_PUBLISHED_AT_KEY.to_string())
        .ok()
        .flatten()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    let list_event = list_events.into_iter().max_by_key(|e| e.created_at);
    let addrs: Vec<PackAddress> = match list_event {
        Some(ev) if ev.created_at.as_secs() < our_last_publish => {
            crate::log_debug!(
                "[EmojiPacks] fetched kind 10030 (created_at {}) predates our publish ({}) — keeping local subs",
                ev.created_at.as_secs(), our_last_publish,
            );
            local_addrs()
        }
        Some(ev) => decrypt_subscribed_addresses(client, &my_pubkey, &ev).await,
        None => {
            crate::log_debug!(
                "[EmojiPacks] kind 10030 not on relays — refreshing local subs only",
            );
            local_addrs()
        }
    };

    let addr_strings: Vec<String> = addrs.iter().map(|a| a.to_addr_string()).collect();

    // Batched resolve: one home request for every subscribed pack, plus one
    // batched outbox pass for any not on our relays. Packs that still don't
    // resolve keep their cached copy (we never shrink the subscription set on
    // a transient miss); the health engine below decides whether an absence
    // is judgeable at all. The per-pack `fetch_pack_from_relays` is reserved
    // for the independent preview/theme flows.
    let PackSweep { packs: fresh, outcomes } = sweep_packs_from_relays(client, &addrs).await;
    let tombstoned = outcomes.values().filter(|o| matches!(o, PackFetchOutcome::Tombstoned)).count();
    let unresolved = addrs.len().saturating_sub(fresh.len()).saturating_sub(tombstoned);
    if unresolved > 0 {
        crate::log_warn!(
            "[EmojiPacks] {} subscribed pack(s) not on relays — keeping cached copies",
            unresolved,
        );
    }
    if tombstoned > 0 {
        crate::log_info!("[EmojiPacks] {} subscribed pack(s) deleted by their creator", tombstoned);
    }

    if !session.is_valid() {
        return Ok(fresh);
    }

    // Health verdicts BEFORE the save loop: save_pack's REPLACE resets the
    // health columns, so `Found` must read the pre-save status or a revival
    // (revoked back to active) would go unreported. Own packs are skipped:
    // deleting an own pack removes its rows locally, so there's nothing to
    // grieve, and a self-authored pack must never grey out over relay state.
    let me_hex = my_pubkey.to_hex();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let mut health_changed = false;
    for addr in &addrs {
        if addr.pubkey.to_hex() == me_hex { continue; }
        let raw = addr.to_addr_string();
        let Some(outcome) = outcomes.get(&raw) else { continue; };
        match apply_pack_health(&raw, outcome, now) {
            Ok(true) => {
                health_changed = true;
                crate::log_info!(
                    "[EmojiPacks] pack `{}` health changed ({:?})",
                    addr.identifier, outcome,
                );
            }
            Ok(false) => {}
            Err(e) => crate::log_warn!("[EmojiPacks] health update for `{}` failed: {}", addr.identifier, e),
        }
    }
    for pack in &fresh {
        if let Err(e) = save_pack(pack) {
            crate::log_warn!("[EmojiPacks] save pack {} failed: {}", pack.identifier, e);
        }
    }
    if health_changed {
        crate::traits::emit_event("emoji_packs_updated", &());
    }
    // Persist the full subscription list (10030-driven, or local-mirror
    // when 10030 was missing). Per-pack fetch failures don't shrink it —
    // the user is still subscribed, they just have a cached copy for now.
    if let Err(e) = save_subscriptions(&addr_strings) {
        crate::log_warn!("[EmojiPacks] save subscriptions failed: {}", e);
    }

    crate::log_info!(
        "[EmojiPacks] Resolved {} of {} subscribed pack(s){}",
        fresh.len(),
        addrs.len(),
        if unresolved > 0 {
            format!(" ({} via cache)", unresolved)
        } else {
            String::new()
        },
    );

    // Return the unified view: freshly-fetched packs overlay the cached
    // ones, and load_all_packs filters to subscribed-only for us.
    load_all_packs()
}

/// Convenience entry point that grabs the client + my_pubkey internally
/// and runs the full subscribed-packs refresh. Intended for the boot
/// path; in-app commands pass an explicit `SessionGuard` via the lower
/// helper to make the safety contract visible at every call site.
pub async fn refresh_subscribed_packs() -> Result<Vec<EmojiPack>, String> {
    let client = nostr_client().ok_or_else(|| "Nostr client not initialised".to_string())?;
    let me = crate::state::my_public_key().ok_or_else(|| "Not logged in".to_string())?;
    let session = crate::state::SessionGuard::capture();
    fetch_subscribed_packs(&client, me, session).await
}

/// Preview-only fetch by naddr — resolves + parses but never touches
/// local DB. Lets the UI render a "Pack Preview" card without committing
/// to a subscription.
pub async fn fetch_pack_by_naddr(naddr: &str) -> Result<EmojiPack, String> {
    let addr = parse_naddr(naddr)?;
    let client = nostr_client().ok_or_else(|| "Nostr client not initialised".to_string())?;
    fetch_pack_from_relays(&client, &addr).await
        .ok_or_else(|| format!("Pack not found on any relay: {}:{}", addr.pubkey.to_hex(), addr.identifier))
}

/// Resolve a theme pack cache-first: return the locally-persisted copy
/// instantly when present (and refresh it in the background), otherwise fetch
/// live, persist, and return. Theme packs are pinned by the active theme, not
/// subscribed — `save_pack` persists their data without a subscription row, so
/// they survive restarts (no per-session relay round-trip) yet never occupy an
/// equip slot or land in the kind-10030 list. Returns `None` if uncached and
/// the live fetch finds nothing.
pub async fn get_or_fetch_theme_pack(naddr: &str) -> Result<Option<EmojiPack>, String> {
    let addr = parse_naddr(naddr)?;
    let coord = addr.to_addr_string();

    // Cache hit: return immediately, refresh in the background so a later
    // creator-side edit still propagates without blocking first paint.
    if let Some(cached) = load_cached_pack(&coord)? {
        let naddr_owned = naddr.to_string();
        let session = crate::state::SessionGuard::capture();
        tokio::spawn(async move {
            if !session.is_valid() { return; }
            let Some(client) = nostr_client() else { return };
            if let Ok(parsed) = parse_naddr(&naddr_owned) {
                if let Some(fresh) = fetch_pack_from_relays(&client, &parsed).await {
                    if session.is_valid() && fresh.updated_at > cached.updated_at {
                        if let Err(e) = save_pack(&fresh) {
                            crate::log_warn!("[EmojiPacks] theme pack refresh save failed: {}", e);
                        } else {
                            crate::traits::emit_event("emoji_packs_updated", &());
                        }
                    }
                }
            }
        });
        return Ok(Some(cached));
    }

    // Cache miss: fetch live, persist for next session, return.
    let session = crate::state::SessionGuard::capture();
    let client = nostr_client().ok_or_else(|| "Nostr client not initialised".to_string())?;
    match fetch_pack_from_relays(&client, &addr).await {
        Some(pack) => {
            // Still show the pack this session, but only persist if the account
            // didn't swap during the fetch — otherwise we'd write into the wrong
            // account's DB.
            if session.is_valid() {
                if let Err(e) = save_pack(&pack) {
                    crate::log_warn!("[EmojiPacks] theme pack cache save failed: {}", e);
                }
            }
            Ok(Some(pack))
        }
        None => Ok(None),
    }
}

/// Publish a kind 10030 "Emojis" list containing every subscribed pack.
///
/// Encrypted-items mode: the entire subscription set lives inside a
/// NIP-44-self-encrypted JSON array of `["a", "30030:pk:d"]` tuples
/// stored in `content`. The event's public `tags` field is left empty
/// — Vector treats which packs a user follows as private information,
/// matching the NIP-51 "encrypted items" pattern that mute lists use.
/// Replaceable per spec, so peers (the same npub on another device)
/// always read the freshest set on next sync.
pub async fn publish_emoji_list(client: &Client) -> Result<(), String> {
    let addrs = load_subscriptions()?;
    let my_pk = crate::state::my_public_key()
        .ok_or_else(|| "Not logged in".to_string())?;

    let inner_tags: Vec<Vec<String>> = addrs.iter()
        .map(|addr| vec!["a".to_string(), addr.clone()])
        .collect();
    let plaintext = serde_json::to_string(&inner_tags)
        .map_err(|e| format!("Serialise emoji list: {}", e))?;

    let signer = client.signer().await
        .map_err(|e| format!("Signer unavailable: {}", e))?;
    let content = signer.nip44_encrypt(&my_pk, &plaintext).await
        .map_err(|e| format!("nip44 encrypt emoji list: {}", e))?;

    let builder = EventBuilder::new(Kind::Custom(KIND_EMOJI_LIST), content);
    client.send_event_builder(builder).await
        .map_err(|e| format!("Failed to publish emoji list (kind 10030): {}", e))?;

    crate::log_info!("[EmojiPacks] Published encrypted kind 10030 with {} pack subscription(s)", addrs.len());
    Ok(())
}

/// Settings key holding the UNIX-seconds timestamp of our most recent local
/// subscription mutation. A refresh ignores any relay kind-10030 older than
/// this so our just-changed (not-yet-propagated) list can't be clobbered.
const EMOJI_LIST_PUBLISHED_AT_KEY: &str = "emoji_list_published_at";

static REPUBLISH_GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Coalesce rapid subscribe/unsubscribe taps into one network publish.
/// Captures `SessionGuard` BEFORE the spawn boundary so a mid-debounce
/// account swap can't sign account A's pack list with account B's key.
pub fn republish_emoji_list_debounced() {
    use std::sync::atomic::Ordering;
    // Stamp the mutation time NOW (synchronously, before the debounce sleep)
    // so a refresh racing the not-yet-fired publish still treats the local
    // set as newer than any stale relay copy. Every local subscription change
    // funnels through here; the refresh-persist path does not, so this can't
    // wrongly suppress a legit cross-device update.
    let _ = crate::db::settings::set_sql_setting(
        EMOJI_LIST_PUBLISHED_AT_KEY.to_string(),
        Timestamp::now().as_secs().to_string(),
    );
    let gen = REPUBLISH_GEN.fetch_add(1, Ordering::SeqCst) + 1;
    let session = crate::state::SessionGuard::capture();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(800)).await;
        if REPUBLISH_GEN.load(Ordering::SeqCst) != gen { return; }
        if !session.is_valid() { return; }
        let client = match nostr_client() {
            Some(c) => c,
            None => return,
        };
        if let Err(e) = publish_emoji_list(&client).await {
            crate::log_warn!("[EmojiPacks] Republish failed: {} (retrying in 5s)", e);
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            if REPUBLISH_GEN.load(Ordering::SeqCst) != gen { return; }
            if !session.is_valid() { return; }
            if let Err(e2) = publish_emoji_list(&client).await {
                crate::log_warn!("[EmojiPacks] Republish retry failed: {}", e2);
            }
        }
    });
}

/// Subscribe to a pack by naddr: fetch the pack, persist it + the
/// subscription, then schedule a debounced republish of kind 10030.
/// Returns the hydrated pack on success.
pub async fn subscribe_pack(naddr: &str) -> Result<EmojiPack, String> {
    let session = crate::state::SessionGuard::capture();
    let pack = fetch_pack_by_naddr(naddr).await?;
    if !session.is_valid() {
        return Err("Account swapped during fetch — aborted".to_string());
    }

    // Equipped-pack cap. Idempotent re-subscribe to a pack we already
    // have stays free; only adding a brand-new addr counts toward the limit.
    let pack_addr = pack.addr();
    {
        let existing_subs = load_subscriptions()?;
        let is_new = !existing_subs.iter().any(|a| a == &pack_addr);
        let cap = effective_max_equipped_packs();
        if is_new && existing_subs.len() >= cap {
            return Err(format!(
                "You can equip at most {} packs. Remove one to add another.",
                cap,
            ));
        }
    }

    save_pack(&pack)?;

    let mut subs = load_subscriptions()?;
    if !subs.iter().any(|a| a == &pack_addr) {
        subs.push(pack_addr.clone());
    }
    if !session.is_valid() {
        return Err("Account swapped before subscription save — aborted".to_string());
    }
    save_subscriptions(&subs)?;

    republish_emoji_list_debounced();
    crate::traits::emit_event("emoji_packs_updated", &());

    Ok(pack)
}

// ============================================================================
// Pack publish (own creator path)
// ============================================================================

/// Build a kind 30030 EventBuilder for one of the user's own packs.
/// Dual-writes the NIP-51 spec tags (`title`/`image`/`description`)
/// alongside the Ditto-style (`name`/`picture`/`about`) tags so packs
/// interop with both ecosystems — see `MEMORY.md` plan notes.
fn build_pack_event(pack: &EmojiPack) -> Result<EventBuilder, String> {
    if pack.identifier.is_empty() {
        return Err("pack identifier required".to_string());
    }
    let mut builder = EventBuilder::new(Kind::Custom(KIND_EMOJI_SET), "")
        .tag(Tag::custom(TagKind::custom("d"), [pack.identifier.clone()]));

    // Spec-compliant metadata (NIP-51).
    if !pack.title.is_empty() {
        builder = builder
            .tag(Tag::custom(TagKind::custom("title"), [pack.title.clone()]))
            .tag(Tag::custom(TagKind::custom("name"), [pack.title.clone()]));
    }
    if !pack.image_url.is_empty() {
        builder = builder
            .tag(Tag::custom(TagKind::custom("image"), [pack.image_url.clone()]))
            .tag(Tag::custom(TagKind::custom("picture"), [pack.image_url.clone()]));
    }
    if !pack.description.is_empty() {
        builder = builder
            .tag(Tag::custom(TagKind::custom("description"), [pack.description.clone()]))
            .tag(Tag::custom(TagKind::custom("about"), [pack.description.clone()]));
    }

    for e in &pack.emojis {
        if e.shortcode.is_empty() || e.url.is_empty() { continue; }
        builder = builder.tag(Tag::custom(
            TagKind::custom("emoji"),
            [e.shortcode.clone(), e.url.clone()],
        ));
    }
    Ok(builder)
}

/// Publish (or replace) one of the user's own packs as a kind 30030
/// event, persist it locally, and add it to the subscription list so
/// the picker surfaces it immediately. SessionGuard-gated so a mid-
/// network account swap can't push account A's pack signed by B's key.
pub async fn publish_pack(pack: &EmojiPack) -> Result<EmojiPack, String> {
    let session = crate::state::SessionGuard::capture();
    let client = nostr_client().ok_or_else(|| "Nostr client not initialised".to_string())?;
    let my_pk = crate::state::my_public_key().ok_or_else(|| "Not logged in".to_string())?;

    // Per-pack emoji cap. Applies to own packs only — shared packs the
    // user receives can exceed this, the display layer truncates.
    let emoji_cap = effective_max_emojis_per_pack();
    if pack.emojis.len() > emoji_cap {
        return Err(format!(
            "A pack can hold at most {} emojis.",
            emoji_cap,
        ));
    }

    // Force `pubkey` + `is_own` regardless of caller — protects against
    // a malformed payload claiming ownership of someone else's pack.
    let mut to_save = pack.clone();
    to_save.pubkey = my_pk.to_hex();
    to_save.is_own = true;
    let raw_addr = build_pack_addr(&to_save.pubkey, &to_save.identifier);
    to_save.id = naddr_from_addr(&raw_addr)?;
    to_save.updated_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();

    // Equipped-pack cap. Replacing an existing own pack is fine — only
    // a *new* identifier would push us over the limit.
    {
        let existing_subs = load_subscriptions()?;
        let is_new = !existing_subs.iter().any(|a| a == &raw_addr);
        let cap = effective_max_equipped_packs();
        if is_new && existing_subs.len() >= cap {
            return Err(format!(
                "You can equip at most {} packs. Remove one to add another.",
                cap,
            ));
        }
    }

    let builder = build_pack_event(&to_save)?;
    client.send_event_builder(builder).await
        .map_err(|e| format!("publish kind 30030: {}", e))?;

    if !session.is_valid() {
        return Err("Account swapped during publish — local state untouched".to_string());
    }

    save_pack(&to_save)?;

    // Add to local subscriptions so the picker shows it without waiting
    // for the next 10030 republish to land.
    let mut subs = load_subscriptions()?;
    if !subs.iter().any(|a| a == &raw_addr) {
        subs.push(raw_addr.clone());
        if !session.is_valid() {
            return Err("Account swapped before subscription save — aborted".to_string());
        }
        save_subscriptions(&subs)?;
        republish_emoji_list_debounced();
    }

    crate::traits::emit_event("emoji_packs_updated", &());
    crate::log_info!("[EmojiPacks] Published own pack `{}` with {} emoji(s)",
        to_save.identifier, to_save.emojis.len());

    Ok(to_save)
}

/// Tombstone one of the user's own packs by publishing an empty kind
/// 30030 with just the `d` tag (relays replace the prior payload), drop
/// the local subscription, and republish kind 10030.
pub async fn delete_own_pack(id: &str) -> Result<(), String> {
    let session = crate::state::SessionGuard::capture();
    let parsed = parse_naddr(id)?;
    let raw_addr = parsed.to_addr_string();
    let my_pk = crate::state::my_public_key().ok_or_else(|| "Not logged in".to_string())?;
    if parsed.pubkey != my_pk {
        return Err("Cannot delete a pack you don't own".to_string());
    }
    let client = nostr_client().ok_or_else(|| "Nostr client not initialised".to_string())?;

    let builder = EventBuilder::new(Kind::Custom(KIND_EMOJI_SET), "")
        .tag(Tag::custom(TagKind::custom("d"), [parsed.identifier.clone()]));
    client.send_event_builder(builder).await
        .map_err(|e| format!("publish empty kind 30030: {}", e))?;

    if !session.is_valid() {
        return Err("Account swapped during delete — local state untouched".to_string());
    }

    // Drop subscription + pack rows (CASCADE wipes pack items).
    // Wrapped in a transaction so a crash between the two deletes can't
    // leave an orphan subscription pointing at a pack row that's already gone.
    {
        let mut conn = crate::db::get_write_connection_guard_static()?;
        let tx = conn.transaction()
            .map_err(|e| format!("begin delete tx: {}", e))?;
        tx.execute("DELETE FROM emoji_pack_subscriptions WHERE addr = ?1",
            rusqlite::params![raw_addr])
            .map_err(|e| format!("drop subscription: {}", e))?;
        tx.execute("DELETE FROM emoji_packs WHERE addr = ?1",
            rusqlite::params![raw_addr])
            .map_err(|e| format!("drop pack row: {}", e))?;
        tx.commit()
            .map_err(|e| format!("commit delete tx: {}", e))?;
    }

    republish_emoji_list_debounced();
    crate::traits::emit_event("emoji_packs_updated", &());
    crate::log_info!("[EmojiPacks] Deleted own pack `{}`", parsed.identifier);
    Ok(())
}

/// Unsubscribe locally and republish kind 10030 without the pack.
/// The pack row itself stays in `emoji_packs` (caller may still want
/// to render old reactions); only the subscription link is dropped.
pub async fn unsubscribe_pack(id: &str) -> Result<(), String> {
    let session = crate::state::SessionGuard::capture();
    let raw_addr = parse_naddr(id)?.to_addr_string();
    let mut subs = load_subscriptions()?;
    let before = subs.len();
    subs.retain(|a| a != &raw_addr);
    if subs.len() == before {
        return Ok(()); // not subscribed, noop
    }
    if !session.is_valid() {
        return Err("Account swapped before unsubscribe save — aborted".to_string());
    }
    save_subscriptions(&subs)?;
    republish_emoji_list_debounced();
    crate::traits::emit_event("emoji_packs_updated", &());
    Ok(())
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn keys() -> Keys {
        Keys::generate()
    }

    fn build_pack_event(
        k: &Keys,
        d: &str,
        title_tag: Option<(&str, &str)>,
        image_tag: Option<(&str, &str)>,
        desc_tag: Option<(&str, &str)>,
        emojis: &[(&str, &str)],
    ) -> Event {
        let mut tags: Vec<Tag> = Vec::new();
        tags.push(Tag::custom(TagKind::custom("d"), [d]));
        if let Some((key, val)) = title_tag {
            tags.push(Tag::custom(TagKind::custom(key), [val]));
        }
        if let Some((key, val)) = image_tag {
            tags.push(Tag::custom(TagKind::custom(key), [val]));
        }
        if let Some((key, val)) = desc_tag {
            tags.push(Tag::custom(TagKind::custom(key), [val]));
        }
        for (code, url) in emojis {
            tags.push(Tag::custom(TagKind::custom("emoji"), [*code, *url]));
        }
        EventBuilder::new(Kind::Custom(KIND_EMOJI_SET), "")
            .tags(tags)
            .sign_with_keys(k)
            .unwrap()
    }

    fn init_test_db() -> (tempfile::TempDir, std::sync::MutexGuard<'static, ()>) {
        let guard = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        crate::db::close_database();
        // Per-account row-id caches survive close_database; stale entries would
        // point into a prior test's DB.
        crate::db::clear_id_caches();
        let tmp = tempfile::tempdir().unwrap();
        let account = keys().public_key().to_bech32().unwrap();
        std::fs::create_dir_all(tmp.path().join(&account)).unwrap();
        crate::db::set_app_data_dir(tmp.path().to_path_buf());
        crate::db::set_current_account(account.clone()).unwrap();
        crate::db::init_database(&account).unwrap();
        (tmp, guard)
    }

    /// Save a parsed pack + return its raw addr.
    fn seed_pack(k: &Keys, d: &str, emojis: &[(&str, &str)]) -> String {
        let ev = build_pack_event(k, d, None, None, None, emojis);
        let pack = parse_pack_from_event(&ev, None).unwrap();
        save_pack(&pack).unwrap();
        pack.addr()
    }

    fn seed_usage(url: &str) {
        let conn = crate::db::get_write_connection_guard_static().unwrap();
        conn.execute(
            "INSERT INTO emoji_usage (kind, id, url, score, last_used) VALUES (1, ?1, ?1, 1.0, 0)",
            rusqlite::params![url],
        ).unwrap();
    }

    fn usage_count() -> i64 {
        let conn = crate::db::get_db_connection_guard_static().unwrap();
        conn.query_row("SELECT COUNT(*) FROM emoji_usage", [], |r| r.get(0)).unwrap()
    }

    fn pack_status(addr: &str) -> u8 {
        load_cached_pack(addr).unwrap().unwrap().status
    }

    fn build_deletion_event(k: &Keys, addr: &str) -> Event {
        EventBuilder::new(Kind::EventDeletion, "")
            .tag(Tag::custom(TagKind::custom("a"), [addr]))
            .sign_with_keys(k)
            .unwrap()
    }

    #[test]
    fn classify_verdict_table() {
        let k = keys();
        let live = build_pack_event(&k, "p", None, None, None, &[("a", "https://e.x/a.png")]);
        let empty = build_pack_event(&k, "p", None, None, None, &[]);
        let addr = format!("30030:{}:p", k.public_key().to_hex());
        let del = build_deletion_event(&k, &addr);

        // Live event, no deletion → Found (regardless of the absence gate).
        let (o, p) = classify_pack(Some(&live), None, None, false);
        assert_eq!(o, PackFetchOutcome::Found);
        assert!(p.is_some());

        // Empty replacement (Vector's own delete shape) → Tombstoned.
        let (o, _) = classify_pack(Some(&empty), None, None, true);
        assert_eq!(o, PackFetchOutcome::Tombstoned);

        // Kind-5 only, no event at all → Tombstoned.
        let (o, _) = classify_pack(None, Some(&del), None, true);
        assert_eq!(o, PackFetchOutcome::Tombstoned);

        // Timestamps pinned: a second boundary between two wall-clock builders
        // would otherwise flip the comparison and flake the test.
        let ts = Timestamp::from_secs(1_700_000_000);
        let live_pinned = EventBuilder::new(Kind::Custom(KIND_EMOJI_SET), "")
            .tags(vec![
                Tag::custom(TagKind::custom("d"), ["p"]),
                Tag::custom(TagKind::custom("emoji"), ["a", "https://e.x/a.png"]),
            ])
            .custom_created_at(ts)
            .sign_with_keys(&k)
            .unwrap();
        let del_at = |secs: u64| {
            EventBuilder::new(Kind::EventDeletion, "")
                .tag(Tag::custom(TagKind::custom("a"), [addr.as_str()]))
                .custom_created_at(Timestamp::from_secs(secs))
                .sign_with_keys(&k)
                .unwrap()
        };

        // Kind-5 with an EQUAL timestamp to a live event: the pack wins
        // (fail-safe toward alive).
        let (o, _) = classify_pack(Some(&live_pinned), Some(&del_at(1_700_000_000)), None, false);
        assert_eq!(o, PackFetchOutcome::Found);
        // A NEWER deletion tombstones the older live event.
        let (o, _) = classify_pack(Some(&live_pinned), Some(&del_at(1_700_000_001)), None, false);
        assert_eq!(o, PackFetchOutcome::Tombstoned);
        // An OLDER deletion loses to a republish.
        let (o, _) = classify_pack(Some(&live_pinned), Some(&del_at(1_699_999_999)), None, false);
        assert_eq!(o, PackFetchOutcome::Found);

        // Nothing found: gate decides.
        let (o, _) = classify_pack(None, None, None, true);
        assert_eq!(o, PackFetchOutcome::CleanMiss);
        let (o, _) = classify_pack(None, None, None, false);
        assert_eq!(o, PackFetchOutcome::Unreachable);
    }

    #[test]
    fn classify_invalid_emojis_is_not_a_tombstone() {
        // An event that HAS emoji tags Vector merely can't validate (bad
        // shortcode charset) must judge nothing, not brand the author.
        let k = keys();
        let ev = build_pack_event(&k, "weird", None, None, None, &[("bad!code", "https://e.x/a.png")]);
        assert!(parse_pack_from_event(&ev, None).is_none(), "precondition: unparseable");
        let (o, _) = classify_pack(Some(&ev), None, None, true);
        assert_eq!(o, PackFetchOutcome::Unreachable);
    }

    #[test]
    fn absence_gate_table() {
        let some_writes = vec![RelayUrl::parse("wss://relay.author.example").unwrap()];
        let no_writes: Vec<RelayUrl> = Vec::new();

        // Home failed or too few live relays: never clean.
        assert!(!absence_is_clean(false, 5, Some(&no_writes), true, 1));
        assert!(!absence_is_clean(true, 1, Some(&no_writes), true, 1));
        // Author outbox unknown (NIP-65 cold / fetch failed): not clean —
        // the pack's canonical home was never really checked.
        assert!(!absence_is_clean(true, 3, None, true, 1));
        // Author verifiably lists no write relays: home evidence suffices.
        assert!(absence_is_clean(true, 2, Some(&no_writes), false, 0));
        // Author outbox known: it must have been swept live.
        assert!(!absence_is_clean(true, 3, Some(&some_writes), false, 0));
        assert!(!absence_is_clean(true, 3, Some(&some_writes), true, 0));
        assert!(absence_is_clean(true, 3, Some(&some_writes), true, 1));
    }

    #[test]
    fn deletion_matching_is_author_bound() {
        let author = keys();
        let stranger = keys();
        let addr = format!("30030:{}:p", author.public_key().to_hex());
        // A third party citing the coordinate must never count as a revocation.
        let forged = build_deletion_event(&stranger, &addr);
        assert!(!deletion_matches(&forged, &author.public_key(), &addr));
        let real = build_deletion_event(&author, &addr);
        assert!(deletion_matches(&real, &author.public_key(), &addr));
    }

    #[test]
    fn tombstone_revokes_immediately_and_purges_frecency() {
        let (_tmp, _guard) = init_test_db();
        let k = keys();
        let addr = seed_pack(&k, "deadpack", &[("boom", "https://e.x/boom.png")]);
        seed_usage("https://e.x/boom.png");
        seed_usage("https://e.x/unrelated.png");

        let changed = apply_pack_health(&addr, &PackFetchOutcome::Tombstoned, 1_000).unwrap();
        assert!(changed, "tombstone must flip status in one sweep");
        assert_eq!(pack_status(&addr), PACK_STATUS_REVOKED);
        assert_eq!(usage_count(), 1, "only the dead pack's usage rows purge");
    }

    #[test]
    fn found_self_heals_from_revoked() {
        let (_tmp, _guard) = init_test_db();
        let k = keys();
        let addr = seed_pack(&k, "phoenix", &[("rise", "https://e.x/r.png")]);
        apply_pack_health(&addr, &PackFetchOutcome::Tombstoned, 1_000).unwrap();
        assert_eq!(pack_status(&addr), PACK_STATUS_REVOKED);

        let changed = apply_pack_health(&addr, &PackFetchOutcome::Found, 2_000).unwrap();
        assert!(changed);
        assert_eq!(pack_status(&addr), PACK_STATUS_ACTIVE, "a republished pack revives");
    }

    #[test]
    fn clean_miss_gauntlet_rate_limit_and_promotion() {
        let (_tmp, _guard) = init_test_db();
        let k = keys();
        let addr = seed_pack(&k, "fading", &[("bye", "https://e.x/bye.png")]);
        let t0: i64 = 1_000_000;
        let h = 3_600;

        // First miss counts; a second inside the rate-limit window doesn't.
        assert!(!apply_pack_health(&addr, &PackFetchOutcome::CleanMiss, t0).unwrap());
        assert!(!apply_pack_health(&addr, &PackFetchOutcome::CleanMiss, t0 + h).unwrap());
        // Two more spaced misses reach the count, but 48h hasn't elapsed yet.
        assert!(!apply_pack_health(&addr, &PackFetchOutcome::CleanMiss, t0 + 13 * h).unwrap());
        assert!(!apply_pack_health(&addr, &PackFetchOutcome::CleanMiss, t0 + 26 * h).unwrap());
        assert_eq!(pack_status(&addr), PACK_STATUS_ACTIVE, "count alone must not promote");
        // Past both thresholds: promoted.
        let changed = apply_pack_health(&addr, &PackFetchOutcome::CleanMiss, t0 + 49 * h).unwrap();
        assert!(changed);
        assert_eq!(pack_status(&addr), PACK_STATUS_MISSING);
    }

    #[test]
    fn unreachable_never_moves_counters() {
        let (_tmp, _guard) = init_test_db();
        let k = keys();
        let addr = seed_pack(&k, "offline", &[("zz", "https://e.x/z.png")]);
        for t in [1_000i64, 200_000, 400_000, 800_000] {
            assert!(!apply_pack_health(&addr, &PackFetchOutcome::Unreachable, t).unwrap());
        }
        assert_eq!(pack_status(&addr), PACK_STATUS_ACTIVE);
        let conn = crate::db::get_db_connection_guard_static().unwrap();
        let miss: i64 = conn.query_row(
            "SELECT miss_count FROM emoji_packs WHERE addr = ?1",
            rusqlite::params![addr], |r| r.get(0),
        ).unwrap();
        assert_eq!(miss, 0, "offline sweeps must not accrue misses");
    }

    #[test]
    fn revoked_ignores_clean_misses() {
        let (_tmp, _guard) = init_test_db();
        let k = keys();
        let addr = seed_pack(&k, "sealed", &[("x", "https://e.x/x.png")]);
        apply_pack_health(&addr, &PackFetchOutcome::Tombstoned, 1_000).unwrap();
        for t in [100_000i64, 300_000, 600_000] {
            assert!(!apply_pack_health(&addr, &PackFetchOutcome::CleanMiss, t).unwrap());
        }
        assert_eq!(pack_status(&addr), PACK_STATUS_REVOKED, "misses can't downgrade a tombstone verdict");
    }

    #[test]
    fn empty_pack_event_is_the_tombstone_shape() {
        // Vector's own delete flow publishes an empty replacement; the parser
        // must keep rejecting it so the sweep can read that rejection as a
        // deterministic tombstone.
        let k = keys();
        let ev = build_pack_event(&k, "gone", None, None, None, &[]);
        assert!(parse_pack_from_event(&ev, None).is_none());
    }

    #[test]
    fn parse_pack_reads_nip51_spec_tags() {
        let k = keys();
        let ev = build_pack_event(
            &k, "myPack",
            Some(("title", "Spec Pack")),
            Some(("image", "https://example.com/p.png")),
            Some(("description", "specd")),
            &[("smile", "https://e.x/s.png"), ("heart", "https://e.x/h.png")],
        );
        let pack = parse_pack_from_event(&ev, None).unwrap();
        assert_eq!(pack.identifier, "myPack");
        assert_eq!(pack.title, "Spec Pack");
        assert_eq!(pack.image_url, "https://example.com/p.png");
        assert_eq!(pack.description, "specd");
        assert_eq!(pack.emojis.len(), 2);
        assert_eq!(pack.addr(), format!("30030:{}:myPack", k.public_key().to_hex()));
    }

    #[test]
    fn parse_pack_falls_back_to_ditto_tags_when_spec_missing() {
        let k = keys();
        let ev = build_pack_event(
            &k, "ditto",
            Some(("name", "Ditto Pack")),
            Some(("picture", "https://example.com/d.png")),
            Some(("about", "ditto-style")),
            &[("yes", "https://e.x/y.png")],
        );
        let pack = parse_pack_from_event(&ev, None).unwrap();
        assert_eq!(pack.title, "Ditto Pack");
        assert_eq!(pack.image_url, "https://example.com/d.png");
        assert_eq!(pack.description, "ditto-style");
    }

    #[test]
    fn parse_pack_prefers_spec_tags_over_ditto() {
        let k = keys();
        let mut tags: Vec<Tag> = vec![
            Tag::custom(TagKind::custom("d"), ["both"]),
            Tag::custom(TagKind::custom("title"), ["SpecTitle"]),
            Tag::custom(TagKind::custom("name"), ["DittoName"]),
            Tag::custom(TagKind::custom("image"), ["spec.png"]),
            Tag::custom(TagKind::custom("picture"), ["ditto.png"]),
            Tag::custom(TagKind::custom("emoji"), ["a", "https://e.x/a.png"]),
        ];
        tags.extend(std::iter::empty());
        let ev = EventBuilder::new(Kind::Custom(KIND_EMOJI_SET), "")
            .tags(tags).sign_with_keys(&k).unwrap();
        let pack = parse_pack_from_event(&ev, None).unwrap();
        assert_eq!(pack.title, "SpecTitle");
        assert_eq!(pack.image_url, "spec.png");
    }

    #[test]
    fn parse_pack_returns_none_without_d_tag() {
        let k = keys();
        let ev = EventBuilder::new(Kind::Custom(KIND_EMOJI_SET), "")
            .tags(vec![
                Tag::custom(TagKind::custom("title"), ["No D"]),
                Tag::custom(TagKind::custom("emoji"), ["a", "https://e.x/a.png"]),
            ])
            .sign_with_keys(&k).unwrap();
        assert!(parse_pack_from_event(&ev, None).is_none());
    }

    #[test]
    fn parse_pack_returns_none_when_no_valid_emojis() {
        let k = keys();
        let ev = build_pack_event(&k, "empty", Some(("title", "Empty")), None, None, &[]);
        assert!(parse_pack_from_event(&ev, None).is_none());
    }

    #[test]
    fn parse_pack_rejects_invalid_shortcodes() {
        let k = keys();
        let ev = build_pack_event(
            &k, "mix", Some(("title", "Mix")), None, None,
            &[("ok_name", "https://e.x/a.png"),
              ("bad name", "https://e.x/b.png"),
              ("colons:no", "https://e.x/c.png"),
              ("", "https://e.x/d.png"),
              ("dash-ok", "https://e.x/e.png")],
        );
        let pack = parse_pack_from_event(&ev, None).unwrap();
        let codes: Vec<&str> = pack.emojis.iter().map(|e| e.shortcode.as_str()).collect();
        assert_eq!(codes, vec!["ok_name", "dash-ok"]);
    }

    #[test]
    fn parse_pack_dedupes_shortcodes_first_wins() {
        let k = keys();
        let ev = build_pack_event(
            &k, "dup", Some(("title", "Dup")), None, None,
            &[("smile", "https://e.x/first.png"),
              ("smile", "https://e.x/second.png")],
        );
        let pack = parse_pack_from_event(&ev, None).unwrap();
        assert_eq!(pack.emojis.len(), 1);
        assert_eq!(pack.emojis[0].url, "https://e.x/first.png");
    }

    #[test]
    fn parse_pack_marks_is_own_only_when_my_pubkey_matches() {
        let k = keys();
        let ev = build_pack_event(&k, "mine", Some(("title", "Mine")), None, None,
            &[("a", "https://e.x/a.png")]);
        let my_hex = k.public_key().to_hex();
        let pack_mine = parse_pack_from_event(&ev, Some(&my_hex)).unwrap();
        assert!(pack_mine.is_own);

        let stranger = keys().public_key().to_hex();
        let pack_other = parse_pack_from_event(&ev, Some(&stranger)).unwrap();
        assert!(!pack_other.is_own);
    }

    #[test]
    fn pack_address_to_string_round_trips() {
        let k = keys();
        let hex = k.public_key().to_hex();
        let addr = PackAddress {
            kind: 30030,
            pubkey: k.public_key(),
            identifier: "myPack".to_string(),
        };
        assert_eq!(addr.to_addr_string(), format!("30030:{}:myPack", hex));
        let parsed = parse_pack_address(&addr.to_addr_string()).unwrap();
        assert_eq!(parsed, addr);
    }

    #[test]
    fn parse_naddr_round_trips_kind_30030_coordinate() {
        // Construct a synthetic naddr via nostr-sdk and verify our decoder
        // round-trips kind / pubkey / identifier.
        let k = keys();
        let coord = nostr_sdk::nips::nip01::Coordinate {
            kind: Kind::Custom(30030),
            public_key: k.public_key(),
            identifier: "trip".to_string(),
        };
        let n19 = nostr_sdk::nips::nip19::Nip19Coordinate {
            coordinate: coord,
            relays: Vec::new(),
        };
        let naddr = nostr_sdk::nips::nip19::Nip19::Coordinate(n19).to_bech32().unwrap();
        let parsed = parse_naddr(&naddr).unwrap();
        assert_eq!(parsed.kind, 30030);
        assert_eq!(parsed.pubkey, k.public_key());
        assert_eq!(parsed.identifier, "trip");
    }

    #[test]
    fn parse_naddr_rejects_non_30030_kinds() {
        let k = keys();
        let coord = nostr_sdk::nips::nip01::Coordinate {
            kind: Kind::Custom(30023), // long-form article
            public_key: k.public_key(),
            identifier: "essay".to_string(),
        };
        let n19 = nostr_sdk::nips::nip19::Nip19Coordinate {
            coordinate: coord,
            relays: Vec::new(),
        };
        let naddr = nostr_sdk::nips::nip19::Nip19::Coordinate(n19).to_bech32().unwrap();
        let err = parse_naddr(&naddr).unwrap_err();
        assert!(err.contains("expected kind 30030"),
            "expected kind-rejection error, got: {}", err);
    }

    #[test]
    fn parse_naddr_strips_nostr_uri_prefix() {
        let k = keys();
        let coord = nostr_sdk::nips::nip01::Coordinate {
            kind: Kind::Custom(30030),
            public_key: k.public_key(),
            identifier: "prefixed".to_string(),
        };
        let n19 = nostr_sdk::nips::nip19::Nip19Coordinate {
            coordinate: coord,
            relays: Vec::new(),
        };
        let naddr = nostr_sdk::nips::nip19::Nip19::Coordinate(n19).to_bech32().unwrap();
        let with_prefix = format!("nostr:{}", naddr);
        let parsed = parse_naddr(&with_prefix).unwrap();
        assert_eq!(parsed.identifier, "prefixed");
    }

    #[test]
    fn parse_naddr_rejects_garbage_input() {
        assert!(parse_naddr("not an naddr").is_err());
        assert!(parse_naddr("naddr1invalid").is_err());
        assert!(parse_naddr("").is_err());
    }

    #[test]
    fn parse_pack_address_round_trips_valid_input() {
        let k = keys();
        let hex = k.public_key().to_hex();
        let addr = format!("30030:{}:myId", hex);
        let parsed = parse_pack_address(&addr).unwrap();
        assert_eq!(parsed.kind, 30030);
        assert_eq!(parsed.pubkey, k.public_key());
        assert_eq!(parsed.identifier, "myId");
    }

    #[test]
    fn parse_pack_address_rejects_wrong_kind() {
        let hex = keys().public_key().to_hex();
        let addr = format!("10030:{}:x", hex);
        assert!(parse_pack_address(&addr).is_err());
    }

    #[test]
    fn parse_pack_address_rejects_malformed_pubkey() {
        let addr = "30030:not-hex:x".to_string();
        assert!(parse_pack_address(&addr).is_err());
    }

    #[test]
    fn parse_pack_address_preserves_colons_in_identifier() {
        // d-tag values can be arbitrary strings, including colons.
        let hex = keys().public_key().to_hex();
        let addr = format!("30030:{}:id:with:colons", hex);
        let parsed = parse_pack_address(&addr).unwrap();
        assert_eq!(parsed.identifier, "id:with:colons");
    }

    #[test]
    fn parse_inner_tag_list_extracts_valid_a_tags() {
        // The inner tag list lives JSON-encoded inside the NIP-44-encrypted
        // event content; exercise the parser directly so we don't pull a
        // signer + network into a unit test.
        let hex_a = keys().public_key().to_hex();
        let hex_b = keys().public_key().to_hex();
        let plaintext = format!(
            r#"[["a","30030:{a}:packA"],["a","30030:{b}:packB"],["a","malformed"],["a","10030:{a}:wrongkind"],["p","not-an-a-tag"]]"#,
            a = hex_a,
            b = hex_b,
        );
        let addrs = parse_inner_tag_list(&plaintext);
        assert_eq!(addrs.len(), 2);
        assert_eq!(addrs[0].identifier, "packA");
        assert_eq!(addrs[1].identifier, "packB");
    }

    #[test]
    fn parse_inner_tag_list_returns_empty_on_malformed_json() {
        assert!(parse_inner_tag_list("not json").is_empty());
        assert!(parse_inner_tag_list("").is_empty());
    }

    #[test]
    fn shortcode_validator_accepts_alphanum_dash_underscore() {
        assert!(is_valid_shortcode("smile"));
        assert!(is_valid_shortcode("smile_face"));
        assert!(is_valid_shortcode("smile-face"));
        assert!(is_valid_shortcode("Smile2"));
        assert!(!is_valid_shortcode(""));
        assert!(!is_valid_shortcode("smile face"));
        assert!(!is_valid_shortcode("smile:face"));
        assert!(!is_valid_shortcode("😀"));
        // `~` is reserved for message-tag disambiguation, NOT valid in
        // pack-authored shortcodes.
        assert!(!is_valid_shortcode("love~2"));
    }

    #[test]
    fn resolve_token_disambiguates_duplicate_shortcodes() {
        // Two distinct images share `:love:`, sorted lexicographically by URL.
        let mut by_code: HashMap<String, Vec<String>> = HashMap::new();
        by_code.insert(
            "love".to_string(),
            vec!["https://a.example/love.png".to_string(), "https://b.example/love.gif".to_string()],
        );
        by_code.insert("cat".to_string(), vec!["https://a.example/cat.png".to_string()]);

        // Plain code → first candidate (matches a bare `:love:`).
        assert_eq!(resolve_emoji_token(&by_code, "love").as_deref(), Some("https://a.example/love.png"));
        // `~1` / `~2` select by 1-based index.
        assert_eq!(resolve_emoji_token(&by_code, "love~1").as_deref(), Some("https://a.example/love.png"));
        assert_eq!(resolve_emoji_token(&by_code, "love~2").as_deref(), Some("https://b.example/love.gif"));
        // Out-of-range / zero / unknown base → nothing (renders literal).
        assert_eq!(resolve_emoji_token(&by_code, "love~3"), None);
        assert_eq!(resolve_emoji_token(&by_code, "love~0"), None);
        assert_eq!(resolve_emoji_token(&by_code, "nope~1"), None);
        // Non-colliding code resolves bare; a stray `~N` on it is out of range.
        assert_eq!(resolve_emoji_token(&by_code, "cat").as_deref(), Some("https://a.example/cat.png"));
        assert_eq!(resolve_emoji_token(&by_code, "cat~2"), None);
    }

    #[test]
    fn message_emoji_tags_accept_disambiguation_separator() {
        // Inbound `love~2` tags must survive parsing so the recipient renders.
        let tags = vec![
            vec!["emoji".to_string(), "love~2".to_string(), "https://b.example/love.gif".to_string()],
            vec!["emoji".to_string(), "bad name".to_string(), "https://x".to_string()],
        ];
        let out = crate::types::EmojiTag::extract_from_stored(&tags);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].shortcode, "love~2");
    }
}
