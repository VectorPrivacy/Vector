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

pub mod hex;
pub mod image;

// Re-export commonly used functions at the simd level
pub use hex::{
    bytes_to_hex_16, bytes_to_hex_32, bytes_to_hex_string,
    hex_string_to_bytes, hex_to_bytes_16, hex_to_bytes_32,
};
pub use image::{
    has_alpha_transparency, set_all_alpha_opaque,
    nearest_neighbor_downsample, rgb_to_rgba,
};

#[cfg(target_os = "windows")]
pub use image::has_all_alpha_near_zero;
