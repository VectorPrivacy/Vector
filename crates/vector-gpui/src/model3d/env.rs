//! The light a model is seen in: three.js's `RoomEnvironment` ray-traced into an equirect, then
//! prefiltered per roughness into a mip chain. Built once, on the engine thread.

use glam::{Quat, Vec2, Vec3};
use std::f32::consts::PI;

struct Boxed {
    c: Vec3,
    half: Vec3,
    rot: Quat,
    emit: f32,
    inside: bool,
}

const LIFT: f32 = -3.5;

fn scene() -> Vec<Boxed> {
    let b = |p: [f32; 3], ry: f32, s: [f32; 3], emit: f32| Boxed {
        c: Vec3::new(p[0], p[1] + LIFT, p[2]),
        half: Vec3::from(s) * 0.5,
        rot: Quat::from_rotation_y(ry),
        emit,
        inside: false,
    };
    let mut v = vec![Boxed { inside: true, ..b([-0.757, 13.219, 0.717], 0., [31.713, 28.305, 28.591], 0.) }];
    v.extend([
        b([-10.906, 2.009, 1.846], -0.195, [2.328, 7.905, 4.651], 0.),
        b([-5.607, -0.754, -0.758], 0.994, [1.970, 1.534, 3.955], 0.),
        b([6.167, 0.857, 7.803], 0.561, [3.927, 6.285, 3.687], 0.),
        b([-2.017, 0.018, 6.124], 0.333, [2.002, 4.566, 2.064], 0.),
        b([2.291, -0.756, -2.621], -0.286, [1.546, 1.552, 1.496], 0.),
        b([-2.193, -0.369, -5.547], 0.516, [3.875, 3.487, 2.986], 0.),
        b([-16.116, 14.37, 8.208], 0., [0.1, 2.428, 2.739], 50.),
        b([-16.109, 18.021, -8.207], 0., [0.1, 2.425, 2.751], 50.),
        b([14.904, 12.198, -1.832], 0., [0.15, 4.265, 6.331], 17.),
        b([-0.462, 8.89, 14.520], 0., [4.38, 5.441, 0.088], 43.),
        b([3.235, 11.486, -12.541], 0., [2.5, 2.0, 0.1], 20.),
        b([0.0, 20.0, 0.0], 0., [1.0, 0.1, 1.0], 100.),
    ]);
    v
}

fn hit(b: &Boxed, d: Vec3) -> Option<(f32, Vec3)> {
    let inv = b.rot.inverse();
    let o = inv * (Vec3::ZERO - b.c);
    let dl = inv * d;
    let (mut t0, mut t1) = (f32::NEG_INFINITY, f32::INFINITY);
    let (mut n0, mut n1) = (Vec3::ZERO, Vec3::ZERO);
    for a in 0..3 {
        if dl[a].abs() < 1e-9 {
            if o[a].abs() > b.half[a] {
                return None;
            }
            continue;
        }
        let mut lo = (-b.half[a] - o[a]) / dl[a];
        let mut hi = (b.half[a] - o[a]) / dl[a];
        let mut axis = Vec3::ZERO;
        axis[a] = 1.;
        let (mut nlo, mut nhi) = (-axis, axis);
        if lo > hi {
            std::mem::swap(&mut lo, &mut hi);
            std::mem::swap(&mut nlo, &mut nhi);
        }
        if lo > t0 {
            t0 = lo;
            n0 = nlo;
        }
        if hi < t1 {
            t1 = hi;
            n1 = nhi;
        }
    }
    if t0 > t1 {
        return None;
    }
    if b.inside {
        (t1 > 0.).then(|| (t1, b.rot * -n1))
    } else {
        (t0 > 0.).then(|| (t0, b.rot * n0))
    }
}

const LIGHT: Vec3 = Vec3::new(0.418, 16.199 + LIFT, 0.300);

fn radiance(boxes: &[Boxed], d: Vec3) -> f32 {
    let mut best: Option<(f32, Vec3, &Boxed)> = None;
    for b in boxes {
        if let Some((t, n)) = hit(b, d)
            && best.is_none_or(|(bt, _, _)| t < bt)
        {
            best = Some((t, n, b));
        }
    }
    let Some((t, n, b)) = best else { return 0. };
    if b.emit > 0. {
        return b.emit;
    }
    let p = d * t;
    let to = LIGHT - p;
    let dist = to.length();
    let l = to / dist;
    let nl = n.dot(l).max(0.);
    if nl <= 0. {
        return 0.;
    }
    let window = (1. - (dist / 28.).powi(4)).clamp(0., 1.).powi(2);
    let irradiance = 900. * window / (dist * dist).max(0.01) * nl;
    let v = -d;
    let nv = n.dot(v).max(1e-4);
    let h = (l + v).normalize();
    let f = 0.04 + 0.96 * (1. - v.dot(h).max(0.)).powi(5);
    let spec = f * (0.5 / (nl + nv)) / PI;
    irradiance * (1. / PI + spec)
}

fn dir(u: f32, v: f32) -> Vec3 {
    let phi = (u - 0.5) * 2. * PI;
    let theta = v * PI;
    Vec3::new(theta.sin() * phi.cos(), theta.cos(), theta.sin() * phi.sin())
}

fn uv(d: Vec3) -> Vec2 {
    Vec2::new(d.z.atan2(d.x) / (2. * PI) + 0.5, d.y.clamp(-1., 1.).acos() / PI)
}

struct Level {
    w: usize,
    h: usize,
    px: Vec<f32>,
}

impl Level {
    fn at(&self, u: f32, v: f32) -> f32 {
        let x = u * self.w as f32 - 0.5;
        let y = (v * self.h as f32 - 0.5).clamp(0., self.h as f32 - 1.);
        let (x0, y0) = (x.floor(), y.floor());
        let (fx, fy) = (x - x0, y - y0);
        let wrap = |x: i64| x.rem_euclid(self.w as i64) as usize;
        let (xa, xb) = (wrap(x0 as i64), wrap(x0 as i64 + 1));
        let (ya, yb) = (y0 as usize, (y0 as usize + 1).min(self.h - 1));
        let p = |x: usize, y: usize| self.px[y * self.w + x];
        (p(xa, ya) * (1. - fx) + p(xb, ya) * fx) * (1. - fy) + (p(xa, yb) * (1. - fx) + p(xb, yb) * fx) * fy
    }
    fn half(&self) -> Level {
        let (w, h) = ((self.w / 2).max(1), (self.h / 2).max(1));
        let mut px = vec![0.; w * h];
        for y in 0..h {
            for x in 0..w {
                let s = |dx: usize, dy: usize| self.px[(y * 2 + dy).min(self.h - 1) * self.w + (x * 2 + dx).min(self.w - 1)];
                px[y * w + x] = (s(0, 0) + s(1, 0) + s(0, 1) + s(1, 1)) * 0.25;
            }
        }
        Level { w, h, px }
    }
}

fn hammersley(i: u32, n: u32) -> Vec2 {
    Vec2::new(i as f32 / n as f32, i.reverse_bits() as f32 * 2.328_306_4e-10)
}

pub const LEVELS: usize = 6;
pub const WIDTH: usize = 256;

/// The prefiltered chain, level `k` for roughness `k / (LEVELS - 1)`, each as Rgb9e5 texels.
pub fn build() -> Vec<(usize, usize, Vec<u32>)> {
    let boxes = scene();
    let (sw, sh) = (WIDTH * 2, WIDTH);
    let mut src = Level { w: sw, h: sh, px: vec![0.; sw * sh] };
    for y in 0..sh {
        for x in 0..sw {
            src.px[y * sw + x] = radiance(&boxes, dir((x as f32 + 0.5) / sw as f32, (y as f32 + 0.5) / sh as f32));
        }
    }
    let mut chain = vec![src];
    while chain.last().is_some_and(|l| l.w > 4) {
        let next = chain.last().map(Level::half).unwrap_or(Level { w: 1, h: 1, px: vec![0.] });
        chain.push(next);
    }
    let texel = 4. * PI / (sw * sh) as f32;
    let mut out = Vec::with_capacity(LEVELS);
    for k in 0..LEVELS {
        let (w, h) = (WIDTH >> k, (WIDTH / 2) >> k);
        let rough = k as f32 / (LEVELS - 1) as f32;
        let a = (rough * rough).max(1e-4);
        let n_samples = if k == 0 { 1 } else { 96 };
        let mut px = Vec::with_capacity(w * h);
        for y in 0..h {
            for x in 0..w {
                let n = dir((x as f32 + 0.5) / w as f32, (y as f32 + 0.5) / h as f32);
                let value = if k == 0 {
                    let t = uv(n);
                    chain[1].at(t.x, t.y)
                } else {
                    let up = if n.y.abs() < 0.999 { Vec3::Y } else { Vec3::X };
                    let tx = up.cross(n).normalize();
                    let ty = n.cross(tx);
                    let (mut sum, mut weight) = (0., 0.);
                    for i in 0..n_samples {
                        let e = hammersley(i, n_samples);
                        let phi = 2. * PI * e.x;
                        let cos_t = ((1. - e.y) / (1. + (a * a - 1.) * e.y)).sqrt();
                        let sin_t = (1. - cos_t * cos_t).max(0.).sqrt();
                        let hv = tx * (sin_t * phi.cos()) + ty * (sin_t * phi.sin()) + n * cos_t;
                        let l = hv * (2. * n.dot(hv)) - n;
                        let nl = n.dot(l);
                        if nl <= 0. {
                            continue;
                        }
                        let nh = cos_t;
                        let d = a * a / (PI * (nh * nh * (a * a - 1.) + 1.).powi(2));
                        let pdf = d / 4.;
                        let omega = 1. / (n_samples as f32 * pdf + 1e-4);
                        let lod = (0.5 * (omega / texel).log2() + 1.).clamp(0., (chain.len() - 1) as f32);
                        let t = uv(l);
                        let (l0, f) = (lod.floor() as usize, lod.fract());
                        let s0 = chain[l0].at(t.x, t.y);
                        let s = if l0 + 1 < chain.len() { s0 * (1. - f) + chain[l0 + 1].at(t.x, t.y) * f } else { s0 };
                        sum += s * nl;
                        weight += nl;
                    }
                    if weight > 0. { sum / weight } else { 0. }
                };
                px.push(rgb9e5(value, value, value));
            }
        }
        out.push((w, h, px));
    }
    out
}

/// Shared-exponent HDR texel: three 9-bit mantissas and a 5-bit exponent.
fn rgb9e5(r: f32, g: f32, b: f32) -> u32 {
    const MAX: f32 = 65408.;
    let (r, g, b) = (r.clamp(0., MAX), g.clamp(0., MAX), b.clamp(0., MAX));
    let m = r.max(g).max(b);
    if m <= 0. {
        return 0;
    }
    let mut e = (m.log2().floor() as i32 + 1 + 15).max(0);
    let scale = |e: i32| 2f32.powi(e - 15 - 9);
    if (m / scale(e) + 0.5).floor() as i32 == 512 {
        e += 1;
    }
    let s = scale(e);
    let q = |v: f32| ((v / s + 0.5).floor() as u32).min(511);
    q(r) | (q(g) << 9) | (q(b) << 18) | ((e as u32 & 31) << 27)
}
