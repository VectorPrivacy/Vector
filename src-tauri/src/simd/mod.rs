//! SIMD-accelerated operations
//!
//! - [`html_meta`] - HTML metadata extraction (SIMD `<` scanner + scalar tag parser)
//! - [`image`] - Image operations (~9x faster with parallel SIMD)
//! - [`audio`] - Audio sample conversion (2.3x faster f32→i16)
//! - [`url`] - URL delimiter scanning (4.7-5.2x faster)
//!
//! Hex encoding/decoding lives in vector-core (`vector_core::hex`).

pub mod audio;
pub mod html_meta;
pub mod image;
pub mod url;

pub use image::{has_alpha_transparency, set_all_alpha_opaque};

#[cfg(target_os = "windows")]
pub use image::has_all_alpha_near_zero;
