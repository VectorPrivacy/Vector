//! NeuQuant colour quantiser (Dekker 1994), bit-identical to `color_quant` 1.1, which the gif
//! crate uses. The network is laid out per channel so the distance pass vectorises, and the
//! per-pixel palette search is memoised.

const NET: usize = 256;
const LANES: usize = 8;
const RADIUS_DEC: i32 = 30;
const ALPHA_BIASSHIFT: i32 = 10;
const INIT_ALPHA: i32 = 1 << ALPHA_BIASSHIFT;
const GAMMA: f64 = 1024.0;
const BETA: f64 = 1.0 / GAMMA;
const BETAGAMMA: f64 = BETA * GAMMA;
const PRIMES: [usize; 4] = [499, 491, 478, 503];

pub struct NeuQuant {
    b: [f64; NET],
    g: [f64; NET],
    r: [f64; NET],
    a: [f64; NET],
    bias: [f64; NET],
    freq: [f64; NET],
    /// RGBA, sorted on green after training.
    colormap: [[i32; 4]; NET],
    netindex: [usize; 256],
}

impl NeuQuant {
    /// Train a 256-colour network on RGBA `pixels`; `samplefac` 1 (best) to 30 (fastest).
    pub fn new(samplefac: i32, pixels: &[u8]) -> Self {
        let mut nq = NeuQuant {
            b: [0.0; NET],
            g: [0.0; NET],
            r: [0.0; NET],
            a: [0.0; NET],
            bias: [0.0; NET],
            freq: [(NET as f64).recip(); NET],
            colormap: [[0, 0, 0, 255]; NET],
            netindex: [0; 256],
        };
        for i in 0..NET {
            let tmp = (i as f64) * 256.0 / (NET as f64);
            nq.b[i] = tmp;
            nq.g[i] = tmp;
            nq.r[i] = tmp;
            nq.a[i] = if i < 16 { i as f64 * 16.0 } else { 255.0 };
        }
        nq.learn(samplefac, pixels);
        for i in 0..NET {
            let c = |v: f64| (v.round() as i32).clamp(0, 255);
            nq.colormap[i] = [c(nq.r[i]), c(nq.g[i]), c(nq.b[i]), c(nq.a[i])];
        }
        nq.build_netindex();
        nq
    }

    /// Lloyd passes over every pixel: each entry moves to the mean of the pixels it serves.
    /// Training samples a fraction of the image, which can leave a small vivid cluster with
    /// no entry of its own; a pass pulls the nearest entry onto it.
    pub fn refine(&mut self, pixels: &[u8], passes: usize) {
        for _ in 0..passes {
            let mut sum = [[0u64; 4]; NET];
            let mut n = [0u64; NET];
            {
                let mut memo = IndexCache::new(self);
                for p in pixels.chunks_exact(4) {
                    let i = usize::from(memo.index_of([p[0], p[1], p[2], p[3]]));
                    for c in 0..4 {
                        sum[i][c] += u64::from(p[c]);
                    }
                    n[i] += 1;
                }
            }
            let mut moved = false;
            for ((entry, s), &n) in self.colormap.iter_mut().zip(&sum).zip(&n) {
                if n == 0 {
                    continue;
                }
                let mean: [i32; 4] = std::array::from_fn(|c| ((s[c] + n / 2) / n) as i32);
                moved |= mean != *entry;
                *entry = mean;
            }
            if !moved {
                break;
            }
            self.build_netindex();
        }
    }

    pub fn color_map_rgb(&self) -> Vec<u8> {
        self.colormap.iter().flat_map(|c| [c[0] as u8, c[1] as u8, c[2] as u8]).collect()
    }

    /// Nearest palette entry to an RGBA pixel.
    pub fn index_of(&self, p: [u8; 4]) -> u8 {
        self.search(p[2], p[1], p[0], p[3]) as u8
    }

    /// Finds the closest neuron (updating its frequency) and returns the best biased one.
    /// Distances are computed a block at a time; a block where no lane beats the running
    /// bests is skipped whole, which is exact because the bests cannot change inside it.
    #[inline(always)]
    fn contest(&mut self, b: f64, g: f64, r: f64, a: f64) -> usize {
        let (mut bestd, mut bestbiasd) = (f64::MAX, f64::MAX);
        let (mut bestpos, mut bestbiaspos) = (0usize, 0usize);
        let mut part = [0.0f64; LANES];
        let mut full = [0.0f64; LANES];
        for base in (0..NET).step_by(LANES) {
            let mut hit = false;
            for k in 0..LANES {
                let i = base + k;
                let p = (self.b[i] - b).abs() + (self.r[i] - r).abs();
                part[k] = p;
                full[k] = p + (self.g[i] - g).abs() + (self.a[i] - a).abs();
                hit |= (p < bestd) | (p < bestbiasd + self.bias[i]);
            }
            if !hit {
                continue;
            }
            for k in 0..LANES {
                let i = base + k;
                if part[k] < bestd || part[k] < bestbiasd + self.bias[i] {
                    let dist = full[k];
                    if dist < bestd {
                        bestd = dist;
                        bestpos = i;
                    }
                    let biasdist = dist - self.bias[i];
                    if biasdist < bestbiasd {
                        bestbiasd = biasdist;
                        bestbiaspos = i;
                    }
                }
            }
        }
        for (f, bi) in self.freq.iter_mut().zip(self.bias.iter_mut()) {
            *f -= BETA * *f;
            *bi += BETAGAMMA * *f;
        }
        self.freq[bestpos] += BETA;
        self.bias[bestpos] -= BETAGAMMA;
        bestbiaspos
    }

    #[inline(always)]
    fn pull(&mut self, i: usize, alpha: f64, (b, g, r, a): (f64, f64, f64, f64)) {
        self.b[i] -= alpha * (self.b[i] - b);
        self.g[i] -= alpha * (self.g[i] - g);
        self.r[i] -= alpha * (self.r[i] - r);
        self.a[i] -= alpha * (self.a[i] - a);
    }

    /// Pull the neighbours of neuron `i` towards the sample, `w[q]` for those `q + 1` away.
    /// Each neuron gets one independent update, so the two sides run as contiguous slices.
    #[inline(always)]
    fn alter_neighbour(&mut self, w: &[f64], rad: i32, i: usize, px: (f64, f64, f64, f64)) {
        let lo = (i as i32 - rad).max(0) as usize;
        let hi = (i as i32 + rad).min(NET as i32) as usize;
        let up = i + 1..hi.max(i + 1);
        let down = (lo + 1).min(i)..i;
        for (chan, v) in [(&mut self.b, px.0), (&mut self.g, px.1), (&mut self.r, px.2), (&mut self.a, px.3)] {
            for (x, &a) in chan[up.clone()].iter_mut().zip(w) {
                *x -= a * (*x - v);
            }
            for (x, &a) in chan[down.clone()].iter_mut().rev().zip(w) {
                *x -= a * (*x - v);
            }
        }
    }

    fn learn(&mut self, samplefac: i32, pixels: &[u8]) {
        #[cfg(target_arch = "x86_64")]
        if is_x86_feature_detected!("avx2") {
            // SAFETY: only reached where the CPU reports AVX2.
            return unsafe { self.learn_avx2(samplefac, pixels) };
        }
        self.learn_body(samplefac, pixels)
    }

    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "avx2")]
    unsafe fn learn_avx2(&mut self, samplefac: i32, pixels: &[u8]) {
        self.learn_body(samplefac, pixels)
    }

    #[inline(always)]
    fn learn_body(&mut self, samplefac: i32, pixels: &[u8]) {
        let initrad = NET as i32 / 8;
        let radiusbiasshift = 6;
        let mut bias_radius = initrad * (1 << radiusbiasshift);
        let alphadec = 30 + ((samplefac - 1) / 3);
        let lengthcount = pixels.len() / 4;
        let samplepixels = lengthcount / samplefac as usize;
        let n_cycles = (NET >> 1).max(100);
        let delta = (samplepixels / n_cycles).max(1);
        let mut alpha = INIT_ALPHA;
        let mut rad = bias_radius >> radiusbiasshift;
        if rad <= 1 {
            rad = 0;
        }
        // Neighbour weights change only with alpha and radius, once per `delta` samples.
        let weights = |alpha: i32, rad: i32| -> Vec<f64> {
            let alpha = f64::from(alpha) / f64::from(INIT_ALPHA);
            let rad_sq = rad as f64 * rad as f64;
            (0..rad).map(|q| (alpha * (rad_sq - q as f64 * q as f64)) / rad_sq).collect()
        };
        let mut w = weights(alpha, rad);
        let step = *PRIMES.iter().find(|&&p| !lengthcount.is_multiple_of(p)).unwrap_or(&PRIMES[3]);
        let mut pos = 0;
        for i in 1..=samplepixels {
            let p = &pixels[4 * pos..][..4];
            let px = (f64::from(p[2]), f64::from(p[1]), f64::from(p[0]), f64::from(p[3]));
            let j = self.contest(px.0, px.1, px.2, px.3);
            self.pull(j, f64::from(alpha) / f64::from(INIT_ALPHA), px);
            if rad > 0 {
                self.alter_neighbour(&w, rad, j, px);
            }
            pos += step;
            while pos >= lengthcount {
                pos -= lengthcount;
            }
            if i % delta == 0 {
                alpha -= alpha / alphadec;
                bias_radius -= bias_radius / RADIUS_DEC;
                rad = bias_radius >> radiusbiasshift;
                if rad <= 1 {
                    rad = 0;
                }
                w = weights(alpha, rad);
            }
        }
    }

    /// Selection sort on green (stable against the reference's swap order) and the
    /// green-keyed start index for searches.
    fn build_netindex(&mut self) {
        let (mut previouscol, mut startpos) = (0usize, 0usize);
        for i in 0..NET {
            let mut smallpos = i;
            let mut smallval = self.colormap[i][1] as usize;
            for j in i + 1..NET {
                if (self.colormap[j][1] as usize) < smallval {
                    smallpos = j;
                    smallval = self.colormap[j][1] as usize;
                }
            }
            self.colormap.swap(i, smallpos);
            if smallval != previouscol {
                self.netindex[previouscol] = (startpos + i) >> 1;
                for j in previouscol + 1..smallval {
                    self.netindex[j] = i;
                }
                previouscol = smallval;
                startpos = i;
            }
        }
        self.netindex[previouscol] = (startpos + NET - 1) >> 1;
        for j in previouscol + 1..256 {
            self.netindex[j] = NET - 1;
        }
    }

    fn search(&self, b: u8, g: u8, r: u8, a: u8) -> usize {
        let (b, g, r, a) = (i32::from(b), i32::from(g), i32::from(r), i32::from(a));
        let dist = |c: &[i32; 4], bestd: i32| -> Option<i32> {
            let e = c[1] - g;
            let mut d = e * e;
            if d >= bestd {
                return None;
            }
            for (v, want) in [(c[2], b), (c[0], r), (c[3], a)] {
                if d >= bestd {
                    return Some(i32::MAX);
                }
                d += (v - want) * (v - want);
            }
            Some(d)
        };
        let (mut bestd, mut best) = (1 << 30, 0);
        let mut i = self.netindex[g as usize];
        let mut j = i.saturating_sub(1);
        while i < NET || j > 0 {
            if i < NET {
                match dist(&self.colormap[i], bestd) {
                    None => break,
                    Some(d) => {
                        if d < bestd {
                            bestd = d;
                            best = i;
                        }
                        i += 1;
                    }
                }
            }
            if j > 0 {
                match dist(&self.colormap[j], bestd) {
                    None => break,
                    Some(d) => {
                        if d < bestd {
                            bestd = d;
                            best = j;
                        }
                        j -= 1;
                    }
                }
            }
        }
        best
    }
}

/// Exact memo for `index_of`: frames repeat colours heavily, and a miss costs a network walk.
pub struct IndexCache<'a> {
    nq: &'a NeuQuant,
    slots: Vec<u64>,
}

impl<'a> IndexCache<'a> {
    const BITS: u32 = 12;

    pub fn new(nq: &'a NeuQuant) -> Self {
        IndexCache { nq, slots: vec![u64::MAX; 1 << Self::BITS] }
    }

    #[inline]
    pub fn index_of(&mut self, p: [u8; 4]) -> u8 {
        let key = u32::from_le_bytes(p);
        let slot = (key.wrapping_mul(0x9E37_79B1) >> (32 - Self::BITS)) as usize;
        let e = self.slots[slot];
        if e >> 8 == u64::from(key) {
            return e as u8;
        }
        let idx = self.nq.index_of(p);
        self.slots[slot] = u64::from(key) << 8 | u64::from(idx);
        idx
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixels(n: usize, seed: u32, spread: u32) -> Vec<u8> {
        let mut s = seed;
        (0..n * 4)
            .map(|i| {
                s ^= s << 13;
                s ^= s >> 17;
                s ^= s << 5;
                if i % 4 == 3 {
                    if s.is_multiple_of(17) { 0 } else { 255 }
                } else {
                    ((i / 4) as u32 / 97 * 13 + s % spread) as u8
                }
            })
            .collect()
    }

    #[test]
    fn matches_color_quant_bit_for_bit() {
        for (n, seed, spread, fac) in
            [(64 * 64, 1, 256, 10), (320 * 200, 7, 40, 10), (97, 3, 256, 1), (1, 5, 256, 10), (5000, 9, 7, 30)]
        {
            let px = pixels(n, seed, spread);
            let theirs = color_quant::NeuQuant::new(fac, 256, &px);
            let ours = NeuQuant::new(fac, &px);
            assert_eq!(ours.color_map_rgb(), theirs.color_map_rgb(), "palette, {n} px");
            let mut memo = IndexCache::new(&ours);
            let probe = pixels(20_000, seed + 1, 256);
            for p in px.chunks_exact(4).chain(probe.chunks_exact(4)) {
                let p4 = [p[0], p[1], p[2], p[3]];
                let want = theirs.index_of(p) as u8;
                assert_eq!(ours.index_of(p4), want);
                assert_eq!(memo.index_of(p4), want);
            }
        }
    }
}

#[cfg(test)]
mod bench {
    use super::*;
    #[test]
    #[ignore]
    fn bench_neuquant() {
        let path = std::env::var("NQ_IMAGE").unwrap();
        let img = image::open(path).unwrap().to_rgba8();
        let px = img.as_raw();
        for _ in 0..2 {
            let t = std::time::Instant::now();
            let theirs = color_quant::NeuQuant::new(10, 256, px);
            let tl = t.elapsed();
            let t = std::time::Instant::now();
            let v: Vec<u8> = px.chunks_exact(4).map(|p| theirs.index_of(p) as u8).collect();
            let ti = t.elapsed();
            let t = std::time::Instant::now();
            let ours = NeuQuant::new(10, px);
            let ol = t.elapsed();
            let t = std::time::Instant::now();
            let mut m = IndexCache::new(&ours);
            let w: Vec<u8> = px.chunks_exact(4).map(|p| m.index_of([p[0], p[1], p[2], p[3]])).collect();
            let oi = t.elapsed();
            assert_eq!(v, w);
            println!("color_quant learn {tl:?} index {ti:?} | ours learn {ol:?} index {oi:?}");
        }
    }
}
