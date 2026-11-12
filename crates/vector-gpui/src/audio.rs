//! Audio attachments on the shared engine (`AudioPlayer.svelte`, `AudioSession`): play and
//! pause, the spectral bars, the scrub, and one player sounding at a time.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use gpui::{
    AnyElement, App, Bounds, Corners, CursorStyle, HitboxBehavior, InteractiveElement,
    IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement,
    Pixels, RenderImage, SharedString, StatefulInteractiveElement, Styled, Window, canvas, div, fill, point,
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
    pub album: Option<crate::album::Album>,
}

/// Where the audio is, kept so a load can run off the UI thread.
#[derive(Clone)]
pub enum AudioInputSpec {
    File(std::path::PathBuf),
    Held([u8; 32]),
}

impl AudioInputSpec {
    pub(crate) fn open(&self) -> Result<AudioInput, String> {
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
            album: None,
        }
    }

    pub fn position(&self) -> u64 {
        if !self.playing {
            return self.paused_at;
        }
        let p = self.start_pos + self.started.map_or(0, |s| s.elapsed().as_millis() as u64);
        if self.duration > 0 { p.min(self.duration) } else { p }
    }

    pub fn display(&self) -> &[f32] {
        &self.display
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
    /// A named file's tags: None once read and found empty.
    tags: HashMap<String, Option<Arc<Tags>>>,
    reading: HashSet<String>,
    pub sheets: HashMap<String, crate::lyrics::Sheet>,
    /// Albums showing every song rather than the first few.
    pub all_tracks: HashSet<String>,
    pub transcripts: crate::transcript::Transcripts,
    /// Auto-transcribe is on and its model is here; None until read, and after a settings change.
    pub auto_transcribe: Option<bool>,
}

/// What a file's tags give the card.
pub struct Tags {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub year: Option<u16>,
    pub cover: Option<Arc<RenderImage>>,
    /// The art shrunk to a few pixels: scaled up, it is the card's blurred glow.
    pub glow: Option<Arc<RenderImage>>,
    /// The same for the card with its lyrics open, a taller window on the art.
    pub glow_tall: Option<Arc<RenderImage>>,
    /// The art's own colour for the controls; white where the art has none.
    pub accent: Option<gpui::Hsla>,
    pub lyrics: Option<vector_media::lyrics::Lyrics>,
    pub chapters: Vec<vector_media::chapters::Chapter>,
}

#[derive(Clone, Copy, Debug)]
pub enum Act {
    Toggle,
    /// A scrub to this fraction; `done` on release.
    Seek(f32, bool),
    Lyrics,
    Album(AlbumAct),
    Transcribe,
    CancelDownload,
    /// A transcript section clicked: to this millisecond.
    SeekTo(u64),
    /// A lyric line clicked (play from it), or None: the reader scrolled, so hold the view.
    Line(Option<u32>),
}

#[derive(Clone, Copy, Debug)]
pub enum AlbumAct {
    /// Play from the first song, or a random one.
    Play(bool),
    Track(usize),
    Next,
    Prev,
    Shuffle,
    Repeat,
    More,
}

/// What the app does for the audio state: the work that needs the engine or a thread.
pub enum Work {
    Probe(String, AudioInputSpec),
    Load(String, AudioInputSpec),
    Tags(String, AudioInputSpec),
    Transcribe(String, AudioInputSpec),
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

    /// A read of a named file's tags, once per file.
    pub fn want_tags(&mut self, att: &str, input: &AudioInputSpec) {
        if !self.tags.contains_key(att) && self.reading.insert(att.to_string()) {
            self.queue.push(Work::Tags(att.to_string(), input.clone()));
        }
    }

    pub fn transcribed(&mut self, att: &str, res: Result<vector_media::whisper::TranscriptionResult, String>) {
        use crate::transcript::{Phase, Section};
        if self.transcripts.download.as_ref().is_some_and(|d| d.until.is_none()) {
            self.transcripts.download_done(res.is_ok());
        }
        let phase = match res {
            Ok(r) => Phase::Ready {
                sections: r
                    .sections
                    .into_iter()
                    .map(|s| Section { text: s.text.trim().to_string(), at: s.at.max(0) as u64 })
                    .filter(|s| !s.text.is_empty())
                    .collect(),
                lang: r.lang,
                language: r.language,
            },
            Err(e) => Phase::Error(e),
        };
        self.transcripts.landed(att, phase);
    }

    /// Read, and found to be music: it has its own words, or none to transcribe.
    pub fn tags_read(&self, att: &str) -> Option<bool> {
        self.tags.get(att).map(|t| {
            t.as_ref().is_some_and(|t| t.title.is_some() || t.artist.is_some() || t.album.is_some() || t.lyrics.is_some())
        })
    }

    pub fn tags(&self, att: &str) -> Option<&Arc<Tags>> {
        self.tags.get(att).and_then(|t| t.as_ref())
    }

    pub fn read_tags(&mut self, att: String, tags: Option<Tags>) {
        self.reading.remove(&att);
        self.tags.insert(att, tags.map(Arc::new));
    }

    /// A probe for the header's duration, once per file.
    pub fn want_probe(&mut self, att: &str, input: &AudioInputSpec) {
        if !self.probed.contains_key(att) && self.probing.insert(att.to_string()) {
            self.queue.push(Work::Probe(att.to_string(), input.clone()));
        }
    }

    /// `autoTranscribe`: a fresh voice message from someone else transcribes itself.
    pub fn want_transcript(&mut self, att: &str, input: &AudioInputSpec) {
        if self.transcripts.get(att).is_some() || self.transcripts.download.is_some() {
            return;
        }
        let on = *self.auto_transcribe.get_or_insert_with(|| {
            crate::backend::transcribe::auto_transcribe()
                && crate::backend::transcribe::downloaded(&crate::backend::transcribe::choice().0)
        });
        if on && self.transcripts.toggle(att).is_some() {
            self.queue.push(Work::Transcribe(att.to_string(), input.clone()));
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
            Act::Lyrics => {
                self.sheets.entry(att.to_string()).or_default().toggle();
                None
            }
            Act::Line(None) => {
                self.sheets.entry(att.to_string()).or_default().hold();
                None
            }
            Act::Line(Some(ms)) => {
                self.sheets.entry(att.to_string()).or_default().release();
                let duration = self.duration(att).max(1);
                let work = self.act(msg, att, input.clone(), Act::Seek(ms as f32 / duration as f32, true));
                if self.players.get(att).is_some_and(|p| p.playing || p.loading) {
                    work
                } else {
                    self.play(msg, att, input)
                }
            }
            Act::Album(a) => self.album_act(msg, att, input, a),
            Act::Transcribe => {
                self.transcripts.toggle(att)?;
                let (model, _) = crate::backend::transcribe::choice();
                if !crate::backend::transcribe::downloaded(&model) {
                    self.transcripts.download = Some(crate::transcript::Download {
                        pct: 0,
                        text: "Downloading model...".into(),
                        failed: false,
                        until: None,
                    });
                }
                Some(Work::Transcribe(att.to_string(), input))
            }
            Act::CancelDownload => {
                crate::backend::transcribe::cancel_download();
                None
            }
            Act::SeekTo(ms) => {
                let p = self.player_mut(msg, att);
                if let Some(a) = p.album.as_mut() {
                    a.seeked(ms);
                }
                self.seek_ms(att, ms);
                None
            }
            Act::Seek(frac, done) => {
                let probed = self.probed.get(att).copied().unwrap_or(0);
                let p = self.player_mut(msg, att);
                if p.duration == 0 {
                    p.duration = probed;
                }
                if p.duration == 0 {
                    return None;
                }
                let (start, end) = p.album.as_ref().map_or((0, p.duration), |a| a.now(p.duration));
                let ms = start + (frac.clamp(0., 1.) * end.saturating_sub(start) as f32) as u64;
                if let Some(a) = p.album.as_mut() {
                    a.seeked(ms);
                }
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

    /// The player for `att`, knowing its album once the tags have one.
    fn player_mut(&mut self, msg: &str, att: &str) -> &mut Player {
        let chapters = self
            .tags
            .get(att)
            .and_then(|t| t.as_ref())
            .filter(|t| t.chapters.len() > 1)
            .map(|t| t.chapters.clone());
        let p = self.players.entry(att.to_string()).or_insert_with(|| Player::new(msg));
        if p.album.is_none()
            && let Some(ch) = chapters
        {
            p.album = Some(crate::album::Album::new(&ch, p.position()));
        }
        p
    }

    /// Move the playhead to `ms`, playing or not.
    fn seek_ms(&mut self, att: &str, ms: u64) {
        let Some(p) = self.players.get_mut(att) else { return };
        if p.playing {
            p.start_pos = ms;
            p.started = Some(Instant::now());
        } else {
            p.paused_at = ms;
            if p.look == Look::Fresh {
                p.look = Look::Rest;
            }
        }
        if let (Some(id), Ok(e)) = (p.source, engine()) {
            let _ = e.seek(id, ms);
        }
    }

    fn album_act(&mut self, msg: &str, att: &str, input: AudioInputSpec, act: AlbumAct) -> Option<Work> {
        if let AlbumAct::More = act {
            if !self.all_tracks.remove(att) {
                self.all_tracks.insert(att.to_string());
            }
            return None;
        }
        let probed = self.probed.get(att).copied().unwrap_or(0);
        let p = self.player_mut(msg, att);
        if p.duration == 0 {
            p.duration = probed;
        }
        let (pos, duration, playing) = (p.position(), p.duration, p.playing || p.loading);
        let a = p.album.as_mut()?;
        let (to, start) = match act {
            AlbumAct::Play(shuffled) => {
                a.set_shuffle(shuffled);
                let i = if shuffled { rand::random::<usize>() % a.tracks.len().max(1) } else { 0 };
                (a.play_track(i), true)
            }
            AlbumAct::Track(i) => (a.play_track(i), true),
            AlbumAct::Next => (a.next(), false),
            AlbumAct::Prev => (Some(a.prev(pos, duration)), false),
            AlbumAct::Shuffle => {
                let on = !a.shuffle;
                a.set_shuffle(on);
                (None, false)
            }
            AlbumAct::Repeat => {
                a.cycle_repeat();
                (None, false)
            }
            AlbumAct::More => unreachable!(),
        };
        if let Some(ms) = to {
            self.seek_ms(att, ms);
        }
        if start && !playing { self.play(msg, att, input) } else { None }
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
        let _ = self.player_mut(msg, att);
        let p = self.players.get_mut(att)?;
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

    /// The attachment sounding now, if any.
    pub fn playing(&self) -> Option<&str> {
        let a = self.active.as_deref()?;
        self.players.get(a).is_some_and(|p| p.playing || p.loading).then_some(a)
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
        if name == "whisper_download_progress" {
            let pct = v.get("progress").and_then(|p| p.as_f64()).unwrap_or(0.) as u32;
            if self.transcripts.download.as_ref().is_some_and(|d| d.until.is_none()) {
                self.transcripts.progress(pct);
            }
            return None;
        }
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
                if self.active.as_deref() == Some(att.as_str()) {
                    self.active = None;
                }
                // The file's end is the last song's end: the listener's way decides what follows.
                if let Some(ms) = p.album.as_mut().and_then(|a| a.ended()) {
                    p.paused_at = ms;
                    let (msg, path) = (p.msg.clone(), p.path.clone());
                    if let Some(input) = path
                        && let Some(w) = self.play(&msg, &att, input)
                    {
                        self.queue.push(w);
                    }
                    return None;
                }
                p.look = Look::Settling;
                (!p.album.is_some()).then_some(att)
            }
            _ => None,
        }
    }

    /// One frame of the bars; true while any still moves.
    pub fn tick(&mut self) -> bool {
        let mut moving = self.transcripts.tick();
        let mut steps = Vec::new();
        for (att, p) in self.players.iter_mut() {
            let pos = p.position();
            if p.playing
                && let Some(a) = p.album.as_mut()
            {
                match a.tick(pos) {
                    crate::album::Step::Stay => {}
                    step => steps.push((att.clone(), step)),
                }
            }
        }
        for (att, step) in steps {
            match step {
                crate::album::Step::Seek(ms) => self.seek_ms(&att, ms),
                crate::album::Step::Stop(ms) => {
                    self.pause(&att);
                    self.seek_ms(&att, ms);
                }
                crate::album::Step::Stay => {}
            }
        }
        for (att, sheet) in self.sheets.iter_mut() {
            if let Some(l) = self.tags.get(att).and_then(|t| t.as_ref()).and_then(|t| t.lyrics.as_ref()) {
                let pos = self.players.get(att).map_or(0, |p| p.position());
                moving |= sheet.tick(l, pos);
            }
        }
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
            Work::Transcribe(att, input) => {
                let res = input.open().and_then(|i| {
                    tauri::async_runtime::block_on(crate::backend::transcribe::transcribe(i))
                });
                WorkDone::Transcribed(att, res)
            }
            Work::Tags(att, input) => WorkDone::Tags(
                att,
                input
                    .open()
                    .and_then(|i| vector_media::tags::read(&i))
                    .ok()
                    .flatten()
                    .map(tags_from),
            ),
        };
        let _ = tx.send(done);
    });
    rx
}

pub enum WorkDone {
    Probed(String, u64),
    Loaded(String, Result<(u32, u64), String>),
    Tags(String, Option<Tags>),
    Transcribed(String, Result<vector_media::whisper::TranscriptionResult, String>),
}

fn tags_from(m: vector_media::tags::Meta) -> Tags {
    let art = m.cover.as_deref().and_then(|b| image::load_from_memory(b).ok());
    let cover = art.as_ref().map(|a| {
        let a = a.resize_to_fill(256, 256, image::imageops::FilterType::CatmullRom);
        Arc::new(RenderImage::new(vec![image::Frame::new(crate::media::to_bgra(a.into_rgba8()))]))
    });
    let small = art.as_ref().map(|a| a.resize_exact(24, 24, image::imageops::FilterType::Triangle).into_rgba8());
    // `.audio-art-glow` covers 160% of the card, so a 500x150 card sees this band of the
    // art, and one with its lyrics open (500x422) a taller one.
    let square = art.as_ref().map(|a| a.resize_exact(256, 256, image::imageops::FilterType::Triangle).into_rgba8());
    let band = |y: u32, h: u32, out: (u32, u32)| {
        square.as_ref().map(|a| {
            let band = image::imageops::crop_imm(a, 48, y, 160, h).to_image();
            let small = image::imageops::resize(&band, out.0, out.1, image::imageops::FilterType::Triangle);
            let mut g = image::imageops::blur(&small, 8.);
            for px in g.pixels_mut() {
                saturate(px, 1.4);
            }
            Arc::new(RenderImage::new(vec![image::Frame::new(crate::media::to_bgra(g))]))
        })
    };
    let glow = band(104, 48, (100, 30));
    let glow_tall = band(60, 136, (100, 84));
    let accent = cover
        .as_ref()
        .map(|_| small.as_ref().and_then(art_accent).unwrap_or(gpui::hsla(0., 0., 0.949, 1.)));
    Tags {
        title: m.title.filter(|t| !t.is_empty()),
        artist: m.artist.filter(|t| !t.is_empty()),
        album: m.album.filter(|t| !t.is_empty()),
        year: m.year,
        cover,
        glow,
        glow_tall,
        accent,
        lyrics: m.lyrics,
        chapters: m.chapters,
    }
}

fn saturate(px: &mut image::Rgba<u8>, k: f32) {
    let [r, g, b, _] = px.0.map(|v| v as f32);
    let l = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    for (i, v) in [r, g, b].into_iter().enumerate() {
        px.0[i] = (l + (v - l) * k).clamp(0., 255.) as u8;
    }
}

/// `artAccent`: the weightiest hue among the art's saturated mid-tones, lifted to 68% light.
fn art_accent(px: &image::RgbaImage) -> Option<gpui::Hsla> {
    let mut bins = [(0f32, 0f32, 0f32, 0f32); 12];
    let mut total = 0f32;
    for p in px.pixels() {
        let [r, g, b] = [p[0], p[1], p[2]].map(|v| v as f32 / 255.);
        let (max, min) = (r.max(g).max(b), r.min(g).min(b));
        let l = (max + min) / 2.;
        let sat = if max == min { 0. } else { (max - min) / (1. - (2. * l - 1.).abs()) };
        let w = sat * sat * (1. - (2. * l - 1.).abs());
        total += 1.;
        if w < 0.02 {
            continue;
        }
        let d = max - min;
        let hue = if max == r {
            ((g - b) / d) % 6.
        } else if max == g {
            (b - r) / d + 2.
        } else {
            (r - g) / d + 4.
        };
        let bin = &mut bins[(((hue * 60. + 360.) % 360.) / 30.) as usize % 12];
        bin.0 += w;
        bin.1 += p[0] as f32 * w;
        bin.2 += p[1] as f32 * w;
        bin.3 += p[2] as f32 * w;
    }
    let top = bins.iter().copied().fold((0., 0., 0., 0.), |a, b| if b.0 > a.0 { b } else { a });
    if total == 0. || top.0 / total < 0.03 {
        return None;
    }
    let (r, g, b) = (top.1 / top.0 / 255., top.2 / top.0 / 255., top.3 / top.0 / 255.);
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let (l, d) = ((max + min) / 2., max - min);
    if d == 0. {
        return None;
    }
    let s = d / (1. - (2. * l - 1.).abs());
    let h = if max == r {
        ((g - b) / d + 6.) % 6.
    } else if max == g {
        (b - r) / d + 2.
    } else {
        (r - g) / d + 4.
    };
    Some(gpui::hsla(h * 60. / 360., s.clamp(0.45, 0.85), 0.68, 1.))
}

/// `splitTitle`: an edition note ("Remastered 2011", "Live") set apart from the title.
pub fn split_title(t: &str) -> (String, String) {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        regex::Regex::new(r"(?i)\s*(?:\(([^()]*\b(?:remaster(?:ed)?|remix|live|mono|stereo|edit|version|demo|acoustic|deluxe|bonus)\b[^()]*)\)|-\s*((?:\d{4}\s*)?remaster(?:ed)?(?:\s*\d{4})?|live|radio edit))\s*$").unwrap()
    });
    match re.captures(t) {
        Some(c) => {
            let at = c.get(0).unwrap().start();
            let note = c.get(1).or(c.get(2)).map_or("", |m| m.as_str()).trim();
            (t[..at].trim().to_string(), note.to_string())
        }
        None => (t.to_string(), String::new()),
    }
}

pub fn format_time(ms: u64) -> String {
    let s = ms / 1000;
    format!("{}:{:02}", s / 60, s % 60)
}

/// `.audio-album.is-full`: the art beside the head, then the songs; the caller adds the
/// controls under them.
#[allow(clippy::too_many_arguments)]
fn album_view(
    att: &vector_core::Attachment,
    t: &Tags,
    art: Option<Arc<RenderImage>>,
    album: Option<&crate::album::Album>,
    playing: bool,
    duration: u64,
    accent: gpui::Hsla,
    all: bool,
    on: Rc<dyn Fn(AlbumAct, &mut Window, &mut App)>,
) -> gpui::Div {
    let id = &att.id;
    let line = |d: gpui::Div| d.overflow_hidden().text_ellipsis().whitespace_nowrap();
    let (main, note) = split_title(t.album.as_deref().or(t.title.as_deref()).unwrap_or(&att.name));
    let byline = [t.artist.clone(), t.year.map(|y| y.to_string())]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" \u{b7} ");
    let n = t.chapters.len();
    let stats = if duration > 0 {
        format!("{n} songs \u{b7} {} min", (duration as f64 / 60000.).round())
    } else {
        format!("{n} songs")
    };
    let shuffle_on = album.is_some_and(|a| a.shuffle);
    let repeat = album.map_or(crate::album::Repeat::Off, |a| a.repeat);
    let pill = |key: &str, act: AlbumAct, primary: bool, icon: &'static str, label: &'static str| {
        let on = on.clone();
        div()
            .id(SharedString::from(format!("album-{key}-{id}")))
            .flex()
            .items_center()
            .gap(px(5.))
            .pl(px(10.))
            .pr(px(14.))
            .py(px(6.))
            .rounded_full()
            .text_size(px(12.3))
            .line_height(px(17.22))
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .cursor_pointer()
            .when(primary, |d| d.bg(accent).text_color(c(0x0a0a0a)).hover(|s| s.opacity(0.92)))
            .when(!primary, |d| {
                d.border_1()
                    .border_color(c(0x363636))
                    .bg(white_a(0.04))
                    .text_color(white_a(0.92))
                    .hover(|s| s.bg(white_a(0.09)))
            })
            .on_click(move |_, w, cx| {
                cx.stop_propagation();
                on(act, w, cx)
            })
            .child(
                svg()
                    .path(icon)
                    .size(px(if primary { 16. } else { 14. }))
                    .text_color(if primary { c(0x0a0a0a) } else { white_a(0.7) }),
            )
            .child(label)
    };
    let toggle = |key: &str, act: AlbumAct, lit: bool, icon: &'static str| {
        let on = on.clone();
        div()
            .id(SharedString::from(format!("album-{key}-{id}")))
            .size(px(30.))
            .rounded(px(8.))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .hover(|s| s.bg(white_a(0.07)))
            .on_click(move |_, w, cx| {
                cx.stop_propagation();
                on(act, w, cx)
            })
            .child(svg().path(icon).size(px(16.)).text_color(if lit { accent } else { white_a(0.34) }))
    };
    let head = div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .justify_center()
        .gap(px(2.))
        .child(
            line(div())
                .text_size(px(15.75))
                .line_height(px(19.7))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(white_a(0.92))
                .child(main)
                .when(!note.is_empty(), |d| {
                    d.child(
                        div()
                            .ml(px(4.7))
                            .font_weight(gpui::FontWeight::NORMAL)
                            .opacity(0.5)
                            .child(note),
                    )
                })
                .flex(),
        )
        .when(!byline.is_empty(), |d| {
            d.child(line(div()).text_size(px(12.)).line_height(px(15.)).text_color(white_a(0.55)).child(byline))
        })
        .child(
            div()
                .mt(px(1.))
                .text_size(px(11.4))
                .line_height(px(15.96))
                .text_color(white_a(0.34))
                .child(stats),
        )
        .child(
            div()
                .mt(px(10.))
                .flex()
                .items_center()
                .gap(px(8.))
                .child(pill("play", AlbumAct::Play(false), true, "icons/play.svg", "Play"))
                .child(pill("shuffle-play", AlbumAct::Play(true), false, "icons/shuffle.svg", "Shuffle"))
                .child(
                    div()
                        .ml_auto()
                        .flex()
                        .gap(px(2.))
                        .child(toggle("shuffle", AlbumAct::Shuffle, shuffle_on, "icons/shuffle.svg"))
                        .child(toggle(
                            "repeat",
                            AlbumAct::Repeat,
                            repeat != crate::album::Repeat::Off,
                            if repeat == crate::album::Repeat::One { "icons/repeat-one.svg" } else { "icons/repeat.svg" },
                        )),
                ),
        );
    let current = album.map_or(-1, |a| a.track);
    let shown = if all { n } else { n.min(4) };
    let eq = |t: f32| {
        div().h(px(12.)).flex().items_end().gap(px(2.)).children((0..3).map(move |k| {
            let p = ((t / 0.9 + k as f32 / 3.).fract() * std::f32::consts::TAU).cos();
            let scale = 0.3 + 0.7 * (1. - p) / 2.;
            div().w(px(3.)).h(px(12. * scale)).rounded(px(1.)).bg(accent)
        }))
    };
    let clock = album_clock();
    let tracks = t.chapters.iter().take(shown).enumerate().map(|(i, ch)| {
        let (tmain, tnote) = split_title(&ch.title);
        let len = ch.end_ms.map_or(duration, u64::from).saturating_sub(ch.start_ms as u64);
        let is_cur = i as isize == current;
        let on = on.clone();
        div()
            .id(SharedString::from(format!("album-track-{i}-{id}")))
            .w_full()
            .flex()
            .items_center()
            .gap(px(10.))
            .px(px(6.))
            .py(px(7.))
            .rounded(px(8.))
            .text_size(px(13.2))
            .line_height(px(18.48))
            .text_color(if is_cur { accent } else { white_a(0.92) })
            .when(is_cur, |d| d.font_weight(gpui::FontWeight::SEMIBOLD))
            .cursor_pointer()
            .hover(|s| s.bg(white_a(0.05)))
            .on_click(move |_, w, cx| {
                cx.stop_propagation();
                on(AlbumAct::Track(i), w, cx)
            })
            .child(
                div()
                    .w(px(26.))
                    .flex_shrink_0()
                    .flex()
                    .justify_center()
                    .font_weight(gpui::FontWeight::NORMAL)
                    .text_color(white_a(0.34))
                    .child(if is_cur && playing {
                        eq(clock).into_any_element()
                    } else {
                        (i + 1).to_string().into_any_element()
                    }),
            )
            .child(
                line(div().flex_1().min_w_0().flex())
                    .child(tmain)
                    .when(!tnote.is_empty(), |d| {
                        d.child(div().ml(px(4.)).font_weight(gpui::FontWeight::NORMAL).opacity(0.5).child(tnote))
                    }),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .font_weight(gpui::FontWeight::NORMAL)
                    .text_color(white_a(0.34))
                    .child(format_time(len)),
            )
    });
    let more = (n > 4).then(|| {
        let on = on.clone();
        div()
            .id(SharedString::from(format!("album-more-{id}")))
            .w_full()
            .pt(px(7.))
            .pb(px(3.))
            .flex()
            .justify_center()
            .text_size(px(12.))
            .line_height(px(16.8))
            .text_color(white_a(0.55))
            .cursor_pointer()
            .hover(|s| s.text_color(white_a(0.92)))
            .on_click(move |_, w, cx| {
                cx.stop_propagation();
                on(AlbumAct::More, w, cx)
            })
            .child(if all { "Show fewer".to_string() } else { format!("Show all {n} songs") })
    });
    div()
        .relative()
        .flex()
        .flex_col()
        .gap(px(12.))
        .child(
            div()
                .flex()
                .gap(px(14.))
                .when_some(art, |d, art| {
                    d.child(
                        div()
                            .size(px(128.))
                            .flex_shrink_0()
                            .rounded(px(10.))
                            .bg(c(0x262626))
                            .shadow(vec![gpui::BoxShadow {
                                color: ca(0, 0, 0, 0.35),
                                offset: point(px(0.), px(4.)),
                                blur_radius: px(14.),
                                spread_radius: px(0.),
                                inset: false,
                            }])
                            .child(gpui::img(art).size_full().rounded(px(10.))),
                    )
                })
                .child(head),
        )
        .child(
            div()
                .pt(px(4.))
                .border_t_1()
                .border_color(white_a(0.06))
                .children(tracks.map(|t| div().pt(px(4.)).h(px(36.45)).child(t)))
                .children(more.map(|m| div().pt(px(4.)).pb(px(4.1)).child(m))),
        )
}

/// Seconds on a shared clock, for the playing song's bars.
fn album_clock() -> f32 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs_f32()
}

/// `.audio-art-glow`, crossfading to the taller window as the lyrics open.
pub(crate) fn glows(tags: Option<&Tags>, open: f32) -> Vec<AnyElement> {
    let Some(t) = tags else { return Vec::new() };
    [(t.glow.clone(), 1. - open), (t.glow_tall.clone(), open)]
        .into_iter()
        .filter_map(|(g, w)| g.filter(|_| w > 0.).map(|g| (g, w)))
        .map(|(g, w)| {
            div()
                .absolute()
                .inset_0()
                .opacity(0.22 * w)
                .child(gpui::img(g).size_full().rounded(px(11.)).object_fit(gpui::ObjectFit::Fill))
                .into_any_element()
        })
        .collect()
}

/// The bars, painted per frame, and the scrub across them.
fn waveform(
    id: SharedString,
    p: Option<&Player>,
    duration: u64,
    accent: gpui::Hsla,
    tall: f32,
    span: (u64, u64),
    on_seek: Rc<dyn Fn(f32, bool, &mut Window, &mut App)>,
) -> impl IntoElement {
    let look = p.map_or(Look::Fresh, |p| p.look);
    let playing = p.is_some_and(|p| p.playing);
    let progress = p.map_or(0., |p| {
        if span.1 > span.0 {
            (p.position().saturating_sub(span.0) as f32 / (span.1 - span.0) as f32).min(1.)
        } else {
            p.progress()
        }
    });
    let at_zero = p.is_none_or(|p| p.position() == 0);
    let lift = p.map_or(REST_Y, |p| p.lift);
    let display: Rc<[f32]> = p.map_or_else(|| Rc::from([]), |p| Rc::from(p.display.as_slice()));
    let has_wave = p.is_some_and(|p| p.wave.is_some());
    let seekable = duration > 0;
    div()
        .id(id)
        .group("audio-waveform")
        .flex_1()
        .h(px(tall))
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
                        let bottom = f32::from(bounds.origin.y) + tall / 2. + 10. + y;
                        let rect = Bounds::new(point(px(x), px(bottom - h)), size(px(bw), px(h)));
                        let color = accent;
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
    tags: Option<&Tags>,
    sheet: Option<&crate::lyrics::Sheet>,
    all_tracks: bool,
    transcripts: Option<&crate::transcript::Transcripts>,
    on: Rc<dyn Fn(Act, &mut Window, &mut App)>,
) -> AnyElement {
    let chapters = tags.map(|t| t.chapters.as_slice()).filter(|c| c.len() > 1);
    let album = p.and_then(|p| p.album.as_ref());
    let span = match (album, chapters) {
        (Some(a), _) => a.now(duration),
        (None, Some(ch)) => (ch[0].start_ms as u64, ch[0].end_ms.map_or(duration, u64::from)),
        _ => (0, duration),
    };
    let track_idx = album.map_or(-1, |a| a.track);
    let lyrics_btn = tags.and_then(|t| t.lyrics.as_ref()).map(|_| {
        let open = sheet.is_some_and(|s| s.open);
        let on = on.clone();
        div()
            .id(SharedString::from(format!("audio-lyrics-{}", att.id)))
            .size(px(34.))
            .ml(px(10.))
            .flex_shrink_0()
            .rounded(px(8.))
            .border_1()
            .border_color(c(0x363636))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .hover(|s| s.bg(white_a(0.07)))
            .on_click(move |_, w, cx| {
                cx.stop_propagation();
                on(Act::Lyrics, w, cx)
            })
            .child(svg().path("icons/align-left.svg").size(px(17.)).text_color(if open {
                tags.and_then(|t| t.accent).unwrap_or(c(PRIMARY))
            } else {
                white_a(0.7)
            }))
    });
    let sheet_el = tags.and_then(|t| t.lyrics.as_ref()).zip(sheet).and_then(|(l, s)| {
        let on = on.clone();
        crate::lyrics::view(
            &att.id,
            l,
            s,
            p.map_or(0, |p| p.position()),
            p.is_some_and(|p| p.playing),
            Rc::new(move |ms, w, cx| on(Act::Line(ms), w, cx)),
        )
    });
    let accent = tags.and_then(|t| t.accent).unwrap_or(c(PRIMARY));
    let art = tags.and_then(|t| t.cover.clone());
    let playing = p.is_some_and(|p| p.playing);
    let loading = p.is_some_and(|p| p.loading);
    let pos = p.map_or(0, |p| p.position()).min(duration.max(1));
    let pos = if duration == 0 { p.map_or(0, |p| p.position()) } else { pos };
    let id = att.id.clone();
    let toggle = on.clone();
    let on_album: Rc<dyn Fn(AlbumAct, &mut Window, &mut App)> = {
        let on = on.clone();
        Rc::new(move |a, w, cx| on(Act::Album(a), w, cx))
    };
    let seek: Rc<dyn Fn(f32, bool, &mut Window, &mut App)> = {
        let on = on.clone();
        Rc::new(move |f, done, w, cx| on(Act::Seek(f, done), w, cx))
    };
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
        .bg(accent)
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
        accent,
        if (att.name.is_empty() || art.is_some()) && chapters.is_none() { 30. } else { 20. },
        span,
        seek,
    );
    // `artist — track`, else the file's name.
    let title = match tags.and_then(|t| t.title.as_deref()) {
        Some(track) => match tags.and_then(|t| t.artist.as_deref()) {
            Some(artist) => format!("{artist} \u{2014} {track}"),
            None => track.to_string(),
        },
        None => att.name.clone(),
    };
    let now_title = chapters.map(|ch| split_title(&ch[track_idx.max(0) as usize].title).0);
    let title = now_title.unwrap_or(title);
    let transcript = transcripts.and_then(|t| t.get(&att.id));
    let downloading = transcripts
        .and_then(|t| t.download.as_ref())
        .filter(|_| transcript.is_some_and(|t| matches!(t.phase, crate::transcript::Phase::Loading)));
    let transcribe_btn = transcripts.map(|_| {
        let on = on.clone();
        crate::transcript::button(&att.id, transcript, accent, Rc::new(move |w, cx| on(Act::Transcribe, w, cx)))
    });
    let transcript_el = transcript.and_then(|t| {
        let on = on.clone();
        crate::transcript::view(
            &att.id,
            t,
            p.map_or(0, |p| p.position()),
            p.is_some_and(|p| p.playing),
            accent,
            Rc::new(move |ms, w, cx| on(Act::SeekTo(ms), w, cx)),
        )
    });
    let middle = if let Some(d) = downloading {
        let on = on.clone();
        crate::transcript::download_view(d, Rc::new(move |w, cx| on(Act::CancelDownload, w, cx)))
    } else if (att.name.is_empty() || art.is_some()) && chapters.is_none() {
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
                    .text_size(px(12.))
                    .line_height(px(15.))
                    .text_color(white_a(0.92))
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .child(title),
            )
            .child(div().flex().mx(px(-8.)).child(bars))
            .into_any_element()
    };
    let time = div()
        .min_w(px(72.))
        .flex()
        .justify_end()
        .whitespace_nowrap()
        .text_size(px(10.8))
        .line_height(px(15.12))
        .text_color(white_a(0.55))
        .child(
            div()
                .text_color(if pos > 0 { white_a(0.92) } else { white_a(0.34) })
                .child(format_time(pos.saturating_sub(span.0))),
        )
        .child(div().mx(px(4.)).text_color(white_a(0.34)).child("/"))
        .child(format_time(span.1.saturating_sub(span.0)));
    let skip = |dir: &'static str, icon: &'static str, act: AlbumAct| {
        let on = on_album.clone();
        div()
            .id(SharedString::from(format!("audio-{dir}-{}", att.id)))
            .size(px(30.))
            .mx(px(2.))
            .flex_shrink_0()
            .rounded(px(8.))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .hover(|s| s.bg(white_a(0.07)))
            .on_click(move |_, w, cx| {
                cx.stop_propagation();
                on(act, w, cx)
            })
            .child(svg().path(icon).size(px(20.)).text_color(white_a(0.92)))
    };
    let controls = div()
        .flex()
        .items_center()
        .when(chapters.is_some(), |d| d.child(skip("prev", "icons/skip-back.svg", AlbumAct::Prev)))
        .child(button)
        .when(chapters.is_some(), |d| d.child(skip("next", "icons/skip-forward.svg", AlbumAct::Next)))
        .child(middle)
        .child(time)
        .children(lyrics_btn)
        .children(transcribe_btn);
    let body = match (tags, art) {
        (Some(t), art) if chapters.is_some() => album_view(
            att,
            t,
            art,
            album,
            p.is_some_and(|p| p.playing),
            duration,
            accent,
            all_tracks,
            on_album.clone(),
        )
        .child(controls)
        .into_any_element(),
        (Some(t), Some(art)) => {
            let track = t.title.clone().unwrap_or_else(|| att.name.clone());
            let byline = [
                t.artist.clone(),
                t.album.clone().filter(|a| Some(a) != t.title.as_ref()),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" \u{b7} ");
            let line = |d: gpui::Div| d.overflow_hidden().text_ellipsis().whitespace_nowrap();
            div()
                .relative()
                .flex()
                .gap(px(14.))
                .child(
                    div()
                        .size(px(128.))
                        .flex_shrink_0()
                        .rounded(px(10.))
                        .overflow_hidden()
                        .bg(c(0x262626))
                        .shadow(vec![gpui::BoxShadow {
                            color: ca(0, 0, 0, 0.35),
                            offset: point(px(0.), px(4.)),
                            blur_radius: px(14.),
                            spread_radius: px(0.),
                            inset: false,
                        }])
                        .child(gpui::img(art).size_full().rounded(px(10.))),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .flex_1()
                                .flex()
                                .flex_col()
                                .justify_center()
                                .gap(px(2.))
                                .child(
                                    line(div())
                                        .text_size(px(15.75))
                                        .line_height(px(19.7))
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .text_color(white_a(0.92))
                                        .child(track),
                                )
                                .when(!byline.is_empty(), |d| {
                                    d.child(
                                        line(div())
                                            .text_size(px(12.))
                                            .line_height(px(15.))
                                            .text_color(white_a(0.55))
                                            .child(byline),
                                    )
                                }),
                        )
                        .child(controls),
                )
                .into_any_element()
        }
        _ => controls.into_any_element(),
    };
    div()
        .id(SharedString::from(format!("audio-{id}")))
        .relative()
        .overflow_hidden()
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
        .children(glows(tags, sheet.map_or(0., |s| s.reveal())))
        .child(body)
        .children(sheet_el)
        .children(transcript_el)
        .into_any_element()
}
