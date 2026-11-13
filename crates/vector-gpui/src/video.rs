//! Video attachments (`VideoAttachment.svelte`): FFmpeg decodes off the UI thread at the size
//! the clip draws, the audio engine sounds its track and keeps the time, one plays at a time.

#![cfg_attr(not(feature = "video"), allow(dead_code))]

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, Bounds, CursorStyle, HitboxBehavior, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ObjectFit, Pixels, RenderImage, SharedString, Window, canvas, div, img, prelude::*, px,
    svg,
};
use vector_media::audio_engine::AudioInput;

use crate::audio::AudioInputSpec;
use crate::theme::*;

pub const EXTENSIONS: &[&str] = &["mp4", "webm", "mov"];

/// `.dmsg-attachments video`: fitted inside this box, never upscaled.
pub const MAX_W: f32 = 450.;
pub const MAX_H: f32 = 350.;
const QUEUE: usize = 4;
/// Controls fade this long after the pointer stops while a clip plays.
const IDLE: Duration = Duration::from_millis(2200);
/// The picture follows the sound; drift past this snaps it back.
const DRIFT_MS: i64 = 60;

pub fn fit(w: u32, h: u32) -> (f32, f32) {
    let (w, h) = (w.max(1) as f32, h.max(1) as f32);
    let s = (MAX_W / w).min(MAX_H / h).min(1.);
    ((w * s).round(), (h * s).round())
}

#[derive(Clone)]
pub struct Info {
    /// As it shows: turned already when the file says to.
    pub w: u32,
    pub h: u32,
    pub duration: u64,
    pub audio: bool,
    pub poster: Option<Arc<RenderImage>>,
}

#[derive(Clone, Debug)]
pub enum Act {
    Toggle,
    /// A scrub to this fraction; `done` on release.
    Seek(f32, bool),
    Mute,
    Hover(bool),
    Moved,
}

pub enum Work {
    Probe(String, AudioInputSpec, f32),
}

pub enum WorkDone {
    Probed(String, Option<Info>),
}

enum Cmd {
    Seek(u64, u32),
    Stop,
}

enum Out {
    Frame { epoch: u32, pts: u64, image: image::RgbaImage },
    End { epoch: u32 },
}

#[derive(Clone, Copy)]
struct Clock {
    anchor: u64,
    at: Option<Instant>,
}

impl Clock {
    fn now(&self) -> u64 {
        self.anchor + self.at.map_or(0, |t| t.elapsed().as_millis() as u64)
    }
}

/// A clip being played: its decoder and its sound.
struct Play {
    cmd: Sender<Cmd>,
    frames: Receiver<Out>,
    next: Option<Out>,
    epoch: u32,
    /// A seek landed and its first frame shows whatever the clock says.
    fresh: bool,
    clock: Clock,
    started: bool,
    audio: Arc<Mutex<Option<Result<u32, String>>>>,
    source: Option<u32>,
    /// The track would not load: the picture goes on alone.
    silent: bool,
    heard: Option<u64>,
    ended: bool,
}

impl Drop for Play {
    fn drop(&mut self) {
        let _ = self.cmd.send(Cmd::Stop);
        if let (Some(id), Ok(e)) = (self.source, crate::audio::engine()) {
            let _ = e.stop(id);
        }
    }
}

#[derive(Default)]
pub struct Clip {
    pub info: Option<Info>,
    play: Option<Play>,
    pub playing: bool,
    pub loading: bool,
    position: u64,
    frame: Option<Arc<RenderImage>>,
    pub muted: bool,
    hovered: bool,
    moved: Option<Instant>,
    scrub: Option<f32>,
}

impl Clip {
    pub fn position(&self) -> u64 {
        if let Some(f) = self.scrub
            && let Some(i) = &self.info
        {
            return (f * i.duration as f32) as u64;
        }
        match &self.play {
            Some(p) if self.playing && p.started => p.clock.now().min(self.info.as_ref().map_or(u64::MAX, |i| i.duration)),
            _ => self.position,
        }
    }

    /// What shows: the frame it is on, else the poster.
    pub fn picture(&self) -> Option<Arc<RenderImage>> {
        self.frame.clone().or_else(|| self.info.as_ref().and_then(|i| i.poster.clone()))
    }

    pub fn aspect(&self) -> f32 {
        self.info.as_ref().map_or(16. / 9., |i| i.w.max(1) as f32 / i.h.max(1) as f32)
    }

    pub fn duration(&self) -> u64 {
        self.info.as_ref().map_or(0, |i| i.duration)
    }

    fn controls(&self) -> bool {
        !self.playing || self.scrub.is_some() || (self.hovered && self.moved.is_some_and(|t| t.elapsed() < IDLE))
    }
}

#[derive(Default)]
pub struct Videos {
    clips: HashMap<String, Clip>,
    probing: HashSet<String>,
    failed: HashSet<String>,
    pub queue: Vec<Work>,
    active: Option<String>,
    /// Frames off screen, for the app to take out of the atlas.
    pub retired: Vec<Arc<RenderImage>>,
    pub scale: f32,
}

impl Videos {
    pub fn want_probe(&mut self, att: &str, input: &AudioInputSpec) {
        if self.clips.contains_key(att) || self.failed.contains(att) || !self.probing.insert(att.to_string()) {
            return;
        }
        self.queue.push(Work::Probe(att.to_string(), input.clone(), self.scale.max(1.)));
    }

    pub fn probed(&mut self, att: String, info: Option<Info>) {
        self.probing.remove(&att);
        match info {
            Some(i) => self.clips.entry(att).or_default().info = Some(i),
            None => {
                self.failed.insert(att);
            }
        }
    }

    /// Unreadable here: drawn as a file instead.
    pub fn failed(&self, att: &str) -> bool {
        self.failed.contains(att)
    }

    pub fn clip(&self, att: &str) -> Option<&Clip> {
        self.clips.get(att).filter(|c| c.info.is_some())
    }

    /// Returns true when this started a clip, so other sound stops.
    pub fn act(&mut self, att: &str, input: Option<AudioInputSpec>, act: Act) -> bool {
        let scale = self.scale.max(1.);
        match act {
            Act::Toggle => {
                let playing = self.clips.get(att).is_some_and(|c| c.playing || c.loading);
                if playing {
                    self.pause(att);
                    false
                } else if let Some(input) = input {
                    self.play(att, input, scale);
                    true
                } else {
                    false
                }
            }
            Act::Seek(f, done) => {
                let Some(c) = self.clips.get_mut(att) else { return false };
                let f = f.clamp(0., 1.);
                if !done {
                    c.scrub = Some(f);
                    return false;
                }
                c.scrub = None;
                let ms = (f * c.info.as_ref().map_or(0, |i| i.duration) as f32) as u64;
                seek(c, ms, input, scale);
                false
            }
            Act::Mute => {
                if let Some(c) = self.clips.get_mut(att) {
                    c.muted = !c.muted;
                    if let (Some(id), Ok(e)) = (c.play.as_ref().and_then(|p| p.source), crate::audio::engine()) {
                        let _ = e.set_volume(id, if c.muted { 0. } else { 1. });
                    }
                }
                false
            }
            Act::Hover(on) => {
                if let Some(c) = self.clips.get_mut(att) {
                    c.hovered = on;
                    c.moved = on.then(Instant::now);
                }
                false
            }
            Act::Moved => {
                if let Some(c) = self.clips.get_mut(att) {
                    c.hovered = true;
                    c.moved = Some(Instant::now());
                }
                false
            }
        }
    }

    fn play(&mut self, att: &str, input: AudioInputSpec, scale: f32) {
        if let Some(prev) = self.active.replace(att.to_string())
            && prev != att
        {
            self.stop(&prev);
        }
        let Some(c) = self.clips.get_mut(att) else { return };
        let Some(info) = c.info.clone() else { return };
        if c.play.as_ref().is_some_and(|p| p.ended) || (info.duration > 0 && c.position + 50 >= info.duration) {
            c.position = 0;
            if let Some(p) = &mut c.play {
                p.ended = false;
                restart(p, 0);
            }
        }
        c.playing = true;
        match &mut c.play {
            Some(p) => {
                p.clock = Clock { anchor: c.position, at: Some(Instant::now()) };
                if let (Some(id), Ok(e)) = (p.source, crate::audio::engine()) {
                    let _ = e.seek(id, c.position);
                    let _ = e.play(id);
                }
            }
            None => {
                c.loading = true;
                c.play = start(input, &info, c.position, scale);
            }
        }
    }

    pub fn pause(&mut self, att: &str) {
        if self.active.as_deref() == Some(att) {
            self.active = None;
        }
        let Some(c) = self.clips.get_mut(att) else { return };
        c.position = c.position();
        c.playing = false;
        c.loading = false;
        if let Some(p) = &mut c.play {
            p.clock = Clock { anchor: c.position, at: None };
            if let (Some(id), Ok(e)) = (p.source, crate::audio::engine()) {
                let _ = e.pause(id);
            }
        }
    }

    /// The clip playing now, if any.
    pub fn playing(&self) -> Option<&str> {
        let a = self.active.as_deref()?;
        self.clips.get(a).is_some_and(|c| c.playing || c.loading).then_some(a)
    }

    /// Every clip but `keep` stopped.
    pub fn stop_except(&mut self, keep: Option<&str>) {
        let ids: Vec<String> = self.clips.keys().filter(|a| Some(a.as_str()) != keep).cloned().collect();
        for a in ids {
            self.stop(&a);
        }
    }

    /// Paused, its decoder and sound let go; the frame it was on stays.
    pub fn stop(&mut self, att: &str) {
        self.pause(att);
        if let Some(c) = self.clips.get_mut(att) {
            c.play = None;
        }
    }

    /// Dropped with its frames, so the next sight of it probes its source afresh.
    pub fn forget(&mut self, att: &str) {
        self.stop(att);
        self.failed.remove(att);
        if let Some(c) = self.clips.remove(att) {
            self.retired.extend(c.frame);
            self.retired.extend(c.info.and_then(|i| i.poster));
        }
    }

    /// One frame: true while anything moves.
    pub fn tick(&mut self) -> bool {
        let mut moving = false;
        for c in self.clips.values_mut() {
            if c.scrub.is_some() {
                moving = true;
            }
            if c.hovered && c.playing && c.moved.is_some_and(|t| t.elapsed() < IDLE + Duration::from_millis(200)) {
                moving = true;
            }
            let Some(p) = &mut c.play else { continue };
            let duration = c.info.as_ref().map_or(0, |i| i.duration);
            if p.source.is_none()
                && let Some(res) = p.audio.lock().ok().and_then(|mut s| s.take())
            {
                p.source = res.ok();
                p.silent = p.source.is_none();
                if let (Some(id), Ok(e)) = (p.source, crate::audio::engine()) {
                    if c.muted {
                        let _ = e.set_volume(id, 0.);
                    }
                    let _ = e.seek(id, c.position);
                }
            }
            let audio_ready = c.info.as_ref().is_none_or(|i| !i.audio) || p.source.is_some() || p.silent;
            // Wait for a picture and the sound before the clock starts.
            if c.playing && !p.started && !p.fresh && audio_ready {
                p.started = true;
                c.loading = false;
                p.clock = Clock { anchor: c.position, at: Some(Instant::now()) };
                if let (Some(id), Ok(e)) = (p.source, crate::audio::engine()) {
                    let _ = e.play(id);
                }
            }
            if c.playing && p.started && let (Some(id), Ok(e)) = (p.source, crate::audio::engine()) {
                // Only a moving engine clock leads: a device that is not sounding holds still.
                if let Ok(a) = e.get_position(id)
                    && p.heard.replace(a).is_some_and(|prev| prev != a)
                {
                    let v = p.clock.now();
                    if a + 100 < duration && (a as i64 - v as i64).abs() > DRIFT_MS {
                        p.clock = Clock { anchor: a, at: Some(Instant::now()) };
                    }
                }
            }
            let now = if c.playing && p.started { p.clock.now() } else { c.position };
            loop {
                if p.next.is_none() {
                    match p.frames.try_recv() {
                        Ok(o) => p.next = Some(o),
                        Err(TryRecvError::Empty) => break,
                        Err(TryRecvError::Disconnected) => {
                            p.ended = true;
                            break;
                        }
                    }
                }
                match p.next.take() {
                    Some(Out::Frame { epoch, .. }) if epoch != p.epoch => {}
                    Some(Out::Frame { pts, image, .. }) if p.fresh || pts <= now => {
                        p.fresh = false;
                        let fresh = Arc::new(RenderImage::new(vec![image::Frame::new(image)]));
                        if let Some(old) = c.frame.replace(fresh) {
                            self.retired.push(old);
                        }
                    }
                    Some(Out::End { epoch }) if epoch == p.epoch => p.ended = true,
                    Some(Out::End { .. }) => {}
                    next => {
                        p.next = next;
                        break;
                    }
                }
            }
            let over = (duration > 0 && now >= duration)
                || (p.ended && p.next.is_none() && (duration == 0 || now + 40 >= duration));
            if c.playing && p.started && over {
                c.playing = false;
                c.position = duration;
                p.clock = Clock { anchor: duration, at: None };
                p.ended = true;
                if let (Some(id), Ok(e)) = (p.source, crate::audio::engine()) {
                    let _ = e.pause(id);
                }
            }
            if c.playing || c.loading || p.fresh {
                moving = true;
            }
        }
        if self.active.as_ref().is_some_and(|a| self.clips.get(a).is_some_and(|c| !c.playing && !c.loading)) {
            self.active = None;
        }
        moving
    }
}

fn restart(p: &mut Play, ms: u64) {
    p.epoch = p.epoch.wrapping_add(1);
    p.fresh = true;
    p.next = None;
    p.heard = None;
    let _ = p.cmd.send(Cmd::Seek(ms, p.epoch));
}

fn seek(c: &mut Clip, ms: u64, input: Option<AudioInputSpec>, scale: f32) {
    c.position = ms;
    if let Some(p) = &mut c.play {
        p.ended = false;
        restart(p, ms);
        p.clock = Clock { anchor: ms, at: c.playing.then(Instant::now) };
        if let (Some(id), Ok(e)) = (p.source, crate::audio::engine()) {
            let _ = e.seek(id, ms);
        }
    } else if let (Some(info), Some(input)) = (&c.info, input) {
        // Never played: a decoder for the frame there, kept for the play that follows.
        c.play = start(input, info, ms, scale);
    }
}

fn start(input: AudioInputSpec, info: &Info, from: u64, scale: f32) -> Option<Play> {
    let (cmd_tx, cmd_rx) = std::sync::mpsc::channel();
    let (tx, rx) = std::sync::mpsc::sync_channel(QUEUE);
    let (w, h) = fit(info.w, info.h);
    let target = (((w * scale) as u32).min(info.w).max(1), ((h * scale) as u32).min(info.h).max(1));
    let slot = Arc::new(Mutex::new(None));
    if info.audio {
        let (slot, input) = (slot.clone(), input.clone());
        // spawn-detached: decodes bytes in hand into the engine; owns no account state.
        std::thread::spawn(move || {
            let res = input.open().and_then(|i| load_audio(&i));
            if let Ok(mut s) = slot.lock() {
                *s = Some(res);
            }
        });
    }
    // spawn-detached: the decoder reads one file and answers only its channel.
    std::thread::Builder::new()
        .name("video-decode".into())
        .spawn(move || {
            if let Ok(i) = input.open() {
                let _ = decode(i, target, from, cmd_rx, tx);
            }
        })
        .ok()?;
    Some(Play {
        cmd: cmd_tx,
        frames: rx,
        next: None,
        epoch: 0,
        fresh: true,
        clock: Clock { anchor: from, at: None },
        started: false,
        audio: slot,
        source: None,
        silent: false,
        heard: None,
        ended: false,
    })
}

/// The track through the engine: streamed where it reads the container, decoded whole where not.
fn load_audio(input: &AudioInput) -> Result<u32, String> {
    let e = crate::audio::engine()?;
    let mp4 = input.extension().is_some_and(|x| matches!(x.as_str(), "mp4" | "mov" | "m4v"));
    if mp4 && let Ok(r) = e.load_from_file(input) {
        return Ok(r.id);
    }
    let (samples, rate) = decode_audio(input)?;
    e.load_from_samples(samples, rate).map(|r| r.id)
}

pub fn run(work: Work) -> futures::channel::oneshot::Receiver<WorkDone> {
    let (tx, rx) = futures::channel::oneshot::channel();
    // spawn-detached: reads the file's header and first frame; owns no account state.
    std::thread::spawn(move || {
        let done = match work {
            Work::Probe(att, input, scale) => WorkDone::Probed(att, input.open().ok().and_then(|i| probe(i, scale))),
        };
        let _ = tx.send(done);
    });
    rx
}

#[cfg(feature = "video")]
#[path = "video_ffmpeg.rs"]
mod ff;

/// Built without FFmpeg: every clip draws as a file.
#[cfg(not(feature = "video"))]
mod ff {
    use super::*;
    pub fn probe(_: AudioInput, _: f32) -> Option<Info> {
        None
    }
    pub fn decode(_: AudioInput, _: (u32, u32), _: u64, _: Receiver<Cmd>, _: std::sync::mpsc::SyncSender<Out>) -> Option<()> {
        None
    }
    pub fn decode_audio(_: &AudioInput) -> Result<(Vec<f32>, u32), String> {
        Err("no decoder".into())
    }
}

use ff::{decode, decode_audio, probe};

// ── The player ──────────────────────────────────────────────────────────────────────────

pub type OnAct = Rc<dyn Fn(Act, &mut Window, &mut App)>;

/// The clip at its fitted size, its controls over it while paused or pointed at.
pub fn view(id: &str, clip: &Clip, dim: bool, size: Option<(f32, f32)>, radius: f32, on: OnAct) -> AnyElement {
    let info = clip.info.as_ref();
    let (w, h) = size.unwrap_or_else(|| info.map_or((MAX_W, MAX_H * 0.6), |i| fit(i.w, i.h)));
    let picture = clip.picture();
    let duration = info.map_or(0, |i| i.duration);
    let pos = clip.position().min(duration);
    let shown = clip.controls() && !dim;
    let frac = if duration > 0 { pos as f32 / duration as f32 } else { 0. };
    let playing = clip.playing || clip.loading;
    let (o1, o2, o3, o4, o5) = (on.clone(), on.clone(), on.clone(), on.clone(), on.clone());
    let time = format!("{} / {}", crate::audio::format_time(pos), crate::audio::format_time(duration));
    let button = |icon: &'static str, key: &'static str, act: Act, on: OnAct| {
        div()
            .id(SharedString::from(format!("{key}-{id}")))
            .size(px(28.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(14.))
            .cursor_pointer()
            .hover(|s| s.bg(white_a(0.12)))
            .on_click(move |_, w, cx| {
                cx.stop_propagation();
                on(act.clone(), w, cx)
            })
            .child(svg().path(icon).size(px(16.)).text_color(gpui::white()))
    };
    let bar = div()
        .absolute()
        .left_0()
        .right_0()
        .bottom_0()
        .h(px(44.))
        .px(px(6.))
        .pb(px(4.))
        .flex()
        .items_end()
        .bg(gpui::linear_gradient(
            180.,
            gpui::linear_color_stop(gpui::black().opacity(0.), 0.),
            gpui::linear_color_stop(gpui::black().opacity(0.55), 1.),
        ))
        .child(
            div()
                .w_full()
                .h(px(28.))
                .flex()
                .items_center()
                .gap(px(4.))
                .child(button(
                    if playing { "icons/pause.svg" } else { "icons/play.svg" },
                    "vplay",
                    Act::Toggle,
                    o2,
                ))
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(gpui::white())
                        .flex_none()
                        .child(time),
                )
                .child(scrubber(id, frac, duration > 0, clip.scrub.is_some(), o3))
                .child(button(
                    if clip.muted { "icons/volume-mute.svg" } else { "icons/volume-max.svg" },
                    "vmute",
                    Act::Mute,
                    o4,
                )),
        );
    let veil = (!playing && pos == 0 && !dim).then(|| {
        div()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .size(px(52.))
                    .rounded(px(26.))
                    .bg(gpui::black().opacity(0.45))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(svg().path("icons/play.svg").size(px(22.)).text_color(gpui::white())),
            )
    });
    let spin = (clip.loading && !dim).then(|| {
        div()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .child(div().size(px(36.)).child(crate::attachments::spinner(36., 3., 1., 0.25, white_a(0.2), gpui::white())))
    });
    div()
        .id(SharedString::from(format!("video-{id}")))
        .relative()
        .w(px(w))
        .h(px(h))
        .rounded(px(radius))
        .overflow_hidden()
        .bg(gpui::black())
        .cursor_pointer()
        .when(dim, |d| d.opacity(0.25))
        .on_hover(move |on, w, cx| o5(Act::Hover(*on), w, cx))
        .on_mouse_move(move |_: &MouseMoveEvent, w, cx| o1(Act::Moved, w, cx))
        .on_click(move |_, w, cx| {
            cx.stop_propagation();
            on(Act::Toggle, w, cx)
        })
        .children(picture.map(|p| img(p).size_full().object_fit(ObjectFit::Fill)))
        .children(veil)
        .children(spin)
        .when(shown, |d| d.child(bar))
        .into_any_element()
}

fn scrubber(id: &str, frac: f32, seekable: bool, dragging: bool, on: OnAct) -> impl IntoElement {
    div()
        .id(SharedString::from(format!("vseek-{id}")))
        .flex_1()
        .h(px(20.))
        .mx(px(6.))
        .on_click(|_, _, cx| cx.stop_propagation())
        .child(
            canvas(
                move |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
                move |bounds: Bounds<Pixels>, hitbox, window, _| {
                    let mid = bounds.center().y;
                    let x0 = bounds.origin.x;
                    let w = bounds.size.width;
                    let track = Bounds::new(gpui::point(x0, mid - px(2.)), gpui::size(w, px(4.)));
                    window.paint_quad(gpui::fill(track, white_a(0.3)).corner_radii(px(2.)));
                    let done = Bounds::new(track.origin, gpui::size(w * frac, px(4.)));
                    window.paint_quad(gpui::fill(done, gpui::white()).corner_radii(px(2.)));
                    let knob = Bounds::new(gpui::point(x0 + w * frac - px(6.), mid - px(6.)), gpui::size(px(12.), px(12.)));
                    window.paint_quad(gpui::fill(knob, gpui::white()).corner_radii(px(6.)));
                    if !seekable {
                        return;
                    }
                    window.set_cursor_style(CursorStyle::PointingHand, &hitbox);
                    let at = move |x: Pixels| f32::from(x - bounds.origin.x) / f32::from(bounds.size.width).max(1.);
                    {
                        let (on, hitbox) = (on.clone(), hitbox.clone());
                        window.on_mouse_event(move |e: &MouseDownEvent, phase, window, cx| {
                            if phase.bubble() && e.button == MouseButton::Left && hitbox.is_hovered(window) {
                                cx.stop_propagation();
                                on(Act::Seek(at(e.position.x), false), window, cx);
                            }
                        });
                    }
                    if !dragging {
                        return;
                    }
                    {
                        let on = on.clone();
                        window.on_mouse_event(move |e: &MouseMoveEvent, phase, window, cx| {
                            if phase.bubble() && e.dragging() {
                                on(Act::Seek(at(e.position.x), false), window, cx);
                            }
                        });
                    }
                    window.on_mouse_event(move |e: &MouseUpEvent, phase, window, cx| {
                        if phase.bubble() {
                            on(Act::Seek(at(e.position.x), true), window, cx);
                        }
                    });
                },
            )
            .size_full(),
        )
}

#[cfg(feature = "video")]
#[path = "../../../src-tauri/src/video.rs"]
mod encode;

#[cfg(feature = "video")]
pub use encode::{Quality, compress};

/// Hardware H.264 only: a system FFmpeg's own `mpeg4` encoder makes clips phones play badly.
#[cfg(feature = "video")]
pub fn available() -> bool {
    static OK: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *OK.get_or_init(|| {
        let test = std::env::var_os("VECTOR_VIDEO_TEST_ENCODER").is_some();
        encode::available()
            && (test
                || ["h264_videotoolbox", "h264_mediacodec", "h264_mf"]
                    .iter()
                    .any(|n| ffmpeg_next::encoder::find_by_name(n).is_some()))
    })
}

#[cfg(not(feature = "video"))]
pub fn available() -> bool {
    false
}
