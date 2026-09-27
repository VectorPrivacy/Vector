//! ChaCha20 keystream (RFC 8439), computed N blocks at a time in the "vertical"
//! layout: vector `x[i]` holds state word `i` of N consecutive blocks, so one
//! round function serves every backend and only rotation and the final
//! transpose are ISA-specific.

use zeroize::Zeroize;

/// Largest batch any backend produces (AVX2: 8 blocks).
pub(super) const MAX_BLOCKS: usize = 8;
pub(super) const BUF: usize = MAX_BLOCKS * 64;

const SIGMA: [u32; 4] = [0x6170_7865, 0x3320_646e, 0x7962_2d32, 0x6b20_6574];

/// Key and nonce expanded to the 16-word ChaCha input, minus the counter.
pub(super) struct State {
    pub(super) key: [u32; 8],
    pub(super) nonce: [u32; 3],
}

impl Drop for State {
    fn drop(&mut self) {
        self.key.zeroize();
        self.nonce.zeroize();
    }
}

trait Lanes: Copy {
    unsafe fn splat(x: u32) -> Self;
    /// `base + lane_index` in each lane.
    unsafe fn counters(base: u32) -> Self;
    unsafe fn add(a: Self, b: Self) -> Self;
    unsafe fn xor(a: Self, b: Self) -> Self;
    unsafe fn rotl16(a: Self) -> Self;
    unsafe fn rotl12(a: Self) -> Self;
    unsafe fn rotl8(a: Self) -> Self;
    unsafe fn rotl7(a: Self) -> Self;
    /// Write the N blocks contiguously to `out[..64 * N]`.
    unsafe fn store(x: &[Self; 16], out: &mut [u8]);
}

#[inline(always)]
unsafe fn quarter<V: Lanes>(x: &mut [V; 16], a: usize, b: usize, c: usize, d: usize) {
    x[a] = V::add(x[a], x[b]);
    x[d] = V::rotl16(V::xor(x[d], x[a]));
    x[c] = V::add(x[c], x[d]);
    x[b] = V::rotl12(V::xor(x[b], x[c]));
    x[a] = V::add(x[a], x[b]);
    x[d] = V::rotl8(V::xor(x[d], x[a]));
    x[c] = V::add(x[c], x[d]);
    x[b] = V::rotl7(V::xor(x[b], x[c]));
}

#[inline(always)]
unsafe fn blocks<V: Lanes>(st: &State, counter: u32, out: &mut [u8]) {
    let init: [V; 16] = [
        V::splat(SIGMA[0]), V::splat(SIGMA[1]), V::splat(SIGMA[2]), V::splat(SIGMA[3]),
        V::splat(st.key[0]), V::splat(st.key[1]), V::splat(st.key[2]), V::splat(st.key[3]),
        V::splat(st.key[4]), V::splat(st.key[5]), V::splat(st.key[6]), V::splat(st.key[7]),
        V::counters(counter), V::splat(st.nonce[0]), V::splat(st.nonce[1]), V::splat(st.nonce[2]),
    ];
    let mut x = init;
    for _ in 0..10 {
        quarter(&mut x, 0, 4, 8, 12);
        quarter(&mut x, 1, 5, 9, 13);
        quarter(&mut x, 2, 6, 10, 14);
        quarter(&mut x, 3, 7, 11, 15);
        quarter(&mut x, 0, 5, 10, 15);
        quarter(&mut x, 1, 6, 11, 12);
        quarter(&mut x, 2, 7, 8, 13);
        quarter(&mut x, 3, 4, 9, 14);
    }
    for i in 0..16 {
        x[i] = V::add(x[i], init[i]);
    }
    V::store(&x, out);
}

// ---------------------------------------------------------------------------
// Portable
// ---------------------------------------------------------------------------

impl Lanes for u32 {
    #[inline(always)] unsafe fn splat(x: u32) -> Self { x }
    #[inline(always)] unsafe fn counters(base: u32) -> Self { base }
    #[inline(always)] unsafe fn add(a: Self, b: Self) -> Self { a.wrapping_add(b) }
    #[inline(always)] unsafe fn xor(a: Self, b: Self) -> Self { a ^ b }
    #[inline(always)] unsafe fn rotl16(a: Self) -> Self { a.rotate_left(16) }
    #[inline(always)] unsafe fn rotl12(a: Self) -> Self { a.rotate_left(12) }
    #[inline(always)] unsafe fn rotl8(a: Self) -> Self { a.rotate_left(8) }
    #[inline(always)] unsafe fn rotl7(a: Self) -> Self { a.rotate_left(7) }
    #[inline(always)]
    unsafe fn store(x: &[Self; 16], out: &mut [u8]) {
        for (w, o) in x.iter().zip(out.chunks_exact_mut(4)) {
            o.copy_from_slice(&w.to_le_bytes());
        }
    }
}

/// Two blocks interleaved on the integer units, for cores wide enough that a vector
/// pass computing blocks nobody needs costs more.
#[cfg(any(test, target_arch = "aarch64"))]
#[derive(Clone, Copy)]
pub(super) struct Pair(u32, u32);

#[cfg(any(test, target_arch = "aarch64"))]
impl Lanes for Pair {
    #[inline(always)] unsafe fn splat(x: u32) -> Self { Pair(x, x) }
    #[inline(always)] unsafe fn counters(base: u32) -> Self { Pair(base, base.wrapping_add(1)) }
    #[inline(always)] unsafe fn add(a: Self, b: Self) -> Self { Pair(a.0.wrapping_add(b.0), a.1.wrapping_add(b.1)) }
    #[inline(always)] unsafe fn xor(a: Self, b: Self) -> Self { Pair(a.0 ^ b.0, a.1 ^ b.1) }
    #[inline(always)] unsafe fn rotl16(a: Self) -> Self { Pair(a.0.rotate_left(16), a.1.rotate_left(16)) }
    #[inline(always)] unsafe fn rotl12(a: Self) -> Self { Pair(a.0.rotate_left(12), a.1.rotate_left(12)) }
    #[inline(always)] unsafe fn rotl8(a: Self) -> Self { Pair(a.0.rotate_left(8), a.1.rotate_left(8)) }
    #[inline(always)] unsafe fn rotl7(a: Self) -> Self { Pair(a.0.rotate_left(7), a.1.rotate_left(7)) }
    #[inline(always)]
    unsafe fn store(x: &[Self; 16], out: &mut [u8]) {
        for (i, w) in x.iter().enumerate() {
            out[4 * i..4 * i + 4].copy_from_slice(&w.0.to_le_bytes());
            out[64 + 4 * i..64 + 4 * i + 4].copy_from_slice(&w.1.to_le_bytes());
        }
    }
}

#[cfg(test)]
pub(super) fn pair_blocks(st: &State, counter: u32, out: &mut [u8]) {
    // SAFETY: plain integer arithmetic.
    unsafe { blocks::<Pair>(st, counter, out) }
}

// ---------------------------------------------------------------------------
// x86 / x86_64
// ---------------------------------------------------------------------------

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
mod x86 {
    use super::Lanes;
    #[cfg(target_arch = "x86")]
    use core::arch::x86::*;
    #[cfg(target_arch = "x86_64")]
    use core::arch::x86_64::*;

    #[inline(always)]
    unsafe fn transpose4(a: __m128i, b: __m128i, c: __m128i, d: __m128i) -> [__m128i; 4] {
        let t0 = _mm_unpacklo_epi32(a, b);
        let t1 = _mm_unpacklo_epi32(c, d);
        let t2 = _mm_unpackhi_epi32(a, b);
        let t3 = _mm_unpackhi_epi32(c, d);
        [
            _mm_unpacklo_epi64(t0, t1),
            _mm_unpackhi_epi64(t0, t1),
            _mm_unpacklo_epi64(t2, t3),
            _mm_unpackhi_epi64(t2, t3),
        ]
    }

    #[inline(always)]
    unsafe fn store4(x: &[__m128i; 16], out: &mut [u8]) {
        debug_assert!(out.len() >= 256);
        let p = out.as_mut_ptr();
        for g in 0..4 {
            let b = transpose4(x[4 * g], x[4 * g + 1], x[4 * g + 2], x[4 * g + 3]);
            for (j, v) in b.iter().enumerate() {
                _mm_storeu_si128(p.add(64 * j + 16 * g) as *mut __m128i, *v);
            }
        }
    }

    /// SSE2 only, rotations as shift/shift/or.
    #[derive(Clone, Copy)]
    pub(super) struct Sse2(__m128i);

    impl Lanes for Sse2 {
        #[inline(always)] unsafe fn splat(x: u32) -> Self { Sse2(_mm_set1_epi32(x as i32)) }
        #[inline(always)] unsafe fn counters(b: u32) -> Self {
            Sse2(_mm_add_epi32(_mm_set1_epi32(b as i32), _mm_setr_epi32(0, 1, 2, 3)))
        }
        #[inline(always)] unsafe fn add(a: Self, b: Self) -> Self { Sse2(_mm_add_epi32(a.0, b.0)) }
        #[inline(always)] unsafe fn xor(a: Self, b: Self) -> Self { Sse2(_mm_xor_si128(a.0, b.0)) }
        #[inline(always)] unsafe fn rotl16(a: Self) -> Self {
            Sse2(_mm_or_si128(_mm_slli_epi32(a.0, 16), _mm_srli_epi32(a.0, 16)))
        }
        #[inline(always)] unsafe fn rotl12(a: Self) -> Self {
            Sse2(_mm_or_si128(_mm_slli_epi32(a.0, 12), _mm_srli_epi32(a.0, 20)))
        }
        #[inline(always)] unsafe fn rotl8(a: Self) -> Self {
            Sse2(_mm_or_si128(_mm_slli_epi32(a.0, 8), _mm_srli_epi32(a.0, 24)))
        }
        #[inline(always)] unsafe fn rotl7(a: Self) -> Self {
            Sse2(_mm_or_si128(_mm_slli_epi32(a.0, 7), _mm_srli_epi32(a.0, 25)))
        }
        #[inline(always)] unsafe fn store(x: &[Self; 16], out: &mut [u8]) {
            store4(&core::mem::transmute::<[Self; 16], [__m128i; 16]>(*x), out)
        }
    }

    #[derive(Clone, Copy)]
    pub(super) struct Avx8(__m256i);

    impl Lanes for Avx8 {
        #[inline(always)] unsafe fn splat(x: u32) -> Self { Avx8(_mm256_set1_epi32(x as i32)) }
        #[inline(always)] unsafe fn counters(b: u32) -> Self {
            Avx8(_mm256_add_epi32(_mm256_set1_epi32(b as i32), _mm256_setr_epi32(0, 1, 2, 3, 4, 5, 6, 7)))
        }
        #[inline(always)] unsafe fn add(a: Self, b: Self) -> Self { Avx8(_mm256_add_epi32(a.0, b.0)) }
        #[inline(always)] unsafe fn xor(a: Self, b: Self) -> Self { Avx8(_mm256_xor_si256(a.0, b.0)) }
        #[inline(always)] unsafe fn rotl16(a: Self) -> Self {
            Avx8(_mm256_shuffle_epi8(a.0, _mm256_setr_epi8(
                2, 3, 0, 1, 6, 7, 4, 5, 10, 11, 8, 9, 14, 15, 12, 13,
                2, 3, 0, 1, 6, 7, 4, 5, 10, 11, 8, 9, 14, 15, 12, 13)))
        }
        #[inline(always)] unsafe fn rotl12(a: Self) -> Self {
            Avx8(_mm256_or_si256(_mm256_slli_epi32(a.0, 12), _mm256_srli_epi32(a.0, 20)))
        }
        #[inline(always)] unsafe fn rotl8(a: Self) -> Self {
            Avx8(_mm256_shuffle_epi8(a.0, _mm256_setr_epi8(
                3, 0, 1, 2, 7, 4, 5, 6, 11, 8, 9, 10, 15, 12, 13, 14,
                3, 0, 1, 2, 7, 4, 5, 6, 11, 8, 9, 10, 15, 12, 13, 14)))
        }
        #[inline(always)] unsafe fn rotl7(a: Self) -> Self {
            Avx8(_mm256_or_si256(_mm256_slli_epi32(a.0, 7), _mm256_srli_epi32(a.0, 25)))
        }
        #[inline(always)]
        unsafe fn store(x: &[Self; 16], out: &mut [u8]) {
            debug_assert!(out.len() >= 512);
            let p = out.as_mut_ptr();
            for g in 0..4 {
                // Per 128-bit half: lane j of the low half is block j, of the high half block j+4.
                let (a, b, c, d) = (x[4 * g].0, x[4 * g + 1].0, x[4 * g + 2].0, x[4 * g + 3].0);
                let t0 = _mm256_unpacklo_epi32(a, b);
                let t1 = _mm256_unpacklo_epi32(c, d);
                let t2 = _mm256_unpackhi_epi32(a, b);
                let t3 = _mm256_unpackhi_epi32(c, d);
                let r = [
                    _mm256_unpacklo_epi64(t0, t1),
                    _mm256_unpackhi_epi64(t0, t1),
                    _mm256_unpacklo_epi64(t2, t3),
                    _mm256_unpackhi_epi64(t2, t3),
                ];
                for (j, v) in r.iter().enumerate() {
                    _mm_storeu_si128(p.add(64 * j + 16 * g) as *mut __m128i, _mm256_castsi256_si128(*v));
                    _mm_storeu_si128(p.add(64 * (j + 4) + 16 * g) as *mut __m128i, _mm256_extracti128_si256(*v, 1));
                }
            }
        }
    }

    #[target_feature(enable = "sse2")]
    pub(super) unsafe fn sse2(st: &super::State, ctr: u32, out: &mut [u8]) {
        super::blocks::<Sse2>(st, ctr, out)
    }

    /// Four blocks in the row layout: two blocks per register, two independent
    /// register sets. Fewer instructions than the column layout when only a few
    /// blocks are needed, which is every chat message.
    #[target_feature(enable = "avx2")]
    pub(super) unsafe fn avx_rows4(st: &super::State, ctr: u32, out: &mut [u8]) {
        debug_assert!(out.len() >= 256);
        let k = &st.key;
        let row0 = _mm256_broadcastsi128_si256(_mm_setr_epi32(
            super::SIGMA[0] as i32, super::SIGMA[1] as i32, super::SIGMA[2] as i32, super::SIGMA[3] as i32));
        let row1 = _mm256_broadcastsi128_si256(_mm_setr_epi32(k[0] as i32, k[1] as i32, k[2] as i32, k[3] as i32));
        let row2 = _mm256_broadcastsi128_si256(_mm_setr_epi32(k[4] as i32, k[5] as i32, k[6] as i32, k[7] as i32));
        let n = &st.nonce;
        let row3 = _mm256_broadcastsi128_si256(_mm_setr_epi32(ctr as i32, n[0] as i32, n[1] as i32, n[2] as i32));
        let d0 = _mm256_add_epi32(row3, _mm256_setr_epi32(0, 0, 0, 0, 1, 0, 0, 0));
        let d1 = _mm256_add_epi32(row3, _mm256_setr_epi32(2, 0, 0, 0, 3, 0, 0, 0));
        let r16 = _mm256_setr_epi8(
            2, 3, 0, 1, 6, 7, 4, 5, 10, 11, 8, 9, 14, 15, 12, 13,
            2, 3, 0, 1, 6, 7, 4, 5, 10, 11, 8, 9, 14, 15, 12, 13);
        let r8 = _mm256_setr_epi8(
            3, 0, 1, 2, 7, 4, 5, 6, 11, 8, 9, 10, 15, 12, 13, 14,
            3, 0, 1, 2, 7, 4, 5, 6, 11, 8, 9, 10, 15, 12, 13, 14);

        let (mut a0, mut b0, mut c0, mut e0) = (row0, row1, row2, d0);
        let (mut a1, mut b1, mut c1, mut e1) = (row0, row1, row2, d1);

        macro_rules! half {
            ($a:ident, $b:ident, $c:ident, $d:ident) => {
                $a = _mm256_add_epi32($a, $b); $d = _mm256_shuffle_epi8(_mm256_xor_si256($d, $a), r16);
                $c = _mm256_add_epi32($c, $d); $b = _mm256_xor_si256($b, $c);
                $b = _mm256_or_si256(_mm256_slli_epi32($b, 12), _mm256_srli_epi32($b, 20));
                $a = _mm256_add_epi32($a, $b); $d = _mm256_shuffle_epi8(_mm256_xor_si256($d, $a), r8);
                $c = _mm256_add_epi32($c, $d); $b = _mm256_xor_si256($b, $c);
                $b = _mm256_or_si256(_mm256_slli_epi32($b, 7), _mm256_srli_epi32($b, 25));
            };
        }
        for _ in 0..10 {
            half!(a0, b0, c0, e0);
            half!(a1, b1, c1, e1);
            // Diagonalise: lane i now holds (a_i, b_{i+1}, c_{i+2}, d_{i+3}).
            b0 = _mm256_shuffle_epi32(b0, 0x39); c0 = _mm256_shuffle_epi32(c0, 0x4e); e0 = _mm256_shuffle_epi32(e0, 0x93);
            b1 = _mm256_shuffle_epi32(b1, 0x39); c1 = _mm256_shuffle_epi32(c1, 0x4e); e1 = _mm256_shuffle_epi32(e1, 0x93);
            half!(a0, b0, c0, e0);
            half!(a1, b1, c1, e1);
            b0 = _mm256_shuffle_epi32(b0, 0x93); c0 = _mm256_shuffle_epi32(c0, 0x4e); e0 = _mm256_shuffle_epi32(e0, 0x39);
            b1 = _mm256_shuffle_epi32(b1, 0x93); c1 = _mm256_shuffle_epi32(c1, 0x4e); e1 = _mm256_shuffle_epi32(e1, 0x39);
        }
        a0 = _mm256_add_epi32(a0, row0); b0 = _mm256_add_epi32(b0, row1);
        c0 = _mm256_add_epi32(c0, row2); e0 = _mm256_add_epi32(e0, d0);
        a1 = _mm256_add_epi32(a1, row0); b1 = _mm256_add_epi32(b1, row1);
        c1 = _mm256_add_epi32(c1, row2); e1 = _mm256_add_epi32(e1, d1);

        let p = out.as_mut_ptr() as *mut __m256i;
        // Low 128 bits are the even block of each set, high 128 bits the odd one.
        _mm256_storeu_si256(p, _mm256_permute2x128_si256(a0, b0, 0x20));
        _mm256_storeu_si256(p.add(1), _mm256_permute2x128_si256(c0, e0, 0x20));
        _mm256_storeu_si256(p.add(2), _mm256_permute2x128_si256(a0, b0, 0x31));
        _mm256_storeu_si256(p.add(3), _mm256_permute2x128_si256(c0, e0, 0x31));
        _mm256_storeu_si256(p.add(4), _mm256_permute2x128_si256(a1, b1, 0x20));
        _mm256_storeu_si256(p.add(5), _mm256_permute2x128_si256(c1, e1, 0x20));
        _mm256_storeu_si256(p.add(6), _mm256_permute2x128_si256(a1, b1, 0x31));
        _mm256_storeu_si256(p.add(7), _mm256_permute2x128_si256(c1, e1, 0x31));
    }

    #[target_feature(enable = "avx2")]
    pub(super) unsafe fn avx8(st: &super::State, ctr: u32, out: &mut [u8]) {
        super::blocks::<Avx8>(st, ctr, out)
    }
}

// ---------------------------------------------------------------------------
// aarch64 — NEON is baseline, no runtime detection
// ---------------------------------------------------------------------------

#[cfg(target_arch = "aarch64")]
mod arm {
    use super::Lanes;
    use core::arch::aarch64::*;

    #[derive(Clone, Copy)]
    pub(super) struct Neon(uint32x4_t);

    impl Lanes for Neon {
        #[inline(always)] unsafe fn splat(x: u32) -> Self { Neon(vdupq_n_u32(x)) }
        #[inline(always)] unsafe fn counters(b: u32) -> Self {
            const IDX: [u32; 4] = [0, 1, 2, 3];
            Neon(vaddq_u32(vdupq_n_u32(b), vld1q_u32(IDX.as_ptr())))
        }
        #[inline(always)] unsafe fn add(a: Self, b: Self) -> Self { Neon(vaddq_u32(a.0, b.0)) }
        #[inline(always)] unsafe fn xor(a: Self, b: Self) -> Self { Neon(veorq_u32(a.0, b.0)) }
        #[inline(always)] unsafe fn rotl16(a: Self) -> Self {
            Neon(vreinterpretq_u32_u16(vrev32q_u16(vreinterpretq_u16_u32(a.0))))
        }
        #[inline(always)] unsafe fn rotl12(a: Self) -> Self {
            Neon(vsriq_n_u32::<20>(vshlq_n_u32::<12>(a.0), a.0))
        }
        #[inline(always)] unsafe fn rotl8(a: Self) -> Self {
            const ROT8: [u8; 16] = [3, 0, 1, 2, 7, 4, 5, 6, 11, 8, 9, 10, 15, 12, 13, 14];
            Neon(vreinterpretq_u32_u8(vqtbl1q_u8(vreinterpretq_u8_u32(a.0), vld1q_u8(ROT8.as_ptr()))))
        }
        #[inline(always)] unsafe fn rotl7(a: Self) -> Self {
            Neon(vsriq_n_u32::<25>(vshlq_n_u32::<7>(a.0), a.0))
        }
        #[inline(always)]
        unsafe fn store(x: &[Self; 16], out: &mut [u8]) {
            debug_assert!(out.len() >= 256);
            let p = out.as_mut_ptr();
            for g in 0..4 {
                let (a, b, c, d) = (x[4 * g].0, x[4 * g + 1].0, x[4 * g + 2].0, x[4 * g + 3].0);
                let t0 = vreinterpretq_u64_u32(vzip1q_u32(a, b));
                let t1 = vreinterpretq_u64_u32(vzip1q_u32(c, d));
                let t2 = vreinterpretq_u64_u32(vzip2q_u32(a, b));
                let t3 = vreinterpretq_u64_u32(vzip2q_u32(c, d));
                let r = [vzip1q_u64(t0, t1), vzip2q_u64(t0, t1), vzip1q_u64(t2, t3), vzip2q_u64(t2, t3)];
                for (j, v) in r.iter().enumerate() {
                    vst1q_u8(p.add(64 * j + 16 * g), vreinterpretq_u8_u64(*v));
                }
            }
        }
    }

    /// Four NEON blocks and two integer-unit blocks in one instruction stream, so the
    /// vector and scalar pipes both work: blocks 0-3 in NEON, 4-5 in `Pair`.
    #[derive(Clone, Copy)]
    pub(super) struct Hybrid(Neon, super::Pair);

    impl Lanes for Hybrid {
        #[inline(always)] unsafe fn splat(x: u32) -> Self { Hybrid(Neon::splat(x), super::Pair::splat(x)) }
        #[inline(always)] unsafe fn counters(b: u32) -> Self {
            Hybrid(Neon::counters(b), super::Pair::counters(b.wrapping_add(4)))
        }
        #[inline(always)] unsafe fn add(a: Self, b: Self) -> Self { Hybrid(Neon::add(a.0, b.0), super::Pair::add(a.1, b.1)) }
        #[inline(always)] unsafe fn xor(a: Self, b: Self) -> Self { Hybrid(Neon::xor(a.0, b.0), super::Pair::xor(a.1, b.1)) }
        #[inline(always)] unsafe fn rotl16(a: Self) -> Self { Hybrid(Neon::rotl16(a.0), super::Pair::rotl16(a.1)) }
        #[inline(always)] unsafe fn rotl12(a: Self) -> Self { Hybrid(Neon::rotl12(a.0), super::Pair::rotl12(a.1)) }
        #[inline(always)] unsafe fn rotl8(a: Self) -> Self { Hybrid(Neon::rotl8(a.0), super::Pair::rotl8(a.1)) }
        #[inline(always)] unsafe fn rotl7(a: Self) -> Self { Hybrid(Neon::rotl7(a.0), super::Pair::rotl7(a.1)) }
        #[inline(always)]
        unsafe fn store(x: &[Self; 16], out: &mut [u8]) {
            debug_assert!(out.len() >= 384);
            Neon::store(&x.map(|h| h.0), &mut out[..256]);
            super::Pair::store(&x.map(|h| h.1), &mut out[256..384]);
        }
    }

    pub(super) fn neon(st: &super::State, ctr: u32, out: &mut [u8]) {
        // SAFETY: NEON is mandatory on aarch64.
        unsafe { super::blocks::<Neon>(st, ctr, out) }
    }

    pub(super) fn hybrid(st: &super::State, ctr: u32, out: &mut [u8]) {
        // SAFETY: NEON is mandatory on aarch64.
        unsafe { super::blocks::<Hybrid>(st, ctr, out) }
    }

    pub(super) fn pair(st: &super::State, ctr: u32, out: &mut [u8]) {
        // SAFETY: plain integer arithmetic.
        unsafe { super::blocks::<super::Pair>(st, ctr, out) }
    }
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Backend {
    Portable,
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    Sse2,
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    Avx2,
    /// aarch64 production policy: picks among the three below by blocks wanted.
    #[cfg(target_arch = "aarch64")]
    Neon,
    #[cfg(target_arch = "aarch64")]
    Neon4,
    #[cfg(target_arch = "aarch64")]
    Scalar2,
    #[cfg(target_arch = "aarch64")]
    Neon6,
}

/// Largest block count the two-block scalar path serves on aarch64. 0 until an
/// on-device sweep (`crypto_bench sweep`) proves a crossover.
#[cfg(target_arch = "aarch64")]
const SCALAR2_MAX_BLOCKS: usize = 0;
/// Smallest block count the NEON+scalar hybrid serves on aarch64; off until measured.
#[cfg(target_arch = "aarch64")]
const NEON6_MIN_BLOCKS: usize = usize::MAX;

impl Backend {
    #[inline]
    pub(super) fn detect() -> Self {
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        {
            if std::arch::is_x86_feature_detected!("avx2") {
                return Backend::Avx2;
            }
            if std::arch::is_x86_feature_detected!("sse2") {
                return Backend::Sse2;
            }
        }
        #[cfg(target_arch = "aarch64")]
        {
            return Backend::Neon;
        }
        #[allow(unreachable_code)]
        Backend::Portable
    }

    /// Fill `out` with keystream blocks starting at `counter`, sized to `want`
    /// blocks where the backend allows. Returns how many blocks were written.
    #[inline]
    pub(super) fn keystream(self, st: &State, counter: u32, want: usize, out: &mut [u8; BUF]) -> usize {
        match self {
            Backend::Portable => {
                let n = want.clamp(1, MAX_BLOCKS);
                for i in 0..n {
                    // SAFETY: the portable lanes are plain integer arithmetic.
                    unsafe { blocks::<u32>(st, counter.wrapping_add(i as u32), &mut out[64 * i..]) };
                }
                n
            }
            #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
            Backend::Sse2 => {
                // SAFETY: `detect` saw SSE2.
                unsafe { x86::sse2(st, counter, out) };
                4
            }
            #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
            Backend::Avx2 => {
                // SAFETY: `detect` saw AVX2.
                unsafe {
                    if want <= 4 {
                        x86::avx_rows4(st, counter, out);
                        4
                    } else {
                        x86::avx8(st, counter, out);
                        8
                    }
                }
            }
            #[cfg(target_arch = "aarch64")]
            Backend::Neon => {
                let pick = if want <= SCALAR2_MAX_BLOCKS {
                    Backend::Scalar2
                } else if want >= NEON6_MIN_BLOCKS {
                    Backend::Neon6
                } else {
                    Backend::Neon4
                };
                pick.keystream(st, counter, want, out)
            }
            #[cfg(target_arch = "aarch64")]
            Backend::Neon4 => {
                arm::neon(st, counter, out);
                4
            }
            #[cfg(target_arch = "aarch64")]
            Backend::Scalar2 => {
                arm::pair(st, counter, out);
                2
            }
            #[cfg(target_arch = "aarch64")]
            Backend::Neon6 => {
                arm::hybrid(st, counter, out);
                6
            }
        }
    }

    /// Every backend this CPU can run, by name: the tests cover each, and the bench
    /// forces each to find crossovers.
    pub(super) fn available() -> Vec<(&'static str, Backend)> {
        #[allow(unused_mut)]
        let mut v = vec![("portable", Backend::Portable)];
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        {
            if std::arch::is_x86_feature_detected!("sse2") {
                v.push(("sse2", Backend::Sse2));
            }
            if std::arch::is_x86_feature_detected!("avx2") {
                v.push(("avx2", Backend::Avx2));
            }
        }
        #[cfg(target_arch = "aarch64")]
        v.extend([
            ("neon", Backend::Neon),
            ("neon4", Backend::Neon4),
            ("scalar2", Backend::Scalar2),
            ("neon6", Backend::Neon6),
        ]);
        v
    }
}
