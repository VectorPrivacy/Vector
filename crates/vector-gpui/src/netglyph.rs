//! The Routing card's glyphs (`tor-glyph.js`, `NetGlyph.svelte`): Tor's rings with comets that
//! double as its bootstrap progress, I2P's turning tunnels, Clearnet's globe. Painted on a 120
//! unit grid scaled to the box, every animation timed from one clock so a repaint never jumps.

use std::f32::consts::TAU;
use std::sync::LazyLock;
use std::time::Instant;

use gpui::prelude::*;
use gpui::{AnyElement, Bounds, Hsla, PathBuilder, Pixels, Window, canvas, point, px};

use crate::theme::ca;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Net {
    Tor,
    I2p,
    Clearnet,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum State {
    Disabled,
    Starting,
    Connected,
    Failed,
}

static EPOCH: LazyLock<Instant> = LazyLock::new(Instant::now);

fn secs() -> f32 {
    EPOCH.elapsed().as_secs_f32()
}

/// 0..1 through a loop of `period` seconds.
fn phase(period: f32) -> f32 {
    (secs() / period).fract()
}

/// CSS `ease-in-out` over a 0→1→0 keyframe at 50%.
fn breathe(period: f32) -> f32 {
    let t = phase(period);
    let half = if t < 0.5 { t * 2. } else { (1. - t) * 2. };
    ease_in_out(half)
}

fn ease_in_out(x: f32) -> f32 {
    // cubic-bezier(0.42, 0, 0.58, 1), close enough by smoothstep.
    x * x * (3. - 2. * x)
}

fn rgba(r: u8, g: u8, b: u8, a: f32) -> Hsla {
    ca(r, g, b, a)
}

/// `progress` drives Tor's comets while it bootstraps (0..=100).
pub fn glyph(net: Net, state: State, progress: Option<u8>, size: f32) -> AnyElement {
    let animated = matches!(state, State::Starting | State::Connected) && net != Net::Clearnet;
    canvas(
        |_, _, _| {},
        move |b, _, window, _| {
            let g = Grid { b, k: f32::from(b.size.width) / 120. };
            match net {
                Net::Tor => tor(&g, state, progress, window),
                Net::I2p => i2p(&g, state, window),
                Net::Clearnet => globe(&g, state, window),
            }
            if animated {
                window.request_animation_frame();
            }
        },
    )
    .size(px(size))
    .flex_shrink_0()
    .into_any_element()
}

struct Grid {
    b: Bounds<Pixels>,
    k: f32,
}

impl Grid {
    fn p(&self, x: f32, y: f32) -> gpui::Point<Pixels> {
        point(self.b.origin.x + px(x * self.k), self.b.origin.y + px(y * self.k))
    }

    /// A stroked arc of the circle at the centre, from `from` turning `len` (both 0..1 of a turn,
    /// clockwise from 3 o'clock as an SVG circle's path runs).
    fn arc(&self, window: &mut Window, r: f32, from: f32, len: f32, width: f32, color: Hsla, round: bool) {
        if len <= 0. || color.a <= 0. {
            return;
        }
        let len = len.min(1.);
        let steps = ((len * 96.).ceil() as usize).max(2);
        let mut path = PathBuilder::stroke(px(width * self.k));
        for i in 0..=steps {
            let a = (from + len * i as f32 / steps as f32) * TAU;
            let q = self.p(60. + r * a.cos(), 60. + r * a.sin());
            if i == 0 { path.move_to(q) } else { path.line_to(q) }
        }
        if len >= 1. {
            path.close();
        }
        if let Ok(path) = path.build() {
            window.paint_path(path, color);
        }
        if round && len < 1. {
            for t in [from, from + len] {
                let a = t * TAU;
                self.dot(window, 60. + r * a.cos(), 60. + r * a.sin(), width / 2., color);
            }
        }
    }

    fn ring(&self, window: &mut Window, r: f32, width: f32, color: Hsla) {
        self.arc(window, r, 0., 1., width, color, false);
    }

    fn dot(&self, window: &mut Window, x: f32, y: f32, r: f32, color: Hsla) {
        if color.a <= 0. {
            return;
        }
        let mut path = PathBuilder::fill();
        let n = 32;
        for i in 0..=n {
            let a = i as f32 / n as f32 * TAU;
            let q = self.p(x + r * a.cos(), y + r * a.sin());
            if i == 0 { path.move_to(q) } else { path.line_to(q) }
        }
        path.close();
        if let Ok(path) = path.build() {
            window.paint_path(path, color);
        }
    }

    /// `.tor-halo` / `.i2p-halo`: r 55, breathing in scale and opacity.
    fn halo(&self, window: &mut Window, color: Hsla) {
        let t = breathe(3.2);
        let scale = 1. + 0.06 * t;
        let opacity = 0.55 + 0.4 * t;
        self.ring(window, 55. * scale, 2. * scale, Hsla { a: color.a * opacity, ..color });
    }

    /// A soft glow behind a lit glyph, standing in for its drop-shadow.
    fn glow(&self, window: &mut Window, color: Hsla) {
        for (r, w, a) in [(48., 10., 0.10), (34., 9., 0.08), (20., 8., 0.08)] {
            self.ring(window, r, w, Hsla { a: color.a * a, ..color });
        }
    }
}

fn tor(g: &Grid, state: State, progress: Option<u8>, window: &mut Window) {
    const RINGS: [(f32, f32, f32, bool); 3] = [(20., 1.4, 7., false), (34., 2.1, 11., true), (48., 3.0, 18., false)];
    let ring = match state {
        State::Disabled => rgba(255, 255, 255, 0.18),
        State::Starting => rgba(194, 133, 255, 0.18),
        State::Connected => rgba(178, 102, 255, 0.55),
        State::Failed => rgba(255, 120, 96, 0.45),
    };
    if state == State::Connected {
        g.glow(window, rgba(149, 48, 244, 0.45));
        g.halo(window, rgba(149, 48, 244, 0.45));
    }
    for (r, ..) in RINGS {
        g.ring(window, r, 1.6, ring);
    }
    if state == State::Starting {
        let lit = f32::from(progress.unwrap_or(8).max(8)) / 100.;
        for (r, spin, _, reverse) in RINGS {
            let t = phase(spin);
            let from = if reverse { -t } else { t };
            g.arc(window, r, from, lit, 2.6, rgba(0xc2, 0x85, 0xff, 1.), true);
        }
    }
    let dots = match state {
        State::Connected => Some(rgba(0xd9, 0xb3, 0xff, 1.)),
        State::Failed => Some(rgba(0xff, 0x78, 0x60, 0.7)),
        _ => None,
    };
    if let Some(color) = dots {
        for ((r, _, orbit, reverse), size) in RINGS.into_iter().zip([2.5, 3., 3.5]) {
            let turn = if state == State::Connected { phase(orbit) * if reverse { -1. } else { 1. } } else { 0. };
            let a = (turn - 0.25) * TAU;
            g.dot(window, 60. + r * a.cos(), 60. + r * a.sin(), size, color);
        }
    }
    let (core, pulse) = match state {
        State::Disabled => (rgba(255, 255, 255, 0.4), 1.),
        State::Starting => (rgba(0xc2, 0x85, 0xff, 1.), 1. + 0.25 * breathe(1.4)),
        State::Connected => (rgba(0xd9, 0xb3, 0xff, 1.), 1.),
        State::Failed => (rgba(0xff, 0x78, 0x60, 1.), 1.),
    };
    g.dot(window, 60., 60., 6. * pulse, core);
}

fn i2p(g: &Grid, state: State, window: &mut Window) {
    // (radius, the arc's own rotation, starting spin, connected spin, reversed)
    const LAYERS: [(f32, f32, f32, f32, bool); 3] =
        [(20., -90., 1.4, 9., false), (34., 30., 2.1, 14., true), (48., 150., 3., 22., false)];
    let (track, arc, lit) = match state {
        State::Disabled => (rgba(255, 255, 255, 0.05), rgba(255, 255, 255, 0.2), 0.7),
        State::Starting => (rgba(95, 212, 234, 0.14), rgba(0x5f, 0xd4, 0xea, 1.), 0.16),
        State::Connected => (rgba(95, 212, 234, 0.12), rgba(122, 222, 240, 0.88), 0.7),
        State::Failed => (rgba(255, 255, 255, 0.05), rgba(255, 120, 96, 0.5), 0.7),
    };
    if state == State::Connected {
        g.glow(window, rgba(64, 200, 228, 0.42));
        g.halo(window, rgba(64, 200, 228, 0.42));
    }
    for (r, rot, fast, slow, reverse) in LAYERS {
        let turn = match state {
            State::Starting => phase(fast),
            State::Connected => phase(slow),
            _ => 0.,
        } * if reverse { -1. } else { 1. };
        g.ring(window, r, 1.6, track);
        g.arc(window, r, rot / 360. + turn, lit, 2.4, arc, true);
    }
    let (core, pulse) = match state {
        State::Disabled => (rgba(255, 255, 255, 0.4), 1.),
        State::Starting => (rgba(0x5f, 0xd4, 0xea, 1.), 1. + 0.25 * breathe(1.4)),
        State::Connected => (rgba(0xb3, 0xec, 0xf7, 1.), 1.),
        State::Failed => (rgba(0xff, 0x78, 0x60, 1.), 1.),
    };
    g.dot(window, 60., 60., 6. * pulse, core);
}

fn globe(g: &Grid, state: State, window: &mut Window) {
    let lit = state == State::Connected;
    let line = rgba(255, 255, 255, if lit { 0.75 } else { 0.2 });
    if lit {
        g.ring(window, 55., 2., rgba(255, 255, 255, 0.14));
    }
    g.ring(window, 40., 1.6, line);
    let mut path = PathBuilder::stroke(px(1.6 * g.k));
    let n = 64;
    for i in 0..=n {
        let a = i as f32 / n as f32 * TAU;
        let q = g.p(60. + 17. * a.cos(), 60. + 40. * a.sin());
        if i == 0 { path.move_to(q) } else { path.line_to(q) }
    }
    path.close();
    for (x0, x1, y) in [(20., 100., 60.), (25.5, 94.5, 40.), (25.5, 94.5, 80.)] {
        path.move_to(g.p(x0, y));
        path.line_to(g.p(x1, y));
    }
    if let Ok(path) = path.build() {
        window.paint_path(path, line);
    }
}
