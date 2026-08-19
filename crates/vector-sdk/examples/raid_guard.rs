//! Anti-raid moderation bot for Communities.
//!
//! Sybil raids don't look like spam from one account — they look like one message
//! each from a hundred fresh accounts, so per-sender rate limits never fire. The
//! signal that *does* survive is the cohort: many DISTINCT senders posting the
//! same thing inside one window. That's what this watches, and it bans the whole
//! cohort as a single moderation unit.
//!
//! Two modes, both on by default:
//!   * startup **sweep** — scans recent history and contains a raid already in progress
//!   * live **watch** — contains new waves as they arrive
//!
//! Admins and the owner are never moderated, and `RAID_GUARD_DRY_RUN=1` reports
//! without banning so you can check the verdict before arming it.
//!
//! Run with:
//! ```sh
//! VECTOR_NSEC=nsec1... RAID_GUARD_DRY_RUN=1 \
//!   cargo run -p vector_sdk --example raid_guard
//! ```
//!
//! Tuning (all optional):
//!   `RAID_GUARD_COMMUNITY`  restrict to one community id (default: every community it can moderate)
//!   `RAID_GUARD_DRY_RUN`    `1` = report only, ban nothing
//!   `RAID_GUARD_SWEEP`      `0` = skip the startup history sweep
//!   `RAID_GUARD_SWEEP_DEPTH` messages of history to scan per channel (default 500)
//!   `RAID_MIN_COHORT`       distinct senders that make a raid (default 5)
//!   `RAID_WINDOW_SECS`      how far apart cohort members may be (default 120)
//!   `RAID_SETTLE_SECS`      quiet period before a batch is banned (default 6)
//!   `RAID_MAX_BANS`         circuit breaker for one run (default 400)
//!   `RAID_GUARD_ANNOUNCE`   `0` = contain silently, posting nothing

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::sync::Mutex;
use vector_sdk::{BotEvent, Community, VectorBot};

/// Below this, a shared message is too generic to convict on ("gm", "lol", "+1"),
/// so a cohort of that shape needs `SHORT_TEXT_FACTOR` times the members instead.
const MIN_SKELETON_LEN: usize = 8;
const SHORT_TEXT_FACTOR: usize = 3;
/// The wire caps a banlist at 500 entries and rejects an over-cap batch whole.
const BANLIST_CAP: usize = 500;
const BAN_CHUNK: usize = 100;

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

fn env_flag(key: &str, default: bool) -> bool {
    match std::env::var(key).ok().as_deref() {
        Some("0") | Some("false") | Some("no") => false,
        Some(_) => true,
        None => default,
    }
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
}

/// Collapse a message to the shape a raid script repeats: case, punctuation,
/// spacing and digit variation are all cheap for an attacker to vary per-account.
fn skeleton(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_alphanumeric())
        .filter(|c| !c.is_numeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

#[derive(Clone)]
struct Config {
    min_cohort: usize,
    window_ms: u64,
    settle_ms: u64,
    max_bans: usize,
    dry_run: bool,
    announce: bool,
    only_community: Option<String>,
}

/// One observed post, kept only as long as the detection window needs it.
struct Post {
    at: u64,
    npub: String,
    skeleton: String,
}

#[derive(Default)]
struct CommunityState {
    posts: Vec<Post>,
    /// Convicted, not yet banned. Drains on the next settle tick.
    pending: HashSet<String>,
    /// Already banned this run — never re-ban, never re-announce.
    banned: HashSet<String>,
    last_detect_ms: u64,
}

struct Guard {
    cfg: Config,
    by_community: HashMap<String, CommunityState>,
    total_banned: usize,
    tripped: bool,
}

impl Guard {
    fn new(cfg: Config) -> Self {
        Self { cfg, by_community: HashMap::new(), total_banned: 0, tripped: false }
    }

    fn guards(&self, community_id: &str) -> bool {
        self.cfg.only_community.as_deref().map_or(true, |only| only == community_id)
    }

    /// Record a post and return the cohort it convicts, if it completes one.
    fn observe(&mut self, community_id: &str, npub: &str, text: &str, at: u64) -> Vec<String> {
        let cfg = self.cfg.clone();
        let state = self.by_community.entry(community_id.to_string()).or_default();

        let cutoff = at.saturating_sub(cfg.window_ms);
        state.posts.retain(|p| p.at >= cutoff);

        let skel = skeleton(text);
        if skel.is_empty() {
            return Vec::new();
        }
        if !state.posts.iter().any(|p| p.npub == npub && p.skeleton == skel) {
            state.posts.push(Post { at, npub: npub.to_string(), skeleton: skel.clone() });
        }

        let cohort: Vec<String> = state
            .posts
            .iter()
            .filter(|p| p.skeleton == skel)
            .map(|p| p.npub.clone())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();

        // Generic one-liners need a bigger crowd before they mean anything.
        let threshold = if skel.len() >= MIN_SKELETON_LEN {
            cfg.min_cohort
        } else {
            cfg.min_cohort * SHORT_TEXT_FACTOR
        };
        if cohort.len() < threshold {
            return Vec::new();
        }

        let fresh: Vec<String> = cohort
            .into_iter()
            .filter(|n| !state.banned.contains(n) && !state.pending.contains(n))
            .collect();
        if !fresh.is_empty() {
            state.last_detect_ms = at;
            for n in &fresh {
                state.pending.insert(n.clone());
            }
        }
        fresh
    }

    /// Communities whose pending batch has gone quiet long enough to ban.
    fn due(&mut self, now: u64) -> Vec<(String, Vec<String>)> {
        let settle = self.cfg.settle_ms;
        self.by_community
            .iter_mut()
            .filter(|(_, s)| !s.pending.is_empty() && now.saturating_sub(s.last_detect_ms) >= settle)
            .map(|(id, s)| (id.clone(), s.pending.drain().collect()))
            .collect()
    }

    fn record_banned(&mut self, community_id: &str, npubs: &[String]) {
        let state = self.by_community.entry(community_id.to_string()).or_default();
        for n in npubs {
            state.banned.insert(n.clone());
        }
        self.total_banned += npubs.len();
    }

    fn requeue(&mut self, community_id: &str, npubs: Vec<String>) {
        let state = self.by_community.entry(community_id.to_string()).or_default();
        for n in npubs {
            state.pending.insert(n);
        }
    }
}

/// Ban a convicted batch as one moderation unit, chunked under the wire's banlist cap.
async fn contain(bot: &VectorBot, guard: &Arc<Mutex<Guard>>, community_id: &str, mut targets: Vec<String>) {
    if targets.is_empty() {
        return;
    }
    targets.sort();

    let community = bot.community(community_id);
    let (dry_run, announce, max_bans, already) = {
        let g = guard.lock().await;
        (g.cfg.dry_run, g.cfg.announce, g.cfg.max_bans, g.total_banned)
    };

    // Never moderate the people who could stop us, and never ourselves.
    let mut safe = Vec::new();
    for npub in targets {
        if npub == bot.npub() {
            continue;
        }
        if community.member(&npub).is_admin() {
            println!("  skip {} (admin/owner)", short(&npub));
            continue;
        }
        safe.push(npub);
    }
    if safe.is_empty() {
        return;
    }

    // A bug here bans the whole community, so cap what one run can ever do.
    let room = max_bans.saturating_sub(already);
    if room == 0 {
        let mut g = guard.lock().await;
        if !g.tripped {
            g.tripped = true;
            eprintln!(
                "!! circuit breaker: {max_bans} bans this run. Refusing more. Raise RAID_MAX_BANS to continue."
            );
        }
        g.requeue(community_id, safe);
        return;
    }
    if safe.len() > room {
        eprintln!("!! circuit breaker: trimming batch to the remaining {room} of RAID_MAX_BANS");
        safe.truncate(room);
    }

    if dry_run {
        println!("[dry-run] would ban {} in {}:", safe.len(), short(community_id));
        for n in &safe {
            println!("  {n}");
        }
        guard.lock().await.record_banned(community_id, &safe);
        return;
    }

    let mut banned = Vec::new();
    for chunk in safe.chunks(BAN_CHUNK) {
        let refs: Vec<&str> = chunk.iter().map(String::as_str).collect();
        match community.ban_many(&refs).await {
            Ok(()) => {
                println!("Banned {} accounts in {}", chunk.len(), short(community_id));
                banned.extend_from_slice(chunk);
            }
            Err(e) => {
                eprintln!("Ban batch of {} failed: {e}", chunk.len());
                if e.to_string().contains("500") || e.to_string().contains("cap") {
                    eprintln!(
                        "!! the {BANLIST_CAP}-entry banlist is full. Further bans need existing \
                         entries lifted, or the community re-founded."
                    );
                }
                guard.lock().await.requeue(community_id, chunk.to_vec());
                break;
            }
        }
    }

    if banned.is_empty() {
        return;
    }
    guard.lock().await.record_banned(community_id, &banned);

    if announce {
        if let Some(channel_id) = first_channel(&community).await {
            let msg = format!("Raid contained: {} accounts banned.", banned.len());
            let _ = bot.channel(channel_id).send(&msg).await;
        }
    }
}

async fn first_channel(community: &Community) -> Option<String> {
    community.channels().await.into_iter().find(|c| !c.is_private()).map(|c| c.id().to_string())
}

fn short(id: &str) -> String {
    id.chars().take(16).collect()
}

/// Replay recent history through the same detector, so a raid that landed before
/// the bot started is contained too.
async fn sweep(bot: &VectorBot, guard: &Arc<Mutex<Guard>>, depth: usize) {
    println!("Sweeping the last {depth} messages per channel for raids already in progress...");
    let mut found = 0usize;

    for community in bot.communities().await {
        if !guard.lock().await.guards(community.id()) {
            continue;
        }
        for channel in community.channels().await {
            if !channel.is_readable() {
                continue;
            }
            let history = bot.community(community.id()).channel(channel.id()).history(depth).await;
            let mut convicted: HashSet<String> = HashSet::new();
            for msg in &history {
                if msg.mine {
                    continue;
                }
                let Some(npub) = msg.npub.as_deref() else { continue };
                let hits = guard.lock().await.observe(community.id(), npub, &msg.content, msg.at);
                convicted.extend(hits);
            }
            if !convicted.is_empty() {
                println!(
                    "  #{} — {} raid accounts across {} messages",
                    channel.name(),
                    convicted.len(),
                    history.len()
                );
                found += convicted.len();
            }
        }

        // Drain whatever the replay convicted; historical timestamps never settle on their own.
        let batch: Vec<String> = {
            let mut g = guard.lock().await;
            let state = g.by_community.entry(community.id().to_string()).or_default();
            state.pending.drain().collect()
        };
        contain(bot, guard, community.id(), batch).await;
    }

    if found == 0 {
        println!("Sweep clean — no raid in recent history.");
    }
}

#[tokio::main]
async fn main() -> vector_sdk::Result<()> {
    let cfg = Config {
        min_cohort: env_usize("RAID_MIN_COHORT", 5),
        window_ms: env_usize("RAID_WINDOW_SECS", 120) as u64 * 1000,
        settle_ms: env_usize("RAID_SETTLE_SECS", 6) as u64 * 1000,
        max_bans: env_usize("RAID_MAX_BANS", 400),
        dry_run: env_flag("RAID_GUARD_DRY_RUN", false),
        announce: env_flag("RAID_GUARD_ANNOUNCE", true),
        only_community: std::env::var("RAID_GUARD_COMMUNITY").ok().filter(|s| !s.is_empty()),
    };

    let mut builder = VectorBot::builder().public();
    if let Ok(nsec) = std::env::var("VECTOR_NSEC") {
        builder = builder.nsec(nsec);
    }
    let bot = builder.build().await?;

    println!("Raid guard online as {}", bot.npub());
    println!(
        "  cohort >= {} identical posts / {}s window, settle {}s, cap {} bans{}",
        cfg.min_cohort,
        cfg.window_ms / 1000,
        cfg.settle_ms / 1000,
        cfg.max_bans,
        if cfg.dry_run { "  [DRY RUN — nothing will be banned]" } else { "" }
    );
    if let Some(only) = &cfg.only_community {
        println!("  guarding only {}", short(only));
    }

    let guard = Arc::new(Mutex::new(Guard::new(cfg)));

    // Settle ticker: batches convictions so one wave costs one banlist edition
    // rather than one per raider.
    if env_flag("RAID_GUARD_SWEEP", true) {
        sweep(&bot, &guard, env_usize("RAID_GUARD_SWEEP_DEPTH", 500)).await;
    }

    // Started only after the sweep: replayed history is already past its settle
    // window, so a ticker running alongside would drain the sweep's batch
    // mid-scan and split one wave across several banlist editions.
    let ticker = {
        let guard = guard.clone();
        let bot = bot.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(1)).await;
                let due = guard.lock().await.due(now_ms());
                for (community_id, batch) in due {
                    contain(&bot, &guard, &community_id, batch).await;
                }
            }
        })
    };

    let watch = guard.clone();
    bot.on_event(move |_bot, event| {
        let guard = watch.clone();
        async move {
            match event {
                BotEvent::Ready { communities } => {
                    println!("Watching {communities} communities live.");
                }
                BotEvent::Message(msg) if msg.is_group && !msg.is_mine() => {
                    let Some(community) = msg.community() else { return };
                    if !guard.lock().await.guards(community.id()) {
                        return;
                    }
                    let Some(npub) = msg.message.npub.clone() else { return };
                    let hits = {
                        let mut g = guard.lock().await;
                        g.observe(community.id(), &npub, msg.text(), msg.message.at)
                    };
                    if !hits.is_empty() {
                        println!(
                            "Raid detected in {}: {} accounts posting \"{}\"",
                            short(community.id()),
                            hits.len(),
                            msg.text().chars().take(40).collect::<String>()
                        );
                    }
                }
                _ => {}
            }
        }
    })
    .await?;

    ticker.abort();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMMUNITY: &str = "vector";

    fn guard(min_cohort: usize) -> Guard {
        Guard::new(Config {
            min_cohort,
            window_ms: 120_000,
            settle_ms: 6_000,
            max_bans: 400,
            dry_run: true,
            announce: false,
            only_community: None,
        })
    }

    fn npub(i: usize) -> String {
        format!("npub1raider{i:04}")
    }

    /// The live raid's exact shape: one message each from many fresh accounts,
    /// roughly one per second. Per-sender limits see nothing; the cohort is the signal.
    #[test]
    fn a_sybil_wave_convicts_every_participant() {
        let mut g = guard(5);
        let mut convicted: HashSet<String> = HashSet::new();
        for i in 0..100 {
            let at = 1_787_163_101_000 + (i as u64 * 1_000);
            convicted.extend(g.observe(COMMUNITY, &npub(i), "hello world", at));
        }
        assert_eq!(convicted.len(), 100, "every raider in the wave must be convicted");
    }

    /// The threshold has to be reached before anyone is convicted, so an
    /// ordinary coincidence of four people never bans anyone.
    #[test]
    fn a_cohort_below_the_threshold_convicts_nobody() {
        let mut g = guard(5);
        for i in 0..4 {
            let hits = g.observe(COMMUNITY, &npub(i), "hello world", 1_000 + i as u64 * 1_000);
            assert!(hits.is_empty(), "convicted at only {} senders", i + 1);
        }
    }

    #[test]
    fn ordinary_conversation_never_convicts() {
        let mut g = guard(5);
        let chat = [
            "has anyone tried the new build yet",
            "yeah it fixed the scroll flicker for me",
            "nice, the android one still crashes though",
            "what device? mine is fine on a pixel",
            "samsung, i'll grab a logcat",
            "please do, that would help a lot",
        ];
        for (i, line) in chat.iter().enumerate() {
            let hits = g.observe(COMMUNITY, &npub(i), line, 1_000 + i as u64 * 5_000);
            assert!(hits.is_empty(), "convicted real conversation: {line}");
        }
    }

    /// "gm" from five people is a morning, not a raid.
    #[test]
    fn a_short_greeting_needs_a_bigger_crowd() {
        let mut g = guard(5);
        for i in 0..14 {
            let hits = g.observe(COMMUNITY, &npub(i), "gm", 1_000 + i as u64 * 1_000);
            assert!(hits.is_empty(), "convicted \"gm\" at only {} senders", i + 1);
        }
        let hits = g.observe(COMMUNITY, &npub(14), "gm", 20_000);
        assert_eq!(hits.len(), 15, "a 15-strong \"gm\" flood is a raid");
    }

    /// One account flooding is a different problem, and this bot must not
    /// mistake it for a wave: the cohort never grows past one.
    #[test]
    fn one_account_repeating_itself_is_not_a_cohort() {
        let mut g = guard(5);
        for i in 0..20 {
            let hits = g.observe(COMMUNITY, "npub1lonespammer", "buy my coin now", 1_000 + i * 1_000);
            assert!(hits.is_empty(), "a single flooder was treated as a raid");
        }
    }

    /// Case, punctuation, spacing and per-account numbering are all free for an
    /// attacker to vary, so none of them may split the cohort.
    #[test]
    fn cosmetic_variation_does_not_evade_the_cohort() {
        let mut g = guard(5);
        let variants = [
            "hello world",
            "Hello World!",
            "hello  world",
            "HELLO, WORLD.",
            "hello world 42",
        ];
        let mut convicted: HashSet<String> = HashSet::new();
        for (i, text) in variants.iter().enumerate() {
            convicted.extend(g.observe(COMMUNITY, &npub(i), text, 1_000 + i as u64 * 1_000));
        }
        assert_eq!(convicted.len(), 5, "cosmetic variation split the cohort");
    }

    /// Five people saying the same thing across a week is not a wave.
    #[test]
    fn a_slow_drip_outside_the_window_never_accumulates() {
        let mut g = guard(5);
        for i in 0..10 {
            let at = 1_000 + i as u64 * 200_000; // ~3.3min apart, window is 2min
            let hits = g.observe(COMMUNITY, &npub(i), "hello world", at);
            assert!(hits.is_empty(), "a slow drip accumulated into a raid at post {i}");
        }
    }

    /// A raider already banned this run must never re-enter a later batch.
    #[test]
    fn conviction_never_repeats_for_the_same_account() {
        let mut g = guard(5);
        let mut first: HashSet<String> = HashSet::new();
        for i in 0..6 {
            first.extend(g.observe(COMMUNITY, &npub(i), "hello world", 1_000 + i as u64 * 1_000));
        }
        assert_eq!(first.len(), 6);
        let batch: Vec<String> = g.by_community.get_mut(COMMUNITY).unwrap().pending.drain().collect();
        g.record_banned(COMMUNITY, &batch);

        let again = g.observe(COMMUNITY, &npub(0), "hello world", 8_000);
        assert!(again.is_empty(), "an already-banned raider was convicted twice");
    }

    /// The batch drains only after the wave goes quiet, so one wave costs one
    /// banlist edition instead of one per raider.
    #[test]
    fn a_batch_drains_only_after_the_wave_settles() {
        let mut g = guard(5);
        for i in 0..6 {
            g.observe(COMMUNITY, &npub(i), "hello world", 100_000 + i as u64 * 1_000);
        }
        let last = 105_000;
        assert!(g.due(last + 3_000).is_empty(), "drained while the wave was still arriving");

        let due = g.due(last + 6_000);
        assert_eq!(due.len(), 1, "the settled wave did not drain");
        assert_eq!(due[0].1.len(), 6, "the whole wave must drain as one batch");
    }

    /// A bug in detection must not be able to empty a community.
    #[test]
    fn the_circuit_breaker_bounds_one_run() {
        let mut g = guard(5);
        g.cfg.max_bans = 10;
        let banned: Vec<String> = (0..10).map(npub).collect();
        g.record_banned(COMMUNITY, &banned);
        assert_eq!(g.total_banned, 10, "the breaker's ceiling must be reachable");
    }
}
