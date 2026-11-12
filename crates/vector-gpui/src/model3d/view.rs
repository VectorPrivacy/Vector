//! One model on screen: its turntable (a drag turns the model about the view's own axes, a
//! flick coasts, the wheel zooms), its pedestal spin, its animation clock, and the last frame
//! the engine drew for it.

use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::Instant;

use glam::{Quat, Vec3};
use gpui::RenderImage;

use super::{Clip, Job, Out, Shot, Src};

const SPIN_PER_PX: f32 = 0.01;
/// Per ms: a flick coasts for about half a second.
const SPIN_DECAY: f32 = 0.0045;
/// The pedestal: an untouched model turns about its vertical axis, a turn in about 18 s, at 30 fps.
const PEDESTAL_SPEED: f32 = 0.35;
const PEDESTAL_FRAME: f32 = 1. / 30.;
pub const SPEEDS: [f32; 4] = [0.25, 0.5, 1., 2.];
const FADE: f32 = 0.25;

#[derive(Clone, PartialEq)]
pub enum Phase {
    Loading,
    Ready,
    Failed(String),
    Unavailable,
}

struct Press {
    last: (f32, f32),
    travel: f32,
    at: Instant,
    vel: (f32, f32),
    vel_at: Instant,
}

pub struct View {
    id: u64,
    rx: Receiver<Out>,
    pub phase: Phase,
    pub clips: Vec<Clip>,
    pivot: Quat,
    home: (Quat, f32),
    distance: f32,
    limits: (f32, f32),
    pub playing: bool,
    pub clip: usize,
    time: f32,
    pub speed: f32,
    spinning: bool,
    spun: f32,
    press: Option<Press>,
    coast: Option<((f32, f32), Instant)>,
    pub frame: Option<Arc<RenderImage>>,
    ready_at: Option<Instant>,
    inflight: bool,
    dirty: bool,
    css: (f32, f32),
    px: (u32, u32),
    last: Option<Instant>,
    pub fullscreen: bool,
    pub retired: Vec<Arc<RenderImage>>,
    pub seen: Option<Instant>,
}

fn axes() -> (Vec3, Vec3) {
    let eye = Vec3::new(0.6, 0.45, 1.).normalize();
    let fwd = -eye;
    let right = fwd.cross(Vec3::Y).normalize();
    (right, right.cross(fwd))
}

impl View {
    pub fn new(src: Src, ext: &str, fullscreen: bool, wake: futures::channel::mpsc::UnboundedSender<()>) -> Self {
        let id = super::next_id();
        let (out, rx) = std::sync::mpsc::channel();
        let _ = super::engine().send(Job::Load { id, src, ext: ext.to_string(), out, wake });
        Self {
            id,
            rx,
            phase: Phase::Loading,
            clips: Vec::new(),
            pivot: Quat::IDENTITY,
            home: (Quat::IDENTITY, 1.),
            distance: 1.,
            limits: (0.2, 4.),
            playing: false,
            clip: 0,
            time: 0.,
            speed: 1.,
            spinning: false,
            spun: 0.,
            press: None,
            coast: None,
            frame: None,
            ready_at: None,
            inflight: false,
            dirty: false,
            css: (0., 0.),
            px: (0, 0),
            last: None,
            fullscreen,
            retired: Vec::new(),
            seen: None,
        }
    }

    pub fn ready(&self) -> bool {
        self.phase == Phase::Ready
    }

    /// The canvas fades in over its first quarter second.
    pub fn opacity(&self) -> f32 {
        self.ready_at.map_or(0., |t| (t.elapsed().as_secs_f32() / FADE).min(1.))
    }

    pub fn duration(&self) -> f32 {
        self.clips.get(self.clip).map_or(0., |c| c.duration)
    }

    pub fn time(&self) -> f32 {
        let d = self.duration();
        if d > 0. { self.time.rem_euclid(d) } else { 0. }
    }

    /// Sharp up to about 4 MP: a full screen at 2x would otherwise be 16.
    pub fn set_size(&mut self, w: f32, h: f32, scale: f32) {
        if w < 1. || h < 1. {
            return;
        }
        let dpr = scale.min(2.).min((4e6 / (w * h)).sqrt());
        let px = (((w * dpr).round() as u32).max(1), ((h * dpr).round() as u32).max(1));
        self.css = (w, h);
        if px != self.px {
            self.px = px;
            self.dirty = true;
        }
    }

    fn poll(&mut self) {
        while let Ok(o) = self.rx.try_recv() {
            match o {
                Out::Ready { clips, radius } => {
                    let aspect = if self.css.1 > 0. { self.css.0 / self.css.1 } else { 1. };
                    let fit = super::fit(radius, aspect);
                    self.distance = fit;
                    self.limits = (radius * 1.05, fit * 4.);
                    self.home = (self.pivot, fit);
                    self.clips = clips;
                    self.phase = Phase::Ready;
                    self.spinning = true;
                    if self.fullscreen && !self.clips.is_empty() {
                        self.playing = true;
                    }
                    self.dirty = true;
                }
                Out::Failed(text) => self.phase = Phase::Failed(text),
                Out::Unavailable => self.phase = Phase::Unavailable,
                Out::Frame { w, h, bgra } => {
                    self.inflight = false;
                    if let Some(buf) = image::RgbaImage::from_raw(w, h, bgra) {
                        let img = Arc::new(RenderImage::new(vec![image::Frame::new(buf)]));
                        if let Some(old) = self.frame.replace(img) {
                            self.retired.push(old);
                        }
                        self.ready_at.get_or_insert_with(Instant::now);
                    }
                }
            }
        }
    }

    fn submit(&mut self) {
        if !self.dirty || self.inflight || self.phase != Phase::Ready || self.px.0 == 0 {
            return;
        }
        self.dirty = false;
        self.inflight = true;
        let clip = (!self.clips.is_empty()).then(|| (self.clip, self.time()));
        let shot = Shot { w: self.px.0, h: self.px.1, pivot: self.pivot, distance: self.distance, clip };
        let _ = super::engine().send(Job::Render { id: self.id, shot });
    }

    /// One frame: true while it moves on its own. `runs` is false while it is off screen, the
    /// window is in the background, or a full-screen view stands in front of it.
    pub fn tick(&mut self, runs: bool) -> bool {
        self.poll();
        let now = Instant::now();
        let dt = self.last.map_or(0., |l| (now - l).as_secs_f32()).min(0.1);
        self.last = Some(now);
        let mut moving = false;
        if runs && self.ready() {
            if self.playing && self.duration() > 0. {
                self.time = (self.time + dt * self.speed).rem_euclid(self.duration());
                self.dirty = true;
                moving = true;
            }
            if self.spinning {
                moving = true;
                self.spun += dt;
                if self.spun >= PEDESTAL_FRAME {
                    self.pivot = Quat::from_rotation_y(self.spun * PEDESTAL_SPEED) * self.pivot;
                    self.spun = 0.;
                    self.dirty = true;
                }
            }
            if let Some(((vx, vy), t0)) = self.coast {
                let ms = ((now - t0).as_secs_f32() * 1000.).min(50.);
                let k = (-SPIN_DECAY * ms).exp();
                let (vx, vy) = (vx * k, vy * k);
                if vx.hypot(vy) < 0.02 {
                    self.coast = None;
                } else {
                    self.turn(vx * ms, vy * ms);
                    self.coast = Some(((vx, vy), now));
                    moving = true;
                }
            }
        }
        self.submit();
        moving || (self.ready_at.is_some() && self.opacity() < 1.)
    }

    fn turn(&mut self, dx: f32, dy: f32) {
        let (right, up) = axes();
        self.pivot = Quat::from_axis_angle(up, dx * SPIN_PER_PX) * self.pivot;
        self.pivot = Quat::from_axis_angle(right, dy * SPIN_PER_PX) * self.pivot;
        self.dirty = true;
    }

    fn zoom(&mut self, f: f32) {
        self.distance = (self.distance * f).clamp(self.limits.0, self.limits.1.max(self.limits.0));
        self.dirty = true;
    }

    fn touched(&mut self) {
        self.spinning = false;
    }

    pub fn down(&mut self, x: f32, y: f32) {
        self.coast = None;
        let now = Instant::now();
        self.press = Some(Press { last: (x, y), travel: 0., at: now, vel: (0., 0.), vel_at: now });
    }

    pub fn css_size(&self) -> (f32, f32) {
        self.css
    }

    /// Whether it will want the next frame: its paint asks for one, as ticks stop off screen.
    pub fn moving_hint(&self) -> bool {
        self.ready() && (self.playing || self.spinning || self.coast.is_some() || (self.ready_at.is_some() && self.opacity() < 1.))
    }

    pub fn dragging(&self) -> bool {
        self.press.is_some()
    }

    pub fn moved(&mut self, x: f32, y: f32) {
        let Some(p) = &mut self.press else { return };
        let (dx, dy) = (x - p.last.0, y - p.last.1);
        if dx == 0. && dy == 0. {
            return;
        }
        p.last = (x, y);
        p.travel += dx.hypot(dy);
        let now = Instant::now();
        let dt = ((now - p.vel_at).as_secs_f32() * 1000.).max(1.);
        p.vel = (dx / dt, dy / dt);
        p.vel_at = now;
        // A press only takes the model over once it moves: a tap is a tap.
        if p.travel > 4. {
            self.touched();
        }
        self.turn(dx, dy);
    }

    /// The press ends: true when it was a tap.
    pub fn up(&mut self) -> bool {
        let Some(p) = self.press.take() else { return false };
        let now = Instant::now();
        // A flick still moving when released coasts; a drag that stopped first doesn't.
        if (now - p.vel_at).as_millis() < 60 && p.travel > 4. {
            self.coast = Some((p.vel, now));
        }
        p.travel <= 4. && (now - p.at).as_millis() < 500
    }

    /// Browser wheel units: positive zooms out.
    pub fn wheel(&mut self, delta_y: f32) {
        if !self.ready() {
            return;
        }
        self.touched();
        self.zoom((delta_y * 0.01).exp());
    }

    pub fn reset(&mut self) {
        self.coast = None;
        self.pivot = self.home.0;
        self.distance = self.home.1;
        self.spinning = true;
        self.dirty = true;
    }

    pub fn set_playing(&mut self, on: bool) -> bool {
        if self.clips.is_empty() {
            return false;
        }
        self.playing = on;
        on
    }

    pub fn set_clip(&mut self, i: usize) {
        if i >= self.clips.len() || i == self.clip {
            return;
        }
        self.clip = i;
        self.time = 0.;
        self.dirty = true;
    }

    pub fn seek(&mut self, t: f32) {
        self.time = t.clamp(0., self.duration());
        self.dirty = true;
    }

    pub fn cycle_speed(&mut self) {
        let i = SPEEDS.iter().position(|&s| s == self.speed).unwrap_or(2);
        self.speed = SPEEDS[(i + 1) % SPEEDS.len()];
    }
}

impl Drop for View {
    fn drop(&mut self) {
        let _ = super::engine().send(Job::Drop { id: self.id });
    }
}
