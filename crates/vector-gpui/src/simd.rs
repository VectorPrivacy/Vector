//! The desktop client's SIMD image codecs, compiled from its own sources so both clients
//! prepare pictures byte for byte alike.

#[path = "../../../src-tauri/src/simd/audio.rs"]
#[allow(dead_code)]
pub mod audio;
#[path = "../../../src-tauri/src/simd/image.rs"]
pub mod image;
#[path = "../../../src-tauri/src/simd/jpeg/mod.rs"]
pub mod jpeg;
#[path = "../../../src-tauri/src/simd/neuquant.rs"]
pub mod neuquant;
#[path = "../../../src-tauri/src/simd/png.rs"]
pub mod png;
