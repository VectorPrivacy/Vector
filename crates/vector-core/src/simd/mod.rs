//! SIMD-accelerated operations.
//!
//! Platform support:
//! - **ARM64**: NEON intrinsics
//! - **x86_64**: SSE2/AVX2 intrinsics
//! - **Other** (WASM, etc.): Scalar fallbacks
//!
//! NOTE: `hex.rs` (SIMD hex encode/decode) currently lives at the crate root
//! for historical reasons. It should eventually move here.

pub mod image;
