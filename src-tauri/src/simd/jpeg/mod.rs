//! Baseline JPEG codec. The block kernels are written once over [`Simd`] and instantiated for
//! AVX2, NEON and scalar; no FMA, so every backend produces identical output.

#[cfg(target_arch = "aarch64")]
use std::arch::aarch64::*;
#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::*;



mod decode;
mod encode;

pub use decode::decode_at_least;
pub use encode::encode_rgb;

/// Natural (row-major) index of each zigzag position.
const ZIGZAG: [u8; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27,
    20, 13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58,
    59, 52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

/// Zigzag position of each coefficient as the kernel stores it: `u * 8 + v`, horizontal
/// frequency major (the transpose of natural order).
const ZZ_OF: [u8; 64] = {
    let mut t = [0u8; 64];
    let mut k = 0;
    while k < 64 {
        let n = ZIGZAG[k] as usize;
        t[(n % 8) * 8 + n / 8] = k as u8;
        k += 1;
    }
    t
};

/// Kernel index (`u * 8 + v`) of each zigzag position.
const S_OF: [u8; 64] = {
    let mut t = [0u8; 64];
    let mut s = 0;
    while s < 64 {
        t[ZZ_OF[s] as usize] = s as u8;
        s += 1;
    }
    t
};

/// Coefficients past this would need an 11-bit AC category, which baseline forbids.
const COEF_LIMIT: i32 = 1023;

/// AAN output scale per frequency: 1 for DC, sqrt(2)·cos(kπ/16) otherwise.
const AAN: [f64; 8] = [
    1.0, 1.387039845, 1.306562965, 1.175875602, 1.0, 0.785694958, 0.541196100, 0.275899379,
];

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Backend {
    Scalar,
    #[cfg(target_arch = "x86_64")]
    Avx2,
    #[cfg(target_arch = "aarch64")]
    Neon,
}

impl Backend {
    fn detect() -> Self {
        #[cfg(target_arch = "x86_64")]
        if is_x86_feature_detected!("avx2")
            && is_x86_feature_detected!("bmi1")
            && is_x86_feature_detected!("bmi2")
            && is_x86_feature_detected!("lzcnt")
        {
            return Backend::Avx2;
        }
        #[cfg(target_arch = "aarch64")]
        return Backend::Neon;
        #[allow(unreachable_code)]
        Backend::Scalar
    }
}

/// The vector operations the block kernel needs. `V` holds eight f32 lanes (one block row).
trait Simd {
    type V: Copy;
    unsafe fn splat(x: f32) -> Self::V;
    unsafe fn add(a: Self::V, b: Self::V) -> Self::V;
    unsafe fn sub(a: Self::V, b: Self::V) -> Self::V;
    unsafe fn mul(a: Self::V, b: Self::V) -> Self::V;
    /// 24 bytes of packed RGB as three channel vectors.
    unsafe fn load_rgb(p: *const u8) -> [Self::V; 3];
    unsafe fn transpose(r: &mut [Self::V; 8]);
    /// Round `v · recip` to nearest-even, clamp to ±COEF_LIMIT, store 8 i32, return the
    /// nonzero mask.
    unsafe fn quantize(v: Self::V, recip: *const f32, out: *mut i32) -> u8;
    unsafe fn load_f32(p: *const f32) -> Self::V;
    /// Eight i16 as f32.
    unsafe fn load_i16(p: *const i16) -> Self::V;
    /// Round to nearest-even and store as eight saturated u8.
    unsafe fn store_u8(v: Self::V, p: *mut u8);
    unsafe fn load_u8(p: *const u8) -> Self::V;
    unsafe fn store_f32(v: Self::V, p: *mut f32);
    /// Interleave lanes: `a0 b0 a1 b1 a2 b2 a3 b3`, then `a4 b4 .. a7 b7`.
    unsafe fn zip(a: Self::V, b: Self::V) -> (Self::V, Self::V);
    /// Round, saturate and store as 24 bytes of packed RGB.
    unsafe fn store_rgb(r: Self::V, g: Self::V, b: Self::V, p: *mut u8);
}

struct Scalar;

impl Simd for Scalar {
    type V = [f32; 8];
    #[inline(always)]
    unsafe fn splat(x: f32) -> Self::V {
        [x; 8]
    }
    #[inline(always)]
    unsafe fn add(a: Self::V, b: Self::V) -> Self::V {
        std::array::from_fn(|i| a[i] + b[i])
    }
    #[inline(always)]
    unsafe fn sub(a: Self::V, b: Self::V) -> Self::V {
        std::array::from_fn(|i| a[i] - b[i])
    }
    #[inline(always)]
    unsafe fn mul(a: Self::V, b: Self::V) -> Self::V {
        std::array::from_fn(|i| a[i] * b[i])
    }
    #[inline(always)]
    unsafe fn load_rgb(p: *const u8) -> [Self::V; 3] {
        std::array::from_fn(|c| std::array::from_fn(|i| f32::from(*p.add(i * 3 + c))))
    }
    #[inline(always)]
    unsafe fn transpose(r: &mut [Self::V; 8]) {
        let t = *r;
        for i in 0..8 {
            for j in 0..8 {
                r[i][j] = t[j][i];
            }
        }
    }
    #[inline(always)]
    unsafe fn quantize(v: Self::V, recip: *const f32, out: *mut i32) -> u8 {
        let mut mask = 0;
        for (i, &x) in v.iter().enumerate() {
            let q = ((x * *recip.add(i)).round_ties_even() as i32).clamp(-COEF_LIMIT, COEF_LIMIT);
            *out.add(i) = q;
            mask |= u8::from(q != 0) << i;
        }
        mask
    }
    #[inline(always)]
    unsafe fn load_f32(p: *const f32) -> Self::V {
        std::array::from_fn(|i| *p.add(i))
    }
    #[inline(always)]
    unsafe fn load_i16(p: *const i16) -> Self::V {
        std::array::from_fn(|i| f32::from(*p.add(i)))
    }
    #[inline(always)]
    unsafe fn store_u8(v: Self::V, p: *mut u8) {
        for (i, &x) in v.iter().enumerate() {
            *p.add(i) = (x.round_ties_even() as i32).clamp(0, 255) as u8;
        }
    }
    #[inline(always)]
    unsafe fn load_u8(p: *const u8) -> Self::V {
        std::array::from_fn(|i| f32::from(*p.add(i)))
    }
    #[inline(always)]
    unsafe fn store_f32(v: Self::V, p: *mut f32) {
        std::ptr::copy_nonoverlapping(v.as_ptr(), p, 8);
    }
    #[inline(always)]
    unsafe fn zip(a: Self::V, b: Self::V) -> (Self::V, Self::V) {
        let f = |o: usize| std::array::from_fn(|i| if i % 2 == 0 { a[o + i / 2] } else { b[o + i / 2] });
        (f(0), f(4))
    }
    #[inline(always)]
    unsafe fn store_rgb(r: Self::V, g: Self::V, b: Self::V, p: *mut u8) {
        for i in 0..8 {
            for (c, v) in [r, g, b].iter().enumerate() {
                *p.add(i * 3 + c) = (v[i].round_ties_even() as i32).clamp(0, 255) as u8;
            }
        }
    }
}

#[cfg(target_arch = "x86_64")]
struct Avx2;

#[cfg(target_arch = "x86_64")]
impl Simd for Avx2 {
    type V = __m256;
    #[inline(always)]
    unsafe fn splat(x: f32) -> __m256 {
        _mm256_set1_ps(x)
    }
    #[inline(always)]
    unsafe fn add(a: __m256, b: __m256) -> __m256 {
        _mm256_add_ps(a, b)
    }
    #[inline(always)]
    unsafe fn sub(a: __m256, b: __m256) -> __m256 {
        _mm256_sub_ps(a, b)
    }
    #[inline(always)]
    unsafe fn mul(a: __m256, b: __m256) -> __m256 {
        _mm256_mul_ps(a, b)
    }
    #[inline(always)]
    unsafe fn load_rgb(p: *const u8) -> [__m256; 3] {
        // Exactly 24 bytes: 16 then 8, so a block at the buffer's end never over-reads.
        let lo = _mm_loadu_si128(p as *const __m128i);
        let hi = _mm_loadl_epi64(p.add(16) as *const __m128i);
        let rg = _mm_or_si128(
            _mm_shuffle_epi8(lo, _mm_setr_epi8(0, 3, 6, 9, 12, 15, -1, -1, 1, 4, 7, 10, 13, -1, -1, -1)),
            _mm_shuffle_epi8(hi, _mm_setr_epi8(-1, -1, -1, -1, -1, -1, 2, 5, -1, -1, -1, -1, -1, 0, 3, 6)),
        );
        let b = _mm_or_si128(
            _mm_shuffle_epi8(lo, _mm_setr_epi8(2, 5, 8, 11, 14, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1)),
            _mm_shuffle_epi8(hi, _mm_setr_epi8(-1, -1, -1, -1, -1, 1, 4, 7, -1, -1, -1, -1, -1, -1, -1, -1)),
        );
        let f = |x: __m128i| _mm256_cvtepi32_ps(_mm256_cvtepu8_epi32(x));
        [f(rg), f(_mm_srli_si128(rg, 8)), f(b)]
    }
    #[inline(always)]
    unsafe fn transpose(r: &mut [__m256; 8]) {
        let t0 = _mm256_unpacklo_ps(r[0], r[1]);
        let t1 = _mm256_unpackhi_ps(r[0], r[1]);
        let t2 = _mm256_unpacklo_ps(r[2], r[3]);
        let t3 = _mm256_unpackhi_ps(r[2], r[3]);
        let t4 = _mm256_unpacklo_ps(r[4], r[5]);
        let t5 = _mm256_unpackhi_ps(r[4], r[5]);
        let t6 = _mm256_unpacklo_ps(r[6], r[7]);
        let t7 = _mm256_unpackhi_ps(r[6], r[7]);
        let s0 = _mm256_shuffle_ps::<0x44>(t0, t2);
        let s1 = _mm256_shuffle_ps::<0xEE>(t0, t2);
        let s2 = _mm256_shuffle_ps::<0x44>(t1, t3);
        let s3 = _mm256_shuffle_ps::<0xEE>(t1, t3);
        let s4 = _mm256_shuffle_ps::<0x44>(t4, t6);
        let s5 = _mm256_shuffle_ps::<0xEE>(t4, t6);
        let s6 = _mm256_shuffle_ps::<0x44>(t5, t7);
        let s7 = _mm256_shuffle_ps::<0xEE>(t5, t7);
        r[0] = _mm256_permute2f128_ps::<0x20>(s0, s4);
        r[1] = _mm256_permute2f128_ps::<0x20>(s1, s5);
        r[2] = _mm256_permute2f128_ps::<0x20>(s2, s6);
        r[3] = _mm256_permute2f128_ps::<0x20>(s3, s7);
        r[4] = _mm256_permute2f128_ps::<0x31>(s0, s4);
        r[5] = _mm256_permute2f128_ps::<0x31>(s1, s5);
        r[6] = _mm256_permute2f128_ps::<0x31>(s2, s6);
        r[7] = _mm256_permute2f128_ps::<0x31>(s3, s7);
    }
    #[inline(always)]
    unsafe fn quantize(v: __m256, recip: *const f32, out: *mut i32) -> u8 {
        let q = _mm256_cvtps_epi32(_mm256_mul_ps(v, _mm256_loadu_ps(recip)));
        let q = _mm256_min_epi32(_mm256_max_epi32(q, _mm256_set1_epi32(-COEF_LIMIT)), _mm256_set1_epi32(COEF_LIMIT));
        _mm256_storeu_si256(out as *mut __m256i, q);
        let zero = _mm256_cmpeq_epi32(q, _mm256_setzero_si256());
        !(_mm256_movemask_ps(_mm256_castsi256_ps(zero)) as u8)
    }
    #[inline(always)]
    unsafe fn load_f32(p: *const f32) -> __m256 {
        _mm256_loadu_ps(p)
    }
    #[inline(always)]
    unsafe fn load_i16(p: *const i16) -> __m256 {
        _mm256_cvtepi32_ps(_mm256_cvtepi16_epi32(_mm_loadu_si128(p as *const __m128i)))
    }
    #[inline(always)]
    unsafe fn store_u8(v: __m256, p: *mut u8) {
        _mm_storel_epi64(p as *mut __m128i, Self::to_u8x8(v));
    }
    #[inline(always)]
    unsafe fn load_u8(p: *const u8) -> __m256 {
        _mm256_cvtepi32_ps(_mm256_cvtepu8_epi32(_mm_loadl_epi64(p as *const __m128i)))
    }
    #[inline(always)]
    unsafe fn store_f32(v: __m256, p: *mut f32) {
        _mm256_storeu_ps(p, v)
    }
    #[inline(always)]
    unsafe fn zip(a: __m256, b: __m256) -> (__m256, __m256) {
        let (lo, hi) = (_mm256_unpacklo_ps(a, b), _mm256_unpackhi_ps(a, b));
        (_mm256_permute2f128_ps::<0x20>(lo, hi), _mm256_permute2f128_ps::<0x31>(lo, hi))
    }
    #[inline(always)]
    unsafe fn store_rgb(r: __m256, g: __m256, b: __m256, p: *mut u8) {
        let rg = _mm_unpacklo_epi64(Self::to_u8x8(r), Self::to_u8x8(g));
        let b = Self::to_u8x8(b);
        let head = _mm_or_si128(
            _mm_shuffle_epi8(rg, _mm_setr_epi8(0, 8, -1, 1, 9, -1, 2, 10, -1, 3, 11, -1, 4, 12, -1, 5)),
            _mm_shuffle_epi8(b, _mm_setr_epi8(-1, -1, 0, -1, -1, 1, -1, -1, 2, -1, -1, 3, -1, -1, 4, -1)),
        );
        let tail = _mm_or_si128(
            _mm_shuffle_epi8(rg, _mm_setr_epi8(13, -1, 6, 14, -1, 7, 15, -1, -1, -1, -1, -1, -1, -1, -1, -1)),
            _mm_shuffle_epi8(b, _mm_setr_epi8(-1, 5, -1, -1, 6, -1, -1, 7, -1, -1, -1, -1, -1, -1, -1, -1)),
        );
        _mm_storeu_si128(p as *mut __m128i, head);
        _mm_storel_epi64(p.add(16) as *mut __m128i, tail);
    }
}

#[cfg(target_arch = "x86_64")]
impl Avx2 {
    /// Round and saturate eight lanes into the low 8 bytes.
    #[inline(always)]
    unsafe fn to_u8x8(v: __m256) -> __m128i {
        let i = _mm256_cvtps_epi32(v);
        let w = _mm_packs_epi32(_mm256_castsi256_si128(i), _mm256_extracti128_si256::<1>(i));
        _mm_packus_epi16(w, w)
    }
}

#[cfg(target_arch = "aarch64")]
struct Neon;

#[cfg(target_arch = "aarch64")]
#[derive(Clone, Copy)]
struct F32x8(float32x4_t, float32x4_t);

#[cfg(target_arch = "aarch64")]
impl Neon {
    /// Round and saturate eight lanes to u8.
    #[inline(always)]
    unsafe fn to_u8x8(v: F32x8) -> uint8x8_t {
        vqmovun_s16(vcombine_s16(vqmovn_s32(vcvtnq_s32_f32(v.0)), vqmovn_s32(vcvtnq_s32_f32(v.1))))
    }

    #[inline(always)]
    unsafe fn transpose4(a: float32x4_t, b: float32x4_t, c: float32x4_t, d: float32x4_t) -> [float32x4_t; 4] {
        let t0 = vreinterpretq_f64_f32(vtrn1q_f32(a, b));
        let t1 = vreinterpretq_f64_f32(vtrn2q_f32(a, b));
        let t2 = vreinterpretq_f64_f32(vtrn1q_f32(c, d));
        let t3 = vreinterpretq_f64_f32(vtrn2q_f32(c, d));
        [
            vreinterpretq_f32_f64(vtrn1q_f64(t0, t2)),
            vreinterpretq_f32_f64(vtrn1q_f64(t1, t3)),
            vreinterpretq_f32_f64(vtrn2q_f64(t0, t2)),
            vreinterpretq_f32_f64(vtrn2q_f64(t1, t3)),
        ]
    }
}

#[cfg(target_arch = "aarch64")]
impl Simd for Neon {
    type V = F32x8;
    #[inline(always)]
    unsafe fn splat(x: f32) -> F32x8 {
        F32x8(vdupq_n_f32(x), vdupq_n_f32(x))
    }
    #[inline(always)]
    unsafe fn add(a: F32x8, b: F32x8) -> F32x8 {
        F32x8(vaddq_f32(a.0, b.0), vaddq_f32(a.1, b.1))
    }
    #[inline(always)]
    unsafe fn sub(a: F32x8, b: F32x8) -> F32x8 {
        F32x8(vsubq_f32(a.0, b.0), vsubq_f32(a.1, b.1))
    }
    #[inline(always)]
    unsafe fn mul(a: F32x8, b: F32x8) -> F32x8 {
        F32x8(vmulq_f32(a.0, b.0), vmulq_f32(a.1, b.1))
    }
    #[inline(always)]
    unsafe fn load_rgb(p: *const u8) -> [F32x8; 3] {
        let px = vld3_u8(p);
        let f = |c: uint8x8_t| {
            let w = vmovl_u8(c);
            F32x8(vcvtq_f32_u32(vmovl_u16(vget_low_u16(w))), vcvtq_f32_u32(vmovl_high_u16(w)))
        };
        [f(px.0), f(px.1), f(px.2)]
    }
    #[inline(always)]
    unsafe fn transpose(r: &mut [F32x8; 8]) {
        let a = Self::transpose4(r[0].0, r[1].0, r[2].0, r[3].0);
        let b = Self::transpose4(r[4].0, r[5].0, r[6].0, r[7].0);
        let c = Self::transpose4(r[0].1, r[1].1, r[2].1, r[3].1);
        let d = Self::transpose4(r[4].1, r[5].1, r[6].1, r[7].1);
        for i in 0..4 {
            r[i] = F32x8(a[i], b[i]);
            r[i + 4] = F32x8(c[i], d[i]);
        }
    }
    #[inline(always)]
    unsafe fn quantize(v: F32x8, recip: *const f32, out: *mut i32) -> u8 {
        let (lo, hi) = (vdupq_n_s32(-COEF_LIMIT), vdupq_n_s32(COEF_LIMIT));
        let q = |x: float32x4_t, r: *const f32| vminq_s32(vmaxq_s32(vcvtnq_s32_f32(vmulq_f32(x, vld1q_f32(r))), lo), hi);
        let (a, b) = (q(v.0, recip), q(v.1, recip.add(4)));
        vst1q_s32(out, a);
        vst1q_s32(out.add(4), b);
        const WEIGHTS: [u32; 8] = [1, 2, 4, 8, 16, 32, 64, 128];
        let w = |x: int32x4_t, o: usize| vaddvq_u32(vandq_u32(vtstq_s32(x, x), vld1q_u32(WEIGHTS.as_ptr().add(o))));
        (w(a, 0) | w(b, 4)) as u8
    }
    #[inline(always)]
    unsafe fn load_f32(p: *const f32) -> F32x8 {
        F32x8(vld1q_f32(p), vld1q_f32(p.add(4)))
    }
    #[inline(always)]
    unsafe fn load_i16(p: *const i16) -> F32x8 {
        let x = vld1q_s16(p);
        F32x8(vcvtq_f32_s32(vmovl_s16(vget_low_s16(x))), vcvtq_f32_s32(vmovl_high_s16(x)))
    }
    #[inline(always)]
    unsafe fn store_u8(v: F32x8, p: *mut u8) {
        vst1_u8(p, Self::to_u8x8(v));
    }
    #[inline(always)]
    unsafe fn load_u8(p: *const u8) -> F32x8 {
        let w = vmovl_u8(vld1_u8(p));
        F32x8(vcvtq_f32_u32(vmovl_u16(vget_low_u16(w))), vcvtq_f32_u32(vmovl_high_u16(w)))
    }
    #[inline(always)]
    unsafe fn store_f32(v: F32x8, p: *mut f32) {
        vst1q_f32(p, v.0);
        vst1q_f32(p.add(4), v.1);
    }
    #[inline(always)]
    unsafe fn zip(a: F32x8, b: F32x8) -> (F32x8, F32x8) {
        (F32x8(vzip1q_f32(a.0, b.0), vzip2q_f32(a.0, b.0)), F32x8(vzip1q_f32(a.1, b.1), vzip2q_f32(a.1, b.1)))
    }
    #[inline(always)]
    unsafe fn store_rgb(r: F32x8, g: F32x8, b: F32x8, p: *mut u8) {
        vst3_u8(p, uint8x8x3_t(Self::to_u8x8(r), Self::to_u8x8(g), Self::to_u8x8(b)));
    }
}

