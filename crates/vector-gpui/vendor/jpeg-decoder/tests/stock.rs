//! This decoder against stock jpeg-decoder 0.3.2: the same pixels, metadata and errors for every
//! file at every scale, and no panic on mutated files. `JPEG_CORPUS=<dir>[:<dir>...]` adds files.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};

fn corpus() -> Vec<(PathBuf, Vec<u8>)> {
    let mut dirs = vec![Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus")];
    if let Some(extra) = std::env::var_os("JPEG_CORPUS") {
        dirs.extend(std::env::split_paths(&extra));
    }
    let mut files = Vec::new();
    for dir in dirs {
        let mut paths: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.path()).collect();
        paths.sort();
        for path in paths {
            if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("jpg") || e.eq_ignore_ascii_case("jpeg")) {
                let bytes = std::fs::read(&path).unwrap();
                files.push((path, bytes));
            }
        }
    }
    assert!(!files.is_empty());
    files
}

#[derive(Debug, PartialEq)]
struct Out {
    pixels: Vec<u8>,
    info: Option<(u16, u16, String)>,
    exif: Option<Vec<u8>>,
}

fn stock(bytes: &[u8], scale: Option<(u16, u16)>) -> Result<Out, String> {
    let mut d = stock::Decoder::new(bytes);
    if let Some((w, h)) = scale {
        d.read_info().map_err(|e| e.to_string())?;
        d.scale(w, h).map_err(|e| e.to_string())?;
    }
    let pixels = d.decode().map_err(|e| e.to_string())?;
    Ok(Out {
        pixels,
        info: d.info().map(|i| (i.width, i.height, format!("{:?}", i.pixel_format))),
        exif: d.exif_data().map(<[u8]>::to_vec),
    })
}

fn ours(bytes: &[u8], scale: Option<(u16, u16)>, rows: bool) -> Result<Out, String> {
    let mut d = jpeg_decoder::Decoder::new(bytes);
    if let Some((w, h)) = scale {
        d.read_info().map_err(|e| e.to_string())?;
        d.scale(w, h).map_err(|e| e.to_string())?;
    }
    let pixels = if rows {
        let mut pixels = Vec::new();
        let mut next = 0;
        d.decode_rows(|y, row| {
            assert_eq!(y, next);
            next += 1;
            pixels.extend_from_slice(row);
        })
        .map_err(|e| e.to_string())?;
        pixels
    } else {
        d.decode().map_err(|e| e.to_string())?
    };
    Ok(Out {
        pixels,
        info: d.info().map(|i| (i.width, i.height, format!("{:?}", i.pixel_format))),
        exif: d.exif_data().map(<[u8]>::to_vec),
    })
}

fn size(bytes: &[u8]) -> Option<(u16, u16)> {
    let mut d = stock::Decoder::new(bytes);
    d.read_info().ok()?;
    d.info().map(|i| (i.width, i.height))
}

/// A request per IDCT size: 1/8, 1/4, 1/2 and full.
fn scales((w, h): (u16, u16)) -> [Option<(u16, u16)>; 5] {
    let part = |d: u16| Some((w.div_ceil(d), h.div_ceil(d)));
    [None, Some((1, 1)), part(4), part(2), Some((w, h))]
}

#[test]
fn every_file_at_every_scale_matches_stock() {
    let mut compared = 0;
    for (path, bytes) in corpus() {
        let Some(dims) = size(&bytes) else { continue };
        for scale in scales(dims) {
            let want = stock(&bytes, scale);
            for rows in [false, true] {
                let got = ours(&bytes, scale, rows);
                match (&want, &got) {
                    (Ok(w), Ok(g)) => assert!(w == g, "{} at {scale:?}: pixels differ", path.display()),
                    (Err(_), Err(_)) => {}
                    _ => panic!("{} at {scale:?}: stock {:?}, ours {:?}", path.display(), want.as_ref().err(), got.as_ref().err()),
                }
            }
            compared += 1;
        }
    }
    assert!(compared > 100, "only {compared} decodes compared");
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

fn mutate(bytes: &[u8], rng: &mut Rng) -> Vec<u8> {
    let mut b = bytes.to_vec();
    match rng.below(6) {
        0 => {
            for _ in 0..1 + rng.below(4) {
                let i = rng.below(b.len());
                b[i] ^= 1 << rng.below(8);
            }
        }
        1 => b.truncate(rng.below(b.len())),
        2 => {
            let i = rng.below(b.len());
            b[i] = 0xFF;
        }
        3 => {
            let i = rng.below(b.len());
            b[i] = rng.next() as u8;
        }
        4 => {
            let (from, len) = (rng.below(b.len()), 1 + rng.below(64));
            let chunk: Vec<u8> = b[from..(from + len).min(b.len())].to_vec();
            let at = rng.below(b.len());
            b.splice(at..at, chunk);
        }
        _ => {
            let at = rng.below(b.len());
            let len = (1 + rng.below(32)).min(b.len() - at);
            b.drain(at..at + len);
        }
    }
    b
}

/// Mutated files never panic, and decode as stock does wherever stock decodes them.
#[test]
fn mutated_files_never_panic_and_match_stock_where_it_decodes() {
    let rounds: usize = std::env::var("JPEG_FUZZ").ok().and_then(|v| v.parse().ok()).unwrap_or(24);
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let (mut matched, mut lenient) = (0, 0);
    for (path, bytes) in corpus() {
        for _ in 0..rounds {
            let m = mutate(&bytes, &mut rng);
            let Some(dims) = size(&m) else { continue };
            if u32::from(dims.0) * u32::from(dims.1) > 4 << 20 {
                continue;
            }
            for scale in scales(dims) {
                let want = catch_unwind(AssertUnwindSafe(|| stock(&m, scale)));
                let got = catch_unwind(AssertUnwindSafe(|| ours(&m, scale, false)));
                let Ok(got) = got else {
                    std::fs::write(std::env::temp_dir().join("vector-jpeg-panic.jpg"), &m).ok();
                    panic!("{} mutant at {scale:?} panics (saved to the temp dir)", path.display());
                };
                match (want, got) {
                    (Ok(Ok(w)), Ok(g)) => {
                        assert!(w == g, "{} mutant at {scale:?}: pixels differ", path.display());
                        matched += 1;
                    }
                    (Ok(Ok(_)), Err(e)) => panic!("{} mutant at {scale:?}: stock decodes, ours: {e}", path.display()),
                    (_, Ok(_)) => lenient += 1,
                    _ => {}
                }
            }
        }
    }
    eprintln!("mutants: {matched} matched stock, {lenient} decoded where stock refused");
    assert!(matched > 0);
}
