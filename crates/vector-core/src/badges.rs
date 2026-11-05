//! Profile badges — fetch, validate, and cache.
//!
//! Badges are not loaded during the critical boot path. The cache is filled
//! once after initial sync (see `refresh_own_badges`) so badge-gated perks
//! (e.g. raised emoji-pack limits) resolve without an on-demand network
//! round-trip. `has_vector_badge` is the cheap synchronous reader used by
//! those gates.

use nostr_sdk::prelude::*;
use std::collections::HashSet;
use std::sync::LazyLock;

// Guy Fawkes Day 2025 — V for Vector badge claim window.
const FAWKES_DAY_START: u64 = 1762300800; // 2025-11-05 00:00:00 UTC
const FAWKES_DAY_END: u64 = 1762387200; // 2025-11-06 00:00:00 UTC

/// Per-account settings key: "true" once we've confirmed the Vector badge.
const BADGE_VECTOR_KEY: &str = "badge_vector";
/// Per-account settings key: unix-secs of the last unsuccessful resolve pass.
/// Throttles re-checking for accounts that don't (yet) hold the badge.
const BADGE_CHECK_TS_KEY: &str = "badge_check_ts";
/// Don't re-run the full retry loop more than this often for an account we've
/// already checked without success. The claim window is permanently closed, so
/// a non-holder can never become a holder — frequent restarts shouldn't each
/// trigger a fresh relay sweep.
const RECHECK_COOLDOWN_SECS: u64 = 6 * 3600;

fn unix_now() -> u64 {
    crate::clock::now_secs()
}

/// Whether a kind-30078 event is a valid Fawkes badge claim: right content and
/// a timestamp inside the (half-open) event window. Pure so it's unit-testable.
fn is_valid_fawkes_claim(content: &str, created_at: u64) -> bool {
    content == "fawkes_badge_claimed"
        && (FAWKES_DAY_START..FAWKES_DAY_END).contains(&created_at)
}

/// Fetch + validate whether `pubkey` holds the V for Vector (Guy Fawkes 2025)
/// badge: a kind-30078 `d=fawkes_2025` claim published within the event window.
///
/// Queries the full relay pool rather than only the trusted relays: the claim
/// was published during the event to whatever relays the holder used, and any
/// single relay (including a trusted one) can be transiently down. Broadest
/// net gives the best chance of locating the permanent claim.
pub async fn has_fawkes_badge(pubkey: &PublicKey) -> Result<bool, String> {
    let client = crate::state::nostr_client().ok_or("Nostr client not initialized")?;
    let filter = Filter::new()
        .author(*pubkey)
        .kind(Kind::ApplicationSpecificData)
        .custom_tag(SingleLetterTag::LOWERCASE_D, "fawkes_2025")
        // > 1 to tolerate relays serving superseded copies of the replaceable
        // event alongside the current one.
        .limit(10);
    let mut events = client
        .stream_events(filter)
        .timeout(crate::relay_request_timeout(std::time::Duration::from_secs(10)))
        .await
        .map_err(|e| e.to_string())?;
    // 0.45 streams (relay, Result<Event>) so callers can attribute per-relay failures.
    while let Some((_relay, res)) = events.next().await {
        let Ok(event) = res else { continue };
        if is_valid_fawkes_claim(&event.content, event.created_at.as_secs()) {
            return Ok(true);
        }
    }
    if !any_relay_connected(&client).await {
        return Err("no relay connected".into());
    }
    Ok(false)
}

// ── V for Vector 2026 (self-claimed, the 24 hours from release) ──────────────────

/// The claim window, unix seconds, half-open: 16:00 UTC on 2026-11-05 to the next day's 16:00.
pub const V2026_START: u64 = 1793894400;
pub const V2026_END: u64 = 1793980800;
/// For a test window: a claim carries a NIP-40 expiration this many seconds out, so relays drop
/// it and the local cache lets it go, and any build may claim. None for the real thing.
pub const V2026_CLAIM_TTL: Option<u64> = None;

const V2026_D: &str = "vector_2026";
const V2026_CONTENT: &str = "vector_2026_badge_claimed";
/// Per-account settings key: "true" once the badge is ours, or (a test claim) the unix-secs
/// it expires at.
const BADGE_V2026_KEY: &str = "badge_vector_2026";

/// Whether the claim window is open on Vector's clock.
pub fn v2026_open() -> bool {
    (V2026_START..V2026_END).contains(&unix_now())
}

fn is_valid_v2026_claim(content: &str, created_at: u64) -> bool {
    content == V2026_CONTENT && (V2026_START..V2026_END).contains(&created_at)
}

/// Fetch whether `pubkey` holds V for Vector 2026: its own kind-30078 `d=vector_2026` claim,
/// published inside the window.
pub async fn has_v2026_badge(pubkey: &PublicKey) -> Result<bool, String> {
    let client = crate::state::nostr_client().ok_or("Nostr client not initialized")?;
    let filter = Filter::new()
        .author(*pubkey)
        .kind(Kind::ApplicationSpecificData)
        .custom_tag(SingleLetterTag::LOWERCASE_D, V2026_D)
        .limit(10);
    let mut events = client
        .stream_events(filter)
        .timeout(crate::relay_request_timeout(std::time::Duration::from_secs(10)))
        .await
        .map_err(|e| e.to_string())?;
    while let Some((_relay, res)) = events.next().await {
        let Ok(event) = res else { continue };
        if is_valid_v2026_claim(&event.content, event.created_at.as_secs()) && !event.is_expired() {
            return Ok(true);
        }
    }
    if !any_relay_connected(&client).await {
        return Err("no relay connected".into());
    }
    Ok(false)
}

/// Cached: whether this account holds V for Vector 2026 (a test claim, until it expires).
pub fn has_vector_2026_badge() -> bool {
    match crate::db::settings::get_sql_setting_strict(BADGE_V2026_KEY) {
        Ok(v) => {
            let held = v2026_held(v.as_deref(), unix_now());
            with_last(|l| l.vector_2026 = Some(held));
            held
        }
        Err(_) => with_last(|l| l.vector_2026).unwrap_or(false),
    }
}

fn v2026_held(cached: Option<&str>, now: u64) -> bool {
    match cached {
        Some("true") => true,
        Some(expires) => expires.parse::<u64>().is_ok_and(|at| now < at),
        None => false,
    }
}

fn store_v2026_badge(expires_at: Option<u64>) {
    with_last(|l| l.vector_2026 = Some(true));
    let value = expires_at.map_or_else(|| "true".to_string(), |at| at.to_string());
    let _ = crate::db::set_sql_setting(BADGE_V2026_KEY.to_string(), value);
}

/// Claim V for Vector 2026 for this account: publish its claim while the window is open, then
/// hold the badge. Claiming again is harmless: the claim is a replaceable event.
pub async fn claim_v2026() -> Result<(), String> {
    // Bound to the account that claims: a swap mid-publish keeps the badge with it.
    crate::db::scoped_result(async move {
        if !v2026_open() {
            return Err("closed: the claim window isn't open".to_string());
        }
        let client = crate::state::nostr_client().ok_or("Nostr client not initialized")?;
        let mut builder = EventBuilder::new(Kind::ApplicationSpecificData, V2026_CONTENT)
            .tag(Tag::identifier(V2026_D));
        let expires_at = V2026_CLAIM_TTL.map(|ttl| unix_now() + ttl);
        if let Some(at) = expires_at {
            builder = builder.tag(Tag::expiration(Timestamp::from_secs(at)));
        }
        let sent = crate::sign_and_send(&client, builder).await?;
        if sent.success.is_empty() {
            return Err("no relay accepted the claim".to_string());
        }
        store_v2026_badge(expires_at);
        emit_badges();
        Ok(())
    })
    .await
}

/// Someone's V for Vector 2026, looked up on demand (a profile). Our own, when found, is held,
/// as the post-sync refresh would.
pub async fn check_v2026(pubkey: &PublicKey) -> Result<bool, String> {
    let has = has_v2026_badge(pubkey).await?;
    if has && !has_vector_2026_badge() && crate::state::my_public_key().as_ref() == Some(pubkey) {
        store_v2026_badge(V2026_CLAIM_TTL.map(|ttl| unix_now() + ttl));
        emit_badges();
    }
    Ok(has)
}

/// This account's badges and perk tier, with the 2026 claim window, for the UI.
pub fn own_summary() -> serde_json::Value {
    serde_json::json!({
        "vector": has_vector_badge(),
        "vector_2026": has_vector_2026_badge(),
        "tier": effective_tier(),
        "bug_hunter": bug_hunter_tier(),
        "drop_2026": {
            "opens_at": V2026_START * 1000,
            "closes_at": V2026_END * 1000,
            "test": V2026_CLAIM_TTL.is_some(),
        },
    })
}

/// Our own claim, made on another device (or before a reinstall): found once the window has
/// opened and kept, as the 2025 badge is.
async fn refresh_own_v2026(pk: PublicKey) {
    if has_vector_2026_badge() || unix_now() < V2026_START {
        return;
    }
    if let Ok(true) = has_v2026_badge(&pk).await {
        // A test claim found on a relay is about to expire there too: hold it no longer.
        store_v2026_badge(V2026_CLAIM_TTL.map(|ttl| unix_now() + ttl));
        emit_badges();
    }
}

/// An empty answer means "none" only if some relay was there to give it.
async fn any_relay_connected(client: &Client) -> bool {
    client.relays().await.values().any(|r| r.status() == RelayStatus::Connected)
}

/// The last badge state this session read from or wrote to its database. A failed
/// read answers from here, never as "no badge": a busy or unbound connection must
/// not take an account's perks away.
#[derive(Default)]
struct LastRead {
    vector: Option<bool>,
    vector_2026: Option<bool>,
    bug_hunter: Option<u8>,
}
struct LastReadKey;

fn last_read() -> std::sync::Arc<std::sync::Mutex<LastRead>> {
    crate::db::current_session().scoped::<LastReadKey, _>()
}

fn with_last<T>(f: impl FnOnce(&mut LastRead) -> T) -> T {
    f(&mut last_read().lock().unwrap_or_else(|e| e.into_inner()))
}

/// Cached flag for whether we hold the Vector badge. Cheap + synchronous, so
/// safe to call from limit checks. Defaults to false when unset — badge perks
/// stay off until the cache is filled post-sync.
pub fn has_vector_badge() -> bool {
    match crate::db::settings::get_sql_setting_strict(BADGE_VECTOR_KEY) {
        Ok(v) => {
            let held = v.as_deref() == Some("true");
            with_last(|l| l.vector = Some(held));
            held
        }
        Err(_) => with_last(|l| l.vector).unwrap_or(false),
    }
}

fn store_vector_badge() {
    with_last(|l| l.vector = Some(true));
    let _ = crate::db::set_sql_setting(BADGE_VECTOR_KEY.to_string(), "true".to_string());
}

/// Per-account settings key: cached Bug Hunter NIP-58 tier (0-3). Filled by the
/// award fetch; downgraded only on a seen issuer revocation, never on absence.
const BADGE_BUG_HUNTER_TIER_KEY: &str = "badge_bug_hunter_tier";

/// Per-account settings key: comma-separated hex ids of the award events backing
/// the cached tier, so a revocation still resolves after the relays purge the award.
const BADGE_BUG_HUNTER_AWARD_IDS_KEY: &str = "badge_bug_hunter_award_ids";

/// Cached Bug Hunter tier (0-3) for the current account. Cheap + synchronous;
/// 0 until the award fetch fills it.
pub fn bug_hunter_tier() -> u8 {
    match crate::db::settings::get_sql_setting_strict(BADGE_BUG_HUNTER_TIER_KEY) {
        Ok(v) => {
            let tier = v.and_then(|v| v.parse::<u8>().ok()).map(|t| t.min(3)).unwrap_or(0);
            with_last(|l| l.bug_hunter = Some(tier));
            tier
        }
        Err(_) => with_last(|l| l.bug_hunter).unwrap_or(0),
    }
}

fn store_bug_hunter_tier(tier: u8) {
    with_last(|l| l.bug_hunter = Some(tier));
    let _ = crate::db::set_sql_setting(BADGE_BUG_HUNTER_TIER_KEY.to_string(), tier.to_string());
}

/// Tell the UI what the account's perks are now.
fn emit_badges() {
    crate::traits::emit_event_json("badges_updated", own_summary());
}

/// The account's effective premium tier (0-3): the highest its badges grant. V for Vector
/// 2025 is full premium (3); 2026 grants what Bug Hunter's first tier does (1). Per-account
/// perks (emoji limits, pins, Text Effects) read this.
pub fn effective_tier() -> u8 {
    if is_bot() { return 3; }
    badge_tier(bug_hunter_tier(), has_vector_badge(), has_vector_2026_badge())
}

fn badge_tier(bug_hunter: u8, vector: bool, vector_2026: bool) -> u8 {
    bug_hunter.max(if vector { 3 } else { 0 }).max(if vector_2026 { 1 } else { 0 })
}

/// Text Effects (coloured text, gradients, rainbows) are a perk of any badge: tier 1 and up.
/// Others' coloured messages always render; only sending colour is gated.
pub fn has_text_effects() -> bool {
    effective_tier() >= 1
}

/// A process role, not account state: the SDK runs one bot per process.
static BOT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Run this process as a bot: every tiered perk at its top tier, and emoji packs
/// uncapped (any number equipped, every emoji in each). Fixed protocol ceilings,
/// like a message's reaction groups and the 64 KB event, still apply.
pub fn set_bot(on: bool) {
    BOT.store(on, std::sync::atomic::Ordering::Relaxed);
}

pub fn is_bot() -> bool {
    BOT.load(std::sync::atomic::Ordering::Relaxed)
}

/// Per-effective-tier cap on NEW reaction groups one user may open on a single
/// message. Joining an existing reaction (+1) is never gated — only introducing
/// an emoji the message doesn't yet carry spends this allowance.
pub const NEW_REACTIONS_PER_POST_BY_TIER: [usize; 4] = [6, 6, 9, 12];

/// The current account's fresh-reaction allowance per message.
pub fn effective_max_new_reactions_per_post() -> usize {
    NEW_REACTIONS_PER_POST_BY_TIER[effective_tier() as usize]
}

/// Sender-side gate for reacting to `reference_id` with `emoji`: joining an
/// existing group always passes; opening a new group checks the message-wide
/// group ceiling, then the per-tier allowance. "Groups I opened" is judged by
/// insertion order in our own view — the earliest held reaction per emoji.
/// `Ok(())` when the send may proceed.
pub async fn check_new_reaction_allowance(reference_id: &str, emoji: &str) -> Result<(), String> {
    use nostr_sdk::prelude::ToBech32;
    let me = match crate::state::my_public_key().and_then(|pk| pk.to_bech32().ok()) {
        Some(npub) => npub,
        None => return Err("Not logged in".to_string()),
    };
    let st = crate::state::STATE.lock().await;
    let Some((_, message)) = st.find_message(reference_id) else { return Ok(()) };
    if message.reactions.iter().any(|r| r.emoji == emoji) {
        return Ok(());
    }
    let mut first_by_emoji: std::collections::HashMap<&str, &str> = std::collections::HashMap::new();
    for r in &message.reactions {
        first_by_emoji.entry(r.emoji.as_str()).or_insert(r.author_id.as_str());
    }
    if first_by_emoji.len() >= crate::compact::MAX_REACTION_GROUPS {
        return Err("This message has reached its reaction limit".to_string());
    }
    let spent = first_by_emoji.values().filter(|a| **a == me).count();
    if spent >= effective_max_new_reactions_per_post() {
        return Err("You've used all your new reactions on this message".to_string());
    }
    Ok(())
}

/// The highest effective tier across ALL accounts on this install. The
/// multi-account cap is device-level (adding a profile spans accounts), so it
/// must not drop when you switch to an un-badged account — unlike the per-account
/// perks. Reads each account's badge state straight from its vector.db.
pub fn max_account_tier() -> u8 {
    let mut max = effective_tier();
    if let Ok(accounts) = crate::db::get_accounts() {
        for npub in accounts {
            max = max.max(read_account_tier(&npub).unwrap_or(0));
        }
    }
    max
}

/// A (possibly non-active) account's effective tier, read read-only from its
/// vector.db settings (plaintext KV). None if the DB/keys are absent or locked.
fn read_account_tier(npub: &str) -> Option<u8> {
    let path = crate::db::account_dir(npub).ok()?.join("vector.db");
    let conn = rusqlite::Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
    let get = |key: &str| -> Option<String> {
        conn.query_row("SELECT value FROM settings WHERE key = ?1", rusqlite::params![key], |r| r.get(0)).ok()
    };
    let bug = get(BADGE_BUG_HUNTER_TIER_KEY).and_then(|v| v.parse::<u8>().ok()).map(|t| t.min(3)).unwrap_or(0);
    let vector = get(BADGE_VECTOR_KEY).map(|v| v == "true").unwrap_or(false);
    let vector_2026 = v2026_held(get(BADGE_V2026_KEY).as_deref(), unix_now());
    Some(badge_tier(bug, vector, vector_2026))
}

/// Record the result of an on-demand badge check. When the checked key is our
/// own and the badge is present, persist it (sticky) and emit `badges_updated`
/// so badge-gated perks (raised emoji-pack limits) turn on immediately — this is
/// the safety net for a post-sync `refresh_own_badges` that missed the claim
/// (the holding relay is often flaky during the saturated sync window) and is now
/// sitting in its multi-hour re-check cooldown. An on-demand check runs at a
/// quiet moment, so it lands where the sync-time sweep didn't.
///
/// No-op for other users, a negative result, or an account swap mid-check (the
/// own-key comparison re-reads the *current* account, so a stale key never
/// writes the wrong DB). No awaits, so the read + write stay on one account.
pub fn note_own_badge_confirmed(pubkey: &PublicKey, has_badge: bool) {
    if !has_badge || has_vector_badge() {
        return;
    }
    if crate::state::my_public_key().as_ref() != Some(pubkey) {
        return;
    }
    store_vector_badge();
    crate::log_info!("[Badges] vector badge confirmed via on-demand check");
    emit_badges();
}

/// Resolve the account's badges once its sync has settled, on every login and on
/// every client, then tell the UI. The relay holding a claim is often the one the
/// sync just saturated.
pub async fn refresh_own() {
    crate::rt::time::sleep(std::time::Duration::from_secs(30)).await;
    let v2026 = async {
        if let Some(pk) = crate::state::my_public_key() {
            crate::db::scoped(refresh_own_v2026(pk)).await;
        }
    };
    futures_util::join!(refresh_own_badges(), refresh_own_bug_hunter(), v2026);
    emit_badges();
}

/// Fetch our own badges and persist to the per-account cache. Called once
/// after initial sync. The std::sync::Arc<crate::db::Session> straddles the network fetch so a
/// mid-fetch account swap can't write account A's badge into account B's DB.
async fn refresh_own_badges() {
    crate::db::scoped(async move {
        let Some(pk) = crate::state::my_public_key() else {
            crate::log_warn!("[Badges] refresh skipped — no public key");
            return;
        };

        // Sticky: the badge is a permanent achievement, so once confirmed we never
        // re-query (avoids a flaky relay later flipping it off) and never downgrade.
        if has_vector_badge() {
            crate::log_info!("[Badges] vector badge already cached — skipping refresh");
            return;
        }

        // Throttle: skip the relay sweep if we already checked recently without
        // success. The window is closed, so a miss now will still be a miss in an
        // hour — no need to re-sweep on every restart.
        let now = unix_now();
        if let Some(last) = crate::db::get_sql_setting(BADGE_CHECK_TS_KEY.to_string())
            .ok()
            .flatten()
            .and_then(|v| v.parse::<u64>().ok())
        {
            if now.saturating_sub(last) < RECHECK_COOLDOWN_SECS {
                return;
            }
        }

        crate::log_info!(
            "[Badges] resolving own badges for {}…",
            pk.to_bech32().unwrap_or_default()
        );

        // The holding relay (often the user's own) is flaky/overloaded during the
        // heavy sync window, so retry a few times to catch it during a quiet
        // moment. A miss leaves the badge cache untouched (records only the check
        // time for the cooldown); the next boot past the cooldown tries again until
        // it lands once (then sticky-cached forever).
        const ATTEMPTS: u8 = 3;
        let mut answered = false;
        for attempt in 1..=ATTEMPTS {
            match has_fawkes_badge(&pk).await {
                Ok(true) => {
                    crate::log_info!("[Badges] vector badge confirmed (attempt {})", attempt);
                    store_vector_badge();
                    emit_badges();
                    return;
                }
                Ok(false) => {
                    answered = true;
                    crate::log_info!("[Badges] vector badge not found (attempt {}/{})", attempt, ATTEMPTS);
                }
                Err(e) => {
                    crate::log_warn!("[Badges] refresh attempt {}/{} failed: {}", attempt, ATTEMPTS, e);
                }
            }
            if attempt < ATTEMPTS {
                crate::rt::time::sleep(std::time::Duration::from_secs(20)).await;
            }
        }
        // The cooldown starts only once a relay answered: an offline boot checked nothing.
        if answered {
            let _ = crate::db::set_sql_setting(BADGE_CHECK_TS_KEY.to_string(), now.to_string());
            crate::log_info!("[Badges] vector badge not resolved this boot — will retry after cooldown");
        } else {
            crate::log_info!("[Badges] no relay answered — will retry next login");
        }
    })
    .await
}

// ── Bug Hunter (NIP-58 tiered, team-awarded) ───────────────────────────────

/// The Vector Team issuer key — the NIP-58 trust root for Bug Hunter badges.
/// Only awards (kind 8) and revocations (kind 5) signed by this key are honored,
/// so a forged award from any other key grants nothing.
const BUG_HUNTER_ISSUER_NPUB: &str =
    "npub1hrujuc08r4zcdtn0u6ts7u7apldcjqgftz0z7stmaaz9hwaf9jxs66f3yh";

static BUG_HUNTER_ISSUER: LazyLock<PublicKey> = LazyLock::new(|| {
    PublicKey::from_bech32(BUG_HUNTER_ISSUER_NPUB)
        .expect("hardcoded Bug Hunter issuer npub must be valid")
});

/// Map a badge-definition `d` identifier to its tier. The display `name` can
/// change freely; the `d` slug is the permanent identity.
fn tier_from_slug(d: &str) -> Option<u8> {
    match d {
        "bug-hunter-tier-1" => Some(1),
        "bug-hunter-tier-2" => Some(2),
        "bug-hunter-tier-3" => Some(3),
        _ => None,
    }
}

/// Parse a NIP-58 `a`-tag coordinate (`30009:<issuer-hex>:<d>`) into a tier, but
/// only when the kind is 30009 AND the author is our trusted issuer — an award
/// pointing at a definition minted under any other key is ignored.
fn tier_from_coord(coord: &str, issuer_hex: &str) -> Option<u8> {
    let mut parts = coord.splitn(3, ':');
    let kind = parts.next()?;
    let author = parts.next()?;
    let d = parts.next()?;
    if kind != "30009" || author != issuer_hex {
        return None;
    }
    tier_from_slug(d)
}

/// Fold awards (event id + tier) and the set of revoked award ids into the tier
/// seen this fetch, plus whether a revocation actually applied to one of our
/// awards. Pure so it's unit-testable. `saw_revocation` is the positive signal
/// that justifies a downgrade (vs. a flaky-relay absence, which must not).
fn fold_bug_hunter(awards: &[(EventId, u8)], revoked: &HashSet<EventId>) -> (u8, bool) {
    let mut seen_tier = 0u8;
    let mut saw_revocation = false;
    for (id, tier) in awards {
        if revoked.contains(id) {
            saw_revocation = true;
        } else {
            seen_tier = seen_tier.max(*tier);
        }
    }
    (seen_tier, saw_revocation)
}

/// Fetch `pubkey`'s raw Bug Hunter standing from the issuer: the seen kind-8
/// awards (id + tier) whose `a` points at one of our tier definitions, plus the
/// set of award ids the issuer has revoked (kind-5). Queries the full pool.
async fn fetch_bug_hunter_raw(pubkey: &PublicKey) -> Result<(Vec<(EventId, u8)>, HashSet<EventId>), String> {
    let client = crate::state::nostr_client().ok_or("Nostr client not initialized")?;
    let issuer = *BUG_HUNTER_ISSUER;
    let issuer_hex = issuer.to_hex();

    // Awards: kind 8 signed by the issuer, p-tagging this user.
    let award_filter = Filter::new()
        .author(issuer)
        .kind(Kind::Custom(8))
        .custom_tag(SingleLetterTag::LOWERCASE_P, pubkey.to_hex())
        .limit(64);
    let mut awards: Vec<(EventId, u8)> = Vec::new();
    let mut stream = client
        .stream_events(award_filter)
        .timeout(crate::relay_request_timeout(std::time::Duration::from_secs(10)))
        .await
        .map_err(|e| e.to_string())?;
    while let Some((_relay, res)) = stream.next().await {
        let Ok(ev) = res else { continue };
        let coord = ev.tags.iter().find_map(|t| {
            let s = t.as_slice();
            if s.first().map(|k| k == "a").unwrap_or(false) {
                s.get(1).cloned()
            } else {
                None
            }
        });
        if let Some(c) = coord {
            if let Some(tier) = tier_from_coord(&c, &issuer_hex) {
                awards.push((ev.id, tier));
            }
        }
    }

    // Revocations: kind 5 from the issuer (NIP-09); each `e` tag names a revoked award.
    let revoke_filter = Filter::new()
        .author(issuer)
        .kind(Kind::Custom(5))
        .limit(256);
    let mut revoked: HashSet<EventId> = HashSet::new();
    let mut rstream = client
        .stream_events(revoke_filter)
        .timeout(crate::relay_request_timeout(std::time::Duration::from_secs(10)))
        .await
        .map_err(|e| e.to_string())?;
    while let Some((_relay, res)) = rstream.next().await {
        let Ok(ev) = res else { continue };
        for t in ev.tags.iter() {
            let s = t.as_slice();
            if s.first().map(|k| k == "e").unwrap_or(false) {
                if let Some(id) = s.get(1).and_then(|h| EventId::from_hex(h).ok()) {
                    revoked.insert(id);
                }
            }
        }
    }

    if awards.is_empty() && !any_relay_connected(&client).await {
        return Err("no relay connected".into());
    }
    Ok((awards, revoked))
}

/// Highest non-revoked tier + whether a seen award was revoked, for displaying
/// another user's badge. Our own status goes through `refresh_own_bug_hunter`,
/// which also catches revocation of a since-purged award.
pub async fn fetch_bug_hunter_tier(pubkey: &PublicKey) -> Result<(u8, bool), String> {
    let (awards, revoked) = fetch_bug_hunter_raw(pubkey).await?;
    let (tier, revoked_seen) = fold_bug_hunter(&awards, &revoked);
    note_own_bug_hunter(pubkey, tier);
    Ok((tier, revoked_seen))
}

/// A live look at our own Bug Hunter awards raises the cached tier at once, so a
/// badge the profile shows always carries its perks. Only ever upward: a lower
/// result is a relay that missed the award, and downgrades go through
/// `refresh_own_bug_hunter`'s revocation check.
fn note_own_bug_hunter(pubkey: &PublicKey, tier: u8) {
    if crate::state::my_public_key().as_ref() != Some(pubkey) || tier <= bug_hunter_tier() {
        return;
    }
    store_bug_hunter_tier(tier);
    crate::log_info!("[Badges] bug hunter tier {} confirmed via on-demand check", tier);
    emit_badges();
}

fn read_cached_award_ids() -> Vec<EventId> {
    crate::db::get_sql_setting(BADGE_BUG_HUNTER_AWARD_IDS_KEY.to_string())
        .ok()
        .flatten()
        .map(|s| s.split(',').filter_map(|h| EventId::from_hex(h.trim()).ok()).collect())
        .unwrap_or_default()
}

fn write_cached_award_ids(ids: &[EventId]) {
    let csv = ids.iter().map(|id| id.to_hex()).collect::<Vec<_>>().join(",");
    let _ = crate::db::set_sql_setting(BADGE_BUG_HUNTER_AWARD_IDS_KEY.to_string(), csv);
}

/// The tier to hold after a fetch: a higher award applies at once, a lower view only
/// when the issuer's revocation was seen, since a missing award is a relay that missed it.
fn next_tier(cached: u8, seen: u8, saw_revocation: bool) -> u8 {
    if seen > cached || (seen < cached && saw_revocation) { seen } else { cached }
}

/// Resolve + persist our own Bug Hunter tier (called post-sync). Sticky cache:
/// upgrades apply immediately; a downgrade is honored ONLY on a seen revocation,
/// never a mere absent award (flaky relay). The ids of the awards backing the
/// current tier are cached so a revocation still resolves after the relays purge
/// the award itself (NIP-09 deletion).
async fn refresh_own_bug_hunter() {
    let Some(pk) = crate::state::my_public_key() else {
        return;
    };

    // A failed fetch, or an empty one for an account with nothing cached yet, is
    // most often a relay still busy with the sync: ask again before settling.
    const ATTEMPTS: u8 = 3;
    let mut attempt = 0;
    let (awards, revoked) = loop {
        attempt += 1;
        match fetch_bug_hunter_raw(&pk).await {
            Ok((awards, revoked)) if !awards.is_empty() || bug_hunter_tier() > 0 || attempt >= ATTEMPTS => {
                break (awards, revoked);
            }
            Ok(_) => crate::log_info!("[Badges] no bug hunter award found (attempt {attempt}/{ATTEMPTS})"),
            Err(e) if attempt >= ATTEMPTS => {
                crate::log_warn!("[Badges] bug hunter fetch failed: {}", e);
                return;
            }
            Err(e) => crate::log_warn!("[Badges] bug hunter fetch attempt {attempt}/{ATTEMPTS} failed: {e}"),
        }
        crate::rt::time::sleep(std::time::Duration::from_secs(20)).await;
    };

    // Highest non-revoked seen tier + the award ids backing it.
    let mut seen_tier = 0u8;
    let mut active_ids: Vec<EventId> = Vec::new();
    let mut seen_revoked = false;
    for (id, tier) in &awards {
        if revoked.contains(id) {
            seen_revoked = true;
        } else {
            seen_tier = seen_tier.max(*tier);
            active_ids.push(*id);
        }
    }
    // Also honor a revocation of an award we previously cached but that the relays
    // have since purged (so it's no longer in `awards` to match directly).
    let cached_revoked = read_cached_award_ids().iter().any(|id| revoked.contains(id));
    let saw_revocation = seen_revoked || cached_revoked;

    // Re-read the current account before any write: never persist A's state into B.
    if crate::state::my_public_key().as_ref() != Some(&pk) {
        return;
    }

    let cached = bug_hunter_tier();
    let new_tier = next_tier(cached, seen_tier, saw_revocation);

    // Remember the awards behind the current tier — but only when we actually saw
    // awards, so a flaky empty fetch never wipes the memory we need for revocation.
    if !awards.is_empty() {
        write_cached_award_ids(&active_ids);
    }
    if new_tier == cached {
        return;
    }
    store_bug_hunter_tier(new_tier);
    crate::log_info!("[Badges] bug hunter tier {} -> {}", cached, new_tier);
    emit_badges();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fawkes_claim_window_boundaries() {
        // Correct content, inside the window.
        assert!(is_valid_fawkes_claim("fawkes_badge_claimed", FAWKES_DAY_START));
        assert!(is_valid_fawkes_claim("fawkes_badge_claimed", FAWKES_DAY_END - 1));
        // End is exclusive.
        assert!(!is_valid_fawkes_claim("fawkes_badge_claimed", FAWKES_DAY_END));
        // Before the window.
        assert!(!is_valid_fawkes_claim("fawkes_badge_claimed", FAWKES_DAY_START - 1));
        // Wrong / empty content, even inside the window.
        assert!(!is_valid_fawkes_claim("", FAWKES_DAY_START));
        assert!(!is_valid_fawkes_claim("something_else", FAWKES_DAY_START));
    }

    #[test]
    fn bug_hunter_issuer_npub_is_valid() {
        // A malformed hardcoded issuer would panic when the LazyLock forces.
        let _ = *BUG_HUNTER_ISSUER;
        assert!(PublicKey::from_bech32(BUG_HUNTER_ISSUER_NPUB).is_ok());
    }

    #[test]
    fn tier_from_coord_trusts_only_issuer_kind_and_slug() {
        let issuer = "abc123";
        assert_eq!(tier_from_coord("30009:abc123:bug-hunter-tier-1", issuer), Some(1));
        assert_eq!(tier_from_coord("30009:abc123:bug-hunter-tier-2", issuer), Some(2));
        assert_eq!(tier_from_coord("30009:abc123:bug-hunter-tier-3", issuer), Some(3));
        // Forged: a definition minted under another key.
        assert_eq!(tier_from_coord("30009:evil:bug-hunter-tier-3", issuer), None);
        // Wrong kind.
        assert_eq!(tier_from_coord("30008:abc123:bug-hunter-tier-3", issuer), None);
        // Unknown slug.
        assert_eq!(tier_from_coord("30009:abc123:bug-hunter-tier-9", issuer), None);
    }

    #[test]
    fn fold_bug_hunter_highest_non_revoked_with_revocation_flag() {
        let id = |b: u8| EventId::from_hex(&format!("{:02x}", b).repeat(32)).unwrap();
        // No revocations: highest tier, flag clear.
        assert_eq!(fold_bug_hunter(&[(id(1), 1), (id(3), 3), (id(2), 2)], &HashSet::new()), (3, false));
        // Revoke the tier-3 award: tier drops to 2, flag set.
        let revoked: HashSet<EventId> = [id(3)].into_iter().collect();
        assert_eq!(fold_bug_hunter(&[(id(1), 1), (id(3), 3), (id(2), 2)], &revoked), (2, true));
        // No awards: tier 0, no revocation.
        assert_eq!(fold_bug_hunter(&[], &HashSet::new()), (0, false));
    }

    fn init_test_db() -> (tempfile::TempDir, std::sync::MutexGuard<'static, ()>, Keys) {
        let guard = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        crate::db::close_database();
        crate::db::clear_id_caches();
        let tmp = tempfile::tempdir().unwrap();
        let me = Keys::generate();
        let account = me.public_key().to_bech32().unwrap();
        crate::db::set_app_data_dir(crate::db::shared_test_data_dir().to_path_buf());
        crate::db::set_current_account(account.clone()).unwrap();
        crate::db::init_database(&account).unwrap();
        crate::state::set_my_public_key(me.public_key());
        (tmp, guard, me)
    }

    fn rename_settings(from: &str, to: &str) {
        let conn = crate::db::get_write_connection_guard_static().unwrap();
        conn.execute(&format!("ALTER TABLE {from} RENAME TO {to}"), []).unwrap();
    }

    #[test]
    fn a_missing_award_never_lowers_the_tier_and_a_revocation_does() {
        assert_eq!(next_tier(0, 2, false), 2, "a new award applies at once");
        assert_eq!(next_tier(2, 3, false), 3, "an upgrade applies at once");
        assert_eq!(next_tier(2, 0, false), 2, "a relay that missed the award changes nothing");
        assert_eq!(next_tier(3, 1, false), 3, "nor does one that only holds an older award");
        assert_eq!(next_tier(2, 0, true), 0, "a seen revocation takes it away");
        assert_eq!(next_tier(3, 2, true), 2, "or down to what remains");
    }

    #[test]
    fn a_failed_settings_read_keeps_the_perks_it_last_saw() {
        let (_tmp, _guard, _me) = init_test_db();
        store_bug_hunter_tier(2);
        assert_eq!(effective_tier(), 2);

        rename_settings("settings", "settings_away");
        assert_eq!(bug_hunter_tier(), 2, "an unreadable cache is not an absent badge");
        assert_eq!(effective_tier(), 2);
        assert_eq!(crate::emoji_packs::effective_max_emojis_per_pack(), 60);
        assert_eq!(crate::emoji_packs::effective_max_equipped_packs(), 9);
        assert_eq!(crate::pinned_chats::effective_max_pinned(), 9);
        rename_settings("settings_away", "settings");

        store_vector_badge();
        rename_settings("settings", "settings_away");
        assert!(has_vector_badge());
        assert_eq!(effective_tier(), 3);
        rename_settings("settings_away", "settings");
    }

    #[test]
    fn v2026_claims_count_inside_the_window_and_test_claims_lapse() {
        assert!(is_valid_v2026_claim(V2026_CONTENT, V2026_START));
        assert!(is_valid_v2026_claim(V2026_CONTENT, V2026_END - 1));
        assert!(!is_valid_v2026_claim(V2026_CONTENT, V2026_START - 1), "before the window");
        assert!(!is_valid_v2026_claim(V2026_CONTENT, V2026_END), "the window is half-open");
        assert!(!is_valid_v2026_claim("fawkes_badge_claimed", V2026_START), "another badge's claim");

        assert!(v2026_held(Some("true"), u64::MAX), "a launch claim is kept");
        assert!(v2026_held(Some("1000"), 999), "a test claim, before it expires");
        assert!(!v2026_held(Some("1000"), 1000), "and not after");
        assert!(!v2026_held(None, 0));
    }

    #[test]
    fn v2026_grants_the_first_tier_and_no_more() {
        assert_eq!(badge_tier(0, false, true), 1);
        assert_eq!(badge_tier(2, false, true), 2, "a higher Bug Hunter tier wins");
        assert_eq!(badge_tier(0, true, true), 3, "and 2025's full premium");
        assert_eq!(badge_tier(0, false, false), 0);
    }

    #[test]
    fn colour_only_sends_with_a_badge() {
        let (_tmp, _guard, _me) = init_test_db();
        let colours = |text: &str| {
            let (plain, spans) = crate::text_spans::extract_own(text);
            (plain, spans.iter().filter(|s| matches!(s, crate::text_spans::TextSpan::Color(_))).count())
        };
        assert!(!has_text_effects());
        assert_eq!(colours("I <rainbow>love</rainbow> it"), ("I love it".into(), 0), "the markup goes, its colour with it");
        store_bug_hunter_tier(1);
        assert!(has_text_effects());
        assert_eq!(colours("I <rainbow>love</rainbow> it"), ("I love it".into(), 1));
    }

    #[test]
    fn perks_hold_across_a_restart_with_no_network() {
        let (_tmp, _guard, me) = init_test_db();
        store_bug_hunter_tier(3);
        let account = me.public_key().to_bech32().unwrap();

        // A relaunch: a new session over the same database, nothing fetched.
        crate::db::close_database();
        crate::db::set_current_account(account.clone()).unwrap();
        crate::db::init_database(&account).unwrap();
        crate::state::set_my_public_key(me.public_key());
        assert_eq!(bug_hunter_tier(), 3);
        assert_eq!(effective_tier(), 3);
        assert_eq!(crate::emoji_packs::effective_max_emojis_per_pack(), 90);
        assert_eq!(crate::emoji_packs::effective_max_equipped_packs(), usize::MAX);
    }

    #[test]
    fn a_live_look_at_our_own_awards_raises_the_cache_and_never_lowers_it() {
        let (_tmp, _guard, me) = init_test_db();
        assert_eq!(effective_tier(), 0);

        note_own_bug_hunter(&me.public_key(), 2);
        assert_eq!(bug_hunter_tier(), 2, "the badge the profile shows carries its perks");
        assert_eq!(crate::emoji_packs::effective_max_emojis_per_pack(), 60);

        note_own_bug_hunter(&me.public_key(), 1);
        note_own_bug_hunter(&me.public_key(), 0);
        assert_eq!(bug_hunter_tier(), 2, "a lower live view is a relay that missed the award");

        note_own_bug_hunter(&Keys::generate().public_key(), 3);
        assert_eq!(bug_hunter_tier(), 2, "someone else's awards are not ours");
    }

    #[test]
    fn perks_stay_with_their_account_across_a_swap() {
        let (_tmp, _guard, a) = init_test_db();
        store_bug_hunter_tier(3);
        store_vector_badge();
        assert_eq!(effective_tier(), 3);

        let b = Keys::generate();
        let b_npub = b.public_key().to_bech32().unwrap();
        crate::db::close_database();
        crate::db::set_current_account(b_npub.clone()).unwrap();
        crate::db::init_database(&b_npub).unwrap();
        crate::state::set_my_public_key(b.public_key());
        assert_eq!(effective_tier(), 0, "another account's badges never carry over");
        assert!(!has_vector_badge());
        assert_eq!(crate::emoji_packs::effective_max_emojis_per_pack(), 30);

        let a_npub = a.public_key().to_bech32().unwrap();
        crate::db::close_database();
        crate::db::set_current_account(a_npub.clone()).unwrap();
        crate::db::init_database(&a_npub).unwrap();
        crate::state::set_my_public_key(a.public_key());
        assert_eq!(effective_tier(), 3, "and swapping back finds them where they were");
    }
}
