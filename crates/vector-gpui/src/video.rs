//! Video attachments (`VideoAttachment.svelte`): FFmpeg decodes off the UI thread at the size
//! the clip draws, the audio engine sounds its track and keeps the time, one plays at a time.

#![cfg_attr(not(feature = "video"), allow(dead_code))]

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::kit::Tipped;
use gpui::{AnyElement, App, Bounds, CursorStyle, HitboxBehavior, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ObjectFit, Pixels, RenderImage, SharedString, Window, canvas, div, prelude::*, px, svg};
use crate::pic::img;
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
/// A wait on the network this short passes unseen: the spinner would only flicker.
const STARVE_GRACE: Duration = Duration::from_millis(150);

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
    /// The volume slider to this level; `done` on release.
    Volume(f32, bool),
    /// Fill the window, or leave it.
    Fullscreen,
    /// The next playback speed.
    Speed,
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
    /// Clip time per real time: the playback speed.
    rate: f32,
}

impl Clock {
    fn now(&self) -> u64 {
        self.anchor + self.at.map_or(0, |t| (t.elapsed().as_millis() as f32 * self.rate) as u64)
    }
}

/// The speeds a clip plays at, in the order the button steps through them.
const SPEEDS: [f32; 6] = [1., 1.25, 1.5, 2., 0.5, 0.75];

/// A clip time in the stretched track the engine plays.
fn on_track(ms: u64, speed: f32) -> u64 {
    (ms as f32 / speed) as u64
}

/// The decoded track, mono, and its rate: kept once read so another speed needs no decode.
type Track = Arc<Mutex<Option<Arc<(Vec<f32>, u32)>>>>;

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
    /// The speed the engine's track was stretched for.
    speed: f32,
    track: Track,
    /// The track would not load: the picture goes on alone.
    silent: bool,
    heard: Option<u64>,
    ended: bool,
    /// A web video still downloading: playback waits on it.
    net: Option<Arc<crate::backend::stream::Stream<crate::backend::stream::Web>>>,
    /// Paused on an empty buffer, to go on once a little is back.
    rebuffering: bool,
    /// Restarting after a seek, which wants less in hand than a first play.
    resumed: bool,
    /// Since when the picture's reader has been waiting, before the clock holds for it.
    starved: Option<Instant>,
}

impl Drop for Play {
    fn drop(&mut self) {
        let _ = self.cmd.send(Cmd::Stop);
        if let (Some(id), Ok(e)) = (self.source, crate::audio::engine()) {
            let _ = e.stop(id);
        }
    }
}

pub struct Clip {
    pub info: Option<Info>,
    play: Option<Play>,
    pub playing: bool,
    pub loading: bool,
    position: u64,
    frame: Option<Arc<RenderImage>>,
    pub muted: bool,
    /// The level it sounds at when not muted.
    pub volume: f32,
    /// How fast it plays, the pitch kept.
    pub speed: f32,
    /// The volume slider is held.
    leveling: bool,
    /// Decoded for the whole window rather than its row.
    big: bool,
    hovered: bool,
    moved: Option<Instant>,
    scrub: Option<f32>,
    /// Since when it has sat paused or ended with its decoder held.
    still: Option<Instant>,
    /// Since when it has been loading: a wait too short to notice shows no spinner.
    waiting: Option<Instant>,
}

impl Default for Clip {
    fn default() -> Self {
        Self {
            info: None,
            play: None,
            playing: false,
            loading: false,
            position: 0,
            frame: None,
            muted: false,
            volume: 1.,
            speed: 1.,
            leveling: false,
            big: false,
            hovered: false,
            moved: None,
            scrub: None,
            still: None,
            waiting: None,
        }
    }
}

impl Clip {
    /// How far the loading spinner has faded in.
    pub fn spinner(&self) -> f32 {
        match self.waiting {
            Some(t) if self.loading => ((t.elapsed().as_secs_f32() - SPIN_AFTER) / SPIN_FADE).clamp(0., 1.),
            _ => 0.,
        }
    }

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

    /// What the engine is told to sound at.
    fn gain(&self) -> f32 {
        if self.muted { 0. } else { self.volume }
    }

    fn controls(&self) -> bool {
        !self.playing || self.scrub.is_some() || self.leveling || (self.hovered && self.moved.is_some_and(|t| t.elapsed() < IDLE))
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
    /// Retired frames out of the atlas, their buffers going back to the decoders once ours alone.
    spent: Vec<Arc<RenderImage>>,
    pub scale: f32,
    /// The clip filling the window, and where it reads from.
    pub full: Option<(String, Option<AudioInputSpec>)>,
    /// When each clip was last drawn.
    seen: HashMap<String, Instant>,
    /// Where a clip let go for memory stood, restored when it is probed again.
    resume: HashMap<String, Resume>,
    /// A poster landed since the media cache last looked.
    landed: bool,
    /// Clips whose probe failed, by how often they were probed again.
    retried: HashMap<String, u8>,
}

struct Resume {
    position: u64,
    muted: bool,
    volume: f32,
    speed: f32,
}

impl Videos {
    pub fn want_probe(&mut self, att: &str, input: &AudioInputSpec) {
        match self.seen.get_mut(att) {
            Some(at) => *at = Instant::now(),
            None => {
                self.seen.insert(att.to_string(), Instant::now());
            }
        }
        if self.clips.contains_key(att) || self.failed.contains(att) || !self.probing.insert(att.to_string()) {
            return;
        }
        self.queue.push(Work::Probe(att.to_string(), input.clone(), self.scale.max(1.)));
    }

    pub fn probed(&mut self, att: String, info: Option<Info>) {
        self.probing.remove(&att);
        match info {
            Some(i) => {
                self.landed = true;
                let back = self.resume.remove(&att);
                let c = self.clips.entry(att).or_default();
                c.info = Some(i);
                if let Some(r) = back {
                    (c.position, c.muted, c.volume, c.speed) = (r.position, r.muted, r.volume, r.speed);
                }
            }
            // Bytes let go meanwhile: the next sight probes again, a couple of times.
            None => {
                let tries = self.retried.entry(att.clone()).or_default();
                if *tries < 2 {
                    *tries += 1;
                } else {
                    self.failed.insert(att);
                }
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
                if c.scrub.take().is_none() {
                    return false;
                }
                let ms = (f * c.info.as_ref().map_or(0, |i| i.duration) as f32) as u64;
                seek(c, ms, input, scale);
                false
            }
            Act::Mute => {
                if let Some(c) = self.clips.get_mut(att) {
                    c.muted = !c.muted;
                    if !c.muted && c.volume <= 0.01 {
                        c.volume = 1.;
                    }
                    if let (Some(id), Ok(e)) = (c.play.as_ref().and_then(|p| p.source), crate::audio::engine()) {
                        let _ = e.set_volume(id, c.gain());
                    }
                }
                false
            }
            Act::Volume(v, done) => {
                if let Some(c) = self.clips.get_mut(att)
                    && (!done || c.leveling)
                {
                    c.volume = v.clamp(0., 1.);
                    c.muted = c.volume <= 0.01;
                    c.leveling = !done;
                    if let (Some(id), Ok(e)) = (c.play.as_ref().and_then(|p| p.source), crate::audio::engine()) {
                        let _ = e.set_volume(id, c.gain());
                    }
                }
                false
            }
            Act::Speed => {
                if let Some(c) = self.clips.get_mut(att) {
                    let at = SPEEDS.iter().position(|s| (s - c.speed).abs() < 1e-3).unwrap_or(0);
                    c.speed = SPEEDS[(at + 1) % SPEEDS.len()];
                    respeed(c, input);
                }
                false
            }
            Act::Fullscreen => {
                let leaving = self.full.as_ref().is_some_and(|(a, _)| a == att);
                let input = input.or_else(|| self.full.as_ref().filter(|(a, _)| a == att).and_then(|(_, i)| i.clone()));
                if let Some((prev, prev_input)) = self.full.take()
                    && prev != att
                {
                    self.redecode(&prev, prev_input, false);
                }
                if !leaving {
                    self.full = Some((att.to_string(), input.clone()));
                }
                self.redecode(att, input, !leaving);
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

    /// Decoded again for the window (`big`) or its row, carrying on from where it is.
    fn redecode(&mut self, att: &str, input: Option<AudioInputSpec>, big: bool) {
        let scale = self.scale.max(1.);
        let Some(c) = self.clips.get_mut(att) else { return };
        if c.big == big {
            return;
        }
        c.big = big;
        if c.play.is_none() {
            return;
        }
        let (at, playing) = (c.position(), c.playing || c.loading);
        c.play = None;
        c.position = at;
        c.playing = false;
        c.loading = false;
        if let Some(input) = input {
            if playing {
                self.play(att, input, scale);
            } else if let Some(c) = self.clips.get_mut(att) {
                seek(c, at, Some(input), scale);
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
            // Not yet under way: the start gate runs the clock and the sound together.
            Some(p) if !p.started => c.loading = true,
            Some(p) => {
                p.clock = Clock { anchor: c.position, at: Some(Instant::now()), rate: p.speed };
                if let (Some(id), Ok(e)) = (p.source, crate::audio::engine()) {
                    let _ = e.seek(id, on_track(c.position, p.speed));
                    let _ = e.play(id);
                }
            }
            None => {
                c.loading = true;
                c.play = start(input, &info, c.position, scale, c.big, c.speed);
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
            p.rebuffering = false;
            p.clock = Clock { anchor: c.position, at: None, rate: p.speed };
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

    /// Decoders of clips paused or ended for [`LET_GO`] dropped, and the frame buffers kept for
    /// playback too once nothing plays.
    pub fn let_go(&mut self) -> bool {
        let mut any = false;
        for c in self.clips.values_mut() {
            let resting = !c.playing && !c.loading && c.scrub.is_none() && !c.big && c.play.as_ref().is_some_and(|p| !p.fresh);
            if !resting {
                c.still = None;
            } else if c.still.get_or_insert_with(Instant::now).elapsed() >= LET_GO {
                c.still = None;
                c.play = None;
                any = true;
            }
        }
        if !self.clips.values().any(|c| c.playing || c.loading) {
            let mut pool = BUFFERS.lock().unwrap_or_else(|e| e.into_inner());
            any |= !pool.is_empty();
            pool.clear();
        }
        any
    }

    /// Dropped with its frames, so the next sight of it probes its source afresh.
    pub fn forget(&mut self, att: &str) {
        self.stop(att);
        self.failed.remove(att);
        self.seen.remove(att);
        self.resume.remove(att);
        if let Some(c) = self.clips.remove(att) {
            self.retired.extend(c.frame);
            self.retired.extend(c.info.and_then(|i| i.poster));
        }
    }

    /// Clips at rest (stopped, not filling the window), when each was last drawn, and the bytes
    /// their pictures hold.
    pub fn at_rest(&self) -> Vec<(String, Instant, usize)> {
        let full = self.full.as_ref().map(|(a, _)| a.as_str());
        self.clips
            .iter()
            .filter(|(a, c)| {
                c.play.is_none() && !c.playing && !c.loading && c.scrub.is_none() && Some(a.as_str()) != full && self.active.as_deref() != Some(a.as_str())
            })
            .filter_map(|(a, c)| {
                let bytes = c.frame.iter().chain(c.info.as_ref().and_then(|i| i.poster.as_ref())).map(|i| crate::media::image_bytes(i)).sum();
                Some((a.clone(), *self.seen.get(a)?, bytes))
            })
            .collect()
    }

    pub fn take_landed(&mut self) -> bool {
        std::mem::take(&mut self.landed)
    }

    /// The bytes every clip's pictures hold.
    pub fn held(&self) -> usize {
        self.clips
            .values()
            .flat_map(|c| c.frame.iter().chain(c.info.as_ref().and_then(|i| i.poster.as_ref())))
            .map(|i| crate::media::image_bytes(i))
            .sum()
    }

    /// A clip at rest let go with its pictures, keeping where it stood for its next sight.
    pub fn shed(&mut self, att: &str) {
        self.seen.remove(att);
        let Some(c) = self.clips.remove(att) else { return };
        if c.position > 0 || c.muted || c.volume != 1. || c.speed != 1. {
            self.resume.insert(att.to_string(), Resume { position: c.position, muted: c.muted, volume: c.volume, speed: c.speed });
        }
        self.retired.extend(c.frame);
        self.retired.extend(c.info.and_then(|i| i.poster));
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
            c.waiting = if c.loading { c.waiting.or_else(|| Some(Instant::now())) } else { None };
            let gain = c.gain();
            let Some(p) = &mut c.play else { continue };
            let duration = c.info.as_ref().map_or(0, |i| i.duration);
            if p.source.is_none()
                && let Some(res) = p.audio.lock().ok().and_then(|mut s| s.take())
            {
                p.source = res.ok();
                p.silent = p.source.is_none();
                if let (Some(id), Ok(e)) = (p.source, crate::audio::engine()) {
                    let _ = e.set_volume(id, gain);
                    let _ = e.seek(id, on_track(c.position, p.speed));
                }
            }
            let audio_ready = c.info.as_ref().is_none_or(|i| !i.audio) || p.source.is_some() || p.silent;
            // A stream starts with a few seconds in hand, and holds (spinner up) when they run out.
            let buffered = |secs: f64| p.net.as_ref().is_none_or(|s| s.error().is_some() || s.complete_ahead() || s.ahead_secs().is_some_and(|a| a >= secs));
            if let Some(e) = p.net.as_ref().and_then(|s| s.error()) {
                log::warn!("[video] stream: {e}");
                c.playing = false;
                c.loading = false;
            }
            let starving = p.net.as_ref().is_some_and(|s| s.starving());
            let since = *p.starved.get_or_insert_with(Instant::now);
            if !starving {
                p.starved = None;
            }
            if c.playing && p.started && !p.rebuffering && starving && since.elapsed() >= STARVE_GRACE {
                p.rebuffering = true;
                c.loading = true;
                p.clock = Clock { anchor: p.clock.now(), at: None, rate: p.speed };
                if let (Some(id), Ok(e)) = (p.source, crate::audio::engine()) {
                    let _ = e.pause(id);
                }
            } else if p.rebuffering && !starving && buffered(1.) {
                p.rebuffering = false;
                c.loading = false;
                p.clock = Clock { anchor: p.clock.now(), at: Some(Instant::now()), rate: p.speed };
                if let (Some(id), Ok(e)) = (p.source, crate::audio::engine()) {
                    let _ = e.play(id);
                }
            }
            // Wait for a picture and the sound before the clock starts.
            if c.playing && !p.started && !p.fresh && audio_ready && buffered(if p.resumed { 1. } else { 2.5 }) {
                p.started = true;
                p.resumed = false;
                c.loading = false;
                p.clock = Clock { anchor: c.position, at: Some(Instant::now()), rate: p.speed };
                if let (Some(id), Ok(e)) = (p.source, crate::audio::engine()) {
                    let _ = e.play(id);
                }
            }
            if c.playing && p.started && let (Some(id), Ok(e)) = (p.source, crate::audio::engine()) {
                // Only a moving engine clock leads: a device that is not sounding holds still.
                if let Ok(heard) = e.get_position(id)
                    && p.heard.replace(heard).is_some_and(|prev| prev != heard)
                {
                    let a = (heard as f32 * p.speed) as u64;
                    let v = p.clock.now();
                    if a + 100 < duration && (a as i64 - v as i64).abs() > DRIFT_MS {
                        p.clock = Clock { anchor: a, at: Some(Instant::now()), rate: p.speed };
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
                p.clock = Clock { anchor: duration, at: None, rate: p.speed };
                p.ended = true;
                if let (Some(id), Ok(e)) = (p.source, crate::audio::engine()) {
                    let _ = e.pause(id);
                }
            }
            if c.playing || c.loading || p.fresh {
                moving = true;
            }
        }
        if self.let_go() {
            crate::trim::soon();
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
        // A stream starts over from the new spot: the clock waits for its bytes like a first play.
        let wait = p.net.as_ref().inspect(|s| s.sought()).is_some() && c.playing;
        if wait {
            p.started = false;
            p.rebuffering = false;
            p.resumed = true;
            c.loading = true;
        }
        p.clock = Clock { anchor: ms, at: (c.playing && !wait).then(Instant::now), rate: p.speed };
        if let (Some(id), Ok(e)) = (p.source, crate::audio::engine()) {
            let _ = e.seek(id, on_track(ms, p.speed));
            if wait {
                let _ = e.pause(id);
            }
        }
    } else if let (Some(info), Some(input)) = (&c.info, input) {
        // Never played: a decoder for the frame there, kept for the play that follows.
        c.play = start(input, info, ms, scale, c.big, c.speed);
    }
}

/// Seconds of loading before its spinner shows, and then fades in over.
const SPIN_AFTER: f32 = 0.3;
const SPIN_FADE: f32 = 0.15;

/// The box a window-filling clip decodes for: a large display's, never past the source.
const BIG: (f32, f32) = (2560., 1440.);

/// Another speed while playing: the clock carries on from the same moment, and the sound waits
/// for its track stretched to match, the picture holding with it.
fn respeed(c: &mut Clip, input: Option<AudioInputSpec>) {
    let at = c.position();
    let (speed, playing, audio) = (c.speed, c.playing, c.info.as_ref().is_some_and(|i| i.audio));
    let duration = c.info.as_ref().map_or(0, |i| i.duration);
    let Some(p) = &mut c.play else { return };
    if (p.speed - speed).abs() < 1e-3 {
        return;
    }
    p.speed = speed;
    c.position = at;
    p.clock = Clock { anchor: at, at: (playing && !audio).then(Instant::now), rate: speed };
    if !audio {
        return;
    }
    if let (Some(id), Ok(e)) = (p.source.take(), crate::audio::engine()) {
        let _ = e.stop(id);
    }
    p.silent = false;
    p.heard = None;
    p.started = false;
    c.loading = playing;
    p.audio = Arc::new(Mutex::new(None));
    let (slot, track) = (p.audio.clone(), p.track.clone());
    // spawn-detached: stretches samples in hand for the engine; owns no account state.
    std::thread::spawn(move || {
        let res = stretched(&track, input.as_ref(), speed, duration);
        if let Ok(mut s) = slot.lock() {
            *s = Some(res);
        }
    });
}

/// Past this, a clip's track is stretched as the engine reads it rather than whole first.
const STRETCH_WHOLE_MAX_MS: u64 = 30_000;

/// The track at `speed` in the engine. A web video's, or a long one's, is made as the engine
/// reads it; a short one's is read once (then kept), stretched and loaded.
fn stretched(track: &Track, input: Option<&AudioInputSpec>, speed: f32, duration: u64) -> Result<u32, String> {
    let net = matches!(input, Some(AudioInputSpec::Stream(_)));
    if let Some(spec) = input.filter(|_| net || duration > STRETCH_WHOLE_MAX_MS)
        && duration > 0
    {
        if (speed - 1.).abs() < 1e-3 {
            return spec.open().and_then(|i| load_audio(&i));
        }
        let wav = stretched::open(spec.open()?, speed, duration)?;
        return crate::audio::engine()?.load_from_file(&AudioInput::Held(wav)).map(|r| r.id);
    }
    let kept = track.lock().ok().and_then(|t| t.clone());
    let pcm = match kept {
        Some(t) => t,
        None => {
            let i = input.ok_or("no source")?.open()?; // i18n-exempt: developer error
            let t = Arc::new(decode_audio(&i)?);
            if let Ok(mut slot) = track.lock() {
                *slot = Some(t.clone());
            }
            t
        }
    };
    let e = crate::audio::engine()?;
    e.load_from_samples(crate::stretch::stretch(&pcm.0, pcm.1, speed), pcm.1).map(|r| r.id)
}

fn start(input: AudioInputSpec, info: &Info, from: u64, scale: f32, big: bool, speed: f32) -> Option<Play> {
    let (cmd_tx, cmd_rx) = std::sync::mpsc::channel();
    let (tx, rx) = std::sync::mpsc::sync_channel(QUEUE);
    let (w, h) = if big {
        let s = (BIG.0 / info.w.max(1) as f32).min(BIG.1 / info.h.max(1) as f32).min(1.);
        // Already the source's pixels: the scale below must not shrink them again.
        ((info.w as f32 * s) / scale, (info.h as f32 * s) / scale)
    } else {
        fit(info.w, info.h)
    };
    let target = (((w * scale) as u32).min(info.w).max(1), ((h * scale) as u32).min(info.h).max(1));
    let slot = Arc::new(Mutex::new(None));
    let track: Track = Arc::default();
    let net = match &input {
        AudioInputSpec::Stream(s) => Some(s.clone()),
        _ => None,
    };
    if info.audio {
        let (slot, input, track, duration) = (slot.clone(), input.clone(), track.clone(), info.duration);
        // spawn-detached: decodes bytes in hand into the engine; owns no account state.
        std::thread::spawn(move || {
            let res = if (speed - 1.).abs() < 1e-3 {
                input.open().and_then(|i| load_audio(&i))
            } else {
                stretched(&track, Some(&input), speed, duration)
            };
            if let Ok(mut s) = slot.lock() {
                *s = Some(res);
            }
        });
    }
    // spawn-detached: the decoder reads one file and answers only its channel.
    let decoder = std::thread::Builder::new()
        .name("video-decode".into())
        .spawn(move || {
            if let Ok(i) = input.open() {
                let _ = decode(i, target, from, cmd_rx, tx);
            }
        })
        .ok()?;
    // The picture's reader leads the stream: the sound's can sit lower in the file yet ahead.
    if let Some(n) = &net {
        n.lead(decoder.thread().id());
    }
    Some(Play {
        cmd: cmd_tx,
        frames: rx,
        next: None,
        epoch: 0,
        fresh: true,
        clock: Clock { anchor: from, at: None, rate: speed },
        started: false,
        audio: slot,
        source: None,
        speed,
        track,
        silent: false,
        heard: None,
        ended: false,
        net,
        rebuffering: false,
        resumed: false,
        starved: None,
    })
}

/// The track through the engine: streamed where it reads the container, decoded whole where not.
fn load_audio(input: &AudioInput) -> Result<u32, String> {
    let e = crate::audio::engine()?;
    let mp4 = input.extension().is_some_and(|x| matches!(x.as_str(), "mp4" | "mov" | "m4v"));
    // A stream decodes as it plays: reading all of it first would download the whole video.
    if (mp4 || input.is_remote()) && let Ok(r) = e.load_from_file(input) {
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
            Work::Probe(att, input, scale) => {
                let info = input.open().ok().and_then(|i| probe(i, scale));
                if let (AudioInputSpec::Stream(s), Some(i)) = (&input, &info) {
                    s.set_duration(i.duration);
                }
                WorkDone::Probed(att, info)
            }
        };
        let _ = tx.send(done);
    });
    rx
}

/// Decoded frames' buffers for the next ones: playback fills the same few instead of
/// allocating (and the allocator keeping) a fresh one per frame.
static BUFFERS: Mutex<Vec<Vec<u8>>> = Mutex::new(Vec::new());
const BUFFERS_KEPT: usize = 8;
/// A clip paused or ended this long lets its decoder go; the frame it shows stays.
const LET_GO: Duration = Duration::from_secs(15);

#[cfg_attr(not(feature = "video"), allow(dead_code))]
fn frame_buffer(len: usize) -> Vec<u8> {
    let mut pool = BUFFERS.lock().unwrap_or_else(|e| e.into_inner());
    match pool.iter().rposition(|b| b.len() == len) {
        Some(i) => pool.swap_remove(i),
        None => vec![0; len],
    }
}

impl Videos {
    #[cfg(feature = "devctl")]
    pub fn dev_clips(&self) -> serde_json::Value {
        serde_json::json!({
            "clips": self.clips.len(),
            "playing": self.clips.values().filter(|c| c.play.is_some()).count(),
            "spent": self.spent.len(),
            "pooled_buffers": BUFFERS.lock().map_or(0, |p| p.len()),
        })
    }

    /// A retired frame, after the app has handed it to `drop_image`.
    pub fn recycle(&mut self, frame: Arc<RenderImage>) {
        let mut pool = BUFFERS.lock().unwrap_or_else(|e| e.into_inner());
        // Nothing playing needs a buffer next: these go back to the allocator.
        if !self.clips.values().any(|c| c.playing || c.loading) {
            pool.clear();
            self.spent.clear();
            return;
        }
        self.spent.push(frame);
        let mut held = Vec::new();
        for frame in self.spent.drain(..) {
            match Arc::try_unwrap(frame) {
                Ok(image) => {
                    for buffer in image.into_buffers() {
                        // A new size (the window filled or left) never takes the old ones.
                        pool.retain(|b| b.len() == buffer.len());
                        if pool.len() >= BUFFERS_KEPT {
                            pool.remove(0);
                        }
                        pool.push(buffer);
                    }
                }
                Err(frame) => held.push(frame),
            }
        }
        // Still drawn: kept a few frames for its buffer, then left to whoever holds it.
        let over = held.len().saturating_sub(BUFFERS_KEPT);
        self.spent = held.split_off(over);
    }
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
        Err("no decoder".into()) // i18n-exempt: developer error
    }
    pub struct AudioReader;
    impl AudioReader {
        pub fn open(_: &AudioInput) -> Result<Self, String> {
            Err("no decoder".into()) // i18n-exempt: developer error
        }
        pub fn rate(&self) -> u32 {
            0
        }
        pub fn seek(&mut self, _: u64) {}
        pub fn next(&mut self, _: &mut Vec<f32>) -> Option<Option<f64>> {
            None
        }
    }
}

use ff::{decode, decode_audio, probe};

#[path = "video_stretched.rs"]
mod stretched;

// ── The player ──────────────────────────────────────────────────────────────────────────

pub type OnAct = Rc<dyn Fn(Act, &mut Window, &mut App)>;

/// The clip at its fitted size, its controls over it while paused or pointed at.
/// `radius`: the top corners' and the bottom corners'.
pub fn view(id: &str, clip: &Clip, dim: bool, size: Option<(f32, f32)>, radius: (f32, f32), on: OnAct) -> AnyElement {
    let (top, bottom) = (px(radius.0), px(radius.1));
    let info = clip.info.as_ref();
    let (w, h) = size.unwrap_or_else(|| info.map_or((MAX_W, MAX_H * 0.6), |i| fit(i.w, i.h)));
    let picture = clip.picture();
    let duration = info.map_or(0, |i| i.duration);
    let pos = clip.position().min(duration);
    let shown = clip.controls() && !dim;
    let frac = if duration > 0 { pos as f32 / duration as f32 } else { 0. };
    let playing = clip.playing || clip.loading;
    let (o1, o2, o3, o4, o5) = (on.clone(), on.clone(), on.clone(), on.clone(), on.clone());
    let (o6, o7, o8) = (on.clone(), on.clone(), on.clone());
    let speed_label = if (clip.speed - clip.speed.round()).abs() < 1e-3 {
        format!("{}×", clip.speed as u32)
    } else {
        format!("{}×", clip.speed)
    };
    let level = if clip.muted { 0. } else { clip.volume };
    let full = clip.big;
    let time: SharedString = format!("{} / {}", crate::audio::format_time(pos), crate::audio::format_time(duration)).into();
    let button = |icon: &'static str, key: &'static str, label: SharedString, act: Act, on: OnAct| {
        div()
            .id(SharedString::from(format!("{key}-{id}")))
            .aria_label(label)
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
                .keep_ltr()
                .child(button(
                    if playing { "icons/pause.svg" } else { "icons/play.svg" },
                    "vplay",
                    if playing { crate::t::audio_pause() } else { crate::t::audio_play() },
                    Act::Toggle,
                    o2,
                ))
                // A narrow clip gives its time's room to the buttons.
                .when(w >= 260., {
                    let time = time.clone();
                    move |d| d.child(
                        div()
                            .text_size(px(12.))
                            .text_color(gpui::white())
                            .flex_none()
                            .child(time),
                    )
                })
                .child(scrubber(id, frac, time.clone(), duration > 0, clip.scrub.is_some(), o3))
                .when(w >= 290., |d| {
                    d.child(
                        div()
                            .id(SharedString::from(format!("vspeed-{id}")))
                            .aria_label(crate::t::video_speed())
                            .aria_value(speed_label.clone())
                            .h(px(22.))
                            .min_w(px(34.))
                            .px(px(5.))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(11.))
                            .cursor_pointer()
                            .text_size(px(12.))
                            .text_color(gpui::white())
                            .hover(|s| s.bg(white_a(0.12)))
                            .on_click(move |_, w, cx| {
                                cx.stop_propagation();
                                o8(Act::Speed, w, cx)
                            })
                            .child(speed_label),
                    )
                })
                .child(button(
                    if clip.muted { "icons/volume-mute.svg" } else { "icons/volume-max.svg" },
                    "vmute",
                    if clip.muted { crate::t::video_unmute() } else { crate::t::video_mute() },
                    Act::Mute,
                    o4,
                ))
                // The seek bar keeps its room: the level, then the speed, then the time give way.
                .when(w >= 380. && info.is_some_and(|i| i.audio), |d| {
                    // Room for the knob, which overhangs the track's ends.
                    d.child(div().w(px(56.)).flex_none().h(px(20.)).mx(px(6.)).child(slider(
                        &format!("vvol-{id}"),
                        crate::t::video_volume(),
                        format!("{}%", (level * 100.).round()).into(),
                        level,
                        true,
                        clip.leveling,
                        o6,
                        Act::Volume,
                    )))
                })
                .child(button(
                    if full { "ui/video-exit-full.svg" } else { "ui/video-full.svg" },
                    "vfull",
                    if full { crate::t::video_exit_full_screen() } else { crate::t::video_full_screen() },
                    Act::Fullscreen,
                    o7,
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
    let spun = clip.spinner();
    let spin = (spun > 0. && !dim).then(|| {
        div()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .child(div().size(px(36.)).child(crate::attachments::spinner(36., 3., 1., 0.25, white_a(0.2 * spun), white_a(spun))))
    });
    div()
        .id(SharedString::from(format!("video-{id}")))
        .relative()
        .w(px(w))
        .h(px(h))
        .rounded_t(top)
        .rounded_b(bottom)
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
        .children(picture.map(|p| img(p).size_full().object_fit(ObjectFit::Contain).rounded_t(top).rounded_b(bottom)))
        .children(veil)
        .children(spin)
        .when(shown, |d| d.child(bar))
        .into_any_element()
}

fn scrubber(id: &str, frac: f32, time: SharedString, seekable: bool, dragging: bool, on: OnAct) -> impl IntoElement {
    div().flex_1().h(px(20.)).mx(px(6.)).child(slider(&format!("vseek-{id}"), crate::t::audio_seek(), time, frac, seekable, dragging, on, Act::Seek))
}

thread_local! {
    static PRESSED: std::cell::RefCell<Option<SharedString>> = const { std::cell::RefCell::new(None) };
}

/// A track's press, held apart from the frame that paints it: its release can arrive first.
pub(crate) fn press(key: &SharedString) {
    PRESSED.with_borrow_mut(|p| *p = Some(key.clone()));
}

/// Whether this release ends a press on `key`.
pub(crate) fn release(key: &SharedString) -> bool {
    PRESSED.with_borrow_mut(|p| p.take_if(|k| *k == *key).is_some())
}

/// A track with a knob: `act(fraction, done)` as it is pressed, dragged and let go.
#[allow(clippy::too_many_arguments)]
fn slider(key: &str, label: SharedString, value: SharedString, frac: f32, live: bool, dragging: bool, on: OnAct, act: fn(f32, bool) -> Act) -> impl IntoElement {
    let seekable = live;
    let key = SharedString::from(key.to_string());
    let name = key.clone();
    div()
        .id(key)
        .as_slider(label, value, frac)
        .when(live, |d| {
            let on = on.clone();
            d.seek_keys(frac, 0.05, move |f, w, cx| on(act(f, true), w, cx))
        })
        .size_full()
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
                        let (on, hitbox, name) = (on.clone(), hitbox.clone(), name.clone());
                        window.on_mouse_event(move |e: &MouseDownEvent, phase, window, cx| {
                            if phase.bubble() && e.button == MouseButton::Left && hitbox.is_hovered(window) {
                                cx.stop_propagation();
                                press(&name);
                                on(act(at(e.position.x).clamp(0., 1.), false), window, cx);
                            }
                        });
                    }
                    if dragging {
                        let on = on.clone();
                        window.on_mouse_event(move |e: &MouseMoveEvent, phase, window, cx| {
                            if phase.bubble() && e.dragging() {
                                on(act(at(e.position.x).clamp(0., 1.), false), window, cx);
                            }
                        });
                    }
                    window.on_mouse_event(move |e: &MouseUpEvent, phase, window, cx| {
                        if phase.bubble() && release(&name) {
                            on(act(at(e.position.x).clamp(0., 1.), true), window, cx);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_clip_let_go_comes_back_where_it_stood() {
        let mut v = Videos::default();
        let poster = Arc::new(RenderImage::new(vec![image::Frame::new(image::RgbaImage::new(512, 512))]));
        let info = |poster| Info { w: 16, h: 9, duration: 10_000, audio: false, poster };
        let spec = AudioInputSpec::File(std::path::PathBuf::from("/nonexistent.mp4"));
        v.want_probe("a", &spec);
        v.probed("a".into(), Some(info(Some(poster))));
        let c = v.clips.get_mut("a").unwrap();
        (c.muted, c.position) = (true, 4_000);
        let rest = v.at_rest();
        assert_eq!(rest.len(), 1);
        assert_eq!(rest[0].2, 512 * 512 * 4);
        v.shed("a");
        assert!(v.clip("a").is_none());
        assert_eq!(v.retired.len(), 1);
        v.probed("a".into(), Some(info(None)));
        assert!(v.clip("a").is_some_and(|c| c.muted && c.position == 4_000));
    }
    #[test]
    fn frames_retired_with_nothing_playing_go_back_to_the_allocator() {
        let mut v = Videos::default();
        let frame = || Arc::new(RenderImage::new(vec![image::Frame::new(image::RgbaImage::new(64, 36))]));
        v.clips.insert("a".into(), Clip { playing: true, ..Clip::default() });
        v.recycle(frame());
        assert_eq!(BUFFERS.lock().unwrap().len(), 1);
        v.clips.get_mut("a").unwrap().playing = false;
        v.recycle(frame());
        assert!(BUFFERS.lock().unwrap().is_empty() && v.spent.is_empty());
    }
}
