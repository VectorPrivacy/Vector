//! AES-256-GCM on AES-NI and PCLMULQDQ, stitched: each pass over eight blocks runs
//! the AES rounds and the GHASH multiplies together, so the AES and carry-less
//! multiply units work at once instead of in two passes over the data.
//!
//! GHASH is computed as POLYVAL over byte-reversed blocks (RFC 8452, Appendix A),
//! using the R/F multiplication of "Efficient GHASH Implementation Using CLMUL"
//! (eprint 2025/2171): four carry-less multiplies per block and one per reduction,
//! so eight aggregated blocks cost 33.

use core::arch::x86_64::*;

/// x^63 + x^62 + x^57: the low half of POLYVAL's reduction constant.
const P1: u64 = 0xC200_0000_0000_0000;

/// The stitched path's instructions are present, and the CPU is one it was measured
/// on: with VAES the `aes` crate has a wider path that has not been compared yet.
pub(super) fn available() -> bool {
    std::arch::is_x86_feature_detected!("aes")
        && std::arch::is_x86_feature_detected!("pclmulqdq")
        && std::arch::is_x86_feature_detected!("ssse3")
        && std::arch::is_x86_feature_detected!("sse4.1")
        && !std::arch::is_x86_feature_detected!("vaes")
}

/// The instructions alone, for tests and the bench.
pub(super) fn instructions_present() -> bool {
    std::arch::is_x86_feature_detected!("aes")
        && std::arch::is_x86_feature_detected!("pclmulqdq")
        && std::arch::is_x86_feature_detected!("ssse3")
        && std::arch::is_x86_feature_detected!("sse4.1")
}

/// Expanded AES-256 key and GHASH powers H^1..H^8 (with their R/F companions).
pub(super) struct Keys {
    rk: [__m128i; 15],
    h: [__m128i; 8],
    d: [__m128i; 8],
}

impl Drop for Keys {
    fn drop(&mut self) {
        // SAFETY: plain-old-data registers; zeros are a valid value.
        unsafe {
            for v in self.rk.iter_mut().chain(self.h.iter_mut()).chain(self.d.iter_mut()) {
                core::ptr::write_volatile(v, _mm_setzero_si128());
            }
        }
        core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
    }
}

#[inline(always)]
unsafe fn xor(a: __m128i, b: __m128i) -> __m128i {
    _mm_xor_si128(a, b)
}

#[inline(always)]
unsafe fn bswap(x: __m128i) -> __m128i {
    _mm_shuffle_epi8(x, _mm_set_epi8(0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15))
}

#[inline(always)]
unsafe fn load(p: *const u8) -> __m128i {
    _mm_loadu_si128(p as *const __m128i)
}

#[inline(always)]
unsafe fn store(p: *mut u8, v: __m128i) {
    _mm_storeu_si128(p as *mut __m128i, v)
}

// ---------------------------------------------------------------------------
// AES-256
// ---------------------------------------------------------------------------

#[inline(always)]
unsafe fn assist1(mut t1: __m128i, t2: __m128i) -> __m128i {
    let t2 = _mm_shuffle_epi32(t2, 0xff);
    let mut t4 = _mm_slli_si128(t1, 4);
    t1 = xor(t1, t4);
    t4 = _mm_slli_si128(t4, 4);
    t1 = xor(t1, t4);
    t4 = _mm_slli_si128(t4, 4);
    xor(xor(t1, t4), t2)
}

#[inline(always)]
unsafe fn assist2(t1: __m128i, mut t3: __m128i) -> __m128i {
    let t2 = _mm_shuffle_epi32(_mm_aeskeygenassist_si128::<0>(t1), 0xaa);
    let mut t4 = _mm_slli_si128(t3, 4);
    t3 = xor(t3, t4);
    t4 = _mm_slli_si128(t4, 4);
    t3 = xor(t3, t4);
    t4 = _mm_slli_si128(t4, 4);
    xor(xor(t3, t4), t2)
}

#[inline(always)]
unsafe fn expand(key: &[u8; 32]) -> [__m128i; 15] {
    let mut rk = [_mm_setzero_si128(); 15];
    let mut t1 = load(key.as_ptr());
    let mut t3 = load(key.as_ptr().add(16));
    rk[0] = t1;
    rk[1] = t3;
    macro_rules! round {
        ($i:expr, $rcon:expr) => {
            t1 = assist1(t1, _mm_aeskeygenassist_si128::<$rcon>(t3));
            rk[$i] = t1;
            t3 = assist2(t1, t3);
            rk[$i + 1] = t3;
        };
    }
    round!(2, 0x01);
    round!(4, 0x02);
    round!(6, 0x04);
    round!(8, 0x08);
    round!(10, 0x10);
    round!(12, 0x20);
    rk[14] = assist1(t1, _mm_aeskeygenassist_si128::<0x40>(t3));
    rk
}

#[inline(always)]
unsafe fn encrypt1(rk: &[__m128i; 15], block: __m128i) -> __m128i {
    let mut x = xor(block, rk[0]);
    for k in &rk[1..14] {
        x = _mm_aesenc_si128(x, *k);
    }
    _mm_aesenclast_si128(x, rk[14])
}

// ---------------------------------------------------------------------------
// POLYVAL (R/F)
// ---------------------------------------------------------------------------

#[inline(always)]
unsafe fn companion(h: __m128i) -> __m128i {
    let p = _mm_set_epi64x(P1 as i64, 0);
    xor(_mm_shuffle_epi32(h, 0x4e), _mm_clmulepi64_si128(h, p, 0x10))
}

/// Unreduced `m * h` as the R and F halves.
#[inline(always)]
unsafe fn rf(m: __m128i, h: __m128i, d: __m128i) -> (__m128i, __m128i) {
    let r = xor(_mm_clmulepi64_si128(m, d, 0x10), _mm_clmulepi64_si128(m, h, 0x11));
    let f = xor(_mm_clmulepi64_si128(m, d, 0x00), _mm_clmulepi64_si128(m, h, 0x01));
    (r, f)
}

#[inline(always)]
unsafe fn reduce(r: __m128i, f: __m128i) -> __m128i {
    let p1 = _mm_set_epi64x(0, P1 as i64);
    let x = xor(r, _mm_srli_si128(f, 8));
    let x = xor(x, _mm_slli_si128(f, 8));
    xor(x, _mm_clmulepi64_si128(f, p1, 0x00))
}

#[inline(always)]
unsafe fn mul(m: __m128i, h: __m128i, d: __m128i) -> __m128i {
    let (r, f) = rf(m, h, d);
    reduce(r, f)
}

/// `acc = (acc ^ x) * H` for one byte-reversed block.
#[inline(always)]
unsafe fn ghash1(k: &Keys, acc: __m128i, x: __m128i) -> __m128i {
    mul(xor(acc, x), k.h[0], k.d[0])
}

/// Eight byte-reversed blocks with one reduction: `x[0]` meets `acc` and H^8,
/// `x[7]` meets H^1.
#[inline(always)]
unsafe fn ghash8(k: &Keys, acc: __m128i, x: &[__m128i; 8]) -> __m128i {
    let (mut r, mut f) = rf(xor(acc, x[0]), k.h[7], k.d[7]);
    for (i, xi) in x.iter().enumerate().skip(1) {
        let (ri, fi) = rf(*xi, k.h[7 - i], k.d[7 - i]);
        r = xor(r, ri);
        f = xor(f, fi);
    }
    reduce(r, f)
}

/// POLYVAL's mulX, the key conversion GHASH-as-POLYVAL needs.
fn mulx(v: u128) -> u128 {
    let hi = v >> 127;
    (v << 1) ^ hi ^ (hi << 127) ^ (hi << 126) ^ (hi << 121)
}

// ---------------------------------------------------------------------------
// GCM
// ---------------------------------------------------------------------------

/// Stream state: counter base, next counter, GHASH accumulator, and E(J0).
pub(super) struct Gcm {
    keys: Keys,
    j0: __m128i,
    counter: u32,
    acc: __m128i,
    tag_mask: __m128i,
}

impl Drop for Gcm {
    fn drop(&mut self) {
        // SAFETY: plain-old-data registers; zeros are a valid value.
        unsafe {
            for v in [&mut self.acc, &mut self.tag_mask, &mut self.j0] {
                core::ptr::write_volatile(v, _mm_setzero_si128());
            }
        }
    }
}

impl Gcm {
    /// SAFETY: requires `available()`.
    #[target_feature(enable = "aes,pclmulqdq,sse2,ssse3,sse4.1")]
    pub(super) unsafe fn new(key: &[u8; 32], nonce: &[u8; 16]) -> Self {
        let rk = expand(key);
        let mut raw = [0u8; 16];
        store(raw.as_mut_ptr(), encrypt1(&rk, _mm_setzero_si128()));
        raw.reverse();
        let mut h1 = mulx(u128::from_le_bytes(raw)).to_le_bytes();
        crate::crypto::wipe(&mut raw);
        // H^2..H^8 as a tree of depth three rather than a chain of seven.
        let mut h = [load(h1.as_ptr()); 8];
        let mut d = [companion(h[0]); 8];
        for (hi, lo) in [(1, 0), (2, 1), (3, 1), (4, 3), (5, 3), (6, 3), (7, 3)] {
            // H^(hi+1) = H^(hi-lo) * H^(lo+1)
            h[hi] = mul(h[hi - lo - 1], h[lo], d[lo]);
            d[hi] = companion(h[hi]);
        }
        crate::crypto::wipe(&mut h1);
        let keys = Keys { rk, h, d };

        // A 16-byte nonce is not the 96-bit fast case: J0 = GHASH(IV || 0^64 || [128]_64).
        let mut j = ghash1(&keys, _mm_setzero_si128(), bswap(load(nonce.as_ptr())));
        let mut lens = [0u8; 16];
        lens[8..].copy_from_slice(&128u64.to_be_bytes());
        j = ghash1(&keys, j, bswap(load(lens.as_ptr())));
        let j0 = bswap(j);
        let counter = (_mm_extract_epi32::<3>(j0) as u32).swap_bytes();
        let tag_mask = encrypt1(&keys.rk, j0);
        Gcm { keys, j0, counter: counter.wrapping_add(1), acc: _mm_setzero_si128(), tag_mask }
    }

    /// Restart the keystream at `block` (a whole counter block), for testing the wrap.
    #[cfg(test)]
    #[target_feature(enable = "sse2,sse4.1")]
    pub(super) unsafe fn seek(&mut self, block: [u8; 16]) {
        self.j0 = load(block.as_ptr());
        self.counter = u32::from_be_bytes(block[12..].try_into().unwrap());
    }

    #[inline(always)]
    unsafe fn counter_block(&self, c: u32) -> __m128i {
        _mm_insert_epi32::<3>(self.j0, c.swap_bytes() as i32)
    }

    /// Eight keystream blocks for counters `c..c+8`, XORed into `out`'s blocks.
    #[inline(always)]
    unsafe fn ctr8(&self, c: u32) -> [__m128i; 8] {
        let rk = &self.keys.rk;
        let mut b: [__m128i; 8] = core::array::from_fn(|i| xor(self.counter_block(c.wrapping_add(i as u32)), rk[0]));
        for k in &rk[1..14] {
            for x in b.iter_mut() {
                *x = _mm_aesenc_si128(*x, *k);
            }
        }
        for x in b.iter_mut() {
            *x = _mm_aesenclast_si128(*x, rk[14]);
        }
        b
    }

    /// Eight keystream blocks from counter `c`, with the GHASH of the eight ciphertext
    /// blocks at `hashed` folded into the accumulator between the AES rounds: after each
    /// of the first eight rounds, one block's carry-less multiplies; after the ninth, the
    /// reduction. Interleaving in program order keeps both units busy.
    #[inline(always)]
    unsafe fn stitched8(&mut self, c: u32, hashed: *const u8) -> [__m128i; 8] {
        let rk = &self.keys.rk;
        let mut b: [__m128i; 8] = core::array::from_fn(|i| xor(self.counter_block(c.wrapping_add(i as u32)), rk[0]));
        let (mut r, mut f) = (_mm_setzero_si128(), _mm_setzero_si128());
        for (round, k) in rk.iter().enumerate().take(14).skip(1) {
            for x in b.iter_mut() {
                *x = _mm_aesenc_si128(*x, *k);
            }
            if round <= 8 {
                let i = round - 1;
                let mut m = bswap(load(hashed.add(16 * i)));
                if i == 0 {
                    m = xor(m, self.acc);
                }
                let (ri, fi) = rf(m, self.keys.h[7 - i], self.keys.d[7 - i]);
                r = xor(r, ri);
                f = xor(f, fi);
            } else if round == 9 {
                self.acc = reduce(r, f);
            }
        }
        for x in b.iter_mut() {
            *x = _mm_aesenclast_si128(*x, rk[14]);
        }
        b
    }

    /// Encrypt `data` in place; its length must be a multiple of 16 unless this is the
    /// last call before `finish`.
    #[target_feature(enable = "aes,pclmulqdq,sse2,ssse3,sse4.1")]
    pub(super) unsafe fn encrypt(&mut self, data: &mut [u8]) {
        let groups = data.len() / 128;
        let p = data.as_mut_ptr();
        // GHASH trails one group behind the AES, reading back the ciphertext it just
        // stored, so the two are independent within an iteration and hold no extra
        // registers across it.
        for g in 0..groups {
            let base = p.add(128 * g);
            let ks = if g > 0 {
                self.stitched8(self.counter, p.add(128 * (g - 1)))
            } else {
                self.ctr8(self.counter)
            };
            self.counter = self.counter.wrapping_add(8);
            for (i, k) in ks.iter().enumerate() {
                store(base.add(16 * i), xor(load(base.add(16 * i)), *k));
            }
        }
        if groups > 0 {
            let prev = p.add(128 * (groups - 1));
            self.acc = ghash8(&self.keys, self.acc, &core::array::from_fn(|i| bswap(load(prev.add(16 * i)))));
        }
        self.tail(&mut data[128 * groups..], true);
    }

    /// Decrypt `data` in place; same length rule as `encrypt`.
    #[target_feature(enable = "aes,pclmulqdq,sse2,ssse3,sse4.1")]
    pub(super) unsafe fn decrypt(&mut self, data: &mut [u8]) {
        let groups = data.len() / 128;
        let p = data.as_mut_ptr();
        for g in 0..groups {
            let base = p.add(128 * g);
            // The ciphertext is known up front, so it is hashed while its own keystream
            // is computed.
            let ks = self.stitched8(self.counter, base);
            self.counter = self.counter.wrapping_add(8);
            for (i, k) in ks.iter().enumerate() {
                store(base.add(16 * i), xor(load(base.add(16 * i)), *k));
            }
        }
        self.tail(&mut data[128 * groups..], false);
    }

    /// Fewer than eight blocks, the last possibly partial, one at a time.
    #[inline(always)]
    unsafe fn tail(&mut self, data: &mut [u8], encrypting: bool) {
        for block in data.chunks_mut(16) {
            let ks = encrypt1(&self.keys.rk, self.counter_block(self.counter));
            self.counter = self.counter.wrapping_add(1);
            let mut buf = [0u8; 16];
            buf[..block.len()].copy_from_slice(block);
            let input = load(buf.as_ptr());
            let out = xor(input, ks);
            store(buf.as_mut_ptr(), out);
            // Zero the keystream past the data so the hashed ciphertext is zero-padded.
            buf[block.len()..].fill(0);
            let ct = if encrypting { load(buf.as_ptr()) } else { input };
            self.acc = ghash1(&self.keys, self.acc, bswap(ct));
            block.copy_from_slice(&buf[..block.len()]);
        }
    }

    /// The tag over `len` bytes of ciphertext and no associated data.
    #[target_feature(enable = "aes,pclmulqdq,sse2,ssse3,sse4.1")]
    pub(super) unsafe fn finish(mut self, len: u64) -> [u8; 16] {
        let mut lens = [0u8; 16];
        lens[8..].copy_from_slice(&(len * 8).to_be_bytes());
        self.acc = ghash1(&self.keys, self.acc, bswap(load(lens.as_ptr())));
        let mut tag = [0u8; 16];
        store(tag.as_mut_ptr(), xor(bswap(self.acc), self.tag_mask));
        tag
    }
}

