//! Echo cancellation for calls, behind one trait so the canceller can change
//! (WebRTC AEC3, a platform voice-processing unit) without the engine noticing.
//!
//! The default is SpeexDSP's MDF canceller plus its preprocessor for residual echo
//! and noise, compiled from the vendored C sources in `csrc/speexdsp`.

use std::ffi::c_void;

/// Removes what the speaker played from what the microphone heard.
/// `near`, `far` and `out` are one engine frame each, at the engine rate.
pub trait EchoCanceller: Send {
    fn process(&mut self, near: &[i16], far: &[i16], out: &mut [i16]);
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
            let mut noise_db: i32 = -25;
            speex_preprocess_ctl(pre, SPEEX_PREPROCESS_SET_NOISE_SUPPRESS, &mut noise_db as *mut i32 as *mut c_void);
            speex_preprocess_ctl(pre, SPEEX_PREPROCESS_SET_ECHO_STATE, echo as *mut c_void);
            let mut echo_db: i32 = -45;
            speex_preprocess_ctl(pre, SPEEX_PREPROCESS_SET_ECHO_SUPPRESS, &mut echo_db as *mut i32 as *mut c_void);
            let mut echo_active_db: i32 = -20;
            speex_preprocess_ctl(pre, SPEEX_PREPROCESS_SET_ECHO_SUPPRESS_ACTIVE, &mut echo_active_db as *mut i32 as *mut c_void);

            Ok(Self { echo, pre, frame })
        }
    }
}

impl EchoCanceller for SpeexAec {
    fn process(&mut self, near: &[i16], far: &[i16], out: &mut [i16]) {
        debug_assert!(near.len() == self.frame && far.len() == self.frame && out.len() == self.frame);
        unsafe {
            speex_echo_cancellation(self.echo, near.as_ptr(), far.as_ptr(), out.as_mut_ptr());
            speex_preprocess_run(self.pre, out.as_mut_ptr());
        }
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

    #[test]
    fn a_pure_echo_is_attenuated() {
        let frame = 320;
        let mut aec = SpeexAec::new(16_000, frame, 200).unwrap();
        let mut phase = 0f32;
        let mut residual = 0f64;
        let mut input = 0f64;
        // A tone the speaker plays and the mic hears at half level, 40 ms late.
        let mut history: Vec<i16> = vec![0; frame * 2];
        for _ in 0..300 {
            let far: Vec<i16> = (0..frame)
                .map(|_| {
                    phase += 2.0 * std::f32::consts::PI * 440.0 / 16_000.0;
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
}
