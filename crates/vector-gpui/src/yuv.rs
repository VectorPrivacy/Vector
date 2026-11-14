//! Decoded YUV frames to BGRA at the size they draw: one pass that scales, rotates and converts.

use rayon::prelude::*;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Depth {
    /// One byte a sample.
    U8,
    /// Two bytes, little endian, the value in the low bits.
    Low(u8),
    /// Two bytes, little endian, the value in the high ten bits (P010).
    High,
}

#[derive(Clone, Copy)]
pub struct Plane<'a> {
    pub data: &'a [u8],
    pub stride: usize,
}

/// A frame as the decoder left it.
pub struct Yuv<'a> {
    pub w: usize,
    pub h: usize,
    pub y: Plane<'a>,
    /// Two planes, or one with U and V interleaved.
    pub u: Plane<'a>,
    pub v: Option<Plane<'a>>,
    pub shift: (u8, u8),
    pub depth: Depth,
    pub full_range: bool,
    pub bt709: bool,
}

#[derive(Clone, Copy)]
struct Tap {
    i0: u32,
    i1: u32,
    f: u32,
    near: u32,
}

/// Source taps for each output index along one axis, reversed when the rotation flips it.
fn taps(out: usize, src: usize, flip: bool) -> Vec<Tap> {
    let scale = src as f32 / out as f32;
    let max = src.saturating_sub(1) as f32;
    (0..out)
        .map(|o| {
            let o = if flip { out - 1 - o } else { o };
            let c = ((o as f32 + 0.5) * scale - 0.5).clamp(0., max);
            let i0 = c.floor();
            Tap {
                i0: i0 as u32,
                i1: (i0 + 1.).min(max) as u32,
                f: ((c - i0) * 256.) as u32,
                near: c.round() as u32,
            }
        })
        .collect()
}

#[inline(always)]
fn at(p: &Plane, depth: Depth, x: usize, y: usize) -> i32 {
    let row = &p.data[y * p.stride..];
    match depth {
        Depth::U8 => (row[x] as i32) << 2,
        Depth::Low(bits) => {
            let v = u16::from_le_bytes([row[x * 2], row[x * 2 + 1]]) as i32;
            if bits >= 10 { v >> (bits - 10) } else { v << (10 - bits) }
        }
        Depth::High => (u16::from_le_bytes([row[x * 2], row[x * 2 + 1]]) >> 6) as i32,
    }
}

/// `(y, crv, cgu, cgv, cbu)` at 2^12, for 10-bit samples.
fn matrix(full: bool, bt709: bool) -> [i32; 5] {
    match (full, bt709) {
        (false, false) => [4768, 6537, 1606, 3330, 8262],
        (false, true) => [4768, 7344, 872, 2183, 8651],
        (true, false) => [4096, 5743, 1409, 2925, 7258],
        (true, true) => [4096, 6450, 767, 1917, 7601],
    }
}

/// Converts into `out` (BGRA, `ow`×`oh`), turned `rot` degrees clockwise.
pub fn convert(f: &Yuv, ow: usize, oh: usize, rot: u16, out: &mut [u8]) {
    debug_assert_eq!(out.len(), ow * oh * 4);
    let swap = rot == 90 || rot == 270;
    // Output columns walk the source's x, or its y when the frame is turned a quarter.
    let (cols, rows) = if swap {
        (taps(ow, f.h, rot == 90), taps(oh, f.w, rot == 270))
    } else {
        (taps(ow, f.w, rot == 180), taps(oh, f.h, rot == 180))
    };
    let [ky, crv, cgu, cgv, cbu] = matrix(f.full_range, f.bt709);
    let y_off = if f.full_range { 0 } else { 64 };
    let (sx, sy) = (f.shift.0 as u32, f.shift.1 as u32);
    out.par_chunks_mut(ow * 4).enumerate().for_each(|(oy, line)| {
        let r = rows[oy];
        for (ox, px) in line.chunks_exact_mut(4).enumerate() {
            let c = cols[ox];
            let (xa, xb, xf, xn, ya, yb, yf, yn) = if swap {
                (r.i0, r.i1, r.f, r.near, c.i0, c.i1, c.f, c.near)
            } else {
                (c.i0, c.i1, c.f, c.near, r.i0, r.i1, r.f, r.near)
            };
            let (xa, xb, ya, yb) = (xa as usize, xb as usize, ya as usize, yb as usize);
            let top = at(&f.y, f.depth, xa, ya) * (256 - xf as i32) + at(&f.y, f.depth, xb, ya) * xf as i32;
            let bot = at(&f.y, f.depth, xa, yb) * (256 - xf as i32) + at(&f.y, f.depth, xb, yb) * xf as i32;
            let luma = (top * (256 - yf as i32) + bot * yf as i32) >> 16;
            let (cx, cy) = ((xn >> sx) as usize, (yn >> sy) as usize);
            let (u, v) = match &f.v {
                Some(vp) => (at(&f.u, f.depth, cx, cy), at(vp, f.depth, cx, cy)),
                None => (at(&f.u, f.depth, cx * 2, cy), at(&f.u, f.depth, cx * 2 + 1, cy)),
            };
            let (u, v) = (u - 512, v - 512);
            let yv = (luma - y_off) * ky;
            let r = (yv + crv * v) >> 14;
            let g = (yv - cgu * u - cgv * v) >> 14;
            let b = (yv + cbu * u) >> 14;
            px[0] = b.clamp(0, 255) as u8;
            px[1] = g.clamp(0, 255) as u8;
            px[2] = r.clamp(0, 255) as u8;
            px[3] = 255;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(y: u8, u: u8, v: u8) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        (vec![y; 16], vec![u; 4], vec![v; 4])
    }

    #[test]
    fn limited_white_and_black() {
        for (y, want) in [(235u8, 255u8), (16, 0)] {
            let (yp, up, vp) = flat(y, 128, 128);
            let f = Yuv {
                w: 4,
                h: 4,
                y: Plane { data: &yp, stride: 4 },
                u: Plane { data: &up, stride: 2 },
                v: Some(Plane { data: &vp, stride: 2 }),
                shift: (1, 1),
                depth: Depth::U8,
                full_range: false,
                bt709: true,
            };
            let mut out = vec![0; 2 * 2 * 4];
            convert(&f, 2, 2, 0, &mut out);
            assert!(out.chunks(4).all(|p| p[..3].iter().all(|c| c.abs_diff(want) <= 1) && p[3] == 255));
        }
    }

    #[test]
    fn quarter_turn_moves_the_bottom_left_to_the_top_left() {
        // 2×2 source, bright bottom-left; turned clockwise it lands top-left.
        let yp = vec![16, 16, 235, 16];
        let (up, vp) = (vec![128], vec![128]);
        let f = Yuv {
            w: 2,
            h: 2,
            y: Plane { data: &yp, stride: 2 },
            u: Plane { data: &up, stride: 1 },
            v: Some(Plane { data: &vp, stride: 1 }),
            shift: (1, 1),
            depth: Depth::U8,
            full_range: false,
            bt709: false,
        };
        let mut out = vec![0; 16];
        convert(&f, 2, 2, 90, &mut out);
        assert!(out[0] > 250 && out[4] < 5 && out[8] < 5 && out[12] < 5);
        convert(&f, 2, 2, 270, &mut out);
        assert!(out[12] > 250 && out[0] < 5);
    }
}
