//! SIMD-accelerated image operations
//!
//! # Performance (27 MP image, 109 MB RGBA)
//!
//! | Function | Scalar | SIMD + Parallel | Speedup |
//! |----------|--------|-----------------|---------|
//! | `has_alpha_transparency` | 5.37ms | 0.59ms | 9.1x |
//! | `set_all_alpha_opaque` | 3.08ms | 0.67ms | 4.6x |
//!
//! Theoretical minimum at 200 GB/s memory bandwidth: 0.55ms
//!
//! # Platform Support
//!
//! - **ARM64**: NEON (vld3/vst4 for RGB↔RGBA, TBL for lookups)
//! - **x86_64**: AVX2/SSSE3 (pshufb for byte rearrangement) or SSE2 fallback
//!
//! # Algorithms
//!
//! **Alpha check/set** - Processes 128 bytes (32 RGBA pixels) per iteration:
//! 1. Load 8 x 16-byte chunks into SIMD registers
//! 2. AND/OR all chunks to combine alpha checks/sets
//! 3. Check alpha bytes at positions 3, 7, 11, 15 (every 4th byte)
//! 4. For large images (>4MB), parallelize across CPU cores with rayon
//!
//! **RGB→RGBA conversion** - Uses architecture-specific deinterleaving:
//! - NEON: vld3q loads RGB planes directly, vst4q stores RGBA
//! - SSSE3: pshufb rearranges 12 RGB bytes → 16 RGBA bytes per iteration

#[cfg(target_arch = "aarch64")]
use std::arch::aarch64::*;

#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::*;

use rayon::prelude::*;

// ============================================================================
// Alpha Transparency Check
// ============================================================================

/// NEON-optimized alpha check - processes 128 bytes (32 pixels) per iteration
#[cfg(target_arch = "aarch64")]
#[inline]
fn has_alpha_neon(rgba_pixels: &[u8]) -> bool {
    unsafe {
        let len = rgba_pixels.len();
        let ptr = rgba_pixels.as_ptr();
        let mut i = 0;

        // Process 128 bytes (32 pixels) at a time
        while i + 128 <= len {
            // Load 8 x 16-byte chunks
            let c0 = vld1q_u8(ptr.add(i));
            let c1 = vld1q_u8(ptr.add(i + 16));
            let c2 = vld1q_u8(ptr.add(i + 32));
            let c3 = vld1q_u8(ptr.add(i + 48));
            let c4 = vld1q_u8(ptr.add(i + 64));
            let c5 = vld1q_u8(ptr.add(i + 80));
            let c6 = vld1q_u8(ptr.add(i + 96));
            let c7 = vld1q_u8(ptr.add(i + 112));

            // AND all chunks together - if any alpha was < 255, it will show
            let and01 = vandq_u8(c0, c1);
            let and23 = vandq_u8(c2, c3);
            let and45 = vandq_u8(c4, c5);
            let and67 = vandq_u8(c6, c7);
            let and0123 = vandq_u8(and01, and23);
            let and4567 = vandq_u8(and45, and67);
            let and_all = vandq_u8(and0123, and4567);

            // Check alpha positions (bytes 3, 7, 11, 15 in each 16-byte chunk)
            let a3 = vgetq_lane_u8(and_all, 3);
            let a7 = vgetq_lane_u8(and_all, 7);
            let a11 = vgetq_lane_u8(and_all, 11);
            let a15 = vgetq_lane_u8(and_all, 15);

            if (a3 & a7 & a11 & a15) != 255 {
                return true;
            }
            i += 128;
        }

        // Process remaining 16 bytes at a time
        while i + 16 <= len {
            let c = vld1q_u8(ptr.add(i));
            let a3 = vgetq_lane_u8(c, 3);
            let a7 = vgetq_lane_u8(c, 7);
            let a11 = vgetq_lane_u8(c, 11);
            let a15 = vgetq_lane_u8(c, 15);
            if (a3 & a7 & a11 & a15) != 255 {
                return true;
            }
            i += 16;
        }

        // Handle remainder with scalar
        while i + 4 <= len {
            if rgba_pixels[i + 3] < 255 {
                return true;
            }
            i += 4;
        }
        false
    }
}

// ============================================================================
// Alpha Transparency Check - x86_64 SIMD (SSE2 + AVX2)
// ============================================================================

/// AVX2-optimized alpha check - processes 128 bytes (32 pixels) per iteration
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
#[inline]
unsafe fn has_alpha_avx2(rgba_pixels: &[u8]) -> bool {
    let len = rgba_pixels.len();
    let ptr = rgba_pixels.as_ptr();
    let mut i = 0;

    // All 0xFF for comparison
    let all_255 = _mm256_set1_epi8(-1i8); // 0xFF

    // Process 128 bytes (32 pixels) at a time using 4 x 256-bit loads
    while i + 128 <= len {
        // Load and AND 4 x 32-byte chunks
        let c0 = _mm256_loadu_si256(ptr.add(i) as *const __m256i);
        let c1 = _mm256_loadu_si256(ptr.add(i + 32) as *const __m256i);
        let c2 = _mm256_loadu_si256(ptr.add(i + 64) as *const __m256i);
        let c3 = _mm256_loadu_si256(ptr.add(i + 96) as *const __m256i);

        let and01 = _mm256_and_si256(c0, c1);
        let and23 = _mm256_and_si256(c2, c3);
        let and_all = _mm256_and_si256(and01, and23);

        // Compare with 255 - if any byte < 255, comparison fails for that byte
        let cmp = _mm256_cmpeq_epi8(and_all, all_255);
        let mask = _mm256_movemask_epi8(cmp);

        // Alpha bytes are at positions 3,7,11,15,19,23,27,31 (every 4th byte starting at 3)
        // In the mask, these are bits: 3,7,11,15,19,23,27,31 = 0x88888888
        if (mask as u32 & 0x88888888) != 0x88888888 {
            return true;
        }
        i += 128;
    }

    // Fall back to SSE2 for remainder
    has_alpha_sse2_remainder(&rgba_pixels[i..])
}

/// SSE2-optimized alpha check - processes 64 bytes (16 pixels) per iteration
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
#[inline]
unsafe fn has_alpha_sse2(rgba_pixels: &[u8]) -> bool {
    let len = rgba_pixels.len();
    let ptr = rgba_pixels.as_ptr();
    let mut i = 0;

    let all_255 = _mm_set1_epi8(-1i8); // 0xFF

    // Process 64 bytes (16 pixels) at a time using 4 x 128-bit loads
    while i + 64 <= len {
        let c0 = _mm_loadu_si128(ptr.add(i) as *const __m128i);
        let c1 = _mm_loadu_si128(ptr.add(i + 16) as *const __m128i);
        let c2 = _mm_loadu_si128(ptr.add(i + 32) as *const __m128i);
        let c3 = _mm_loadu_si128(ptr.add(i + 48) as *const __m128i);

        let and01 = _mm_and_si128(c0, c1);
        let and23 = _mm_and_si128(c2, c3);
        let and_all = _mm_and_si128(and01, and23);

        let cmp = _mm_cmpeq_epi8(and_all, all_255);
        let mask = _mm_movemask_epi8(cmp);

        // Alpha bytes at positions 3,7,11,15 = bits 3,7,11,15 = 0x8888
        if (mask & 0x8888) != 0x8888 {
            return true;
        }
        i += 64;
    }

    // Handle remainder
    has_alpha_sse2_remainder(&rgba_pixels[i..])
}

/// SSE2 remainder handler (also used by AVX2)
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
#[inline]
unsafe fn has_alpha_sse2_remainder(rgba_pixels: &[u8]) -> bool {
    let len = rgba_pixels.len();
    let ptr = rgba_pixels.as_ptr();
    let mut i = 0;

    let all_255 = _mm_set1_epi8(-1i8);

    // Process 16 bytes at a time
    while i + 16 <= len {
        let c = _mm_loadu_si128(ptr.add(i) as *const __m128i);
        let cmp = _mm_cmpeq_epi8(c, all_255);
        let mask = _mm_movemask_epi8(cmp);

        if (mask & 0x8888) != 0x8888 {
            return true;
        }
        i += 16;
    }

    // Scalar for final pixels
    while i + 4 <= len {
        if rgba_pixels[i + 3] < 255 {
            return true;
        }
        i += 4;
    }
    false
}

/// x86_64 dispatcher - uses AVX2 if available, otherwise SSE2
#[cfg(target_arch = "x86_64")]
#[inline]
fn has_alpha_simd(rgba_pixels: &[u8]) -> bool {
    unsafe {
        if is_x86_feature_detected!("avx2") {
            has_alpha_avx2(rgba_pixels)
        } else {
            has_alpha_sse2(rgba_pixels)
        }
    }
}

/// Scalar fallback for non-SIMD platforms
#[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
#[inline]
fn has_alpha_simd(rgba_pixels: &[u8]) -> bool {
    // Fast path for little-endian (WASM, RISC-V, most platforms)
    #[cfg(target_endian = "little")]
    {
        let mut chunks = rgba_pixels.chunks_exact(8);
        // On little-endian, alpha bytes are at positions 3 and 7 within each 8-byte chunk
        // which correspond to bits 24-31 and 56-63 in the u64
        const ALPHA_MASK: u64 = 0xFF000000_FF000000;

        for chunk in chunks.by_ref() {
            let val = u64::from_ne_bytes(chunk.try_into().unwrap());
            if (val & ALPHA_MASK) != ALPHA_MASK {
                return true;
            }
        }
        chunks.remainder().chunks_exact(4).any(|px| px[3] < 255)
    }

    // Byte-by-byte fallback for big-endian (rare)
    #[cfg(target_endian = "big")]
    {
        for pixel in rgba_pixels.chunks_exact(4) {
            if pixel[3] < 255 {
                return true;
            }
        }
        false
    }
}

/// Check if RGBA pixel data contains any meaningful transparency (alpha < 255)
///
/// Uses SIMD acceleration with parallel processing for maximum performance:
/// - **ARM64**: NEON
/// - **x86_64**: AVX2 (if available) or SSE2
///
/// Achieves ~0.6ms on 27 MP images (near memory bandwidth limit).
///
/// # Parallelization Strategy
/// - Images < 4 MB: Single-threaded (parallel overhead > benefit)
/// - Images >= 4 MB: 256 KB chunks (optimal for L2 cache + core utilization)
///
/// # Example
/// ```ignore
/// let has_transparency = has_alpha_transparency(&rgba_pixels);
/// if has_transparency {
///     // Image has transparent pixels, encode as PNG
/// } else {
///     // Image is fully opaque, can use JPEG
/// }
/// ```
#[inline]
pub fn has_alpha_transparency(rgba_pixels: &[u8]) -> bool {
    // 256 KB chunks: fits L2 cache, good core utilization
    // Benchmarked: 2-3x faster than 1 MB chunks for large images
    const CHUNK_SIZE: usize = 256 * 1024;
    const PARALLEL_THRESHOLD: usize = 4 * 1024 * 1024; // 4 MB (~1 MP)

    #[cfg(target_arch = "aarch64")]
    let check_fn = has_alpha_neon;

    #[cfg(target_arch = "x86_64")]
    let check_fn = has_alpha_simd;

    #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
    let check_fn = has_alpha_simd;

    // Only parallelize for large images where benefit > overhead
    if rgba_pixels.len() > PARALLEL_THRESHOLD {
        rgba_pixels
            .par_chunks(CHUNK_SIZE)
            .any(|chunk| check_fn(chunk))
    } else {
        check_fn(rgba_pixels)
    }
}

// ============================================================================
// Alpha Near-Zero Check (Windows clipboard bug)
// ============================================================================

/// Check if all alpha values are nearly zero (Windows clipboard bug detection)
///
/// Returns true if ALL pixels have alpha <= 2. This detects a Windows clipboard
/// bug where RGBA images have their alpha channel corrupted to near-zero values.
#[inline]
#[cfg(target_os = "windows")]
pub fn has_all_alpha_near_zero(rgba_pixels: &[u8]) -> bool {
    let mut chunks = rgba_pixels.chunks_exact(8);
    // Mask to check if alpha bytes are > 2: if (alpha & 0xFC) != 0, then alpha > 3
    const ALPHA_HIGH_BITS: u64 = 0xFC000000_FC000000;

    for chunk in chunks.by_ref() {
        let val = u64::from_ne_bytes(chunk.try_into().unwrap());
        if (val & ALPHA_HIGH_BITS) != 0 {
            return false; // Found alpha > 2
        }
    }

    chunks.remainder().chunks_exact(4).all(|px| px[3] <= 2)
}

// ============================================================================
// Set Alpha Opaque
// ============================================================================

/// NEON-optimized alpha set - processes 128 bytes (32 pixels) per iteration
#[cfg(target_arch = "aarch64")]
#[inline]
fn set_alpha_neon(rgba_pixels: &mut [u8]) {
    unsafe {
        let len = rgba_pixels.len();
        let ptr = rgba_pixels.as_mut_ptr();
        let mut i = 0;

        // Alpha mask: 0x00 for RGB, 0xFF for alpha channel
        let mask = vld1q_u8([0, 0, 0, 0xFF, 0, 0, 0, 0xFF, 0, 0, 0, 0xFF, 0, 0, 0, 0xFF].as_ptr());

        // Process 128 bytes (32 pixels) at a time
        while i + 128 <= len {
            vst1q_u8(ptr.add(i), vorrq_u8(vld1q_u8(ptr.add(i)), mask));
            vst1q_u8(ptr.add(i + 16), vorrq_u8(vld1q_u8(ptr.add(i + 16)), mask));
            vst1q_u8(ptr.add(i + 32), vorrq_u8(vld1q_u8(ptr.add(i + 32)), mask));
            vst1q_u8(ptr.add(i + 48), vorrq_u8(vld1q_u8(ptr.add(i + 48)), mask));
            vst1q_u8(ptr.add(i + 64), vorrq_u8(vld1q_u8(ptr.add(i + 64)), mask));
            vst1q_u8(ptr.add(i + 80), vorrq_u8(vld1q_u8(ptr.add(i + 80)), mask));
            vst1q_u8(ptr.add(i + 96), vorrq_u8(vld1q_u8(ptr.add(i + 96)), mask));
            vst1q_u8(ptr.add(i + 112), vorrq_u8(vld1q_u8(ptr.add(i + 112)), mask));
            i += 128;
        }

        // Process remaining 16 bytes at a time
        while i + 16 <= len {
            vst1q_u8(ptr.add(i), vorrq_u8(vld1q_u8(ptr.add(i)), mask));
            i += 16;
        }

        // Handle remainder with scalar
        while i + 4 <= len {
            rgba_pixels[i + 3] = 255;
            i += 4;
        }
    }
}

// ============================================================================
// Set Alpha Opaque - x86_64 SIMD (SSE2 + AVX2)
// ============================================================================

/// AVX2-optimized alpha set - processes 128 bytes (32 pixels) per iteration
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
#[inline]
unsafe fn set_alpha_avx2(rgba_pixels: &mut [u8]) {
    let len = rgba_pixels.len();
    let ptr = rgba_pixels.as_mut_ptr();
    let mut i = 0;

    // Alpha mask: 0xFF at positions 3,7,11,15,19,23,27,31 (alpha bytes)
    let alpha_mask = _mm256_set_epi8(
        -1, 0, 0, 0, -1, 0, 0, 0, -1, 0, 0, 0, -1, 0, 0, 0,
        -1, 0, 0, 0, -1, 0, 0, 0, -1, 0, 0, 0, -1, 0, 0, 0,
    );

    // Process 128 bytes (32 pixels) at a time
    while i + 128 <= len {
        let p0 = ptr.add(i) as *mut __m256i;
        let p1 = ptr.add(i + 32) as *mut __m256i;
        let p2 = ptr.add(i + 64) as *mut __m256i;
        let p3 = ptr.add(i + 96) as *mut __m256i;

        _mm256_storeu_si256(p0, _mm256_or_si256(_mm256_loadu_si256(p0), alpha_mask));
        _mm256_storeu_si256(p1, _mm256_or_si256(_mm256_loadu_si256(p1), alpha_mask));
        _mm256_storeu_si256(p2, _mm256_or_si256(_mm256_loadu_si256(p2), alpha_mask));
        _mm256_storeu_si256(p3, _mm256_or_si256(_mm256_loadu_si256(p3), alpha_mask));

        i += 128;
    }

    // Fall back to SSE2 for remainder
    set_alpha_sse2_remainder(&mut rgba_pixels[i..]);
}

/// SSE2-optimized alpha set - processes 64 bytes (16 pixels) per iteration
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
#[inline]
unsafe fn set_alpha_sse2(rgba_pixels: &mut [u8]) {
    let len = rgba_pixels.len();
    let ptr = rgba_pixels.as_mut_ptr();
    let mut i = 0;

    // Alpha mask: 0xFF at positions 3,7,11,15
    let alpha_mask = _mm_set_epi8(-1, 0, 0, 0, -1, 0, 0, 0, -1, 0, 0, 0, -1, 0, 0, 0);

    // Process 64 bytes (16 pixels) at a time
    while i + 64 <= len {
        let p0 = ptr.add(i) as *mut __m128i;
        let p1 = ptr.add(i + 16) as *mut __m128i;
        let p2 = ptr.add(i + 32) as *mut __m128i;
        let p3 = ptr.add(i + 48) as *mut __m128i;

        _mm_storeu_si128(p0, _mm_or_si128(_mm_loadu_si128(p0), alpha_mask));
        _mm_storeu_si128(p1, _mm_or_si128(_mm_loadu_si128(p1), alpha_mask));
        _mm_storeu_si128(p2, _mm_or_si128(_mm_loadu_si128(p2), alpha_mask));
        _mm_storeu_si128(p3, _mm_or_si128(_mm_loadu_si128(p3), alpha_mask));

        i += 64;
    }

    // Handle remainder
    set_alpha_sse2_remainder(&mut rgba_pixels[i..]);
}

/// SSE2 remainder handler (also used by AVX2)
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
#[inline]
unsafe fn set_alpha_sse2_remainder(rgba_pixels: &mut [u8]) {
    let len = rgba_pixels.len();
    let ptr = rgba_pixels.as_mut_ptr();
    let mut i = 0;

    let alpha_mask = _mm_set_epi8(-1, 0, 0, 0, -1, 0, 0, 0, -1, 0, 0, 0, -1, 0, 0, 0);

    // Process 16 bytes at a time
    while i + 16 <= len {
        let p = ptr.add(i) as *mut __m128i;
        _mm_storeu_si128(p, _mm_or_si128(_mm_loadu_si128(p), alpha_mask));
        i += 16;
    }

    // Scalar for final pixels
    while i + 4 <= len {
        rgba_pixels[i + 3] = 255;
        i += 4;
    }
}

/// x86_64 dispatcher - uses AVX2 if available, otherwise SSE2
#[cfg(target_arch = "x86_64")]
#[inline]
fn set_alpha_simd(rgba_pixels: &mut [u8]) {
    unsafe {
        if is_x86_feature_detected!("avx2") {
            set_alpha_avx2(rgba_pixels);
        } else {
            set_alpha_sse2(rgba_pixels);
        }
    }
}

/// Scalar fallback for non-SIMD platforms
#[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
#[inline]
fn set_alpha_simd(rgba_pixels: &mut [u8]) {
    // Fast path for little-endian (WASM, RISC-V, most platforms)
    #[cfg(target_endian = "little")]
    {
        let mut chunks = rgba_pixels.chunks_exact_mut(8);
        const ALPHA_MASK: u64 = 0xFF000000_FF000000;

        for chunk in chunks.by_ref() {
            let val = u64::from_ne_bytes(chunk.try_into().unwrap());
            chunk.copy_from_slice(&(val | ALPHA_MASK).to_ne_bytes());
        }
        for px in chunks.into_remainder().chunks_exact_mut(4) {
            px[3] = 255;
        }
    }

    // Byte-by-byte fallback for big-endian (rare)
    #[cfg(target_endian = "big")]
    {
        for pixel in rgba_pixels.chunks_exact_mut(4) {
            pixel[3] = 255;
        }
    }
}

/// Set all alpha values to 255 (opaque) in-place
///
/// Uses SIMD acceleration with parallel processing for maximum performance:
/// - **ARM64**: NEON
/// - **x86_64**: AVX2 (if available) or SSE2
///
/// Achieves ~0.7ms on 27 MP images (near memory bandwidth limit).
///
/// # Parallelization Strategy
/// - Images < 4 MB: Single-threaded (parallel overhead > benefit)
/// - Images >= 4 MB: 256 KB chunks (optimal for L2 cache + core utilization)
///
/// # Example
/// ```ignore
/// // Fix Windows clipboard alpha bug
/// set_all_alpha_opaque(&mut rgba_pixels);
/// ```
#[inline]
pub fn set_all_alpha_opaque(rgba_pixels: &mut [u8]) {
    // 256 KB chunks: fits L2 cache, good core utilization
    const CHUNK_SIZE: usize = 256 * 1024;
    const PARALLEL_THRESHOLD: usize = 4 * 1024 * 1024; // 4 MB (~1 MP)

    #[cfg(target_arch = "aarch64")]
    let set_fn = set_alpha_neon;

    #[cfg(target_arch = "x86_64")]
    let set_fn = set_alpha_simd;

    #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
    let set_fn = set_alpha_simd;

    // Only parallelize for large images where benefit > overhead
    if rgba_pixels.len() > PARALLEL_THRESHOLD {
        rgba_pixels
            .par_chunks_mut(CHUNK_SIZE)
            .for_each(|chunk| set_fn(chunk));
    } else {
        set_fn(rgba_pixels);
    }
}

// ============================================================================
// Nearest Neighbor Downsampling - Optimized
// ============================================================================

/// Fast nearest-neighbor downsampling for RGBA images.
///
/// Uses integer arithmetic and direct u32 pixel copies for efficiency.
/// Performance is comparable to float-based implementation but with more
/// predictable results due to integer math.
///
/// # Arguments
/// * `pixels` - Source RGBA pixel data (4 bytes per pixel)
/// * `src_width`, `src_height` - Source image dimensions
/// * `dst_width`, `dst_height` - Target image dimensions
///
/// # Returns
/// Downsampled RGBA pixel data
///
/// # Panics
/// - If `pixels` is smaller than `src_width * src_height * 4`
/// - If output size would overflow
pub fn nearest_neighbor_downsample(
    pixels: &[u8],
    src_width: u32,
    src_height: u32,
    dst_width: u32,
    dst_height: u32,
) -> Vec<u8> {
    // Validate input dimensions
    let src_size = (src_width as usize)
        .checked_mul(src_height as usize)
        .and_then(|n| n.checked_mul(4))
        .expect("source dimensions overflow");
    assert!(pixels.len() >= src_size, "pixels buffer too small for source dimensions");

    // Calculate output size with overflow protection
    let dst_size = (dst_width as usize)
        .checked_mul(dst_height as usize)
        .and_then(|n| n.checked_mul(4))
        .expect("destination dimensions overflow");

    let mut result: Vec<u8> = Vec::with_capacity(dst_size);
    let src_stride = src_width as usize * 4;

    unsafe {
        result.set_len(dst_size);
        let dst_ptr = result.as_mut_ptr() as *mut u32;
        let src_ptr = pixels.as_ptr();
        let mut dst_idx = 0usize;

        for ty in 0..dst_height {
            // Integer division for y coordinate
            let sy = (ty as u64 * src_height as u64 / dst_height as u64) as usize;
            let row_ptr = src_ptr.add(sy * src_stride);

            for tx in 0..dst_width {
                // Integer division for x coordinate
                let sx = (tx as u64 * src_width as u64 / dst_width as u64) as usize;
                // Copy pixel as u32 (4 bytes at once)
                *dst_ptr.add(dst_idx) = *(row_ptr.add(sx * 4) as *const u32);
                dst_idx += 1;
            }
        }
    }

    result
}

// ============================================================================
// RGB to RGBA Conversion - SIMD Optimized
// ============================================================================

/// Convert RGB pixel data to RGBA, setting alpha to 255.
///
/// Uses SIMD acceleration where available:
/// - **ARM64**: NEON with vld3/vst4 deinterleave
/// - **x86_64**: SSSE3 pshufb (with scalar fallback)
///
/// ~4x speedup on large images compared to naive scalar.
#[inline]
pub fn rgb_to_rgba(rgb_data: &[u8]) -> Vec<u8> {
    let pixel_count = rgb_data.len() / 3;
    let mut rgba_data = Vec::with_capacity(pixel_count * 4);

    #[cfg(target_arch = "aarch64")]
    unsafe {
        rgb_to_rgba_neon(rgb_data, &mut rgba_data);
    }

    #[cfg(target_arch = "x86_64")]
    unsafe {
        // SSSE3 is available on all x86_64 CPUs since 2006, but check anyway
        if is_x86_feature_detected!("ssse3") {
            rgb_to_rgba_ssse3(rgb_data, &mut rgba_data);
        } else {
            rgb_to_rgba_scalar_x86(rgb_data, &mut rgba_data);
        }
    }

    #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
    {
        rgb_to_rgba_scalar(rgb_data, &mut rgba_data);
    }

    rgba_data
}

/// NEON-optimized RGB to RGBA conversion
///
/// Uses vld3q to load RGB data deinterleaved, then vst4q to store as RGBA.
/// Unlike SSE/SSSE3, NEON's vld3q loads exactly 48 bytes, so no OOB risk.
#[cfg(target_arch = "aarch64")]
#[inline]
unsafe fn rgb_to_rgba_neon(rgb_data: &[u8], rgba_data: &mut Vec<u8>) {
    let pixel_count = rgb_data.len() / 3;
    let rgba_size = pixel_count.checked_mul(4).expect("RGB to RGBA size overflow");

    rgba_data.clear();
    rgba_data.reserve_exact(rgba_size);
    rgba_data.set_len(rgba_size);

    let src = rgb_data.as_ptr();
    let dst = rgba_data.as_mut_ptr();

    let mut i = 0usize;
    let mut o = 0usize;

    // Process 16 pixels at a time (48 RGB bytes -> 64 RGBA bytes)
    // Using vld3 to deinterleave RGB channels
    while i + 48 <= rgb_data.len() {
        // Load 48 bytes as 16 RGB pixels (deinterleaved into R, G, B planes)
        let rgb = vld3q_u8(src.add(i));

        // Create alpha channel (all 255)
        let alpha = vdupq_n_u8(255);

        // Interleave as RGBA and store
        let rgba = uint8x16x4_t(rgb.0, rgb.1, rgb.2, alpha);
        vst4q_u8(dst.add(o), rgba);

        i += 48;
        o += 64;
    }

    // Scalar remainder
    while i + 3 <= rgb_data.len() {
        *dst.add(o) = *src.add(i);
        *dst.add(o + 1) = *src.add(i + 1);
        *dst.add(o + 2) = *src.add(i + 2);
        *dst.add(o + 3) = 255;
        i += 3;
        o += 4;
    }
}

/// SSSE3-optimized RGB to RGBA conversion using pshufb for byte rearrangement
///
/// Processes 16 pixels (48 RGB bytes → 64 RGBA bytes) per iteration.
/// Uses pshufb to rearrange RGB bytes and insert alpha in one operation.
///
/// # Safety
/// - Caller must ensure SSSE3 is available (use `is_x86_feature_detected!`)
/// - Input length should be a multiple of 3 (trailing 1-2 bytes are ignored)
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "ssse3")]
#[inline]
unsafe fn rgb_to_rgba_ssse3(rgb_data: &[u8], rgba_data: &mut Vec<u8>) {
    let pixel_count = rgb_data.len() / 3;
    // Use checked_mul to prevent overflow on very large inputs
    let rgba_size = pixel_count.checked_mul(4).expect("RGB to RGBA size overflow");

    // Clear and reserve exact capacity to avoid over-allocation on reuse
    rgba_data.clear();
    rgba_data.reserve_exact(rgba_size);
    rgba_data.set_len(rgba_size);

    let src = rgb_data.as_ptr();
    let dst = rgba_data.as_mut_ptr();

    let mut i = 0usize;
    let mut o = 0usize;

    // Shuffle mask: rearranges 12 RGB bytes to 16 RGBA bytes
    // Input positions 0-11 map to output, -1 (0x80) produces zero for alpha slots
    let shuffle = _mm_setr_epi8(
        0, 1, 2, -1,    // pixel 0: R G B 0
        3, 4, 5, -1,    // pixel 1: R G B 0
        6, 7, 8, -1,    // pixel 2: R G B 0
        9, 10, 11, -1   // pixel 3: R G B 0
    );
    // Alpha mask: 0xFF at alpha positions (bytes 3, 7, 11, 15)
    let alpha = _mm_set_epi8(-1, 0, 0, 0, -1, 0, 0, 0, -1, 0, 0, 0, -1, 0, 0, 0);

    // Process 16 pixels at a time (48 RGB bytes -> 64 RGBA bytes)
    // Loop bound: last load is at i+36, reads 16 bytes -> needs i+52 <= len
    while i + 52 <= rgb_data.len() {
        let rgb0 = _mm_loadu_si128(src.add(i) as *const __m128i);
        let rgb1 = _mm_loadu_si128(src.add(i + 12) as *const __m128i);
        let rgb2 = _mm_loadu_si128(src.add(i + 24) as *const __m128i);
        let rgb3 = _mm_loadu_si128(src.add(i + 36) as *const __m128i);

        let rgba0 = _mm_or_si128(_mm_shuffle_epi8(rgb0, shuffle), alpha);
        let rgba1 = _mm_or_si128(_mm_shuffle_epi8(rgb1, shuffle), alpha);
        let rgba2 = _mm_or_si128(_mm_shuffle_epi8(rgb2, shuffle), alpha);
        let rgba3 = _mm_or_si128(_mm_shuffle_epi8(rgb3, shuffle), alpha);

        _mm_storeu_si128(dst.add(o) as *mut __m128i, rgba0);
        _mm_storeu_si128(dst.add(o + 16) as *mut __m128i, rgba1);
        _mm_storeu_si128(dst.add(o + 32) as *mut __m128i, rgba2);
        _mm_storeu_si128(dst.add(o + 48) as *mut __m128i, rgba3);

        i += 48;
        o += 64;
    }

    // Process 4 pixels at a time (12 RGB bytes -> 16 RGBA bytes)
    // Loop bound: load at i reads 16 bytes -> needs i+16 <= len
    while i + 16 <= rgb_data.len() {
        let rgb = _mm_loadu_si128(src.add(i) as *const __m128i);
        let rgba = _mm_or_si128(_mm_shuffle_epi8(rgb, shuffle), alpha);
        _mm_storeu_si128(dst.add(o) as *mut __m128i, rgba);
        i += 12;
        o += 16;
    }

    // Scalar remainder (handles final pixels where SIMD would read OOB)
    while i + 3 <= rgb_data.len() {
        *dst.add(o) = *src.add(i);
        *dst.add(o + 1) = *src.add(i + 1);
        *dst.add(o + 2) = *src.add(i + 2);
        *dst.add(o + 3) = 255;
        i += 3;
        o += 4;
    }
}

/// Scalar fallback for RGB to RGBA (used when SSSE3 not available)
#[cfg(target_arch = "x86_64")]
#[inline]
unsafe fn rgb_to_rgba_scalar_x86(rgb_data: &[u8], rgba_data: &mut Vec<u8>) {
    let pixel_count = rgb_data.len() / 3;
    let rgba_size = pixel_count.checked_mul(4).expect("RGB to RGBA size overflow");

    rgba_data.clear();
    rgba_data.reserve_exact(rgba_size);
    rgba_data.set_len(rgba_size);

    let src = rgb_data.as_ptr();
    let dst = rgba_data.as_mut_ptr();

    let mut i = 0usize;
    let mut o = 0usize;

    // Process 4 pixels at a time using u32 operations
    while i + 12 <= rgb_data.len() {
        let p0 = *src.add(i) as u32 | (*src.add(i+1) as u32) << 8 | (*src.add(i+2) as u32) << 16 | 0xFF000000;
        let p1 = *src.add(i+3) as u32 | (*src.add(i+4) as u32) << 8 | (*src.add(i+5) as u32) << 16 | 0xFF000000;
        let p2 = *src.add(i+6) as u32 | (*src.add(i+7) as u32) << 8 | (*src.add(i+8) as u32) << 16 | 0xFF000000;
        let p3 = *src.add(i+9) as u32 | (*src.add(i+10) as u32) << 8 | (*src.add(i+11) as u32) << 16 | 0xFF000000;

        *(dst.add(o) as *mut u32) = p0;
        *(dst.add(o+4) as *mut u32) = p1;
        *(dst.add(o+8) as *mut u32) = p2;
        *(dst.add(o+12) as *mut u32) = p3;

        i += 12;
        o += 16;
    }

    // Scalar remainder
    while i + 3 <= rgb_data.len() {
        *dst.add(o) = *src.add(i);
        *dst.add(o + 1) = *src.add(i + 1);
        *dst.add(o + 2) = *src.add(i + 2);
        *dst.add(o + 3) = 255;
        i += 3;
        o += 4;
    }
}

/// Scalar RGB to RGBA conversion (fallback for non-SIMD platforms)
#[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
#[inline]
fn rgb_to_rgba_scalar(rgb_data: &[u8], rgba_data: &mut Vec<u8>) {
    let pixel_count = rgb_data.len() / 3;
    let rgba_size = pixel_count.checked_mul(4).expect("RGB to RGBA size overflow");

    rgba_data.clear();
    rgba_data.reserve_exact(rgba_size);

    for chunk in rgb_data.chunks_exact(3) {
        rgba_data.extend_from_slice(&[chunk[0], chunk[1], chunk[2], 255]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_has_alpha_opaque() {
        // All pixels opaque (alpha = 255)
        let pixels = vec![255u8; 1024]; // 256 pixels
        assert!(!has_alpha_transparency(&pixels));
    }

    #[test]
    fn test_has_alpha_transparent() {
        // One transparent pixel
        let mut pixels = vec![255u8; 1024];
        pixels[3] = 128; // First pixel has alpha = 128
        assert!(has_alpha_transparency(&pixels));
    }

    #[test]
    fn test_set_alpha_opaque() {
        let mut pixels = vec![0u8; 16]; // 4 pixels, all zero
        set_all_alpha_opaque(&mut pixels);

        // Check alpha channels are now 255
        assert_eq!(pixels[3], 255);
        assert_eq!(pixels[7], 255);
        assert_eq!(pixels[11], 255);
        assert_eq!(pixels[15], 255);

        // RGB should still be 0
        assert_eq!(pixels[0], 0);
        assert_eq!(pixels[1], 0);
        assert_eq!(pixels[2], 0);
    }

    #[test]
    fn test_rgb_to_rgba_basic() {
        // 4 RGB pixels: red, green, blue, white
        let rgb = vec![
            255, 0, 0,      // red
            0, 255, 0,      // green
            0, 0, 255,      // blue
            255, 255, 255,  // white
        ];
        let rgba = rgb_to_rgba(&rgb);

        assert_eq!(rgba.len(), 16);
        // Red pixel
        assert_eq!(&rgba[0..4], &[255, 0, 0, 255]);
        // Green pixel
        assert_eq!(&rgba[4..8], &[0, 255, 0, 255]);
        // Blue pixel
        assert_eq!(&rgba[8..12], &[0, 0, 255, 255]);
        // White pixel
        assert_eq!(&rgba[12..16], &[255, 255, 255, 255]);
    }

    #[test]
    fn test_rgb_to_rgba_large() {
        // Test with enough pixels to trigger SIMD path (48+ bytes = 16+ pixels)
        let pixel_count = 64;
        let mut rgb = Vec::with_capacity(pixel_count * 3);
        for i in 0..pixel_count {
            rgb.push((i * 4) as u8);       // R
            rgb.push((i * 4 + 1) as u8);   // G
            rgb.push((i * 4 + 2) as u8);   // B
        }

        let rgba = rgb_to_rgba(&rgb);
        assert_eq!(rgba.len(), pixel_count * 4);

        // Verify each pixel
        for i in 0..pixel_count {
            let r = (i * 4) as u8;
            let g = (i * 4 + 1) as u8;
            let b = (i * 4 + 2) as u8;
            assert_eq!(rgba[i * 4], r, "pixel {} R mismatch", i);
            assert_eq!(rgba[i * 4 + 1], g, "pixel {} G mismatch", i);
            assert_eq!(rgba[i * 4 + 2], b, "pixel {} B mismatch", i);
            assert_eq!(rgba[i * 4 + 3], 255, "pixel {} A mismatch", i);
        }
    }
}
