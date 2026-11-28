//! Sideways pulls (lib/railmotion.js): a value from 0 to 1 held to a finger, or a trackpad's
//! two-finger swipe, then eased to where its release sends it.

use gpui::{ScrollEvent, ScrollSource, TouchPhase, Window, px};
use std::collections::VecDeque;
use std::time::{Duration, Instant};

const SETTLE_MS: f32 = 280.;
/// A release faster than this (px/ms) goes the way it was thrown, wherever it stopped.
const FLING: f32 = 0.35;
/// The release's speed is read over this much of the pull's end.
const SAMPLED: Duration = Duration::from_millis(100);

/// How long an ease from `from` to `to` takes: a shorter way home takes less time, so a
/// nearly-shut pane doesn't crawl.
pub fn settle_ms(from: f32, to: f32) -> f32 {
    if from == to { 0. } else { SETTLE_MS * (0.35 + (to - from).abs()).min(1.) }
}

/// A value eased between rests (a cubic ease-out), or held where it is put.
#[derive(Clone, Copy, Debug)]
pub struct Eased {
    from: f32,
    to: f32,
    at: Option<Instant>,
    ms: f32,
}

impl Eased {
    pub fn at(v: f32) -> Self {
        Self { from: v, to: v, at: None, ms: 0. }
    }

    fn value_at(&self, now: Instant) -> f32 {
        let Some(at) = self.at else { return self.to };
        let t = (now.saturating_duration_since(at).as_secs_f32() * 1000. / self.ms).clamp(0., 1.);
        self.from + (self.to - self.from) * (1. - (1. - t).powi(3))
    }

    fn moving_at(&self, now: Instant) -> bool {
        self.at.is_some_and(|at| now.saturating_duration_since(at).as_secs_f32() * 1000. < self.ms)
    }

    pub fn value(&self) -> f32 {
        self.value_at(Instant::now())
    }

    pub fn moving(&self) -> bool {
        self.moving_at(Instant::now())
    }

    /// Where it rests, or is on its way to.
    pub fn target(&self) -> f32 {
        self.to
    }

    /// The value to draw this frame, asking for the next one while it moves.
    pub fn get(&mut self, window: &mut Window) -> f32 {
        let now = Instant::now();
        if self.moving_at(now) {
            window.request_animation_frame();
            self.value_at(now)
        } else {
            *self = Self::at(self.to);
            self.to
        }
    }

    /// Ease from wherever it is now; a newer target takes over mid-way.
    pub fn go(&mut self, to: f32) {
        self.go_at(to, Instant::now());
    }

    fn go_at(&mut self, to: f32, now: Instant) {
        let from = self.value_at(now);
        let ms = settle_ms(from, to);
        *self = if ms == 0. { Self::at(to) } else { Self { from, to, at: Some(now), ms } };
    }

    /// Held there: a finger has it.
    pub fn set(&mut self, v: f32) {
        *self = Self::at(v);
    }

    /// Stopped where it is, for a pull to take hold.
    pub fn stop(&mut self) -> f32 {
        let v = self.value();
        self.set(v);
        v
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
enum Phase {
    #[default]
    Idle,
    /// Begun without moving yet (a trackpad's first touch, or a finger catching a fling).
    Undecided,
    Pulling,
    /// Upright, or someone else's: followed only to its end.
    Refused,
}

/// What a scroll event did to a pull.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Step {
    /// Held at this amount.
    Move(f32),
    /// Let go: whether it ends open (at 1).
    End(bool),
}

/// One pull, fed the scroll events of what it lies on: sideways only, so a gesture that
/// heads up or down is left to scroll. A release decides by its speed, else by how far it got.
#[derive(Default)]
pub struct Pull {
    phase: Phase,
    p0: f32,
    width: f32,
    dx: f32,
    dy: f32,
    last: f32,
    samples: VecDeque<(Instant, f32)>,
}

impl Pull {
    pub fn pulling(&self) -> bool {
        self.phase == Phase::Pulling
    }

    /// `begin(dx)`, for a sideways start (+ rightward): the amount it starts from and the px
    /// of a full pull, or none when the gesture is someone else's.
    pub fn feed(&mut self, e: &ScrollEvent, begin: impl FnOnce(f32) -> Option<(f32, f32)>) -> Option<Step> {
        // A wheel's notches, and a trackpad that never says when it's touched, aren't pulls.
        if e.source != ScrollSource::Touch && !e.delta.precise() {
            return None;
        }
        let d = e.delta.pixel_delta(px(16.));
        self.feed_at(e.touch_phase, f32::from(d.x), f32::from(d.y), Instant::now(), begin)
    }

    fn feed_at(
        &mut self,
        phase: TouchPhase,
        dx: f32,
        dy: f32,
        now: Instant,
        begin: impl FnOnce(f32) -> Option<(f32, f32)>,
    ) -> Option<Step> {
        match phase {
            TouchPhase::Started => {
                // A start without the last one's end: that one was let go unseen.
                let lost = self.abandon();
                self.phase = Phase::Undecided;
                self.dx = dx;
                self.dy = dy;
                self.samples.clear();
                self.decide(now, begin).or(lost)
            }
            TouchPhase::Moved => match self.phase {
                Phase::Undecided => {
                    self.dx += dx;
                    self.dy += dy;
                    self.decide(now, begin)
                }
                Phase::Pulling => {
                    self.dx += dx;
                    Some(Step::Move(self.track(now)))
                }
                // Momentum after a release, or a gesture that was never ours.
                Phase::Idle | Phase::Refused => None,
            },
            TouchPhase::Ended => {
                let was = std::mem::take(&mut self.phase);
                if was != Phase::Pulling {
                    return None;
                }
                self.dx += dx;
                let last = self.track(now);
                let (t0, d0) = self.samples.front().copied().unwrap_or((now, self.dx));
                let v = (self.dx - d0) / (now.saturating_duration_since(t0).as_secs_f32() * 1000.).max(1.);
                Some(Step::End(if v > FLING { true } else if v < -FLING { false } else { last > 0.5 }))
            }
            TouchPhase::Cancelled => self.abandon(),
        }
    }

    /// A pull that loses its gesture ends where it got to, so it can't hold a pane mid-way.
    pub fn abandon(&mut self) -> Option<Step> {
        (std::mem::take(&mut self.phase) == Phase::Pulling).then(|| Step::End(self.last > 0.5))
    }

    fn decide(&mut self, now: Instant, begin: impl FnOnce(f32) -> Option<(f32, f32)>) -> Option<Step> {
        if self.dx == 0. && self.dy == 0. {
            return None;
        }
        if self.dx.abs() <= self.dy.abs() {
            self.phase = Phase::Refused;
            return None;
        }
        let Some((p0, width)) = begin(self.dx) else {
            self.phase = Phase::Refused;
            return None;
        };
        self.phase = Phase::Pulling;
        self.p0 = p0;
        self.width = width.max(1.);
        // Measured from the gesture's start, so the slop it crossed to become one counts.
        self.samples.push_back((now, 0.));
        Some(Step::Move(self.track(now)))
    }

    fn track(&mut self, now: Instant) -> f32 {
        self.samples.push_back((now, self.dx));
        while self.samples.len() > 2 && self.samples.front().is_some_and(|(t, _)| now.saturating_duration_since(*t) > SAMPLED) {
            self.samples.pop_front();
        }
        self.last = (self.p0 + self.dx / self.width).clamp(0., 1.);
        self.last
    }
}

/// A finger's sideways drag in px from where it began, upright gestures left to scroll.
#[derive(Default)]
pub struct Drag {
    phase: Phase,
    dx: f32,
    dy: f32,
}

impl Drag {
    pub fn dragging(&self) -> bool {
        self.phase == Phase::Pulling
    }

    /// `begin(dx)`: whether a sideways start (+ rightward) is this drag's. A touch's only: a
    /// trackpad's swipe is the pointer's.
    pub fn feed(&mut self, e: &ScrollEvent, begin: impl FnOnce(f32) -> bool) -> Option<Step> {
        if e.source != ScrollSource::Touch {
            return None;
        }
        let d = e.delta.pixel_delta(px(16.));
        self.feed_at(e.touch_phase, f32::from(d.x), f32::from(d.y), begin)
    }

    /// `Move(dx)` while it drags; `End(true)` let go, `End(false)` lost.
    fn feed_at(&mut self, phase: TouchPhase, dx: f32, dy: f32, begin: impl FnOnce(f32) -> bool) -> Option<Step> {
        match phase {
            TouchPhase::Started => {
                let lost = (std::mem::take(&mut self.phase) == Phase::Pulling).then_some(Step::End(false));
                self.phase = Phase::Undecided;
                (self.dx, self.dy) = (dx, dy);
                self.decide(begin).or(lost)
            }
            TouchPhase::Moved => match self.phase {
                Phase::Undecided => {
                    self.dx += dx;
                    self.dy += dy;
                    self.decide(begin)
                }
                Phase::Pulling => {
                    self.dx += dx;
                    Some(Step::Move(self.dx))
                }
                Phase::Idle | Phase::Refused => None,
            },
            TouchPhase::Ended => (std::mem::take(&mut self.phase) == Phase::Pulling).then(|| {
                self.dx += dx;
                Step::End(true)
            }),
            TouchPhase::Cancelled => (std::mem::take(&mut self.phase) == Phase::Pulling).then_some(Step::End(false)),
        }
    }

    fn decide(&mut self, begin: impl FnOnce(f32) -> bool) -> Option<Step> {
        if self.dx == 0. && self.dy == 0. {
            return None;
        }
        if self.dx.abs() <= self.dy.abs() || !begin(self.dx) {
            self.phase = Phase::Refused;
            return None;
        }
        self.phase = Phase::Pulling;
        Some(Step::Move(self.dx))
    }
}

/// Whether a pull that may only move one way at a rest takes a gesture heading `dx`: out
/// (rightward) while away, back while out, and either way mid-slide.
pub fn takes(at: &Eased, dx: f32) -> bool {
    if at.moving() {
        return true;
    }
    let p = at.value();
    !((p <= 0. && dx < 0.) || (p >= 1. && dx > 0.))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn a_sideways_pull_follows_and_a_slow_release_goes_by_distance() {
        let t = Instant::now();
        let mut p = Pull::default();
        assert_eq!(p.feed_at(TouchPhase::Started, 10., 0., t, |_| Some((0., 100.))), Some(Step::Move(0.1)));
        assert_eq!(p.feed_at(TouchPhase::Moved, 50., 0., t + ms(300), |_| None), Some(Step::Move(0.6)));
        // Held still, then let go past half: open.
        assert_eq!(p.feed_at(TouchPhase::Ended, 0., 0., t + ms(900), |_| None), Some(Step::End(true)));
        // Its momentum is nobody's.
        assert_eq!(p.feed_at(TouchPhase::Moved, 30., 0., t + ms(916), |_| None), None);
    }

    #[test]
    fn a_fling_goes_the_way_it_was_thrown() {
        let t = Instant::now();
        let mut p = Pull::default();
        p.feed_at(TouchPhase::Started, 10., 0., t, |_| Some((0., 300.)));
        p.feed_at(TouchPhase::Moved, 30., 0., t + ms(40), |_| None);
        // 40 px of 300 (short of half), but thrown at 1 px/ms.
        assert_eq!(p.feed_at(TouchPhase::Ended, 0., 0., t + ms(40), |_| None), Some(Step::End(true)));
        p.feed_at(TouchPhase::Started, -10., 0., t + ms(500), |_| Some((1., 300.)));
        assert_eq!(p.feed_at(TouchPhase::Ended, -20., 0., t + ms(520), |_| None), Some(Step::End(false)));
    }

    #[test]
    fn an_upright_gesture_or_a_declined_one_is_left_alone() {
        let t = Instant::now();
        let mut p = Pull::default();
        assert_eq!(p.feed_at(TouchPhase::Started, 0., 12., t, |_| panic!("asked about an upright gesture")), None);
        assert_eq!(p.feed_at(TouchPhase::Moved, 80., 0., t + ms(16), |_| panic!()), None);
        assert_eq!(p.feed_at(TouchPhase::Ended, 0., 0., t + ms(32), |_| panic!()), None);
        assert_eq!(p.feed_at(TouchPhase::Started, -12., 0., t + ms(100), |dx| (dx > 0.).then_some((0., 100.))), None);
        assert_eq!(p.feed_at(TouchPhase::Moved, -40., 0., t + ms(116), |_| panic!()), None);
    }

    #[test]
    fn a_trackpad_decides_on_its_first_movement() {
        let t = Instant::now();
        let mut p = Pull::default();
        assert_eq!(p.feed_at(TouchPhase::Started, 0., 0., t, |_| panic!("decided before moving")), None);
        assert_eq!(p.feed_at(TouchPhase::Moved, -5., 1., t + ms(8), |_| Some((1., 50.))), Some(Step::Move(0.9)));
        assert!(p.pulling());
        // Lost without an end: it settles where it got to.
        assert_eq!(p.feed_at(TouchPhase::Cancelled, 0., 0., t + ms(16), |_| None), Some(Step::End(true)));
        assert!(!p.pulling());
    }

    #[test]
    fn the_pull_is_clamped_to_its_range() {
        let t = Instant::now();
        let mut p = Pull::default();
        p.feed_at(TouchPhase::Started, 10., 0., t, |_| Some((0.5, 100.)));
        assert_eq!(p.feed_at(TouchPhase::Moved, 500., 0., t + ms(200), |_| None), Some(Step::Move(1.)));
        assert_eq!(p.feed_at(TouchPhase::Moved, -2000., 0., t + ms(400), |_| None), Some(Step::Move(0.)));
    }

    #[test]
    fn a_drag_follows_in_px_and_reports_how_it_ended() {
        let mut d = Drag::default();
        assert_eq!(d.feed_at(TouchPhase::Started, -9., 1., |dx| dx < 0.), Some(Step::Move(-9.)));
        assert_eq!(d.feed_at(TouchPhase::Moved, -40., 3., |_| panic!()), Some(Step::Move(-49.)));
        assert_eq!(d.feed_at(TouchPhase::Ended, -2., 0., |_| panic!()), Some(Step::End(true)));
        assert_eq!(d.dx, -51.);
        assert_eq!(d.feed_at(TouchPhase::Moved, -30., 0., |_| panic!()), None);
        // Heading the other way is someone else's.
        assert_eq!(d.feed_at(TouchPhase::Started, 9., 0., |dx| dx < 0.), None);
        assert_eq!(d.feed_at(TouchPhase::Ended, 0., 0., |_| panic!()), None);
        d.feed_at(TouchPhase::Started, -9., 0., |_| true);
        assert_eq!(d.feed_at(TouchPhase::Cancelled, 0., 0., |_| panic!()), Some(Step::End(false)));
    }

    #[test]
    fn an_ease_eases_out_and_a_shorter_way_takes_less_time() {
        let t = Instant::now();
        let mut e = Eased::at(0.);
        e.go_at(1., t);
        assert_eq!(e.value_at(t), 0.);
        assert!((e.value_at(t + ms(140)) - 0.875).abs() < 1e-3);
        assert_eq!(e.value_at(t + ms(280)), 1.);
        assert!((settle_ms(0.9, 1.) - 126.).abs() < 1e-3);
        // A newer target takes over from where it is.
        let mut e = Eased::at(0.);
        e.go_at(1., t);
        e.go_at(0., t + ms(140));
        assert!((e.value_at(t + ms(140)) - 0.875).abs() < 1e-3);
    }
}
