//! ThumbHash decode at a chosen size, straight to BGRA: the hash is a DCT, so a larger
//! evaluation is a smoother blur than stretching the stock 32 px one.

use std::f32::consts::PI;

struct Channel {
    dc: f32,
    nx: usize,
    ny: usize,
    ac: Vec<f32>,
}

impl Channel {
    /// The coefficients laid out per row of the image: `xc[cy * w + x]`.
    fn x_contrib(&self, fx: &[f32], w: usize) -> Vec<f32> {
        let mut xc = vec![0f32; self.ny * w];
        let mut j = 0;
        for cy in 0..self.ny {
            let mut cx = usize::from(cy == 0);
            while cx * self.ny < self.nx * (self.ny - cy) {
                let k = self.ac[j];
                for (o, f) in xc[cy * w..(cy + 1) * w].iter_mut().zip(&fx[cx * w..(cx + 1) * w]) {
                    *o += k * f;
                }
                j += 1;
                cx += 1;
            }
        }
        xc
    }
}

/// `(width, height, bgra)` with the longer side `side` px, or `None` for a malformed hash.
pub fn decode_bgra(hash: &[u8], side: usize) -> Option<(usize, usize, Vec<u8>)> {
    if hash.len() < 5 || side == 0 {
        return None;
    }
    let h24 = u32::from(hash[0]) | (u32::from(hash[1]) << 8) | (u32::from(hash[2]) << 16);
    let h16 = u16::from(hash[3]) | (u16::from(hash[4]) << 8);
    let l_dc = (h24 & 63) as f32 / 63.;
    let p_dc = ((h24 >> 6) & 63) as f32 / 31.5 - 1.;
    let q_dc = ((h24 >> 12) & 63) as f32 / 31.5 - 1.;
    let l_scale = ((h24 >> 18) & 31) as f32 / 31.;
    let alpha = (h24 >> 23) != 0;
    let p_scale = ((h16 >> 3) & 63) as f32 / 63.;
    let q_scale = ((h16 >> 9) & 63) as f32 / 63.;
    let landscape = (h16 >> 15) != 0;
    let l_max: u16 = if alpha { 5 } else { 7 };
    let lx = usize::from(3.max(if landscape { l_max } else { h16 & 7 }));
    let ly = usize::from(3.max(if landscape { h16 & 7 } else { l_max }));
    let (a_dc, a_scale) = match (alpha, hash.get(5)) {
        (false, _) => (1., 1.),
        (true, Some(b)) => (f32::from(b & 15) / 15., f32::from(b >> 4) / 15.),
        (true, None) => return None,
    };
    let data = &hash[if alpha { 6 } else { 5 }..];
    let mut nib = 0usize;
    let mut read = |nx: usize, ny: usize, scale: f32| -> Option<Vec<f32>> {
        let mut ac = Vec::with_capacity(nx * ny);
        for cy in 0..ny {
            let mut cx = usize::from(cy == 0);
            while cx * ny < nx * (ny - cy) {
                let b = *data.get(nib / 2)?;
                let v = if nib % 2 == 0 { b & 15 } else { b >> 4 };
                nib += 1;
                ac.push((f32::from(v) / 7.5 - 1.) * scale);
                cx += 1;
            }
        }
        Some(ac)
    };
    let l = Channel { dc: l_dc, nx: lx, ny: ly, ac: read(lx, ly, l_scale)? };
    let p = Channel { dc: p_dc, nx: 3, ny: 3, ac: read(3, 3, p_scale * 1.25)? };
    let q = Channel { dc: q_dc, nx: 3, ny: 3, ac: read(3, 3, q_scale * 1.25)? };
    let a = if alpha { Some(Channel { dc: a_dc, nx: 5, ny: 5, ac: read(5, 5, a_scale)? }) } else { None };

    let (ax, ay) = if landscape { (l_max, h16 & 7) } else { (h16 & 7, l_max) };
    let ratio = f32::from(ax) / f32::from(ay.max(1));
    let s = side as f32;
    let (w, h) = if ratio > 1. { (side, ((s / ratio).round() as usize).max(1)) } else { (((s * ratio).round() as usize).max(1), side) };

    let max_x = lx.max(if alpha { 5 } else { 3 });
    let max_y = ly.max(if alpha { 5 } else { 3 });
    let table = |n: usize, len: usize| -> Vec<f32> {
        (0..n).flat_map(|c| (0..len).map(move |i| (PI / len as f32 * c as f32 * (i as f32 + 0.5)).cos())).collect()
    };
    let fx = table(max_x, w);
    let fy = table(max_y, h);
    let chans: Vec<(&Channel, Vec<f32>)> = [Some(&l), Some(&p), Some(&q), a.as_ref()]
        .into_iter()
        .flatten()
        .map(|c| (c, c.x_contrib(&fx, w)))
        .collect();

    let mut out = vec![0u8; w * h * 4];
    let mut rows = vec![0f32; 4 * w];
    for y in 0..h {
        for (k, (c, xc)) in chans.iter().enumerate() {
            let row = &mut rows[k * w..(k + 1) * w];
            row.fill(c.dc);
            for cy in 0..c.ny {
                let f = fy[cy * h + y] * 2.;
                for (o, v) in row.iter_mut().zip(&xc[cy * w..(cy + 1) * w]) {
                    *o += v * f;
                }
            }
        }
        if a.is_none() {
            rows[3 * w..].fill(1.);
        }
        let px = &mut out[y * w * 4..(y + 1) * w * 4];
        for x in 0..w {
            let (l, p, q, a) = (rows[x], rows[w + x], rows[2 * w + x], rows[3 * w + x]);
            let b = l - 2. / 3. * p;
            let r = (3. * l - b + q) / 2.;
            let g = r - q;
            let unit = |v: f32| (v.clamp(0., 1.) * 255.) as u8;
            px[x * 4..x * 4 + 4].copy_from_slice(&[unit(b), unit(g), unit(r), unit(a)]);
        }
    }
    Some((w, h, out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hashes() -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        for (w, h, seed) in [(48usize, 27usize, 1u32), (23, 32, 7), (32, 32, 3), (64, 20, 11)] {
            let mut x = seed;
            let mut rgba = Vec::with_capacity(w * h * 4);
            for i in 0..w * h {
                x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let alpha = if seed == 3 && i % 5 == 0 { 40 } else { 255 };
                rgba.extend_from_slice(&[(x >> 24) as u8, (x >> 16) as u8, ((i * 3) % 256) as u8, alpha]);
            }
            out.push(fast_thumbhash::rgba_to_thumb_hash(w, h, &rgba));
        }
        out
    }

    #[test]
    fn matches_the_stock_decode_at_its_own_size() {
        for hash in hashes() {
            let (w, h, rgba) = fast_thumbhash::thumb_hash_to_rgba(&hash).unwrap();
            let (w2, h2, bgra) = decode_bgra(&hash, 32).unwrap();
            assert_eq!((w, h), (w2, h2));
            for (s, d) in rgba.chunks_exact(4).zip(bgra.chunks_exact(4)) {
                for (a, b) in [(s[0], d[2]), (s[1], d[1]), (s[2], d[0]), (s[3], d[3])] {
                    assert!(a.abs_diff(b) <= 1, "{a} vs {b}");
                }
            }
        }
    }

    #[test]
    fn a_larger_side_keeps_the_aspect() {
        for hash in hashes() {
            let (w, h, _) = decode_bgra(&hash, 32).unwrap();
            let (w2, h2, px) = decode_bgra(&hash, 64).unwrap();
            assert_eq!(w2.max(h2), 64);
            assert!((w2 as f32 / h2 as f32 - w as f32 / h as f32).abs() < 0.1);
            assert_eq!(px.len(), w2 * h2 * 4);
        }
    }

    #[test]
    fn malformed_hashes_refuse() {
        assert!(decode_bgra(&[], 64).is_none());
        assert!(decode_bgra(&[1, 2, 3, 4], 64).is_none());
        let mut cut = hashes()[0].clone();
        cut.truncate(7);
        assert!(decode_bgra(&cut, 64).is_none());
    }
}
