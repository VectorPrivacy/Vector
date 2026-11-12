//! SpeexDSP for the call engine, and the slice of `audio.rs` it reads.

/// The desktop client's call DSP and the WAV reader its audio engine leans on, from its sources.
fn main() {
    println!("cargo:rustc-check-cfg=cfg(desktop)");
    println!("cargo:rustc-cfg=desktop");
    let speex = std::path::Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../src-tauri/csrc/speexdsp"
    ));
    println!("cargo:rerun-if-changed={}", speex.display());
    cc::Build::new()
        .include(speex)
        .files(
            [
                "mdf.c",
                "preprocess.c",
                "fftwrap.c",
                "filterbank.c",
                "kiss_fft.c",
                "kiss_fftr.c",
            ]
            .iter()
            .map(|f| speex.join(f)),
        )
        .define("FLOATING_POINT", "1")
        .define("USE_KISS_FFT", "1")
        .define("EXPORT", "")
        .opt_level(2)
        .warnings(false)
        .compile("speexdsp_calls");

    // audio.rs is mostly Tauri commands; these functions are plain Rust.
    let src = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../src-tauri/src/audio.rs"
    );
    println!("cargo:rerun-if-changed={src}");
    let rs = std::fs::read_to_string(src).expect("audio.rs");
    let mut out = String::new();
    for head in [
        "fn wav_fast_decode(",
        "pub fn wav_fast_decode_for_engine(",
        "fn wav_channel_count(",
        "pub fn decode_for_whisper(",
        "pub(crate) fn symphonia_decode_mono(",
    ] {
        let start = rs
            .lines()
            .position(|l| l.starts_with(head))
            .unwrap_or_else(|| panic!("audio.rs lost {head}"));
        for line in rs.lines().skip(start) {
            out.push_str(line);
            out.push('\n');
            if line == "}" {
                break;
            }
        }
    }
    let dest = std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("wav.rs");
    std::fs::write(dest, out).unwrap();
}
