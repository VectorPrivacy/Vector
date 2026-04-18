//! SIMD-accelerated image operations.
//!
//! # Platform Support
//!
//! - **ARM64**: NEON (vsetq_lane_u32 gather + vst1q_u32 store)
//! - **x86_64**: SSE2 (_mm_set_epi32 gather + _mm_storeu_si128 store)
//! - **Other**: Scalar u32-at-a-time pixel copy
//!
//! # Algorithms
//!
//! **Nearest-neighbor downsample**: Processes 4 output pixels per SIMD iteration.
//! Pre-computes x-coordinate mapping table (shared across rows) to avoid
//! per-pixel integer division. Used for thumbhash generation (100x100 max).

#[cfg(target_arch = "aarch64")]
use std::arch::aarch64::*;

#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::*;

// ============================================================================
// Nearest-Neighbor RGBA Downsample
// ============================================================================

/// Nearest-neighbor RGBA downsample with SIMD acceleration.
///
/// Validates input dimensions. Panics on overflow or undersized buffer.
pub fn nearest_neighbor_downsample_rgba(
    pixels: &[u8],
    src_width: u32,
    src_height: u32,
    dst_width: u32,
    dst_height: u32,
) -> Vec<u8> {
    let src_size = (src_width as usize)
        .checked_mul(src_height as usize)
        .and_then(|n| n.checked_mul(4))
        .expect("source dimensions overflow");
    assert!(pixels.len() >= src_size, "pixels buffer too small for source dimensions");

    let dst_size = (dst_width as usize)
        .checked_mul(dst_height as usize)
        .and_then(|n| n.checked_mul(4))
        .expect("destination dimensions overflow");

    let mut result: Vec<u8> = vec![0u8; dst_size];
    let src_stride = src_width as usize * 4;

    // Pre-compute x mapping table (shared across all rows)
    let x_map: Vec<usize> = (0..dst_width)
        .map(|tx| (tx as u64 * src_width as u64 / dst_width as u64) as usize * 4)
        .collect();

    #[cfg(target_arch = "aarch64")]
    downsample_neon(pixels, &mut result, src_stride, src_height, dst_width, dst_height, &x_map);

    #[cfg(target_arch = "x86_64")]
    downsample_sse2(pixels, &mut result, src_stride, src_height, dst_width, dst_height, &x_map);

    #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
    downsample_scalar(pixels, &mut result, src_stride, src_height, dst_width, dst_height, &x_map);

    result
}

// ============================================================================
// NEON (ARM64)
// ============================================================================

/// Gather 4 source pixels into a NEON register and store.
#[cfg(target_arch = "aarch64")]
fn downsample_neon(
    src: &[u8], dst: &mut [u8], src_stride: usize,
    src_height: u32, dst_width: u32, dst_height: u32, x_map: &[usize],
) {
    unsafe {
        let src_ptr = src.as_ptr();
        let dst_ptr = dst.as_mut_ptr() as *mut u32;
        let mut dst_idx = 0usize;
        let chunks = dst_width as usize / 4;
        let remainder = dst_width as usize % 4;

        for ty in 0..dst_height {
            let sy = (ty as u64 * src_height as u64 / dst_height as u64) as usize;
            let row = src_ptr.add(sy * src_stride);

            for cx in 0..chunks {
                let base = cx * 4;
                let p0 = *(row.add(x_map[base]) as *const u32);
                let p1 = *(row.add(x_map[base + 1]) as *const u32);
                let p2 = *(row.add(x_map[base + 2]) as *const u32);
                let p3 = *(row.add(x_map[base + 3]) as *const u32);

                let mut v = vdupq_n_u32(p0);
                v = vsetq_lane_u32(p1, v, 1);
                v = vsetq_lane_u32(p2, v, 2);
                v = vsetq_lane_u32(p3, v, 3);
                vst1q_u32(dst_ptr.add(dst_idx) as *mut u32, v);
                dst_idx += 4;
            }

            for rx in 0..remainder {
                *dst_ptr.add(dst_idx) = *(row.add(x_map[chunks * 4 + rx]) as *const u32);
                dst_idx += 1;
            }
        }
    }
}

// ============================================================================
// SSE2 (x86_64)
// ============================================================================

/// Gather 4 source pixels into an SSE2 register and store.
#[cfg(target_arch = "x86_64")]
fn downsample_sse2(
    src: &[u8], dst: &mut [u8], src_stride: usize,
    src_height: u32, dst_width: u32, dst_height: u32, x_map: &[usize],
) {
    unsafe {
        let src_ptr = src.as_ptr();
        let dst_ptr = dst.as_mut_ptr();
        let mut dst_idx = 0usize;
        let chunks = dst_width as usize / 4;
        let remainder = dst_width as usize % 4;

        for ty in 0..dst_height {
            let sy = (ty as u64 * src_height as u64 / dst_height as u64) as usize;
            let row = src_ptr.add(sy * src_stride);

            for cx in 0..chunks {
                let base = cx * 4;
                let p0 = *(row.add(x_map[base]) as *const i32);
                let p1 = *(row.add(x_map[base + 1]) as *const i32);
                let p2 = *(row.add(x_map[base + 2]) as *const i32);
                let p3 = *(row.add(x_map[base + 3]) as *const i32);

                let v = _mm_set_epi32(p3, p2, p1, p0);
                _mm_storeu_si128(dst_ptr.add(dst_idx * 4) as *mut __m128i, v);
                dst_idx += 4;
            }

            for rx in 0..remainder {
                let sx = x_map[chunks * 4 + rx];
                *(dst_ptr.add(dst_idx * 4) as *mut u32) = *(row.add(sx) as *const u32);
                dst_idx += 1;
            }
        }
    }
}

// ============================================================================
// Scalar Fallback
// ============================================================================

/// u32-at-a-time pixel copy — for platforms without SIMD (WASM, etc.).
#[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
fn downsample_scalar(
    src: &[u8], dst: &mut [u8], src_stride: usize,
    src_height: u32, dst_width: u32, dst_height: u32, x_map: &[usize],
) {
    unsafe {
        let src_ptr = src.as_ptr();
        let dst_ptr = dst.as_mut_ptr() as *mut u32;
        let mut dst_idx = 0usize;

        for ty in 0..dst_height {
            let sy = (ty as u64 * src_height as u64 / dst_height as u64) as usize;
            let row = src_ptr.add(sy * src_stride);

            for tx in 0..dst_width as usize {
                *dst_ptr.add(dst_idx) = *(row.add(x_map[tx]) as *const u32);
                dst_idx += 1;
            }
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downsample_basic_red() {
        // 4x4 red → 2x2 — every pixel stays red
        let pixels: Vec<u8> = vec![255, 0, 0, 255].repeat(16);
        let result = nearest_neighbor_downsample_rgba(&pixels, 4, 4, 2, 2);
        assert_eq!(result.len(), 2 * 2 * 4);
        for chunk in result.chunks(4) {
            assert_eq!(chunk, &[255, 0, 0, 255]);
        }
    }

    #[test]
    fn downsample_identity() {
        // Same size → same output
        let pixels: Vec<u8> = vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16];
        let result = nearest_neighbor_downsample_rgba(&pixels, 2, 2, 2, 2);
        assert_eq!(result, pixels);
    }

    #[test]
    fn downsample_checkerboard() {
        // 4x4 checkerboard → 2x2
        let red = [255u8, 0, 0, 255];
        let blue = [0u8, 0, 255, 255];
        let mut pixels = Vec::with_capacity(4 * 4 * 4);
        for y in 0..4 {
            for x in 0..4 {
                if (x + y) % 2 == 0 { pixels.extend_from_slice(&red); }
                else { pixels.extend_from_slice(&blue); }
            }
        }
        let result = nearest_neighbor_downsample_rgba(&pixels, 4, 4, 2, 2);
        assert_eq!(result.len(), 2 * 2 * 4);
        // (0,0) → src(0,0) = red
        assert_eq!(&result[0..4], &red);
        // (1,0) → src(2,0) = red
        assert_eq!(&result[4..8], &red);
    }

    #[test]
    fn downsample_large_to_small() {
        // 100x100 → 10x10 — verify dimensions and no panic
        let pixels: Vec<u8> = vec![128, 64, 32, 255].repeat(100 * 100);
        let result = nearest_neighbor_downsample_rgba(&pixels, 100, 100, 10, 10);
        assert_eq!(result.len(), 10 * 10 * 4);
    }

    #[test]
    fn downsample_non_power_of_two() {
        // 7x5 → 3x2 — odd dimensions, tests remainder handling
        let pixels: Vec<u8> = (0..7 * 5 * 4).map(|i| (i % 256) as u8).collect();
        let result = nearest_neighbor_downsample_rgba(&pixels, 7, 5, 3, 2);
        assert_eq!(result.len(), 3 * 2 * 4);
    }

    #[test]
    #[should_panic(expected = "pixels buffer too small")]
    fn downsample_undersized_buffer() {
        let pixels = vec![0u8; 10]; // too small for 4x4
        nearest_neighbor_downsample_rgba(&pixels, 4, 4, 2, 2);
    }
}
