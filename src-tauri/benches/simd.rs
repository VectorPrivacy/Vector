//! The shell's SIMD benchmarks against the crates they replace, built optimised. From `src-tauri/`:
//!
//!     cargo bench --no-default-features --features bench --bench simd [-- jpeg | neuquant]
//!
//! `neuquant` quantises a real picture: point `NQ_IMAGE` at one.

use std::time::Instant;
use vector_lib::bench::{backend_name, encode_rgb, IndexCache, NeuQuant};

const ALL: &[(&str, fn())] = &[("jpeg", jpeg), ("neuquant", neuquant)];

fn main() {
    // cargo passes `--bench`; every other flag is ignored.
    let filters: Vec<String> = std::env::args().skip(1).filter(|a| !a.starts_with('-')).collect();
    for (name, run) in ALL.iter().filter(|(name, _)| filters.is_empty() || filters.iter().any(|f| name.contains(f.as_str()))) {
        println!("\n== {name}");
        run();
    }
}

fn noise_image(w: usize, h: usize, seed: u32) -> Vec<u8> {
    let mut s = seed | 1;
    (0..w * h * 3)
        .map(|i| {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            let (x, y) = ((i / 3) % w, (i / 3) / w);
            let base = ((x * 255) / w.max(1) + (y * 127) / h.max(1) + (i % 3) * 40) as i32;
            (base + (s % 23) as i32 - 11).clamp(0, 255) as u8
        })
        .collect()
}

/// Our encoder against the image crate's, one thread and all of them.
fn jpeg() {
    let (w, h) = (1920u32, 1440u32);
    let px = noise_image(w as usize, h as usize, 11);
    let time = |f: &dyn Fn() -> Vec<u8>| {
        let mut t: Vec<f64> = (0..9)
            .map(|_| {
                let s = Instant::now();
                std::hint::black_box(f());
                s.elapsed().as_secs_f64() * 1e3
            })
            .collect();
        t.sort_by(f64::total_cmp);
        (t[4], f().len())
    };
    let image = time(&|| {
        let mut o = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut o, 85)
            .encode(&px, w, h, image::ExtendedColorType::Rgb8)
            .unwrap();
        o
    });
    let one = rayon::ThreadPoolBuilder::new().num_threads(1).build().unwrap();
    let single = time(&|| one.install(|| encode_rgb(&px, w, h, 85).unwrap()));
    let all = time(&|| encode_rgb(&px, w, h, 85).unwrap());
    println!("{}, {} threads, 1920x1440 q85", backend_name(), rayon::current_num_threads());
    println!("  image crate  {:7.1} ms  {} B", image.0, image.1);
    println!("  ours, 1      {:7.1} ms  {} B  {:.1}x", single.0, single.1, image.0 / single.0);
    println!("  ours, all    {:7.1} ms  {} B  {:.1}x", all.0, all.1, image.0 / all.0);
}

/// Our NeuQuant against color_quant's: learning the palette, then indexing every pixel.
fn neuquant() {
    let Ok(path) = std::env::var("NQ_IMAGE") else {
        println!("skipped: set NQ_IMAGE to a picture");
        return;
    };
    let img = image::open(path).unwrap().to_rgba8();
    let px = img.as_raw();
    for _ in 0..2 {
        let t = Instant::now();
        let theirs = color_quant::NeuQuant::new(10, 256, px);
        let tl = t.elapsed();
        let t = Instant::now();
        let v: Vec<u8> = px.chunks_exact(4).map(|p| theirs.index_of(p) as u8).collect();
        let ti = t.elapsed();
        let t = Instant::now();
        let ours = NeuQuant::new(10, px);
        let ol = t.elapsed();
        let t = Instant::now();
        let mut m = IndexCache::new(&ours);
        let w: Vec<u8> = px.chunks_exact(4).map(|p| m.index_of([p[0], p[1], p[2], p[3]])).collect();
        let oi = t.elapsed();
        assert_eq!(v, w, "ours indexes as color_quant does");
        println!("color_quant learn {tl:?} index {ti:?} | ours learn {ol:?} index {oi:?}");
    }
}
