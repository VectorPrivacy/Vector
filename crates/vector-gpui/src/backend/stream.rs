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
/// Kept behind a read head for the decoder's short backward reads.
const KEEP_BEHIND: u64 = 2 * 1024 * 1024;
/// How long a head keeps the lowest offset its reader touched.
const HEAD_SPAN: Duration = Duration::from_secs(2);
/// A head nobody has read from for this long is gone.
const HEAD_LIFE: Duration = Duration::from_secs(3);
/// One request at most this many chunks: big enough to amortise a slow link's round trip.
const MAX_BURST: u64 = 8;
/// While the picture waits: a megabyte, enough for a second of most clips in one round trip.
const WAITING_BURST: u64 = 4;
const READ_TIMEOUT: Duration = Duration::from_secs(45);
const ATTEMPTS: u32 = 4;
/// Requests in flight at once: a host slow to answer each still fills the window.
const PARALLEL: usize = 3;

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

/// Where one reader is reading. Each decoder reads on a thread of its own (the picture and the
/// sound decode apart), so its thread names it and a seek moves its head rather than leaving one.
struct Head {
    reader: Option<std::thread::ThreadId>,
    /// The lowest offset it read this span: a demuxer hops between its tracks' samples, and
    /// the bytes at the low end are still wanted while it reads the high.
    at: u64,
    span: Instant,
    seen: Instant,
    waiting: bool,
}

struct State {
    chunks: BTreeMap<u64, Vec<u8>>,
    inflight: BTreeSet<u64>,
    heads: Vec<Head>,
    /// The offset a read is blocked on.
    wanted: Option<u64>,
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
    ranges: Arc<R>,
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
        ranges: Arc::new(ranges),
        state: Mutex::new(State {
            chunks: BTreeMap::new(),
            inflight: BTreeSet::new(),
            heads: vec![Head { reader: None, at: 0, span: Instant::now(), seen: Instant::now(), waiting: false }],
            wanted: None,
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

    /// Bytes already here past `head`, up to the first gap.
    fn held_past(&self, st: &State, head: u64) -> u64 {
        let mut i = head / CHUNK;
        while st.chunks.contains_key(&i) {
            i += 1;
        }
        (i * CHUNK).min(self.len).saturating_sub(head)
    }

    /// The reader furthest back: the picture at the playhead. The sound's decoder reads as far
    /// ahead as its window allows, so it waits at the download's edge even with plenty in hand.
    fn earliest(st: &State) -> Option<u64> {
        st.heads.iter().map(|h| h.at).min()
    }

    /// Only the playhead's reader waiting holds playback.
    fn playhead_waits(st: &State) -> bool {
        st.heads.iter().min_by_key(|h| h.at).is_some_and(|h| h.waiting)
    }

    /// Seconds of the clip already here past the playhead's reader.
    pub fn ahead_secs(&self) -> Option<f64> {
        let st = self.state.lock().unwrap();
        let d = st.duration_ms.filter(|d| *d > 0)?;
        let held = self.held_past(&st, Self::earliest(&st)?);
        Some(held as f64 * d as f64 / 1000. / self.len as f64)
    }

    /// Everything past every reader to the end is here.
    pub fn complete_ahead(&self) -> bool {
        let st = self.state.lock().unwrap();
        !st.heads.is_empty() && st.heads.iter().all(|h| (h.at / CHUNK..self.chunks()).all(|i| st.chunks.contains_key(&i)))
    }

    /// The player moved: until the readers read again, nothing counts as buffered.
    pub fn sought(&self) {
        let mut st = self.state.lock().unwrap();
        st.heads.retain(|h| h.waiting);
        self.moved.notify_one();
    }

    fn touch(&self, st: &mut State, offset: u64, waiting: bool) {
        let now = Instant::now();
        let me = std::thread::current().id();
        let before = st.heads.len();
        // The opening's stand-in head goes once a real reader shows up.
        st.heads.retain(|h| h.reader.is_some() && (h.reader == Some(me) || h.waiting || now.duration_since(h.seen) < HEAD_LIFE));
        let pruned = st.heads.len() < before;
        match st.heads.iter_mut().find(|h| h.reader == Some(me)) {
            Some(h) => {
                if now.duration_since(h.span) > HEAD_SPAN {
                    (h.at, h.span) = (offset, now);
                } else {
                    h.at = h.at.min(offset);
                }
                (h.seen, h.waiting) = (now, waiting);
            }
            None => st.heads.push(Head { reader: Some(me), at: offset, span: now, seen: now, waiting }),
        }
        if pruned {
            self.evict(st);
        }
    }

    #[cfg(test)]
    pub fn ranges(&self) -> &R {
        &self.ranges
    }

    /// A read is waiting on the network.
    pub fn starving(&self) -> bool {
        Self::playhead_waits(&self.state.lock().unwrap())
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

    /// What no reader needs goes: far behind each, or past twice the window ahead. The opening
    /// stays, as players re-read the header on a seek.
    fn evict(&self, st: &mut State) {
        let behind = KEEP_BEHIND / CHUNK;
        let ahead = self.window(st).div_ceil(CHUNK) * 2 + 1;
        let unhashed = st.hasher.is_some().then(|| st.hashed / CHUNK);
        let heads: Vec<u64> = st.heads.iter().map(|h| h.at / CHUNK).collect();
        let needed = |i: u64| i < HEAD / CHUNK || heads.iter().any(|&h| i + behind >= h && i <= h + ahead);
        let drop: Vec<u64> = st.chunks.keys().copied().filter(|&i| !needed(i)).collect();
        for i in drop {
            st.chunks.remove(&i);
            // A hole now precedes chunks not yet hashed: the in-order hash can't finish.
            if unhashed.is_some_and(|h| i >= h) {
                st.hasher = None;
            }
        }
    }

    /// The next run of chunks to fetch.
    fn next(&self, st: &mut State) -> Option<(u64, u64)> {
        let window = self.window(st);
        let gap = |from: u64, st: &State| {
            let end = ((from + window).div_ceil(CHUNK)).min(self.chunks());
            (from / CHUNK..end).find(|i| !st.chunks.contains_key(i) && !st.inflight.contains(i)).map(|i| (i, end))
        };
        // The playhead's window first, then a read waiting elsewhere (the sound at the edge),
        // then the other readers front to back.
        let urgent = Self::earliest(st).and_then(|h| gap(h, st));
        let blocked = st.wanted.and_then(|w| gap(w, st));
        let (start, end) = urgent.or(blocked).or_else(|| {
            let mut heads: Vec<u64> = st.heads.iter().map(|h| h.at).collect();
            heads.sort_unstable();
            heads.into_iter().find_map(|h| gap(h, st))
        })?;
        let burst = if Self::playhead_waits(st) { WAITING_BURST } else { MAX_BURST };
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
    use futures::stream::{FuturesUnordered, StreamExt};
    let mut failures = 0u32;
    let mut flying = FuturesUnordered::new();
    loop {
        let Some(s) = weak.upgrade() else { return };
        if s.state.lock().unwrap().error.is_some() {
            return;
        }
        while flying.len() < PARALLEL {
            let next = {
                let mut st = s.state.lock().unwrap();
                s.next(&mut st)
            };
            let Some((first, n)) = next else { break };
            let (start, end) = (first * CHUNK, ((first + n) * CHUNK).min(s.len));
            // The requests hold the host, never the stream: letting go of the player ends them.
            let ranges = s.ranges.clone();
            flying.push(async move {
                let t0 = Instant::now();
                let got = ranges.get(start, end).await;
                (first, n, end - start, got, t0.elapsed())
            });
        }
        let moved = s.moved.clone();
        drop(s);
        let landed = tokio::select! {
            l = flying.next(), if !flying.is_empty() => l,
            _ = tokio::time::timeout(Duration::from_millis(500), moved.notified()) => None,
        };
        let Some((first, n, bytes, got, took)) = landed else { continue };
        let Some(s) = weak.upgrade() else { return };
        // Round trip of a small request is mostly latency; of a big one mostly transfer.
        let ttfb = if bytes <= CHUNK { took } else { took.mul_f64(CHUNK as f64 / bytes as f64) };
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
        self.touch(&mut st, offset, false);
        self.moved.notify_one();
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
            st.wanted = Some(at);
            self.touch(&mut st, at, true);
            self.moved.notify_one();
            let step = Duration::from_millis(250);
            st = self.arrived.wait_timeout(st, step).unwrap().0;
            waited += step;
        }
        if st.wanted.is_some_and(|w| w >= offset && w < offset + want as u64) {
            st.wanted = None;
        }
        if waited > Duration::ZERO {
            self.touch(&mut st, offset + done as u64, false);
        }
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
        // Past a head's life, a read leaves only its own window held.
        std::thread::sleep(HEAD_LIFE + Duration::from_millis(200));
        r.seek(SeekFrom::Start(5_000_000)).unwrap();
        r.read_exact(&mut [0u8; 1024]).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        let held: u64 = s.state.lock().unwrap().chunks.values().map(|c| c.len() as u64).sum();
        // Window (10 s of a 4 Mbit/s clip ≈ 5 MB) twice, plus what sits behind and the opening.
        assert!(held < 16 * 1024 * 1024, "held {held}");
    }

    #[test]
    fn a_seek_moves_its_readers_head_and_waiting_there_starves() {
        let rt = rt();
        let v = video(40 * 1024 * 1024);
        let mut m = mem(v.clone());
        m.bps = Some(1024. * 1024.);
        let s = rt.block_on(open(m, None)).ok().unwrap();
        s.set_duration(80_000);
        let src: Arc<dyn MediaSource> = s.clone();
        let far = 30 * 1024 * 1024usize;
        let (went, gone) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            let mut r = vector_core::media::reader(src);
            r.read_exact(&mut [0u8; 1024]).unwrap();
            went.send(()).unwrap();
            r.seek(SeekFrom::Start(far as u64)).unwrap();
            let mut b = vec![0u8; 4096];
            r.read_exact(&mut b).unwrap();
            b
        });
        gone.recv().unwrap();
        let t0 = Instant::now();
        while !s.starving() && t0.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(s.starving(), "a read blocked at the new spot holds playback");
        assert_eq!(s.state.lock().unwrap().heads.len(), 1, "the seek moved the head, leaving none behind");
        assert_eq!(&reader.join().unwrap()[..], &v[far..far + 4096]);
        assert!(!s.starving());
        s.sought();
        assert_eq!(s.ahead_secs(), None);
        assert!(!s.complete_ahead());
    }

    #[test]
    fn a_host_slow_to_answer_is_asked_several_ranges_at_once() {
        let rt = rt();
        let mut m = mem(video(40 * 1024 * 1024));
        m.latency = Duration::from_secs(1);
        let s = rt.block_on(open(m, None)).ok().unwrap();
        s.set_duration(80_000);
        std::thread::sleep(Duration::from_millis(2500));
        // One at a time, two 2 MB answers fit in this; three lanes fill the window.
        let served = s.ranges().served.load(std::sync::atomic::Ordering::Relaxed);
        assert!(served >= 11 * 512 * 1024, "served {served}");
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
