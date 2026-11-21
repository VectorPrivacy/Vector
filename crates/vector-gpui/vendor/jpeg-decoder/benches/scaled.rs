//! Scaled decodes, this decoder against stock jpeg-decoder: time, allocations and bytes allocated
//! per decode, and the largest live heap a decode reaches.
//!   cargo bench -p vector-jpeg --bench scaled -- <dir or file>...

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::time::Instant;

struct Counting;
static CALLS: AtomicUsize = AtomicUsize::new(0);
static BYTES: AtomicUsize = AtomicUsize::new(0);
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        CALLS.fetch_add(1, Relaxed);
        BYTES.fetch_add(layout.size(), Relaxed);
        let live = LIVE.fetch_add(layout.size(), Relaxed) + layout.size();
        PEAK.fetch_max(live, Relaxed);
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        CALLS.fetch_add(1, Relaxed);
        BYTES.fetch_add(layout.size(), Relaxed);
        let live = LIVE.fetch_add(layout.size(), Relaxed) + layout.size();
        PEAK.fetch_max(live, Relaxed);
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size(), Relaxed);
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new: usize) -> *mut u8 {
        CALLS.fetch_add(1, Relaxed);
        BYTES.fetch_add(new, Relaxed);
        let live = LIVE.fetch_add(new, Relaxed) + new;
        LIVE.fetch_sub(layout.size(), Relaxed);
        PEAK.fetch_max(live, Relaxed);
        unsafe { System.realloc(ptr, layout, new) }
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

/// The process's CPU time so far, every thread's (Linux), in microseconds at 10 ms resolution.
fn cpu_micros() -> f64 {
    let stat = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
    let fields: Vec<&str> = stat.rsplit(')').next().unwrap_or("").split_whitespace().collect();
    let ticks = |i: usize| fields.get(i).and_then(|f| f.parse::<f64>().ok()).unwrap_or(0.);
    (ticks(11) + ticks(12)) * 10_000.
}

struct Run {
    micros: f64,
    cpu: f64,
    calls: usize,
    bytes: usize,
    peak: usize,
}

fn measure(iterations: usize, mut decode: impl FnMut() -> usize) -> Run {
    decode();
    let (c0, b0) = (CALLS.load(Relaxed), BYTES.load(Relaxed));
    PEAK.store(LIVE.load(Relaxed), Relaxed);
    let base = LIVE.load(Relaxed);
    let start = Instant::now();
    let cpu = cpu_micros();
    let mut sink = 0;
    for _ in 0..iterations {
        sink ^= decode();
    }
    let micros = start.elapsed().as_secs_f64() * 1e6 / iterations as f64;
    let cpu = (cpu_micros() - cpu) / iterations as f64;
    std::hint::black_box(sink);
    Run {
        micros,
        cpu,
        calls: (CALLS.load(Relaxed) - c0) / iterations,
        bytes: (BYTES.load(Relaxed) - b0) / iterations,
        peak: PEAK.load(Relaxed) - base,
    }
}

fn main() {
    let mut files = Vec::new();
    for arg in std::env::args().skip(1).filter(|a| !a.starts_with('-')) {
        let path = std::path::PathBuf::from(arg);
        if path.is_dir() {
            let mut entries: Vec<_> = std::fs::read_dir(&path).unwrap().flatten().map(|e| e.path()).collect();
            entries.sort();
            files.extend(entries.into_iter().filter(|p| p.extension().is_some_and(|e| e == "jpg" || e == "jpeg")));
        } else {
            files.push(path);
        }
    }
    let kb = |b: usize| b as f64 / 1024.;
    let mut totals = [[0f64; 5]; 2];
    println!("{:<24} {:>9} {:>3} | {:>7} {:>7} {:>6} {:>8} {:>7} | {:>7} {:>7} {:>6} {:>8} {:>7}", "file", "size", "1/n", "wall µs", "cpu µs", "allocs", "alloc KB", "peak KB", "wall µs", "cpu µs", "allocs", "alloc KB", "peak KB");
    for path in files {
        let bytes = std::fs::read(&path).unwrap();
        let mut probe = stock::Decoder::new(&bytes[..]);
        if probe.read_info().is_err() {
            continue;
        }
        let info = probe.info().unwrap();
        if stock::Decoder::new(&bytes[..]).decode().is_err() {
            continue;
        }
        for d in [8u16, 4, 2] {
            let (w, h) = (info.width.div_ceil(d), info.height.div_ceil(d));
            let iterations = (2_000_000 / (bytes.len() + 1)).clamp(3, 400);
            let only = std::env::var("BENCH_ONLY").ok();
            let iterations = std::env::var("BENCH_N").ok().and_then(|n| n.parse().ok()).unwrap_or(if only.is_some() { iterations * 20 } else { iterations });
            let s = measure(if only.as_deref() == Some("ours") { 1 } else { iterations }, || {
                let mut dec = stock::Decoder::new(&bytes[..]);
                dec.read_info().unwrap();
                dec.scale(w, h).unwrap();
                dec.decode().unwrap().len()
            });
            let o = measure(if only.as_deref() == Some("stock") { 1 } else { iterations }, || {
                let mut dec = jpeg_decoder::Decoder::new(&bytes);
                dec.read_info().unwrap();
                dec.scale(w, h).unwrap();
                let mut n = 0;
                dec.decode_rows(|_, row| n += row.len()).unwrap();
                n
            });
            for (t, r) in totals.iter_mut().zip([&s, &o]) {
                t[0] += r.micros;
                t[1] += r.calls as f64;
                t[2] += r.bytes as f64;
                t[3] = t[3].max(r.peak as f64);
                t[4] += r.cpu;
            }
            let name = path.file_name().unwrap().to_string_lossy();
            println!(
                "{:<24} {:>4}x{:<4} {:>3} | {:>7.0} {:>7.0} {:>6} {:>8.0} {:>7.0} | {:>7.0} {:>7.0} {:>6} {:>8.0} {:>7.0}",
                &name[..name.len().min(24)], info.width, info.height, d,
                s.micros, s.cpu, s.calls, kb(s.bytes), kb(s.peak), o.micros, o.cpu, o.calls, kb(o.bytes), kb(o.peak)
            );
        }
    }
    println!(
        "total: stock {:.1} ms wall, {:.1} ms cpu, {:.0} allocs, {:.1} MB | ours {:.1} ms wall, {:.1} ms cpu, {:.0} allocs, {:.1} MB",
        totals[0][0] / 1000., totals[0][4] / 1000., totals[0][1], totals[0][2] / 1048576.,
        totals[1][0] / 1000., totals[1][4] / 1000., totals[1][1], totals[1][2] / 1048576.
    );
}
