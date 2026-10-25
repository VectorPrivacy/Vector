//! Decrypted media held in memory, one cache per account session.
//!
//! The budget counts every entry still alive, in the map or out: one evicted while a player
//! still reads it is still memory. A held entry (a mounted player) is never evicted. The rest
//! go least recently used first, and anything idle for half an hour goes.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use web_time::Instant;
use zeroize::Zeroizing;

const IDLE: Duration = Duration::from_secs(30 * 60);
const TRIM_EVERY: Duration = Duration::from_secs(5 * 60);

/// One decrypted file. Its bytes are zeroed when the last reader lets go.
pub struct Entry {
    bytes: Zeroizing<Vec<u8>>,
    mime: Box<str>,
    live: Arc<AtomicU64>,
}

impl Entry {
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

impl Drop for Entry {
    fn drop(&mut self) {
        self.live.fetch_sub(self.bytes.len() as u64, Ordering::Relaxed);
    }
}

impl super::MediaSource for Entry {
    fn len(&self) -> u64 {
        self.bytes.len() as u64
    }

    fn mime(&self) -> &str {
        &self.mime
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<usize> {
        let start = usize::try_from(offset).unwrap_or(usize::MAX).min(self.bytes.len());
        let n = buf.len().min(self.bytes.len() - start);
        buf[..n].copy_from_slice(&self.bytes[start..start + n]);
        Ok(n)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refused {
    /// Over a third of the budget: such a file is kept on disk, never here.
    TooLarge,
    /// What players hold already fills the budget.
    Busy,
}

struct Slot {
    entry: Arc<Entry>,
    touched: Instant,
    holds: u32,
}

pub struct MediaRam {
    budget: u64,
    live: Arc<AtomicU64>,
    slots: Mutex<HashMap<[u8; 32], Slot>>,
}

impl Default for MediaRam {
    fn default() -> Self {
        Self::with_budget(budget())
    }
}

impl MediaRam {
    fn with_budget(budget: u64) -> Self {
        MediaRam { budget, live: Arc::new(AtomicU64::new(0)), slots: Mutex::new(HashMap::new()) }
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<[u8; 32], Slot>> {
        self.slots.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn insert(&self, hash: [u8; 32], bytes: Vec<u8>, mime: &str) -> Result<Arc<Entry>, Refused> {
        self.insert_at(hash, bytes, mime, Instant::now())
    }

    fn insert_at(&self, hash: [u8; 32], bytes: Vec<u8>, mime: &str, now: Instant) -> Result<Arc<Entry>, Refused> {
        let bytes = Zeroizing::new(bytes);
        let len = bytes.len() as u64;
        if !self.admits(len) {
            return Err(Refused::TooLarge);
        }
        let mut slots = self.lock();
        if let Some(slot) = slots.get_mut(&hash) {
            slot.touched = now;
            return Ok(slot.entry.clone());
        }
        trim_idle(&mut slots, now);

        // Only an entry nobody else reads gives its memory back when it leaves the map.
        let freeable = |slot: &Slot| slot.holds == 0 && Arc::strong_count(&slot.entry) == 1;
        let free_now: u64 = slots.values().filter(|s| freeable(s)).map(|s| s.entry.bytes.len() as u64).sum();
        if self.live.load(Ordering::Relaxed).saturating_sub(free_now) + len > self.budget {
            return Err(Refused::Busy);
        }
        while self.live.load(Ordering::Relaxed) + len > self.budget {
            let oldest = slots.iter().filter(|(_, s)| freeable(s)).min_by_key(|(_, s)| s.touched).map(|(h, _)| *h);
            let Some(oldest) = oldest else { return Err(Refused::Busy) };
            slots.remove(&oldest);
        }

        self.live.fetch_add(len, Ordering::Relaxed);
        let entry = Arc::new(Entry { bytes, mime: mime.into(), live: self.live.clone() });
        slots.insert(hash, Slot { entry: entry.clone(), touched: now, holds: 0 });
        Ok(entry)
    }

    /// The largest file that may live here at all.
    pub fn largest(&self) -> u64 {
        self.budget / 3
    }

    pub fn admits(&self, len: u64) -> bool {
        len <= self.largest()
    }

    pub fn get(&self, hash: &[u8; 32]) -> Option<Arc<Entry>> {
        let mut slots = self.lock();
        let slot = slots.get_mut(hash)?;
        slot.touched = Instant::now();
        Some(slot.entry.clone())
    }

    pub fn contains(&self, hash: &[u8; 32]) -> bool {
        self.lock().contains_key(hash)
    }

    /// Keep `hash` for as long as a player shows it. False when it isn't here.
    pub fn hold(&self, hash: &[u8; 32]) -> bool {
        let mut slots = self.lock();
        let Some(slot) = slots.get_mut(hash) else { return false };
        slot.holds += 1;
        slot.touched = Instant::now();
        true
    }

    pub fn release(&self, hash: &[u8; 32]) {
        if let Some(slot) = self.lock().get_mut(hash) {
            slot.holds = slot.holds.saturating_sub(1);
            slot.touched = Instant::now();
        }
    }

    pub fn remove(&self, hash: &[u8; 32]) {
        self.lock().remove(hash);
    }

    pub fn clear(&self) {
        self.lock().clear();
    }

    pub fn trim(&self) {
        trim_idle(&mut self.lock(), Instant::now());
    }

    /// Bytes alive across every entry, readers outside the map included.
    pub fn live_bytes(&self) -> u64 {
        self.live.load(Ordering::Relaxed)
    }
}

fn trim_idle(slots: &mut HashMap<[u8; 32], Slot>, now: Instant) {
    slots.retain(|_, s| s.holds > 0 || now.saturating_duration_since(s.touched) < IDLE);
}

fn budget() -> u64 {
    const MIB: u64 = 1 << 20;
    // The page holds its own copy of what it shows as blobs.
    #[cfg(target_arch = "wasm32")]
    let fixed = 128 * MIB;
    #[cfg(any(target_os = "android", target_os = "ios"))]
    let fixed = 192 * MIB;
    #[cfg(not(any(target_arch = "wasm32", target_os = "android", target_os = "ios")))]
    let fixed = 512 * MIB;
    device_memory().map_or(fixed, |total| fixed.min(total / 8))
}

#[cfg(any(target_os = "android", target_os = "linux"))]
fn device_memory() -> Option<u64> {
    let info = std::fs::read_to_string("/proc/meminfo").ok()?;
    let kib = info.lines().find_map(|l| l.strip_prefix("MemTotal:"))?.trim().strip_suffix("kB")?.trim();
    kib.parse::<u64>().ok().map(|k| k * 1024)
}

#[cfg(not(any(target_os = "android", target_os = "linux")))]
fn device_memory() -> Option<u64> {
    None
}

struct MediaRamKey;

/// This account's cache.
pub fn cache() -> Arc<MediaRam> {
    crate::db::current_session().scoped::<MediaRamKey, MediaRam>()
}

/// Built with the session, so a rebind's copy of the session map shares this one cache.
pub(crate) fn attach(session: &Arc<crate::db::Session>) {
    let _ = session.scoped::<MediaRamKey, MediaRam>();
}

pub(crate) fn clear_for(session: &Arc<crate::db::Session>) {
    session.scoped::<MediaRamKey, MediaRam>().clear();
}

/// Keep decrypted `bytes` for this account, and keep idle entries trimmed from then on.
pub fn insert(hash: [u8; 32], bytes: Vec<u8>, mime: &str) -> Result<Arc<Entry>, Refused> {
    start_trimming();
    cache().insert(hash, bytes, mime)
}

/// A per-session loop would outlive its session across a rebind, so one loop trims
/// whichever account is live.
fn start_trimming() {
    static STARTED: AtomicBool = AtomicBool::new(false);
    #[cfg(not(target_arch = "wasm32"))]
    if tokio::runtime::Handle::try_current().is_err() {
        return;
    }
    if STARTED.swap(true, Ordering::AcqRel) {
        return;
    }
    // spawn-detached: trims the live account's cache, resolved afresh on every tick.
    crate::rt::spawn(async {
        loop {
            crate::rt::time::sleep(TRIM_EVERY).await;
            cache().trim();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::MediaSource;

    const H1: [u8; 32] = [1; 32];
    const H2: [u8; 32] = [2; 32];
    const H3: [u8; 32] = [3; 32];
    const H4: [u8; 32] = [4; 32];

    fn bytes(n: usize) -> Vec<u8> {
        vec![0x5a; n]
    }

    #[test]
    fn the_least_recently_used_goes_first_and_a_held_entry_never_does() {
        let ram = MediaRam::with_budget(300);
        let t0 = Instant::now();
        ram.insert_at(H1, bytes(100), "image/png", t0).unwrap();
        ram.insert_at(H2, bytes(100), "image/png", t0 + Duration::from_secs(1)).unwrap();
        assert!(ram.hold(&H1));
        ram.insert_at(H3, bytes(100), "image/png", t0 + Duration::from_secs(2)).unwrap();
        ram.insert_at(H4, bytes(100), "image/png", t0 + Duration::from_secs(3)).unwrap();
        assert!(ram.contains(&H1), "held, though oldest");
        assert!(!ram.contains(&H2), "the oldest unheld went");
        assert!(ram.contains(&H3) && ram.contains(&H4));
        assert_eq!(ram.live_bytes(), 300);
    }

    #[test]
    fn a_file_too_big_or_a_budget_full_of_holds_is_refused() {
        let ram = MediaRam::with_budget(300);
        assert_eq!(ram.insert(H1, bytes(101), "video/mp4").err(), Some(Refused::TooLarge));
        for h in [H1, H2, H3] {
            ram.insert(h, bytes(100), "video/mp4").unwrap();
            assert!(ram.hold(&h));
        }
        assert_eq!(ram.insert(H4, bytes(1), "image/png").err(), Some(Refused::Busy));
        assert!(ram.contains(&H1) && ram.contains(&H2) && ram.contains(&H3), "nothing held was given up");
        ram.release(&H2);
        ram.insert(H4, bytes(1), "image/png").unwrap();
        assert!(!ram.contains(&H2), "released, then the first to go");
    }

    #[test]
    fn bytes_a_reader_still_holds_count_and_leave_with_the_reader() {
        let ram = MediaRam::with_budget(300);
        let playing = ram.insert(H1, bytes(100), "audio/mpeg").unwrap();
        ram.insert(H2, bytes(100), "image/png").unwrap();
        ram.remove(&H1);
        assert_eq!(ram.live_bytes(), 200, "out of the map, still in memory");
        assert_eq!(ram.insert(H3, bytes(100), "image/png").err(), None);
        assert_eq!(ram.insert(H4, bytes(100), "image/png").err(), None, "H2 made room; the reader's copy did not");
        assert!(!ram.contains(&H2));
        assert_eq!(ram.live_bytes(), 300);
        drop(playing);
        assert_eq!(ram.live_bytes(), 200);
    }

    #[test]
    fn a_reader_mid_request_keeps_its_entry_out_of_eviction() {
        let ram = MediaRam::with_budget(300);
        let t0 = Instant::now();
        ram.insert_at(H1, bytes(100), "video/mp4", t0).unwrap();
        let reading = ram.get(&H1).unwrap();
        ram.insert_at(H2, bytes(100), "image/png", t0 + Duration::from_secs(1)).unwrap();
        ram.insert_at(H3, bytes(100), "image/png", t0 + Duration::from_secs(2)).unwrap();
        ram.insert_at(H4, bytes(100), "image/png", t0 + Duration::from_secs(3)).unwrap();
        assert!(ram.contains(&H1), "the oldest, but being read");
        assert!(!ram.contains(&H2), "so the next oldest went");
        drop(reading);
    }

    #[test]
    fn an_idle_entry_goes_after_half_an_hour_unless_held() {
        let ram = MediaRam::with_budget(1000);
        let t0 = Instant::now();
        ram.insert_at(H1, bytes(10), "image/png", t0).unwrap();
        ram.insert_at(H2, bytes(10), "video/mp4", t0).unwrap();
        assert!(ram.hold(&H2));
        ram.insert_at(H3, bytes(10), "image/png", t0 + IDLE + Duration::from_secs(1)).unwrap();
        assert!(!ram.contains(&H1), "idle");
        assert!(ram.contains(&H2), "held");
        assert!(ram.contains(&H3));
    }

    #[test]
    fn an_entry_reads_like_the_bytes_it_holds() {
        let ram = MediaRam::with_budget(1000);
        let data: Vec<u8> = (0..50u8).collect();
        let entry = ram.insert(H1, data.clone(), "image/png").unwrap();
        assert_eq!((entry.len(), entry.mime()), (50, "image/png"));
        let mut buf = [0u8; 8];
        assert_eq!(entry.read_at(45, &mut buf).unwrap(), 5);
        assert_eq!(&buf[..5], &data[45..]);
        assert_eq!(entry.read_at(50, &mut buf).unwrap(), 0);
        assert_eq!(entry.read_at(u64::MAX, &mut buf).unwrap(), 0);
        let again = ram.insert(H1, bytes(3), "image/png").unwrap();
        assert_eq!(again.bytes(), &data[..], "a second insert of a hash keeps the first copy");
        assert_eq!(ram.live_bytes(), 50);
    }

    static TEST_COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(93000);

    fn make_test_npub(n: u32) -> String {
        const BECH32: &[u8] = b"qpzry9x8gf2tvdw0s3jn54khce6mua7l";
        let mut payload = vec![b'q'; 58];
        let mut x = n as u64;
        let mut i = 58;
        while x > 0 && i > 0 {
            i -= 1;
            payload[i] = BECH32[(x as usize) % 32];
            x /= 32;
        }
        format!("npub1{}", String::from_utf8(payload).unwrap())
    }

    fn init_test_db() -> (tempfile::TempDir, std::sync::MutexGuard<'static, ()>, String) {
        let guard = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        crate::db::close_database();
        let tmp = tempfile::tempdir().unwrap();
        let account = make_test_npub(TEST_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
        std::fs::create_dir_all(tmp.path().join(&account)).unwrap();
        crate::db::set_app_data_dir(crate::db::shared_test_data_dir().to_path_buf());
        crate::db::set_current_account(account.clone()).unwrap();
        crate::db::init_database(&account).unwrap();
        (tmp, guard, account)
    }

    #[test]
    fn a_rebind_keeps_the_one_cache_and_a_swap_empties_it() {
        let (_tmp, _guard, account) = init_test_db();
        let before = crate::db::current_session();
        cache().insert(H1, bytes(10), "image/png").unwrap();

        crate::db::init_database(&account).unwrap();
        let after = crate::db::current_session();
        assert!(!Arc::ptr_eq(&before, &after), "init_database rebinds");
        assert!(Arc::ptr_eq(&before.scoped::<MediaRamKey, MediaRam>(), &cache()), "one cache across the rebind");
        assert!(cache().contains(&H1));

        let held = cache();
        crate::db::close_database();
        assert!(!held.contains(&H1), "the outgoing account's media leaves with it");
        assert!(!cache().contains(&H1));
    }
}
