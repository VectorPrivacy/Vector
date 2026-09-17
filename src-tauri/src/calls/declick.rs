//! Removes the click a microphone makes when its cable is pulled or pushed in: a
//! sample-to-sample slam that speech never produces, followed by a short offset
//! tail. Runs a couple of milliseconds behind the input so the samples just
//! before the slam can go too.

/// Full-scale fraction of a sample-to-sample jump that speech never reaches.
const JUMP: i32 = 8000;
/// Anything this close to the rail is a slam, whatever came before it.
const RAIL: i32 = 30000;
/// The jump must also dwarf the recent loudness, so a shout is not a click.
const RATIO: f32 = 8.0;
const PRE_MS: u32 = 2;
const HOLD_MS: u32 = 20;
const FADE_MS: u32 = 4;
const ENV_MS: f32 = 100.0;

pub struct Declicker {
    delay: Vec<i16>,
    head: usize,
    prev: i32,
    env: f32,
    alpha: f32,
    /// One-pole high-pass state: the slam leaves an offset that decays for tens of
    /// milliseconds, and a microphone can carry one of its own.
    dc_x1: f32,
    dc_y1: f32,
    dc_r: f32,
    hold: usize,
    fade: usize,
    mute_left: usize,
    fade_left: usize,
    triggers: u32,
}

impl Declicker {
    pub fn new(rate: u32) -> Self {
        let pre = (rate * PRE_MS / 1000).max(1) as usize;
        Self {
            delay: vec![0; pre],
            head: 0,
            prev: 0,
            env: 300.0,
            alpha: 1.0 / (ENV_MS / 1000.0 * rate as f32),
            dc_x1: 0.0,
            dc_y1: 0.0,
            dc_r: 1.0 - 2.0 * std::f32::consts::PI * 20.0 / rate as f32,
            hold: (rate * HOLD_MS / 1000) as usize,
            fade: (rate * FADE_MS / 1000).max(1) as usize,
            mute_left: 0,
            fade_left: 0,
            triggers: 0,
        }
    }

    /// How many clicks have been cut so far.
    pub fn triggers(&self) -> u32 {
        self.triggers
    }

    /// Cleans the block in place. Output lags input by `PRE_MS`.
    pub fn process(&mut self, block: &mut [i16]) {
        for x in block.iter_mut() {
            // Block DC first, so an offset tail cannot outlive the mute.
            let xin = *x as f32;
            let y = xin - self.dc_x1 + self.dc_r * self.dc_y1;
            self.dc_x1 = xin;
            self.dc_y1 = y;
            *x = y.clamp(-32768.0, 32767.0) as i16;

            let now = *x as i32;
            let jump = (now - self.prev).abs();
            self.prev = now;
            // A slam on the rail, or a jump that dwarfs the recent loudness. Speech
            // that clips still rises sample by sample; a cable does not.
            let slam = now.abs() > RAIL && jump > JUMP;
            let spike = jump > JUMP && (now.abs() as f32) > RATIO * self.env;
            if self.mute_left == 0 && (slam || spike) {
                self.triggers += 1;
                self.mute_left = self.hold;
                self.delay.iter_mut().for_each(|d| *d = 0);
            }
            // The sample leaving the delay line is this position's output.
            let out = self.delay[self.head];
            let mut input = *x;
            if self.mute_left > 0 {
                input = 0;
                self.mute_left -= 1;
                if self.mute_left == 0 {
                    self.fade_left = self.fade;
                }
            } else if self.fade_left > 0 {
                input = ((*x as f32) * (1.0 - self.fade_left as f32 / self.fade as f32)) as i16;
                self.fade_left -= 1;
            } else {
                let f = now as f32;
                self.env = ((1.0 - self.alpha) * self.env * self.env + self.alpha * f * f).sqrt();
            }
            self.delay[self.head] = input;
            self.head = (self.head + 1) % self.delay.len();
            *x = out;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `DECLICK_WAV=/path/to/note.wav cargo test declick::real -- --ignored --nocapture`:
    /// prints where the clicks were cut in a real 16-bit recording.
    #[test]
    #[ignore]
    fn real_recording() {
        let Ok(path) = std::env::var("DECLICK_WAV") else { return };
        let mut reader = hound::WavReader::open(&path).unwrap();
        let spec = reader.spec();
        let ch = spec.channels as usize;
        let mut pcm: Vec<i16> = reader.samples::<i16>().map(|s| s.unwrap()).step_by(ch).collect();
        let before: Vec<i16> = pcm.clone();
        let mut d = Declicker::new(spec.sample_rate);
        let mut last = 0u32;
        let mut cuts = Vec::new();
        for (i, block) in pcm.chunks_mut(spec.sample_rate as usize / 100).enumerate() {
            d.process(block);
            if d.triggers() != last {
                last = d.triggers();
                cuts.push(i as f32 / 100.0);
            }
        }
        let peak_before = before.iter().map(|s| s.abs()).max().unwrap();
        let peak_after = pcm.iter().map(|s| s.abs()).max().unwrap();
        eprintln!("{path}: {} cut(s) at {:?} s; peak {peak_before} -> {peak_after}", d.triggers(), cuts);
    }

    fn voice(rate: u32, secs: f32) -> Vec<i16> {
        // A buzz in bursts, like the AGC test: rich in harmonics, moderate level.
        let mut phase = 0f32;
        (0..(rate as f32 * secs) as usize)
            .map(|i| {
                phase += 2.0 * std::f32::consts::PI * 140.0 / rate as f32;
                let on = (i / (rate as usize / 5)) % 2 == 0;
                let v: f32 = (1..=8).map(|h| (phase * h as f32).sin() / h as f32).sum();
                if on { (v * 6000.0) as i16 } else { 0 }
            })
            .collect()
    }

    #[test]
    fn a_cable_slam_is_cut_and_speech_is_not() {
        let rate = 16_000;
        let clean = voice(rate, 2.0);
        let mut hit = clean.clone();
        // The rip: 1.5 ms on the rail, then an offset that decays over 30 ms.
        let at = rate as usize;
        for k in 0..24 {
            hit[at + k] = -32766;
        }
        for k in 24..(24 + 480) {
            hit[at + k] = (20000.0 * (-(k as f32 - 24.0) / 160.0).exp()) as i16;
        }
        let mut d = Declicker::new(rate);
        d.process(&mut hit);
        let lag = (rate * PRE_MS / 1000) as usize;
        // The slam and its tail are gone.
        let region = &hit[at - 16 + lag..at + 24 + 320 + lag];
        assert!(region.iter().all(|s| s.abs() < 800), "residue {}", region.iter().map(|s| s.abs()).max().unwrap());
        assert_eq!(d.triggers(), 1);
        // Clean voice never triggers, and keeps its loudness through the high-pass.
        let mut pristine = clean.clone();
        let mut d2 = Declicker::new(rate);
        d2.process(&mut pristine);
        assert_eq!(d2.triggers(), 0);
        let rms = |v: &[i16]| (v.iter().map(|s| (*s as f64).powi(2)).sum::<f64>() / v.len() as f64).sqrt();
        let ratio = rms(&pristine[lag..]) / rms(&clean[..clean.len() - lag]);
        assert!((0.97..1.03).contains(&ratio), "loudness ratio {ratio}");
    }
}
