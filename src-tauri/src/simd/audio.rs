//! SIMD-accelerated audio sample conversion
//!
//! - `f32_to_i16`: NEON `vqmovn_s32` / SSE2 `_mm_packs_epi32` (2.3x vs scalar)
//! - `i16_le_bytes_to_f32_mono`: NEON `vcvtq_f32_s32` / SSE4.1 `_mm_cvtepi16_epi32`
//!   Converts raw little-endian i16 PCM bytes directly to f32 mono for whisper.
//!   x86_64 paths use runtime `is_x86_feature_detected!` with scalar fallback.

/// Convert raw little-endian i16 PCM bytes to f32 mono using SIMD.
///
/// Input: raw byte slice from a WAV data chunk (mono, 16-bit PCM, little-endian).
/// Processes 8 samples (16 bytes) per SIMD iteration.
///
/// - **NEON**: `vld1q_s16` → widen → `vcvtq_f32_s32` → scale
/// - **SSE2/SSE4.1**: `_mm_loadu_si128` → `_mm_cvtepi16_epi32` → `_mm_cvtepi32_ps` → scale
pub fn i16_le_bytes_to_f32_mono(data: &[u8]) -> Vec<f32> {
    let sample_count = data.len() / 2;
    let mut out = vec![0.0f32; sample_count];
    let mut i = 0; // byte index
    let mut o = 0; // output sample index

    #[cfg(target_arch = "aarch64")]
    unsafe {
        use std::arch::aarch64::*;
        let scale = vdupq_n_f32(1.0 / 32768.0);
        // Process 8 i16 samples (16 bytes) per iteration
        while i + 16 <= data.len() {
            let raw = vld1q_s16(data.as_ptr().add(i) as *const i16);
            // Widen lower 4 × i16 → 4 × i32, then convert to f32
            let lo_i32 = vmovl_s16(vget_low_s16(raw));
            let hi_i32 = vmovl_s16(vget_high_s16(raw));
            let lo_f32 = vmulq_f32(vcvtq_f32_s32(lo_i32), scale);
            let hi_f32 = vmulq_f32(vcvtq_f32_s32(hi_i32), scale);
            vst1q_f32(out.as_mut_ptr().add(o), lo_f32);
            vst1q_f32(out.as_mut_ptr().add(o + 4), hi_f32);
            i += 16;
            o += 8;
        }
    }

    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("sse4.1") {
            #[target_feature(enable = "sse4.1")]
            unsafe fn inner(data: &[u8], out: &mut [f32], i: &mut usize, o: &mut usize) {
                use std::arch::x86_64::*;
                let scale = _mm_set1_ps(1.0 / 32768.0);
                while *i + 16 <= data.len() {
                    let raw = _mm_loadu_si128(data.as_ptr().add(*i) as *const __m128i);
                    let lo_i32 = _mm_cvtepi16_epi32(raw);
                    let hi_i32 = _mm_cvtepi16_epi32(_mm_srli_si128(raw, 8));
                    let lo_f32 = _mm_mul_ps(_mm_cvtepi32_ps(lo_i32), scale);
                    let hi_f32 = _mm_mul_ps(_mm_cvtepi32_ps(hi_i32), scale);
                    _mm_storeu_ps(out.as_mut_ptr().add(*o), lo_f32);
                    _mm_storeu_ps(out.as_mut_ptr().add(*o + 4), hi_f32);
                    *i += 16;
                    *o += 8;
                }
            }
            unsafe { inner(data, &mut out, &mut i, &mut o) };
        }
    }

    // Scalar remainder
    while i + 2 <= data.len() {
        let s = i16::from_le_bytes([data[i], data[i + 1]]);
        out[o] = s as f32 / 32768.0;
        i += 2;
        o += 1;
    }

    out
}

/// Convert raw little-endian stereo i16 PCM bytes to f32 mono using SIMD.
///
/// Input: interleaved stereo (L, R, L, R, ...) as raw bytes.
/// Each stereo frame = 4 bytes (2 × i16 LE). Averages L+R per frame.
///
/// - **NEON**: load 8 i16 (4 stereo frames), deinterleave, add, convert, scale
/// - **SSE**: load 8 i16 (4 stereo frames), shuffle, add, convert, scale
pub fn i16_le_stereo_to_f32_mono(data: &[u8]) -> Vec<f32> {
    let frame_count = data.len() / 4; // 4 bytes per stereo frame
    let mut out = vec![0.0f32; frame_count];
    let mut i = 0; // byte index
    let mut o = 0; // output sample index

    #[cfg(target_arch = "aarch64")]
    unsafe {
        use std::arch::aarch64::*;
        let scale = vdupq_n_f32(0.5 / 32768.0); // divide by 2 (average) and 32768 (normalize)
        // Process 4 stereo frames (16 bytes, 8 i16) per iteration → 4 mono f32
        while i + 16 <= data.len() {
            // Load 8 interleaved i16: [L0, R0, L1, R1, L2, R2, L3, R3]
            let raw = vld1q_s16(data.as_ptr().add(i) as *const i16);
            // Widen to i32: lower 4 and upper 4
            let lo_i32 = vmovl_s16(vget_low_s16(raw));   // [L0, R0, L1, R1] as i32
            let hi_i32 = vmovl_s16(vget_high_s16(raw));  // [L2, R2, L3, R3] as i32
            // Add pairs: L+R for each frame using pairwise add
            let sums = vpaddq_s32(lo_i32, hi_i32); // [L0+R0, L1+R1, L2+R2, L3+R3]
            let f = vmulq_f32(vcvtq_f32_s32(sums), scale);
            vst1q_f32(out.as_mut_ptr().add(o), f);
            i += 16;
            o += 4;
        }
    }

    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("sse4.1") && is_x86_feature_detected!("ssse3") {
            #[target_feature(enable = "sse4.1,ssse3")]
            unsafe fn inner(data: &[u8], out: &mut [f32], i: &mut usize, o: &mut usize) {
                use std::arch::x86_64::*;
                let scale = _mm_set1_ps(0.5 / 32768.0);
                while *i + 16 <= data.len() {
                    let raw = _mm_loadu_si128(data.as_ptr().add(*i) as *const __m128i);
                    let lo_i32 = _mm_cvtepi16_epi32(raw);
                    let hi_i32 = _mm_cvtepi16_epi32(_mm_srli_si128(raw, 8));
                    let sums = _mm_hadd_epi32(lo_i32, hi_i32);
                    let f = _mm_mul_ps(_mm_cvtepi32_ps(sums), scale);
                    _mm_storeu_ps(out.as_mut_ptr().add(*o), f);
                    *i += 16;
                    *o += 4;
                }
            }
            unsafe { inner(data, &mut out, &mut i, &mut o) };
        }
    }

    // Scalar remainder
    while i + 4 <= data.len() {
        let l = i16::from_le_bytes([data[i], data[i + 1]]) as f32;
        let r = i16::from_le_bytes([data[i + 2], data[i + 3]]) as f32;
        out[o] = (l + r) * (0.5 / 32768.0);
        i += 4;
        o += 1;
    }

    out
}

/// Fused: stereo i16 LE bytes → mono f32 with 2:1 decimation using SIMD.
///
/// Reads 8 stereo frames (32 bytes) per SIMD iteration, produces 4 mono f32 outputs.
/// Each output = average of 2 stereo frames: (L0+R0+L1+R1) / 4.
/// Output is at half the input sample rate (e.g., 44.1kHz stereo → 22.05kHz mono).
///
/// - **NEON**: double `vpaddq_s32` — first for stereo→mono, second for 2:1 decimation
/// - **SSE**: double `_mm_hadd_epi32` — same two-stage horizontal add
pub fn i16_le_stereo_decimate2_mono(data: &[u8]) -> Vec<f32> {
    // 2 stereo frames = 8 bytes per output sample
    let out_len = data.len() / 8;
    let mut out = vec![0.0f32; out_len];
    let mut i = 0; // byte index
    let mut o = 0; // output index

    #[cfg(target_arch = "aarch64")]
    unsafe {
        use std::arch::aarch64::*;
        // Scale: 1/(4×32768) — average of 4 i16 values (2ch × 2frames), then normalize
        let scale = vdupq_n_f32(1.0 / (4.0 * 32768.0));
        // Process 8 stereo frames (32 bytes) → 4 output samples per iteration
        while i + 32 <= data.len() {
            let raw_a = vld1q_s16(data.as_ptr().add(i) as *const i16);      // frames 0-3
            let raw_b = vld1q_s16(data.as_ptr().add(i + 16) as *const i16); // frames 4-7

            // Widen to i32
            let lo_a = vmovl_s16(vget_low_s16(raw_a));  // [L0, R0, L1, R1]
            let hi_a = vmovl_s16(vget_high_s16(raw_a)); // [L2, R2, L3, R3]
            let lo_b = vmovl_s16(vget_low_s16(raw_b));  // [L4, R4, L5, R5]
            let hi_b = vmovl_s16(vget_high_s16(raw_b)); // [L6, R6, L7, R7]

            // Pairwise add: L+R per frame → mono
            let mono_a = vpaddq_s32(lo_a, hi_a); // [L0+R0, L1+R1, L2+R2, L3+R3]
            let mono_b = vpaddq_s32(lo_b, hi_b); // [L4+R4, L5+R5, L6+R6, L7+R7]

            // Pairwise add again: sum adjacent mono samples → 2:1 decimation
            let decimated = vpaddq_s32(mono_a, mono_b);
            // [(L0+R0)+(L1+R1), (L2+R2)+(L3+R3), (L4+R4)+(L5+R5), (L6+R6)+(L7+R7)]

            let f = vmulq_f32(vcvtq_f32_s32(decimated), scale);
            vst1q_f32(out.as_mut_ptr().add(o), f);
            i += 32;
            o += 4;
        }
    }

    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("sse4.1") && is_x86_feature_detected!("ssse3") {
            #[target_feature(enable = "sse4.1,ssse3")]
            unsafe fn inner(data: &[u8], out: &mut [f32], i: &mut usize, o: &mut usize) {
                use std::arch::x86_64::*;
                let scale = _mm_set1_ps(1.0 / (4.0 * 32768.0));
                while *i + 32 <= data.len() {
                    let raw_a = _mm_loadu_si128(data.as_ptr().add(*i) as *const __m128i);
                    let raw_b = _mm_loadu_si128(data.as_ptr().add(*i + 16) as *const __m128i);

                    let lo_a = _mm_cvtepi16_epi32(raw_a);
                    let hi_a = _mm_cvtepi16_epi32(_mm_srli_si128(raw_a, 8));
                    let lo_b = _mm_cvtepi16_epi32(raw_b);
                    let hi_b = _mm_cvtepi16_epi32(_mm_srli_si128(raw_b, 8));

                    let mono_a = _mm_hadd_epi32(lo_a, hi_a);
                    let mono_b = _mm_hadd_epi32(lo_b, hi_b);
                    let decimated = _mm_hadd_epi32(mono_a, mono_b);

                    let f = _mm_mul_ps(_mm_cvtepi32_ps(decimated), scale);
                    _mm_storeu_ps(out.as_mut_ptr().add(*o), f);
                    *i += 32;
                    *o += 4;
                }
            }
            unsafe { inner(data, &mut out, &mut i, &mut o) };
        }
    }

    // Scalar remainder
    while i + 8 <= data.len() {
        let l0 = i16::from_le_bytes([data[i], data[i + 1]]) as f32;
        let r0 = i16::from_le_bytes([data[i + 2], data[i + 3]]) as f32;
        let l1 = i16::from_le_bytes([data[i + 4], data[i + 5]]) as f32;
        let r1 = i16::from_le_bytes([data[i + 6], data[i + 7]]) as f32;
        out[o] = (l0 + r0 + l1 + r1) * (1.0 / (4.0 * 32768.0));
        i += 8;
        o += 1;
    }

    out
}

/// SIMD-accelerated linear interpolation resampler for mono f32 audio.
///
/// Processes 4 output samples per iteration: scalar gather + SIMD lerp.
/// No library overhead, no thread dispatch, no constructor allocations.
///
/// - **NEON**: `vmlaq_f32` fused multiply-add for lerp
/// - **SSE**: `_mm_add_ps` + `_mm_mul_ps` for lerp
pub fn linear_resample_mono(samples: &[f32], from_rate: u32, to_rate: u32) -> Vec<f32> {
    let step = from_rate as f64 / to_rate as f64;
    // Conservative bound: ensures idx+1 never exceeds input length
    let out_len = ((samples.len().saturating_sub(1)) as f64 / step) as usize;
    let mut out: Vec<f32> = Vec::with_capacity(out_len);
    let mut pos = 0.0f64;

    #[cfg(target_arch = "aarch64")]
    unsafe {
        use std::arch::aarch64::*;
        let step4 = step * 4.0;

        while out.len() + 4 <= out_len {
            let i0 = pos as usize;
            let i1 = (pos + step) as usize;
            let i2 = (pos + step * 2.0) as usize;
            let i3 = (pos + step * 3.0) as usize;

            // Scalar gather (NEON has no gather instruction)
            let a_arr: [f32; 4] = [
                *samples.get_unchecked(i0),
                *samples.get_unchecked(i1),
                *samples.get_unchecked(i2),
                *samples.get_unchecked(i3),
            ];
            let b_arr: [f32; 4] = [
                *samples.get_unchecked(i0 + 1),
                *samples.get_unchecked(i1 + 1),
                *samples.get_unchecked(i2 + 1),
                *samples.get_unchecked(i3 + 1),
            ];
            let f_arr: [f32; 4] = [
                (pos - i0 as f64) as f32,
                (pos + step - i1 as f64) as f32,
                (pos + step * 2.0 - i2 as f64) as f32,
                (pos + step * 3.0 - i3 as f64) as f32,
            ];

            let a = vld1q_f32(a_arr.as_ptr());
            let b = vld1q_f32(b_arr.as_ptr());
            let frac = vld1q_f32(f_arr.as_ptr());

            // Fused multiply-add lerp: a + (b - a) * frac
            let result = vmlaq_f32(a, vsubq_f32(b, a), frac);
            vst1q_f32(out.as_mut_ptr().add(out.len()), result);
            out.set_len(out.len() + 4);
            pos += step4;
        }
    }

    #[cfg(target_arch = "x86_64")]
    unsafe {
        use std::arch::x86_64::*;
        let step4 = step * 4.0;

        while out.len() + 4 <= out_len {
            let i0 = pos as usize;
            let i1 = (pos + step) as usize;
            let i2 = (pos + step * 2.0) as usize;
            let i3 = (pos + step * 3.0) as usize;

            // _mm_set_ps takes args in reverse lane order (lane3, lane2, lane1, lane0)
            let a = _mm_set_ps(
                *samples.get_unchecked(i3),
                *samples.get_unchecked(i2),
                *samples.get_unchecked(i1),
                *samples.get_unchecked(i0),
            );
            let b = _mm_set_ps(
                *samples.get_unchecked(i3 + 1),
                *samples.get_unchecked(i2 + 1),
                *samples.get_unchecked(i1 + 1),
                *samples.get_unchecked(i0 + 1),
            );
            let frac = _mm_set_ps(
                (pos + step * 3.0 - i3 as f64) as f32,
                (pos + step * 2.0 - i2 as f64) as f32,
                (pos + step - i1 as f64) as f32,
                (pos - i0 as f64) as f32,
            );

            let diff = _mm_sub_ps(b, a);
            let result = _mm_add_ps(a, _mm_mul_ps(diff, frac));
            _mm_storeu_ps(out.as_mut_ptr().add(out.len()), result);
            out.set_len(out.len() + 4);
            pos += step4;
        }
    }

    // Scalar remainder
    while out.len() < out_len {
        let idx = pos as usize;
        let frac = (pos - idx as f64) as f32;
        let s = if idx + 1 < samples.len() {
            samples[idx] + (samples[idx + 1] - samples[idx]) * frac
        } else {
            samples[samples.len() - 1]
        };
        out.push(s);
        pos += step;
    }

    out
}

/// Convert f32 audio samples to i16 using SIMD-accelerated saturating narrowing.
pub fn f32_to_i16(samples: &[f32]) -> Vec<i16> {
    let mut out = vec![0i16; samples.len()];
    let len = samples.len();
    let mut i = 0;

    #[cfg(target_arch = "aarch64")]
    unsafe {
        use std::arch::aarch64::*;
        let scale = vdupq_n_f32(32767.0);
        while i + 8 <= len {
            let lo_f32 = vld1q_f32(samples.as_ptr().add(i));
            let hi_f32 = vld1q_f32(samples.as_ptr().add(i + 4));
            let lo_i32 = vcvtq_s32_f32(vmulq_f32(lo_f32, scale));
            let hi_i32 = vcvtq_s32_f32(vmulq_f32(hi_f32, scale));
            // Saturating narrow i32 → i16 (clamps to [-32768, 32767] automatically)
            let lo_i16 = vqmovn_s32(lo_i32);
            let hi_i16 = vqmovn_s32(hi_i32);
            vst1q_s16(out.as_mut_ptr().add(i), vcombine_s16(lo_i16, hi_i16));
            i += 8;
        }
    }

    #[cfg(target_arch = "x86_64")]
    unsafe {
        use std::arch::x86_64::*;
        let scale = _mm_set1_ps(32767.0);
        while i + 8 <= len {
            let lo_f32 = _mm_loadu_ps(samples.as_ptr().add(i));
            let hi_f32 = _mm_loadu_ps(samples.as_ptr().add(i + 4));
            let lo_i32 = _mm_cvtps_epi32(_mm_mul_ps(lo_f32, scale));
            let hi_i32 = _mm_cvtps_epi32(_mm_mul_ps(hi_f32, scale));
            // Signed saturating pack 2×4 i32 → 8 i16
            let packed = _mm_packs_epi32(lo_i32, hi_i32);
            _mm_storeu_si128(out.as_mut_ptr().add(i) as *mut __m128i, packed);
            i += 8;
        }
    }

    // Scalar remainder (and fallback for other architectures)
    while i < len {
        out[i] = (samples[i] * 32767.0).clamp(-32768.0, 32767.0) as i16;
        i += 1;
    }

    out
}
