//! A clip sampled at a time, onto the rest pose, as three.js's mixer leaves it with one action.

use glam::{Quat, Vec4};

use super::scene::{Channel, Clip, Interp, Path, Trs};

fn key(times: &[f32], t: f32) -> (usize, usize, f32) {
    let n = times.len();
    if n == 1 || t <= times[0] {
        return (0, 0, 0.);
    }
    if t >= times[n - 1] {
        return (n - 1, n - 1, 0.);
    }
    let i = times.partition_point(|&k| k <= t).saturating_sub(1).min(n - 2);
    let span = times[i + 1] - times[i];
    let f = if span > 0. { (t - times[i]) / span } else { 0. };
    (i, i + 1, f)
}

fn sample(c: &Channel, t: f32, width: usize) -> Option<Vec4> {
    if c.times.is_empty() {
        return None;
    }
    let stride = if c.interp == Interp::Cubic { width * 3 } else { width };
    let at = |k: usize, o: usize| -> Vec4 {
        let b = k * stride + o;
        let mut v = [0f32; 4];
        for (j, slot) in v.iter_mut().enumerate().take(width) {
            *slot = c.values.get(b + j).copied().unwrap_or(0.);
        }
        Vec4::from(v)
    };
    let (a, b, f) = key(&c.times, t);
    let out = match c.interp {
        Interp::Step => at(a, 0),
        Interp::Linear if c.path == Path::R => {
            let qa = Quat::from_vec4(at(a, 0)).normalize();
            let qb = Quat::from_vec4(at(b, 0)).normalize();
            Vec4::from(qa.slerp(qb, f))
        }
        Interp::Linear => at(a, 0).lerp(at(b, 0), f),
        Interp::Cubic => {
            if a == b {
                at(a, width)
            } else {
                let dt = c.times[b] - c.times[a];
                let (p0, m0) = (at(a, width), at(a, width * 2) * dt);
                let (p1, m1) = (at(b, width), at(b, 0) * dt);
                let (f2, f3) = (f * f, f * f * f);
                p0 * (2. * f3 - 3. * f2 + 1.) + m0 * (f3 - 2. * f2 + f) + p1 * (-2. * f3 + 3. * f2) + m1 * (f3 - f2)
            }
        }
    };
    Some(out)
}

pub fn pose(rest: &[Trs], clip: &Clip, t: f32) -> Vec<Trs> {
    let mut out = rest.to_vec();
    for c in &clip.channels {
        let Some(n) = out.get_mut(c.node) else { continue };
        match c.path {
            Path::T => {
                if let Some(v) = sample(c, t, 3) {
                    n.t = v.truncate();
                }
            }
            Path::S => {
                if let Some(v) = sample(c, t, 3) {
                    n.s = v.truncate();
                }
            }
            Path::R => {
                if let Some(v) = sample(c, t, 4) {
                    let q = Quat::from_vec4(v);
                    if q.length_squared() > 0. {
                        n.r = q.normalize();
                    }
                }
            }
            Path::W => {}
        }
    }
    out
}

/// Each node's animated morph weights at `t`, where the clip drives any.
pub fn weights(nodes: usize, clip: &Clip, t: f32) -> Vec<Option<Vec<f32>>> {
    let mut out = vec![None; nodes];
    for c in clip.channels.iter().filter(|c| c.path == Path::W && c.node < nodes) {
        let keys = c.times.len();
        let per = if c.interp == Interp::Cubic { 3 } else { 1 };
        if keys == 0 || c.values.len() % (keys * per) != 0 {
            continue;
        }
        let width = c.values.len() / (keys * per);
        let at = |k: usize, o: usize| &c.values[(k * per + o) * width..(k * per + o + 1) * width];
        let (a, b, f) = key(&c.times, t);
        let w = match c.interp {
            Interp::Step => at(a, 0).to_vec(),
            Interp::Linear => at(a, 0).iter().zip(at(b, 0)).map(|(x, y)| x + (y - x) * f).collect(),
            Interp::Cubic if a == b => at(a, 1).to_vec(),
            Interp::Cubic => {
                let dt = c.times[b] - c.times[a];
                let (f2, f3) = (f * f, f * f * f);
                (0..width)
                    .map(|j| {
                        let (p0, m0, p1, m1) = (at(a, 1)[j], at(a, 2)[j] * dt, at(b, 1)[j], at(b, 0)[j] * dt);
                        p0 * (2. * f3 - 3. * f2 + 1.) + m0 * (f3 - 2. * f2 + f) + p1 * (-2. * f3 + 3. * f2) + m1 * (f3 - f2)
                    })
                    .collect()
            }
        };
        out[c.node] = Some(w);
    }
    out
}

pub fn duration(channels: &[Channel]) -> f32 {
    channels.iter().filter_map(|c| c.times.last().copied()).fold(0., f32::max)
}

