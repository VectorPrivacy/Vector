//! SIMD-accelerated hex encoding and decoding
//!
//! # Performance
//!
//! **Encoding (32 bytes → 64 hex chars):**
//! - `format!()`: ~1630 ns
//! - Scalar LUT: ~35 ns (47x faster)
//! - NEON SIMD (ARM64): ~26 ns (62x faster)
//! - SSE2 SIMD (x86_64): ~30 ns (estimated)
//! - AVX2 SIMD (x86_64): ~25 ns (estimated)
//!
//! **Decoding (64 hex chars → 32 bytes):**
//! - NEON SIMD (ARM64): ~3 ns (7x faster than LUT)
//! - SSE2 SIMD (x86_64): ~5 ns (estimated)
//! - Scalar LUT fallback: ~19 ns
//!
//! # Algorithm
//!
//! **NEON (ARM64):** Uses TBL instruction for 16-byte lookup table
//!
//! **SSE2/AVX2 (x86_64):** Uses arithmetic approach:
//! 1. Split bytes into nibbles (high = byte >> 4, low = byte & 0x0F)
//! 2. Compare nibbles > 9 to identify hex letters (a-f)
//! 3. Add '0' (0x30) to all, then add 0x27 for letters (a-f)
//! 4. Interleave and store

#[cfg(target_arch = "aarch64")]
use std::arch::aarch64::*;

#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::*;

// ============================================================================
// Lookup Tables
// ============================================================================

/// Nibble-to-hex lookup table for NEON SIMD (16 bytes fits in one register).
#[cfg(target_arch = "aarch64")]
const HEX_NIBBLE: &[u8; 16] = b"0123456789abcdef";

/// Lookup table for scalar hex encoding (non-SIMD platforms).
/// Each byte maps to its 2-char hex representation.
#[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
const HEX_ENCODE_LUT: &[u8; 512] = b"000102030405060708090a0b0c0d0e0f\
101112131415161718191a1b1c1d1e1f\
202122232425262728292a2b2c2d2e2f\
303132333435363738393a3b3c3d3e3f\
404142434445464748494a4b4c4d4e4f\
505152535455565758595a5b5c5d5e5f\
606162636465666768696a6b6c6d6e6f\
707172737475767778797a7b7c7d7e7f\
808182838485868788898a8b8c8d8e8f\
909192939495969798999a9b9c9d9e9f\
a0a1a2a3a4a5a6a7a8a9aaabacadaeaf\
b0b1b2b3b4b5b6b7b8b9babbbcbdbebf\
c0c1c2c3c4c5c6c7c8c9cacbcccdcecf\
d0d1d2d3d4d5d6d7d8d9dadbdcdddedf\
e0e1e2e3e4e5e6e7e8e9eaebecedeeef\
f0f1f2f3f4f5f6f7f8f9fafbfcfdfeff";

/// Compile-time lookup table for hex character to nibble conversion.
/// Maps ASCII byte values to their nibble value (0-15), invalid chars map to 0.
const HEX_DECODE_LUT: [u8; 256] = {
    let mut table = [0u8; 256];
    let mut i = 0;
    while i < 256 {
        table[i] = match i as u8 {
            b'0'..=b'9' => (i as u8) - b'0',
            b'a'..=b'f' => (i as u8) - b'a' + 10,
            b'A'..=b'F' => (i as u8) - b'A' + 10,
            _ => 0,
        };
        i += 1;
    }
    table
};

// ============================================================================
// Hex Encoding - NEON (ARM64)
// ============================================================================

/// Convert 32-byte array to hex string using NEON SIMD (ARM64).
///
/// # Performance
/// - ~26 ns total (including String allocation)
/// - Zero-copy: writes directly into String buffer
/// - 62x faster than format!()
#[cfg(target_arch = "aarch64")]
#[inline]
pub fn bytes_to_hex_32(bytes: &[u8; 32]) -> String {
    unsafe {
        // Allocate String directly - no intermediate buffer, no copy
        let mut s = String::with_capacity(64);
        let buf = s.as_mut_vec().as_mut_ptr();
        let hex_lut = vld1q_u8(HEX_NIBBLE.as_ptr());

        for chunk_idx in 0..2 {
            let offset = chunk_idx * 16;
            let out_offset = chunk_idx * 32;

            let input = vld1q_u8(bytes.as_ptr().add(offset));
            let hi_nibbles = vshrq_n_u8(input, 4);
            let lo_nibbles = vandq_u8(input, vdupq_n_u8(0x0f));
            let hi_hex = vqtbl1q_u8(hex_lut, hi_nibbles);
            let lo_hex = vqtbl1q_u8(hex_lut, lo_nibbles);
            let result_lo = vzip1q_u8(hi_hex, lo_hex);
            let result_hi = vzip2q_u8(hi_hex, lo_hex);

            vst1q_u8(buf.add(out_offset), result_lo);
            vst1q_u8(buf.add(out_offset + 16), result_hi);
        }

        // SAFETY: We wrote exactly 64 ASCII hex chars (0-9, a-f)
        s.as_mut_vec().set_len(64);
        s
    }
}

/// Convert 16-byte array to hex string using NEON SIMD (ARM64).
/// Zero-copy: writes directly into String buffer.
#[cfg(target_arch = "aarch64")]
#[inline]
pub fn bytes_to_hex_16(bytes: &[u8; 16]) -> String {
    unsafe {
        // Allocate String directly - no intermediate buffer, no copy
        let mut s = String::with_capacity(32);
        let buf = s.as_mut_vec().as_mut_ptr();
        let hex_lut = vld1q_u8(HEX_NIBBLE.as_ptr());
        let input = vld1q_u8(bytes.as_ptr());

        let hi_nibbles = vshrq_n_u8(input, 4);
        let lo_nibbles = vandq_u8(input, vdupq_n_u8(0x0f));
        let hi_hex = vqtbl1q_u8(hex_lut, hi_nibbles);
        let lo_hex = vqtbl1q_u8(hex_lut, lo_nibbles);
        let result_lo = vzip1q_u8(hi_hex, lo_hex);
        let result_hi = vzip2q_u8(hi_hex, lo_hex);

        vst1q_u8(buf, result_lo);
        vst1q_u8(buf.add(16), result_hi);

        // SAFETY: We wrote exactly 32 ASCII hex chars (0-9, a-f)
        s.as_mut_vec().set_len(32);
        s
    }
}

// ============================================================================
// Hex Encoding - x86_64 SIMD (SSE2 + AVX2)
// ============================================================================

/// Internal: AVX2 implementation for 32-byte hex encoding.
/// Processes all 32 bytes in a single operation using 256-bit registers.
///
/// # Safety
/// Caller must ensure AVX2 is available (use `is_x86_feature_detected!`).
///
/// # Reference
/// Algorithm based on faster-hex crate (MIT license):
/// https://github.com/nervosnetwork/faster-hex
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
#[inline]
unsafe fn hex_encode_32_avx2(bytes: &[u8; 32], buf: *mut u8) {
    // Constants for hex conversion
    let and4bits = _mm256_set1_epi8(0x0f);
    let nines = _mm256_set1_epi8(9);
    let ascii_zero = _mm256_set1_epi8(b'0' as i8);
    // 'a' - 9 - 1 = 87, so nibble + 87 = 'a' for nibble 10
    let ascii_a_offset = _mm256_set1_epi8((b'a' - 9 - 1) as i8);

    // Load all 32 bytes at once
    let invec = _mm256_loadu_si256(bytes.as_ptr() as *const __m256i);

    // Extract nibbles: low = byte & 0x0F, high = (byte >> 4) & 0x0F
    // Note: srli_epi64 shifts 64-bit lanes, but we mask afterward so it's fine
    let lo_nibbles = _mm256_and_si256(invec, and4bits);
    let hi_nibbles = _mm256_and_si256(_mm256_srli_epi64(invec, 4), and4bits);

    // Compare > 9 to identify hex letters (a-f)
    let lo_gt9 = _mm256_cmpgt_epi8(lo_nibbles, nines);
    let hi_gt9 = _mm256_cmpgt_epi8(hi_nibbles, nines);

    // Convert to ASCII using blendv for conditional offset:
    // if nibble <= 9: nibble + '0'
    // if nibble > 9:  nibble + ('a' - 10) = nibble + 87
    let lo_hex = _mm256_add_epi8(
        lo_nibbles,
        _mm256_blendv_epi8(ascii_zero, ascii_a_offset, lo_gt9),
    );
    let hi_hex = _mm256_add_epi8(
        hi_nibbles,
        _mm256_blendv_epi8(ascii_zero, ascii_a_offset, hi_gt9),
    );

    // Interleave high and low nibbles: [H0,L0,H1,L1,...]
    // Note: AVX2 unpack operates within 128-bit lanes, so output is:
    //   res1: [H0,L0..H7,L7 | H16,L16..H23,L23]  (bytes 0-7, 16-23)
    //   res2: [H8,L8..H15,L15 | H24,L24..H31,L31] (bytes 8-15, 24-31)
    let res1 = _mm256_unpacklo_epi8(hi_hex, lo_hex);
    let res2 = _mm256_unpackhi_epi8(hi_hex, lo_hex);

    // Store with lane correction using storeu2_m128i:
    // res1 low 128 bits  -> positions 0-15  (bytes 0-7 interleaved)
    // res2 low 128 bits  -> positions 16-31 (bytes 8-15 interleaved)
    // res1 high 128 bits -> positions 32-47 (bytes 16-23 interleaved)
    // res2 high 128 bits -> positions 48-63 (bytes 24-31 interleaved)
    _mm256_storeu2_m128i(
        buf.add(32) as *mut __m128i,  // high 128 bits
        buf as *mut __m128i,          // low 128 bits
        res1,
    );
    _mm256_storeu2_m128i(
        buf.add(48) as *mut __m128i,  // high 128 bits
        buf.add(16) as *mut __m128i,  // low 128 bits
        res2,
    );
}

/// Internal: SSE2 implementation for 32-byte hex encoding.
/// Processes 16 bytes at a time using 128-bit registers.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
#[inline]
unsafe fn hex_encode_32_sse2(bytes: &[u8; 32], buf: *mut u8) {
    let mask_lo = _mm_set1_epi8(0x0f);
    let nine = _mm_set1_epi8(9);
    let ascii_zero = _mm_set1_epi8(b'0' as i8);
    let letter_offset = _mm_set1_epi8(0x27); // 'a' - '0' - 10 = 0x27

    for chunk_idx in 0..2 {
        let offset = chunk_idx * 16;
        let out_offset = chunk_idx * 32;

        let input = _mm_loadu_si128(bytes.as_ptr().add(offset) as *const __m128i);

        // Extract nibbles (use epi16 shift then mask)
        let hi_nibbles = _mm_and_si128(_mm_srli_epi16(input, 4), mask_lo);
        let lo_nibbles = _mm_and_si128(input, mask_lo);

        // Convert: nibble + '0' + ((nibble > 9) ? 0x27 : 0)
        let hi_gt9 = _mm_cmpgt_epi8(hi_nibbles, nine);
        let lo_gt9 = _mm_cmpgt_epi8(lo_nibbles, nine);

        let hi_hex = _mm_add_epi8(
            _mm_add_epi8(hi_nibbles, ascii_zero),
            _mm_and_si128(hi_gt9, letter_offset),
        );
        let lo_hex = _mm_add_epi8(
            _mm_add_epi8(lo_nibbles, ascii_zero),
            _mm_and_si128(lo_gt9, letter_offset),
        );

        // Interleave and store
        let result_lo = _mm_unpacklo_epi8(hi_hex, lo_hex);
        let result_hi = _mm_unpackhi_epi8(hi_hex, lo_hex);

        _mm_storeu_si128(buf.add(out_offset) as *mut __m128i, result_lo);
        _mm_storeu_si128(buf.add(out_offset + 16) as *mut __m128i, result_hi);
    }
}

/// Convert 32-byte array to hex string using SIMD (x86_64).
///
/// Automatically uses AVX2 if available, otherwise falls back to SSE2.
/// Zero-copy: writes directly into String buffer.
#[cfg(target_arch = "x86_64")]
#[inline]
pub fn bytes_to_hex_32(bytes: &[u8; 32]) -> String {
    unsafe {
        let mut s = String::with_capacity(64);
        let buf = s.as_mut_vec().as_mut_ptr();

        // Runtime feature detection (cached after first call)
        if is_x86_feature_detected!("avx2") {
            hex_encode_32_avx2(bytes, buf);
        } else {
            hex_encode_32_sse2(bytes, buf);
        }

        s.as_mut_vec().set_len(64);
        s
    }
}

/// Internal: SSE2 implementation for 16-byte hex encoding.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
#[inline]
unsafe fn hex_encode_16_sse2(bytes: &[u8; 16], buf: *mut u8) {
    let mask_lo = _mm_set1_epi8(0x0f);
    let nine = _mm_set1_epi8(9);
    let ascii_zero = _mm_set1_epi8(b'0' as i8);
    let letter_offset = _mm_set1_epi8(0x27);

    let input = _mm_loadu_si128(bytes.as_ptr() as *const __m128i);

    let hi_nibbles = _mm_and_si128(_mm_srli_epi16(input, 4), mask_lo);
    let lo_nibbles = _mm_and_si128(input, mask_lo);

    let hi_gt9 = _mm_cmpgt_epi8(hi_nibbles, nine);
    let lo_gt9 = _mm_cmpgt_epi8(lo_nibbles, nine);

    let hi_hex = _mm_add_epi8(
        _mm_add_epi8(hi_nibbles, ascii_zero),
        _mm_and_si128(hi_gt9, letter_offset),
    );
    let lo_hex = _mm_add_epi8(
        _mm_add_epi8(lo_nibbles, ascii_zero),
        _mm_and_si128(lo_gt9, letter_offset),
    );

    let result_lo = _mm_unpacklo_epi8(hi_hex, lo_hex);
    let result_hi = _mm_unpackhi_epi8(hi_hex, lo_hex);

    _mm_storeu_si128(buf as *mut __m128i, result_lo);
    _mm_storeu_si128(buf.add(16) as *mut __m128i, result_hi);
}

/// Convert 16-byte array to hex string using SSE2 SIMD (x86_64).
#[cfg(target_arch = "x86_64")]
#[inline]
pub fn bytes_to_hex_16(bytes: &[u8; 16]) -> String {
    unsafe {
        let mut s = String::with_capacity(32);
        let buf = s.as_mut_vec().as_mut_ptr();
        hex_encode_16_sse2(bytes, buf);
        s.as_mut_vec().set_len(32);
        s
    }
}

// ============================================================================
// Hex Encoding - Scalar Fallback (other architectures)
// ============================================================================

/// Fallback: Convert 32-byte array to hex using scalar LUT.
#[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
#[inline]
pub fn bytes_to_hex_32(bytes: &[u8; 32]) -> String {
    unsafe {
        let mut s = String::with_capacity(64);
        let buf = s.as_mut_vec().as_mut_ptr();
        for (i, &b) in bytes.iter().enumerate() {
            let idx = (b as usize) * 2;
            *buf.add(i * 2) = HEX_ENCODE_LUT[idx];
            *buf.add(i * 2 + 1) = HEX_ENCODE_LUT[idx + 1];
        }
        s.as_mut_vec().set_len(64);
        s
    }
}

/// Fallback: Convert 16-byte array to hex using scalar LUT.
#[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
#[inline]
pub fn bytes_to_hex_16(bytes: &[u8; 16]) -> String {
    unsafe {
        let mut s = String::with_capacity(32);
        let buf = s.as_mut_vec().as_mut_ptr();
        for (i, &b) in bytes.iter().enumerate() {
            let idx = (b as usize) * 2;
            *buf.add(i * 2) = HEX_ENCODE_LUT[idx];
            *buf.add(i * 2 + 1) = HEX_ENCODE_LUT[idx + 1];
        }
        s.as_mut_vec().set_len(32);
        s
    }
}

// ============================================================================
// Hex Encoding - Variable Length
// ============================================================================

/// Convert a byte slice to a hex string.
///
/// For fixed-size arrays, prefer [`bytes_to_hex_32`] or [`bytes_to_hex_16`]
/// which use SIMD acceleration:
/// - **ARM64**: NEON with TBL lookup
/// - **x86_64**: AVX2 (if available) or SSE2 fallback
///
/// Zero-copy: writes directly into String buffer.
pub fn bytes_to_hex_string(bytes: &[u8]) -> String {
    // Use optimized paths for common fixed sizes
    if bytes.len() == 32 {
        return bytes_to_hex_32(bytes.try_into().unwrap());
    }
    if bytes.len() == 16 {
        return bytes_to_hex_16(bytes.try_into().unwrap());
    }

    let out_len = bytes.len().checked_mul(2).expect("hex string length overflow");

    #[cfg(target_arch = "aarch64")]
    unsafe {
        // Allocate once, write directly - no intermediate buffers
        let mut s = String::with_capacity(out_len);
        let out_ptr = s.as_mut_vec().as_mut_ptr();
        let chunks = bytes.len() / 16;
        let hex_lut = vld1q_u8(HEX_NIBBLE.as_ptr());

        // SIMD: process 16 input bytes -> 32 output bytes per iteration
        for i in 0..chunks {
            let input = vld1q_u8(bytes.as_ptr().add(i * 16));
            let hi = vshrq_n_u8(input, 4);
            let lo = vandq_u8(input, vdupq_n_u8(0x0f));
            let hi_hex = vqtbl1q_u8(hex_lut, hi);
            let lo_hex = vqtbl1q_u8(hex_lut, lo);
            vst1q_u8(out_ptr.add(i * 32), vzip1q_u8(hi_hex, lo_hex));
            vst1q_u8(out_ptr.add(i * 32 + 16), vzip2q_u8(hi_hex, lo_hex));
        }

        // Scalar for remaining bytes (0-15 bytes)
        let remainder_start = chunks * 16;
        let mut out_idx = chunks * 32;
        for &b in &bytes[remainder_start..] {
            *out_ptr.add(out_idx) = HEX_NIBBLE[(b >> 4) as usize];
            *out_ptr.add(out_idx + 1) = HEX_NIBBLE[(b & 0xf) as usize];
            out_idx += 2;
        }

        s.as_mut_vec().set_len(out_len);
        s
    }

    #[cfg(target_arch = "x86_64")]
    unsafe {
        // Allocate once, write directly - no intermediate buffers
        let mut s = String::with_capacity(out_len);
        let out_ptr = s.as_mut_vec().as_mut_ptr();
        let chunks = bytes.len() / 16;

        // SSE2 constants
        let mask_lo = _mm_set1_epi8(0x0f);
        let nine = _mm_set1_epi8(9);
        let ascii_zero = _mm_set1_epi8(b'0' as i8);
        let letter_offset = _mm_set1_epi8(0x27);

        // SIMD: process 16 input bytes -> 32 output bytes per iteration
        for i in 0..chunks {
            let input = _mm_loadu_si128(bytes.as_ptr().add(i * 16) as *const __m128i);

            let hi = _mm_and_si128(_mm_srli_epi16(input, 4), mask_lo);
            let lo = _mm_and_si128(input, mask_lo);

            let hi_gt9 = _mm_cmpgt_epi8(hi, nine);
            let lo_gt9 = _mm_cmpgt_epi8(lo, nine);

            let hi_hex = _mm_add_epi8(
                _mm_add_epi8(hi, ascii_zero),
                _mm_and_si128(hi_gt9, letter_offset),
            );
            let lo_hex = _mm_add_epi8(
                _mm_add_epi8(lo, ascii_zero),
                _mm_and_si128(lo_gt9, letter_offset),
            );

            _mm_storeu_si128(out_ptr.add(i * 32) as *mut __m128i, _mm_unpacklo_epi8(hi_hex, lo_hex));
            _mm_storeu_si128(out_ptr.add(i * 32 + 16) as *mut __m128i, _mm_unpackhi_epi8(hi_hex, lo_hex));
        }

        // Scalar for remaining bytes (0-15 bytes)
        const HEX_CHARS: &[u8; 16] = b"0123456789abcdef";
        let remainder_start = chunks * 16;
        let mut out_idx = chunks * 32;
        for &b in &bytes[remainder_start..] {
            *out_ptr.add(out_idx) = HEX_CHARS[(b >> 4) as usize];
            *out_ptr.add(out_idx + 1) = HEX_CHARS[(b & 0xf) as usize];
            out_idx += 2;
        }

        s.as_mut_vec().set_len(out_len);
        s
    }

    #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
    unsafe {
        // Allocate once, write directly
        let mut s = String::with_capacity(out_len);
        let out_ptr = s.as_mut_vec().as_mut_ptr();
        for (i, &b) in bytes.iter().enumerate() {
            let idx = (b as usize) * 2;
            *out_ptr.add(i * 2) = HEX_ENCODE_LUT[idx];
            *out_ptr.add(i * 2 + 1) = HEX_ENCODE_LUT[idx + 1];
        }
        s.as_mut_vec().set_len(out_len);
        s
    }
}

// ============================================================================
// Hex Decoding - SIMD Accelerated
// ============================================================================

/// Convert hex string to fixed 32-byte array.
///
/// # Performance
/// - NEON (ARM64): ~2.5 ns / 8 cycles (7.7x faster than LUT)
/// - SSE2 (x86_64): ~5 ns (estimated)
/// - Scalar fallback: ~19 ns
///
/// # Note
/// Invalid hex characters are treated as 0x00. Short strings are zero-padded.
#[cfg(target_arch = "aarch64")]
#[inline]
pub fn hex_to_bytes_32(hex: &str) -> [u8; 32] {
    let h = hex.as_bytes();

    // Fast path: exactly 64 chars, use SIMD
    if h.len() >= 64 {
        return unsafe { hex_decode_32_neon(h) };
    }

    // Slow path for short strings (zero-pad on left)
    hex_to_bytes_32_scalar_padded(h)
}

/// NEON implementation: decode 64 hex chars to 32 bytes
///
/// Optimized algorithm:
/// 1. Simplified nibble conversion: (char & 0x0F) + 9*(char has bit 0x40 set)
///    - For '0'-'9': (0x30-0x39 & 0x0F) = 0-9, bit 0x40 not set, so +0
///    - For 'A'-'F': (0x41-0x46 & 0x0F) = 1-6, bit 0x40 set, so +9 = 10-15
///    - For 'a'-'f': (0x61-0x66 & 0x0F) = 1-6, bit 0x40 set, so +9 = 10-15
/// 2. Uses SLI (Shift Left and Insert) to combine nibbles in one instruction
/// 3. Fully unrolled for maximum throughput
#[cfg(target_arch = "aarch64")]
#[inline]
unsafe fn hex_decode_32_neon(h: &[u8]) -> [u8; 32] {
    let mut result = [0u8; 32];

    let mask_0f = vdupq_n_u8(0x0F);
    let mask_40 = vdupq_n_u8(0x40);
    let nine = vdupq_n_u8(9);

    // Load all 64 hex chars at once
    let hex_0 = vld1q_u8(h.as_ptr());
    let hex_1 = vld1q_u8(h.as_ptr().add(16));
    let hex_2 = vld1q_u8(h.as_ptr().add(32));
    let hex_3 = vld1q_u8(h.as_ptr().add(48));

    // Convert ASCII to nibbles using simplified algorithm
    // (char & 0x0F) + 9 if letter (bit 0x40 set)
    let lo0 = vandq_u8(hex_0, mask_0f);
    let lo1 = vandq_u8(hex_1, mask_0f);
    let lo2 = vandq_u8(hex_2, mask_0f);
    let lo3 = vandq_u8(hex_3, mask_0f);

    let is_letter0 = vtstq_u8(hex_0, mask_40);
    let is_letter1 = vtstq_u8(hex_1, mask_40);
    let is_letter2 = vtstq_u8(hex_2, mask_40);
    let is_letter3 = vtstq_u8(hex_3, mask_40);

    let n0 = vaddq_u8(lo0, vandq_u8(is_letter0, nine));
    let n1 = vaddq_u8(lo1, vandq_u8(is_letter1, nine));
    let n2 = vaddq_u8(lo2, vandq_u8(is_letter2, nine));
    let n3 = vaddq_u8(lo3, vandq_u8(is_letter3, nine));

    // Pack nibbles to bytes using UZP + SLI
    // SLI (Shift Left and Insert) combines shift+or into one instruction
    let evens_a = vuzp1q_u8(n0, n1);
    let odds_a = vuzp2q_u8(n0, n1);
    let bytes_a = vsliq_n_u8(odds_a, evens_a, 4);

    let evens_b = vuzp1q_u8(n2, n3);
    let odds_b = vuzp2q_u8(n2, n3);
    let bytes_b = vsliq_n_u8(odds_b, evens_b, 4);

    vst1q_u8(result.as_mut_ptr(), bytes_a);
    vst1q_u8(result.as_mut_ptr().add(16), bytes_b);

    result
}

/// NEON: per-lane hex validity for a 16-char vector. Returns 0xFF iff EVERY lane is `[0-9A-Fa-f]`,
/// else 0x00. Three parallel range tests (`[0-9]`, `[A-F]`, `[a-f]`) OR'd per lane; the horizontal
/// min collapses "all lanes valid" to a single byte.
#[cfg(target_arch = "aarch64")]
#[inline]
unsafe fn neon_all_hex(c: uint8x16_t) -> u8 {
    let is_digit = vandq_u8(vcgeq_u8(c, vdupq_n_u8(b'0')), vcleq_u8(c, vdupq_n_u8(b'9')));
    let is_upper = vandq_u8(vcgeq_u8(c, vdupq_n_u8(b'A')), vcleq_u8(c, vdupq_n_u8(b'F')));
    let is_lower = vandq_u8(vcgeq_u8(c, vdupq_n_u8(b'a')), vcleq_u8(c, vdupq_n_u8(b'f')));
    vminvq_u8(vorrq_u8(vorrq_u8(is_digit, is_upper), is_lower))
}

/// NEON: validate + decode 64 hex chars to 32 bytes. Returns None if any char isn't `[0-9A-Fa-f]`.
/// Validation reuses the same vector loads as the decode, so a clean id costs one extra range-test
/// pass over the (L1-hot) bytes — same speed class as the unchecked decode.
#[cfg(target_arch = "aarch64")]
#[inline]
unsafe fn hex_decode_32_neon_checked(h: &[u8]) -> Option<[u8; 32]> {
    let hex_0 = vld1q_u8(h.as_ptr());
    let hex_1 = vld1q_u8(h.as_ptr().add(16));
    let hex_2 = vld1q_u8(h.as_ptr().add(32));
    let hex_3 = vld1q_u8(h.as_ptr().add(48));
    if (neon_all_hex(hex_0) & neon_all_hex(hex_1) & neon_all_hex(hex_2) & neon_all_hex(hex_3)) != 0xFF {
        return None;
    }
    Some(hex_decode_32_neon(h))
}

/// x86_64 SIMD implementation
#[cfg(target_arch = "x86_64")]
#[inline]
pub fn hex_to_bytes_32(hex: &str) -> [u8; 32] {
    let h = hex.as_bytes();

    if h.len() >= 64 {
        // SAFETY: We verified length >= 64, and hex_decode_32_sse2 only reads first 64 bytes
        let arr: &[u8; 64] = h[..64].try_into().unwrap();
        return unsafe { hex_decode_32_sse2(arr) };
    }

    hex_to_bytes_32_scalar_padded(h)
}

/// SSE2 implementation: decode 64 hex chars to 32 bytes
///
/// Uses the same algorithm as NEON: `(char & 0x0F) + 9*(char has bit 0x40 set)`
/// This correctly handles '0'-'9', 'A'-'F', and 'a'-'f'.
///
/// # Safety
/// Caller must ensure input contains only valid hex characters.
/// Invalid input produces garbage output (no validation performed).
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
#[inline]
unsafe fn hex_decode_32_sse2(h: &[u8; 64]) -> [u8; 32] {
    let mut result = [0u8; 32];

    // Same algorithm as NEON: (char & 0x0F) + 9 if letter
    let mask_0f = _mm_set1_epi8(0x0F);
    let mask_40 = _mm_set1_epi8(0x40);
    let nine = _mm_set1_epi8(9);
    let hi_mask = _mm_set1_epi16(0x00F0u16 as i16);
    let lo_mask = _mm_set1_epi16(0x000Fu16 as i16);
    let zero = _mm_setzero_si128();

    // Process 16 hex chars -> 8 bytes at a time (4 iterations)
    for chunk in 0..4 {
        let in_offset = chunk * 16;
        let out_offset = chunk * 8;

        let hex_chars = _mm_loadu_si128(h.as_ptr().add(in_offset) as *const __m128i);

        // Convert ASCII to nibbles using NEON-style algorithm:
        // nibble = (char & 0x0F) + ((char & 0x40) == 0x40 ? 9 : 0)
        let lo = _mm_and_si128(hex_chars, mask_0f);
        let masked = _mm_and_si128(hex_chars, mask_40);
        let is_letter = _mm_cmpeq_epi8(masked, mask_40);
        let nine_if_letter = _mm_and_si128(is_letter, nine);
        let nibbles = _mm_add_epi8(lo, nine_if_letter);

        // Pack pairs of nibbles into bytes
        let hi_nibbles = _mm_slli_epi16(nibbles, 4);
        let hi = _mm_and_si128(hi_nibbles, hi_mask);
        let lo_shifted = _mm_and_si128(_mm_srli_epi16(nibbles, 8), lo_mask);
        let combined = _mm_or_si128(hi, lo_shifted);

        let packed = _mm_packus_epi16(combined, zero);
        _mm_storel_epi64(result.as_mut_ptr().add(out_offset) as *mut __m128i, packed);
    }

    result
}

/// SSE2: whether all 16 lanes of `c` are hex `[0-9A-Fa-f]`. Every hex char is < 0x80, so signed
/// `cmpgt` range tests are exact (`c > lo-1 && hi+1 > c`); movemask == 0xFFFF iff every lane valid.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
#[inline]
unsafe fn sse2_chunk_all_hex(c: __m128i) -> bool {
    let is_digit = _mm_and_si128(_mm_cmpgt_epi8(c, _mm_set1_epi8(0x2F)), _mm_cmpgt_epi8(_mm_set1_epi8(0x3A), c));
    let is_upper = _mm_and_si128(_mm_cmpgt_epi8(c, _mm_set1_epi8(0x40)), _mm_cmpgt_epi8(_mm_set1_epi8(0x47), c));
    let is_lower = _mm_and_si128(_mm_cmpgt_epi8(c, _mm_set1_epi8(0x60)), _mm_cmpgt_epi8(_mm_set1_epi8(0x67), c));
    let valid = _mm_or_si128(_mm_or_si128(is_digit, is_upper), is_lower);
    _mm_movemask_epi8(valid) == 0xFFFF
}

/// SSE2: validate + decode 64 hex chars to 32 bytes. None if any char isn't `[0-9A-Fa-f]`.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
#[inline]
unsafe fn hex_decode_32_sse2_checked(h: &[u8; 64]) -> Option<[u8; 32]> {
    let mut chunk = 0;
    while chunk < 4 {
        let c = _mm_loadu_si128(h.as_ptr().add(chunk * 16) as *const __m128i);
        if !sse2_chunk_all_hex(c) {
            return None;
        }
        chunk += 1;
    }
    Some(hex_decode_32_sse2(h))
}

/// SSE2 implementation: decode 32 hex chars to 16 bytes
///
/// Uses the same algorithm as NEON: `(char & 0x0F) + 9*(char has bit 0x40 set)`
/// This correctly handles '0'-'9', 'A'-'F', and 'a'-'f'.
///
/// # Safety
/// Caller must ensure input contains only valid hex characters.
/// Invalid input produces garbage output (no validation performed).
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
#[inline]
unsafe fn hex_decode_16_sse2(h: &[u8; 32]) -> [u8; 16] {
    let mut result = [0u8; 16];

    // Same algorithm as NEON: (char & 0x0F) + 9 if letter
    let mask_0f = _mm_set1_epi8(0x0F);
    let mask_40 = _mm_set1_epi8(0x40);
    let nine = _mm_set1_epi8(9);
    let hi_mask = _mm_set1_epi16(0x00F0u16 as i16);
    let lo_mask = _mm_set1_epi16(0x000Fu16 as i16);
    let zero = _mm_setzero_si128();

    // Process 16 hex chars -> 8 bytes at a time (2 iterations for 16 bytes)
    for chunk in 0..2 {
        let in_offset = chunk * 16;
        let out_offset = chunk * 8;

        let hex_chars = _mm_loadu_si128(h.as_ptr().add(in_offset) as *const __m128i);

        // Convert ASCII to nibbles using NEON-style algorithm:
        // nibble = (char & 0x0F) + ((char & 0x40) == 0x40 ? 9 : 0)
        let lo = _mm_and_si128(hex_chars, mask_0f);
        let masked = _mm_and_si128(hex_chars, mask_40);
        let is_letter = _mm_cmpeq_epi8(masked, mask_40);
        let nine_if_letter = _mm_and_si128(is_letter, nine);
        let nibbles = _mm_add_epi8(lo, nine_if_letter);

        // Pack pairs of nibbles into bytes
        let hi_nibbles = _mm_slli_epi16(nibbles, 4);
        let hi = _mm_and_si128(hi_nibbles, hi_mask);
        let lo_shifted = _mm_and_si128(_mm_srli_epi16(nibbles, 8), lo_mask);
        let combined = _mm_or_si128(hi, lo_shifted);

        let packed = _mm_packus_epi16(combined, zero);
        _mm_storel_epi64(result.as_mut_ptr().add(out_offset) as *mut __m128i, packed);
    }

    result
}

/// Scalar fallback for other platforms
#[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
#[inline]
pub fn hex_to_bytes_32(hex: &str) -> [u8; 32] {
    let h = hex.as_bytes();

    if h.len() >= 64 {
        let mut bytes = [0u8; 32];
        for i in 0..32 {
            bytes[i] = (HEX_DECODE_LUT[h[i * 2] as usize] << 4)
                     | HEX_DECODE_LUT[h[i * 2 + 1] as usize];
        }
        return bytes;
    }

    hex_to_bytes_32_scalar_padded(h)
}

/// Scalar helper for short/padded hex strings (all platforms)
#[inline]
fn hex_to_bytes_32_scalar_padded(h: &[u8]) -> [u8; 32] {
    let mut bytes = [0u8; 32];
    let hex_len = h.len();
    let start_idx = (64 - hex_len) / 2;
    let mut out_idx = start_idx / 2;

    let mut i = 0;
    while i + 1 < hex_len && out_idx < 32 {
        bytes[out_idx] = (HEX_DECODE_LUT[h[i] as usize] << 4)
                       | HEX_DECODE_LUT[h[i + 1] as usize];
        out_idx += 1;
        i += 2;
    }
    bytes
}

/// Validating decode of EXACTLY 64 hex chars → `[u8; 32]`, for PUBLICLY-obtained ids (inbound
/// events). A valid signature attests authorship, NOT that a tag is well-formed hex, so a hostile
/// peer can still ship garbage here — this returns `None` unless `hex` is precisely 64 ASCII hex
/// digits. Validation runs in-register on the SIMD path (NEON / SSE2), keeping the network boundary
/// in the same speed class as the unchecked decode. Deterministic/internal ids (DB rows, our own
/// encrypted self-lists, frontend command params) should use the infallible [`hex_to_bytes_32`].
#[cfg(target_arch = "aarch64")]
#[inline]
pub fn hex_to_bytes_32_checked(hex: &str) -> Option<[u8; 32]> {
    let h = hex.as_bytes();
    if h.len() != 64 {
        return None;
    }
    unsafe { hex_decode_32_neon_checked(h) }
}

/// SSE2 variant — see the aarch64 `hex_to_bytes_32_checked` for semantics.
#[cfg(target_arch = "x86_64")]
#[inline]
pub fn hex_to_bytes_32_checked(hex: &str) -> Option<[u8; 32]> {
    let h = hex.as_bytes();
    if h.len() != 64 {
        return None;
    }
    let arr: &[u8; 64] = h[..64].try_into().unwrap();
    unsafe { hex_decode_32_sse2_checked(arr) }
}

/// Scalar fallback — see the aarch64 `hex_to_bytes_32_checked` for semantics.
#[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
#[inline]
pub fn hex_to_bytes_32_checked(hex: &str) -> Option<[u8; 32]> {
    let h = hex.as_bytes();
    if h.len() != 64 || !h.iter().all(u8::is_ascii_hexdigit) {
        return None;
    }
    Some(hex_to_bytes_32(hex))
}

// ============================================================================
// Hex Decoding - 16 bytes
// ============================================================================

/// Convert hex string to fixed 16-byte array.
///
/// # Performance
/// - NEON (ARM64): ~2 ns
/// - Scalar fallback: ~10 ns
#[cfg(target_arch = "aarch64")]
#[inline]
pub fn hex_to_bytes_16(hex: &str) -> [u8; 16] {
    let h = hex.as_bytes();

    if h.len() >= 32 {
        return unsafe { hex_decode_16_neon(h) };
    }

    hex_to_bytes_16_scalar_padded(h)
}

/// NEON implementation: decode 32 hex chars to 16 bytes
///
/// Uses the same optimized algorithm as hex_decode_32_neon:
/// - Simplified nibble conversion: (char & 0x0F) + 9*(char has bit 0x40 set)
/// - SLI instruction to combine nibbles in one operation
#[cfg(target_arch = "aarch64")]
#[inline]
unsafe fn hex_decode_16_neon(h: &[u8]) -> [u8; 16] {
    let mut result = [0u8; 16];

    let mask_0f = vdupq_n_u8(0x0F);
    let mask_40 = vdupq_n_u8(0x40);
    let nine = vdupq_n_u8(9);

    // Load 32 hex characters
    let hex_0 = vld1q_u8(h.as_ptr());
    let hex_1 = vld1q_u8(h.as_ptr().add(16));

    // Convert ASCII to nibbles: (char & 0x0F) + 9 if letter
    let lo0 = vandq_u8(hex_0, mask_0f);
    let lo1 = vandq_u8(hex_1, mask_0f);

    let is_letter0 = vtstq_u8(hex_0, mask_40);
    let is_letter1 = vtstq_u8(hex_1, mask_40);

    let n0 = vaddq_u8(lo0, vandq_u8(is_letter0, nine));
    let n1 = vaddq_u8(lo1, vandq_u8(is_letter1, nine));

    // Pack nibbles using UZP + SLI
    let evens = vuzp1q_u8(n0, n1);
    let odds = vuzp2q_u8(n0, n1);
    let bytes = vsliq_n_u8(odds, evens, 4);

    vst1q_u8(result.as_mut_ptr(), bytes);
    result
}

#[cfg(target_arch = "x86_64")]
#[inline]
pub fn hex_to_bytes_16(hex: &str) -> [u8; 16] {
    let h = hex.as_bytes();

    if h.len() >= 32 {
        // SAFETY: We verified length >= 32, and hex_decode_16_sse2 only reads first 32 bytes
        let arr: &[u8; 32] = h[..32].try_into().unwrap();
        return unsafe { hex_decode_16_sse2(arr) };
    }

    hex_to_bytes_16_scalar_padded(h)
}

#[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
#[inline]
pub fn hex_to_bytes_16(hex: &str) -> [u8; 16] {
    let h = hex.as_bytes();

    if h.len() >= 32 {
        let mut bytes = [0u8; 16];
        for i in 0..16 {
            bytes[i] = (HEX_DECODE_LUT[h[i * 2] as usize] << 4)
                     | HEX_DECODE_LUT[h[i * 2 + 1] as usize];
        }
        return bytes;
    }

    hex_to_bytes_16_scalar_padded(h)
}

#[inline]
fn hex_to_bytes_16_scalar_padded(h: &[u8]) -> [u8; 16] {
    let mut bytes = [0u8; 16];
    let hex_len = h.len();
    let start_idx = (32 - hex_len) / 2;
    let mut out_idx = start_idx / 2;

    let mut i = 0;
    while i + 1 < hex_len && out_idx < 16 {
        bytes[out_idx] = (HEX_DECODE_LUT[h[i] as usize] << 4)
                       | HEX_DECODE_LUT[h[i + 1] as usize];
        out_idx += 1;
        i += 2;
    }
    bytes
}

// ============================================================================
// Hex Decoding - Variable Length
// ============================================================================

/// Convert hex string to bytes (arbitrary length).
///
/// Uses SIMD for the bulk of the conversion when input is large enough.
// `set_len` before init is deliberate: every architecture branch writes all `out_len` bytes
// through `out_ptr` before `result` is read, so pre-zeroing would only waste a memset on a hot path.
#[allow(clippy::uninit_vec)]
pub fn hex_string_to_bytes(s: &str) -> Vec<u8> {
    let h = s.as_bytes();
    let out_len = h.len() / 2;
    let mut result = Vec::with_capacity(out_len);

    #[cfg(target_arch = "aarch64")]
    unsafe {
        result.set_len(out_len);
        let out_ptr: *mut u8 = result.as_mut_ptr();

        let mask_0f = vdupq_n_u8(0x0F);
        let mask_40 = vdupq_n_u8(0x40);
        let nine = vdupq_n_u8(9);

        let chunks = out_len / 16; // 32 hex chars -> 16 bytes per chunk
        for chunk in 0..chunks {
            let in_offset = chunk * 32;
            let out_offset = chunk * 16;

            // Load 32 hex characters
            let hex_0 = vld1q_u8(h.as_ptr().add(in_offset));
            let hex_1 = vld1q_u8(h.as_ptr().add(in_offset + 16));

            // Convert ASCII to nibbles: (char & 0x0F) + 9 if letter
            let lo0 = vandq_u8(hex_0, mask_0f);
            let lo1 = vandq_u8(hex_1, mask_0f);

            let is_letter0 = vtstq_u8(hex_0, mask_40);
            let is_letter1 = vtstq_u8(hex_1, mask_40);

            let n0 = vaddq_u8(lo0, vandq_u8(is_letter0, nine));
            let n1 = vaddq_u8(lo1, vandq_u8(is_letter1, nine));

            // Pack nibbles using UZP + SLI
            let evens = vuzp1q_u8(n0, n1);
            let odds = vuzp2q_u8(n0, n1);
            let bytes = vsliq_n_u8(odds, evens, 4);

            vst1q_u8(out_ptr.add(out_offset), bytes);
        }

        // Scalar remainder
        let remainder_start = chunks * 32;
        let mut out_idx = chunks * 16;
        let mut i = remainder_start;
        while i + 1 < h.len() {
            *out_ptr.add(out_idx) = (HEX_DECODE_LUT[h[i] as usize] << 4)
                                  | HEX_DECODE_LUT[h[i + 1] as usize];
            out_idx += 1;
            i += 2;
        }
    }

    #[cfg(target_arch = "x86_64")]
    unsafe {
        result.set_len(out_len);
        let out_ptr: *mut u8 = result.as_mut_ptr();

        let mask_0f = _mm_set1_epi8(0x0F);
        let mask_40 = _mm_set1_epi8(0x40);
        let nine = _mm_set1_epi8(9);
        let hi_mask = _mm_set1_epi16(0x00F0u16 as i16);
        let lo_mask = _mm_set1_epi16(0x000Fu16 as i16);
        let zero = _mm_setzero_si128();

        let chunks = out_len / 8; // 16 hex chars → 8 output bytes per SSE2 iteration
        for chunk in 0..chunks {
            let in_offset = chunk * 16;
            let out_offset = chunk * 8;

            let hex_chars = _mm_loadu_si128(h.as_ptr().add(in_offset) as *const __m128i);

            // Convert ASCII to nibbles: (char & 0x0F) + 9 if letter (bit 0x40 set)
            let lo = _mm_and_si128(hex_chars, mask_0f);
            let masked = _mm_and_si128(hex_chars, mask_40);
            let is_letter = _mm_cmpeq_epi8(masked, mask_40);
            let nine_if_letter = _mm_and_si128(is_letter, nine);
            let nibbles = _mm_add_epi8(lo, nine_if_letter);

            // Pack pairs of nibbles into bytes (same as hex_decode_32_sse2)
            let hi_nibbles = _mm_slli_epi16(nibbles, 4);
            let hi = _mm_and_si128(hi_nibbles, hi_mask);
            let lo_shifted = _mm_and_si128(_mm_srli_epi16(nibbles, 8), lo_mask);
            let combined = _mm_or_si128(hi, lo_shifted);

            let packed = _mm_packus_epi16(combined, zero);
            _mm_storel_epi64(out_ptr.add(out_offset) as *mut __m128i, packed);
        }

        // Scalar remainder
        let remainder_start = chunks * 16;
        let mut out_idx = chunks * 8;
        let mut i = remainder_start;
        while i + 1 < h.len() {
            *out_ptr.add(out_idx) = (HEX_DECODE_LUT[h[i] as usize] << 4)
                                  | HEX_DECODE_LUT[h[i + 1] as usize];
            out_idx += 1;
            i += 2;
        }
    }

    #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
    {
        for chunk in h.chunks(2) {
            if chunk.len() == 2 {
                result.push(
                    (HEX_DECODE_LUT[chunk[0] as usize] << 4) | HEX_DECODE_LUT[chunk[1] as usize]
                );
            }
        }
    }

    result
}

/// Whether every byte of `h` is an ASCII hex digit. Full 16-byte chunks are checked in-register with
/// the same lane-validator the fixed-32 path uses; the sub-16 remainder falls to a scalar scan.
#[cfg(target_arch = "aarch64")]
#[inline]
fn all_ascii_hex(h: &[u8]) -> bool {
    let chunks = h.len() / 16;
    unsafe {
        let mut acc = 0xFFu8;
        for i in 0..chunks {
            acc &= neon_all_hex(vld1q_u8(h.as_ptr().add(i * 16)));
        }
        if acc != 0xFF {
            return false;
        }
    }
    h[chunks * 16..].iter().all(u8::is_ascii_hexdigit)
}

/// SSE2 twin of the NEON `all_ascii_hex` — 16-byte chunks validated in-register, scalar remainder.
#[cfg(target_arch = "x86_64")]
#[inline]
fn all_ascii_hex(h: &[u8]) -> bool {
    let chunks = h.len() / 16;
    unsafe {
        for i in 0..chunks {
            let c = _mm_loadu_si128(h.as_ptr().add(i * 16) as *const __m128i);
            if !sse2_chunk_all_hex(c) {
                return false;
            }
        }
    }
    h[chunks * 16..].iter().all(u8::is_ascii_hexdigit)
}

/// Scalar fallback validator.
#[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
#[inline]
fn all_ascii_hex(h: &[u8]) -> bool {
    h.iter().all(u8::is_ascii_hexdigit)
}

/// Validating arbitrary-length hex decode → `Some(bytes)` only when `s` is even-length and every char
/// is `[0-9A-Fa-f]`, else `None`. The infallible [`hex_string_to_bytes`] silently maps non-hex to 0, so
/// this is the variant for decoding hex that could be malformed (corrupt storage, untrusted input).
/// Validation runs in-register on the SIMD path (full 16-char chunks), so the check is the same speed
/// class as the decode it guards.
#[inline]
pub fn hex_string_to_bytes_checked(s: &str) -> Option<Vec<u8>> {
    let h = s.as_bytes();
    if h.len() % 2 != 0 || !all_ascii_hex(h) {
        return None;
    }
    Some(hex_string_to_bytes(s))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checked_varlen_decode_accepts_valid_and_rejects_garbage() {
        // Even-length hex of assorted lengths (incl. non-multiples of 16 chars, exercising the
        // SIMD chunk + scalar remainder) decodes identically to the infallible path.
        for len_bytes in [0usize, 1, 7, 8, 16, 39, 78, 256] {
            let bytes: Vec<u8> = (0..len_bytes).map(|i| (i * 7 + 3) as u8).collect();
            let hex = bytes_to_hex_string(&bytes);
            assert_eq!(hex_string_to_bytes_checked(&hex), Some(hex_string_to_bytes(&hex)), "{len_bytes}B");
            assert_eq!(hex_string_to_bytes_checked(&hex).unwrap(), bytes, "{len_bytes}B roundtrip");
        }
        // Uppercase accepted too.
        assert!(hex_string_to_bytes_checked("DEADBEEF").is_some());
        // Odd length → None.
        assert_eq!(hex_string_to_bytes_checked("abc"), None);
        // Non-hex in a FULL 16-char chunk (position 5) → None.
        assert_eq!(hex_string_to_bytes_checked("00112g3344556677"), None, "bad char in SIMD chunk");
        // Non-hex in the sub-16 REMAINDER (a 20-char string: 16-char chunk OK, bad char at 18) → None.
        assert_eq!(hex_string_to_bytes_checked("00112233445566778z99"), None, "bad char in remainder");
        // Empty → Some(empty).
        assert_eq!(hex_string_to_bytes_checked(""), Some(Vec::new()));
    }

    #[test]
    fn checked_decode_accepts_valid_and_rejects_garbage() {
        // Valid lowercase, uppercase, and mixed all decode to the SAME bytes as the unchecked path.
        let lower = "00112233445566778899aabbccddeeff0123456789abcdeffedcba9876543210";
        let upper = lower.to_uppercase();
        assert_eq!(hex_to_bytes_32_checked(lower), Some(hex_to_bytes_32(lower)));
        assert_eq!(hex_to_bytes_32_checked(&upper), Some(hex_to_bytes_32(lower)));
        assert_eq!(hex_to_bytes_32_checked(lower).unwrap()[0], 0x00);
        assert_eq!(hex_to_bytes_32_checked(lower).unwrap()[31], 0x10);

        // Wrong length → None (too short, too long, empty).
        assert_eq!(hex_to_bytes_32_checked(&lower[..63]), None);
        assert_eq!(hex_to_bytes_32_checked(&format!("{lower}0")), None);
        assert_eq!(hex_to_bytes_32_checked(""), None);

        // A single non-hex char anywhere → None (first, middle, last byte positions).
        let mut bad = lower.to_string();
        bad.replace_range(0..1, "g");
        assert_eq!(hex_to_bytes_32_checked(&bad), None, "non-hex at start");
        let mut bad = lower.to_string();
        bad.replace_range(32..33, "Z");
        assert_eq!(hex_to_bytes_32_checked(&bad), None, "non-hex in middle");
        let mut bad = lower.to_string();
        bad.replace_range(63..64, " ");
        assert_eq!(hex_to_bytes_32_checked(&bad), None, "non-hex at end");

        // Range-boundary chars: the byte just outside each valid run must be rejected.
        // '/' (0x2F) | ':' (0x3A) | '@' (0x40) | 'G' (0x47) | '`' (0x60) | 'g' (0x67).
        for bad_char in ['/', ':', '@', 'G', '`', 'g'] {
            let mut s = lower.to_string();
            s.replace_range(10..11, &bad_char.to_string());
            assert_eq!(hex_to_bytes_32_checked(&s), None, "boundary char {bad_char:?} must reject");
        }
        // ...and the inclusive endpoints must be accepted.
        for ok_char in ['0', '9', 'a', 'f', 'A', 'F'] {
            let mut s = lower.to_string();
            s.replace_range(10..11, &ok_char.to_string());
            assert!(hex_to_bytes_32_checked(&s).is_some(), "valid char {ok_char:?} must accept");
        }
    }

    #[test]
    fn test_hex_encode_32() {
        let bytes: [u8; 32] = [
            0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77,
            0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff,
            0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef,
            0xfe, 0xdc, 0xba, 0x98, 0x76, 0x54, 0x32, 0x10,
        ];
        let hex = bytes_to_hex_32(&bytes);
        assert_eq!(hex, "00112233445566778899aabbccddeeff0123456789abcdeffedcba9876543210");
    }

    #[test]
    fn test_hex_decode_32() {
        let hex = "00112233445566778899aabbccddeeff0123456789abcdeffedcba9876543210";
        let bytes = hex_to_bytes_32(hex);
        assert_eq!(bytes[0], 0x00);
        assert_eq!(bytes[15], 0xff);
        assert_eq!(bytes[31], 0x10);
    }

    #[test]
    fn test_roundtrip() {
        let original: [u8; 32] = [42; 32];
        let hex = bytes_to_hex_32(&original);
        let decoded = hex_to_bytes_32(&hex);
        assert_eq!(original, decoded);
    }

    #[test]
    fn test_hex_decode_16() {
        let hex = "00112233445566778899aabbccddeeff";
        let bytes = hex_to_bytes_16(hex);
        assert_eq!(bytes[0], 0x00);
        assert_eq!(bytes[7], 0x77);
        assert_eq!(bytes[15], 0xff);
    }

    #[test]
    fn test_hex_decode_uppercase() {
        // Test that uppercase hex is decoded correctly
        let lowercase = "00112233445566778899aabbccddeeff0123456789abcdeffedcba9876543210";
        let uppercase = "00112233445566778899AABBCCDDEEFF0123456789ABCDEFFEDCBA9876543210";
        assert_eq!(hex_to_bytes_32(lowercase), hex_to_bytes_32(uppercase));
    }

    #[test]
    fn test_hex_string_to_bytes() {
        let hex = "deadbeef";
        let bytes = hex_string_to_bytes(hex);
        assert_eq!(bytes, vec![0xde, 0xad, 0xbe, 0xef]);
    }

    #[test]
    fn test_hex_string_to_bytes_long() {
        // Test variable-length decode with longer input (uses SIMD path)
        let hex = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";
        let bytes = hex_string_to_bytes(hex);
        assert_eq!(bytes.len(), 32);
        assert_eq!(bytes[0], 0x00);
        assert_eq!(bytes[15], 0xff);
        assert_eq!(bytes[31], 0xff);
    }

    #[test]
    fn test_roundtrip_16() {
        let original: [u8; 16] = [0xab; 16];
        let hex = bytes_to_hex_16(&original);
        let decoded = hex_to_bytes_16(&hex);
        assert_eq!(original, decoded);
    }

}
