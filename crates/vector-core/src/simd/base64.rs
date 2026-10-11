//! Base64 (RFC 4648) encode and decode: NEON on ARM64, AVX2/SSSE3 on x86_64, scalar elsewhere.
//!
//! Decoding is strict: no whitespace, padding exactly as the engine says, and the unused bits of
//! the last group must be zero, so every byte string has one accepted encoding.
//!
//! # Performance (x86_64 AVX2, `encode_to_string` / `decode_to_vec`, allocation included)
//!
//! | bytes | encode | decode | decode, base64-simd 0.8 |
//! |------:|-------:|-------:|------------------------:|
//! |    48 |  20 ns |  17 ns |                   25 ns |
//! |   600 |  71 ns |  55 ns |                   93 ns |
//! |  64 K | 6.2 µs | 4.9 µs |                  8.5 µs |
//!
//! Encoding is level with base64-simd from 32 bytes up.
//!
//! # Algorithm
//!
//! **NEON:** `vld3`/`vst4` de-interleave 48 bytes into 64 indices, and a 64-entry `tbl4` maps
//! them to the alphabet. Decoding runs the same in reverse over a 128-entry `tbl4`/`tbx4` table.
//!
//! **SSSE3/AVX2:** Muła and Lemire's multiply-shift split and `pshufb` offset lookup. Decode
//! validation classifies each byte by nibble pair (`lut_lo[lo] & lut_hi[hi]`), with tables built
//! at compile time from the alphabet, so the URL-safe engine uses the same code.

#[cfg(target_arch = "aarch64")]
use std::arch::aarch64::*;

#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::*;

const STANDARD_CHARSET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const URL_SAFE_CHARSET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// The standard alphabet, padded.
pub const STANDARD: Engine = Engine::new(STANDARD_CHARSET, true);
/// The URL-safe alphabet, unpadded.
pub const URL_SAFE_NO_PAD: Engine = Engine::new(URL_SAFE_CHARSET, false);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodeError;

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("invalid base64")
    }
}

impl std::error::Error for DecodeError {}

pub struct Engine {
    charset: &'static [u8; 64],
    /// Byte to 6-bit value; 0xff for anything outside the alphabet.
    decode: [u8; 256],
    pad: bool,
    #[allow(dead_code)]
    x86: X86Luts,
}

/// The SSSE3/AVX2 tables, as plain bytes so `Engine` stays const-constructible.
#[allow(dead_code)]
struct X86Luts {
    /// Index-to-ASCII offsets for encoding, by `saturating_sub(idx, 51)` (13 for A-Z).
    encode_shift: [u8; 16],
    /// Bit classes: a byte is invalid iff `lo[byte & 15] & hi[byte >> 4] != 0`.
    lo: [u8; 16],
    hi: [u8; 16],
    /// ASCII-to-value offsets by high nibble; slot 1 (never valid) holds `special`'s.
    roll: [u8; 16],
    /// The one character whose offset differs from the rest of its high-nibble row.
    special: u8,
    /// Added to `special`'s high nibble to reach slot 1.
    delta: u8,
}

const ROLL_SLOT: u8 = 1;

impl Engine {
    const fn new(charset: &'static [u8; 64], pad: bool) -> Engine {
        let mut decode = [0xffu8; 256];
        let mut i = 0;
        while i < 64 {
            decode[charset[i] as usize] = i as u8;
            i += 1;
        }
        Engine { charset, decode, pad, x86: X86Luts::new(charset, &decode) }
    }

    /// The encoded length of `n` bytes.
    pub const fn encoded_len(&self, n: usize) -> usize {
        if self.pad {
            n.div_ceil(3) * 4
        } else {
            n / 3 * 4 + [0, 2, 3][n % 3]
        }
    }

    pub fn encode_to_string(&self, data: impl AsRef<[u8]>) -> String {
        let mut out = String::new();
        self.encode_append(data, &mut out);
        out
    }

    pub fn encode_append(&self, data: impl AsRef<[u8]>, out: &mut String) {
        let src = data.as_ref();
        let len = self.encoded_len(src.len());
        // SAFETY: the encoder writes exactly `len` bytes, all from the ASCII alphabet or '='.
        unsafe {
            let v = out.as_mut_vec();
            v.reserve(len);
            self.encode_raw(src, v.as_mut_ptr().add(v.len()), Level::best());
            v.set_len(v.len() + len);
        }
    }

    pub fn decode_to_vec(&self, data: impl AsRef<[u8]>) -> Result<Vec<u8>, DecodeError> {
        self.decode_with(data.as_ref(), Level::best())
    }

    fn decode_with(&self, src: &[u8], level: Level) -> Result<Vec<u8>, DecodeError> {
        let (n, m) = self.decoded_len(src)?;
        // Slack for the 8-byte scalar and the 16/32-byte SIMD stores, which overrun what they produce.
        let mut out = Vec::with_capacity(m + 8);
        // SAFETY: `decode_raw` writes `m` bytes and stays within `m + 8`.
        unsafe {
            self.decode_raw(&src[..n], out.as_mut_ptr(), level)?;
            out.set_len(m);
        }
        Ok(out)
    }

    /// (characters before padding, decoded bytes).
    fn decoded_len(&self, src: &[u8]) -> Result<(usize, usize), DecodeError> {
        let len = src.len();
        if len == 0 {
            return Ok((0, 0));
        }
        let n = if self.pad {
            if len % 4 != 0 {
                return Err(DecodeError);
            }
            len - (src[len - 1] == b'=') as usize - (src[len - 1] == b'=' && src[len - 2] == b'=') as usize
        } else {
            len
        };
        let m = match n % 4 {
            0 => n / 4 * 3,
            2 => n / 4 * 3 + 1,
            3 => n / 4 * 3 + 2,
            _ => return Err(DecodeError),
        };
        Ok((n, m))
    }

    /// # Safety
    /// `dst` must have room for `self.encoded_len(src.len())` bytes.
    unsafe fn encode_raw(&self, src: &[u8], dst: *mut u8, level: Level) {
        let (i, o) = match level {
            #[cfg(target_arch = "aarch64")]
            Level::Neon => encode_neon(src, dst, self.charset),
            #[cfg(target_arch = "x86_64")]
            Level::Avx2 => encode_avx2(src, dst, &self.x86.encode_shift),
            #[cfg(target_arch = "x86_64")]
            Level::Ssse3 => encode_ssse3(src, dst, &self.x86.encode_shift),
            Level::Scalar => (0, 0),
        };
        self.encode_scalar(&src[i..], dst.add(o));
    }

    unsafe fn encode_scalar(&self, src: &[u8], dst: *mut u8) {
        let cs = self.charset;
        let mut o = 0;
        let mut chunks = src.chunks_exact(3);
        for c in &mut chunks {
            let w = (c[0] as u32) << 16 | (c[1] as u32) << 8 | c[2] as u32;
            let out = u32::from_le_bytes([
                *cs.get_unchecked((w >> 18) as usize),
                *cs.get_unchecked((w >> 12 & 63) as usize),
                *cs.get_unchecked((w >> 6 & 63) as usize),
                *cs.get_unchecked((w & 63) as usize),
            ]);
            dst.add(o).cast::<u32>().write_unaligned(out);
            o += 4;
        }
        match *chunks.remainder() {
            [a] => {
                *dst.add(o) = cs[(a >> 2) as usize];
                *dst.add(o + 1) = cs[((a & 3) << 4) as usize];
                if self.pad {
                    *dst.add(o + 2) = b'=';
                    *dst.add(o + 3) = b'=';
                }
            }
            [a, b] => {
                *dst.add(o) = cs[(a >> 2) as usize];
                *dst.add(o + 1) = cs[((a & 3) << 4 | b >> 4) as usize];
                *dst.add(o + 2) = cs[((b & 15) << 2) as usize];
                if self.pad {
                    *dst.add(o + 3) = b'=';
                }
            }
            _ => {}
        }
    }

    /// Decodes `src` (padding already stripped) into `dst`.
    ///
    /// # Safety
    /// `dst` must have room for the decoded length plus 8.
    unsafe fn decode_raw(&self, src: &[u8], dst: *mut u8, level: Level) -> Result<(), DecodeError> {
        let (i, o, ok) = match level {
            #[cfg(target_arch = "aarch64")]
            Level::Neon => decode_neon(src, dst, &self.decode),
            #[cfg(target_arch = "x86_64")]
            Level::Avx2 => decode_avx2(src, dst, &self.x86),
            #[cfg(target_arch = "x86_64")]
            Level::Ssse3 => decode_ssse3(src, dst, &self.x86),
            Level::Scalar => (0, 0, true),
        };
        if !ok {
            return Err(DecodeError);
        }
        self.decode_scalar(&src[i..], dst.add(o))
    }

    unsafe fn decode_scalar(&self, src: &[u8], dst: *mut u8) -> Result<(), DecodeError> {
        let t = &self.decode;
        let mut bad = 0u8;
        let mut o = 0;
        let mut chunks = src.chunks_exact(8);
        for c in &mut chunks {
            let mut w = 0u64;
            for (k, &ch) in c.iter().enumerate() {
                let v = t[ch as usize];
                bad |= v;
                w |= (v as u64) << (58 - 6 * k);
            }
            dst.add(o).cast::<u64>().write_unaligned(w.to_be());
            o += 6;
        }
        let rest = chunks.remainder();
        let mut quads = rest.chunks_exact(4);
        for c in &mut quads {
            let (a, b, c2, d) = (t[c[0] as usize], t[c[1] as usize], t[c[2] as usize], t[c[3] as usize]);
            bad |= a | b | c2 | d;
            let w = (a as u32) << 18 | (b as u32) << 12 | (c2 as u32) << 6 | d as u32;
            *dst.add(o) = (w >> 16) as u8;
            *dst.add(o + 1) = (w >> 8) as u8;
            *dst.add(o + 2) = w as u8;
            o += 3;
        }
        match *quads.remainder() {
            [] => {}
            [a, b] => {
                let (a, b) = (t[a as usize], t[b as usize]);
                bad |= a | b | if b & 0x0f != 0 { 0xff } else { 0 };
                *dst.add(o) = a << 2 | b >> 4;
            }
            [a, b, c] => {
                let (a, b, c) = (t[a as usize], t[b as usize], t[c as usize]);
                bad |= a | b | c | if c & 0x03 != 0 { 0xff } else { 0 };
                *dst.add(o) = a << 2 | b >> 4;
                *dst.add(o + 1) = b << 4 | c >> 2;
            }
            _ => return Err(DecodeError),
        }
        // Valid values are 0..=63; 0xff marks a byte outside the alphabet.
        if bad & 0xc0 != 0 {
            return Err(DecodeError);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Level {
    Scalar,
    #[cfg(target_arch = "x86_64")]
    Ssse3,
    #[cfg(target_arch = "x86_64")]
    Avx2,
    #[cfg(target_arch = "aarch64")]
    Neon,
}

impl Level {
    #[inline]
    fn best() -> Level {
        #[cfg(target_arch = "aarch64")]
        return Level::Neon;
        #[cfg(target_arch = "x86_64")]
        {
            if is_x86_feature_detected!("avx2") {
                return Level::Avx2;
            }
            if is_x86_feature_detected!("ssse3") {
                return Level::Ssse3;
            }
        }
        #[allow(unreachable_code)]
        Level::Scalar
    }
}

impl X86Luts {
    const fn new(charset: &[u8; 64], decode: &[u8; 256]) -> X86Luts {
        // Both alphabets share A-Z a-z 0-9; only the last two characters differ.
        let mut encode_shift = [0u8; 16];
        encode_shift[0] = b'a'.wrapping_sub(26);
        let mut k = 1;
        while k <= 10 {
            encode_shift[k] = b'0'.wrapping_sub(52);
            k += 1;
        }
        encode_shift[11] = charset[62].wrapping_sub(62);
        encode_shift[12] = charset[63].wrapping_sub(63);
        encode_shift[13] = b'A';

        // Each distinct per-row validity pattern gets a class bit.
        let mut patterns = [0u16; 8];
        let mut count = 0;
        let mut row_class = [0u8; 16];
        let mut h = 0;
        while h < 16 {
            let mut valid = 0u16;
            let mut l = 0;
            while l < 16 {
                if decode[h << 4 | l] != 0xff {
                    valid |= 1 << l;
                }
                l += 1;
            }
            let mut p = 0;
            while p < count && patterns[p] != valid {
                p += 1;
            }
            if p == count {
                assert!(count < 8, "base64 alphabet needs more than 8 nibble classes");
                patterns[p] = valid;
                count += 1;
            }
            row_class[h] = p as u8;
            h += 1;
        }
        let mut lo = [0u8; 16];
        let mut hi = [0u8; 16];
        let mut h = 0;
        while h < 16 {
            hi[h] = 1 << row_class[h];
            h += 1;
        }
        let mut l = 0;
        while l < 16 {
            let mut p = 0;
            while p < count {
                if patterns[p] & (1 << l) == 0 {
                    lo[l] |= 1 << p;
                }
                p += 1;
            }
            l += 1;
        }

        // One offset per high nibble, except for at most one character, which goes to slot 1.
        let mut roll = [0u8; 16];
        let mut set = [false; 16];
        let mut special = 0u8;
        let mut has_special = false;
        let mut c = 0;
        while c < 128 {
            let v = decode[c];
            if v != 0xff {
                let off = v.wrapping_sub(c as u8);
                let row = c >> 4;
                if !set[row] {
                    roll[row] = off;
                    set[row] = true;
                } else if roll[row] != off {
                    assert!(!has_special, "base64 alphabet has two characters off their row's offset");
                    special = c as u8;
                    has_special = true;
                    roll[ROLL_SLOT as usize] = off;
                }
            }
            c += 1;
        }
        assert!(!set[ROLL_SLOT as usize], "base64 alphabet uses the roll slot's row");
        let delta = ROLL_SLOT.wrapping_sub(special >> 4);
        X86Luts { encode_shift, lo, hi, roll, special, delta }
    }
}

// ============================================================================
// x86_64
// ============================================================================

#[cfg(target_arch = "x86_64")]
#[inline(always)]
unsafe fn lut128(t: &[u8; 16]) -> __m128i {
    _mm_loadu_si128(t.as_ptr().cast())
}

// The loops carry the target feature too, so the kernels inline and their constants hoist.
// Each finishes its full groups with one block overlapping what it already wrote, rewriting
// those bytes with the same values, so only the last partial group is left to scalar code.

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn encode_avx2(src: &[u8], dst: *mut u8, shift: &[u8; 16]) -> (usize, usize) {
    let (mut i, mut o) = (0, 0);
    if src.len() >= 24 {
        encode24_avx2(src.as_ptr(), dst, shift);
        (i, o) = (24, 32);
        while src.len() - i >= 28 {
            encode24_back_avx2(src.as_ptr().add(i - 4), dst.add(o), shift);
            i += 24;
            o += 32;
        }
    }
    encode_tail_ssse3(src, dst, shift, i, o)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "ssse3")]
unsafe fn encode_ssse3(src: &[u8], dst: *mut u8, shift: &[u8; 16]) -> (usize, usize) {
    encode_tail_ssse3(src, dst, shift, 0, 0)
}

/// 12-byte blocks from `i` on, then one block overlapping the last full triples.
#[cfg(target_arch = "x86_64")]
#[inline]
#[target_feature(enable = "ssse3")]
unsafe fn encode_tail_ssse3(src: &[u8], dst: *mut u8, shift: &[u8; 16], mut i: usize, mut o: usize) -> (usize, usize) {
    let full = src.len() / 3 * 3;
    while full - i >= 12 {
        encode12_at(src, i, dst.add(o), shift);
        i += 12;
        o += 16;
    }
    if full > i && full >= 12 {
        let s = full - 12;
        encode12_at(src, s, dst.add(s / 3 * 4), shift);
        (i, o) = (full, full / 3 * 4);
    }
    (i, o)
}

/// Encodes `src[at..at + 12]` with whichever 16-byte load stays inside `src`.
#[cfg(target_arch = "x86_64")]
#[inline]
#[target_feature(enable = "ssse3")]
unsafe fn encode12_at(src: &[u8], at: usize, dst: *mut u8, shift: &[u8; 16]) {
    if at >= 4 {
        encode12_ssse3::<true>(src.as_ptr().add(at - 4), dst, shift);
    } else if src.len() - at >= 16 {
        encode12_ssse3::<false>(src.as_ptr().add(at), dst, shift);
    } else {
        let mut buf = [0u8; 16];
        buf[4..].copy_from_slice(&src[at..at + 12]);
        encode12_ssse3::<true>(buf.as_ptr(), dst, shift);
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn decode_avx2(src: &[u8], dst: *mut u8, l: &X86Luts) -> (usize, usize, bool) {
    let full = src.len() / 4 * 4;
    let (mut i, mut o) = (0, 0);
    while full - i >= 32 {
        if !decode32_avx2(src.as_ptr().add(i), dst.add(o), l) {
            return (i, o, false);
        }
        i += 32;
        o += 24;
    }
    decode_tail_ssse3(src, dst, l, i, o)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "ssse3")]
unsafe fn decode_ssse3(src: &[u8], dst: *mut u8, l: &X86Luts) -> (usize, usize, bool) {
    decode_tail_ssse3(src, dst, l, 0, 0)
}

#[cfg(target_arch = "x86_64")]
#[inline]
#[target_feature(enable = "ssse3")]
unsafe fn decode_tail_ssse3(src: &[u8], dst: *mut u8, l: &X86Luts, mut i: usize, mut o: usize) -> (usize, usize, bool) {
    let full = src.len() / 4 * 4;
    while full - i >= 16 {
        if !decode16_ssse3(src.as_ptr().add(i), dst.add(o), l) {
            return (i, o, false);
        }
        i += 16;
        o += 12;
    }
    if full > i && full >= 16 {
        let s = full - 16;
        if !decode16_ssse3(src.as_ptr().add(s), dst.add(s / 4 * 3), l) {
            return (i, o, false);
        }
        (i, o) = (full, full / 4 * 3);
    }
    (i, o, true)
}

#[cfg(target_arch = "x86_64")]
const SPLIT_AT_0: [i8; 16] = [1, 0, 2, 1, 4, 3, 5, 4, 7, 6, 8, 7, 10, 9, 11, 10];
#[cfg(target_arch = "x86_64")]
const SPLIT_AT_4: [i8; 16] = [5, 4, 6, 5, 8, 7, 9, 8, 11, 10, 12, 11, 14, 13, 15, 14];

/// Encodes 12 bytes into 16 characters. The 16-byte load starts 4 bytes early when `BACK`.
#[cfg(target_arch = "x86_64")]
#[inline]
#[target_feature(enable = "ssse3")]
unsafe fn encode12_ssse3<const BACK: bool>(load: *const u8, dst: *mut u8, shift: &[u8; 16]) {
    let split = if BACK { &SPLIT_AT_4 } else { &SPLIT_AT_0 };
    let x = _mm_shuffle_epi8(_mm_loadu_si128(load.cast()), _mm_loadu_si128(split.as_ptr().cast()));
    let t0 = _mm_mulhi_epu16(_mm_and_si128(x, _mm_set1_epi32(0x0fc0fc00)), _mm_set1_epi32(0x04000040));
    let t1 = _mm_mullo_epi16(_mm_and_si128(x, _mm_set1_epi32(0x003f03f0)), _mm_set1_epi32(0x01000010));
    let idx = _mm_or_si128(t0, t1);
    let less = _mm_cmpgt_epi8(_mm_set1_epi8(26), idx);
    let r = _mm_or_si128(_mm_subs_epu8(idx, _mm_set1_epi8(51)), _mm_and_si128(less, _mm_set1_epi8(13)));
    let r = _mm_add_epi8(_mm_shuffle_epi8(lut128(shift), r), idx);
    _mm_storeu_si128(dst.cast(), r);
}

/// Encodes 24 bytes into 32 characters, reading exactly those 24.
#[cfg(target_arch = "x86_64")]
#[inline]
#[target_feature(enable = "avx2")]
unsafe fn encode24_avx2(src: *const u8, dst: *mut u8, shift: &[u8; 16]) {
    let lo = _mm_loadu_si128(src.cast());
    let hi = _mm_loadu_si128(src.add(8).cast());
    let x = _mm256_inserti128_si256(_mm256_castsi128_si256(lo), hi, 1);
    let split = _mm256_inserti128_si256(
        _mm256_castsi128_si256(_mm_loadu_si128(SPLIT_AT_0.as_ptr().cast())),
        _mm_loadu_si128(SPLIT_AT_4.as_ptr().cast()),
        1,
    );
    let x = _mm256_shuffle_epi8(x, split);
    let t0 = _mm256_mulhi_epu16(_mm256_and_si256(x, _mm256_set1_epi32(0x0fc0fc00)), _mm256_set1_epi32(0x04000040));
    let t1 = _mm256_mullo_epi16(_mm256_and_si256(x, _mm256_set1_epi32(0x003f03f0)), _mm256_set1_epi32(0x01000010));
    let idx = _mm256_or_si256(t0, t1);
    let less = _mm256_cmpgt_epi8(_mm256_set1_epi8(26), idx);
    let r = _mm256_or_si256(_mm256_subs_epu8(idx, _mm256_set1_epi8(51)), _mm256_and_si256(less, _mm256_set1_epi8(13)));
    let r = _mm256_add_epi8(_mm256_shuffle_epi8(_mm256_broadcastsi128_si256(lut128(shift)), r), idx);
    _mm256_storeu_si256(dst.cast(), r);
}

/// Encodes the 24 bytes 4 past `load` into 32 characters, from one 32-byte load.
#[cfg(target_arch = "x86_64")]
#[inline]
#[target_feature(enable = "avx2")]
unsafe fn encode24_back_avx2(load: *const u8, dst: *mut u8, shift: &[u8; 16]) {
    let x = _mm256_loadu_si256(load.cast());
    let x = _mm256_permutevar8x32_epi32(x, _mm256_setr_epi32(0, 1, 2, 3, 3, 4, 5, 6));
    let x = _mm256_shuffle_epi8(x, _mm256_broadcastsi128_si256(_mm_loadu_si128(SPLIT_AT_4.as_ptr().cast())));
    let t0 = _mm256_mulhi_epu16(_mm256_and_si256(x, _mm256_set1_epi32(0x0fc0fc00)), _mm256_set1_epi32(0x04000040));
    let t1 = _mm256_mullo_epi16(_mm256_and_si256(x, _mm256_set1_epi32(0x003f03f0)), _mm256_set1_epi32(0x01000010));
    let idx = _mm256_or_si256(t0, t1);
    let less = _mm256_cmpgt_epi8(_mm256_set1_epi8(26), idx);
    let r = _mm256_or_si256(_mm256_subs_epu8(idx, _mm256_set1_epi8(51)), _mm256_and_si256(less, _mm256_set1_epi8(13)));
    let r = _mm256_add_epi8(_mm256_shuffle_epi8(_mm256_broadcastsi128_si256(lut128(shift)), r), idx);
    _mm256_storeu_si256(dst.cast(), r);
}

/// Decodes 16 characters into 12 bytes, storing 16; false if any is outside the alphabet.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "ssse3")]
#[inline]
unsafe fn decode16_ssse3(src: *const u8, dst: *mut u8, l: &X86Luts) -> bool {
    let c = _mm_loadu_si128(src.cast());
    let nib = _mm_set1_epi8(0x0f);
    let hi = _mm_and_si128(_mm_srli_epi32(c, 4), nib);
    let lo = _mm_and_si128(c, nib);
    let class = _mm_and_si128(_mm_shuffle_epi8(lut128(&l.lo), lo), _mm_shuffle_epi8(lut128(&l.hi), hi));
    if _mm_movemask_epi8(_mm_cmpeq_epi8(class, _mm_setzero_si128())) != 0xffff {
        return false;
    }
    let eq = _mm_cmpeq_epi8(c, _mm_set1_epi8(l.special as i8));
    let slot = _mm_add_epi8(hi, _mm_and_si128(eq, _mm_set1_epi8(l.delta as i8)));
    let v = _mm_add_epi8(c, _mm_shuffle_epi8(lut128(&l.roll), slot));
    let v = _mm_madd_epi16(_mm_maddubs_epi16(v, _mm_set1_epi32(0x01400140)), _mm_set1_epi32(0x00011000));
    let v = _mm_shuffle_epi8(v, _mm_setr_epi8(2, 1, 0, 6, 5, 4, 10, 9, 8, 14, 13, 12, -1, -1, -1, -1));
    _mm_storeu_si128(dst.cast(), v);
    true
}

/// Decodes 32 characters into 24 bytes, storing 32; false if any is outside the alphabet.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
#[inline]
unsafe fn decode32_avx2(src: *const u8, dst: *mut u8, l: &X86Luts) -> bool {
    let c = _mm256_loadu_si256(src.cast());
    let nib = _mm256_set1_epi8(0x0f);
    let hi = _mm256_and_si256(_mm256_srli_epi32(c, 4), nib);
    let lo = _mm256_and_si256(c, nib);
    let class = _mm256_and_si256(
        _mm256_shuffle_epi8(_mm256_broadcastsi128_si256(lut128(&l.lo)), lo),
        _mm256_shuffle_epi8(_mm256_broadcastsi128_si256(lut128(&l.hi)), hi),
    );
    if _mm256_movemask_epi8(_mm256_cmpeq_epi8(class, _mm256_setzero_si256())) != -1 {
        return false;
    }
    let eq = _mm256_cmpeq_epi8(c, _mm256_set1_epi8(l.special as i8));
    let slot = _mm256_add_epi8(hi, _mm256_and_si256(eq, _mm256_set1_epi8(l.delta as i8)));
    let v = _mm256_add_epi8(c, _mm256_shuffle_epi8(_mm256_broadcastsi128_si256(lut128(&l.roll)), slot));
    let v = _mm256_madd_epi16(_mm256_maddubs_epi16(v, _mm256_set1_epi32(0x01400140)), _mm256_set1_epi32(0x00011000));
    let v = _mm256_shuffle_epi8(
        v,
        _mm256_setr_epi8(
            2, 1, 0, 6, 5, 4, 10, 9, 8, 14, 13, 12, -1, -1, -1, -1, 2, 1, 0, 6, 5, 4, 10, 9, 8, 14, 13, 12, -1, -1, -1,
            -1,
        ),
    );
    let v = _mm256_permutevar8x32_epi32(v, _mm256_setr_epi32(0, 1, 2, 4, 5, 6, 3, 7));
    _mm256_storeu_si256(dst.cast(), v);
    true
}

// ============================================================================
// ARM64
// ============================================================================

#[cfg(target_arch = "aarch64")]
#[inline(always)]
unsafe fn neon_table64(p: *const u8) -> uint8x16x4_t {
    uint8x16x4_t(vld1q_u8(p), vld1q_u8(p.add(16)), vld1q_u8(p.add(32)), vld1q_u8(p.add(48)))
}

/// 48- then 24-byte blocks, finishing with one 24-byte block overlapping what is already written.
#[cfg(target_arch = "aarch64")]
#[inline(always)]
unsafe fn encode_neon(src: &[u8], dst: *mut u8, charset: &[u8; 64]) -> (usize, usize) {
    let table = neon_table64(charset.as_ptr());
    let p = src.as_ptr();
    let full = src.len() / 3 * 3;
    let (mut i, mut o) = (0, 0);
    while full - i >= 48 {
        encode48_neon(p.add(i), dst.add(o), table);
        i += 48;
        o += 64;
    }
    while full - i >= 24 {
        encode24_neon(p.add(i), dst.add(o), table);
        i += 24;
        o += 32;
    }
    if full > i && full >= 24 {
        let s = full - 24;
        encode24_neon(p.add(s), dst.add(s / 3 * 4), table);
        (i, o) = (full, full / 3 * 4);
    }
    (i, o)
}

#[cfg(target_arch = "aarch64")]
#[inline(always)]
unsafe fn decode_neon(src: &[u8], dst: *mut u8, decode: &[u8; 256]) -> (usize, usize, bool) {
    let (lo, hi) = (neon_table64(decode.as_ptr()), neon_table64(decode.as_ptr().add(64)));
    let p = src.as_ptr();
    let full = src.len() / 4 * 4;
    let (mut i, mut o) = (0, 0);
    while full - i >= 64 {
        if !decode64_neon(p.add(i), dst.add(o), lo, hi) {
            return (i, o, false);
        }
        i += 64;
        o += 48;
    }
    while full - i >= 32 {
        if !decode32_neon(p.add(i), dst.add(o), lo, hi) {
            return (i, o, false);
        }
        i += 32;
        o += 24;
    }
    if full > i && full >= 32 {
        let s = full - 32;
        if !decode32_neon(p.add(s), dst.add(s / 4 * 3), lo, hi) {
            return (i, o, false);
        }
        (i, o) = (full, full / 4 * 3);
    }
    (i, o, true)
}

#[cfg(target_arch = "aarch64")]
#[inline(always)]
unsafe fn encode48_neon(src: *const u8, dst: *mut u8, table: uint8x16x4_t) {
    let x = vld3q_u8(src);
    let m = vdupq_n_u8(63);
    let i0 = vshrq_n_u8(x.0, 2);
    let i1 = vandq_u8(vorrq_u8(vshlq_n_u8(x.0, 4), vshrq_n_u8(x.1, 4)), m);
    let i2 = vandq_u8(vorrq_u8(vshlq_n_u8(x.1, 2), vshrq_n_u8(x.2, 6)), m);
    let i3 = vandq_u8(x.2, m);
    vst4q_u8(
        dst,
        uint8x16x4_t(vqtbl4q_u8(table, i0), vqtbl4q_u8(table, i1), vqtbl4q_u8(table, i2), vqtbl4q_u8(table, i3)),
    );
}

#[cfg(target_arch = "aarch64")]
#[inline(always)]
unsafe fn encode24_neon(src: *const u8, dst: *mut u8, table: uint8x16x4_t) {
    let x = vld3_u8(src);
    let m = vdup_n_u8(63);
    let i0 = vshr_n_u8(x.0, 2);
    let i1 = vand_u8(vorr_u8(vshl_n_u8(x.0, 4), vshr_n_u8(x.1, 4)), m);
    let i2 = vand_u8(vorr_u8(vshl_n_u8(x.1, 2), vshr_n_u8(x.2, 6)), m);
    let i3 = vand_u8(x.2, m);
    vst4_u8(dst, uint8x8x4_t(vqtbl4_u8(table, i0), vqtbl4_u8(table, i1), vqtbl4_u8(table, i2), vqtbl4_u8(table, i3)));
}

/// Decodes 64 characters into 48 bytes; false if any is outside the alphabet.
#[cfg(target_arch = "aarch64")]
#[inline(always)]
unsafe fn decode64_neon(src: *const u8, dst: *mut u8, lo: uint8x16x4_t, hi: uint8x16x4_t) -> bool {
    let c = vld4q_u8(src);
    // Bytes below 64 hit `lo`; 64..128 fall through to `hi`; 128+ keep 0 and are caught by their top bit.
    let look = |c: uint8x16_t| vqtbx4q_u8(vqtbl4q_u8(lo, c), hi, vsubq_u8(c, vdupq_n_u8(64)));
    let (v0, v1, v2, v3) = (look(c.0), look(c.1), look(c.2), look(c.3));
    let high = vandq_u8(vorrq_u8(vorrq_u8(c.0, c.1), vorrq_u8(c.2, c.3)), vdupq_n_u8(0x80));
    if vmaxvq_u8(vorrq_u8(vorrq_u8(vorrq_u8(v0, v1), vorrq_u8(v2, v3)), high)) > 63 {
        return false;
    }
    let o0 = vorrq_u8(vshlq_n_u8(v0, 2), vshrq_n_u8(v1, 4));
    let o1 = vorrq_u8(vshlq_n_u8(v1, 4), vshrq_n_u8(v2, 2));
    let o2 = vorrq_u8(vshlq_n_u8(v2, 6), v3);
    vst3q_u8(dst, uint8x16x3_t(o0, o1, o2));
    true
}

/// Decodes 32 characters into 24 bytes; false if any is outside the alphabet.
#[cfg(target_arch = "aarch64")]
#[inline(always)]
unsafe fn decode32_neon(src: *const u8, dst: *mut u8, lo: uint8x16x4_t, hi: uint8x16x4_t) -> bool {
    let c = vld4_u8(src);
    let look = |c: uint8x8_t| vqtbx4_u8(vqtbl4_u8(lo, c), hi, vsub_u8(c, vdup_n_u8(64)));
    let (v0, v1, v2, v3) = (look(c.0), look(c.1), look(c.2), look(c.3));
    let high = vand_u8(vorr_u8(vorr_u8(c.0, c.1), vorr_u8(c.2, c.3)), vdup_n_u8(0x80));
    if vmaxv_u8(vorr_u8(vorr_u8(vorr_u8(v0, v1), vorr_u8(v2, v3)), high)) > 63 {
        return false;
    }
    let o0 = vorr_u8(vshl_n_u8(v0, 2), vshr_n_u8(v1, 4));
    let o1 = vorr_u8(vshl_n_u8(v1, 4), vshr_n_u8(v2, 2));
    let o2 = vorr_u8(vshl_n_u8(v2, 6), v3);
    vst3_u8(dst, uint8x8x3_t(o0, o1, o2));
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) fn levels() -> Vec<Level> {
        #[allow(unused_mut)]
        let mut v = vec![Level::Scalar];
        #[cfg(target_arch = "x86_64")]
        {
            if is_x86_feature_detected!("ssse3") {
                v.push(Level::Ssse3);
            }
            if is_x86_feature_detected!("avx2") {
                v.push(Level::Avx2);
            }
        }
        #[cfg(target_arch = "aarch64")]
        v.push(Level::Neon);
        v
    }

    fn encode_with(e: &Engine, src: &[u8], level: Level) -> String {
        let len = e.encoded_len(src.len());
        let mut v = Vec::with_capacity(len);
        unsafe {
            e.encode_raw(src, v.as_mut_ptr(), level);
            v.set_len(len);
        }
        String::from_utf8(v).unwrap()
    }

    /// A deterministic byte stream (xorshift), so failures reproduce.
    fn bytes(seed: u64, n: usize) -> Vec<u8> {
        let mut s = seed | 1;
        (0..n)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                s as u8
            })
            .collect()
    }

    #[test]
    fn rfc4648_vectors() {
        for (raw, enc) in [("", ""), ("f", "Zg=="), ("fo", "Zm8="), ("foo", "Zm9v"), ("foob", "Zm9vYg=="), ("fooba", "Zm9vYmE="), ("foobar", "Zm9vYmFy")] {
            assert_eq!(STANDARD.encode_to_string(raw), enc);
            assert_eq!(STANDARD.decode_to_vec(enc).unwrap(), raw.as_bytes());
            let bare = enc.trim_end_matches('=');
            assert_eq!(URL_SAFE_NO_PAD.encode_to_string(raw), bare);
            assert_eq!(URL_SAFE_NO_PAD.decode_to_vec(bare).unwrap(), raw.as_bytes());
        }
        assert_eq!(STANDARD.encode_to_string([0xfb, 0xff]), "+/8=");
        assert_eq!(URL_SAFE_NO_PAD.encode_to_string([0xfb, 0xff]), "-_8");
    }

    #[test]
    fn every_level_matches_the_reference_for_every_length() {
        use base64::Engine as _;
        let std_ref = base64::engine::general_purpose::STANDARD;
        let url_ref = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        for n in 0..400 {
            let data = bytes(n as u64 + 7, n);
            let (s, u) = (std_ref.encode(&data), url_ref.encode(&data));
            for level in levels() {
                assert_eq!(encode_with(&STANDARD, &data, level), s, "{level:?} n={n}");
                assert_eq!(encode_with(&URL_SAFE_NO_PAD, &data, level), u, "{level:?} n={n}");
                assert_eq!(STANDARD.decode_with(s.as_bytes(), level).unwrap(), data, "{level:?} n={n}");
                assert_eq!(URL_SAFE_NO_PAD.decode_with(u.as_bytes(), level).unwrap(), data, "{level:?} n={n}");
            }
        }
    }

    #[test]
    fn every_level_rejects_exactly_what_the_reference_rejects() {
        use base64::Engine as _;
        let std_ref = base64::engine::general_purpose::STANDARD;
        let url_ref = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        // One byte of a valid encoding replaced, at every position, by every byte value.
        for n in [1usize, 2, 3, 12, 13, 47, 48, 49, 95, 96, 100] {
            let data = bytes(n as u64, n);
            for (engine, reference) in [(&STANDARD, &std_ref), (&URL_SAFE_NO_PAD, &url_ref)] {
                let good = reference.encode(&data).into_bytes();
                for pos in 0..good.len() {
                    for b in 0..=255u8 {
                        let mut s = good.clone();
                        s[pos] = b;
                        let want = reference.decode(&s).ok();
                        for level in levels() {
                            assert_eq!(engine.decode_with(&s, level).ok(), want, "{level:?} n={n} pos={pos} byte={b}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn padding_and_length_rules() {
        for bad in ["A", "AB", "ABC", "A===", "AB=C", "====", "AB==C===", "Zg==Zg==", "Zh==", "Zm9=", " Zg=", "Zg==\n"] {
            assert!(STANDARD.decode_to_vec(bad).is_err(), "{bad:?}");
        }
        for bad in ["A", "Zg==", "Zh", "Zm9", "AAAAA"] {
            assert!(URL_SAFE_NO_PAD.decode_to_vec(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn append_keeps_what_was_there() {
        let mut s = String::from("data:image/png;base64,");
        STANDARD.encode_append(b"foobar", &mut s);
        assert_eq!(s, "data:image/png;base64,Zm9vYmFy");
    }
}
