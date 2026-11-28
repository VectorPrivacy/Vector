//! Playback speed that keeps the pitch, as a page's `<video>` does: WSOLA. The track is cut into
//! overlapping windows taken `speed` times faster than they are laid down, each one shifted a
//! little to where it best continues the last, and cross-faded.

/// Window length and the most a window may move to line up.
const WINDOW_MS: u32 = 30;
const SEEK_MS: u32 = 8;

/// Mono `samples` at `rate`, played at `speed` with the pitch kept.
pub fn stretch(samples: &[f32], rate: u32, speed: f32) -> Vec<f32> {
    let mut out = Vec::new();
    let mut s = Stretcher::new(rate, speed);
    s.push(samples, &mut out);
    s.finish(&mut out);
    out
}

/// The same stretch over a track that arrives in pieces: each window is laid down once the
/// sound it may line up with is in, so the result never depends on how the track was cut.
pub struct Stretcher {
    unity: bool,
    speed: f32,
    n: usize,
    hop: usize,
    seek: usize,
    step_in: f64,
    window: Vec<f32>,
    /// The track from `base` on (a multiple of `DECIMATE`), and its quarter-rate sums.
    input: Vec<f32>,
    coarse: Vec<f32>,
    base: usize,
    /// What is laid down from `out_base` on, before it is divided by the window weights.
    out: Vec<f32>,
    weight: Vec<f32>,
    out_base: usize,
    k: usize,
    /// Where the last window was taken from: the next continues from there plus a hop.
    last: Option<usize>,
}

impl Stretcher {
    pub fn new(rate: u32, speed: f32) -> Self {
        let n = ((rate * WINDOW_MS / 1000) as usize).max(64) & !1;
        let hop = n / 2;
        Self {
            unity: (speed - 1.).abs() < 1e-3,
            speed,
            n,
            hop,
            seek: (rate * SEEK_MS / 1000) as usize,
            step_in: hop as f64 * speed as f64,
            window: (0..n).map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / n as f32).cos()).collect(),
            input: Vec::new(),
            coarse: Vec::new(),
            base: 0,
            out: Vec::new(),
            weight: Vec::new(),
            out_base: 0,
            k: 0,
            last: None,
        }
    }

    /// More of the track: what is final of the stretched sound goes onto `out`.
    pub fn push(&mut self, samples: &[f32], out: &mut Vec<f32>) {
        if self.unity {
            out.extend_from_slice(samples);
            return;
        }
        self.input.extend_from_slice(samples);
        for c in self.coarse.len()..self.input.len() / DECIMATE {
            self.coarse.push(self.input[c * DECIMATE..(c + 1) * DECIMATE].iter().sum::<f32>());
        }
        self.run(false, out);
    }

    /// The track has ended: the rest goes onto `out`.
    pub fn finish(&mut self, out: &mut Vec<f32>) {
        let total = self.base + self.input.len();
        if self.unity || total == 0 {
            return;
        }
        let whole = self.coarse.len() * DECIMATE;
        if whole < self.input.len() {
            self.coarse.push(self.input[whole..].iter().sum::<f32>());
        }
        self.run(true, out);
        let frames = ((total as f64 / self.step_in) as usize).max(1);
        let end = ((total as f64 / self.speed as f64) as usize).min(frames * self.hop + self.n);
        self.emit(end, out);
    }

    fn run(&mut self, ended: bool, out: &mut Vec<f32>) {
        let total = self.base + self.input.len();
        let frames = ((total as f64 / self.step_in) as usize).max(1);
        loop {
            let nominal = (self.k as f64 * self.step_in) as usize;
            if nominal >= total || (ended && self.k >= frames) {
                return;
            }
            if !ended {
                // In full: every sound this window may line up with, and the next window's start.
                let natural = self.last.map_or(0, |p| p + self.hop);
                let need = ((nominal + self.seek).max(natural) + self.n + DECIMATE + 1).max(((self.k + 1) as f64 * self.step_in) as usize + 1);
                if total < need {
                    return;
                }
            }
            let b = self.base;
            let from = match self.last {
                None => nominal,
                Some(prev) => b + best_offset(&self.input, &self.coarse, prev + self.hop - b, nominal - b, self.seek, self.hop),
            };
            let at = self.k * self.hop - self.out_base;
            if self.out.len() < at + self.n {
                self.out.resize(at + self.n, 0.);
                self.weight.resize(at + self.n, 0.);
            }
            for i in 0..self.n {
                let s = self.input.get(from - b + i).copied().unwrap_or(0.);
                self.out[at + i] += s * self.window[i];
                self.weight[at + i] += self.window[i];
            }
            self.last = Some(from);
            self.k += 1;
            if !ended {
                self.emit(self.k * self.hop, out);
                self.trim();
            }
        }
    }

    /// Everything laid down before `end` is final: divided out and handed on.
    fn emit(&mut self, end: usize, out: &mut Vec<f32>) {
        let take = end.saturating_sub(self.out_base);
        let have = take.min(self.out.len());
        out.extend(self.out[..have].iter().zip(&self.weight[..have]).map(|(o, w)| if *w > 1e-3 { o / w } else { *o }));
        out.resize(out.len() + (take - have), 0.);
        self.out.drain(..have);
        self.weight.drain(..have);
        self.out_base += take;
    }

    /// Lets go of the track no later window can reach.
    fn trim(&mut self) {
        let nominal = (self.k as f64 * self.step_in) as usize;
        let natural = self.last.map_or(0, |p| p + self.hop);
        let keep = nominal.saturating_sub(self.seek).min(natural) / DECIMATE * DECIMATE;
        let cut = keep.saturating_sub(self.base);
        if cut >= 1 << 16 {
            self.input.drain(..cut);
            self.coarse.drain(..cut / DECIMATE);
            self.base += cut;
        }
    }
}

const DECIMATE: usize = 4;

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// The start near `nominal` whose opening `len` samples best match those at `natural`, the
/// sound that would have followed the last window: found on `coarse` (sums of `DECIMATE`
/// samples), then to the sample around it.
fn best_offset(samples: &[f32], coarse: &[f32], natural: usize, nominal: usize, seek: usize, len: usize) -> usize {
    let lo = nominal.saturating_sub(seek);
    let hi = (nominal + seek).min(samples.len().saturating_sub(len + 1));
    if lo >= hi || natural + len > samples.len() {
        return nominal.min(samples.len().saturating_sub(1));
    }
    let clen = len / DECIMATE;
    let (cnat, clo, chi) = (natural / DECIMATE, lo / DECIMATE, hi / DECIMATE);
    let mut at = nominal.clamp(lo, hi);
    if cnat + clen <= coarse.len() && chi + clen <= coarse.len() {
        let target = &coarse[cnat..cnat + clen];
        let mut best = f32::MIN;
        for c in clo..=chi {
            let score = dot(&coarse[c..c + clen], target);
            if score > best {
                best = score;
                at = (c * DECIMATE).clamp(lo, hi);
            }
        }
    }
    let target = &samples[natural..natural + len];
    let (rlo, rhi) = (at.saturating_sub(DECIMATE).max(lo), (at + DECIMATE).min(hi));
    let mut best = (f32::MIN, at);
    for r in rlo..=rhi {
        let score = dot(&samples[r..r + len], target);
        if score > best.0 {
            best = (score, r);
        }
    }
    best.1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(hz: f32, rate: u32, secs: f32) -> Vec<f32> {
        (0..(rate as f32 * secs) as usize).map(|i| (std::f32::consts::TAU * hz * i as f32 / rate as f32).sin()).collect()
    }

    /// Upward zero crossings per second: the pitch.
    fn pitch(s: &[f32], rate: u32) -> f32 {
        let ups = s.windows(2).filter(|w| w[0] < 0. && w[1] >= 0.).count();
        ups as f32 * rate as f32 / s.len() as f32
    }

    #[test]
    fn speed_changes_length_not_pitch() {
        let rate = 48_000;
        let src = tone(440., rate, 2.);
        for speed in [0.5, 0.75, 1.25, 1.5, 2.] {
            let out = stretch(&src, rate, speed);
            let want = src.len() as f32 / speed;
            assert!((out.len() as f32 - want).abs() / want < 0.02, "{speed}: {} vs {want}", out.len());
            let p = pitch(&out[rate as usize / 10..out.len() - rate as usize / 10], rate);
            assert!((p - 440.).abs() < 6., "{speed}: pitch {p}");
        }
    }

    /// The stretch done in one pass over the whole track.
    fn whole(samples: &[f32], rate: u32, speed: f32) -> Vec<f32> {
        let n = ((rate * WINDOW_MS / 1000) as usize).max(64) & !1;
        let hop = n / 2;
        let seek = (rate * SEEK_MS / 1000) as usize;
        let step_in = hop as f64 * speed as f64;
        let window: Vec<f32> = (0..n).map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / n as f32).cos()).collect();
        let frames = ((samples.len() as f64 / step_in) as usize).max(1);
        let coarse: Vec<f32> = samples.chunks(DECIMATE).map(|c| c.iter().sum::<f32>()).collect();
        let mut out = vec![0f32; frames * hop + n];
        let mut weight = vec![0f32; out.len()];
        let mut last: Option<usize> = None;
        for k in 0..frames {
            let nominal = (k as f64 * step_in) as usize;
            if nominal >= samples.len() {
                break;
            }
            let from = match last {
                None => nominal,
                Some(prev) => best_offset(samples, &coarse, prev + hop, nominal, seek, hop),
            };
            for i in 0..n {
                out[k * hop + i] += samples.get(from + i).copied().unwrap_or(0.) * window[i];
                weight[k * hop + i] += window[i];
            }
            last = Some(from);
        }
        for (o, w) in out.iter_mut().zip(&weight) {
            if *w > 1e-3 {
                *o /= w;
            }
        }
        out.truncate(((samples.len() as f64 / speed as f64) as usize).min(out.len()));
        out
    }

    #[test]
    fn a_track_in_pieces_stretches_exactly_as_one_pass() {
        let rate = 44_100;
        // A chirp with noise, so every window lines up somewhere different.
        let mut seed = 7u32;
        let src: Vec<f32> = (0..rate as usize * 7 + 123)
            .map(|i| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let t = i as f32 / rate as f32;
                (std::f32::consts::TAU * (200. + 150. * t) * t).sin() * 0.6 + (seed >> 9) as f32 / (1u32 << 23) as f32 * 0.1
            })
            .collect();
        for speed in [0.5, 0.75, 1.25, 1.5, 2.] {
            let want = whole(&src, rate, speed);
            for piece in [1usize, 997, 4096, 70_001] {
                let mut s = Stretcher::new(rate, speed);
                let mut got = Vec::new();
                for c in src.chunks(piece) {
                    s.push(c, &mut got);
                }
                s.finish(&mut got);
                assert_eq!(got.len(), want.len(), "{speed} in {piece}s");
                assert!(got.iter().zip(&want).all(|(a, b)| a.to_bits() == b.to_bits()), "{speed} in {piece}s");
            }
        }
    }

    #[test]
    fn normal_speed_is_untouched() {
        let src = tone(300., 44_100, 0.5);
        assert_eq!(stretch(&src, 44_100, 1.), src);
    }
}
