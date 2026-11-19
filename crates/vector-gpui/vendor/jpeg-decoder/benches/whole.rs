//! Whole-scale JPEGs, the image crate's decode then a resize against this decoder's rows streamed
//! into GPUI's resizer: time, bytes allocated and the largest live heap per decode, and how far
//! the pixels land apart.
//!   cargo bench -p vector-jpeg --bench whole -- <file>...
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::time::Instant;
use image::imageops::FilterType;

#[path = "../../gpui/src/elements/resample.rs"]
#[allow(dead_code)]
mod resample;

struct Counting;
static BYTES: AtomicUsize = AtomicUsize::new(0);
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 { BYTES.fetch_add(l.size(), Relaxed); let v = LIVE.fetch_add(l.size(), Relaxed) + l.size(); PEAK.fetch_max(v, Relaxed); unsafe { System.alloc(l) } }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 { BYTES.fetch_add(l.size(), Relaxed); let v = LIVE.fetch_add(l.size(), Relaxed) + l.size(); PEAK.fetch_max(v, Relaxed); unsafe { System.alloc_zeroed(l) } }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) { LIVE.fetch_sub(l.size(), Relaxed); unsafe { System.dealloc(p, l) } }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 { BYTES.fetch_add(n, Relaxed); let v = LIVE.fetch_add(n, Relaxed) + n; LIVE.fetch_sub(l.size(), Relaxed); PEAK.fetch_max(v, Relaxed); unsafe { System.realloc(p, l, n) } }
}
#[global_allocator]
static A: Counting = Counting;

fn general(bytes: &[u8], to: Option<(u32, u32)>) -> Vec<u8> {
    let img = image::load_from_memory_with_format(bytes, image::ImageFormat::Jpeg).unwrap();
    let img = match to {
        Some((w, h)) => resample::resample_within(&img, w, h, FilterType::CatmullRom),
        None => img,
    };
    let mut b = img.into_rgba8().into_raw();
    for p in b.chunks_exact_mut(4) { p.swap(0, 2); }
    b
}

fn put(src: &[u8], c: usize, out: &mut [u8]) {
    for (p, o) in src.chunks_exact(c).zip(out.chunks_exact_mut(4)) {
        if c == 3 { o[2] = p[0]; o[1] = p[1]; o[0] = p[2]; } else { o[..3].fill(p[0]); }
        o[3] = 255;
    }
}

fn streamed(bytes: &[u8], to: Option<(u32, u32)>) -> Vec<u8> {
    let mut d = jpeg_decoder::Decoder::new(bytes);
    d.read_info().unwrap();
    let i = d.info().unwrap();
    let (w, h) = (i.width as u32, i.height as u32);
    let c = if i.pixel_format == jpeg_decoder::PixelFormat::L8 { 1 } else { 3 };
    let (tw, th) = to.map(|(mw, mh)| {
        let r = (mw as f64 / w as f64).min(mh as f64 / h as f64);
        (((w as f64 * r).round() as u32).max(1), ((h as f64 * r).round() as u32).max(1))
    }).unwrap_or((w, h));
    let mut out = vec![0u8; tw as usize * th as usize * 4];
    let line = tw as usize * 4;
    if (tw, th) == (w, h) {
        d.decode_rows(|y, row| put(row, c, &mut out[y * line..][..line])).unwrap();
    } else {
        let mut z = resample::Rows::<3>::new(w, h, tw, th, FilterType::CatmullRom).unwrap();
        d.decode_rows(|y, row| z.push(y, row, |r, o| put(o, 3, &mut out[r * line..][..line]))).unwrap();
    }
    out
}

fn run(name: &str, f: impl Fn() -> Vec<u8>) -> Vec<u8> {
    let mut best = f64::MAX;
    let mut out = Vec::new();
    let (mut total, mut peak) = (0, 0);
    for _ in 0..7 {
        let (b0, l0) = (BYTES.load(Relaxed), LIVE.load(Relaxed));
        PEAK.store(l0, Relaxed);
        let t = Instant::now();
        out = f();
        best = best.min(t.elapsed().as_secs_f64() * 1e3);
        total = BYTES.load(Relaxed) - b0;
        peak = PEAK.load(Relaxed) - l0;
    }
    println!("  {name:<10} {best:8.2} ms   allocated {:7.1} MB   peak {:7.1} MB", total as f64 / 1e6, peak as f64 / 1e6);
    out
}

fn main() {
    for path in std::env::args().skip(1).filter(|a| !a.starts_with('-')) {
        let bytes = std::fs::read(&path).unwrap();
        for to in [Some((1200u32, 1200u32)), None] {
            println!("{path} {to:?}");
            let a = run("general", || general(&bytes, to));
            let b = run("streamed", || streamed(&bytes, to));
            let d = a.iter().zip(&b).map(|(x, y)| x.abs_diff(*y)).max();
            let mean = a.iter().zip(&b).map(|(x, y)| x.abs_diff(*y) as u64).sum::<u64>() as f64 / a.len() as f64;
            println!("  same size {}  max diff {d:?}  mean {mean:.3}", a.len() == b.len());
        }
        // Each side's parts on their own: the two decodes, and the two resizes of one picture.
        println!("{path} parts");
        run("image dec", || image::load_from_memory_with_format(&bytes, image::ImageFormat::Jpeg).unwrap().into_bytes());
        run("rows dec", || {
            let mut d = jpeg_decoder::Decoder::new(&bytes[..]);
            let mut sum = 0u8;
            d.decode_rows(|_, row| sum ^= row[0]).unwrap();
            vec![sum]
        });
        let img = image::load_from_memory_with_format(&bytes, image::ImageFormat::Jpeg).unwrap().into_rgb8();
        let (w, h) = img.dimensions();
        let r = (1200. / w as f64).min(1200. / h as f64);
        let (tw, th) = (((w as f64 * r).round() as u32).max(1), ((h as f64 * r).round() as u32).max(1));
        run("resample", || resample::resample(&img, tw, th, FilterType::CatmullRom).into_raw());
        run("rows", || {
            let mut out = vec![0u8; tw as usize * th as usize * 3];
            let line = tw as usize * 3;
            let mut z = resample::Rows::<3>::new(w, h, tw, th, FilterType::CatmullRom).unwrap();
            for (y, row) in img.chunks_exact(w as usize * 3).enumerate() {
                z.push(y, row, |r, o| out[r * line..][..line].copy_from_slice(o));
            }
            out
        });
    }
}
