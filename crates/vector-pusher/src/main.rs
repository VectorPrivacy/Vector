//! The blind pusher. It listens for sealed push requests addressed to it, opens the one part
//! sealed to it (a device's push address and VAPID key), and forwards an encrypted body it
//! cannot read. It never learns who asked or who the device belongs to, and keeps nothing:
//! no endpoints, no keys, no delivery log. Counts only.
//!
//! Environment:
//! - `PUSHER_KEY_FILE`: hex secret key, created (0600) on first run. Default `pusher.key`.
//! - `PUSHER_RELAYS`: comma-separated relays to listen on. Default Vector's.
//! - `PUSHER_SUBJECT`: the VAPID `sub` claim push services may contact. Default
//!   `https://vectorapp.io`.
//! - `PUSHER_REPORT_SECS`: how often the counts are printed. Default 600.
//! - `PUSHER_HOLDS_FILE`: where held capability ids live across restarts. Default
//!   `holds.json`. Random ids and times, nothing else: the one thing this keeps on disk.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use futures_util::StreamExt;
use nostr_sdk::prelude::*;
use vector_push::{request, ticket, vapid, webpush, Incoming, Request, HOLD_FOREVER};

/// A contact may wake a device about this often: a burst, then one per interval.
const BURST: f64 = 20.0;
const REFILL_EVERY: Duration = Duration::from_secs(3);
/// A push service that answered 404/410 is not asked again for this long.
const GONE_FOR: Duration = Duration::from_secs(3600);
const MAX_TTL: u32 = 7 * 24 * 3600;

#[derive(Default)]
struct Stats {
    delivered: AtomicU64,
    gone: AtomicU64,
    failed: AtomicU64,
    rejected: AtomicU64,
    limited: AtomicU64,
    held: AtomicU64,
    controls: AtomicU64,
}

struct Pusher {
    keys: Keys,
    subject: String,
    http: reqwest::Client,
    stats: Stats,
    seen: Mutex<(HashSet<EventId>, VecDeque<EventId>)>,
    buckets: Mutex<HashMap<String, (f64, Instant)>>,
    gone: Mutex<HashMap<String, Instant>>,
    /// Capability id → unix seconds its pushes are held until (`HOLD_FOREVER` for a block).
    holds: Mutex<HashMap<String, u64>>,
    holds_file: String,
}

#[tokio::main]
async fn main() {
    let keys = load_keys(&std::env::var("PUSHER_KEY_FILE").unwrap_or_else(|_| "pusher.key".into()));
    let relays: Vec<String> = std::env::var("PUSHER_RELAYS")
        .map(|s| s.split(',').map(|r| r.trim().to_string()).filter(|r| !r.is_empty()).collect())
        .unwrap_or_else(|_| vector_push::DEFAULT_PUSHER_RELAYS.iter().map(|r| r.to_string()).collect());
    let subject = std::env::var("PUSHER_SUBJECT").unwrap_or_else(|_| "https://vectorapp.io".into());
    let holds_file = std::env::var("PUSHER_HOLDS_FILE").unwrap_or_else(|_| "holds.json".into());
    let holds: HashMap<String, u64> = std::fs::read_to_string(&holds_file).ok().and_then(|j| serde_json::from_str(&j).ok()).unwrap_or_default();
    println!("vector-pusher {} listening on {}", keys.public_key().to_hex(), relays.join(", "));

    let pusher = Arc::new(Pusher {
        keys: keys.clone(),
        subject,
        http: reqwest::Client::builder().timeout(Duration::from_secs(15)).build().expect("http client"),
        stats: Stats::default(),
        seen: Mutex::new((HashSet::new(), VecDeque::new())),
        buckets: Mutex::new(HashMap::new()),
        gone: Mutex::new(HashMap::new()),
        holds: Mutex::new(holds),
        holds_file,
    });

    let client = ClientBuilder::new().build();
    for relay in &relays {
        if let Err(e) = client.add_relay(relay.as_str()).await {
            eprintln!("relay {relay}: {e}");
        }
    }
    client.connect().await;
    // Ephemeral: anything older than the relay's short memory is gone anyway.
    let filter = Filter::new()
        .kind(Kind::Custom(request::KIND))
        .pubkey(keys.public_key())
        .since(Timestamp::from_secs(Timestamp::now().as_secs().saturating_sub(120)));
    if let Err(e) = client.subscribe(filter).await {
        eprintln!("subscribe: {e}");
    }

    let reporter = pusher.clone();
    tokio::spawn(async move {
        let every = std::env::var("PUSHER_REPORT_SECS").ok().and_then(|v| v.parse().ok()).unwrap_or(600);
        let mut tick = tokio::time::interval(Duration::from_secs(every));
        tick.tick().await;
        loop {
            tick.tick().await;
            reporter.report();
        }
    });

    let mut notifications = client.notifications();
    loop {
        tokio::select! {
            n = notifications.next() => {
                let Some(n) = n else { break };
                if let ClientNotification::Event { event, .. } = n {
                    if event.kind == Kind::Custom(request::KIND) && pusher.first_sighting(event.id) {
                        let pusher = pusher.clone();
                        tokio::spawn(async move { pusher.handle(*event).await });
                    }
                }
            }
            _ = tokio::signal::ctrl_c() => break,
        }
    }
    pusher.report();
}

impl Pusher {
    /// The same request arrives once per relay it was published to.
    fn first_sighting(&self, id: EventId) -> bool {
        let mut seen = self.seen.lock().unwrap();
        if !seen.0.insert(id) {
            return false;
        }
        seen.1.push_back(id);
        if seen.1.len() > 20_000 {
            if let Some(old) = seen.1.pop_front() {
                seen.0.remove(&old);
            }
        }
        true
    }

    async fn handle(&self, event: Event) {
        if event.verify().is_err() {
            self.stats.rejected.fetch_add(1, Ordering::Relaxed);
            return;
        }
        let req = match Incoming::open(&self.keys, &event) {
            Ok(Incoming::Push(req)) => req,
            Ok(Incoming::Control(ctl)) => return self.control(ctl.ctl),
            Err(_) => {
                self.stats.rejected.fetch_add(1, Ordering::Relaxed);
                return;
            }
        };
        let Some((cap, body, ttl)) = self.open(req) else {
            self.stats.rejected.fetch_add(1, Ordering::Relaxed);
            return;
        };
        if self.is_held(&cap.cap) {
            self.stats.held.fetch_add(1, Ordering::Relaxed);
            return;
        }
        if self.is_gone(&cap.endpoint) {
            self.stats.gone.fetch_add(1, Ordering::Relaxed);
            return;
        }
        if !self.take_token(&cap.cap) || !self.take_token(&cap.endpoint) {
            self.stats.limited.fetch_add(1, Ordering::Relaxed);
            return;
        }
        let auth = match vapid::authorization(&cap.vapid, &cap.endpoint, &self.subject, unix_now()) {
            Ok(a) => a,
            Err(_) => {
                self.stats.rejected.fetch_add(1, Ordering::Relaxed);
                return;
            }
        };
        let sent = self
            .http
            .post(&cap.endpoint)
            .header("TTL", ttl.to_string())
            .header("Urgency", "high")
            .header("Content-Encoding", "aes128gcm")
            .header("Content-Type", "application/octet-stream")
            .header("Authorization", auth)
            .body(body)
            .send()
            .await;
        match sent.map(|r| r.status().as_u16()) {
            Ok(s) if (200..300).contains(&s) => {
                self.stats.delivered.fetch_add(1, Ordering::Relaxed);
            }
            Ok(404) | Ok(410) => {
                self.gone.lock().unwrap().insert(cap.endpoint, Instant::now());
                self.stats.gone.fetch_add(1, Ordering::Relaxed);
            }
            Ok(s) => {
                eprintln!("push service answered {s}");
                self.stats.failed.fetch_add(1, Ordering::Relaxed);
            }
            Err(e) => {
                eprintln!("push failed: {}", e.without_url());
                self.stats.failed.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    fn open(&self, req: Request) -> Option<(ticket::Capability, Vec<u8>, u32)> {
        let body = req.body().ok()?;
        // Every honest body is exactly this size; anything else is not ours to send.
        if body.len() != webpush::BODY_LEN {
            return None;
        }
        let cap = ticket::unseal(self.keys.secret_key(), &req.s).ok()?;
        vapid::origin(&cap.endpoint)?;
        Some((cap, body, req.t.min(MAX_TTL)))
    }

    /// A device holding or releasing some of its contacts. Ids are only ever learned from a
    /// ticket's sealed part, so whoever names one owns it.
    fn control(&self, holds: Vec<vector_push::Hold>) {
        self.stats.controls.fetch_add(1, Ordering::Relaxed);
        let now = unix_now();
        let snapshot = {
            let mut map = self.holds.lock().unwrap();
            for h in holds.into_iter().take(10_000) {
                if h.c.len() != 32 || !h.c.bytes().all(|b| b.is_ascii_hexdigit()) {
                    continue;
                }
                if h.until == HOLD_FOREVER || h.until > now {
                    map.insert(h.c, h.until);
                } else {
                    map.remove(&h.c);
                }
            }
            map.retain(|_, until| *until == HOLD_FOREVER || *until > now);
            serde_json::to_string(&*map).unwrap_or_default()
        };
        let tmp = format!("{}.tmp", self.holds_file);
        if std::fs::write(&tmp, snapshot).and_then(|_| std::fs::rename(&tmp, &self.holds_file)).is_err() {
            eprintln!("holds not saved");
        }
    }

    fn is_held(&self, cap: &str) -> bool {
        self.holds.lock().unwrap().get(cap).is_some_and(|&until| until == HOLD_FOREVER || until > unix_now())
    }

    fn take_token(&self, key: &str) -> bool {
        let mut buckets = self.buckets.lock().unwrap();
        let now = Instant::now();
        if buckets.len() > 100_000 {
            buckets.retain(|_, (tokens, at)| *tokens < BURST || now.duration_since(*at) < REFILL_EVERY * 20);
        }
        let (tokens, at) = buckets.entry(key.to_string()).or_insert((BURST, now));
        *tokens = (*tokens + now.duration_since(*at).as_secs_f64() / REFILL_EVERY.as_secs_f64()).min(BURST);
        *at = now;
        if *tokens < 1.0 {
            return false;
        }
        *tokens -= 1.0;
        true
    }

    fn is_gone(&self, endpoint: &str) -> bool {
        let mut gone = self.gone.lock().unwrap();
        gone.retain(|_, at| at.elapsed() < GONE_FOR);
        gone.contains_key(endpoint)
    }

    fn report(&self) {
        let s = &self.stats;
        println!(
            "delivered {} held {} gone {} failed {} rejected {} limited {} controls {}",
            s.delivered.load(Ordering::Relaxed),
            s.held.load(Ordering::Relaxed),
            s.gone.load(Ordering::Relaxed),
            s.failed.load(Ordering::Relaxed),
            s.rejected.load(Ordering::Relaxed),
            s.limited.load(Ordering::Relaxed),
            s.controls.load(Ordering::Relaxed),
        );
    }
}

fn load_keys(path: &str) -> Keys {
    if let Ok(hex) = std::fs::read_to_string(path) {
        return Keys::parse(hex.trim()).expect("unreadable pusher key");
    }
    let keys = Keys::generate();
    std::fs::write(path, keys.secret_key().to_secret_hex()).expect("cannot write pusher key");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    keys
}

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}
