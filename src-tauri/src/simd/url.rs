//! SIMD-accelerated URL delimiter scanning
//!
//! Scans 16 bytes at a time for whitespace or URL-terminating characters.
//! Benchmarked at 4.7-5.2x faster than scalar for typical URL lengths (35-57 bytes).

/// LUT for URL delimiter detection (used by scalar remainder + NEON first-match)
const IS_URL_DELIM: [bool; 256] = {
    let mut t = [false; 256];
    t[b' ' as usize] = true;
    t[b'\t' as usize] = true;
    t[b'\n' as usize] = true;
    t[b'\r' as usize] = true;
    t[b'"' as usize] = true;
    t[b'<' as usize] = true;
    t[b'>' as usize] = true;
    t[b')' as usize] = true;
    t[b']' as usize] = true;
    t[b'}' as usize] = true;
    t[b'|' as usize] = true;
    t
};

/// Find the index of the first URL-terminating delimiter in a byte slice.
/// Returns `bytes.len()` if no delimiter is found.
///
/// Delimiters: whitespace (`\t \n \r`), `"`, `<`, `>`, `)`, `]`, `}`, `|`
#[inline]
pub fn find_url_delimiter(bytes: &[u8]) -> usize {
    let len = bytes.len();
    let mut i = 0;

    #[cfg(target_arch = "aarch64")]
    unsafe {
        use std::arch::aarch64::*;
        let space = vdupq_n_u8(b' ');
        let tab = vdupq_n_u8(b'\t');
        let nl = vdupq_n_u8(b'\n');
        let cr = vdupq_n_u8(b'\r');
        let quote = vdupq_n_u8(b'"');
        let lt = vdupq_n_u8(b'<');
        let gt = vdupq_n_u8(b'>');
        let rparen = vdupq_n_u8(b')');
        let rbracket = vdupq_n_u8(b']');
        let rbrace = vdupq_n_u8(b'}');
        let pipe = vdupq_n_u8(b'|');

        while i + 16 <= len {
            let chunk = vld1q_u8(bytes.as_ptr().add(i));
            let mut hits = vceqq_u8(chunk, space);
            hits = vorrq_u8(hits, vceqq_u8(chunk, tab));
            hits = vorrq_u8(hits, vceqq_u8(chunk, nl));
            hits = vorrq_u8(hits, vceqq_u8(chunk, cr));
            hits = vorrq_u8(hits, vceqq_u8(chunk, quote));
            hits = vorrq_u8(hits, vceqq_u8(chunk, lt));
            hits = vorrq_u8(hits, vceqq_u8(chunk, gt));
            hits = vorrq_u8(hits, vceqq_u8(chunk, rparen));
            hits = vorrq_u8(hits, vceqq_u8(chunk, rbracket));
            hits = vorrq_u8(hits, vceqq_u8(chunk, rbrace));
            hits = vorrq_u8(hits, vceqq_u8(chunk, pipe));

            if vmaxvq_u8(hits) != 0 {
                for j in 0..16 {
                    if IS_URL_DELIM[bytes[i + j] as usize] { return i + j; }
                }
            }
            i += 16;
        }
    }

    #[cfg(target_arch = "x86_64")]
    unsafe {
        use std::arch::x86_64::*;
        let space = _mm_set1_epi8(b' ' as i8);
        let tab = _mm_set1_epi8(b'\t' as i8);
        let nl = _mm_set1_epi8(b'\n' as i8);
        let cr = _mm_set1_epi8(b'\r' as i8);
        let quote = _mm_set1_epi8(b'"' as i8);
        let lt = _mm_set1_epi8(b'<' as i8);
        let gt = _mm_set1_epi8(b'>' as i8);
        let rparen = _mm_set1_epi8(b')' as i8);
        let rbracket = _mm_set1_epi8(b']' as i8);
        let rbrace = _mm_set1_epi8(b'}' as i8);
        let pipe = _mm_set1_epi8(b'|' as i8);

        while i + 16 <= len {
            let chunk = _mm_loadu_si128(bytes.as_ptr().add(i) as *const __m128i);
            let mut hits = _mm_cmpeq_epi8(chunk, space);
            hits = _mm_or_si128(hits, _mm_cmpeq_epi8(chunk, tab));
            hits = _mm_or_si128(hits, _mm_cmpeq_epi8(chunk, nl));
            hits = _mm_or_si128(hits, _mm_cmpeq_epi8(chunk, cr));
            hits = _mm_or_si128(hits, _mm_cmpeq_epi8(chunk, quote));
            hits = _mm_or_si128(hits, _mm_cmpeq_epi8(chunk, lt));
            hits = _mm_or_si128(hits, _mm_cmpeq_epi8(chunk, gt));
            hits = _mm_or_si128(hits, _mm_cmpeq_epi8(chunk, rparen));
            hits = _mm_or_si128(hits, _mm_cmpeq_epi8(chunk, rbracket));
            hits = _mm_or_si128(hits, _mm_cmpeq_epi8(chunk, rbrace));
            hits = _mm_or_si128(hits, _mm_cmpeq_epi8(chunk, pipe));

            let mask = _mm_movemask_epi8(hits);
            if mask != 0 {
                return i + mask.trailing_zeros() as usize;
            }
            i += 16;
        }
    }

    // Scalar remainder (and fallback for other architectures)
    while i < len {
        if IS_URL_DELIM[bytes[i] as usize] { return i; }
        i += 1;
    }
    len
}
