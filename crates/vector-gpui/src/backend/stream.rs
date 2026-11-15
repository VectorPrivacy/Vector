//! An unencrypted web video played while it downloads: byte ranges fetched around where the
//! decoder reads, about ten seconds ahead, more on a slow or uneven link. The decoder's reads
//! wait for the bytes they need; a seek moves where fetching continues.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::{Duration, Instant};

const CHUNK: u64 = 256 * 1024;
/// The first request: the header, and for most clips their first seconds.
const HEAD: u64 = 2 * CHUNK;
/// Before the clip's length (so its bitrate) is known.
const DEFAULT_WINDOW: u64 = 8 * 1024 * 1024;
const BASE_SECS: f64 = 10.;
const MAX_SECS: f64 = 30.;
/// Kept behind the read head for the decoder's short backward reads.
const KEEP_BEHIND: u64 = 2 * 1024 * 1024;
/// One request at most this many chunks: big enough to amortise a slow link's round trip.
const MAX_BURST: u64 = 8;
const READ_TIMEOUT: Duration = Duration::from_secs(45);
const ATTEMPTS: u32 = 4;

/// One answer to a range request.
pub struct Got {
    pub bytes: Vec<u8>,
    /// The whole file's size, when the host says.
    pub total: Option<u64>,
    /// A 206: the host honoured the range.
    pub ranged: bool,
}

/// Where byte ranges come from: the web, or memory in a test.
pub trait Ranges: Send + Sync + 'static {
    fn get(&self, start: u64, end: u64) -> impl Future<Output = Result<Got, String>> + Send;
}

pub enum Open {
    /// The host serves only whole files: download it the old way.
    NoRanges,
    Failed(String),
}

struct State {
    chunks: BTreeMap<u64, Vec<u8>>,
    inflight: BTreeSet<u64>,
    /// Where the decoder last read.
    head: u64,
    /// A read is waiting on bytes not here yet.
    starving: bool,
    error: Option<String>,
    duration_ms: Option<u64>,
    /// Smoothed throughput (bytes/s) and time to first byte (s).
    rate: Option<f64>,
    latency: Option<f64>,
    /// In-order hash of what arrived, while nothing was skipped.
    hashed: u64,
    hasher: Option<vector_core::crypto::sha256::Sha256>,
}

pub struct Stream<R: Ranges> {
    len: u64,
    mime: &'static str,
    expected: Option<String>,
    ranges: R,
    state: Mutex<State>,
    arrived: Condvar,
    moved: Arc<tokio::sync::Notify>,
}

fn kind(head: &[u8]) -> Option<&'static str> {
    if head.len() >= 12 && &head[4..8] == b"ftyp" {
        Some(if &head[8..12] == b"qt  " { "video/quicktime" } else { "video/mp4" })
    } else if head.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) {
        Some("video/webm")
    } else {
        None
    }
}

pub async fn open<R: Ranges>(ranges: R, expected: Option<String>) -> Result<Arc<Stream<R>>, Open> {
    let t0 = Instant::now();
    let first = ranges.get(0, HEAD).await.map_err(Open::Failed)?;
    let took = t0.elapsed().as_secs_f64().max(1e-3);
    let total = match first.total {
        Some(t) if first.ranged => t,
        _ => return Err(Open::NoRanges),
    };
    let mime = kind(&first.bytes).ok_or_else(|| Open::Failed("Not a video".into()))?;
    let s = Arc::new(Stream {
        len: total,
        mime,
        expected: expected.map(|e| e.to_ascii_lowercase()),
        ranges,
        state: Mutex::new(State {
            chunks: BTreeMap::new(),
            inflight: BTreeSet::new(),
            head: 0,
            starving: false,
            error: None,
            duration_ms: None,
            rate: Some(first.bytes.len() as f64 / took),
            latency: Some(took * CHUNK as f64 / HEAD as f64),
            hashed: 0,
            hasher: None,
        }),
        arrived: Condvar::new(),
        moved: Arc::new(tokio::sync::Notify::new()),
    });
    {
        let mut st = s.state.lock().unwrap();
        st.hasher = s.expected.as_ref().map(|_| vector_core::crypto::sha256::Sha256::new());
        s.store(&mut st, 0, first.bytes);
    }
    let weak = Arc::downgrade(&s);
    // spawn-detached: fills one web video's byte window; ends when the player lets it go.
    tokio::spawn(fetch_loop(weak));
    Ok(s)
}

impl<R: Ranges> Stream<R> {
    fn chunks(&self) -> u64 {
        self.len.div_ceil(CHUNK)
    }

    /// Bytes to hold ahead of the read head: ten seconds of the clip, more while the link can't
    /// keep twice its bitrate or answers slowly.
    fn window(&self, st: &State) -> u64 {
        let Some(bitrate) = st.duration_ms.filter(|d| *d > 0).map(|d| self.len as f64 * 1000. / d as f64) else {
            return DEFAULT_WINDOW;
        };
        let mut secs = BASE_SECS;
        if let Some(rate) = st.rate {
            secs *= (2. * bitrate / rate.max(1.)).clamp(1., 3.);
        }
        if let Some(lat) = st.latency {
            secs += lat * 4.;
        }
        ((bitrate * secs.min(MAX_SECS)) as u64).max(2 * CHUNK)
    }

    /// The clip's length, which gives its bitrate and so the window's size in bytes.
    pub fn set_duration(&self, ms: u64) {
        self.state.lock().unwrap().duration_ms = Some(ms);
        self.moved.notify_one();
    }

    /// Seconds of the clip already here, from where the decoder reads.
    pub fn ahead_secs(&self) -> Option<f64> {
        let st = self.state.lock().unwrap();
        let d = st.duration_ms.filter(|d| *d > 0)?;
        let mut i = st.head / CHUNK;
        while st.chunks.contains_key(&i) {
            i += 1;
        }
        let bytes = (i * CHUNK).min(self.len).saturating_sub(st.head);
        Some(bytes as f64 * d as f64 / 1000. / self.len as f64)
    }

    /// Everything from the read head to the end is here.
    pub fn complete_ahead(&self) -> bool {
        let st = self.state.lock().unwrap();
        (st.head / CHUNK..self.chunks()).all(|i| st.chunks.contains_key(&i))
    }

    #[cfg(test)]
    pub fn ranges(&self) -> &R {
        &self.ranges
    }

    /// A read is waiting on the network.
    pub fn starving(&self) -> bool {
        self.state.lock().unwrap().starving
    }

    pub fn error(&self) -> Option<String> {
        self.state.lock().unwrap().error.clone()
    }

    fn store(&self, st: &mut State, first: u64, bytes: Vec<u8>) {
        for (k, part) in bytes.chunks(CHUNK as usize).enumerate() {
            let i = first + k as u64;
            st.inflight.remove(&i);
            st.chunks.entry(i).or_insert_with(|| part.to_vec());
        }
        self.hash_along(st);
        self.evict(st);
        self.arrived.notify_all();
    }

    /// Hashed only while every byte arrives in order; a mismatch stops the clip.
    fn hash_along(&self, st: &mut State) {
        let Some(want) = self.expected.as_ref() else { return };
        if st.hasher.is_none() {
            return;
        }
        while st.hashed < self.len
            && let Some(c) = st.chunks.get(&(st.hashed / CHUNK))
        {
            if let Some(h) = st.hasher.as_mut() {
                h.update(c);
            }
            st.hashed += c.len() as u64;
        }
        if st.hashed >= self.len
            && let Some(h) = st.hasher.take()
        {
            let got: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
            if &got != want {
                st.error = Some("The video doesn't match the post's fingerprint".into());
            }
        }
    }

    /// Far behind the read head goes, and so does anything past twice the window ahead; the
    /// opening stays, as players re-read the header on a seek.
    fn evict(&self, st: &mut State) {
        let head = st.head / CHUNK;
        let behind = KEEP_BEHIND / CHUNK;
        let ahead = self.window(st).div_ceil(CHUNK) * 2 + 1;
        let unhashed = st.hasher.is_some().then(|| st.hashed / CHUNK);
        let drop: Vec<u64> = st
            .chunks
            .keys()
            .copied()
            .filter(|&i| i >= HEAD / CHUNK && (i + behind < head || i > head + ahead))
            .collect();
        for i in drop {
            st.chunks.remove(&i);
            // A hole now precedes chunks not yet hashed: the in-order hash can't finish.
            if unhashed.is_some_and(|h| i >= h) {
                st.hasher = None;
            }
        }
    }

    /// The next run of chunks to fetch: the one the decoder waits on first, else the first gap
    /// in the window ahead of it.
    fn next(&self, st: &mut State) -> Option<(u64, u64)> {
        let head = st.head / CHUNK;
        let end = ((st.head + self.window(st)).div_ceil(CHUNK)).min(self.chunks());
        let start = (head..end).find(|i| !st.chunks.contains_key(i) && !st.inflight.contains(i))?;
        let burst = if st.starving { 2 } else { MAX_BURST };
        let mut n = 1;
        while n < burst && start + n < end && !st.chunks.contains_key(&(start + n)) && !st.inflight.contains(&(start + n)) {
            n += 1;
        }
        for i in start..start + n {
            st.inflight.insert(i);
        }
        Some((start, n))
    }

    fn landed(&self, first: u64, n: u64, got: Result<Got, String>, took: Duration, ttfb: Duration) -> bool {
        let mut st = self.state.lock().unwrap();
        match got {
            Ok(g) if g.ranged => {
                let secs = took.as_secs_f64().max(1e-3);
                let rate = g.bytes.len() as f64 / secs;
                st.rate = Some(st.rate.map_or(rate, |r| r * 0.7 + rate * 0.3));
                let lat = ttfb.as_secs_f64();
                st.latency = Some(st.latency.map_or(lat, |l| l * 0.7 + lat * 0.3));
                self.store(&mut st, first, g.bytes);
                true
            }
            other => {
                for i in first..first + n {
                    st.inflight.remove(&i);
                }
                if other.is_ok() {
                    st.error = Some("The video host stopped answering ranges".into());
                    self.arrived.notify_all();
                }
                false
            }
        }
    }

    fn fail(&self, e: String) {
        self.state.lock().unwrap().error = Some(e);
        self.arrived.notify_all();
    }
}

async fn fetch_loop<R: Ranges>(weak: Weak<Stream<R>>) {
    let mut failures = 0u32;
    loop {
        let Some(s) = weak.upgrade() else { return };
        if s.state.lock().unwrap().error.is_some() {
            return;
        }
        let next = {
            let mut st = s.state.lock().unwrap();
            s.next(&mut st)
        };
        let Some((first, n)) = next else {
            let moved = s.moved.clone();
            drop(s);
            let _ = tokio::time::timeout(Duration::from_millis(500), moved.notified()).await;
            continue;
        };
        let start = first * CHUNK;
        let end = ((first + n) * CHUNK).min(s.len);
        let t0 = Instant::now();
        let got = s.ranges.get(start, end).await;
        let took = t0.elapsed();
        // Round trip of a small request is mostly latency; of a big one mostly transfer.
        let ttfb = if end - start <= CHUNK { took } else { took.mul_f64(CHUNK as f64 / (end - start) as f64) };
        let err = got.as_ref().err().cloned();
        if s.landed(first, n, got, took, ttfb) {
            failures = 0;
            continue;
        }
        let Some(e) = err else { return };
        failures += 1;
        if failures >= ATTEMPTS {
            s.fail(e);
            return;
        }
        drop(s);
        tokio::time::sleep(Duration::from_millis(500 * failures as u64)).await;
    }
}

impl<R: Ranges> vector_core::media::MediaSource for Stream<R> {
    fn len(&self) -> u64 {
        self.len
    }

    fn mime(&self) -> &str {
        self.mime
    }

    fn remote(&self) -> bool {
        true
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<usize> {
        if offset >= self.len || buf.is_empty() {
            return Ok(0);
        }
        let want = buf.len().min((self.len - offset) as usize);
        let mut st = self.state.lock().unwrap();
        if st.head != offset {
            st.head = offset;
            self.moved.notify_one();
        }
        let mut done = 0usize;
        let mut waited = Duration::ZERO;
        while done < want {
            let at = offset + done as u64;
            let (i, off) = (at / CHUNK, (at % CHUNK) as usize);
            if let Some(c) = st.chunks.get(&i) {
                let n = (c.len() - off).min(want - done);
                buf[done..done + n].copy_from_slice(&c[off..off + n]);
                done += n;
                continue;
            }
            if let Some(e) = &st.error {
                return Err(std::io::Error::other(e.clone()));
            }
            if waited >= READ_TIMEOUT {
                return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "The video stopped loading"));
            }
            st.starving = true;
            self.moved.notify_one();
            let step = Duration::from_millis(250);
            st = self.arrived.wait_timeout(st, step).unwrap().0;
            waited += step;
        }
        st.starving = false;
        Ok(done)
    }
}

/// Ranges of a web video, through the account's transport like any other media.
pub struct Web {
    url: String,
    client: vector_core::net::HttpClient,
}

impl Web {
    pub fn new(url: &str) -> Result<Self, String> {
        vector_core::net::validate_url_not_private(url).map_err(str::to_string)?;
        let client = vector_core::net::build_http_client_with_options(None, Some(vector_core::net::transfer_stall()), true)?;
        Ok(Self { url: url.to_string(), client })
    }
}

impl Ranges for Web {
    async fn get(&self, start: u64, end: u64) -> Result<Got, String> {
        let resp = vector_core::net::proxied_request(&self.client, reqwest::Method::GET, &self.url)
            .await
            .header(reqwest::header::RANGE, format!("bytes={start}-{}", end - 1))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let status = resp.status();
        if status != reqwest::StatusCode::PARTIAL_CONTENT {
            // A whole file isn't read here: the caller falls back to downloading it.
            return if status.is_success() {
                Ok(Got { bytes: Vec::new(), total: resp.content_length(), ranged: false })
            } else {
                Err(format!("The video host answered {status}"))
            };
        }
        let total = resp
            .headers()
            .get(reqwest::header::CONTENT_RANGE)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.rsplit('/').next())
            .and_then(|t| t.parse().ok());
        let bytes = resp.bytes().await.map_err(|e| e.to_string())?;
        if bytes.len() as u64 != end - start {
            return Err("The video host sent a short range".into());
        }
        Ok(Got { bytes: bytes.into(), total, ranged: true })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Seek, SeekFrom};
    use vector_core::media::MediaSource;

    /// A file in memory, served as ranges at `bps` bytes a second after `latency`.
    struct Mem {
        bytes: Vec<u8>,
        ranged: bool,
        bps: Option<f64>,
        latency: Duration,
        served: std::sync::atomic::AtomicU64,
    }

    impl Ranges for Mem {
        async fn get(&self, start: u64, end: u64) -> Result<Got, String> {
            tokio::time::sleep(self.latency).await;
            if !self.ranged {
                return Ok(Got { bytes: Vec::new(), total: Some(self.bytes.len() as u64), ranged: false });
            }
            let part = self.bytes[start as usize..(end as usize).min(self.bytes.len())].to_vec();
            if let Some(bps) = self.bps {
                tokio::time::sleep(Duration::from_secs_f64(part.len() as f64 / bps)).await;
            }
            self.served.fetch_add(part.len() as u64, std::sync::atomic::Ordering::Relaxed);
            Ok(Got { bytes: part, total: Some(self.bytes.len() as u64), ranged: true })
        }
    }

    fn video(len: usize) -> Vec<u8> {
        let mut v: Vec<u8> = (0..len).map(|i| (i * 31 % 251) as u8).collect();
        v[4..12].copy_from_slice(b"ftypisom");
        v
    }

    fn mem(bytes: Vec<u8>) -> Mem {
        Mem { bytes, ranged: true, bps: None, latency: Duration::ZERO, served: Default::default() }
    }

    fn sha(b: &[u8]) -> String {
        vector_core::crypto::sha256_hex(b)
    }

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap()
    }

    #[test]
    fn reads_the_whole_file_in_order_and_checks_its_hash() {
        let rt = rt();
        let v = video(5 * 1024 * 1024 + 123);
        let s = rt.block_on(open(mem(v.clone()), Some(sha(&v)))).ok().unwrap();
        s.set_duration(20_000);
        let src: Arc<dyn MediaSource> = s.clone();
        let mut r = vector_core::media::reader(src);
        let mut out = Vec::new();
        r.read_to_end(&mut out).unwrap();
        assert_eq!(out, v);
        assert!(s.error().is_none());
    }

    #[test]
    fn a_wrong_hash_stops_the_clip_once_every_byte_is_in() {
        let rt = rt();
        let v = video(1024 * 1024);
        let s = rt.block_on(open(mem(v.clone()), Some("00".repeat(32)))).ok().unwrap();
        let src: Arc<dyn MediaSource> = s.clone();
        let mut r = vector_core::media::reader(src);
        let mut out = Vec::new();
        assert!(r.read_to_end(&mut out).is_err() || s.error().is_some());
    }

    #[test]
    fn seeks_read_the_right_bytes_and_keep_memory_bounded() {
        let rt = rt();
        let v = video(40 * 1024 * 1024);
        let s = rt.block_on(open(mem(v.clone()), None)).ok().unwrap();
        s.set_duration(80_000);
        let src: Arc<dyn MediaSource> = s.clone();
        let mut r = vector_core::media::reader(src);
        for at in [30 * 1024 * 1024u64, 1024, 20 * 1024 * 1024, 39 * 1024 * 1024, 5_000_000] {
            r.seek(SeekFrom::Start(at)).unwrap();
            let mut b = vec![0u8; 300_000];
            r.read_exact(&mut b).unwrap();
            assert_eq!(&b[..], &v[at as usize..at as usize + 300_000]);
        }
        std::thread::sleep(Duration::from_millis(300));
        let held: u64 = s.state.lock().unwrap().chunks.values().map(|c| c.len() as u64).sum();
        // Window (10 s of a 4 Mbit/s clip ≈ 5 MB) twice, plus what sits behind and the opening.
        assert!(held < 16 * 1024 * 1024, "held {held}");
    }

    #[test]
    fn a_host_without_ranges_falls_back() {
        let rt = rt();
        let mut m = mem(video(1024 * 1024));
        m.ranged = false;
        assert!(matches!(rt.block_on(open(m, None)), Err(Open::NoRanges)));
    }

    #[test]
    fn a_slow_link_grows_the_window() {
        let rt = rt();
        let v = video(20 * 1024 * 1024);
        // 20 MB over 40 s is 512 KB/s; served at 400 KB/s with 300 ms latency.
        let mut m = mem(v.clone());
        m.bps = Some(400. * 1024.);
        m.latency = Duration::from_millis(300);
        let s = rt.block_on(open(m, None)).ok().unwrap();
        s.set_duration(40_000);
        std::thread::sleep(Duration::from_secs(6));
        let st = s.state.lock().unwrap();
        let bitrate = 20. * 1024. * 1024. / 40.;
        let w = s.window(&st) as f64 / bitrate;
        assert!(w > BASE_SECS * 1.5, "window {w:.1}s, rate {:?}", st.rate);
    }

    #[test]
    fn stops_fetching_when_the_player_lets_go() {
        let rt = rt();
        let v = video(30 * 1024 * 1024);
        let mut m = mem(v);
        m.bps = Some(2. * 1024. * 1024.);
        let s = rt.block_on(open(m, None)).ok().unwrap();
        s.set_duration(60_000);
        std::thread::sleep(Duration::from_millis(500));
        let weak = Arc::downgrade(&s);
        drop(s);
        std::thread::sleep(Duration::from_millis(2500));
        assert!(weak.upgrade().is_none());
    }
}
