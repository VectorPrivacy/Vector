//! Audio attachments on the shared engine (`AudioPlayer.svelte`, `AudioSession`): play and
//! pause, the spectral bars, the scrub, and one player sounding at a time.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::OnceLock;
use std::time::Instant;

use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use gpui::{
    AnyElement, App, Bounds, Corners, CursorStyle, HitboxBehavior, InteractiveElement,
    IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement,
    Pixels, SharedString, StatefulInteractiveElement, Styled, Window, canvas, div, fill, point,
    prelude::*, px, size, svg,
};
use vector_media::audio_engine::{AudioEngine, AudioInput};

use crate::theme::*;

pub const EXTENSIONS: &[&str] = &["wav", "mp3", "flac", "aac", "m4a", "ogg"];

/// A bar at rest: this short, lifted this far.
const REST_SCALE: f32 = 0.15;
const REST_Y: f32 = -9.;

static EVENTS: OnceLock<UnboundedSender<(String, serde_json::Value)>> = OnceLock::new();

/// The engine's own events (`audio_waveform`, `audio_ended`, `audio_duration`), for the UI.
pub fn engine_event(event: &str, payload: serde_json::Value) {
    if let Some(tx) = EVENTS.get() {
        let _ = tx.unbounded_send((event.to_string(), payload));
    }
}

pub fn listen() -> Option<UnboundedReceiver<(String, serde_json::Value)>> {
    let (tx, rx) = unbounded();
    EVENTS.set(tx).ok().map(|_| rx)
}

pub fn engine() -> Result<&'static AudioEngine, String> {
    AudioEngine::init();
    AudioEngine::get()
}

struct Wave {
    data: Vec<u8>,
    fps: f32,
    bins: usize,
}

#[derive(Clone, Copy, PartialEq)]
enum Look {
    /// Never played: the stylesheet's resting dots.
    Fresh,
    Live,
    /// Stopped: the bars settle, the played part brighter.
    Settling,
    Rest,
}

pub struct Player {
    pub msg: String,
    path: Option<AudioInputSpec>,
    source: Option<u32>,
    pub loading: bool,
    pub playing: bool,
    paused_at: u64,
    start_pos: u64,
    started: Option<Instant>,
    pub duration: u64,
    wave: Option<Wave>,
    display: Vec<f32>,
    lift: f32,
    look: Look,
    last_seek: Option<Instant>,
}

/// Where the audio is, kept so a load can run off the UI thread.
#[derive(Clone)]
pub enum AudioInputSpec {
    File(std::path::PathBuf),
    Held([u8; 32]),
}

impl AudioInputSpec {
    fn open(&self) -> Result<AudioInput, String> {
        match self {
            AudioInputSpec::File(p) => {
                if p.exists() {
                    Ok(AudioInput::File(p.clone()))
                } else {
                    Err("Audio file not found".into())
                }
            }
            AudioInputSpec::Held(h) => vector_core::media::open(h)
                .map(AudioInput::Held)
                .ok_or_else(|| "Audio file not found".into()),
        }
    }
}

impl Player {
    fn new(msg: &str) -> Self {
        Player {
            msg: msg.to_string(),
            path: None,
            source: None,
            loading: false,
            playing: false,
            paused_at: 0,
            start_pos: 0,
            started: None,
            duration: 0,
            wave: None,
            display: Vec::new(),
            lift: REST_Y,
            look: Look::Fresh,
            last_seek: None,
        }
    }

    pub fn position(&self) -> u64 {
        if !self.playing {
            return self.paused_at;
        }
        let p = self.start_pos + self.started.map_or(0, |s| s.elapsed().as_millis() as u64);
        if self.duration > 0 { p.min(self.duration) } else { p }
    }

    fn progress(&self) -> f32 {
        if self.duration == 0 {
            return 0.;
        }
        self.position() as f32 / self.duration as f32
    }
}

#[derive(Default)]
pub struct Audio {
    players: HashMap<String, Player>,
    /// Durations read from the file's header, before anything plays.
    probed: HashMap<String, u64>,
    probing: HashSet<String>,
    sources: HashMap<u32, String>,
    active: Option<String>,
    /// Work a draw asked for, for the app to run.
    pub queue: Vec<Work>,
}

#[derive(Clone, Copy, Debug)]
pub enum Act {
    Toggle,
    /// A scrub to this fraction; `done` on release.
    Seek(f32, bool),
}

/// What the app does for the audio state: the work that needs the engine or a thread.
pub enum Work {
    Probe(String, AudioInputSpec),
    Load(String, AudioInputSpec),
}

impl Audio {
    pub fn duration(&self, att: &str) -> u64 {
        self.players
            .get(att)
            .map(|p| p.duration)
            .filter(|d| *d > 0)
            .or_else(|| self.probed.get(att).copied())
            .unwrap_or(0)
    }

    /// A probe for the header's duration, once per file.
    pub fn want_probe(&mut self, att: &str, input: &AudioInputSpec) {
        if !self.probed.contains_key(att) && self.probing.insert(att.to_string()) {
            self.queue.push(Work::Probe(att.to_string(), input.clone()));
        }
    }

    pub fn probed(&mut self, att: String, ms: u64) {
        self.probing.remove(&att);
        self.probed.insert(att, ms);
    }

    pub fn player(&self, att: &str) -> Option<&Player> {
        self.players.get(att)
    }

    pub fn act(&mut self, msg: &str, att: &str, input: AudioInputSpec, act: Act) -> Option<Work> {
        match act {
            Act::Toggle => {
                if self.players.get(att).is_some_and(|p| p.playing || p.loading) {
                    self.pause(att);
                    None
                } else {
                    self.play(msg, att, input)
                }
            }
            Act::Seek(frac, done) => {
                let probed = self.probed.get(att).copied().unwrap_or(0);
                let p = self
                    .players
                    .entry(att.to_string())
                    .or_insert_with(|| Player::new(msg));
                if p.duration == 0 {
                    p.duration = probed;
                }
                if p.duration == 0 {
                    return None;
                }
                let ms = (frac.clamp(0., 1.) * p.duration as f32) as u64;
                if p.playing {
                    p.start_pos = ms;
                    p.started = Some(Instant::now());
                } else {
                    p.paused_at = ms;
                    if p.look == Look::Fresh {
                        p.look = Look::Rest;
                    }
                }
                let due = done || p.last_seek.is_none_or(|t| t.elapsed().as_millis() >= 50);
                if due && let (Some(id), Ok(e)) = (p.source, engine()) {
                    let _ = e.seek(id, ms);
                    p.last_seek = Some(Instant::now());
                }
                if done {
                    p.last_seek = None;
                }
                None
            }
        }
    }

    fn claim(&mut self, att: &str) {
        if let Some(prev) = self.active.replace(att.to_string())
            && prev != att
        {
            self.pause(&prev);
        }
    }

    pub fn play(&mut self, msg: &str, att: &str, input: AudioInputSpec) -> Option<Work> {
        self.claim(att);
        let probed = self.probed.get(att).copied().unwrap_or(0);
        let p = self
            .players
            .entry(att.to_string())
            .or_insert_with(|| Player::new(msg));
        if p.duration == 0 {
            p.duration = probed;
        }
        p.path = Some(input.clone());
        match p.source {
            None => {
                p.loading = true;
                Some(Work::Load(att.to_string(), input))
            }
            Some(id) => {
                let Ok(e) = engine() else { return None };
                match e.play(id) {
                    Ok(pos) => {
                        p.start_pos = pos;
                        p.started = Some(Instant::now());
                        p.playing = true;
                        p.look = Look::Live;
                        None
                    }
                    // The engine evicts paused sources past its cap: load it again where it was.
                    Err(_) => {
                        self.sources.remove(&id);
                        p.source = None;
                        p.loading = true;
                        Some(Work::Load(att.to_string(), input))
                    }
                }
            }
        }
    }

    /// A load landed: start it, unless it was called off meanwhile.
    pub fn loaded(&mut self, att: &str, res: Result<(u32, u64), String>) {
        let still = self.active.as_deref() == Some(att);
        let Some(p) = self.players.get_mut(att) else {
            return;
        };
        let was_loading = std::mem::replace(&mut p.loading, false);
        let Ok((id, duration)) = res else {
            if still {
                self.active = None;
            }
            return;
        };
        p.source = Some(id);
        self.sources.insert(id, att.to_string());
        if duration > 0 {
            p.duration = duration;
        }
        if !was_loading || !still {
            return;
        }
        let Ok(e) = engine() else { return };
        if p.paused_at > 0 {
            let _ = e.seek(id, p.paused_at);
        }
        if let Ok(pos) = e.play(id) {
            p.start_pos = pos;
            p.started = Some(Instant::now());
            p.playing = true;
            p.look = Look::Live;
        }
    }

    pub fn pause(&mut self, att: &str) {
        if self.active.as_deref() == Some(att) {
            self.active = None;
        }
        let Some(p) = self.players.get_mut(att) else {
            return;
        };
        p.loading = false;
        if !p.playing {
            return;
        }
        p.paused_at = p.position();
        p.playing = false;
        p.look = Look::Settling;
        if let (Some(id), Ok(e)) = (p.source, engine()) {
            let _ = e.pause(id);
        }
    }

    /// An engine event; returns the attachment that just ended.
    pub fn on_event(&mut self, name: &str, v: &serde_json::Value) -> Option<String> {
        let id = v.get("id").and_then(|i| i.as_u64())? as u32;
        let att = self.sources.get(&id)?.clone();
        let p = self.players.get_mut(&att)?;
        match name {
            "audio_waveform" => {
                use base64::Engine;
                let data = v
                    .get("waveform")
                    .and_then(|w| w.as_str())
                    .and_then(|s| base64::engine::general_purpose::STANDARD.decode(s).ok())?;
                p.wave = Some(Wave {
                    data,
                    fps: v.get("waveform_fps").and_then(|f| f.as_f64()).unwrap_or(30.) as f32,
                    bins: v.get("bins").and_then(|b| b.as_u64()).unwrap_or(64).max(1) as usize,
                });
                None
            }
            "audio_duration" => {
                if let Some(ms) = v.get("duration_ms").and_then(|d| d.as_u64()) {
                    p.duration = ms;
                }
                None
            }
            "audio_ended" => {
                p.playing = false;
                p.paused_at = 0;
                p.look = Look::Settling;
                if self.active.as_deref() == Some(att.as_str()) {
                    self.active = None;
                }
                Some(att)
            }
            _ => None,
        }
    }

    /// One frame of the bars; true while any still moves.
    pub fn tick(&mut self) -> bool {
        let mut moving = false;
        for p in self.players.values_mut() {
            match p.look {
                Look::Live => {
                    moving = true;
                    let Some(w) = &p.wave else { continue };
                    let pos = p.position();
                    let offset = ((pos as f32 / 1000.) * w.fps) as usize * w.bins;
                    p.display.resize(w.bins, 0.);
                    p.lift *= 0.92;
                    for i in 0..w.bins {
                        let v = w.data.get(offset + i).map_or(0., |b| *b as f32 / 255.);
                        p.display[i] = smooth(p.display[i], v);
                    }
                }
                Look::Settling => {
                    p.lift = p.lift * 0.92 + REST_Y * 0.08;
                    let mut settled = (p.lift - REST_Y).abs() < 0.2;
                    for d in &mut p.display {
                        *d *= 0.94;
                        if *d > REST_SCALE {
                            settled = false;
                        }
                    }
                    if settled {
                        p.lift = REST_Y;
                        p.look = Look::Rest;
                    } else {
                        moving = true;
                    }
                }
                _ => {}
            }
        }
        moving
    }

    pub fn dispose_all(&mut self) {
        if let Ok(e) = engine() {
            for id in self.sources.keys() {
                let _ = e.stop(*id);
            }
        }
        self.sources.clear();
        self.players.clear();
        self.active = None;
    }
}

fn smooth(prev: f32, level: f32) -> f32 {
    let target = (level * level.sqrt()).min(1.);
    if target > prev { prev * 0.7 + target * 0.3 } else { prev * 0.85 + target * 0.15 }
}

/// Runs the engine work a player asked for.
pub fn run(work: Work) -> futures::channel::oneshot::Receiver<WorkDone> {
    let (tx, rx) = futures::channel::oneshot::channel();
    std::thread::spawn(move || {
        let done = match work {
            Work::Probe(att, input) => WorkDone::Probed(
                att,
                input
                    .open()
                    .and_then(|i| vector_media::audio_engine::probe_duration(&i))
                    .unwrap_or(0),
            ),
            Work::Load(att, input) => WorkDone::Loaded(
                att,
                input
                    .open()
                    .and_then(|i| engine()?.load_from_file(&i))
                    .map(|r| (r.id, r.duration_ms)),
            ),
        };
        let _ = tx.send(done);
    });
    rx
}

pub enum WorkDone {
    Probed(String, u64),
    Loaded(String, Result<(u32, u64), String>),
}

pub fn format_time(ms: u64) -> String {
    let s = ms / 1000;
    format!("{}:{:02}", s / 60, s % 60)
}

/// The bars, painted per frame, and the scrub across them.
fn waveform(
    id: SharedString,
    p: Option<&Player>,
    duration: u64,
    on_seek: Rc<dyn Fn(f32, bool, &mut Window, &mut App)>,
) -> impl IntoElement {
    let look = p.map_or(Look::Fresh, |p| p.look);
    let playing = p.is_some_and(|p| p.playing);
    let progress = p.map_or(0., |p| p.progress());
    let at_zero = p.is_none_or(|p| p.position() == 0);
    let lift = p.map_or(REST_Y, |p| p.lift);
    let display: Rc<[f32]> = p.map_or_else(|| Rc::from([]), |p| Rc::from(p.display.as_slice()));
    let has_wave = p.is_some_and(|p| p.wave.is_some());
    let seekable = duration > 0;
    div()
        .id(id)
        .group("audio-waveform")
        .flex_1()
        .h(px(30.))
        .mx(px(8.))
        .child(
            canvas(
                move |bounds, window, _| {
                    window.insert_hitbox(bounds, HitboxBehavior::Normal)
                },
                move |bounds, hitbox, window, _| {
                    let w = f32::from(bounds.size.width);
                    let n = ((w / 5.).floor() as usize).clamp(16, 64);
                    let gap = 2.;
                    let bw = ((w - gap * (n - 1) as f32) / n as f32).max(2.);
                    let hovered = hitbox.is_hovered(window);
                    let group = if playing || hovered { 1. } else { 0.8 };
                    let bins = display.len().max(1);
                    for i in 0..n {
                        let at = (i as f32 + 0.5) / n as f32;
                        let (scale, y, opacity, glow) = match look {
                            Look::Fresh => (REST_SCALE, REST_Y, 1., 0.),
                            Look::Live if !has_wave => (
                                0.25,
                                -7.,
                                if at <= progress { 0.5 } else { 0.2 },
                                0.,
                            ),
                            Look::Live => {
                                let v = display.get(i * bins / n).copied().unwrap_or(0.);
                                let played = at <= progress;
                                let o = (0.3 + v * 0.7) * if played { 1. } else { 0.4 };
                                let glow = if v > 0.7 && played { v - 0.7 } else { 0. };
                                (v.max(REST_SCALE), lift, o, glow)
                            }
                            Look::Settling | Look::Rest => {
                                let v = display.get(i * bins / n).copied().unwrap_or(0.);
                                let o = if at_zero || at <= progress { 0.3 } else { 0.15 };
                                (v.max(REST_SCALE), lift, o, 0.)
                            }
                        };
                        let y = if y.abs() > 0.5 { y } else { 0. };
                        let h = 20. * scale;
                        let x = f32::from(bounds.origin.x) + i as f32 * (bw + gap);
                        let bottom = f32::from(bounds.origin.y) + 25. + y;
                        let rect = Bounds::new(point(px(x), px(bottom - h)), size(px(bw), px(h)));
                        let color = c(PRIMARY);
                        if glow > 0. {
                            let r = glow * 8.;
                            window.paint_quad(
                                fill(
                                    Bounds::new(
                                        point(px(x - r / 2.), px(bottom - h - r / 2.)),
                                        size(px(bw + r), px(h + r)),
                                    ),
                                    color.opacity(0.25 * opacity * group),
                                )
                                .corner_radii(Corners::all(px(1.5 + r / 2.))),
                            );
                        }
                        window.paint_quad(
                            fill(rect, color.opacity(opacity * group))
                                .corner_radii(Corners::all(px(1.5_f32.min(h / 2.)))),
                        );
                    }
                    if !seekable {
                        return;
                    }
                    window.set_cursor_style(CursorStyle::PointingHand, &hitbox);
                    let frac = move |x: Pixels| {
                        f32::from(x - bounds.origin.x) / f32::from(bounds.size.width).max(1.)
                    };
                    let dragging = Rc::new(Cell::new(false));
                    {
                        let (dragging, on_seek, hitbox) =
                            (dragging.clone(), on_seek.clone(), hitbox.clone());
                        window.on_mouse_event(move |e: &MouseDownEvent, phase, window, cx| {
                            if phase.bubble()
                                && e.button == MouseButton::Left
                                && hitbox.is_hovered(window)
                            {
                                dragging.set(true);
                                cx.stop_propagation();
                                on_seek(frac(e.position.x), false, window, cx);
                            }
                        });
                    }
                    {
                        let (dragging, on_seek) = (dragging.clone(), on_seek.clone());
                        window.on_mouse_event(move |e: &MouseMoveEvent, phase, window, cx| {
                            if phase.bubble() && dragging.get() && e.dragging() {
                                on_seek(frac(e.position.x), false, window, cx);
                            }
                        });
                    }
                    window.on_mouse_event(move |e: &MouseUpEvent, phase, window, cx| {
                        if phase.bubble() && dragging.replace(false) {
                            on_seek(frac(e.position.x), true, window, cx);
                        }
                    });
                },
            )
            .size_full(),
        )
}

/// `.custom-audio-player`: a voice message, or a named clip with its name over the bars.
pub fn view(
    att: &vector_core::Attachment,
    p: Option<&Player>,
    duration: u64,
    on: Rc<dyn Fn(Act, &mut Window, &mut App)>,
) -> AnyElement {
    let playing = p.is_some_and(|p| p.playing);
    let loading = p.is_some_and(|p| p.loading);
    let pos = p.map_or(0, |p| p.position()).min(duration.max(1));
    let pos = if duration == 0 { p.map_or(0, |p| p.position()) } else { pos };
    let id = att.id.clone();
    let toggle = on.clone();
    let seek: Rc<dyn Fn(f32, bool, &mut Window, &mut App)> =
        Rc::new(move |f, done, w, cx| on(Act::Seek(f, done), w, cx));
    let icon = if loading {
        "icons/loading.svg"
    } else if playing {
        "icons/pause.svg"
    } else {
        "icons/play.svg"
    };
    let button = div()
        .id(SharedString::from(format!("audio-play-{id}")))
        .size(px(40.))
        .flex_shrink_0()
        .rounded_full()
        .bg(c(PRIMARY))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .hover(|s| s.opacity(0.93))
        .on_click(move |_, w, cx| {
            cx.stop_propagation();
            toggle(Act::Toggle, w, cx)
        })
        .child(
            svg()
                .path(icon)
                .size(px(28.))
                .when(!playing && !loading, |s| s.ml(px(1.4)))
                .text_color(c(0x0a0a0a)),
        );
    let bars = waveform(
        SharedString::from(format!("audio-wave-{id}")),
        p,
        duration,
        seek,
    );
    let middle = if att.name.is_empty() {
        bars.into_any_element()
    } else {
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .gap(px(2.))
            .mx(px(8.))
            .child(
                div()
                    .text_size(px(12.8))
                    .line_height(px(16.))
                    .text_color(white_a(0.92))
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .child(att.name.clone()),
            )
            .child(div().flex().mx(px(-8.)).child(bars))
            .into_any_element()
    };
    let time = div()
        .min_w(px(72.))
        .flex()
        .justify_end()
        .whitespace_nowrap()
        .text_size(px(11.52))
        .text_color(white_a(0.55))
        .child(
            div()
                .text_color(if pos > 0 { white_a(0.92) } else { white_a(0.34) })
                .child(format_time(pos)),
        )
        .child(div().mx(px(4.)).text_color(white_a(0.34)).child("/"))
        .child(format_time(duration));
    div()
        .id(SharedString::from(format!("audio-{id}")))
        .w(px(500.))
        .max_w_full()
        .min_w(px(240.))
        .px(px(12.))
        .py(px(10.))
        .bg(c(0x161616))
        .border_1()
        .border_color(c(0x262626))
        .rounded(px(12.))
        .hover(|s| s.bg(c(0x1d1d1d)).border_color(c(0x363636)))
        .child(
            div()
                .flex()
                .items_center()
                .child(button)
                .child(middle)
                .child(time),
        )
        .into_any_element()
}
