//! The JPEG decoder takes avatars and banners straight off the network. Seed the corpus with
//! `src/simd/jpeg/testdata/*.jpg` and a few camera photos; run it under ASan (cargo-fuzz's default).
#![no_main]
#![allow(dead_code, unused_imports)]
// is_multiple_of is stable from Rust 1.87; older nightlies need the gate.
#![cfg_attr(fuzzing, allow(stable_features), feature(unsigned_is_multiple_of))]

#[path = "../../src/simd/jpeg/mod.rs"]
mod jpeg;

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some((&pick, file)) = data.split_first() else { return };
    // The first byte picks the DCT scale, so every IDCT size is reached.
    let min = [0u32, 1, 30, 100, 1920][usize::from(pick % 5)];
    if let Ok(d) = jpeg::decode_at_least(file, min) {
        assert_eq!(d.pixels.len(), d.width as usize * d.height as usize * d.channels as usize);
    }
});
