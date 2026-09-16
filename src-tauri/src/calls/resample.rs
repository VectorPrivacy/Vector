//! A streaming linear resampler: carries its phase and last sample across blocks
//! so a live stream has no seam where one block ends and the next begins.

pub struct Resampler {
    /// Input samples per output sample.
    step: f64,
    /// Read position in the virtual input `[prev] ++ block`, in input samples.
    pos: f64,
    prev: f32,
}

impl Resampler {
    pub fn new(from_rate: u32, to_rate: u32) -> Self {
        Self { step: from_rate as f64 / to_rate as f64, pos: 0.0, prev: 0.0 }
    }

    /// Appends the resampled block to `out`.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        if input.is_empty() {
            return;
        }
        let prev = self.prev;
        let at = |i: usize| if i == 0 { prev } else { input[i - 1] };
        // Position p reads virtual samples floor(p) and floor(p)+1; the last
        // readable pair is (len-1, len).
        while self.pos < input.len() as f64 {
            let i = self.pos as usize;
            let frac = (self.pos - i as f64) as f32;
            let a = at(i);
            let b = at(i + 1);
            out.push(a + (b - a) * frac);
            self.pos += self.step;
        }
        self.prev = input[input.len() - 1];
        self.pos -= input.len() as f64;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ramp_stays_a_ramp_across_block_boundaries() {
        let mut rs = Resampler::new(48_000, 16_000);
        let ramp: Vec<f32> = (0..96).map(|i| i as f32).collect();
        let mut out = Vec::new();
        for block in ramp.chunks(7) {
            rs.process(block, &mut out);
        }
        assert!(out.len() >= 31 && out.len() <= 32, "{}", out.len());
        for pair in out.windows(2).skip(1) {
            assert!((pair[1] - pair[0] - 3.0).abs() < 1e-3, "{:?}", pair);
        }
    }

    #[test]
    fn upsampling_produces_the_expected_count() {
        let mut rs = Resampler::new(16_000, 48_000);
        let mut out = Vec::new();
        rs.process(&vec![0.5; 160], &mut out);
        rs.process(&vec![0.5; 160], &mut out);
        assert!((out.len() as i64 - 960).abs() <= 3, "{}", out.len());
        assert!(out.iter().skip(3).all(|s| (s - 0.5).abs() < 1e-6));
    }
}
