//! Baseline JPEG encoder (4:4:4, optimised Huffman tables).
//!
//! Per 8x8 block, one fused kernel: RGB load and YCbCr conversion in float, AAN DCT over whole
//! rows, then quantise, round and clamp; restart-interval strips are transformed and
//! entropy-coded in parallel.

#[cfg(target_arch = "aarch64")]
use super::Neon;
#[cfg(target_arch = "x86_64")]
use super::Avx2;
use super::{Backend, Scalar, Simd, AAN, S_OF, ZIGZAG, ZZ_OF};
use rayon::prelude::*;

/// For kernel row `u` and its 8-bit nonzero mask, the same bits in zigzag positions. Eight
/// lookups turn a block's mask into zigzag order without a data-dependent loop.
static ZZ_MASK: [[u64; 256]; 8] = {
    let mut t = [[0u64; 256]; 8];
    let mut u = 0;
    while u < 8 {
        let mut m = 0;
        while m < 256 {
            let mut v = 0;
            while v < 8 {
                if m & (1 << v) != 0 {
                    t[u][m] |= 1 << ZZ_OF[u * 8 + v];
                }
                v += 1;
            }
            m += 1;
        }
        u += 1;
    }
    t
};

const LUMA_Q: [u16; 64] = [
    16, 11, 10, 16, 24, 40, 51, 61, 12, 12, 14, 19, 26, 58, 60, 55, 14, 13, 16, 24, 40, 57, 69,
    56, 14, 17, 22, 29, 51, 87, 80, 62, 18, 22, 37, 56, 68, 109, 103, 77, 24, 35, 55, 64, 81, 104,
    113, 92, 49, 64, 78, 87, 103, 121, 120, 101, 72, 92, 95, 98, 112, 100, 103, 99,
];

const CHROMA_Q: [u16; 64] = [
    17, 18, 24, 47, 99, 99, 99, 99, 18, 21, 26, 66, 99, 99, 99, 99, 24, 26, 56, 99, 99, 99, 99,
    99, 47, 66, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99,
    99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99,
];

/// Below this many blocks a single strip is cheaper than the pool round trip.
const PARALLEL_MIN_BLOCKS: usize = 4096;

/// Encode packed 8-bit RGB as a baseline JPEG at IJG-scaled `quality` (1-100).
pub fn encode_rgb(pixels: &[u8], width: u32, height: u32, quality: u8) -> Result<Vec<u8>, String> {
    encode_with(pixels, width, height, quality, Backend::detect(), None)
}

struct Tables {
    /// Natural-order quantisers, luma then chroma, as written to DQT.
    quant: [[u8; 64]; 2],
    /// `1 / (q · aan_u · aan_v · 8)` in kernel order, folding the DCT's scale into quantisation.
    recip: [[f32; 64]; 2],
}

impl Tables {
    fn new(quality: u8) -> Self {
        let q = u32::from(quality.clamp(1, 100));
        let scale = if q < 50 { 5000 / q } else { 200 - 2 * q };
        let mut quant = [[0u8; 64]; 2];
        let mut recip = [[0f32; 64]; 2];
        for (t, base) in [&LUMA_Q, &CHROMA_Q].into_iter().enumerate() {
            for i in 0..64 {
                quant[t][i] = ((u32::from(base[i]) * scale + 50) / 100).clamp(1, 255) as u8;
            }
            for u in 0..8 {
                for v in 0..8 {
                    let q = f64::from(quant[t][v * 8 + u]);
                    recip[t][u * 8 + v] = (1.0 / (q * AAN[u] * AAN[v] * 8.0)) as f32;
                }
            }
        }
        Tables { quant, recip }
    }
}

/// One restart interval's entropy symbols. Each entry: value bits << 16 | table << 8 | symbol,
/// where table is 0 luma DC, 1 luma AC, 2 chroma DC, 3 chroma AC.
struct Strip {
    syms: Vec<u32>,
    freq: Box<[u32; 1024]>,
}

fn encode_with(
    pixels: &[u8],
    width: u32,
    height: u32,
    quality: u8,
    backend: Backend,
    rows_per_strip: Option<usize>,
) -> Result<Vec<u8>, String> {
    if width == 0 || height == 0 || width > 65535 || height > 65535 {
        return Err(format!("JPEG dimensions {width}x{height} out of range"));
    }
    let (w, h) = (width as usize, height as usize);
    if pixels.len() < w * h * 3 {
        return Err("JPEG pixel buffer shorter than width x height x 3".into());
    }
    let tables = Tables::new(quality);
    let (mcu_cols, mcu_rows) = (w.div_ceil(8), h.div_ceil(8));

    let rows_per_strip = rows_per_strip.unwrap_or_else(|| {
        let threads = rayon::current_num_threads();
        if mcu_cols * mcu_rows < PARALLEL_MIN_BLOCKS || threads <= 1 {
            mcu_rows
        } else {
            mcu_rows.div_ceil(threads * 4)
        }
    });
    // The restart interval is a 16-bit MCU count.
    let rows_per_strip = rows_per_strip.clamp(1, (65535 / mcu_cols).max(1));
    let strip_count = mcu_rows.div_ceil(rows_per_strip);

    let image = Image { px: pixels, w, h, mcu_cols };
    let pass1 = |i: usize| {
        let rows = i * rows_per_strip..((i + 1) * rows_per_strip).min(mcu_rows);
        transform_strip(&image, rows, &tables, backend)
    };
    let strips: Vec<Strip> = if strip_count == 1 {
        vec![pass1(0)]
    } else {
        (0..strip_count).into_par_iter().map(pass1).collect()
    };

    let mut freq = [0u32; 1024];
    for s in &strips {
        for (f, n) in freq.iter_mut().zip(s.freq.iter()) {
            *f += n;
        }
    }
    let huff: Vec<HuffTable> = (0..4).map(|t| HuffTable::optimal(&freq[t * 256..t * 256 + 256])).collect();
    let mut lut = [0u32; 1024];
    for (t, table) in huff.iter().enumerate() {
        for sym in 0..256 {
            let (code, len) = table.codes[sym];
            let size = if t % 2 == 0 { sym as u32 } else { sym as u32 & 15 };
            lut[t * 256 + sym] = (u32::from(code) << 16) | (size << 8) | u32::from(len);
        }
    }

    let coded: Vec<Vec<u8>> = if strip_count == 1 {
        vec![entropy_code(&strips[0].syms, &lut)]
    } else {
        strips.par_iter().map(|s| entropy_code(&s.syms, &lut)).collect()
    };

    let body: usize = coded.iter().map(Vec::len).sum();
    let mut out = Vec::with_capacity(body + 2 * strip_count + 700);
    write_headers(&mut out, width, height, &tables, &huff, (strip_count > 1).then_some(rows_per_strip * mcu_cols));
    for (i, c) in coded.iter().enumerate() {
        out.extend_from_slice(c);
        if i + 1 < coded.len() {
            out.extend_from_slice(&[0xFF, 0xD0 + (i % 8) as u8]);
        }
    }
    out.extend_from_slice(&[0xFF, 0xD9]);
    Ok(out)
}

struct Image<'a> {
    px: &'a [u8],
    w: usize,
    h: usize,
    mcu_cols: usize,
}

fn transform_strip(img: &Image, rows: std::ops::Range<usize>, t: &Tables, backend: Backend) -> Strip {
    // SAFETY: each backend runs only where `Backend::detect` found its features.
    unsafe {
        match backend {
            #[cfg(target_arch = "x86_64")]
            Backend::Avx2 => strip_avx2(img, rows, t),
            #[cfg(target_arch = "aarch64")]
            Backend::Neon => strip_neon(img, rows, t),
            Backend::Scalar => strip_generic::<Scalar>(img, rows, t),
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,bmi1,bmi2,lzcnt")]
unsafe fn strip_avx2(img: &Image, rows: std::ops::Range<usize>, t: &Tables) -> Strip {
    strip_generic::<Avx2>(img, rows, t)
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn strip_neon(img: &Image, rows: std::ops::Range<usize>, t: &Tables) -> Strip {
    strip_generic::<Neon>(img, rows, t)
}

#[inline(always)]
unsafe fn strip_generic<S: Simd>(img: &Image, rows: std::ops::Range<usize>, t: &Tables) -> Strip {
    let blocks = rows.len() * img.mcu_cols * 3;
    let mut syms = Vec::with_capacity(blocks * 12);
    let mut freq = Box::new([0u32; 1024]);
    let mut pred = [0i32; 3];
    let mut coefs = [[0i32; 64]; 3];
    let mut edge = [0u8; 8 * 8 * 3];
    let stride = img.w * 3;
    for my in rows {
        let y0 = my * 8;
        for mx in 0..img.mcu_cols {
            let x0 = mx * 8;
            let (src, src_stride) = if x0 + 8 <= img.w && y0 + 8 <= img.h {
                (img.px.as_ptr().add(y0 * stride + x0 * 3), stride)
            } else {
                // Partial block: replicate the last column and row, as decoders expect.
                for r in 0..8 {
                    let sy = (y0 + r).min(img.h - 1);
                    for c in 0..8 {
                        let s = (sy * img.w + (x0 + c).min(img.w - 1)) * 3;
                        edge[(r * 8 + c) * 3..][..3].copy_from_slice(&img.px[s..s + 3]);
                    }
                }
                (edge.as_ptr(), 24)
            };
            let masks = transform_block::<S>(src, src_stride, &t.recip, &mut coefs);
            for c in 0..3 {
                let dc_table = if c == 0 { 0 } else { 2 };
                emit_block(&coefs[c], masks[c], &mut pred[c], dc_table, &mut syms, &mut freq);
            }
        }
    }
    Strip { syms, freq }
}

/// Colour-convert, DCT and quantise one 8x8 RGB block into three coefficient blocks in
/// `u * 8 + v` order. Returns each block's nonzero mask in the same order.
#[inline(always)]
unsafe fn transform_block<S: Simd>(
    src: *const u8,
    stride: usize,
    recip: &[[f32; 64]; 2],
    out: &mut [[i32; 64]; 3],
) -> [u64; 3] {
    let zero = S::splat(0.0);
    let mut planes = [[zero; 8]; 3];
    let (yr, yg, yb, yo) = (S::splat(0.299), S::splat(0.587), S::splat(0.114), S::splat(-128.0));
    let (br, bg, bb) = (S::splat(-0.168736), S::splat(-0.331264), S::splat(0.5));
    let (rr, rg, rb) = (S::splat(0.5), S::splat(-0.418688), S::splat(-0.081312));
    let [py, pb, pr] = &mut planes;
    for (row, ((y, cb), cr)) in py.iter_mut().zip(pb.iter_mut()).zip(pr.iter_mut()).enumerate() {
        let [r, g, b] = S::load_rgb(src.add(row * stride));
        *y = S::add(S::add(S::add(S::mul(r, yr), S::mul(g, yg)), S::mul(b, yb)), yo);
        *cb = S::add(S::add(S::mul(r, br), S::mul(g, bg)), S::mul(b, bb));
        *cr = S::add(S::add(S::mul(r, rr), S::mul(g, rg)), S::mul(b, rb));
    }
    let mut masks = [0u64; 3];
    for c in 0..3 {
        let p = &mut planes[c];
        dct8::<S>(p);
        S::transpose(p);
        dct8::<S>(p);
        let table = &recip[(c != 0) as usize];
        for (u, &row) in p.iter().enumerate() {
            let m = S::quantize(row, table.as_ptr().add(u * 8), out[c].as_mut_ptr().add(u * 8));
            masks[c] |= u64::from(m) << (u * 8);
        }
    }
    masks
}

/// AAN forward DCT across the eight vectors (one 1-D transform per lane). Outputs are scaled
/// by `AAN[k]`; quantisation divides it back out.
#[inline(always)]
unsafe fn dct8<S: Simd>(d: &mut [S::V; 8]) {
    let (a, s) = (S::add, S::sub);
    let tmp0 = a(d[0], d[7]);
    let tmp7 = s(d[0], d[7]);
    let tmp1 = a(d[1], d[6]);
    let tmp6 = s(d[1], d[6]);
    let tmp2 = a(d[2], d[5]);
    let tmp5 = s(d[2], d[5]);
    let tmp3 = a(d[3], d[4]);
    let tmp4 = s(d[3], d[4]);

    let tmp10 = a(tmp0, tmp3);
    let tmp13 = s(tmp0, tmp3);
    let tmp11 = a(tmp1, tmp2);
    let tmp12 = s(tmp1, tmp2);
    d[0] = a(tmp10, tmp11);
    d[4] = s(tmp10, tmp11);
    let z1 = S::mul(a(tmp12, tmp13), S::splat(std::f32::consts::FRAC_1_SQRT_2));
    d[2] = a(tmp13, z1);
    d[6] = s(tmp13, z1);

    let tmp10 = a(tmp4, tmp5);
    let tmp11 = a(tmp5, tmp6);
    let tmp12 = a(tmp6, tmp7);
    let z5 = S::mul(s(tmp10, tmp12), S::splat(0.38268343));
    let z2 = a(S::mul(tmp10, S::splat(0.5411961)), z5);
    let z4 = a(S::mul(tmp12, S::splat(1.306563)), z5);
    let z3 = S::mul(tmp11, S::splat(std::f32::consts::FRAC_1_SQRT_2));
    let z11 = a(tmp7, z3);
    let z13 = s(tmp7, z3);
    d[5] = a(z13, z2);
    d[3] = s(z13, z2);
    d[1] = a(z11, z4);
    d[7] = s(z11, z4);
}

/// JPEG magnitude category and its value bits (negatives as one's complement).
#[inline(always)]
fn magnitude(v: i32) -> (u32, u32) {
    let size = 32 - v.unsigned_abs().leading_zeros();
    (size, ((v + (v >> 31)) as u32) & ((1u32 << size) - 1))
}

/// Most symbols one block can emit: DC, 63 AC and EOB (a ZRL always covers 16 positions).
const MAX_BLOCK_SYMS: usize = 65;

#[inline(always)]
fn emit_block(coef: &[i32; 64], mask: u64, pred: &mut i32, dc_table: u32, syms: &mut Vec<u32>, freq: &mut [u32; 1024]) {
    syms.reserve(MAX_BLOCK_SYMS);
    let start = syms.len();
    let base = syms.as_mut_ptr();
    let mut n = start;
    let mut push = |sym: u32, bits: u32| {
        // SAFETY: at most MAX_BLOCK_SYMS pushes into the capacity reserved above.
        unsafe { base.add(n).write((bits << 16) | sym) };
        n += 1;
        freq[sym as usize] += 1;
    };
    let dc = coef[0];
    let (size, bits) = magnitude(dc - *pred);
    *pred = dc;
    push(dc_table << 8 | size, bits);

    let ac = (dc_table + 1) << 8;
    let mut order = 0;
    for (u, t) in ZZ_MASK.iter().enumerate() {
        order |= t[((mask >> (u * 8)) & 0xff) as usize];
    }
    order &= !1;
    let mut last = 0;
    while order != 0 {
        let k = order.trailing_zeros();
        let mut run = k - last - 1;
        while run >= 16 {
            push(ac | 0xF0, 0);
            run -= 16;
        }
        let (size, bits) = magnitude(coef[S_OF[k as usize] as usize]);
        push(ac | run << 4 | size, bits);
        last = k;
        order &= order - 1;
    }
    if last != 63 {
        push(ac, 0);
    }
    // SAFETY: every slot up to `n` was written above.
    unsafe { syms.set_len(n) };
}

/// Huffman-code one strip's symbols, byte-stuffed, padded with 1s to a byte boundary.
fn entropy_code(syms: &[u32], lut: &[u32; 1024]) -> Vec<u8> {
    // Each symbol is at most 27 bits; 8 bytes of slack take the last unaligned store.
    let mut out: Vec<u8> = Vec::with_capacity(syms.len() * 4 + 16);
    let p = out.as_mut_ptr();
    let (mut acc, mut nbits, mut pos) = (0u64, 0u32, 0usize);
    // Branchless: store the pending bits big-endian every symbol and advance by whole bytes; the
    // partial byte is rewritten by the next store.
    for &e in syms {
        let h = lut[(e & 0x3ff) as usize];
        let size = (h >> 8) & 0xff;
        let len = (h & 0xff) + size;
        acc = (acc << len) | u64::from(((h >> 16) << size) | (e >> 16));
        nbits += len;
        // SAFETY: pos grows by at most 27 bits a symbol, within the capacity reserved above.
        unsafe { (p.add(pos) as *mut u64).write_unaligned((acc << (64 - nbits)).to_be()) };
        pos += (nbits >> 3) as usize;
        nbits &= 7;
    }
    if nbits > 0 {
        let pad = 8 - nbits;
        acc = (acc << pad) | ((1 << pad) - 1);
        // SAFETY: as above.
        unsafe { *p.add(pos) = (acc & 0xff) as u8 };
        pos += 1;
    }
    // SAFETY: bytes 0..pos were written by the stores above.
    unsafe { out.set_len(pos) };
    stuff_ff(out)
}

/// Insert the 0x00 after every 0xFF that T.81 requires in entropy-coded data, scanning a word at
/// a time since 0xFF is roughly one byte in 256.
fn stuff_ff(mut data: Vec<u8>) -> Vec<u8> {
    let mut ff = Vec::new();
    let words = data.chunks_exact(8);
    let tail = words.remainder().len();
    for (i, w) in words.enumerate() {
        let inv = !u64::from_ne_bytes(w.try_into().unwrap());
        if inv.wrapping_sub(0x0101_0101_0101_0101) & !inv & 0x8080_8080_8080_8080 != 0 {
            ff.extend((0..8).filter(|&k| w[k] == 0xFF).map(|k| i * 8 + k));
        }
    }
    let n = data.len();
    ff.extend((n - tail..n).filter(|&i| data[i] == 0xFF));
    if ff.is_empty() {
        return data;
    }
    data.resize(n + ff.len(), 0);
    // Back to front: the segment after the j-th 0xFF moves up by j + 1, its zero lands just after
    // that 0xFF's new position.
    let mut end = n;
    for (j, &i) in ff.iter().enumerate().rev() {
        data.copy_within(i + 1..end, i + j + 2);
        data[i + j + 1] = 0;
        end = i + 1;
    }
    data
}

struct HuffTable {
    /// Count of codes per length 1..=16 (index 0 unused).
    bits: [u8; 17],
    values: Vec<u8>,
    /// (code, length) per symbol; length 0 for symbols that never occur.
    codes: [(u16, u8); 256],
}

impl HuffTable {
    /// Length-limited optimal table per ITU T.81 Annex K.2, with the reserved all-ones code.
    fn optimal(freq_in: &[u32]) -> Self {
        let mut freq = [0u64; 257];
        for (f, &n) in freq.iter_mut().zip(freq_in) {
            *f = u64::from(n);
        }
        freq[256] = 1;
        let mut size = [0usize; 257];
        let mut others = [usize::MAX; 257];
        loop {
            // Least frequency first; ties go to the larger symbol, which keeps the reserved
            // symbol among the longest codes.
            let least = |skip: usize| {
                let mut best = (u64::MAX, usize::MAX);
                for (i, &f) in freq.iter().enumerate() {
                    if f != 0 && i != skip && f <= best.0 {
                        best = (f, i);
                    }
                }
                best.1
            };
            let mut v1 = least(usize::MAX);
            let mut v2 = least(v1);
            if v2 == usize::MAX {
                break;
            }
            freq[v1] += freq[v2];
            freq[v2] = 0;
            size[v1] += 1;
            while others[v1] != usize::MAX {
                v1 = others[v1];
                size[v1] += 1;
            }
            others[v1] = v2;
            size[v2] += 1;
            while others[v2] != usize::MAX {
                v2 = others[v2];
                size[v2] += 1;
            }
        }
        let mut count = [0u32; 258];
        for &s in &size {
            if s > 0 {
                count[s] += 1;
            }
        }
        for i in (17..count.len()).rev() {
            while count[i] > 0 {
                let mut j = i - 2;
                while count[j] == 0 {
                    j -= 1;
                }
                count[i] -= 2;
                count[i - 1] += 1;
                count[j + 1] += 2;
                count[j] -= 1;
            }
        }
        if let Some(longest) = (1..=16).rev().find(|&l| count[l] > 0) {
            count[longest] -= 1;
        }

        let mut values = Vec::new();
        for len in 1..size.len() {
            for (sym, &s) in size[..256].iter().enumerate() {
                if s == len {
                    values.push(sym as u8);
                }
            }
        }
        let mut bits = [0u8; 17];
        let mut codes = [(0u16, 0u8); 256];
        let (mut code, mut k) = (0u32, 0);
        for len in 1..=16 {
            bits[len] = count[len] as u8;
            for _ in 0..count[len] {
                codes[values[k] as usize] = (code as u16, len as u8);
                code += 1;
                k += 1;
            }
            code <<= 1;
        }
        HuffTable { bits, values, codes }
    }
}

fn write_headers(
    out: &mut Vec<u8>,
    width: u32,
    height: u32,
    t: &Tables,
    huff: &[HuffTable],
    restart_interval: Option<usize>,
) {
    let seg = |out: &mut Vec<u8>, marker: u8, body: &[u8]| {
        out.extend_from_slice(&[0xFF, marker]);
        out.extend_from_slice(&(body.len() as u16 + 2).to_be_bytes());
        out.extend_from_slice(body);
    };
    out.extend_from_slice(&[0xFF, 0xD8]);
    seg(out, 0xE0, b"JFIF\0\x01\x01\0\0\x01\0\x01\0\0");

    let mut dqt = Vec::with_capacity(130);
    for (id, q) in t.quant.iter().enumerate() {
        dqt.push(id as u8);
        dqt.extend(ZIGZAG.iter().map(|&n| q[n as usize]));
    }
    seg(out, 0xDB, &dqt);

    let (w, h) = ((width as u16).to_be_bytes(), (height as u16).to_be_bytes());
    seg(out, 0xC0, &[8, h[0], h[1], w[0], w[1], 3, 1, 0x11, 0, 2, 0x11, 1, 3, 0x11, 1]);

    let mut dht = Vec::new();
    for (i, table) in huff.iter().enumerate() {
        // Class (DC 0, AC 1) in the high nibble, destination (luma 0, chroma 1) in the low.
        dht.push((((i % 2) << 4) | (i / 2)) as u8);
        dht.extend_from_slice(&table.bits[1..]);
        dht.extend_from_slice(&table.values);
    }
    seg(out, 0xC4, &dht);

    if let Some(ri) = restart_interval {
        seg(out, 0xDD, &(ri as u16).to_be_bytes());
    }
    seg(out, 0xDA, &[3, 1, 0x00, 2, 0x11, 3, 0x11, 0, 63, 0]);
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn psnr(a: &[u8], b: &[u8]) -> f64 {
        let mse = a.iter().zip(b).map(|(x, y)| (f64::from(*x) - f64::from(*y)).powi(2)).sum::<f64>() / a.len() as f64;
        10.0 * (255.0f64.powi(2) / mse.max(1e-9)).log10()
    }

    fn decode(jpeg: &[u8]) -> image::RgbImage {
        image::load_from_memory_with_format(jpeg, image::ImageFormat::Jpeg).expect("decodes").to_rgb8()
    }

    #[test]
    fn the_kernel_computes_the_dct() {
        let mut s = 7u32;
        let px: Vec<u8> = (0..192).map(|_| { s = s.wrapping_mul(1_103_515_245).wrapping_add(12345); (s >> 16) as u8 }).collect();
        let mut recip = [[0f32; 64]; 2];
        for u in 0..8 {
            for v in 0..8 {
                recip[0][u * 8 + v] = (1.0 / (AAN[u] * AAN[v] * 8.0)) as f32;
            }
        }
        recip[1] = recip[0];
        let mut out = [[0i32; 64]; 3];
        unsafe { transform_block::<Scalar>(px.as_ptr(), 24, &recip, &mut out) };
        let luma: Vec<f64> = px.chunks(3).map(|p| 0.299 * f64::from(p[0]) + 0.587 * f64::from(p[1]) + 0.114 * f64::from(p[2]) - 128.0).collect();
        for u in 0..8 {
            for v in 0..8 {
                let c = |k: usize| if k == 0 { std::f64::consts::FRAC_1_SQRT_2 } else { 1.0 };
                let mut sum = 0.0;
                for y in 0..8 {
                    for x in 0..8 {
                        let pi = std::f64::consts::PI;
                        sum += luma[y * 8 + x] * (((2 * x + 1) as f64 * u as f64 * pi) / 16.0).cos() * (((2 * y + 1) as f64 * v as f64 * pi) / 16.0).cos();
                    }
                }
                let reference = 0.25 * c(u) * c(v) * sum;
                let got = f64::from(out[0][u * 8 + v]);
                assert!((reference - got).abs() <= 0.51, "F({u},{v}): {got} vs {reference:.2}");
            }
        }
    }

    #[test]
    fn every_backend_writes_the_same_bytes() {
        let native = Backend::detect();
        #[cfg(target_arch = "aarch64")]
        assert_eq!(native, Backend::Neon);
        for (w, h) in [(1, 1), (7, 9), (8, 8), (17, 33), (203, 61)] {
            let px = noise_image(w, h, (w * h) as u32);
            for q in [10, 50, 85, 100] {
                let a = encode_with(&px, w as u32, h as u32, q, Backend::Scalar, None).unwrap();
                let b = encode_with(&px, w as u32, h as u32, q, native, None).unwrap();
                assert!(a == b, "{w}x{h} q{q}: {native:?} differs from scalar");
            }
        }
    }

    #[test]
    fn decodes_to_the_source_at_every_shape() {
        for (w, h) in [(1, 1), (2, 3), (7, 9), (8, 8), (9, 8), (64, 1), (1, 64), (320, 241)] {
            let px = noise_image(w, h, 99);
            let jpeg = encode_rgb(&px, w as u32, h as u32, 95).unwrap();
            let img = decode(&jpeg);
            assert_eq!(img.dimensions(), (w as u32, h as u32));
            assert!(psnr(&px, img.as_raw()) > 30.0, "{w}x{h}: {:.1} dB", psnr(&px, img.as_raw()));
        }
    }

    #[test]
    fn restart_strips_decode_identically_to_one_strip() {
        let (w, h) = (97, 131);
        let px = noise_image(w, h, 5);
        let one = encode_with(&px, w as u32, h as u32, 85, Backend::detect(), Some(usize::MAX)).unwrap();
        for rows in [1, 2, 5] {
            let many = encode_with(&px, w as u32, h as u32, 85, Backend::detect(), Some(rows)).unwrap();
            assert!(many.windows(2).any(|p| p == [0xFF, 0xDD]), "restart interval declared");
            assert_eq!(decode(&one).as_raw(), decode(&many).as_raw(), "{rows} rows per strip");
        }
    }

    #[test]
    fn quality_tracks_the_ijg_scale() {
        let (w, h) = (256, 256);
        let px = noise_image(w, h, 3);
        let mut last = (0.0, 0);
        for q in [20, 50, 75, 90, 100] {
            let jpeg = encode_rgb(&px, w as u32, h as u32, q).unwrap();
            let p = psnr(&px, decode(&jpeg).as_raw());
            assert!(p > last.0 && jpeg.len() > last.1, "q{q} not better than the step below");
            last = (p, jpeg.len());
        }
    }

    #[test]
    fn huffman_tables_are_valid_prefix_codes() {
        let mut cases: Vec<Vec<u32>> = vec![vec![0; 256], (0..256).map(|i| i as u32 + 1).collect()];
        cases[0][0] = 5;
        // Fibonacci frequencies push the unconstrained tree far past 16 levels.
        let mut fib = vec![0u32; 256];
        let (mut a, mut b) = (1u32, 1u32);
        for f in fib.iter_mut().take(40) {
            *f = a;
            (a, b) = (b, a.saturating_add(b));
        }
        cases.push(fib);
        for freq in cases {
            let t = HuffTable::optimal(&freq);
            let used = freq.iter().filter(|&&f| f > 0).count();
            assert_eq!(t.values.len(), used);
            let kraft: f64 = (1..=16).map(|l| f64::from(t.bits[l]) / f64::from(1u32 << l)).sum();
            assert!(kraft < 1.0, "the all-ones code stays reserved");
            for (sym, &f) in freq.iter().enumerate() {
                let (code, len) = t.codes[sym];
                assert_eq!(f > 0, len > 0);
                if len > 0 {
                    assert!(len <= 16 && u32::from(code) < (1 << len) - 1);
                }
            }
        }
    }

    /// On-device comparison with the image crate's encoder:
    /// `cargo test --release --no-default-features --lib simd::jpeg -- --ignored --nocapture`
    #[test]
    #[ignore = "benchmark, not an assertion"]
    fn bench_against_the_image_crate() {
        let (w, h) = (1920u32, 1440u32);
        let px = noise_image(w as usize, h as usize, 11);
        let time = |f: &dyn Fn() -> Vec<u8>| {
            let mut t: Vec<f64> = (0..9)
                .map(|_| {
                    let s = std::time::Instant::now();
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
        println!("{:?}, {} threads, 1920x1440 q85", Backend::detect(), rayon::current_num_threads());
        println!("  image crate  {:7.1} ms  {} B", image.0, image.1);
        println!("  ours, 1      {:7.1} ms  {} B  {:.1}x", single.0, single.1, image.0 / single.0);
        println!("  ours, all    {:7.1} ms  {} B  {:.1}x", all.0, all.1, image.0 / all.0);
    }

    #[test]
    fn rejects_bad_dimensions() {
        assert!(encode_rgb(&[0; 3], 0, 1, 85).is_err());
        assert!(encode_rgb(&[0; 3], 70_000, 1, 85).is_err());
        assert!(encode_rgb(&[0; 5], 2, 1, 85).is_err());
    }
}
