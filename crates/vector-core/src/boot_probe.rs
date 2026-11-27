//! A startup census in vector.log: what a cold boot fans out to (clients, sockets, reads, CPU).
//! Persisted like a network failure because the log file keeps nothing quieter.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

/// Plane connections built: authed per plane key, and open per relay.
pub static PLANE_DIALS: AtomicU64 = AtomicU64::new(0);
pub static OPEN_DIALS: AtomicU64 = AtomicU64::new(0);
/// Community reads asked of relays.
pub static FETCHES: AtomicU64 = AtomicU64::new(0);

/// When login began, for the census and the first focused page.
static START: std::sync::OnceLock<web_time::Instant> = std::sync::OnceLock::new();

/// A channel opened in the census window has its newest page: how long that took, and since login.
pub fn note_focused(took: std::time::Duration) {
    let Some(start) = START.get() else { return };
    if start.elapsed() > std::time::Duration::from_secs(300) {
        return;
    }
    crate::log_net_fail!("[boot-probe] focused page in {} ms, {} ms after login, plane dials {}", took.as_millis(), start.elapsed().as_millis(), PLANE_DIALS.load(Relaxed));
}

pub fn note(c: &AtomicU64) {
    c.fetch_add(1, Relaxed);
}

/// Planes dialled, told apart: a dial of one already dialled is the pool's churn.
static PLANES_SEEN: std::sync::Mutex<Option<std::collections::HashSet<String>>> = std::sync::Mutex::new(None);

pub fn note_plane(key: &str) {
    note(&PLANE_DIALS);
    let mut seen = PLANES_SEEN.lock().unwrap_or_else(|e| e.into_inner());
    seen.get_or_insert_with(Default::default).insert(key.to_string());
}

fn planes_seen() -> usize {
    PLANES_SEEN.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map_or(0, |s| s.len())
}

#[cfg(not(target_arch = "wasm32"))]
/// One line every 10 s for the first 5 minutes after login.
pub async fn run() {
    let start = *START.get_or_init(web_time::Instant::now);
    let mut last = rusage();
    for _ in 0..30 {
        crate::rt::time::sleep(std::time::Duration::from_secs(10)).await;
        let (pooled, open, mut clients) = crate::community::transport::plane_census();
        if let Some(main) = crate::state::nostr_client() {
            clients.push(main);
        }
        let (mut handles, mut connected) = (0usize, 0usize);
        for c in &clients {
            for r in c.relays().all().await.values() {
                handles += 1;
                connected += usize::from(r.status() == nostr_sdk::prelude::RelayStatus::Connected);
            }
        }
        let now = rusage();
        let tasks = tokio::runtime::Handle::try_current().map_or(0, |h| h.metrics().num_alive_tasks());
        crate::log_net_fail!(
            "[boot-probe] t={}s clients={} (pooled {} open {}) relays={} connected={} dials plane={} ({} distinct) open={} fetches={} tasks={} cpu+10s user={:.1}s sys={:.1}s csw={}",
            start.elapsed().as_secs(),
            clients.len(),
            pooled,
            open,
            handles,
            connected,
            PLANE_DIALS.load(Relaxed),
            planes_seen(),
            OPEN_DIALS.load(Relaxed),
            FETCHES.load(Relaxed),
            tasks,
            now.0 - last.0,
            now.1 - last.1,
            now.2.saturating_sub(last.2),
        );
        last = now;
    }
}

#[cfg(all(unix, not(target_arch = "wasm32")))]
/// This process's user and system CPU seconds and context switches so far.
fn rusage() -> (f64, f64, u64) {
    let mut u: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: getrusage fills the struct it is given.
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut u) } != 0 {
        return (0., 0., 0);
    }
    let secs = |t: libc::timeval| t.tv_sec as f64 + t.tv_usec as f64 / 1e6;
    (secs(u.ru_utime), secs(u.ru_stime), (u.ru_nvcsw + u.ru_nivcsw) as u64)
}

#[cfg(all(not(unix), not(target_arch = "wasm32")))]
fn rusage() -> (f64, f64, u64) {
    (0., 0., 0)
}
