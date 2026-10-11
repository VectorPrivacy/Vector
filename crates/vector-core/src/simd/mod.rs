//! SIMD-accelerated operations.
//!
//! Platform support:
//! - **ARM64**: NEON intrinsics
//! - **x86_64**: SSE2/AVX2 intrinsics
//! - **Other** (WASM, etc.): Scalar fallbacks
//!
//! Modules:
//! - `base64` — base64 encode/decode (NEON TBL, SSSE3/AVX2 multiply-shift and pshufb, scalar LUT)
//! - `hex` — hex encode/decode (NEON TBL, SSE2/AVX2 arithmetic, scalar LUT)
//! - `image` — nearest-neighbor RGBA downsample (NEON gather, SSE2 gather, scalar)

pub mod base64;
pub mod hex;
pub mod image;
