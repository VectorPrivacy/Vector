//! Voice Channels: hear what members say and speak into the call. A bot may be in as many
//! calls at once as it likes, each its own [`VoiceCall`].
//!
//! Audio is 48 kHz mono PCM (`i16`). What you hear arrives one 20 ms frame per speaker at a
//! time; what you say is queued and sent in real time, so a long clip plays at its own pace.
//!
//! ```no_run
//! # async fn f(bot: vector_sdk::VectorBot, channel_id: &str) -> vector_sdk::Result<()> {
//! let listener = bot.clone();
//! tokio::spawn(async move { listener.run(|_, _| async {}).await }); // how the call finds its members
//! let call = bot.channel(channel_id).join_voice().await?;
//! call.say(&vector_sdk::voice::tone(440.0, 1000));      // a second of A4
//! while let Some(speech) = call.listen().await {
//!     println!("{:?} said {} samples", speech.npub, speech.pcm.len());
//! }
//! # Ok(()) }
//! ```

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::{Channel, ChannelKind, Result, VectorError};

pub use vector_core::calls::voice::{InCall, VoicePeer, VoiceState};
/// Samples per second of everything heard and said.
pub const SAMPLE_RATE: u32 = vector_media::ENGINE_RATE;
/// Samples in one frame: 20 ms.
pub const FRAME: usize = vector_media::FRAME;

/// One 20 ms frame someone said.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Speech {
    /// Who said it, once the call knows the link's owner.
    pub npub: Option<String>,
    /// [`FRAME`] samples, 48 kHz mono.
    pub pcm: Vec<i16>,
    /// The link it came over: one per speaker, known or not.
    pub(crate) link: [u8; 32],
}

/// One voice Channel's call this bot is in. [`leave`](Self::leave) when done; dropping it
/// leaves the call running.
///
/// The call can also end without the bot: a moderator disconnects it or moves it elsewhere.
/// [`listen`](Self::listen) then returns `None`, [`say`](Self::say) returns false, and
/// [`moved_to`](Self::moved_to) says where a move sent it.
pub struct VoiceCall {
    channel_id: String,
    engine: Arc<vector_media::Engine>,
    names: Mutex<HashMap<[u8; 32], String>>,
}

impl std::fmt::Debug for VoiceCall {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VoiceCall").field("channel_id", &self.channel_id).finish()
    }
}

impl Channel {
    /// Join this voice Channel's call, staying in any other the bot is in. Needs Connect here;
    /// what the bot says is heard only with Speak.
    ///
    /// The bot must be listening ([`run`](crate::VectorBot::run) or
    /// [`on_event`](crate::VectorBot::on_event), often spawned): that's how the call learns who
    /// is in it. A join waits up to a minute for it.
    pub async fn join_voice(&self) -> Result<VoiceCall> {
        if self.kind == ChannelKind::Dm {
            return Err(VectorError::Other("voice is a Community channel's; a DM has none".into()));
        }
        let until = std::time::Instant::now() + Duration::from_secs(60);
        while !vector_core::community::v2::realtime::subscription_ready() {
            if std::time::Instant::now() > until {
                return Err(VectorError::Other("the bot isn't listening: start bot.run(..) or bot.on_event(..) first".into()));
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        let community = vector_core::db::community::community_id_for_channel(&self.id)
            .map_err(VectorError::Db)?
            .ok_or_else(|| VectorError::NotFound(format!("community channel {}", self.id)))?;
        if !vector_media::install() {
            return Err(VectorError::Other("another calls platform is installed in this process".into()));
        }
        vector_core::calls::voice::join_also(&community, &self.id).await.map_err(VectorError::Other)?;
        let Some(engine) = vector_media::engine_in(&self.id) else {
            vector_core::calls::voice::leave_channel(&self.id).await;
            return Err(VectorError::Other("joined, but the call has no audio".into()));
        };
        Ok(VoiceCall { channel_id: self.id.clone(), engine, names: Mutex::new(HashMap::new()) })
    }

    /// Who is in this voice Channel's call now, whether or not the bot is.
    pub fn voice_members(&self) -> Vec<InCall> {
        vector_core::calls::voice::in_call(&self.id)
    }
}

impl VoiceCall {
    pub fn channel_id(&self) -> &str {
        &self.channel_id
    }

    /// The next frame anyone said, or `None` once the call is over. Frames from different
    /// speakers interleave; tell them apart by [`Speech::npub`]. When someone stops talking,
    /// up to 2 s of silent frames follow, so a pause can be told apart: [`Utterances`] does that.
    /// An empty frame means its speaker stopped being heard for good (they left or lost Speak).
    ///
    /// A program that falls behind loses the oldest frames, not the newest.
    pub async fn listen(&self) -> Option<Speech> {
        let heard = self.engine.listen().await?;
        Some(Speech { npub: self.name_of(&heard.endpoint), pcm: heard.pcm, link: heard.endpoint })
    }

    /// The channel a moderator moved this call to, which ended it: `bot.channel(id).join_voice()`
    /// follows. Read once.
    pub fn moved_to(&self) -> Option<String> {
        vector_core::calls::voice::moved_to(&self.channel_id)
    }

    /// Whether this handle's call is the one the bot is in here: a later join to the same
    /// channel replaces it.
    fn current(&self) -> bool {
        vector_media::engine_in(&self.channel_id).is_some_and(|e| Arc::ptr_eq(&e, &self.engine))
    }

    fn name_of(&self, endpoint: &[u8; 32]) -> Option<String> {
        if let Some(n) = self.names.lock().unwrap_or_else(|e| e.into_inner()).get(endpoint) {
            return Some(n.clone());
        }
        use nostr_sdk::prelude::ToBech32;
        let npub = vector_core::calls::voice::peer_author(&self.channel_id, endpoint)?.to_bech32().ok()?;
        self.names.lock().unwrap_or_else(|e| e.into_inner()).insert(*endpoint, npub.clone());
        Some(npub)
    }

    /// Queue samples to say, after whatever is already queued. Returns at once: false when the
    /// call has ended, and nothing was queued.
    pub fn say(&self, pcm: &[i16]) -> bool {
        self.engine.say(pcm)
    }

    /// [`say`](Self::say), then wait until all of it has been said: false when the call ended
    /// first.
    pub async fn say_and_wait(&self, pcm: &[i16]) -> bool {
        self.engine.say(pcm) && self.engine.finished().await
    }

    /// Wait until everything queued has been said: false when the call ended first.
    pub async fn finished(&self) -> bool {
        self.engine.finished().await
    }

    /// Whether the call has ended: left, disconnected by a moderator, or moved.
    pub fn ended(&self) -> bool {
        self.engine.ended()
    }

    /// How much is still queued to say.
    pub fn queued(&self) -> Duration {
        self.engine.queued()
    }

    /// Stop saying what's queued.
    pub fn hush(&self) {
        self.engine.hush();
    }

    /// How many members the bot's audio reaches now.
    pub fn connected(&self) -> usize {
        self.engine.connected()
    }

    /// The call as the bot sees it: everyone connected, who's muted, who's talking.
    pub fn state(&self) -> Option<VoiceState> {
        self.current().then(|| vector_core::calls::voice::snapshot_in(&self.channel_id)).flatten()
    }

    /// Mute the bot: nothing it queues is sent until unmuted.
    pub fn set_muted(&self, on: bool) -> Result<()> {
        self.ours()?;
        vector_core::calls::voice::set_muted_in(&self.channel_id, on).map_err(VectorError::Other)
    }

    /// Deafen the bot: it hears nothing, and shows as deafened.
    pub fn set_deafened(&self, on: bool) -> Result<()> {
        self.ours()?;
        vector_core::calls::voice::set_deafened_in(&self.channel_id, on).map_err(VectorError::Other)
    }

    fn ours(&self) -> Result<()> {
        if self.current() {
            Ok(())
        } else {
            Err(VectorError::Other("this call has ended".into()))
        }
    }

    /// Leave the call. Others the bot is in carry on, and so does a newer call in this channel.
    pub async fn leave(self) {
        if self.current() {
            vector_core::calls::voice::leave_channel(&self.channel_id).await;
        }
    }
}

/// What one speaker said from when they began until they paused.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Utterance {
    pub npub: Option<String>,
    /// 48 kHz mono, the pause trimmed off.
    pub pcm: Vec<i16>,
}

impl Utterance {
    pub fn duration(&self) -> Duration {
        Duration::from_millis(self.pcm.len() as u64 * 1000 / SAMPLE_RATE as u64)
    }
}

/// Turns the frames a call hears into [`Utterance`]s: each speaker's words, cut at their
/// pauses. Feed it every [`Speech`] from [`VoiceCall::listen`].
///
/// ```no_run
/// # async fn f(call: vector_sdk::VoiceCall) {
/// let mut said = vector_sdk::voice::Utterances::new();
/// while let Some(speech) = call.listen().await {
///     if let Some(u) = said.push(speech) {
///         println!("{:?} spoke for {:?}", u.npub, u.duration());
///     }
/// }
/// # }
/// ```
#[derive(Debug)]
pub struct Utterances {
    open: HashMap<[u8; 32], (Option<String>, Vec<i16>, usize)>,
    quiet_below: f32,
    pause_frames: usize,
    max_frames: usize,
}

impl Default for Utterances {
    fn default() -> Self {
        Utterances { open: HashMap::new(), quiet_below: 0.01, pause_frames: 40, max_frames: 30 * 50 }
    }
}

impl Utterances {
    pub fn new() -> Self {
        Self::default()
    }

    /// How long a silence ends an utterance; 800 ms unless set, at most 1.9 s: a call delivers
    /// 2 s of a speaker's quiet, then nothing until they speak again.
    pub fn pause(mut self, pause: Duration) -> Self {
        self.pause_frames = (pause.as_millis() as usize / 20).clamp(1, 95);
        self
    }

    /// The level beneath which a frame is quiet, 0 to 1; 0.01 unless set.
    pub fn quiet_below(mut self, level: f32) -> Self {
        self.quiet_below = level;
        self
    }

    /// The longest an utterance runs before it's cut anyway; 30 s unless set.
    pub fn longest(mut self, longest: Duration) -> Self {
        self.max_frames = (longest.as_millis() as usize / 20).max(1);
        self
    }

    /// Take one frame. Returns the speaker's utterance once they pause, or stop being heard.
    pub fn push(&mut self, speech: Speech) -> Option<Utterance> {
        if speech.pcm.is_empty() {
            let (npub, mut pcm, quiet) = self.open.remove(&speech.link)?;
            pcm.truncate(pcm.len() - (quiet * FRAME).min(pcm.len()));
            return (!pcm.is_empty()).then_some(Utterance { npub: npub.or(speech.npub), pcm });
        }
        let loud = level(&speech.pcm) >= self.quiet_below;
        let (npub, pcm, quiet) = self.open.entry(speech.link).or_default();
        if npub.is_none() {
            npub.clone_from(&speech.npub);
        }
        if loud {
            pcm.extend_from_slice(&speech.pcm);
            *quiet = 0;
        } else if !pcm.is_empty() {
            pcm.extend_from_slice(&speech.pcm);
            *quiet += 1;
        }
        let full = pcm.len() >= self.max_frames * FRAME;
        if pcm.is_empty() || (*quiet < self.pause_frames && !full) {
            return None;
        }
        let trailing = *quiet * FRAME;
        let (npub, mut pcm) = (npub.take(), std::mem::take(pcm));
        pcm.truncate(pcm.len() - trailing.min(pcm.len()));
        self.open.remove(&speech.link);
        Some(Utterance { npub, pcm })
    }

    /// Everything begun and not yet ended, as utterances: for when the call ends.
    pub fn flush(&mut self) -> Vec<Utterance> {
        self.open.drain().filter(|(_, (_, pcm, _))| !pcm.is_empty()).map(|(_, (npub, pcm, _))| Utterance { npub, pcm }).collect()
    }
}

/// Speech to text with [Whisper](https://github.com/ggerganov/whisper.cpp). Load a ggml model
/// once, then transcribe what [`Utterances`] gathers.
///
/// ```no_run
/// # async fn f(u: vector_sdk::voice::Utterance) -> vector_sdk::Result<()> {
/// let whisper = vector_sdk::voice::Transcriber::load("ggml-base.en.bin")?.language("en");
/// let text = whisper.transcribe(&u.pcm).await?;
/// # Ok(()) }
/// ```
#[cfg(feature = "whisper")]
#[derive(Clone)]
pub struct Transcriber {
    ctx: Arc<whisper_rs::WhisperContext>,
    /// One state, made once and kept: making one allocates the model's buffers (on the GPU too).
    state: Arc<Mutex<Option<whisper_rs::WhisperState>>>,
    language: Option<String>,
}

#[cfg(feature = "whisper")]
impl std::fmt::Debug for Transcriber {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Transcriber").field("language", &self.language).finish()
    }
}

#[cfg(feature = "whisper")]
impl Transcriber {
    /// Load a ggml model, such as `ggml-base.en.bin` from whisper.cpp's model downloads. On
    /// macOS it runs on the GPU. whisper.cpp's own logging is switched off for the process.
    pub fn load(model: impl AsRef<std::path::Path>) -> Result<Self> {
        // Hundreds of lines per model load and per utterance, on stderr.
        whisper_rs::install_logging_hooks();
        let path = model.as_ref().to_string_lossy().into_owned();
        let ctx = whisper_rs::WhisperContext::new_with_params(&path, whisper_rs::WhisperContextParameters::default())
            .map_err(|e| VectorError::Other(format!("couldn't load the Whisper model at {path}: {e}")))?;
        Ok(Transcriber { ctx: Arc::new(ctx), state: Arc::new(Mutex::new(None)), language: Some("auto".into()) })
    }

    /// The language spoken, as a code (`en`, `de`…); `auto`, the default, detects it.
    pub fn language(mut self, code: &str) -> Self {
        self.language = Some(code.to_string());
        self
    }

    /// What `pcm` (48 kHz mono) says. The work runs off the async runtime, one transcription
    /// at a time per `Transcriber`.
    pub async fn transcribe(&self, pcm: &[i16]) -> Result<String> {
        let audio = to_16k(pcm);
        let (ctx, slot, language) = (self.ctx.clone(), self.state.clone(), self.language.clone());
        tokio::task::spawn_blocking(move || {
            let mut held = slot.lock().unwrap_or_else(|e| e.into_inner());
            if held.is_none() {
                *held = Some(ctx.create_state().map_err(|e| VectorError::Other(format!("whisper: {e}")))?);
            }
            let state = held.as_mut().expect("made above");
            let mut params = whisper_rs::FullParams::new(whisper_rs::SamplingStrategy::Greedy { best_of: 1 });
            params.set_language(language.as_deref());
            params.set_print_progress(false);
            params.set_print_realtime(false);
            params.set_print_special(false);
            params.set_no_context(true);
            params.set_suppress_nst(true);
            if audio.len() < 16_000 * 5 {
                params.set_single_segment(true);
            }
            state.full(params, &audio).map_err(|e| VectorError::Other(format!("whisper: {e}")))?;
            let text: Vec<String> = (0..state.full_n_segments())
                .filter_map(|i| state.get_segment(i))
                .filter_map(|s| s.to_str_lossy().ok().map(|t| t.trim().to_string()))
                .filter(|t| !t.is_empty())
                .collect();
            Ok(text.join(" "))
        })
        .await
        .map_err(|e| VectorError::Other(format!("whisper stopped: {e}")))?
    }
}

/// 48 kHz to Whisper's 16 kHz float: low-passed below 8 kHz first, so nothing above folds back.
#[cfg(feature = "whisper")]
fn to_16k(pcm: &[i16]) -> Vec<f32> {
    // A 31-tap windowed sinc at 7.2 kHz, Blackman-windowed.
    const TAPS: usize = 31;
    let cutoff = 7_200.0 / SAMPLE_RATE as f32;
    let taps: Vec<f32> = (0..TAPS)
        .map(|i| {
            let n = i as f32 - (TAPS - 1) as f32 / 2.0;
            let sinc = if n == 0.0 { 2.0 * cutoff } else { (std::f32::consts::TAU * cutoff * n).sin() / (std::f32::consts::PI * n) };
            let w = std::f32::consts::TAU * i as f32 / (TAPS - 1) as f32;
            sinc * (0.42 - 0.5 * w.cos() + 0.08 * (2.0 * w).cos())
        })
        .collect();
    let gain: f32 = taps.iter().sum();
    let half = TAPS / 2;
    (0..pcm.len() / 3)
        .map(|j| {
            let centre = j * 3;
            let mut acc = 0.0;
            for (k, t) in taps.iter().enumerate() {
                let i = (centre + k).checked_sub(half).filter(|&i| i < pcm.len());
                acc += t * i.map_or(0.0, |i| pcm[i] as f32);
            }
            acc / (gain * 32768.0)
        })
        .collect()
}

/// A sine tone, for tests and chimes: `hz` for `ms`, at a moderate level.
pub fn tone(hz: f32, ms: u32) -> Vec<i16> {
    let n = (SAMPLE_RATE as u64 * ms as u64 / 1000) as usize;
    (0..n).map(|i| ((i as f32 * hz * std::f32::consts::TAU / SAMPLE_RATE as f32).sin() * 8_000.0) as i16).collect()
}

/// How loud `pcm` is, 0 to 1.
pub fn level(pcm: &[i16]) -> f32 {
    if pcm.is_empty() {
        return 0.0;
    }
    let sum: f64 = pcm.iter().map(|s| (*s as f64 / i16::MAX as f64).powi(2)).sum();
    (sum / pcm.len() as f64).sqrt() as f32
}

/// Read any audio file `ffmpeg` can, as 48 kHz mono PCM. Needs `ffmpeg` on the PATH.
/// Only local files: a URL would reach the network outside the bot's transport.
pub async fn decode_with_ffmpeg(path: impl AsRef<std::path::Path>) -> Result<Vec<i16>> {
    let out = tokio::process::Command::new("ffmpeg")
        .kill_on_drop(true)
        .args(["-nostdin", "-v", "error", "-protocol_whitelist", "file", "-i"])
        .arg(path.as_ref())
        .args(["-f", "s16le", "-ac", "1", "-ar", "48000", "-"])
        .output()
        .await
        .map_err(|e| VectorError::Other(format!("couldn't run ffmpeg: {e}")))?;
    if !out.status.success() {
        return Err(VectorError::Other(format!("ffmpeg: {}", String::from_utf8_lossy(&out.stderr).trim())));
    }
    Ok(out.stdout.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(who: &str, loud: bool) -> Speech {
        let pcm = if loud { tone(300.0, 20) } else { vec![0; FRAME] };
        Speech { npub: Some(who.into()), pcm, link: [who.as_bytes()[0]; 32] }
    }

    #[test]
    fn an_utterance_ends_at_its_speakers_pause_and_drops_the_pause() {
        let mut u = Utterances::new().pause(Duration::from_millis(100));
        for _ in 0..10 {
            assert!(u.push(frame("a", true)).is_none());
        }
        // Someone else talking doesn't end it.
        assert!(u.push(frame("b", true)).is_none());
        for _ in 0..4 {
            assert!(u.push(frame("a", false)).is_none());
        }
        let said = u.push(frame("a", false)).expect("five quiet frames end it");
        assert_eq!(said.npub.as_deref(), Some("a"));
        assert_eq!(said.pcm.len(), 10 * FRAME, "the speech, without the pause");
        assert_eq!(u.flush().len(), 1, "b is still talking");
    }

    #[test]
    fn a_speaker_who_stops_being_heard_ends_their_utterance() {
        let mut u = Utterances::new();
        for _ in 0..10 {
            assert!(u.push(frame("a", true)).is_none());
        }
        let ended = || Speech { npub: Some("a".into()), pcm: Vec::new(), link: [b'a'; 32] };
        let said = u.push(ended()).expect("they left mid-word");
        assert_eq!(said.pcm.len(), 10 * FRAME);
        assert!(u.push(ended()).is_none(), "nothing open, nothing to end");
        assert!(u.flush().is_empty(), "and nothing glues onto what they say next");
    }

    #[cfg(feature = "whisper")]
    #[test]
    fn whisper_hears_speech_and_not_what_would_fold_back() {
        let level_at = |hz: f32| {
            let out = to_16k(&tone(hz, 200));
            let mid = &out[out.len() / 4..out.len() * 3 / 4];
            (mid.iter().map(|s| s * s).sum::<f32>() / mid.len() as f32).sqrt()
        };
        let (speech, high) = (level_at(1_000.0), level_at(12_000.0));
        assert!(speech > 0.15, "a 1 kHz voice passes: {speech}");
        assert!(high < speech / 20.0, "12 kHz would alias to 4 kHz, and is filtered: {high}");
    }

    #[test]
    fn two_speakers_the_call_cant_name_stay_apart() {
        let mut u = Utterances::new().pause(Duration::from_millis(40));
        let unnamed = |link: u8, loud: bool| Speech { npub: None, pcm: if loud { tone(300.0, 20) } else { vec![0; FRAME] }, link: [link; 32] };
        for _ in 0..5 {
            assert!(u.push(unnamed(1, true)).is_none());
            assert!(u.push(unnamed(2, true)).is_none());
        }
        u.push(unnamed(1, false));
        let first = u.push(unnamed(1, false)).expect("the first pauses");
        assert_eq!(first.pcm.len(), 5 * FRAME, "only its own words");
        assert_eq!(u.flush().len(), 1, "the second still talking");
    }

    #[test]
    fn silence_alone_is_never_an_utterance() {
        let mut u = Utterances::new();
        for _ in 0..200 {
            assert!(u.push(frame("a", false)).is_none());
        }
        assert!(u.flush().is_empty());
    }
}
