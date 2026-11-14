//! Playback speed that keeps the pitch, as a page's `<video>` does: WSOLA. The track is cut into
//! overlapping windows taken `speed` times faster than they are laid down, each one shifted a
//! little to where it best continues the last, and cross-faded.

/// Window length and the most a window may move to line up.
const WINDOW_MS: u32 = 30;
const SEEK_MS: u32 = 8;

/// Mono `samples` at `rate`, played at `speed` with the pitch kept.
pub fn stretch(samples: &[f32], rate: u32, speed: f32) -> Vec<f32> {
    if (speed - 1.).abs() < 1e-3 || samples.is_empty() {
        return samples.to_vec();
    }
    let n = ((rate * WINDOW_MS / 1000) as usize).max(64) & !1;
    let hop = n / 2;
    let seek = (rate * SEEK_MS / 1000) as usize;
    let step_in = hop as f64 * speed as f64;
    let window: Vec<f32> = (0..n).map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / n as f32).cos()).collect();
    let frames = ((samples.len() as f64 / step_in) as usize).max(1);
    // Lining windows up is most of the work: search a quarter-rate copy, then refine.
    let coarse: Vec<f32> = samples.chunks(DECIMATE).map(|c| c.iter().sum::<f32>()).collect();
    let mut out = vec![0f32; frames * hop + n];
    let mut weight = vec![0f32; out.len()];
    // Where the last window was taken from: the next continues from there plus a hop.
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
        let at = k * hop;
        for i in 0..n {
            let s = samples.get(from + i).copied().unwrap_or(0.);
            out[at + i] += s * window[i];
            weight[at + i] += window[i];
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

    #[test]
    fn normal_speed_is_untouched() {
        let src = tone(300., 44_100, 0.5);
        assert_eq!(stretch(&src, 44_100, 1.), src);
    }
}
