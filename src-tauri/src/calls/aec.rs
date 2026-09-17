//! Echo cancellation for calls, behind one trait so the canceller can change
//! (WebRTC AEC3, a platform voice-processing unit) without the engine noticing.
//!
//! The default is SpeexDSP's MDF canceller plus its preprocessor for residual echo
//! and noise, compiled from the vendored C sources in `csrc/speexdsp`.

use super::settings::AudioSettings;
use std::ffi::c_void;

/// Removes what the speaker played from what the microphone heard.
/// `near`, `far` and `out` are one engine frame each, at the engine rate.
pub trait EchoCanceller: Send {
    fn process(&mut self, near: &[i16], far: &[i16], out: &mut [i16]);
    /// Applies the user's switches; called every frame, so it must be cheap when nothing changed.
    fn configure(&mut self, _s: AudioSettings) {}
}

#[repr(C)]
struct SpeexEchoState {
    _private: [u8; 0],
}

#[repr(C)]
struct SpeexPreprocessState {
    _private: [u8; 0],
}

const SPEEX_ECHO_SET_SAMPLING_RATE: i32 = 24;
const SPEEX_PREPROCESS_SET_DENOISE: i32 = 0;
const SPEEX_PREPROCESS_SET_AGC: i32 = 2;
/// Float: the amplitude the gain control steers speech towards, out of 32768.
const SPEEX_PREPROCESS_SET_AGC_LEVEL: i32 = 6;
/// dB per second the gain may rise and fall.
const SPEEX_PREPROCESS_SET_AGC_INCREMENT: i32 = 26;
const SPEEX_PREPROCESS_SET_AGC_DECREMENT: i32 = 28;
/// dB: the most a quiet microphone is lifted.
const SPEEX_PREPROCESS_SET_AGC_MAX_GAIN: i32 = 30;
const SPEEX_PREPROCESS_SET_NOISE_SUPPRESS: i32 = 18;
const SPEEX_PREPROCESS_SET_ECHO_SUPPRESS: i32 = 20;
const SPEEX_PREPROCESS_SET_ECHO_SUPPRESS_ACTIVE: i32 = 22;
const SPEEX_PREPROCESS_SET_ECHO_STATE: i32 = 24;

extern "C" {
    fn speex_echo_state_init(frame_size: i32, filter_length: i32) -> *mut SpeexEchoState;
    fn speex_echo_state_destroy(st: *mut SpeexEchoState);
    fn speex_echo_cancellation(st: *mut SpeexEchoState, rec: *const i16, play: *const i16, out: *mut i16);
    fn speex_echo_ctl(st: *mut SpeexEchoState, request: i32, ptr: *mut c_void) -> i32;
    fn speex_preprocess_state_init(frame_size: i32, sampling_rate: i32) -> *mut SpeexPreprocessState;
    fn speex_preprocess_state_destroy(st: *mut SpeexPreprocessState);
    fn speex_preprocess_run(st: *mut SpeexPreprocessState, x: *mut i16) -> i32;
    fn speex_preprocess_ctl(st: *mut SpeexPreprocessState, request: i32, ptr: *mut c_void) -> i32;
}

pub struct SpeexAec {
    echo: *mut SpeexEchoState,
    pre: *mut SpeexPreprocessState,
    frame: usize,
    applied: AudioSettings,
}

// The states are only ever touched from the capture thread that owns the canceller.
unsafe impl Send for SpeexAec {}

impl SpeexAec {
    /// `tail_ms` is how far behind the far-end reference the echo may arrive:
    /// output buffering plus input buffering plus the room.
    pub fn new(rate: u32, frame: usize, tail_ms: u32) -> Result<Self, String> {
        let filter_length = (rate * tail_ms / 1000) as i32;
        unsafe {
            let echo = speex_echo_state_init(frame as i32, filter_length);
            if echo.is_null() {
                return Err("speex_echo_state_init failed".into());
            }
            let mut rate_i = rate as i32;
            speex_echo_ctl(echo, SPEEX_ECHO_SET_SAMPLING_RATE, &mut rate_i as *mut i32 as *mut c_void);

            let pre = speex_preprocess_state_init(frame as i32, rate as i32);
            if pre.is_null() {
                speex_echo_state_destroy(echo);
                return Err("speex_preprocess_state_init failed".into());
            }
            let mut on: i32 = 1;
            speex_preprocess_ctl(pre, SPEEX_PREPROCESS_SET_DENOISE, &mut on as *mut i32 as *mut c_void);
            // Automatic gain: a laptop microphone at arm's length and a headset an inch
            // away should reach the far end at the same level, without anyone shouting.
            speex_preprocess_ctl(pre, SPEEX_PREPROCESS_SET_AGC, &mut on as *mut i32 as *mut c_void);
            let mut level: f32 = 20_000.0;
            speex_preprocess_ctl(pre, SPEEX_PREPROCESS_SET_AGC_LEVEL, &mut level as *mut f32 as *mut c_void);
            let mut max_gain_db: i32 = 40;
            speex_preprocess_ctl(pre, SPEEX_PREPROCESS_SET_AGC_MAX_GAIN, &mut max_gain_db as *mut i32 as *mut c_void);
            let mut up_db_s: i32 = 24;
            speex_preprocess_ctl(pre, SPEEX_PREPROCESS_SET_AGC_INCREMENT, &mut up_db_s as *mut i32 as *mut c_void);
            let mut down_db_s: i32 = -60;
            speex_preprocess_ctl(pre, SPEEX_PREPROCESS_SET_AGC_DECREMENT, &mut down_db_s as *mut i32 as *mut c_void);
            let mut noise_db: i32 = -25;
            speex_preprocess_ctl(pre, SPEEX_PREPROCESS_SET_NOISE_SUPPRESS, &mut noise_db as *mut i32 as *mut c_void);
            speex_preprocess_ctl(pre, SPEEX_PREPROCESS_SET_ECHO_STATE, echo as *mut c_void);
            let mut echo_db: i32 = -45;
            speex_preprocess_ctl(pre, SPEEX_PREPROCESS_SET_ECHO_SUPPRESS, &mut echo_db as *mut i32 as *mut c_void);
            let mut echo_active_db: i32 = -20;
            speex_preprocess_ctl(pre, SPEEX_PREPROCESS_SET_ECHO_SUPPRESS_ACTIVE, &mut echo_active_db as *mut i32 as *mut c_void);

            Ok(Self { echo, pre, frame, applied: AudioSettings::default() })
        }
    }
}

impl EchoCanceller for SpeexAec {
    fn process(&mut self, near: &[i16], far: &[i16], out: &mut [i16]) {
        debug_assert!(near.len() == self.frame && far.len() == self.frame && out.len() == self.frame);
        unsafe {
            if self.applied.echo_cancel {
                speex_echo_cancellation(self.echo, near.as_ptr(), far.as_ptr(), out.as_mut_ptr());
            } else {
                out.copy_from_slice(near);
            }
            speex_preprocess_run(self.pre, out.as_mut_ptr());
        }
    }

    fn configure(&mut self, s: AudioSettings) {
        if s == self.applied {
            return;
        }
        unsafe {
            if s.auto_gain != self.applied.auto_gain {
                let mut on: i32 = s.auto_gain as i32;
                speex_preprocess_ctl(self.pre, SPEEX_PREPROCESS_SET_AGC, &mut on as *mut i32 as *mut c_void);
            }
            if s.noise_suppress != self.applied.noise_suppress {
                let mut on: i32 = s.noise_suppress as i32;
                speex_preprocess_ctl(self.pre, SPEEX_PREPROCESS_SET_DENOISE, &mut on as *mut i32 as *mut c_void);
            }
            if s.echo_cancel != self.applied.echo_cancel {
                // Detached, the preprocessor stops suppressing residual echo too.
                let state = if s.echo_cancel { self.echo as *mut c_void } else { std::ptr::null_mut() };
                speex_preprocess_ctl(self.pre, SPEEX_PREPROCESS_SET_ECHO_STATE, state);
            }
        }
        self.applied = s;
    }
}

impl Drop for SpeexAec {
    fn drop(&mut self) {
        unsafe {
            speex_preprocess_state_destroy(self.pre);
            speex_echo_state_destroy(self.echo);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calls::{AEC_TAIL_MS, ENGINE_RATE, FRAME};

    #[test]
    fn a_pure_echo_is_attenuated() {
        let frame = FRAME;
        let mut aec = SpeexAec::new(ENGINE_RATE, frame, AEC_TAIL_MS).unwrap();
        let mut phase = 0f32;
        let mut residual = 0f64;
        let mut input = 0f64;
        // A tone the speaker plays and the mic hears at half level, 40 ms late.
        let mut history: Vec<i16> = vec![0; frame * 2];
        for _ in 0..300 {
            let far: Vec<i16> = (0..frame)
                .map(|_| {
                    phase += 2.0 * std::f32::consts::PI * 440.0 / ENGINE_RATE as f32;
                    (phase.sin() * 12_000.0) as i16
                })
                .collect();
            history.extend_from_slice(&far);
            let near: Vec<i16> = history[..frame].iter().map(|s| s / 2).collect();
            history.drain(..frame);
            let mut out = vec![0i16; frame];
            aec.process(&near, &far, &mut out);
            input += near.iter().map(|s| (*s as f64).powi(2)).sum::<f64>();
            residual += out.iter().map(|s| (*s as f64).powi(2)).sum::<f64>();
        }
        assert!(residual < input / 10.0, "residual {residual} vs input {input}");
    }

    #[test]
    fn a_quiet_voice_is_lifted() {
        let frame = FRAME;
        let mut aec = SpeexAec::new(ENGINE_RATE, frame, AEC_TAIL_MS).unwrap();
        let far = vec![0i16; frame];
        let mut phase = 0f32;
        let rms = |s: &[i16]| (s.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / s.len() as f64).sqrt();
        let mut first = 0f64;
        let mut last = 0f64;
        // Whispered "syllables": a harmonic-rich buzz in 300 ms bursts with 200 ms
        // gaps, for ten seconds. A steady tone would read as noise, not speech.
        for i in 0..500 {
            let on = (i % 25) < 15;
            let near: Vec<i16> = (0..frame)
                .map(|_| {
                    phase += 2.0 * std::f32::consts::PI * 140.0 / ENGINE_RATE as f32;
                    let v: f32 = (1..=8).map(|h| (phase * h as f32).sin() / h as f32).sum();
                    if on { (v * 500.0) as i16 } else { 0 }
                })
                .collect();
            let mut out = vec![0i16; frame];
            aec.process(&near, &far, &mut out);
            if on {
                if i < 15 {
                    first = first.max(rms(&out));
                }
                last = rms(&out);
            }
        }
        assert!(last > first * 3.0, "gain never came up: {first} -> {last}");
        assert!(last > 2_000.0, "still quiet: {last}");
    }
}
