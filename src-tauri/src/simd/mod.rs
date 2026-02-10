//! SIMD-accelerated operations for Vector
//!
//! This module provides high-performance implementations using:
//! - **ARM64 (Apple Silicon, Android)**: NEON SIMD intrinsics
//! - **x86_64 (Windows, Linux)**: SSE2/AVX2 SIMD intrinsics
//!
//! All public functions automatically select the best implementation
//! for the current platform at compile time (with runtime AVX2 detection on x86_64).
//!
//! # Modules
//!
//! - [`hex`] - Hex encoding/decoding (~62x faster than format!)
//! - [`image`] - Image operations (~9x faster with parallel SIMD)
//! - [`audio`] - Audio sample conversion (2.3x faster f32→i16)
//! - [`url`] - URL delimiter scanning (4.7-5.2x faster)

pub mod audio;
pub mod hex;
pub mod image;
pub mod url;

// Re-export commonly used functions at the simd level
pub use hex::{
    bytes_to_hex_32, bytes_to_hex_string,
    hex_string_to_bytes, hex_to_bytes_16, hex_to_bytes_32,
};
pub use image::{has_alpha_transparency, set_all_alpha_opaque};

#[cfg(target_os = "windows")]
pub use image::has_all_alpha_near_zero;

// Also available via crate::simd::image::*
// - nearest_neighbor_downsample (used in util.rs for blurhash)
// - rgb_to_rgba (used in util.rs for base64 image decoding)
