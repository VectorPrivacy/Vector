//! An unencrypted web video played while it downloads, as a browser does: a connection per
//! region a decoder reads reads on from where it needs bytes, about ten seconds ahead (more on a
//! slow or uneven link), and pauses when that is in hand. The decoder's reads wait for the bytes
//! they need; a read elsewhere (a seek, the sound's own position) opens a connection there.

use std::collections::BTreeMap;
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
/// A head its reader has read elsewhere for this long is gone.
const HEAD_LIFE: Duration = Duration::from_secs(3);
/// A reader that has read nothing at all for this long is gone, its heads with it.
const READER_LIFE: Duration = Duration::from_secs(30);
const READ_TIMEOUT: Duration = Duration::from_secs(45);
const ATTEMPTS: u32 = 4;
/// A connection that brings nothing for this long is dropped and opened again where it was.
const NO_PROGRESS: Duration = Duration::from_secs(6);
/// Connections kept open at once, each reading its own region.
const CONNS: usize = 3;
/// How far short of a gap a connection may be and still read on to it, in chunks.
const READ_THROUGH: u64 = 4;
/// Unread this long, a connection may have been closed by the host.
const PARKED: Duration = Duration::from_secs(5);
/// A read at least this far from a reader's heads starts a new one, however small the window.
const REGION: u64 = 8 * 1024 * 1024;

/// One answer to a range request.
pub struct Got {
    pub bytes: Vec<u8>,
    /// The whole file's size, when the host says.
    pub total: Option<u64>,
    /// A 206: the host honoured the range.
    pub ranged: bool,
}

/// The bytes of an open connection, as they come; `None` at the file's end.
pub trait Body: Send + 'static {
    fn next(&mut self) -> impl Future<Output = Result<Option<Vec<u8>>, String>> + Send;
}

/// Where byte ranges come from: the web, or memory in a test.
pub trait Ranges: Send + Sync + 'static {
    type Body: Body;
    fn get(&self, start: u64, end: u64) -> impl Future<Output = Result<Got, String>> + Send;
    /// The file from `start` on.
    fn from(&self, start: u64) -> impl Future<Output = Result<Self::Body, String>> + Send;
}

pub enum Open {
    /// The host serves only whole files: download it the old way.
    NoRanges,
    Failed(String),
}

/// Where one reader is reading in one region of the file. Each decoder reads on a thread of its
/// own, and one reader can need two distant regions at once: a file that stores its sound far
/// from its picture.
struct Head {
    reader: Option<std::thread::ThreadId>,
    /// The lowest offset it read this span: a demuxer hops between its tracks' samples, and
    /// the bytes at the low end are still wanted while it reads the high.
    at: u64,
    /// Where it read last: a seek forward lands here before the span's low end moves.
    last: u64,
    span: Instant,
    seen: Instant,
    waiting: bool,
}

struct State {
    chunks: BTreeMap<u64, Vec<u8>>,
    heads: Vec<Head>,
    error: Option<String>,
    duration_ms: Option<u64>,
    /// Smoothed throughput (bytes/s) and time to first byte (s).
    rate: Option<f64>,
    latency: Option<f64>,
    /// In-order hash of what arrived, while nothing was skipped.
    hashed: u64,
    hasher: Option<vector_core::crypto::sha256::Sha256>,
    /// The picture's decoder, when the player names it: the playhead whatever the others read.
    lead: Option<std::thread::ThreadId>,
    /// The connections open or opening: a task each, which ends once its slot is gone.
    conns: Vec<Slot>,
    next_conn: u64,
    /// Failed connections in a row, and when the next may open.
    failures: u32,
    retry_at: Option<Instant>,
}

/// One connection's place: the chunk it fills next, and when it last read.
struct Slot {
    id: u64,
    chunk: u64,
    used: Instant,
}

/// What a connection does next.
enum Want {
    Read,
    /// Past every window but still in a reader's way: unread, the host held back by TCP.
    Park,
    Close,
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
            heads: vec![Head { reader: None, at: 0, last: 0, span: Instant::now(), seen: Instant::now(), waiting: false }],
            error: None,
            duration_ms: None,
            rate: Some(first.bytes.len() as f64 / took),
            latency: Some(took * CHUNK as f64 / HEAD as f64),
            hashed: 0,
            hasher: None,
            lead: None,
            conns: Vec::new(),
            next_conn: 0,
            failures: 0,
            retry_at: None,
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
    fn playhead(st: &State) -> Option<Option<std::thread::ThreadId>> {
        if let Some(lead) = st.lead
            && st.heads.iter().any(|h| h.reader == Some(lead))
        {
            return Some(Some(lead));
        }
        // Two readers can share a span's low end; the one whose last read is further back leads.
        st.heads.iter().min_by_key(|h| (h.at, h.last)).map(|h| h.reader)
    }

    /// The thread decoding the picture: in a file that keeps its sound apart, the sound's
    /// decoder can sit lower in one region while far ahead in time.
    pub fn lead(&self, reader: std::thread::ThreadId) {
        self.state.lock().unwrap().lead = Some(reader);
    }

    /// Only the playhead's reader waiting holds playback.
    fn playhead_waits(st: &State) -> bool {
        let p = Self::playhead(st);
        st.heads.iter().any(|h| Some(h.reader) == p && h.waiting)
    }

    /// Seconds of the clip already here past the playhead's reader, in its region nearest a gap.
    pub fn ahead_secs(&self) -> Option<f64> {
        let st = self.state.lock().unwrap();
        let d = st.duration_ms.filter(|d| *d > 0)?;
        let p = Self::playhead(&st)?;
        let mine: Vec<&Head> = st.heads.iter().filter(|h| h.reader == p).collect();
        let held = |h: &&Head| self.held_past(&st, h.at).min(self.held_past(&st, h.last));
        // A region in hand to the file's end (the index a demuxer reads there) can't run dry.
        let open = |h: &&&Head| h.at + self.held_past(&st, h.at) < self.len;
        let held = mine.iter().filter(open).map(held).min().or_else(|| mine.iter().map(held).max())?;
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
        let near = self.window(st).max(REGION);
        let before = st.heads.len();
        // The opening's stand-in head goes once a real reader shows up. A head lapses once its
        // reader reads on elsewhere: one blocked, or holding still (a full frame queue, playback
        // not started), still needs every region it was reading.
        let blocked: Vec<_> = st.heads.iter().filter(|h| h.waiting).map(|h| h.reader).collect();
        let latest = |r: Option<std::thread::ThreadId>| {
            if r == Some(me) { now } else { st.heads.iter().filter(|h| h.reader == r).map(|h| h.seen).max().unwrap_or(now) }
        };
        let keep: Vec<bool> = st
            .heads
            .iter()
            .map(|h| {
                let last = latest(h.reader);
                h.reader.is_some()
                    && (blocked.contains(&h.reader) || now.duration_since(h.seen) < HEAD_LIFE || (last.duration_since(h.seen) < HEAD_LIFE && now.duration_since(last) < READER_LIFE))
            })
            .collect();
        let mut keep = keep.into_iter();
        st.heads.retain(|_| keep.next().unwrap_or(false));
        let pruned = st.heads.len() < before;
        let dist = |h: &Head| offset.abs_diff(h.at).min(offset.abs_diff(h.last));
        let mine = st.heads.iter().enumerate().filter(|(_, h)| h.reader == Some(me) && dist(h) <= near).min_by_key(|(_, h)| dist(h)).map(|(i, _)| i);
        // A thread blocks on one read at a time: its other regions aren't waiting.
        for h in st.heads.iter_mut().filter(|h| h.reader == Some(me)) {
            h.waiting = false;
        }
        match mine.map(|i| &mut st.heads[i]) {
            Some(h) => {
                if now.duration_since(h.span) > HEAD_SPAN {
                    (h.at, h.span) = (offset, now);
                } else {
                    h.at = h.at.min(offset);
                }
                (h.last, h.seen, h.waiting) = (offset, now, waiting);
            }
            None => st.heads.push(Head { reader: Some(me), at: offset, last: offset, span: now, seen: now, waiting }),
        }
        if pruned {
            self.evict(st);
        }
    }

    #[cfg(test)]
    pub fn ranges(&self) -> &R {
        &self.ranges
    }

    /// What a stuck read was waiting on: the heads, the connections and the gaps they chase.
    #[cfg(test)]
    fn dump(&self) -> String {
        let st = self.state.lock().unwrap();
        let heads: Vec<String> = st.heads.iter().map(|h| format!("{:?}@{}..{}{}", h.reader, h.at / CHUNK, h.last / CHUNK, if h.waiting { " waiting" } else { "" })).collect();
        let conns: Vec<String> = st.conns.iter().map(|c| format!("#{}@{} read {:?} ago", c.id, c.chunk, c.used.elapsed())).collect();
        format!("heads {heads:?}, conns {conns:?}, gaps {:?}, failures {}, window {} chunks", self.gaps(&st), st.failures, self.window(&st) / CHUNK)
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
        let heads: Vec<u64> = st.heads.iter().flat_map(|h| [h.at / CHUNK, h.last / CHUNK]).collect();
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

    /// The chunks to fetch next, a reader's first gap each: a waiting read (the playhead's
    /// first), then the playhead's regions, least in hand first, then the other readers'.
    fn gaps(&self, st: &State) -> Vec<u64> {
        let window = self.window(st);
        let gap = |from: u64| {
            let end = ((from + window).div_ceil(CHUNK)).min(self.chunks());
            (from / CHUNK..end).find(|i| !st.chunks.contains_key(i))
        };
        let p = Self::playhead(st);
        let mut heads: Vec<&Head> = st.heads.iter().collect();
        heads.sort_by_key(|h| (!h.waiting, Some(h.reader) != p, self.held_past(st, h.at)));
        let mut out: Vec<u64> = Vec::new();
        for g in heads.into_iter().filter_map(|h| if h.waiting { gap(h.last).or_else(|| gap(h.at)) } else { gap(h.at) }) {
            if !out.contains(&g) {
                out.push(g);
            }
        }
        out
    }

    /// Whether connection `id`, about to fill `chunk`, reads on, waits, or goes.
    fn want(&self, st: &State, id: u64, chunk: u64) -> Want {
        if chunk >= self.chunks() || !st.conns.iter().any(|c| c.id == id) {
            return Want::Close;
        }
        let have = |k: u64| st.chunks.contains_key(&k);
        // Another connection just ahead has everything between: from here it would read the same.
        if have(chunk) && st.conns.iter().any(|o| o.id != id && o.chunk > chunk && o.chunk <= chunk + READ_THROUGH + 1 && (chunk..o.chunk).all(have)) {
            return Want::Close;
        }
        let window = self.window(st).div_ceil(CHUNK);
        let (mut inside, mut beyond) = (false, false);
        for h in &st.heads {
            let lo = (h.at / CHUNK).saturating_sub(1);
            let hi = h.at.max(h.last) / CHUNK + window;
            inside |= chunk >= lo && chunk < hi;
            beyond |= chunk >= hi && chunk < hi + window;
        }
        let end = (chunk + READ_THROUGH + 1).min(self.chunks());
        if inside && (chunk..end).any(|k| !have(k)) {
            Want::Read
        } else if beyond && !have(chunk) {
            Want::Park
        } else {
            Want::Close
        }
    }

    /// A smoothed throughput (bytes/s) or time to first byte (s) sample.
    fn measured(&self, rate: Option<f64>, latency: Option<f64>) {
        let mut st = self.state.lock().unwrap();
        if let Some(r) = rate {
            st.rate = Some(st.rate.map_or(r, |old| old * 0.7 + r * 0.3));
        }
        if let Some(l) = latency {
            st.latency = Some(st.latency.map_or(l, |old| old * 0.7 + l * 0.3));
        }
    }

    fn fail(&self, e: String) {
        self.state.lock().unwrap().error = Some(e);
        self.arrived.notify_all();
    }
}

/// Opens a connection at each reader's gap no open one covers, without waiting on any: a slow
/// host answering one range must not hold up another the playhead needs.
async fn fetch_loop<R: Ranges>(weak: Weak<Stream<R>>) {
    loop {
        let Some(s) = weak.upgrade() else { return };
        {
            let mut st = s.state.lock().unwrap();
            if st.error.is_some() {
                return;
            }
            if st.retry_at.is_none_or(|t| Instant::now() >= t) {
                let top: Vec<u64> = s.gaps(&st).into_iter().take(CONNS).collect();
                let covers = |c: &Slot, g: u64| c.chunk <= g && g - c.chunk <= READ_THROUGH;
                for &g in &top {
                    if st.conns.iter().any(|c| covers(c, g)) {
                        continue;
                    }
                    if st.conns.len() >= CONNS {
                        let spare = st.conns.iter().enumerate().filter(|(_, c)| !top.iter().any(|&t| covers(c, t))).min_by_key(|(_, c)| c.used).map(|(i, _)| i);
                        let Some(i) = spare else { break };
                        st.conns.swap_remove(i);
                    }
                    let id = st.next_conn;
                    st.next_conn += 1;
                    st.conns.push(Slot { id, chunk: g, used: Instant::now() });
                    // spawn-detached: one connection of a web video; it holds the stream only weakly and ends when the stream or its slot goes.
                    tokio::spawn(connection(weak.clone(), s.ranges.clone(), id, g));
                }
            }
        }
        let moved = s.moved.clone();
        drop(s);
        let _ = tokio::time::timeout(Duration::from_millis(100), moved.notified()).await;
    }
}

/// One range read on while a reader needs it, storing each chunk as it fills.
async fn connection<R: Ranges>(weak: Weak<Stream<R>>, ranges: Arc<R>, id: u64, start: u64) {
    let opened = Instant::now();
    let opening = tokio::time::timeout(NO_PROGRESS, ranges.from(start * CHUNK)).await;
    drop(ranges);
    let mut body = match opening {
        Ok(Ok(body)) => body,
        Ok(Err(e)) => return failed(&weak, id, e),
        Err(_) => return failed(&weak, id, "The video host stopped answering".into()),
    };
    let (mut chunk, mut buf, mut used, mut answered) = (start, Vec::new(), opened, false);
    // Bytes and time spent waiting on the body since the last rate sample.
    let mut tally = (0usize, Duration::ZERO);
    loop {
        let Some(s) = weak.upgrade() else { return };
        let want = s.want(&s.state.lock().unwrap(), id, chunk);
        match want {
            Want::Read => {}
            Want::Park => {
                drop(s);
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
            Want::Close => return s.state.lock().unwrap().conns.retain(|c| c.id != id),
        }
        drop(s);
        // One left unread a while may have been closed by the host: that is no failure.
        let parked = used.elapsed() > PARKED;
        let t0 = Instant::now();
        let piece = tokio::time::timeout(NO_PROGRESS, body.next()).await;
        let waited = t0.elapsed();
        used = Instant::now();
        let Some(s) = weak.upgrade() else { return };
        let bytes = match piece {
            Ok(Ok(Some(bytes))) => bytes,
            Ok(Ok(None)) => return s.state.lock().unwrap().conns.retain(|c| c.id != id),
            Ok(Err(_)) | Err(_) if parked => return s.state.lock().unwrap().conns.retain(|c| c.id != id),
            Ok(Err(e)) => return failed(&weak, id, e),
            Err(_) => return failed(&weak, id, "The video host stalled".into()),
        };
        if !answered {
            answered = true;
            s.measured(None, Some(opened.elapsed().as_secs_f64()));
        } else if !parked {
            tally = (tally.0 + bytes.len(), tally.1 + waited);
        }
        if tally.0 as u64 >= CHUNK {
            s.measured(Some(tally.0 as f64 / tally.1.as_secs_f64().max(1e-3)), None);
            tally = (0, Duration::ZERO);
        }
        buf.extend_from_slice(&bytes);
        let mut st = s.state.lock().unwrap();
        st.failures = 0;
        loop {
            let need = (s.len - (chunk * CHUNK).min(s.len)).min(CHUNK) as usize;
            if need == 0 || buf.len() < need {
                break;
            }
            let rest = buf.split_off(need);
            let whole = std::mem::replace(&mut buf, rest);
            s.store(&mut st, chunk, whole);
            chunk += 1;
        }
        if let Some(c) = st.conns.iter_mut().find(|c| c.id == id) {
            (c.chunk, c.used) = (chunk, used);
        }
    }
}

/// A failed connection: the next waits a little, and a few in a row stop the clip.
fn failed<R: Ranges>(weak: &Weak<Stream<R>>, id: u64, e: String) {
    let Some(s) = weak.upgrade() else { return };
    let mut st = s.state.lock().unwrap();
    st.conns.retain(|c| c.id != id);
    st.failures += 1;
    if st.failures >= ATTEMPTS {
        drop(st);
        s.fail(e);
        return;
    }
    st.retry_at = Some(Instant::now() + Duration::from_millis(500 * st.failures as u64));
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
            self.touch(&mut st, at, true);
            self.moved.notify_one();
            let step = Duration::from_millis(250);
            st = self.arrived.wait_timeout(st, step).unwrap().0;
            waited += step;
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
        // Ranges left unread hold the host back by TCP; on a shared HTTP/2 connection they would
        // fill the window the range being read needs.
        let client = vector_core::net::build_http_client_with_options(None, Some(vector_core::net::transfer_stall()), true)?.http1_only();
        Ok(Self { url: url.to_string(), client })
    }
}

pub struct WebBody(vector_core::net::Resp);

impl Body for WebBody {
    async fn next(&mut self) -> Result<Option<Vec<u8>>, String> {
        self.0.chunk().await.map(|b| b.map(|b| b.to_vec())).map_err(|e| e.to_string())
    }
}

impl Ranges for Web {
    type Body = WebBody;

    async fn from(&self, start: u64) -> Result<WebBody, String> {
        let resp = vector_core::net::proxied_request(&self.client, reqwest::Method::GET, &self.url)
            .await
            .header(reqwest::header::RANGE, format!("bytes={start}-"))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        match resp.status() {
            reqwest::StatusCode::PARTIAL_CONTENT => Ok(WebBody(resp)),
            s if s.is_success() => Err("The video host stopped answering ranges".into()),
            s => Err(format!("The video host answered {s}")),
        }
    }

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
        bytes: Arc<Vec<u8>>,
        ranged: bool,
        bps: Option<f64>,
        latency: Duration,
        served: Arc<std::sync::atomic::AtomicU64>,
        opened: std::sync::atomic::AtomicU32,
        /// The offset of every piece served, to find a byte fetched twice.
        pieces: Arc<Mutex<Vec<u64>>>,
        /// A range from this offset on takes this much longer to answer.
        slow_from: Option<(u64, Duration)>,
    }

    struct MemBody {
        bytes: Arc<Vec<u8>>,
        at: usize,
        bps: Option<f64>,
        served: Arc<std::sync::atomic::AtomicU64>,
        pieces: Arc<Mutex<Vec<u64>>>,
    }

    impl Body for MemBody {
        async fn next(&mut self) -> Result<Option<Vec<u8>>, String> {
            let end = (self.at + 64 * 1024).min(self.bytes.len());
            if self.at >= end {
                return Ok(None);
            }
            let part = self.bytes[self.at..end].to_vec();
            if let Some(bps) = self.bps {
                tokio::time::sleep(Duration::from_secs_f64(part.len() as f64 / bps)).await;
            }
            self.pieces.lock().unwrap().push(self.at as u64);
            self.at = end;
            self.served.fetch_add(part.len() as u64, std::sync::atomic::Ordering::Relaxed);
            Ok(Some(part))
        }
    }

    impl Ranges for Mem {
        type Body = MemBody;

        async fn from(&self, start: u64) -> Result<MemBody, String> {
            self.opened.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            tokio::time::sleep(self.latency).await;
            if let Some((at, extra)) = self.slow_from
                && start >= at
            {
                tokio::time::sleep(extra).await;
            }
            Ok(MemBody { bytes: self.bytes.clone(), at: start as usize, bps: self.bps, served: self.served.clone(), pieces: self.pieces.clone() })
        }

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
        Mem { bytes: Arc::new(bytes), ranged: true, bps: None, latency: Duration::ZERO, served: Default::default(), opened: Default::default(), pieces: Default::default(), slow_from: None }
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
        assert_eq!(s.state.lock().unwrap().heads.iter().filter(|h| h.waiting).count(), 1, "only the read at the new spot waits");
        assert_eq!(&reader.join().unwrap()[..], &v[far..far + 4096]);
        assert!(!s.starving());
        s.sought();
        assert_eq!(s.ahead_secs(), None);
        assert!(!s.complete_ahead());
    }

    #[test]
    fn a_host_slow_to_answer_pays_its_delay_once() {
        let rt = rt();
        let mut m = mem(video(40 * 1024 * 1024));
        m.latency = Duration::from_secs(1);
        let s = rt.block_on(open(m, None)).ok().unwrap();
        s.set_duration(80_000);
        std::thread::sleep(Duration::from_millis(2500));
        // One connection reads on: the window is in hand after a single round trip.
        let held: u64 = s.state.lock().unwrap().chunks.values().map(|c| c.len() as u64).sum();
        assert!(held >= 5 * 1024 * 1024, "held {held}");
        assert_eq!(s.ranges().opened.load(std::sync::atomic::Ordering::Relaxed), 1);
    }

    #[test]
    fn a_reader_in_two_distant_regions_fetches_each_byte_once() {
        let rt = rt();
        let gap = 90 * 1024 * 1024u64;
        let v = video(120 * 1024 * 1024);
        let mut m = mem(v.clone());
        m.bps = Some(4. * 1024. * 1024.);
        m.latency = Duration::from_millis(200);
        let pieces = m.pieces.clone();
        let s = rt.block_on(open(m, None)).ok().unwrap();
        s.set_duration(120_000);
        let src: Arc<dyn MediaSource> = s.clone();
        // The sound for each moment sits 90 MB before its picture; the demuxer reads them in turn.
        let read = |a: u64, len: u64| {
            let mut r = vector_core::media::reader(src.clone());
            let mut b = vec![0u8; 256 * 1024];
            let mut off = 0;
            while off < len {
                for at in [a + off, a + gap + off] {
                    r.seek(SeekFrom::Start(at)).unwrap();
                    r.read_exact(&mut b).unwrap_or_else(|e| panic!("{e} at {at}: {}", s.dump()));
                    assert_eq!(&b[..], &v[at as usize..at as usize + b.len()]);
                }
                off += b.len() as u64;
            }
        };
        let t0 = Instant::now();
        read(1024 * 1024, 6 * 1024 * 1024);
        s.sought();
        read(20 * 1024 * 1024, 6 * 1024 * 1024);
        assert!(t0.elapsed() < Duration::from_secs(20), "took {:?}", t0.elapsed());
        let mut seen = pieces.lock().unwrap().clone();
        let all = seen.len();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), all, "a piece was fetched twice");
        assert!(s.ranges().opened.load(std::sync::atomic::Ordering::Relaxed) <= 6);
    }

    #[test]
    fn the_playheads_range_never_waits_behind_a_slow_answer_elsewhere() {
        let rt = rt();
        let v = video(60 * 1024 * 1024);
        let mut m = mem(v.clone());
        m.slow_from = Some((40 * 1024 * 1024, Duration::from_secs(3)));
        let s = rt.block_on(open(m, None)).ok().unwrap();
        s.set_duration(120_000);
        let src: Arc<dyn MediaSource> = s.clone();
        // Another reader asks far ahead, where the host takes seconds to answer.
        let far = src.clone();
        let other = std::thread::spawn(move || {
            let mut r = vector_core::media::reader(far);
            r.seek(SeekFrom::Start(50 * 1024 * 1024)).unwrap();
            r.read_exact(&mut [0u8; 4096]).unwrap();
        });
        std::thread::sleep(Duration::from_millis(200));
        let t0 = Instant::now();
        let at = 10 * 1024 * 1024usize;
        let mut r = vector_core::media::reader(src);
        r.seek(SeekFrom::Start(at as u64)).unwrap();
        let mut b = vec![0u8; 4096];
        r.read_exact(&mut b).unwrap();
        assert_eq!(&b[..], &v[at..at + 4096]);
        assert!(t0.elapsed() < Duration::from_secs(1), "waited {:?}", t0.elapsed());
        other.join().unwrap();
    }

    #[test]
    fn a_reader_ahead_of_the_playhead_waiting_doesnt_hold_playback() {
        let rt = rt();
        let mut m = mem(video(60 * 1024 * 1024));
        m.bps = Some(1024. * 1024.);
        let s = rt.block_on(open(m, None)).ok().unwrap();
        s.set_duration(120_000);
        let src: Arc<dyn MediaSource> = s.clone();
        let (went, gone) = std::sync::mpsc::channel();
        // The sound's decoder: it starts where the picture does, then reads to the download's edge.
        let ahead = src.clone();
        let sound = std::thread::spawn(move || {
            let mut r = vector_core::media::reader(ahead);
            r.read_exact(&mut [0u8; 1024]).unwrap();
            went.send(()).unwrap();
            r.seek(SeekFrom::Start(6 * 1024 * 1024)).unwrap();
            r.read_exact(&mut [0u8; 1024]).unwrap();
        });
        gone.recv().unwrap();
        let mut r = vector_core::media::reader(src);
        r.read_exact(&mut [0u8; 1024]).unwrap();
        let t0 = Instant::now();
        while !s.state.lock().unwrap().heads.iter().any(|h| h.waiting) && t0.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(s.state.lock().unwrap().heads.iter().any(|h| h.waiting), "the sound waits at the edge");
        assert!(!s.starving(), "the picture has its bytes: playback goes on");
        sound.join().unwrap();
    }

    #[test]
    fn a_reader_holding_still_keeps_its_bytes() {
        let rt = rt();
        let v = video(40 * 1024 * 1024);
        let s = rt.block_on(open(mem(v), None)).ok().unwrap();
        s.set_duration(80_000);
        let src: Arc<dyn MediaSource> = s.clone();
        // The picture's decoder reads, then holds still while its frame queue is full.
        let at = 1024 * 1024u64;
        let mut r = vector_core::media::reader(src.clone());
        r.seek(SeekFrom::Start(at)).unwrap();
        r.read_exact(&mut [0u8; 1024]).unwrap();
        // The sound's decoder reads on far away meanwhile.
        let sound = std::thread::spawn(move || {
            let mut r = vector_core::media::reader(src);
            let t0 = Instant::now();
            let mut off = 20 * 1024 * 1024u64;
            while t0.elapsed() < HEAD_LIFE + Duration::from_secs(1) {
                r.seek(SeekFrom::Start(off)).unwrap();
                r.read_exact(&mut [0u8; 1024]).unwrap();
                off += 16 * 1024;
                std::thread::sleep(Duration::from_millis(100));
            }
        });
        sound.join().unwrap();
        assert!(s.state.lock().unwrap().chunks.contains_key(&(at / CHUNK)), "the bytes it reads next were dropped");
    }

    #[test]
    fn the_named_picture_reader_is_the_playhead() {
        let rt = rt();
        let mut m = mem(video(60 * 1024 * 1024));
        m.bps = Some(1024. * 1024.);
        let s = rt.block_on(open(m, None)).ok().unwrap();
        s.set_duration(120_000);
        let src: Arc<dyn MediaSource> = s.clone();
        s.lead(std::thread::current().id());
        let mut r = vector_core::media::reader(src.clone());
        r.seek(SeekFrom::Start(2 * 1024 * 1024)).unwrap();
        r.read_exact(&mut [0u8; 1024]).unwrap();
        // The sound's decoder reads lower in the file, then waits at the download's edge.
        let sound = std::thread::spawn(move || {
            let mut r = vector_core::media::reader(src);
            r.read_exact(&mut [0u8; 1024]).unwrap();
            r.seek(SeekFrom::Start(7 * 1024 * 1024)).unwrap();
            r.read_exact(&mut [0u8; 1024]).unwrap();
        });
        let t0 = Instant::now();
        while !s.state.lock().unwrap().heads.iter().any(|h| h.waiting) && t0.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(s.state.lock().unwrap().heads.iter().any(|h| h.waiting), "the sound waits at the edge");
        assert!(!s.starving(), "the picture has its bytes: playback goes on");
        sound.join().unwrap();
    }

    #[test]
    fn an_index_at_the_files_end_doesnt_hold_back_playback() {
        let rt = rt();
        let v = video(40 * 1024 * 1024);
        let s = rt.block_on(open(mem(v.clone()), None)).ok().unwrap();
        s.set_duration(80_000);
        let src: Arc<dyn MediaSource> = s.clone();
        let mut r = vector_core::media::reader(src);
        r.read_exact(&mut [0u8; 1024]).unwrap();
        r.seek(SeekFrom::Start(v.len() as u64 - 4096)).unwrap();
        r.read_exact(&mut [0u8; 4096]).unwrap();
        r.seek(SeekFrom::Start(2048)).unwrap();
        r.read_exact(&mut [0u8; 1024]).unwrap();
        std::thread::sleep(Duration::from_millis(500));
        let ahead = s.ahead_secs().unwrap();
        assert!(ahead > 5., "ahead {ahead:.2}s");
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
