//! A clip's sound at another speed, as an AIFF the audio engine reads while it is being made:
//! decoded and stretched a little ahead of the engine's reads, so a web video is never read
//! whole for it. Not a WAV: the engine reads a WAV whole.

use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::Duration;

use vector_media::audio_engine::AudioInput;

use super::ff::AudioReader;
use crate::stretch::Stretcher;

const HEADER: u64 = 54;
/// Past the samples: the demuxer counts the sound chunk's offset fields as audio.
const TAIL: u64 = 8;
/// How far past the engine's latest read the sound is made, and how much before it is kept.
const AHEAD_SECS: u64 = 4;
const BEHIND_SECS: u64 = 2;
/// A read this far past what is made starts over there rather than waiting for it.
const JUMP_SECS: u64 = 3;
const READ_TIMEOUT: Duration = Duration::from_secs(45);

pub struct Stretched {
    len: u64,
    rate: u32,
    header: [u8; HEADER as usize],
    state: Mutex<St>,
    /// Made more: for the readers.
    made: Condvar,
    /// Read on, or elsewhere: for the maker.
    wanted: Condvar,
}

struct St {
    /// Bumped by each start over, so sound made for the old place is never added.
    epoch: u64,
    /// The frame `pcm` starts at.
    start: u64,
    pcm: Vec<i16>,
    /// Made to the track's end from `start`.
    done: bool,
    /// The frame after the engine's latest read.
    read: u64,
}

/// `input`'s sound at `speed`, for a clip of `duration_ms`.
pub fn open(input: AudioInput, speed: f32, duration_ms: u64) -> Result<Arc<Stretched>, String> {
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    // spawn-detached: decodes and stretches one clip's sound; it holds its WAV only weakly.
    std::thread::Builder::new()
        .name("audio-stretch".into())
        .spawn(move || {
            let reader = match AudioReader::open(&input) {
                Ok(r) => r,
                Err(e) => {
                    let _ = tx.send(Err(e));
                    return;
                }
            };
            let frames = (duration_ms as f64 / 1000. * reader.rate() as f64 / speed as f64).ceil() as u64;
            let s = Arc::new(Stretched::new(reader.rate(), frames));
            let weak = Arc::downgrade(&s);
            if tx.send(Ok(s)).is_ok() {
                make(weak, reader, speed);
            }
        })
        .map_err(|e| e.to_string())?;
    rx.recv().map_err(|_| "no audio".to_string())? // i18n-exempt: developer error
}

impl Stretched {
    fn new(rate: u32, frames: u64) -> Self {
        let data = (frames * 2) as u32;
        let len = HEADER + data as u64 + TAIL;
        let mut h = [0u8; HEADER as usize];
        h[0..4].copy_from_slice(b"FORM");
        h[4..8].copy_from_slice(&((len - 8) as u32).to_be_bytes());
        h[8..16].copy_from_slice(b"AIFFCOMM");
        h[16..20].copy_from_slice(&18u32.to_be_bytes());
        h[20..22].copy_from_slice(&1u16.to_be_bytes());
        h[22..26].copy_from_slice(&(frames as u32).to_be_bytes());
        h[26..28].copy_from_slice(&16u16.to_be_bytes());
        // The rate as an 80-bit extended float: a biased exponent, then the mantissa's explicit leading bit.
        let top = 31 - rate.max(1).leading_zeros();
        h[28..30].copy_from_slice(&(16383 + top as u16).to_be_bytes());
        h[30..38].copy_from_slice(&((rate.max(1) as u64) << (63 - top)).to_be_bytes());
        h[38..42].copy_from_slice(b"SSND");
        h[42..46].copy_from_slice(&(8 + data).to_be_bytes());
        Self {
            len,
            rate,
            header: h,
            state: Mutex::new(St { epoch: 0, start: 0, pcm: Vec::new(), done: false, read: 0 }),
            made: Condvar::new(),
            wanted: Condvar::new(),
        }
    }
}

/// Makes the sound from wherever the engine reads, a few seconds ahead of it.
fn make(weak: Weak<Stretched>, mut reader: AudioReader, speed: f32) {
    let rate = reader.rate();
    let secs = rate as u64;
    let mut epoch = u64::MAX;
    let mut stretcher = Stretcher::new(rate, speed);
    // The track's sample the made sound starts at, and the next decoded one.
    let (mut from, mut next) = (0u64, 0u64);
    let mut ended = false;
    let (mut piece, mut out) = (Vec::new(), Vec::new());
    loop {
        let Some(s) = weak.upgrade() else { return };
        {
            let st = s.state.lock().unwrap_or_else(|e| e.into_inner());
            if st.epoch != epoch {
                epoch = st.epoch;
                from = (st.start as f64 * speed as f64).round() as u64;
                next = from;
                reader.seek(from * 1000 / secs);
                stretcher = Stretcher::new(rate, speed);
                ended = false;
            }
            if ended || st.start + st.pcm.len() as u64 >= st.read + AHEAD_SECS * secs {
                let _ = s.wanted.wait_timeout(st, Duration::from_millis(250));
                continue;
            }
        }
        piece.clear();
        match reader.next(&mut piece) {
            Some(at) => {
                let first = at.map_or(next, |t| (t * rate as f64).round().max(0.) as u64);
                next = first + piece.len() as u64;
                // A seek lands on the packet before: what precedes the wanted moment goes.
                let skip = (from.saturating_sub(first) as usize).min(piece.len());
                stretcher.push(&piece[skip..], &mut out);
            }
            None => {
                stretcher.finish(&mut out);
                ended = true;
            }
        }
        let mut st = s.state.lock().unwrap_or_else(|e| e.into_inner());
        if st.epoch != epoch {
            out.clear();
            continue;
        }
        st.pcm.extend(out.drain(..).map(|x| (x.clamp(-1., 1.) * 32767.) as i16));
        st.done = ended;
        let keep = st.read.saturating_sub(BEHIND_SECS * secs);
        if keep > st.start + 4 * secs {
            let cut = ((keep - st.start) as usize).min(st.pcm.len());
            st.pcm.drain(..cut);
            st.start += cut as u64;
        }
        drop(st);
        s.made.notify_all();
    }
}

impl vector_core::media::MediaSource for Stretched {
    fn len(&self) -> u64 {
        self.len
    }

    fn mime(&self) -> &str {
        "audio/aiff"
    }

    fn remote(&self) -> bool {
        true
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<usize> {
        if offset >= self.len || buf.is_empty() {
            return Ok(0);
        }
        let total = buf.len().min((self.len - offset) as usize);
        // The samples end before the tail, which reads as silence.
        let want = (((self.len - TAIL).saturating_sub(offset)) as usize).min(total);
        buf[want..total].fill(0);
        let mut done = 0usize;
        if offset < HEADER {
            done = ((HEADER - offset) as usize).min(want);
            buf[..done].copy_from_slice(&self.header[offset as usize..offset as usize + done]);
        }
        let secs = self.rate as u64;
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let mut waited = Duration::ZERO;
        while done < want {
            let byte = offset + done as u64 - HEADER;
            let frame = byte / 2;
            let end = st.start + st.pcm.len() as u64;
            if frame >= st.start && frame < end {
                let (mut k, mut b) = ((frame - st.start) as usize, (byte % 2) as usize);
                while done < want && k < st.pcm.len() {
                    let bytes = st.pcm[k].to_be_bytes();
                    while b < 2 && done < want {
                        buf[done] = bytes[b];
                        (done, b) = (done + 1, b + 1);
                    }
                    (k, b) = (k + 1, 0);
                }
                continue;
            }
            if frame >= st.start && st.done {
                buf[done..want].fill(0);
                break;
            }
            if frame < st.start || frame > end + JUMP_SECS * secs {
                st.epoch += 1;
                (st.start, st.done) = (frame, false);
                st.pcm.clear();
            }
            st.read = frame + 1;
            self.wanted.notify_all();
            if waited >= READ_TIMEOUT {
                return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, crate::t::video_sound_stopped().to_string()));
            }
            let step = Duration::from_millis(250);
            st = self.made.wait_timeout(st, step).unwrap_or_else(|e| e.into_inner()).0;
            waited += step;
        }
        st.read = (offset + want as u64).saturating_sub(HEADER).div_ceil(2);
        drop(st);
        self.wanted.notify_all();
        Ok(total)
    }
}

#[cfg(all(test, feature = "video"))]
mod tests {
    use super::*;
    use std::io::{Read, Seek, SeekFrom};
    use vector_core::media::MediaSource;

    fn clip() -> AudioInput {
        AudioInput::File(concat!(env!("CARGO_MANIFEST_DIR"), "/reference/media/clip.mp4").into())
    }

    #[test]
    fn reads_as_an_aiff_of_the_stretched_length_and_seeks() {
        let s = open(clip(), 2., 5000).unwrap();
        let mut r = vector_core::media::reader(s.clone());
        let mut head = [0u8; HEADER as usize];
        r.read_exact(&mut head).unwrap();
        assert_eq!((&head[..4], &head[8..12]), (&b"FORM"[..], &b"AIFF"[..]));
        let frames = u32::from_be_bytes(head[22..26].try_into().unwrap()) as u64;
        let data = u32::from_be_bytes(head[42..46].try_into().unwrap()) as u64 - 8;
        assert_eq!(data, frames * 2);
        let mut all = Vec::new();
        r.read_to_end(&mut all).unwrap();
        assert_eq!(all.len() as u64, data + TAIL);
        assert!(all.iter().any(|&b| b != 0), "silent");
        // Far back again: made over from there, the same sound.
        let at = HEADER + data / 4 * 2;
        r.seek(SeekFrom::Start(at)).unwrap();
        let mut again = vec![0u8; 4096];
        r.read_exact(&mut again).unwrap();
        let near = &all[(at - HEADER) as usize..(at - HEADER) as usize + 4096];
        let rms = |b: &[u8]| (b.chunks(2).map(|c| (i16::from_be_bytes([c[0], c[1]]) as f64).powi(2)).sum::<f64>() / (b.len() / 2) as f64).sqrt();
        let (a, b) = (rms(near), rms(&again));
        assert!(b > a * 0.5 && b < a * 2., "{a} vs {b}");
        assert_eq!(s.len(), HEADER + data + TAIL);
        // The engine reads it as the stretched clip: about half the clip's length.
        let input = AudioInput::Held(s);
        let ms = vector_media::audio_engine::probe_duration(&input).unwrap();
        assert!((2400..=2600).contains(&ms), "{ms}");
    }

    #[test]
    fn at_its_own_speed_it_is_the_clip_as_long_as_it_was() {
        let s = open(clip(), 1., 5000).unwrap();
        let ms = vector_media::audio_engine::probe_duration(&AudioInput::Held(s.clone())).unwrap();
        assert!((4900..=5100).contains(&ms), "{ms}");
        let mut all = Vec::new();
        vector_core::media::reader(s).read_to_end(&mut all).unwrap();
        assert!(all.iter().any(|&b| b != 0), "silent");
    }
}
