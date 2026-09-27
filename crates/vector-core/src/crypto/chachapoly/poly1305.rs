//! Poly1305, full blocks only: the AEAD zero-pads every segment, so the 2^128 pad
//! bit is always set and there is no partial-block path to get wrong.
//!
//! 64-bit targets use radix 2^64 (four 64×64→128 multiplies per block, native on
//! x86_64 and aarch64); 32-bit targets use radix 2^26, where 128-bit products would
//! be emulated.

#[cfg(any(test, target_pointer_width = "64"))]
pub(super) mod r64 {
    use zeroize::Zeroize;

    pub(crate) struct Poly1305 {
        r0: u64,
        r1: u64,
        h0: u64,
        h1: u64,
        h2: u64,
        s: [u64; 2],
    }

    #[inline(always)]
    fn le64(b: &[u8]) -> u64 {
        u64::from_le_bytes(b[..8].try_into().unwrap())
    }

    impl Poly1305 {
        #[inline]
        pub(crate) fn new(key: &[u8; 32]) -> Self {
            Self {
                r0: le64(&key[0..]) & 0x0fff_fffc_0fff_ffff,
                r1: le64(&key[8..]) & 0x0fff_fffc_0fff_fffc,
                h0: 0,
                h1: 0,
                h2: 0,
                s: [le64(&key[16..]), le64(&key[24..])],
            }
        }

        /// Absorb `data` (length a multiple of 16) as full blocks.
        #[inline]
        pub(crate) fn blocks(&mut self, data: &[u8]) {
            debug_assert!(data.len().is_multiple_of(16));
            let (r0, r1) = (self.r0, self.r1);
            // r1's low two bits are clamped to zero, so r1 * 5/4 is exact.
            let s1 = r1 + (r1 >> 2);
            let (mut h0, mut h1, mut h2) = (self.h0, self.h1, self.h2);

            for m in data.chunks_exact(16) {
                let d0 = h0 as u128 + le64(&m[0..]) as u128;
                h0 = d0 as u64;
                let d1 = h1 as u128 + le64(&m[8..]) as u128 + (d0 >> 64);
                h1 = d1 as u64;
                h2 = h2.wrapping_add((d1 >> 64) as u64 + 1);

                let d0 = (h0 as u128) * (r0 as u128) + (h1 as u128) * (s1 as u128);
                let mut d1 = (h0 as u128) * (r1 as u128)
                    + (h1 as u128) * (r0 as u128)
                    + (h2.wrapping_mul(s1)) as u128;
                h2 = h2.wrapping_mul(r0);

                h0 = d0 as u64;
                d1 += d0 >> 64;
                h1 = d1 as u64;
                h2 = h2.wrapping_add((d1 >> 64) as u64);

                // Fold bits ≥ 2^130 back in as ×5.
                let c = (h2 >> 2) + (h2 & !3);
                h2 &= 3;
                let (t0, c0) = h0.overflowing_add(c);
                h0 = t0;
                let (t1, c1) = h1.overflowing_add(c0 as u64);
                h1 = t1;
                h2 += c1 as u64;
            }

            self.h0 = h0;
            self.h1 = h1;
            self.h2 = h2;
        }

        /// Absorb `data` zero-padded to a 16-byte boundary.
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
            let (h0, h1, h2) = (self.h0, self.h1, self.h2);
            // h + 5 - 2^130 ≥ 0 ⇔ h ≥ p; select without branching.
            let t = h0 as u128 + 5;
            let g0 = t as u64;
            let t = h1 as u128 + (t >> 64);
            let g1 = t as u64;
            let g2 = h2.wrapping_add((t >> 64) as u64);
            let mask = 0u64.wrapping_sub(g2 >> 2);
            let h0 = (h0 & !mask) | (g0 & mask);
            let h1 = (h1 & !mask) | (g1 & mask);

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
            self.h0.zeroize();
            self.h1.zeroize();
            self.h2.zeroize();
            self.s.zeroize();
        }
    }

    impl Drop for Poly1305 {
        fn drop(&mut self) {
            self.zeroize_state();
        }
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
