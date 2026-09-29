//! PNG encoder: each row takes the filter with the smallest sum of absolute values (libpng's
//! heuristic), strips of rows are filtered and deflated in parallel, and the strips join into
//! one zlib stream through sync flushes, with the Adler-32 combined from each strip's own.

use flate2::{Compress, Compression, FlushCompress, Status};
use rayon::prelude::*;

/// Raw bytes per strip. Each strip starts deflate with an empty window, so smaller strips
/// parallelise better and compress slightly worse.
const STRIP_BYTES: usize = 512 * 1024;

/// Encode 8-bit RGBA (`channels` 4) or RGB (3) at deflate `level` 0-9.
pub fn encode(pixels: &[u8], width: u32, height: u32, channels: usize, level: u8) -> Result<Vec<u8>, String> {
    if width == 0 || height == 0 || width > i32::MAX as u32 || height > i32::MAX as u32 {
        return Err(format!("PNG dimensions {width}x{height} out of range"));
    }
    if channels != 3 && channels != 4 {
        return Err("PNG encoder takes RGB or RGBA".into());
    }
    let row = width as usize * channels;
    if pixels.len() < row * height as usize {
        return Err("PNG pixel buffer shorter than width x height x channels".into());
    }
    let h = height as usize;
    let rows_per_strip = (STRIP_BYTES / row).max(1);
    let strips: Vec<(Vec<u8>, u32, usize)> = (0..h.div_ceil(rows_per_strip))
        .into_par_iter()
        .map(|i| {
            let rows = i * rows_per_strip..((i + 1) * rows_per_strip).min(h);
            let last = rows.end == h;
            let filtered = filter_rows(pixels, row, channels, rows);
            let adler = simd_adler32::adler32(&filtered.as_slice());
            (deflate(&filtered, level, last), adler, filtered.len())
        })
        .collect();

    let body: usize = strips.iter().map(|s| s.0.len()).sum();
    let mut zlib = Vec::with_capacity(body + 6);
    // CMF: deflate, 32 KiB window. FLG: the level hint, padded to a multiple of 31.
    let flevel: u8 = match level {
        0 | 1 => 0,
        2..=5 => 1,
        6 => 2,
        _ => 3,
    };
    let flg = flevel << 6;
    zlib.extend_from_slice(&[0x78, flg + (31 - ((0x78u16 << 8 | u16::from(flg)) % 31) as u8) % 31]);
    let mut adler = 1;
    for (data, a, len) in &strips {
        zlib.extend_from_slice(data);
        adler = adler32_combine(adler, *a, *len);
    }
    zlib.extend_from_slice(&adler.to_be_bytes());

    let mut out = Vec::with_capacity(zlib.len() + 64);
    out.extend_from_slice(b"\x89PNG\r\n\x1a\n");
    let mut ihdr = [0u8; 13];
    ihdr[..4].copy_from_slice(&width.to_be_bytes());
    ihdr[4..8].copy_from_slice(&height.to_be_bytes());
    ihdr[8] = 8;
    ihdr[9] = if channels == 4 { 6 } else { 2 };
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &zlib);
    chunk(&mut out, b"IEND", &[]);
    Ok(out)
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let mut crc = crc32fast::Hasher::new();
    crc.update(kind);
    crc.update(data);
    out.extend_from_slice(&crc.finalize().to_be_bytes());
}

/// Adler-32 of `A || B` from each part's checksum and B's length.
fn adler32_combine(a: u32, b: u32, len_b: usize) -> u32 {
    const BASE: u64 = 65521;
    let (a1, a2) = (u64::from(a & 0xffff), u64::from(a >> 16));
    let (b1, b2) = (u64::from(b & 0xffff), u64::from(b >> 16));
    let s1 = (a1 + b1 + BASE - 1) % BASE;
    let s2 = (a2 + b2 + (len_b as u64 % BASE) * ((a1 + BASE - 1) % BASE)) % BASE;
    (s2 << 16 | s1) as u32
}

/// Raw deflate of one strip (zlib-rs): a sync flush leaves it byte-aligned and open for the
/// next, the last one finishes the stream.
fn deflate(data: &[u8], level: u8, last: bool) -> Vec<u8> {
    let mut c = Compress::new(Compression::new(u32::from(level.min(9))), false);
    let mut out = Vec::with_capacity(data.len() + data.len() / 8 + 64);
    let flush = if last { FlushCompress::Finish } else { FlushCompress::Sync };
    loop {
        let read = c.total_in() as usize;
        let status = c.compress_vec(&data[read..], &mut out, flush).expect("deflate into a growable buffer");
        let done = c.total_in() as usize == data.len() && out.len() < out.capacity();
        if status == Status::StreamEnd || (!last && done) {
            break;
        }
        out.reserve(out.capacity());
    }
    out
}

fn filter_rows(pixels: &[u8], row: usize, bpp: usize, rows: std::ops::Range<usize>) -> Vec<u8> {
    let mut out = vec![0u8; rows.len() * (row + 1)];
    let zero = vec![0u8; row];
    let mut cand = vec![0u8; 4 * row];
    for (k, y) in rows.enumerate() {
        let cur = &pixels[y * row..][..row];
        let prev = if y == 0 { &zero[..] } else { &pixels[(y - 1) * row..][..row] };
        let dst = &mut out[k * (row + 1)..][..row + 1];
        // SAFETY: the AVX2 build runs only where the CPU reports it.
        #[cfg(target_arch = "x86_64")]
        if is_x86_feature_detected!("avx2") {
            unsafe { filter_row_avx2(cur, prev, bpp, &mut cand, dst) };
            continue;
        }
        filter_row(cur, prev, bpp, &mut cand, dst);
    }
    out
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn filter_row_avx2(cur: &[u8], prev: &[u8], bpp: usize, cand: &mut [u8], dst: &mut [u8]) {
    filter_row(cur, prev, bpp, cand, dst)
}

/// Write the filter byte and the filtered row into `dst`. Candidates for Sub, Up, Average
/// and Paeth go through `cand`; loops run over zipped slices, branch-free, so they vectorise.
#[inline(always)]
fn filter_row(cur: &[u8], prev: &[u8], bpp: usize, cand: &mut [u8], dst: &mut [u8]) {
    let n = cur.len();
    let (sub, rest) = cand.split_at_mut(n);
    let (up, rest) = rest.split_at_mut(n);
    let (avg, paeth) = rest.split_at_mut(n);
    let (left, ul) = (&cur[..n - bpp], &prev[..n - bpp]);
    let (x, above) = (&cur[bpp..], &prev[bpp..]);

    sub[..bpp].copy_from_slice(&cur[..bpp]);
    for ((o, &c), &a) in sub[bpp..].iter_mut().zip(x).zip(left) {
        *o = c.wrapping_sub(a);
    }
    for ((o, &c), &b) in up.iter_mut().zip(cur).zip(prev) {
        *o = c.wrapping_sub(b);
    }
    for i in 0..bpp {
        avg[i] = cur[i].wrapping_sub(prev[i] >> 1);
        paeth[i] = cur[i].wrapping_sub(prev[i]);
    }
    for (((o, &c), &a), &b) in avg[bpp..].iter_mut().zip(x).zip(left).zip(above) {
        *o = c.wrapping_sub(((u16::from(a) + u16::from(b)) >> 1) as u8);
    }
    for ((((o, &x), &a), &b), &c) in paeth[bpp..].iter_mut().zip(x).zip(left).zip(above).zip(ul) {
        let (a, b, c) = (i16::from(a), i16::from(b), i16::from(c));
        let (pa, pb, pc) = ((b - c).abs(), (a - c).abs(), (a + b - 2 * c).abs());
        let pred = if pa <= pb && pa <= pc { a } else if pb <= pc { b } else { c };
        *o = x.wrapping_sub(pred as u8);
    }
    let cost = |r: &[u8]| r.iter().fold(0u32, |s, &v| s + u32::from((v as i8).unsigned_abs()));
    let rows: [&[u8]; 5] = [cur, sub, up, avg, paeth];
    let mut best = (0, u32::MAX);
    for (k, r) in rows.iter().enumerate() {
        let c = cost(r);
        if c < best.1 {
            best = (k, c);
        }
    }
    dst[0] = best.0 as u8;
    dst[1..].copy_from_slice(rows[best.0]);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picture(w: usize, h: usize, channels: usize) -> Vec<u8> {
        let mut s = 7u32;
        (0..w * h * channels)
            .map(|i| {
                s ^= s << 13;
                s ^= s >> 17;
                s ^= s << 5;
                let (x, y) = ((i / channels) % w, (i / channels) / w);
                match (x / 13 + y / 7) % 3 {
                    0 => (x * 7 + y) as u8,
                    1 => (s % 4) as u8 + 100,
                    _ => if i % channels == 3 { 255 } else { (s % 251) as u8 },
                }
            })
            .collect()
    }

    #[test]
    fn round_trips_through_a_standard_decoder() {
        for (w, h) in [(1, 1), (3, 2), (17, 9), (640, 480), (5000, 300)] {
            for channels in [3, 4] {
                let px = picture(w, h, channels);
                for level in [1, 6, 9] {
                    let png = encode(&px, w as u32, h as u32, channels, level).unwrap();
                    let img = image::load_from_memory_with_format(&png, image::ImageFormat::Png).unwrap();
                    let back = if channels == 4 { img.to_rgba8().into_raw() } else { img.to_rgb8().into_raw() };
                    assert!(back == px, "{w}x{h}x{channels} level {level}");
                }
            }
        }
    }

    #[test]
    fn adler_combines_like_one_pass() {
        let data = picture(300, 200, 4);
        for cut in [0, 1, 5000, 65521, 100_000, data.len()] {
            let (a, b) = data.split_at(cut);
            let joined = adler32_combine(simd_adler32::adler32(&a), simd_adler32::adler32(&b), b.len());
            assert_eq!(joined, simd_adler32::adler32(&data.as_slice()), "cut at {cut}");
        }
    }

    #[test]
    fn rejects_bad_input() {
        assert!(encode(&[0; 4], 0, 1, 4, 6).is_err());
        assert!(encode(&[0; 4], 2, 1, 4, 6).is_err());
        assert!(encode(&[0; 4], 1, 1, 2, 6).is_err());
    }
}
