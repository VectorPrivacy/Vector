//! Poly1305, full blocks only: the AEAD zero-pads every segment, so the 2^128 pad
//! bit is always set and there is no partial-block path to get wrong.
//!
//! 64-bit targets use radix 2^64 (four 64×64→128 multiplies per block, native on
//! x86_64 and aarch64); 32-bit targets use radix 2^26, where 128-bit products would
//! be emulated.

#[cfg(any(test, target_pointer_width = "64"))]
pub(super) mod r64 {
    use zeroize::Zeroize;

    /// Shortest input for which the 4-lane AVX2 path makes the whole AEAD faster, measured
    /// as interleaved same-process pairs on a Cascade Lake Xeon: 704 bytes won 23 of 31
    /// twice, 512-640 was a coin flip. Poly1305 alone breaks even earlier (~448), but
    /// right after the ChaCha20 pass the lanes pay more to start.
    #[cfg(target_arch = "x86_64")]
    pub(crate) const AVX2_MIN: usize = 704;

    pub(crate) struct Poly1305 {
        r0: u64,
        r1: u64,
        h: [u64; 3],
        s: [u64; 2],
    }

    #[inline(always)]
    fn le64(b: &[u8]) -> u64 {
        u64::from_le_bytes(b[..8].try_into().unwrap())
    }

    /// `h * r`, partially reduced: the result is below 2^130 plus a few units of p.
    #[inline(always)]
    fn mul(h: [u64; 3], r0: u64, r1: u64) -> [u64; 3] {
        // r1's low two bits are clamped to zero, so r1 * 5/4 is exact.
        let s1 = r1 + (r1 >> 2);
        let [h0, h1, h2] = h;
        let d0 = (h0 as u128) * (r0 as u128) + (h1 as u128) * (s1 as u128);
        let mut d1 = (h0 as u128) * (r1 as u128) + (h1 as u128) * (r0 as u128) + (h2.wrapping_mul(s1)) as u128;
        let mut h2 = h2.wrapping_mul(r0);
        let h0 = d0 as u64;
        d1 += d0 >> 64;
        let h1 = d1 as u64;
        h2 = h2.wrapping_add((d1 >> 64) as u64);

        // Fold bits ≥ 2^130 back in as ×5.
        let c = (h2 >> 2) + (h2 & !3);
        h2 &= 3;
        let (h0, c0) = h0.overflowing_add(c);
        let (h1, c1) = h1.overflowing_add(c0 as u64);
        [h0, h1, h2 + c1 as u64]
    }

    /// Canonical `h mod p` for a partially reduced `h`, without branching on it.
    #[inline(always)]
    fn reduce(h: [u64; 3]) -> [u64; 3] {
        let [h0, h1, h2] = h;
        let t = h0 as u128 + 5;
        let g0 = t as u64;
        let t = h1 as u128 + (t >> 64);
        let g1 = t as u64;
        let g2 = h2.wrapping_add((t >> 64) as u64);
        // h + 5 - 2^130 ≥ 0 ⇔ h ≥ p.
        let mask = 0u64.wrapping_sub(g2 >> 2);
        [(h0 & !mask) | (g0 & mask), (h1 & !mask) | (g1 & mask), (h2 & !mask) | ((g2 & 3) & mask)]
    }

    impl Poly1305 {
        #[inline]
        pub(crate) fn new(key: &[u8; 32]) -> Self {
            Self {
                r0: le64(&key[0..]) & 0x0fff_fffc_0fff_ffff,
                r1: le64(&key[8..]) & 0x0fff_fffc_0fff_fffc,
                h: [0; 3],
                s: [le64(&key[16..]), le64(&key[24..])],
            }
        }

        /// Absorb `data` (length a multiple of 16) as full blocks.
        #[inline]
        pub(crate) fn blocks(&mut self, data: &[u8]) {
            debug_assert!(data.len().is_multiple_of(16));
            let (r0, r1) = (self.r0, self.r1);
            let mut h = self.h;
            for m in data.chunks_exact(16) {
                let d0 = h[0] as u128 + le64(&m[0..]) as u128;
                let d1 = h[1] as u128 + le64(&m[8..]) as u128 + (d0 >> 64);
                h = mul([d0 as u64, d1 as u64, h[2].wrapping_add((d1 >> 64) as u64 + 1)], r0, r1);
            }
            self.h = h;
        }

        /// Absorb `data` zero-padded to a 16-byte boundary.
        #[inline]
        pub(crate) fn padded(&mut self, data: &[u8]) {
            #[allow(unused_mut)]
            let mut data = data;
            #[cfg(target_arch = "x86_64")]
            if data.len() >= AVX2_MIN && std::arch::is_x86_feature_detected!("avx2") {
                let n = data.len() & !63;
                // SAFETY: AVX2 was just detected; `n` is a nonzero multiple of 64.
                unsafe { self.blocks_avx2(&data[..n]) };
                data = &data[n..];
            }
            let full = data.len() & !15;
            self.blocks(&data[..full]);
            if full != data.len() {
                let mut last = [0u8; 16];
                last[..data.len() - full].copy_from_slice(&data[full..]);
                self.blocks(&last);
            }
        }

        /// r^1..r^4 as 26-bit limbs. Partially reduced is enough: a top limb under
        /// 2^27 keeps every lane product under 2^56.
        #[cfg(target_arch = "x86_64")]
        fn powers(&self) -> [[u64; 5]; 4] {
            let r = [self.r0, self.r1, 0];
            let mut p = [r; 4];
            for i in 1..4 {
                p[i] = mul(p[i - 1], self.r0, self.r1);
            }
            let out = p.map(super::avx2::to_limbs);
            p.zeroize();
            out
        }

        /// SAFETY: requires AVX2; `data.len()` must be a nonzero multiple of 64.
        #[cfg(target_arch = "x86_64")]
        pub(crate) unsafe fn blocks_avx2(&mut self, data: &[u8]) {
            let mut pw = self.powers();
            self.h = super::avx2::blocks(self.h, &pw, data);
            pw.zeroize();
        }

        #[inline]
        pub(crate) fn finish(mut self) -> [u8; 16] {
            let [h0, h1, _] = reduce(self.h);
            let t = h0 as u128 + self.s[0] as u128;
            let o0 = t as u64;
            let o1 = (h1 as u128 + self.s[1] as u128 + (t >> 64)) as u64;

            let mut out = [0u8; 16];
            out[..8].copy_from_slice(&o0.to_le_bytes());
            out[8..].copy_from_slice(&o1.to_le_bytes());
            self.zeroize_state();
            out
        }

        #[inline]
        fn zeroize_state(&mut self) {
            self.r0.zeroize();
            self.r1.zeroize();
            self.h.zeroize();
            self.s.zeroize();
        }
    }

    impl Drop for Poly1305 {
        fn drop(&mut self) {
            self.zeroize_state();
        }
    }
}

/// Four Poly1305 lanes in radix 2^26, one 64-bit lane per block. Lane j takes blocks
/// j, j+4, … and multiplies by r^4 per step; the last step multiplies each lane by
/// the power that lands its blocks where the serial evaluation would, so the lanes
/// simply sum.
#[cfg(target_arch = "x86_64")]
pub(super) mod avx2 {
    use core::arch::x86_64::*;

    const M26: u64 = 0x3ff_ffff;

    /// A partially reduced 130-bit value to five 26-bit limbs.
    #[inline(always)]
    pub(crate) fn to_limbs(h: [u64; 3]) -> [u64; 5] {
        let [h0, h1, h2] = h;
        [h0 & M26, (h0 >> 26) & M26, ((h0 >> 52) | (h1 << 12)) & M26, (h1 >> 14) & M26, (h1 >> 40) | (h2 << 24)]
    }

    /// Five limbs (each below 2^32) back to a partially reduced radix-2^64 value.
    #[inline(always)]
    fn from_limbs(mut l: [u64; 5]) -> [u64; 3] {
        for i in 0..4 {
            l[i + 1] += l[i] >> 26;
            l[i] &= M26;
        }
        l[0] += (l[4] >> 26) * 5;
        l[4] &= M26;
        l[1] += l[0] >> 26;
        l[0] &= M26;
        let a = l[0] as u128 + ((l[1] as u128) << 26) + ((l[2] as u128) << 52);
        let b = (a >> 64) + ((l[3] as u128) << 14) + ((l[4] as u128) << 40);
        [a as u64, b as u64, (b >> 64) as u64]
    }

    #[inline]
    #[target_feature(enable = "avx2")]
    unsafe fn splat(x: u64) -> __m256i {
        _mm256_set1_epi64x(x as i64)
    }

    /// `vpmuludq`, pinned: `_mm256_mul_epu32` is spelled as masks around a 64-bit
    /// multiply, and LLVM can fold the masks away and emit three multiplies.
    #[inline]
    #[target_feature(enable = "avx2")]
    unsafe fn mul32(x: __m256i, y: __m256i) -> __m256i {
        let out: __m256i;
        core::arch::asm!(
            "vpmuludq {out}, {x}, {y}",
            out = lateout(ymm_reg) out,
            x = in(ymm_reg) x,
            y = in(ymm_reg) y,
            options(pure, nomem, nostack, preserves_flags),
        );
        out
    }

    #[inline]
    #[target_feature(enable = "avx2")]
    unsafe fn add(x: __m256i, y: __m256i) -> __m256i {
        _mm256_add_epi64(x, y)
    }

    #[inline]
    #[target_feature(enable = "avx2")]
    unsafe fn sum5(p: [__m256i; 5]) -> __m256i {
        add(add(add(p[0], p[1]), add(p[2], p[3])), p[4])
    }

    #[inline]
    #[target_feature(enable = "avx2")]
    unsafe fn carry(from: &mut __m256i, into: &mut __m256i) {
        *into = add(*into, _mm256_srli_epi64(*from, 26));
        *from = _mm256_and_si256(*from, splat(M26));
    }

    /// `a * r` per lane. Products stay below 2^59; the two interleaved carry chains
    /// leave every limb under 2^26 + 2^7, inside the 32 bits the next multiply reads.
    #[inline]
    #[target_feature(enable = "avx2")]
    unsafe fn mul(a: &mut [__m256i; 5], r: &[__m256i; 5], s: &[__m256i; 5]) {
        let [a0, a1, a2, a3, a4] = *a;
        let [mut d0, mut d1, mut d2, mut d3, mut d4] = [
            sum5([mul32(a0, r[0]), mul32(a1, s[4]), mul32(a2, s[3]), mul32(a3, s[2]), mul32(a4, s[1])]),
            sum5([mul32(a0, r[1]), mul32(a1, r[0]), mul32(a2, s[4]), mul32(a3, s[3]), mul32(a4, s[2])]),
            sum5([mul32(a0, r[2]), mul32(a1, r[1]), mul32(a2, r[0]), mul32(a3, s[4]), mul32(a4, s[3])]),
            sum5([mul32(a0, r[3]), mul32(a1, r[2]), mul32(a2, r[1]), mul32(a3, r[0]), mul32(a4, s[4])]),
            sum5([mul32(a0, r[4]), mul32(a1, r[3]), mul32(a2, r[2]), mul32(a3, r[1]), mul32(a4, r[0])]),
        ];
        carry(&mut d0, &mut d1);
        carry(&mut d3, &mut d4);
        carry(&mut d1, &mut d2);
        // d4's carry wraps to d0 as ×5.
        let c = _mm256_srli_epi64(d4, 26);
        d4 = _mm256_and_si256(d4, splat(M26));
        d0 = add(d0, add(c, _mm256_slli_epi64(c, 2)));
        carry(&mut d2, &mut d3);
        carry(&mut d0, &mut d1);
        carry(&mut d3, &mut d4);
        *a = [d0, d1, d2, d3, d4];
    }

    /// Four blocks as limbs. The unpack leaves lanes holding blocks 0, 2, 1, 3.
    #[inline]
    #[target_feature(enable = "avx2")]
    unsafe fn load4(p: *const u8) -> [__m256i; 5] {
        let v0 = _mm256_loadu_si256(p as *const __m256i);
        let v1 = _mm256_loadu_si256(p.add(32) as *const __m256i);
        let lo = _mm256_unpacklo_epi64(v0, v1);
        let hi = _mm256_unpackhi_epi64(v0, v1);
        let mask = splat(M26);
        [
            _mm256_and_si256(lo, mask),
            _mm256_and_si256(_mm256_srli_epi64(lo, 26), mask),
            _mm256_and_si256(_mm256_or_si256(_mm256_srli_epi64(lo, 52), _mm256_slli_epi64(hi, 12)), mask),
            _mm256_and_si256(_mm256_srli_epi64(hi, 14), mask),
            _mm256_or_si256(_mm256_srli_epi64(hi, 40), splat(1 << 24)),
        ]
    }

    /// Absorb `data` (a nonzero multiple of 64 bytes) into `h`; `pw` is r^1..r^4 as limbs.
    #[target_feature(enable = "avx2")]
    pub(crate) unsafe fn blocks(h: [u64; 3], pw: &[[u64; 5]; 4], data: &[u8]) -> [u64; 3] {
        debug_assert!(!data.is_empty() && data.len().is_multiple_of(64));
        let [r1, r2, r3, r4] = pw;
        let five = |x: __m256i| _mm256_add_epi64(x, _mm256_slli_epi64(x, 2));
        let step_r: [__m256i; 5] = core::array::from_fn(|i| splat(r4[i]));
        let step_s = step_r.map(five);
        // Lanes hold blocks 0, 2, 1, 3 of each group of four.
        let last_r: [__m256i; 5] = core::array::from_fn(|i| {
            _mm256_setr_epi64x(r4[i] as i64, r2[i] as i64, r3[i] as i64, r1[i] as i64)
        });
        let last_s = last_r.map(five);

        let hl = to_limbs(h);
        let mut a: [__m256i; 5] = core::array::from_fn(|i| _mm256_setr_epi64x(hl[i] as i64, 0, 0, 0));
        let (body, last) = data.split_at(data.len() - 64);
        for group in body.chunks_exact(64) {
            let m = load4(group.as_ptr());
            for i in 0..5 {
                a[i] = add(a[i], m[i]);
            }
            mul(&mut a, &step_r, &step_s);
        }
        let m = load4(last.as_ptr());
        for i in 0..5 {
            a[i] = add(a[i], m[i]);
        }
        mul(&mut a, &last_r, &last_s);

        let mut lanes = [[0u64; 4]; 5];
        for i in 0..5 {
            _mm256_storeu_si256(lanes[i].as_mut_ptr() as *mut __m256i, a[i]);
        }
        let out = from_limbs(lanes.map(|l| l.iter().sum()));
        zeroize::Zeroize::zeroize(&mut lanes);
        out
    }
}

#[cfg(any(test, not(target_pointer_width = "64")))]
pub(super) mod r26 {
    use zeroize::Zeroize;

    const M26: u32 = 0x3ff_ffff;

    pub(crate) struct Poly1305 {
        r: [u32; 5],
        h: [u32; 5],
        s: [u32; 4],
    }

    #[inline(always)]
    fn le32(b: &[u8]) -> u32 {
        u32::from_le_bytes(b[..4].try_into().unwrap())
    }

    impl Poly1305 {
        #[inline]
        pub(crate) fn new(key: &[u8; 32]) -> Self {
            Self {
                r: [
                    le32(&key[0..]) & 0x3ff_ffff,
                    (le32(&key[3..]) >> 2) & 0x3ff_ff03,
                    (le32(&key[6..]) >> 4) & 0x3ff_c0ff,
                    (le32(&key[9..]) >> 6) & 0x3f0_3fff,
                    (le32(&key[12..]) >> 8) & 0x00f_ffff,
                ],
                h: [0; 5],
                s: [le32(&key[16..]), le32(&key[20..]), le32(&key[24..]), le32(&key[28..])],
            }
        }

        #[inline]
        pub(crate) fn blocks(&mut self, data: &[u8]) {
            debug_assert!(data.len().is_multiple_of(16));
            let [r0, r1, r2, r3, r4] = self.r.map(|x| x as u64);
            let (s1, s2, s3, s4) = (r1 * 5, r2 * 5, r3 * 5, r4 * 5);
            let [mut h0, mut h1, mut h2, mut h3, mut h4] = self.h;

            for m in data.chunks_exact(16) {
                h0 += le32(&m[0..]) & M26;
                h1 += (le32(&m[3..]) >> 2) & M26;
                h2 += (le32(&m[6..]) >> 4) & M26;
                h3 += (le32(&m[9..]) >> 6) & M26;
                h4 += (le32(&m[12..]) >> 8) | (1 << 24);

                let (a0, a1, a2, a3, a4) = (h0 as u64, h1 as u64, h2 as u64, h3 as u64, h4 as u64);
                let d0 = a0 * r0 + a1 * s4 + a2 * s3 + a3 * s2 + a4 * s1;
                let mut d1 = a0 * r1 + a1 * r0 + a2 * s4 + a3 * s3 + a4 * s2;
                let mut d2 = a0 * r2 + a1 * r1 + a2 * r0 + a3 * s4 + a4 * s3;
                let mut d3 = a0 * r3 + a1 * r2 + a2 * r1 + a3 * r0 + a4 * s4;
                let mut d4 = a0 * r4 + a1 * r3 + a2 * r2 + a3 * r1 + a4 * r0;

                h0 = d0 as u32 & M26;
                d1 += d0 >> 26;
                h1 = d1 as u32 & M26;
                d2 += d1 >> 26;
                h2 = d2 as u32 & M26;
                d3 += d2 >> 26;
                h3 = d3 as u32 & M26;
                d4 += d3 >> 26;
                h4 = d4 as u32 & M26;
                h0 += (d4 >> 26) as u32 * 5;
                h1 += h0 >> 26;
                h0 &= M26;
            }
            self.h = [h0, h1, h2, h3, h4];
        }

        #[inline]
        pub(crate) fn padded(&mut self, data: &[u8]) {
            let full = data.len() & !15;
            self.blocks(&data[..full]);
            if full != data.len() {
                let mut last = [0u8; 16];
                last[..data.len() - full].copy_from_slice(&data[full..]);
                self.blocks(&last);
            }
        }

        #[inline]
        pub(crate) fn finish(mut self) -> [u8; 16] {
            let [mut h0, mut h1, mut h2, mut h3, mut h4] = self.h;
            h2 += h1 >> 26; h1 &= M26;
            h3 += h2 >> 26; h2 &= M26;
            h4 += h3 >> 26; h3 &= M26;
            h0 += (h4 >> 26) * 5; h4 &= M26;
            h1 += h0 >> 26; h0 &= M26;

            // g = h + 5 - 2^130; keep it when it did not borrow, i.e. h ≥ p.
            let mut g0 = h0 + 5;
            let mut g1 = h1 + (g0 >> 26); g0 &= M26;
            let mut g2 = h2 + (g1 >> 26); g1 &= M26;
            let mut g3 = h3 + (g2 >> 26); g2 &= M26;
            let g4 = (h4 + (g3 >> 26)).wrapping_sub(1 << 26); g3 &= M26;
            let keep_g = (g4 >> 31).wrapping_sub(1);
            h0 = (h0 & !keep_g) | (g0 & keep_g);
            h1 = (h1 & !keep_g) | (g1 & keep_g);
            h2 = (h2 & !keep_g) | (g2 & keep_g);
            h3 = (h3 & !keep_g) | (g3 & keep_g);
            h4 = (h4 & !keep_g) | (g4 & keep_g);

            let w = [
                h0 | (h1 << 26),
                (h1 >> 6) | (h2 << 20),
                (h2 >> 12) | (h3 << 14),
                (h3 >> 18) | (h4 << 8),
            ];
            let mut out = [0u8; 16];
            let mut carry = 0u64;
            for i in 0..4 {
                let f = w[i] as u64 + self.s[i] as u64 + carry;
                out[4 * i..4 * i + 4].copy_from_slice(&(f as u32).to_le_bytes());
                carry = f >> 32;
            }
            self.zeroize_state();
            out
        }

        #[inline]
        fn zeroize_state(&mut self) {
            self.r.zeroize();
            self.h.zeroize();
            self.s.zeroize();
        }
    }

    impl Drop for Poly1305 {
        fn drop(&mut self) {
            self.zeroize_state();
        }
    }
}

#[cfg(target_pointer_width = "64")]
pub(super) use r64::Poly1305;
#[cfg(not(target_pointer_width = "64"))]
pub(super) use r26::Poly1305;
