//! Baseline JPEG decoder: 8-bit sequential Huffman, one interleaved scan, greyscale or YCbCr
//! with 4:4:4, 4:2:2, 4:2:0 or 4:4:0 chroma. Everything else is refused as `Unsupported` for
//! the caller to hand to a general-purpose decoder.

#[cfg(target_arch = "aarch64")]
use super::Neon;
#[cfg(target_arch = "x86_64")]
use super::Avx2;
use super::{Backend, Scalar, Simd, AAN, S_OF, ZZ_OF};
use rayon::prelude::*;

#[derive(Debug, PartialEq, Eq)]
pub enum DecodeError {
    /// Valid JPEG this decoder does not handle (progressive, arithmetic, CMYK, ...).
    Unsupported(&'static str),
    Corrupt(&'static str),
}

pub struct Decoded {
    pub width: u32,
    pub height: u32,
    /// 1 (greyscale) or 3 (RGB).
    pub channels: u8,
    pub pixels: Vec<u8>,
}

/// Matches the general decoder's bounds, so refusing here never admits more there.
const MAX_DIMENSION: usize = 16_384;
const MAX_ALLOC: usize = 256 * 1024 * 1024;

#[cfg(test)]
fn decode(data: &[u8]) -> Result<Decoded, DecodeError> {
    decode_with(data, Backend::detect(), None, 0)
}

/// Decode at the smallest DCT scale (1/1, 1/2, 1/4 or 1/8) whose longer side is still at least
/// `min_long_side`, as when the image is headed for a downscale anyway. Scaling inside the IDCT
/// keeps each block's low frequencies, a proper low-pass, and skips most of the work.
pub fn decode_at_least(data: &[u8], min_long_side: u32) -> Result<Decoded, DecodeError> {
    decode_with(data, Backend::detect(), None, min_long_side as usize)
}

/// `chunks` forces the scan's split (tests); None picks by size and pool.
fn decode_with(data: &[u8], backend: Backend, chunks: Option<usize>, min_long_side: usize) -> Result<Decoded, DecodeError> {
    let header = parse(data)?;
    let long = header.width.max(header.height);
    let denom = [8, 4, 2].into_iter().find(|&d| min_long_side > 0 && long.div_ceil(d) >= min_long_side).unwrap_or(1);
    let frame = Frame::new(&header, denom)?;
    let entropy = destuff(header.scan);
    match entropy.next_marker {
        None | Some(0xD9) => {}
        Some(0xDA) => return Err(DecodeError::Unsupported("multiple scans")),
        Some(_) => return Err(DecodeError::Unsupported("marker after the scan")),
    }
    let mut planes: Vec<Plane> = frame
        .comps
        .iter()
        .map(|c| {
            let (bw, bh) = frame.block_size[c.index];
            let stride = (frame.mcux * c.h * bw).next_multiple_of(8);
            Plane { data: vec![0; stride * frame.mcuy * c.v * bh], stride }
        })
        .collect();
    scan(&frame, &entropy, &mut planes, backend, chunks)?;
    Ok(to_pixels(&frame, &planes, backend))
}

struct Header<'a> {
    width: usize,
    height: usize,
    comps: Vec<Component>,
    qt: [Option<[u16; 64]>; 4],
    dc: [Option<Box<Huff>>; 4],
    ac: [Option<Box<Huff>>; 4],
    restart: usize,
    /// Everything after the SOS header.
    scan: &'a [u8],
}

#[derive(Clone)]
struct Component {
    index: usize,
    id: u8,
    h: usize,
    v: usize,
    tq: usize,
    dc: usize,
    ac: usize,
}

fn be16(d: &[u8], i: usize) -> Result<usize, DecodeError> {
    d.get(i..i + 2).map(|b| usize::from(u16::from_be_bytes([b[0], b[1]]))).ok_or(DecodeError::Corrupt("truncated header"))
}

fn parse(data: &[u8]) -> Result<Header<'_>, DecodeError> {
    use DecodeError::{Corrupt, Unsupported};
    if !data.starts_with(&[0xFF, 0xD8]) {
        return Err(Corrupt("not a JPEG"));
    }
    let mut h = Header { width: 0, height: 0, comps: Vec::new(), qt: [None; 4], dc: Default::default(), ac: Default::default(), restart: 0, scan: &[] };
    let (mut jfif, mut adobe) = (false, None);
    let mut i = 2;
    loop {
        if data.get(i) != Some(&0xFF) {
            return Err(Corrupt("expected a marker"));
        }
        while data.get(i) == Some(&0xFF) {
            i += 1;
        }
        let marker = *data.get(i).ok_or(Corrupt("truncated header"))?;
        i += 1;
        match marker {
            0x01 | 0xD0..=0xD7 => continue,
            0xD8 | 0xD9 => return Err(Corrupt("no scan")),
            _ => {}
        }
        let len = be16(data, i)?;
        let seg = data.get(i + 2..i + len).filter(|_| len >= 2).ok_or(Corrupt("truncated segment"))?;
        i += len;
        match marker {
            0xC0 | 0xC1 => {
                if !h.comps.is_empty() {
                    return Err(Corrupt("second frame header"));
                }
                if seg.len() < 6 {
                    return Err(Corrupt("short frame header"));
                }
                if seg[0] != 8 {
                    return Err(Unsupported("sample precision"));
                }
                h.height = be16(seg, 1)?;
                h.width = be16(seg, 3)?;
                if h.height == 0 {
                    return Err(Unsupported("height defined by DNL"));
                }
                if h.width == 0 {
                    return Err(Corrupt("zero width"));
                }
                let n = usize::from(seg[5]);
                if n != 1 && n != 3 {
                    return Err(Unsupported("component count"));
                }
                let spec = seg.get(6..6 + 3 * n).ok_or(Corrupt("short frame header"))?;
                for c in spec.chunks_exact(3) {
                    let (hs, vs, tq) = (usize::from(c[1] >> 4), usize::from(c[1] & 15), usize::from(c[2]));
                    if !(1..=4).contains(&hs) || !(1..=4).contains(&vs) || tq > 3 {
                        return Err(Corrupt("component parameters"));
                    }
                    h.comps.push(Component { index: h.comps.len(), id: c[0], h: hs, v: vs, tq, dc: 0, ac: 0 });
                }
            }
            0xC2 | 0xC6 | 0xCA | 0xCE => return Err(Unsupported("progressive")),
            0xC3 | 0xC5 | 0xC7 | 0xC9 | 0xCB | 0xCC | 0xCD | 0xCF => {
                return Err(Unsupported("lossless, hierarchical or arithmetic"))
            }
            0xC4 => {
                let mut s = seg;
                while !s.is_empty() {
                    let (class, id) = (s[0] >> 4, usize::from(s[0] & 15));
                    let counts = s.get(1..17).ok_or(Corrupt("short Huffman table"))?;
                    let total: usize = counts.iter().map(|&c| usize::from(c)).sum();
                    let values = s.get(17..17 + total).ok_or(Corrupt("short Huffman table"))?;
                    if class > 1 || id > 3 || total > 256 {
                        return Err(Corrupt("Huffman table parameters"));
                    }
                    let table = Huff::new(counts, values)?;
                    if class == 0 {
                        h.dc[id] = Some(table);
                    } else {
                        h.ac[id] = Some(table);
                    }
                    s = &s[17 + total..];
                }
            }
            0xDB => {
                let mut s = seg;
                while !s.is_empty() {
                    let (wide, id) = (s[0] >> 4, usize::from(s[0] & 15));
                    if wide > 1 || id > 3 {
                        return Err(Corrupt("quantisation table parameters"));
                    }
                    let n = if wide == 1 { 128 } else { 64 };
                    let body = s.get(1..1 + n).ok_or(Corrupt("short quantisation table"))?;
                    h.qt[id] = Some(std::array::from_fn(|k| {
                        if wide == 1 {
                            u16::from_be_bytes([body[2 * k], body[2 * k + 1]])
                        } else {
                            u16::from(body[k])
                        }
                    }));
                    s = &s[1 + n..];
                }
            }
            0xDD => h.restart = be16(seg, 0)?,
            0xDC => return Err(Unsupported("DNL")),
            0xE0 => jfif |= seg.starts_with(b"JFIF\0"),
            0xEE if seg.starts_with(b"Adobe") && seg.len() >= 12 => adobe = Some(seg[11]),
            0xDA => {
                if h.comps.is_empty() {
                    return Err(Corrupt("scan before frame header"));
                }
                let ns = usize::from(*seg.first().ok_or(Corrupt("short scan header"))?);
                if ns != h.comps.len() {
                    return Err(Unsupported("non-interleaved scans"));
                }
                let spec = seg.get(1..1 + 2 * ns + 3).ok_or(Corrupt("short scan header"))?;
                for (c, s) in h.comps.iter_mut().zip(spec.chunks_exact(2)) {
                    if s[0] != c.id {
                        return Err(Unsupported("scan component order"));
                    }
                    (c.dc, c.ac) = (usize::from(s[1] >> 4), usize::from(s[1] & 15));
                    if c.dc > 3 || c.ac > 3 {
                        return Err(Corrupt("scan table selector"));
                    }
                }
                if spec[2 * ns..] != [0, 63, 0] {
                    return Err(Unsupported("spectral selection"));
                }
                h.scan = &data[i..];
                break;
            }
            _ => {}
        }
    }
    if h.comps.len() == 3 && (adobe == Some(0) || (adobe.is_none() && !jfif && h.comps.iter().map(|c| c.id).eq(*b"RGB"))) {
        return Err(Unsupported("RGB-coded JPEG"));
    }
    Ok(h)
}

/// Block geometry and the tables the scan needs, resolved and validated.
struct Frame {
    /// Output size, after any DCT scaling.
    width: usize,
    height: usize,
    /// Pixels each component's block decodes to, across and down.
    block_size: Vec<(usize, usize)>,
    /// How far chroma still has to be upsampled to meet luma.
    upsample: (usize, usize),
    /// Plain dequantisation (kernel order) for the scaled IDCT.
    qraw: Vec<[f32; 64]>,
    /// Cosine bases for 1-, 2-, 4- and 8-point outputs: `[n][i * 8 + u]`.
    basis: [[f32; 64]; 4],
    mcux: usize,
    mcuy: usize,
    comps: Vec<Component>,
    /// Dequantisation per component in kernel order, with the AAN scale and 1/8 folded in.
    qs: Vec<[f32; 64]>,
    dc: Vec<Huff>,
    ac: Vec<Huff>,
    restart: usize,
    /// Per block of an MCU, in coding order: component and its column and row in the MCU.
    slots: Vec<(usize, usize, usize)>,
}

impl Frame {
    fn new(h: &Header, denom: usize) -> Result<Self, DecodeError> {
        use DecodeError::{Corrupt, Unsupported};
        if h.width > MAX_DIMENSION || h.height > MAX_DIMENSION {
            return Err(Unsupported("dimensions"));
        }
        let mut comps = h.comps.clone();
        let (mcux, mcuy) = if comps.len() == 1 {
            // A lone component is coded non-interleaved: one block per MCU whatever its factors.
            (comps[0].h, comps[0].v) = (1, 1);
            (h.width.div_ceil(8), h.height.div_ceil(8))
        } else {
            let (y, c) = (&comps[0], &comps[1..]);
            if y.h > 2 || y.v > 2 || c.iter().any(|c| c.h != 1 || c.v != 1) {
                return Err(Unsupported("chroma subsampling"));
            }
            (h.width.div_ceil(8 * y.h), h.height.div_ceil(8 * y.v))
        };
        let planes: usize = comps.iter().map(|c| mcux * c.h * mcuy * c.v * 64).sum();
        if planes + h.width * h.height * comps.len() > MAX_ALLOC {
            return Err(Unsupported("dimensions"));
        }
        let n = 8 / denom;
        let (hmax, vmax) = (comps[0].h, comps[0].v);
        // Chroma decodes at luma's scale where the IDCT can reach it, leaving no upsampling.
        let block_size: Vec<(usize, usize)> = comps.iter().map(|c| ((n * hmax / c.h).min(8), (n * vmax / c.v).min(8))).collect();
        let upsample = if comps.len() == 1 { (1, 1) } else { (n * hmax / block_size[1].0, n * vmax / block_size[1].1) };
        let basis = std::array::from_fn(|k| {
            let n = 1 << k;
            std::array::from_fn(|j| {
                let (i, u) = (j / 8, j % 8);
                if i >= n || u >= n {
                    return 0.0;
                }
                let c = if u == 0 { std::f64::consts::FRAC_1_SQRT_2 } else { 1.0 };
                (c / 2.0 * (((2 * i + 1) * u) as f64 * std::f64::consts::PI / (2 * n) as f64).cos()) as f32
            })
        });
        let mut qs = Vec::new();
        let mut qraw = Vec::new();
        let (mut dc, mut ac) = (Vec::new(), Vec::new());
        for c in &comps {
            let q = h.qt[c.tq].as_ref().ok_or(Corrupt("missing quantisation table"))?;
            qraw.push(std::array::from_fn(|s| f32::from(q[ZZ_OF[s] as usize])));
            qs.push(std::array::from_fn(|s| {
                let (u, v) = (s / 8, s % 8);
                (f64::from(q[ZZ_OF[s] as usize]) * AAN[u] * AAN[v] / 8.0) as f32
            }));
            dc.push(h.dc[c.dc].as_deref().cloned().ok_or(Corrupt("missing Huffman table"))?);
            ac.push(h.ac[c.ac].as_deref().cloned().ok_or(Corrupt("missing Huffman table"))?);
        }
        let slots = comps.iter().enumerate().flat_map(|(c, k)| (0..k.v).flat_map(move |by| (0..k.h).map(move |bx| (c, bx, by)))).collect();
        Ok(Frame {
            width: h.width.div_ceil(denom),
            height: h.height.div_ceil(denom),
            block_size,
            upsample,
            qraw,
            basis,
            mcux,
            mcuy,
            comps,
            qs,
            dc,
            ac,
            restart: h.restart,
            slots,
        })
    }
}

struct Plane {
    data: Vec<u8>,
    stride: usize,
}

/// The scan's entropy-coded bytes with stuffing removed.
struct Entropy {
    /// Followed by `PAD` zero bytes so the bit reader can load 8 bytes anywhere up to `end`.
    data: Vec<u8>,
    end: usize,
    /// Where each restart interval after the first begins.
    restarts: Vec<usize>,
    /// The marker that ended the scan, if the data did not simply run out.
    next_marker: Option<u8>,
}

const PAD: usize = 8;

fn destuff(scan: &[u8]) -> Entropy {
    let mut data = Vec::with_capacity(scan.len() + PAD);
    let mut restarts = Vec::new();
    let mut next_marker = None;
    let mut i = 0;
    while i < scan.len() {
        // Runs without 0xFF are copied whole; they are nearly everything.
        let run = scan[i..].iter().position(|&b| b == 0xFF).unwrap_or(scan.len() - i);
        data.extend_from_slice(&scan[i..i + run]);
        i += run;
        if i >= scan.len() {
            break;
        }
        let mut k = i + 1;
        while scan.get(k) == Some(&0xFF) {
            k += 1;
        }
        match scan.get(k) {
            None => break,
            Some(0x00) => data.push(0xFF),
            Some(0xD0..=0xD7) => restarts.push(data.len()),
            Some(&m) => {
                next_marker = Some(m);
                break;
            }
        }
        i = k + 1;
    }
    let end = data.len();
    data.resize(end + PAD, 0);
    Entropy { data, end, restarts, next_marker }
}

/// MSB-first reader. Bits past `cnt` in `buf` are already the next input, so refills OR in
/// the same bytes again rather than tracking a partial one.
struct Bits<'a> {
    data: &'a [u8],
    end: usize,
    pos: usize,
    buf: u64,
    cnt: u32,
    /// Sticky: set on an invalid code or index, checked once per MCU.
    bad: bool,
}

impl<'a> Bits<'a> {
    fn new(e: &'a Entropy) -> Self {
        Bits { data: &e.data, end: e.end, pos: 0, buf: 0, cnt: 0, bad: false }
    }

    #[inline(always)]
    fn refill(&mut self) {
        // Past the end it reads the zero padding, as a truncated stream decodes in libjpeg.
        let p = self.pos.min(self.end);
        // SAFETY: `data` holds PAD bytes past `end` and `p <= end`.
        let w = u64::from_be_bytes(unsafe { (self.data.as_ptr().add(p) as *const [u8; 8]).read_unaligned() });
        self.buf |= w >> self.cnt;
        self.pos += ((63 - self.cnt) >> 3) as usize;
        self.cnt |= 56;
    }

    #[inline(always)]
    fn peek(&self, n: u32) -> u32 {
        (self.buf >> (64 - n)) as u32
    }

    #[inline(always)]
    fn consume(&mut self, n: u32) {
        self.buf <<= n;
        self.cnt -= n;
    }

    /// Value bits of magnitude category `s` (1..=15), sign-extended.
    #[inline(always)]
    fn extend(&mut self, s: u32) -> i32 {
        let v = self.peek(s) as i32;
        self.consume(s);
        extend(v, s)
    }

    fn consumed_bits(&self) -> usize {
        self.pos * 8 - self.cnt as usize
    }

    fn jump(&mut self, at: usize) {
        (self.pos, self.buf, self.cnt) = (at, 0, 0);
    }

    fn seek(&mut self, bit: usize) {
        self.jump(bit / 8);
        self.refill();
        self.consume((bit % 8) as u32);
    }
}

/// JPEG's sign convention for `s` value bits: a clear top bit means negative. `s` 0 gives 0.
#[inline(always)]
fn extend(v: i32, s: u32) -> i32 {
    let neg = i32::from(s != 0) & ((v >> s.saturating_sub(1)) ^ 1);
    v - (neg << s) + neg
}

const LUT_BITS: u32 = 10;
const LUT_MASK: usize = (1 << LUT_BITS) - 1;

#[derive(Clone)]
struct Huff {
    /// `len << 8 | symbol` for codes of at most LUT_BITS; 0 where the code is longer.
    lut: [u16; 1 << LUT_BITS],
    maxcode: [i32; 17],
    valoff: [i32; 17],
    values: [u8; 256],
}

impl Huff {
    fn new(counts: &[u8], values: &[u8]) -> Result<Box<Self>, DecodeError> {
        let mut t = Box::new(Huff { lut: [0; 1 << LUT_BITS], maxcode: [-1; 17], valoff: [0; 17], values: [0; 256] });
        t.values[..values.len()].copy_from_slice(values);
        let (mut code, mut k) = (0i32, 0usize);
        for len in 1..=16u32 {
            let n = usize::from(counts[len as usize - 1]);
            t.valoff[len as usize] = k as i32 - code;
            for _ in 0..n {
                if code >= 1 << len {
                    return Err(DecodeError::Corrupt("Huffman table overflows"));
                }
                if len <= LUT_BITS {
                    let shift = LUT_BITS - len;
                    let first = (code as usize) << shift;
                    t.lut[first..first + (1 << shift)].fill((len as u16) << 8 | u16::from(values[k]));
                }
                code += 1;
                k += 1;
            }
            if n > 0 {
                t.maxcode[len as usize] = code - 1;
            }
            code <<= 1;
        }
        Ok(t)
    }

    /// Decode a symbol. An invalid code sets `b.bad` and reads as 0 (DC 0 / EOB), which ends
    /// the block.
    #[inline(always)]
    fn decode(&self, b: &mut Bits) -> u8 {
        let e = self.lut[b.peek(LUT_BITS) as usize & LUT_MASK];
        if e != 0 {
            b.consume(u32::from(e >> 8));
            return e as u8;
        }
        self.decode_long(b)
    }

    #[cold]
    fn decode_long(&self, b: &mut Bits) -> u8 {
        let p = b.peek(16) as i32;
        for len in LUT_BITS + 1..=16 {
            let c = p >> (16 - len);
            if c <= self.maxcode[len as usize] {
                b.consume(len);
                return self.values[((c + self.valoff[len as usize]) & 255) as usize];
            }
        }
        b.bad = true;
        0
    }
}

/// Decode one block into `blk` (kernel order, zeroed by the caller). Returns whether any AC
/// coefficient was present; damage sets `b.bad`.
#[inline(always)]
fn decode_block(b: &mut Bits, dc: &Huff, ac: &Huff, pred: &mut i32, blk: &mut [i16; 64]) -> bool {
    if b.cnt < 32 {
        b.refill();
    }
    let s = u32::from(dc.decode(b));
    if s > 15 {
        b.bad = true;
        return false;
    }
    if s > 0 {
        *pred = pred.wrapping_add(b.extend(s));
    }
    blk[0] = *pred as i16;
    let mut k = 1;
    let mut any = false;
    while k < 64 {
        if b.cnt < 32 {
            b.refill();
        }
        let e = ac.lut[b.peek(LUT_BITS) as usize & LUT_MASK];
        let (rs, value) = if e != 0 {
            // Code and value bits leave the buffer in one shift; the value is read beside it,
            // off the dependency chain that feeds the next lookup.
            let (len, rs) = (u32::from(e >> 8), e as u8);
            let size = u32::from(rs & 15);
            let v = if size == 0 { 0 } else { ((b.buf << len) >> (64 - size)) as i32 };
            b.consume(len + size);
            (rs, extend(v, size))
        } else {
            let rs = ac.decode_long(b);
            let size = u32::from(rs & 15);
            (rs, if size == 0 { 0 } else { b.extend(size) })
        };
        let (run, size) = (usize::from(rs >> 4), rs & 15);
        if size == 0 {
            if run != 15 {
                break;
            }
            k += 16;
            continue;
        }
        k += run;
        if k > 63 {
            b.bad = true;
            break;
        }
        blk[S_OF[k] as usize & 63] = value as i16;
        k += 1;
        any = true;
    }
    any
}

/// A block boundary in the scan: where its bits start, its slot in the MCU, its index among
/// all blocks and the DC predictors in force there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Cursor {
    bit: usize,
    phase: usize,
    block: usize,
    pred: [i32; 3],
}

/// Below this much entropy data one thread decodes the scan alone.
const PARALLEL_MIN_BYTES: usize = 64 * 1024;

/// Decode the whole scan into the planes. Restart intervals give exact starting points to
/// split on; without them, segments come from [`sync_segments`].
fn scan(f: &Frame, e: &Entropy, planes: &mut [Plane], backend: Backend, chunks: Option<usize>) -> Result<(), DecodeError> {
    let bpm = f.slots.len();
    let total = f.mcux * f.mcuy * bpm;
    let start = Cursor { bit: 0, phase: 0, block: 0, pred: [0; 3] };
    let n = chunks.unwrap_or_else(|| if e.end < PARALLEL_MIN_BYTES { 1 } else { rayon::current_num_threads() });
    let segs = if n <= 1 {
        vec![start]
    } else if f.restart > 0 {
        let intervals = (f.mcux * f.mcuy).div_ceil(f.restart);
        let per = intervals.div_ceil(n);
        let mut segs = Vec::new();
        for i in (0..intervals).step_by(per) {
            let bit = if i == 0 { 0 } else { 8 * *e.restarts.get(i - 1).ok_or(DecodeError::Corrupt("missing restart marker"))? };
            segs.push(Cursor { bit, phase: 0, block: i * f.restart * bpm, pred: [0; 3] });
        }
        segs
    } else {
        sync_segments(f, e, n, backend)?
    };
    let targets = PlanePtrs(planes.iter_mut().map(|p| (p.data.as_mut_ptr(), p.stride)).collect());
    let run = |k: usize| -> Result<Cursor, DecodeError> {
        let end = segs.get(k + 1).map_or(total, |c| c.block);
        // SAFETY: segments cover disjoint blocks, so their plane writes never overlap; each
        // backend runs only where `Backend::detect` found its features.
        unsafe {
            match backend {
                #[cfg(target_arch = "x86_64")]
                Backend::Avx2 => range_avx2(f, e, segs[k], end, &targets),
                #[cfg(target_arch = "aarch64")]
                Backend::Neon => range_neon(f, e, segs[k], end, &targets),
                Backend::Scalar => decode_range::<Scalar>(f, e, segs[k], end, &targets),
            }
        }
    };
    let ends: Vec<Cursor> = if segs.len() == 1 {
        vec![run(0)?]
    } else {
        (0..segs.len()).into_par_iter().map(run).collect::<Result<_, _>>()?
    };
    // Each segment must stop exactly where the next was resolved to begin.
    if f.restart == 0 && ends.iter().zip(&segs[1..]).any(|(a, b)| a != b) {
        return Err(DecodeError::Corrupt("segments disagree"));
    }
    if ends.last().is_some_and(|c| c.bit > e.end * 8) {
        return Err(DecodeError::Corrupt("truncated scan"));
    }
    Ok(())
}

struct PlanePtrs(Vec<(*mut u8, usize)>);
// SAFETY: shared only by segments that write disjoint blocks.
unsafe impl Sync for PlanePtrs {}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,bmi1,bmi2,lzcnt")]
unsafe fn range_avx2(f: &Frame, e: &Entropy, from: Cursor, end: usize, out: &PlanePtrs) -> Result<Cursor, DecodeError> {
    decode_range::<Avx2>(f, e, from, end, out)
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn range_neon(f: &Frame, e: &Entropy, from: Cursor, end: usize, out: &PlanePtrs) -> Result<Cursor, DecodeError> {
    decode_range::<Neon>(f, e, from, end, out)
}

/// Decode blocks `from.block..end` into the planes; returns the cursor after the last.
#[inline(always)]
unsafe fn decode_range<S: Simd>(f: &Frame, e: &Entropy, from: Cursor, end: usize, out: &PlanePtrs) -> Result<Cursor, DecodeError> {
    let bpm = f.slots.len();
    let mut b = Bits::new(e);
    b.seek(from.bit);
    let mut pred = from.pred;
    let (mut t, mut phase) = (from.block, from.phase);
    let mcu = t / bpm;
    let (mut mx, mut my) = (mcu % f.mcux, mcu / f.mcux);
    let mut left = if f.restart > 0 { f.restart - mcu % f.restart } else { 0 };
    while t < end {
        if phase == 0 && f.restart > 0 {
            if left == 0 {
                let interval = (my * f.mcux + mx) / f.restart;
                let at = *e.restarts.get(interval - 1).ok_or(DecodeError::Corrupt("missing restart marker"))?;
                if b.consumed_bits() > at * 8 {
                    return Err(DecodeError::Corrupt("restart interval overrun"));
                }
                b.jump(at);
                pred = [0; 3];
                left = f.restart;
            }
            left -= 1;
        }
        let (c, bx, by) = f.slots[phase];
        let comp = &f.comps[c];
        let mut blk = [0i16; 64];
        let any = decode_block(&mut b, &f.dc[c], &f.ac[c], &mut pred[c], &mut blk);
        let (base, stride) = out.0[c];
        let (bw, bh) = f.block_size[c];
        let dst = base.add((my * comp.v + by) * bh * stride + (mx * comp.h + bx) * bw);
        if !any {
            dc_block(blk[0], f.qs[c][0], dst, stride, bw, bh);
        } else if (bw, bh) == (8, 8) {
            idct_block::<S>(&blk, &f.qs[c], dst, stride);
        } else if bw == bh {
            match bw {
                4 => idct_square::<4>(&blk, &f.qraw[c], dst, stride),
                2 => idct_square::<2>(&blk, &f.qraw[c], dst, stride),
                _ => idct_square::<1>(&blk, &f.qraw[c], dst, stride),
            }
        } else {
            idct_scaled(&blk, &f.qraw[c], &f.basis, bw, bh, dst, stride);
        }
        t += 1;
        phase += 1;
        if phase == bpm {
            phase = 0;
            mx += 1;
            if mx == f.mcux {
                (mx, my) = (0, my + 1);
            }
            if b.bad {
                return Err(DecodeError::Corrupt("invalid entropy data"));
            }
        }
    }
    if b.bad {
        return Err(DecodeError::Corrupt("invalid entropy data"));
    }
    Ok(Cursor { bit: b.consumed_bits(), phase, block: t, pred })
}

/// Parse one block without keeping it; its DC difference, or None where the bits do not form
/// a block.
#[inline(always)]
fn skim_block(b: &mut Bits, dc: &Huff, ac: &Huff) -> Option<i32> {
    if b.cnt < 32 {
        b.refill();
    }
    let s = u32::from(dc.decode(b));
    if s > 15 {
        return None;
    }
    let diff = if s > 0 { b.extend(s) } else { 0 };
    let mut k = 1;
    while k < 64 {
        if b.cnt < 32 {
            b.refill();
        }
        let e = ac.lut[b.peek(LUT_BITS) as usize & LUT_MASK];
        let rs = if e != 0 {
            let (len, rs) = (u32::from(e >> 8), e as u8);
            b.consume(len + u32::from(rs & 15));
            rs
        } else {
            let rs = ac.decode_long(b);
            if rs & 15 != 0 {
                b.consume(u32::from(rs & 15));
            }
            rs
        };
        let (run, size) = (usize::from(rs >> 4), rs & 15);
        if size == 0 {
            if run != 15 {
                break;
            }
            k += 16;
            continue;
        }
        k += run + 1;
        if k > 64 {
            return None;
        }
    }
    (!b.bad).then_some(diff)
}

/// A block start seen by a speculative pass.
#[derive(Clone, Copy)]
struct Mark {
    bit: u32,
    dc: i16,
    phase: u8,
    /// The pass lost the block structure just before this mark and restarted here.
    restarted: bool,
}

struct Skim {
    marks: Vec<Mark>,
    /// The first block start at or past the chunk's end.
    exit: (usize, usize),
    exit_restarted: bool,
}

/// Parse blocks from `from` until one starts at or past `stop`. With `exact` the start is
/// known to be a block boundary and any damage is an error; otherwise the start is a guess,
/// and where the bits stop forming blocks the pass restarts one bit later.
fn skim(f: &Frame, e: &Entropy, from: usize, stop: usize, exact: bool) -> Result<Skim, DecodeError> {
    let bpm = f.slots.len();
    let mut b = Bits::new(e);
    b.seek(from);
    let mut marks = Vec::with_capacity((stop - from) / 16);
    let (mut phase, mut restarted) = (0, false);
    loop {
        let bit = b.consumed_bits();
        if bit >= stop {
            return Ok(Skim { marks, exit: (bit, phase), exit_restarted: restarted });
        }
        let c = f.slots[phase].0;
        match skim_block(&mut b, &f.dc[c], &f.ac[c]) {
            Some(dc) => {
                marks.push(Mark { bit: bit as u32, dc: dc as i16, phase: phase as u8, restarted });
                restarted = false;
                phase = (phase + 1) % bpm;
            }
            None if exact => return Err(DecodeError::Corrupt("invalid entropy data")),
            None => {
                b.bad = false;
                b.seek(bit + 1);
                (phase, restarted) = (0, true);
            }
        }
    }
}

/// Split a scan without restart markers into segments that decode in parallel.
///
/// A Huffman decoder started at an arbitrary bit soon falls into step with the true one: once
/// both reach the same block boundary with the same MCU slot, they decode identically from
/// there. Each chunk is parsed speculatively in parallel, then, walking forward from the
/// start, the true boundary leaving one chunk is matched against the next chunk's marks
/// (parsing a few blocks on where they have not met yet). Each match is an exact segment start.
fn sync_segments(f: &Frame, e: &Entropy, n: usize, backend: Backend) -> Result<Vec<Cursor>, DecodeError> {
    let bpm = f.slots.len();
    let total = f.mcux * f.mcuy * bpm;
    let end = e.end * 8;
    let bounds: Vec<usize> = (0..=n).map(|j| j * end / n).collect();
    let skim_chunk = |j: usize| {
        // SAFETY: the x86 build only runs this where `Backend::detect` found BMI2.
        #[cfg(target_arch = "x86_64")]
        if backend == Backend::Avx2 {
            return unsafe { skim_bmi2(f, e, bounds[j], bounds[j + 1], j == 0) };
        }
        let _ = backend;
        skim(f, e, bounds[j], bounds[j + 1], j == 0)
    };
    let skims: Vec<Skim> = (0..n).into_par_iter().map(skim_chunk).collect::<Result<_, _>>()?;

    let mut segs = vec![Cursor { bit: 0, phase: 0, block: 0, pred: [0; 3] }];
    let (mut t, mut pred) = (0, [0i32; 3]);
    let take = |marks: &[Mark], t: &mut usize, pred: &mut [i32; 3]| -> Result<(), DecodeError> {
        for (i, m) in marks.iter().enumerate() {
            if *t == total {
                break;
            }
            if i > 0 && m.restarted {
                return Err(DecodeError::Corrupt("invalid entropy data"));
            }
            let c = f.slots[usize::from(m.phase)].0;
            pred[c] = pred[c].wrapping_add(i32::from(m.dc));
            *t += 1;
        }
        Ok(())
    };
    take(&skims[0].marks, &mut t, &mut pred)?;
    let mut state = skims[0].exit;
    let mut overflow: Option<Bits> = None;
    for sk in &skims[1..] {
        let mut i = 0;
        while t < total {
            while sk.marks.get(i).is_some_and(|m| (m.bit as usize) < state.0) {
                i += 1;
            }
            if sk.marks.get(i).is_some_and(|m| m.bit as usize == state.0 && usize::from(m.phase) == state.1) {
                segs.push(Cursor { bit: state.0, phase: state.1, block: t, pred });
                take(&sk.marks[i..], &mut t, &mut pred)?;
                if t < total && sk.exit_restarted {
                    return Err(DecodeError::Corrupt("invalid entropy data"));
                }
                state = sk.exit;
                overflow = None;
                break;
            }
            if state.0 >= sk.exit.0 {
                break;
            }
            // Not in step yet: parse one true block ourselves.
            let b = overflow.get_or_insert_with(|| {
                let mut b = Bits::new(e);
                b.seek(state.0);
                b
            });
            let c = f.slots[state.1].0;
            let dc = skim_block(b, &f.dc[c], &f.ac[c]).ok_or(DecodeError::Corrupt("invalid entropy data"))?;
            pred[c] = pred[c].wrapping_add(dc);
            t += 1;
            state = (b.consumed_bits(), (state.1 + 1) % bpm);
        }
    }
    if t < total {
        return Err(DecodeError::Corrupt("truncated scan"));
    }
    Ok(segs)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "bmi1,bmi2,lzcnt")]
unsafe fn skim_bmi2(f: &Frame, e: &Entropy, from: usize, stop: usize, exact: bool) -> Result<Skim, DecodeError> {
    skim(f, e, from, stop, exact)
}

/// Write the block's pixels. `out` must have 8 rows of 8 bytes at `stride`.
#[inline(always)]
unsafe fn idct_block<S: Simd>(blk: &[i16; 64], qs: &[f32; 64], out: *mut u8, stride: usize) {
    let mut r: [S::V; 8] = std::array::from_fn(|u| S::mul(S::load_i16(blk.as_ptr().add(u * 8)), S::load_f32(qs.as_ptr().add(u * 8))));
    idct8::<S>(&mut r);
    S::transpose(&mut r);
    idct8::<S>(&mut r);
    let bias = S::splat(128.0);
    for (y, &row) in r.iter().enumerate() {
        S::store_u8(S::add(row, bias), out.add(y * stride));
    }
}

/// A DC-only block: the IDCT passes its input through unchanged, so this is bit-identical.
#[inline(always)]
unsafe fn dc_block(dc: i16, q0: f32, out: *mut u8, stride: usize, w: usize, h: usize) {
    let v = ((f32::from(dc) * q0 + 128.0).round_ties_even() as i32).clamp(0, 255) as u8;
    for y in 0..h {
        std::ptr::write_bytes(out.add(y * stride), v, w);
    }
}

/// `N`-point IDCT (N = 1, 2 or 4) of the lowest N frequencies, JPEG-normalised:
/// `x_i = sum C(u)/2 · F(u) · cos((2i+1)uπ/2N)`.
#[inline(always)]
fn idct_n<const N: usize>(f: [f32; N]) -> [f32; N] {
    // 1/(2√2), cos(π/8)/2, cos(3π/8)/2
    const A: f32 = 0.35355339;
    const B: f32 = 0.46193977;
    const C: f32 = 0.19134172;
    let mut x = [0f32; N];
    match N {
        4 => {
            let (e0, e1) = ((f[0] + f[2]) * A, (f[0] - f[2]) * A);
            let (o0, o1) = (f[1] * B + f[3] * C, f[1] * C - f[3] * B);
            x.copy_from_slice(&[e0 + o0, e1 + o1, e1 - o1, e0 - o0]);
        }
        2 => {
            let (a, b) = (f[0] * A, f[1] * A);
            x.copy_from_slice(&[a + b, a - b]);
        }
        _ => x[0] = f[0] * A,
    }
    x
}

/// Square scaled IDCT, separable, through [`idct_n`].
#[inline(always)]
unsafe fn idct_square<const N: usize>(blk: &[i16; 64], q: &[f32; 64], out: *mut u8, stride: usize) {
    let rows: [[f32; N]; N] = std::array::from_fn(|v| idct_n::<N>(std::array::from_fn(|u| f32::from(blk[u * 8 + v]) * q[u * 8 + v])));
    (0..N).for_each(|x| {
        let col = idct_n::<N>(std::array::from_fn(|v| rows[v][x]));
        for (y, &p) in col.iter().enumerate() {
            *out.add(y * stride + x) = ((p + 128.0).round_ties_even() as i32).clamp(0, 255) as u8;
        }
    });
}

/// IDCT evaluated at `w` x `h` sample points from the block's lowest `w` x `h` frequencies:
/// the 8-point reconstruction low-passed and resampled, as libjpeg scales.
#[inline(always)]
unsafe fn idct_scaled(blk: &[i16; 64], q: &[f32; 64], basis: &[[f32; 64]; 4], w: usize, h: usize, out: *mut u8, stride: usize) {
    let (bx, by) = (&basis[w.trailing_zeros() as usize], &basis[h.trailing_zeros() as usize]);
    let mut rows = [0f32; 64];
    for v in 0..h {
        for x in 0..w {
            rows[v * 8 + x] = (0..w).map(|u| f32::from(blk[u * 8 + v]) * q[u * 8 + v] * bx[x * 8 + u]).sum();
        }
    }
    for y in 0..h {
        for x in 0..w {
            let v: f32 = (0..h).map(|v| rows[v * 8 + x] * by[y * 8 + v]).sum();
            *out.add(y * stride + x) = ((v + 128.0).round_ties_even() as i32).clamp(0, 255) as u8;
        }
    }
}

/// AAN inverse DCT across the eight vectors; inputs carry the AAN scale, outputs are 8x.
#[inline(always)]
unsafe fn idct8<S: Simd>(d: &mut [S::V; 8]) {
    let (a, s) = (S::add, S::sub);
    let sqrt2 = S::splat(std::f32::consts::SQRT_2);
    let t10 = a(d[0], d[4]);
    let t11 = s(d[0], d[4]);
    let t13 = a(d[2], d[6]);
    let t12 = s(S::mul(s(d[2], d[6]), sqrt2), t13);
    let e0 = a(t10, t13);
    let e3 = s(t10, t13);
    let e1 = a(t11, t12);
    let e2 = s(t11, t12);

    let z13 = a(d[5], d[3]);
    let z10 = s(d[5], d[3]);
    let z11 = a(d[1], d[7]);
    let z12 = s(d[1], d[7]);
    let o7 = a(z11, z13);
    let o11 = S::mul(s(z11, z13), sqrt2);
    let z5 = S::mul(a(z10, z12), S::splat(1.847759));
    let o10 = s(z5, S::mul(z12, S::splat(1.0823922)));
    let o12 = s(z5, S::mul(z10, S::splat(2.613126)));
    let o6 = s(o12, o7);
    let o5 = s(o11, o6);
    let o4 = s(o10, o5);
    d[0] = a(e0, o7);
    d[7] = s(e0, o7);
    d[1] = a(e1, o6);
    d[6] = s(e1, o6);
    d[2] = a(e2, o5);
    d[5] = s(e2, o5);
    d[3] = a(e3, o4);
    d[4] = s(e3, o4);
}

/// Crop the planes to the image, upsampling chroma with libjpeg's "fancy" triangle filter
/// (edges replicated) and converting YCbCr to RGB, in row bands across the pool.
fn to_pixels(f: &Frame, planes: &[Plane], backend: Backend) -> Decoded {
    let (w, h) = (f.width, f.height);
    let channels = if planes.len() == 1 { 1 } else { 3 };
    let mut pixels = vec![0u8; w * h * channels];
    const BAND: usize = 32;
    pixels.par_chunks_mut(w * channels * BAND).enumerate().for_each(|(band, out)| {
        let y0 = band * BAND;
        if channels == 1 {
            for (r, row) in out.chunks_exact_mut(w).enumerate() {
                row.copy_from_slice(&planes[0].data[(y0 + r) * planes[0].stride..][..w]);
            }
            return;
        }
        // SAFETY: each backend runs only where `Backend::detect` found its features.
        unsafe {
            match backend {
                #[cfg(target_arch = "x86_64")]
                Backend::Avx2 => colour_avx2(f, planes, y0, out),
                #[cfg(target_arch = "aarch64")]
                Backend::Neon => colour_neon(f, planes, y0, out),
                Backend::Scalar => colour_generic::<Scalar>(f, planes, y0, out),
            }
        }
    });
    Decoded { width: w as u32, height: h as u32, channels: channels as u8, pixels }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn colour_avx2(f: &Frame, planes: &[Plane], y0: usize, out: &mut [u8]) {
    colour_generic::<Avx2>(f, planes, y0, out)
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn colour_neon(f: &Frame, planes: &[Plane], y0: usize, out: &mut [u8]) {
    colour_generic::<Neon>(f, planes, y0, out)
}

/// Rows `y0..` of `out` (whole RGB rows). Plane strides are multiples of 8 and cover the
/// image, so every 8-wide load below stays inside its row.
#[inline(always)]
unsafe fn colour_generic<S: Simd>(f: &Frame, planes: &[Plane], y0: usize, out: &mut [u8]) {
    let w = f.width;
    let (fx, fy) = f.upsample;
    let (cw, ch) = (w.div_ceil(fx), f.height.div_ceil(fy));
    let stride = planes[1].stride;
    let (near_w, far_w) = (S::splat(0.75), S::splat(0.25));
    let (c128, kr, kgb, kgr, kb) = (S::splat(128.0), S::splat(1.402), S::splat(0.344136), S::splat(0.714136), S::splat(1.772));
    // Vertically filtered chroma with one replicated sample either side, then full width.
    let mut cv = [vec![0f32; stride + 16], vec![0f32; stride + 16]];
    let mut full = [vec![0f32; 2 * stride + 16], vec![0f32; 2 * stride + 16]];
    let mut tail = [0u8; 24];
    for (r, row) in out.chunks_exact_mut(w * 3).enumerate() {
        let y = y0 + r;
        let (near, far) = if fy == 1 {
            (y, y)
        } else {
            let n = y / 2;
            (n, if y.is_multiple_of(2) { n.saturating_sub(1) } else { (n + 1).min(ch - 1) })
        };
        for c in 0..2 {
            let p = planes[c + 1].data.as_ptr();
            let (a, b) = (p.add(near * stride), p.add(far * stride));
            let v = cv[c].as_mut_ptr();
            for i in (0..cw).step_by(8) {
                let n = S::load_u8(a.add(i));
                let x = if fy == 1 { n } else { S::add(S::mul(n, near_w), S::mul(S::load_u8(b.add(i)), far_w)) };
                S::store_f32(x, v.add(1 + i));
            }
            *v = *v.add(1);
            *v.add(1 + cw) = *v.add(cw);
            if fx == 2 {
                let o = full[c].as_mut_ptr();
                for i in (0..cw).step_by(8) {
                    let m = S::mul(S::load_f32(v.add(1 + i)), near_w);
                    let even = S::add(m, S::mul(S::load_f32(v.add(i)), far_w));
                    let odd = S::add(m, S::mul(S::load_f32(v.add(2 + i)), far_w));
                    let (lo, hi) = S::zip(even, odd);
                    S::store_f32(lo, o.add(2 * i));
                    S::store_f32(hi, o.add(2 * i + 8));
                }
            }
        }
        let (cb, cr) = if fx == 2 { (full[0].as_ptr(), full[1].as_ptr()) } else { (cv[0].as_ptr().add(1), cv[1].as_ptr().add(1)) };
        let luma = planes[0].data.as_ptr().add(y * planes[0].stride);
        for x in (0..w).step_by(8) {
            let l = S::load_u8(luma.add(x));
            let b = S::sub(S::load_f32(cb.add(x)), c128);
            let rr = S::sub(S::load_f32(cr.add(x)), c128);
            let red = S::add(l, S::mul(rr, kr));
            let green = S::sub(S::sub(l, S::mul(b, kgb)), S::mul(rr, kgr));
            let blue = S::add(l, S::mul(b, kb));
            if x + 8 <= w {
                S::store_rgb(red, green, blue, row.as_mut_ptr().add(3 * x));
            } else {
                S::store_rgb(red, green, blue, tail.as_mut_ptr());
                row[3 * x..].copy_from_slice(&tail[..3 * (w - x)]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn photo(w: usize, h: usize, seed: u32) -> Vec<u8> {
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

    fn reference(jpeg: &[u8]) -> Vec<u8> {
        let img = image::load_from_memory_with_format(jpeg, image::ImageFormat::Jpeg).unwrap();
        if img.color().channel_count() == 1 { img.to_luma8().into_raw() } else { img.to_rgb8().into_raw() }
    }

    #[test]
    fn matches_the_reference_decoder() {
        for (w, h) in [(1, 1), (7, 9), (8, 8), (17, 33), (203, 61), (640, 480)] {
            let px = photo(w, h, (w * h) as u32);
            for q in [50, 85, 100] {
                let jpeg = super::super::encode_rgb(&px, w as u32, h as u32, q).unwrap();
                let ours = decode(&jpeg).unwrap();
                assert_eq!((ours.width, ours.height, ours.channels), (w as u32, h as u32, 3));
                let p = psnr(&ours.pixels, &reference(&jpeg));
                assert!(p > 40.0, "{w}x{h} q{q}: {p:.1} dB from the reference decoder");
            }
        }
    }

    #[test]
    fn every_backend_decodes_the_same_pixels() {
        let px = photo(203, 61, 3);
        let jpeg = super::super::encode_rgb(&px, 203, 61, 90).unwrap();
        let a = decode_with(&jpeg, Backend::Scalar, None, 0).unwrap().pixels;
        let b = decode_with(&jpeg, Backend::detect(), None, 0).unwrap().pixels;
        assert!(a == b);
    }

    #[test]
    fn parallel_segments_decode_exactly_like_one() {
        let (w, h) = (517u32, 389u32);
        let px = photo(w as usize, h as usize, 21);
        // The image crate writes no restart markers, so segments come from resynchronisation;
        // ours, split into strips, carries them.
        let mut plain = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut plain, 90).encode(&px, w, h, image::ExtendedColorType::Rgb8).unwrap();
        let grey: Vec<u8> = px.chunks(3).map(|p| p[1]).collect();
        let mut plain_grey = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut plain_grey, 90).encode(&grey, w, h, image::ExtendedColorType::L8).unwrap();
        let strips = super::super::encode::encode_with(&px, w, h, 90, Backend::detect(), Some(3)).unwrap();
        for jpeg in [&plain, &plain_grey, &strips] {
            for min_side in [0, 300] {
                let one = decode_with(jpeg, Backend::detect(), Some(1), min_side).unwrap().pixels;
                for n in [2, 3, 7, 16] {
                    let many = decode_with(jpeg, Backend::detect(), Some(n), min_side).unwrap().pixels;
                    assert!(one == many, "{n} segments differ from one");
                }
            }
        }
    }

    #[test]
    fn scaled_decodes_are_the_image_low_passed() {
        let (w, h) = (403usize, 301usize);
        let px = photo(w, h, 13);
        let mut sub = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut sub, 92).encode(&px, w as u32, h as u32, image::ExtendedColorType::Rgb8).unwrap();
        let full444 = super::super::encode_rgb(&px, w as u32, h as u32, 92).unwrap();
        for jpeg in [&sub, &full444] {
            let full = decode(jpeg).unwrap();
            for d in [2usize, 4, 8] {
                let small = decode_at_least(jpeg, w.div_ceil(d) as u32).unwrap();
                assert_eq!((small.width as usize, small.height as usize), (w.div_ceil(d), h.div_ceil(d)));
                // Reference: the full decode box-averaged, over whole d x d cells.
                let (sw, sh) = (w / d, h / d);
                let mut a = Vec::new();
                let mut b = Vec::new();
                for y in 0..sh {
                    for x in 0..sw {
                        for c in 0..3 {
                            let mut sum = 0u32;
                            for dy in 0..d {
                                for dx in 0..d {
                                    sum += u32::from(full.pixels[((y * d + dy) * w + x * d + dx) * 3 + c]);
                                }
                            }
                            a.push((sum as f32 / (d * d) as f32).round() as u8);
                            b.push(small.pixels[(y * small.width as usize + x) * 3 + c]);
                        }
                    }
                }
                let p = psnr(&a, &b);
                assert!(p > 30.0, "1/{d}: {p:.1} dB from the box-filtered full decode");
            }
        }
    }

    #[test]
    fn refuses_what_it_does_not_decode() {
        let px = photo(64, 48, 9);
        let mut progressive = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut progressive, 80)
            .encode(&px, 64, 48, image::ExtendedColorType::Rgb8)
            .unwrap();
        // Rewrite the frame marker in place: same bytes, declared progressive.
        let sof = progressive.windows(2).position(|m| m == [0xFF, 0xC0]).unwrap();
        progressive[sof + 1] = 0xC2;
        assert_eq!(decode(&progressive).err(), Some(DecodeError::Unsupported("progressive")));
        assert!(matches!(decode(b"not a jpeg"), Err(DecodeError::Corrupt(_))));
    }

    #[test]
    fn damaged_input_never_panics() {
        let px = photo(120, 90, 4);
        let jpeg = super::super::encode_rgb(&px, 120, 90, 85).unwrap();
        let mut s = 0x9e37_79b9u32;
        for round in 0..400 {
            let mut bad = jpeg.clone();
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            if round % 4 == 0 {
                bad.truncate(s as usize % jpeg.len());
            } else {
                for _ in 0..1 + round % 5 {
                    s ^= s << 13;
                    s ^= s >> 17;
                    s ^= s << 5;
                    let i = s as usize % bad.len();
                    bad[i] ^= (s >> 24) as u8 | 1;
                }
            }
            let _ = decode(&bad);
        }
    }
}
