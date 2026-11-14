//! Planar 4:2:0 pictures and the conversions around them: what a camera hands over into I420 for
//! the encoder, and what the decoder gives back into the BGRA a `RenderImage` paints. BT.601
//! limited range both ways, the matrix WebCodecs peers assume when the stream names none.

use rayon::prelude::*;

/// Even width and height, planes packed without padding.
#[derive(Clone)]
pub struct I420 {
    pub w: usize,
    pub h: usize,
    pub data: Vec<u8>,
}

#[inline(always)]
fn luma(r: i32, g: i32, b: i32) -> u8 {
    (((66 * r + 129 * g + 25 * b + 128) >> 8) + 16) as u8
}

#[inline(always)]
fn chroma(r: i32, g: i32, b: i32) -> (u8, u8) {
    let u = ((-38 * r - 74 * g + 112 * b + 128) >> 8) + 128;
    let v = ((112 * r - 94 * g - 18 * b + 128) >> 8) + 128;
    (u.clamp(0, 255) as u8, v.clamp(0, 255) as u8)
}

impl I420 {
    pub fn new(w: usize, h: usize) -> Self {
        let (w, h) = (w & !1, h & !1);
        Self { w, h, data: vec![0; w * h * 3 / 2] }
    }

    pub fn planes(&self) -> (&[u8], &[u8], &[u8]) {
        let (ys, cs) = (self.w * self.h, (self.w / 2) * (self.h / 2));
        let (y, rest) = self.data.split_at(ys);
        let (u, v) = rest.split_at(cs);
        (y, u, &v[..cs])
    }

    fn planes_mut(&mut self) -> (&mut [u8], &mut [u8], &mut [u8]) {
        let (ys, cs) = (self.w * self.h, (self.w / 2) * (self.h / 2));
        let (y, rest) = self.data.split_at_mut(ys);
        let (u, v) = rest.split_at_mut(cs);
        (y, u, v)
    }

    /// Packed RGB, three bytes a pixel, `stride` bytes a row.
    pub fn from_rgb(src: &[u8], w: usize, h: usize, stride: usize, bpp: usize, bgr: bool) -> Option<Self> {
        let mut out = Self::new(w, h);
        if out.w == 0 || out.h == 0 || src.len() < stride * (h - 1) + w * bpp {
            return None;
        }
        let (ow, oh) = (out.w, out.h);
        let (ri, bi) = if bgr { (2, 0) } else { (0, 2) };
        let (y, u, v) = out.planes_mut();
        y.par_chunks_mut(ow * 2)
            .zip(u.par_chunks_mut(ow / 2))
            .zip(v.par_chunks_mut(ow / 2))
            .enumerate()
            .for_each(|(row, ((yy, uu), vv))| {
                let r0 = &src[row * 2 * stride..];
                let r1 = &src[(row * 2 + 1) * stride..];
                for x in 0..ow / 2 {
                    let mut acc = [0i32; 3];
                    for (dy, line) in [r0, r1].iter().enumerate() {
                        for dx in 0..2 {
                            let p = &line[(x * 2 + dx) * bpp..];
                            let (r, g, b) = (p[ri] as i32, p[1] as i32, p[bi] as i32);
                            yy[dy * ow + x * 2 + dx] = luma(r, g, b);
                            acc[0] += r;
                            acc[1] += g;
                            acc[2] += b;
                        }
                    }
                    let (cu, cv) = chroma(acc[0] >> 2, acc[1] >> 2, acc[2] >> 2);
                    uu[x] = cu;
                    vv[x] = cv;
                }
            });
        let _ = oh;
        Some(out)
    }

    /// YUYV 4:2:2, the webcam's own raw format.
    pub fn from_yuyv(src: &[u8], w: usize, h: usize) -> Option<Self> {
        let mut out = Self::new(w, h);
        let stride = w * 2;
        if out.w == 0 || out.h == 0 || src.len() < stride * h {
            return None;
        }
        let ow = out.w;
        let (y, u, v) = out.planes_mut();
        y.par_chunks_mut(ow * 2)
            .zip(u.par_chunks_mut(ow / 2))
            .zip(v.par_chunks_mut(ow / 2))
            .enumerate()
            .for_each(|(row, ((yy, uu), vv))| {
                let r0 = &src[row * 2 * stride..];
                let r1 = &src[(row * 2 + 1) * stride..];
                for x in 0..ow / 2 {
                    let (a, b) = (&r0[x * 4..x * 4 + 4], &r1[x * 4..x * 4 + 4]);
                    yy[x * 2] = a[0];
                    yy[x * 2 + 1] = a[2];
                    yy[ow + x * 2] = b[0];
                    yy[ow + x * 2 + 1] = b[2];
                    uu[x] = ((a[1] as u16 + b[1] as u16 + 1) >> 1) as u8;
                    vv[x] = ((a[3] as u16 + b[3] as u16 + 1) >> 1) as u8;
                }
            });
        Some(out)
    }

    /// NV12: the luma plane, then U and V interleaved.
    pub fn from_nv12(src: &[u8], w: usize, h: usize) -> Option<Self> {
        let mut out = Self::new(w, h);
        if out.w == 0 || out.h == 0 || src.len() < w * h + w * (h / 2) {
            return None;
        }
        let (ow, oh) = (out.w, out.h);
        let (y, u, v) = out.planes_mut();
        for row in 0..oh {
            y[row * ow..(row + 1) * ow].copy_from_slice(&src[row * w..row * w + ow]);
        }
        let uv = &src[w * h..];
        for row in 0..oh / 2 {
            let line = &uv[row * w..];
            for x in 0..ow / 2 {
                u[row * ow / 2 + x] = line[x * 2];
                v[row * ow / 2 + x] = line[x * 2 + 1];
            }
        }
        Some(out)
    }

    /// Bilinear, to any even size.
    pub fn scale(&self, w: usize, h: usize) -> Self {
        let mut out = Self::new(w, h);
        if out.w == self.w && out.h == self.h {
            out.data.copy_from_slice(&self.data);
            return out;
        }
        let (sy, su, sv) = self.planes();
        let (ow, oh) = (out.w, out.h);
        let (dy, du, dv) = out.planes_mut();
        scale_plane(sy, self.w, self.h, dy, ow, oh);
        scale_plane(su, self.w / 2, self.h / 2, du, ow / 2, oh / 2);
        scale_plane(sv, self.w / 2, self.h / 2, dv, ow / 2, oh / 2);
        out
    }
}

fn scale_plane(src: &[u8], sw: usize, sh: usize, dst: &mut [u8], dw: usize, dh: usize) {
    if sw == 0 || sh == 0 || dw == 0 || dh == 0 {
        return;
    }
    // A steep shrink averages a box first, so a 4K screen at 720p doesn't shimmer.
    let fx = ((sw << 16) / dw) as u32;
    let fy = ((sh << 16) / dh) as u32;
    let box_x = (sw / dw).max(1);
    let box_y = (sh / dh).max(1);
    dst.par_chunks_mut(dw).enumerate().for_each(|(y, row)| {
        let syf = ((y as u32 * fy) + (fy >> 1)).saturating_sub(1 << 15);
        let (y0, wy) = ((syf >> 16) as usize, (syf & 0xffff) >> 8);
        let y0 = y0.min(sh - 1);
        let y1 = (y0 + 1).min(sh - 1);
        for (x, out) in row.iter_mut().enumerate() {
            if box_x > 1 || box_y > 1 {
                let (bx, by) = (x * sw / dw, y * sh / dh);
                let mut acc = 0u32;
                let mut n = 0u32;
                for yy in by..(by + box_y).min(sh) {
                    let line = &src[yy * sw..];
                    for xx in bx..(bx + box_x).min(sw) {
                        acc += line[xx] as u32;
                        n += 1;
                    }
                }
                *out = (acc / n.max(1)) as u8;
                continue;
            }
            let sxf = ((x as u32 * fx) + (fx >> 1)).saturating_sub(1 << 15);
            let (x0, wx) = ((sxf >> 16) as usize, (sxf & 0xffff) >> 8);
            let x0 = x0.min(sw - 1);
            let x1 = (x0 + 1).min(sw - 1);
            let a = src[y0 * sw + x0] as u32 * (256 - wx) + src[y0 * sw + x1] as u32 * wx;
            let b = src[y1 * sw + x0] as u32 * (256 - wx) + src[y1 * sw + x1] as u32 * wx;
            *out = ((a * (256 - wy) + b * wy + 32768) >> 16) as u8;
        }
    });
}

/// A decoded picture's planes, as the decoder strides them, into straight BGRA.
pub fn to_bgra(y: &[u8], u: &[u8], v: &[u8], strides: (usize, usize), w: usize, h: usize) -> Vec<u8> {
    let mut out = vec![0u8; w * h * 4];
    let (ys, cs) = strides;
    out.par_chunks_mut(w * 4).enumerate().for_each(|(row, line)| {
        let yl = &y[row * ys..];
        let ul = &u[(row / 2) * cs..];
        let vl = &v[(row / 2) * cs..];
        for x in 0..w {
            let c = (yl[x] as i32 - 16) * 298;
            let d = ul[x / 2] as i32 - 128;
            let e = vl[x / 2] as i32 - 128;
            let px = &mut line[x * 4..x * 4 + 4];
            px[0] = ((c + 516 * d + 128) >> 8).clamp(0, 255) as u8;
            px[1] = ((c - 100 * d - 208 * e + 128) >> 8).clamp(0, 255) as u8;
            px[2] = ((c + 409 * e + 128) >> 8).clamp(0, 255) as u8;
            px[3] = 255;
        }
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_colour_survives_the_round_trip() {
        let (w, h) = (8, 4);
        let rgb: Vec<u8> = (0..w * h).flat_map(|_| [200u8, 40, 90]).collect();
        let pic = I420::from_rgb(&rgb, w, h, w * 3, 3, false).unwrap();
        let (y, u, v) = pic.planes();
        let bgra = to_bgra(y, u, v, (w, w / 2), w, h);
        let (b, g, r) = (bgra[0] as i32, bgra[1] as i32, bgra[2] as i32);
        assert!((r - 200).abs() <= 3 && (g - 40).abs() <= 3 && (b - 90).abs() <= 3, "{r} {g} {b}");
    }

    #[test]
    fn scaling_keeps_a_flat_field_flat() {
        let mut pic = I420::new(64, 48);
        pic.data.iter_mut().for_each(|b| *b = 120);
        for (w, h) in [(32, 24), (16, 12), (100, 70)] {
            let s = pic.scale(w, h);
            assert!(s.data.iter().all(|&b| b == 120), "{w}x{h}");
        }
    }
}
