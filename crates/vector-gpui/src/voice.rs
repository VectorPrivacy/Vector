//! Voice Channels: the stage above a voice channel's chat, who is in each call under its
//! channel row, and the bar that follows the call around the app (`VoiceStage.svelte`,
//! `VoiceBar.svelte`, `ChannelList.svelte`).

use crate::kit::Tipped as _;
use std::collections::HashMap;
use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use gpui::prelude::*;
use gpui::{AnyElement, App, Bounds, BoxShadow, ObjectFit, Pixels, Point, RenderImage, SharedString, StyledImage, Window, canvas, div, point, px, svg};
use crate::pic::img;
use serde::Deserialize;

use crate::call::{Quality, accent_mix, call_btn, dot, phone, round_btn};
use crate::data::Store;
use crate::t;
use crate::theme::*;

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Peer {
    pub npub: String,
    pub pubkey: String,
    pub volume: f32,
    pub connected: bool,
    pub direct: bool,
    pub rtt_ms: u32,
    /// Recent loss toward them in percent, and the link's usual round trip (0 until known).
    pub loss_pct: f32,
    pub rtt_floor_ms: u32,
    pub level: f32,
    pub muted: bool,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Session {
    pub community_id: String,
    pub channel_id: String,
    pub peers: Vec<Peer>,
    pub muted: bool,
    pub deafened: bool,
    pub audio: bool,
    pub mic_level: f32,
    pub mic: bool,
    pub server_muted: bool,
    pub server_deafened: bool,
    /// Our screen and camera go to whoever watches; `viewers` are watching the screen.
    pub sharing: bool,
    pub viewers: u32,
    pub camera: bool,
    pub can_video: bool,
}

impl Session {
    /// Off for any reason: our mute, a moderator's, or a deafen.
    pub fn mic_off(&self) -> bool {
        self.muted || self.server_muted || self.deafened || self.server_deafened
    }

    fn linked(&self) -> impl Iterator<Item = &Peer> {
        self.peers.iter().filter(|p| p.connected)
    }

    /// Most of our links in trouble at once: the trouble is ours, not theirs.
    fn own_trouble(&self) -> Option<Quality> {
        let grades: Vec<Quality> = self.linked().filter(|p| p.rtt_ms > 0).map(|p| trouble(p).0).collect();
        if grades.len() < 2 {
            return None;
        }
        let poor = grades.iter().filter(|g| **g == Quality::Poor).count();
        let bad = poor + grades.iter().filter(|g| **g == Quality::Fair).count();
        if bad * 2 <= grades.len() {
            return None;
        }
        Some(if poor * 2 > grades.len() { Quality::Poor } else { Quality::Fair })
    }
}

/// A member in a voice Channel's call, as every client lists them.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Member {
    pub npub: String,
    pub muted: bool,
    pub deafened: bool,
    pub server_muted: bool,
    pub server_deafened: bool,
    pub screen: bool,
    pub camera: bool,
    pub elsewhere: bool,
}

#[derive(Default)]
pub struct Voice {
    pub session: Option<Session>,
    pub present: HashMap<String, Vec<Member>>,
    /// The channel a join is under way for.
    pub joining: Option<String>,
    pub since: Option<Instant>,
    pub(crate) pill: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// The stage grid's width, as last laid out: its columns follow it.
    grid_w: Rc<Cell<f32>>,
    pub(crate) drag: Rc<Cell<Option<crate::call::Drag>>>,
    /// The screen on the stage: None picks the first sharer's, an empty one hides it.
    pub focus: Option<String>,
    pub expanded: bool,
    /// How much of the pane the stage takes, as this device last left it.
    pub mode: StageMode,
    /// The stage's width as last laid out, and whether its faces then fit one row.
    stage_w: Rc<Cell<f32>>,
    pub compact_fits: Cell<bool>,
    pub glow: std::cell::RefCell<Glow>,
    /// Who was in each call at the last draw, and who has since left, fading out.
    ghosts: std::cell::RefCell<HashMap<String, (Vec<Member>, Vec<(Member, Instant)>)>>,
    /// What we asked of each member's pictures, (screen, camera).
    pub watched: HashMap<String, (bool, bool)>,
    /// Each member's last link flag and when it was earned.
    flags: std::cell::RefCell<HashMap<String, (Flag, Instant)>>,
}

/// Each member's speaking glow, eased toward their level every frame: quick to light, slower
/// to fade, as a meter reads.
#[derive(Default)]
pub struct Glow {
    /// Each glow's level, shared with the shadows painted from it: they follow it live.
    shown: HashMap<String, Rc<Cell<f32>>>,
    at: Option<Instant>,
}

const GLOW_RISE: f32 = 0.03;
/// How long a member who left takes to fade from the stage.
const LEAVE_MS: u128 = 220;
const GLOW_FALL: f32 = 0.18;

/// `cur` eased toward `target` over `dt` seconds.
pub fn ease_glow(cur: f32, target: f32, dt: f32) -> f32 {
    let tau = if target > cur { GLOW_RISE } else { GLOW_FALL };
    cur + (target - cur) * (1. - (-dt / tau).exp())
}

impl Glow {
    pub fn get(&self, npub: &str) -> f32 {
        self.shown.get(npub).map_or(0., |c| c.get())
    }

    /// The level `npub`'s glow follows, for painting it live.
    pub fn cell(&mut self, npub: &str) -> Rc<Cell<f32>> {
        self.shown.entry(npub.to_string()).or_default().clone()
    }

    /// Eased toward `targets` (npub, 0..1), anyone missing toward silence; whether any glow
    /// is still lit or moving.
    pub fn step(&mut self, targets: &[(String, f32)], now: Instant) -> bool {
        // A long gap (a hidden window) jumps rather than easing through what wasn't drawn.
        let dt = self.at.map_or(1., |a| now.saturating_duration_since(a).as_secs_f32()).min(1.);
        self.at = Some(now);
        for (npub, target) in targets {
            if *target > 0. || self.shown.contains_key(npub) {
                let c = self.shown.entry(npub.clone()).or_default();
                c.set(ease_glow(c.get(), *target, dt));
            }
        }
        let mut lit = false;
        for (npub, c) in &self.shown {
            if !targets.iter().any(|(n, _)| n == npub) {
                let v = ease_glow(c.get(), 0., dt);
                c.set(if v > 0.003 { v } else { 0. });
            }
            lit |= c.get() > 0.003;
        }
        lit
    }

    pub fn clear(&mut self) {
        for c in self.shown.values() {
            c.set(0.);
        }
        self.at = None;
    }
}

/// A member's link to us while it's in trouble: its grade and the figure that earned it.
#[derive(Clone)]
struct Flag {
    grade: Quality,
    label: SharedString,
}

/// A flag outlives its link's recovery by this much, so a link at a grade's edge doesn't blink.
const FLAG_HOLD: std::time::Duration = std::time::Duration::from_secs(4);

fn rank(q: Quality) -> u8 {
    match q {
        Quality::Excellent => 0,
        Quality::Good => 1,
        Quality::Fair => 2,
        Quality::Poor => 3,
    }
}

/// Trouble on a link from what actually hurts: loss, or a round trip grown well past the link's
/// own usual (a relay shows trouble as delay, not loss). A steady long path is fine. The grade,
/// and whether loss earned it.
fn trouble(p: &Peer) -> (Quality, bool) {
    let loss = p.loss_pct;
    let by_loss = if loss < 0.5 {
        Quality::Excellent
    } else if loss < 2. {
        Quality::Good
    } else if loss < 5. {
        Quality::Fair
    } else {
        Quality::Poor
    };
    let floor = p.rtt_floor_ms;
    let queued = if floor > 0 && p.rtt_ms > 0 { p.rtt_ms.saturating_sub(floor) } else { 0 };
    let by_queue = if queued >= 1000 {
        Quality::Poor
    } else if queued >= floor.max(300) {
        Quality::Fair
    } else {
        Quality::Excellent
    };
    if rank(by_loss) >= rank(by_queue) { (by_loss, by_loss != Quality::Excellent) } else { (by_queue, false) }
}

/// A linked member's path in words: round trip against its usual, loss and route.
fn link_title(p: &Peer) -> Option<SharedString> {
    if !p.connected || p.rtt_ms == 0 {
        return None;
    }
    let usual = p.rtt_floor_ms > 0 && p.rtt_ms.saturating_sub(p.rtt_floor_ms) >= 50;
    Some(match (usual, p.direct) {
        (true, true) => t::voice_path_usual_direct(p.rtt_ms, p.rtt_floor_ms, p.loss_pct),
        (true, false) => t::voice_path_usual_relay(p.rtt_ms, p.rtt_floor_ms, p.loss_pct),
        (false, true) => t::voice_path_direct(p.rtt_ms, p.loss_pct),
        (false, false) => t::voice_path_relay(p.rtt_ms, p.loss_pct),
    })
}

impl Voice {
    /// A member's link flag, held a moment past its recovery.
    fn flag(&self, p: &Peer) -> Option<Flag> {
        let mut held = self.flags.borrow_mut();
        let (grade, by_loss) = trouble(p);
        if p.connected && grade != Quality::Excellent {
            let label = if by_loss { t::voice_link_loss(p.loss_pct.round().max(1.) as u32) } else { t::call_delay_ms(p.rtt_ms) };
            let flag = Flag { grade, label };
            held.insert(p.npub.clone(), (flag.clone(), Instant::now()));
            return Some(flag);
        }
        match held.get(&p.npub) {
            Some((f, at)) if p.connected && at.elapsed() < FLAG_HOLD => Some(f.clone()),
            _ => {
                held.remove(&p.npub);
                None
            }
        }
    }
}

/// The pictures the stage shows this frame.
#[derive(Default)]
pub struct Seen {
    /// Members' pictures by their key (hex) and track.
    pub members: HashMap<(String, usize), Arc<RenderImage>>,
    pub mine: [Option<Arc<RenderImage>>; 2],
}

/// Louder than room tone.
const SPEAKING: f32 = 0.15;

impl Voice {
    pub fn set_session(&mut self, s: Option<Session>) {
        match &s {
            None => self.since = None,
            Some(n) if self.session.as_ref().is_none_or(|o| o.channel_id != n.channel_id) => {
                self.since = Some(Instant::now())
            }
            _ => {}
        }
        self.session = s;
    }

    /// Steps the speaking glows of `channel`'s call, when it is ours; whether another frame
    /// is wanted. Live levels are read as they stand; fixtures under VECTOR_VOICE_TALK speak.
    pub fn tick_glow(&self, channel: &str, me: &str, live: bool) -> bool {
        let mut glow = self.glow.borrow_mut();
        let Some(s) = self.in_call(channel) else {
            glow.clear();
            return false;
        };
        let now = Instant::now();
        let targets: Vec<(String, f32)> = if live {
            vector_core::calls::voice::levels_now()
                .into_iter()
                .filter_map(|(key, level)| {
                    let npub = if key.is_empty() {
                        (!s.mic_off() && s.mic).then(|| me.to_string())?
                    } else {
                        s.peers.iter().find(|p| p.pubkey == key)?.npub.clone()
                    };
                    Some((npub, (level * 2.).min(1.)))
                })
                .collect()
        } else if let Some(talk) = std::env::var_os("VECTOR_VOICE_TALK") {
            // `max`: everyone unmuted at full voice, for the widest glows.
            let loud = talk == "max";
            let t = self.since.map_or(0., |a| now.saturating_duration_since(a).as_secs_f32());
            self.present(channel)
                .iter()
                .enumerate()
                .filter(|(i, p)| !p.muted && (loud || i % 3 != 2))
                .map(|(i, p)| (p.npub.clone(), if loud { 1. } else { ((t * (3. + i as f32 * 0.7) + i as f32).sin() * 0.8).max(0.) }))
                .collect()
        } else {
            Vec::new()
        };
        glow.step(&targets, now)
    }

    /// Members of `channel`'s call who left within [`LEAVE_MS`], and how far gone (0..1).
    fn leaving(&self, channel: &str) -> Vec<(Member, f32)> {
        let now = Instant::now();
        let people = self.present(channel);
        let mut all = self.ghosts.borrow_mut();
        let (last, gone) = all.entry(channel.to_string()).or_default();
        for m in last.iter() {
            if !people.iter().any(|p| p.npub == m.npub) && !gone.iter().any(|(g, _)| g.npub == m.npub) {
                gone.push((m.clone(), now));
            }
        }
        gone.retain(|(g, at)| now.saturating_duration_since(*at).as_millis() < LEAVE_MS && !people.iter().any(|p| p.npub == g.npub));
        last.clear();
        last.extend(people.iter().cloned());
        gone.iter().map(|(g, at)| (g.clone(), now.saturating_duration_since(*at).as_millis() as f32 / LEAVE_MS as f32)).collect()
    }

    pub fn any_leaving(&self) -> bool {
        self.ghosts.borrow().values().any(|(_, gone)| !gone.is_empty())
    }

    fn in_call(&self, channel: &str) -> Option<&Session> {
        self.session.as_ref().filter(|s| s.channel_id == channel)
    }

    fn sharing(&self, p: &Member, me: &str) -> bool {
        p.screen || (p.npub == me && self.session.as_ref().is_some_and(|s| s.sharing))
    }

    fn cam(&self, p: &Member, me: &str) -> bool {
        p.camera || (p.npub == me && self.session.as_ref().is_some_and(|s| s.camera))
    }

    /// A linked member's key (hex), which their pictures are tagged with.
    pub fn key_of(&self, npub: &str) -> Option<&str> {
        self.session
            .as_ref()?
            .peers
            .iter()
            .find(|p| p.npub == npub && p.connected)
            .map(|p| p.pubkey.as_str())
            .filter(|k| !k.is_empty())
    }

    /// Whose screen the stage shows: the one picked while it lasts, else the first other sharer's.
    pub fn viewing(&self, channel: &str, me: &str) -> Option<String> {
        self.in_call(channel)?;
        let sharers: Vec<&str> = self
            .present(channel)
            .iter()
            .filter(|p| self.sharing(p, me))
            .map(|p| p.npub.as_str())
            .collect();
        match self.focus.as_deref() {
            Some("") => None,
            Some(f) if sharers.contains(&f) => Some(f.to_string()),
            _ => sharers.into_iter().find(|n| *n != me).map(str::to_string),
        }
    }

    /// What we want of each member's pictures with `shown` on screen: only what the stage
    /// shows, as every picture costs its sender an upload.
    pub fn wants(&self, shown: Option<&str>, me: &str) -> HashMap<String, (bool, bool)> {
        let Some(ch) = shown.filter(|c| self.in_call(c).is_some()) else {
            return HashMap::new();
        };
        let viewing = self.viewing(ch, me);
        self.present(ch)
            .iter()
            .filter(|p| p.npub != me)
            .map(|p| {
                (
                    p.npub.clone(),
                    (viewing.as_deref() == Some(p.npub.as_str()), p.camera),
                )
            })
            .filter(|(_, (a, b))| *a || *b)
            .collect()
    }

    pub fn present(&self, channel: &str) -> &[Member] {
        self.present.get(channel).map_or(&[], Vec::as_slice)
    }

    /// The rail's badge for a community: ours (we're in a call there), or someone's.
    pub fn badge(&self, store: &Store, community: &str) -> Option<bool> {
        if self.session.as_ref().is_some_and(|s| s.community_id == community) {
            return Some(true);
        }
        let com = store.community(community)?;
        com.channels
            .iter()
            .any(|ch| ch.is_voice() && !self.present(&ch.channel_id).is_empty())
            .then_some(false)
    }
}

#[derive(Clone)]
pub enum Act {
    Join(String, String),
    Leave,
    Mic,
    Deafen,
    /// Right-click on a member: (community, channel, npub, where).
    Menu(String, String, String, Point<Pixels>),
    /// A member tapped: their mini profile, beside what was painted under the key.
    Profile(String, String, SharedString),
    /// The bar's tap: back to the call's channel.
    Open(String, String),
    /// The bar dragged: it shares the 1:1 pill's place.
    Move(Point<Pixels>),
    Moved,
    MicAccess,
    Camera(bool),
    Share(bool),
    /// A sharer's tile tapped: their screen on the stage, or off it.
    Pick(String),
    HideScreen,
    Expand,
    /// The stage's chevron: fold it a step, or open it again.
    Fold,
    /// Where a phone plays the call.
    Route(String),
}

pub type OnAct = Rc<dyn Fn(Act, &mut Window, &mut App)>;

fn click(on: &OnAct, act: Act) -> impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static {
    let on = on.clone();
    move |_, w, cx| {
        cx.stop_propagation();
        on(act.clone(), w, cx)
    }
}

fn menu(on: &OnAct, community: &str, channel: &str, npub: &str) -> impl Fn(&gpui::MouseDownEvent, &mut Window, &mut App) + 'static {
    let (on, community, channel, npub) = (on.clone(), community.to_string(), channel.to_string(), npub.to_string());
    move |e, w, cx| {
        cx.stop_propagation();
        on(Act::Menu(community.clone(), channel.clone(), npub.clone(), e.position), w, cx)
    }
}

fn icon(path: &'static str, size: f32, color: gpui::Hsla) -> gpui::Svg {
    svg().path(path).size(px(size)).flex_shrink_0().text_color(color)
}

/// `.chatlist-voice-member` rows under a voice channel: who is in its call, whether or not we are.
pub fn members(voice: &Voice, store: &Store, community: &str, channel: &str, on: &OnAct, anchors: &crate::kit::Anchors) -> AnyElement {
    let live = voice.session.as_ref().filter(|s| s.channel_id == channel);
    let mut list = div().flex().flex_col();
    if voice.joining.as_deref() == Some(channel) {
        list = list.child(
            div()
                .pl(px(34.))
                .py(px(2.))
                .h(px(28.))
                .flex()
                .items_center()
                .text_size(px(12.))
                .italic()
                .text_color(white_a(0.45))
                .child(t::call_connecting()),
        );
    }
    for p in voice.present(channel) {
        let glow = if live.is_some() && !p.muted { voice.glow.borrow().get(&p.npub) } else { 0. };
        let speaking = glow > SPEAKING;
        let flag = |path: &'static str, server: bool, tip: SharedString| {
            div()
                .id(path)
                .flex()
                .tip(tip)
                .child(icon(path, 12., if server { c(crate::call::DANGER) } else { white_a(0.45) }))
        };
        let key = SharedString::from(format!("voice-m-{channel}-{}", p.npub));
        list = list.child(
            div()
                .id(key.clone())
                .motion_host()
                .relative()
                .h(px(28.))
                .pl(px(34.))
                .pr(px(8.))
                .flex()
                .items_center()
                .gap(px(6.))
                .text_size(px(12.))
                .text_color(if speaking { c(0xffffff) } else { white_a(0.6) })
                .cursor_pointer()
                .hover(|d| d.text_color(white_a(0.85)))
                .on_click(click(on, Act::Profile(p.npub.clone(), community.to_string(), key.clone())))
                .child(crate::kit::anchor(anchors, key))
                .on_mouse_down(gpui::MouseButton::Right, menu(on, community, channel, &p.npub))
                .child(
                    div()
                        .relative()
                        .flex_shrink_0()
                        .rounded_full()
                        .child(glow_layer(voice.glow.borrow_mut().cell(&p.npub), 1.5, None, &format!("side-{}", p.npub)))
                        .child(crate::ui::avatar(store, &p.npub, 18.)),
                )
                .child(div().flex_1().min_w_0().truncate().child(store.chat_name(&p.npub, channel)))
                .when(p.screen || p.camera || p.muted || p.deafened, |d| {
                    d.child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .flex_shrink_0()
                            .when(p.screen, |d| {
                                d.child(div().id("live").tip(t::voice_sharing_their_screen()).child(live_tag(9., 14., 5.)))
                            })
                            .when(p.camera, |d| d.child(flag("icons/video.svg", false, t::voice_camera_on())))
                            .when(p.muted, |d| {
                                d.child(flag("icons/mic-off.svg", p.server_muted, if p.server_muted { t::voice_muted_by_mod() } else { t::voice_muted() }))
                            })
                            .when(p.deafened, |d| {
                                d.child(flag("icons/volume-mute.svg", p.server_deafened, if p.server_deafened { t::voice_deafened_by_mod() } else { t::voice_deafened() }))
                            }),
                    )
                }),
        );
    }
    list.into_any_element()
}

fn live_tag(size: f32, line: f32, pad: f32) -> impl IntoElement {
    div()
        .px(px(pad))
        .rounded(px(5.))
        .bg(c(crate::call::DANGER))
        .text_color(c(0xffffff))
        .text_size(px(size))
        .line_height(px(line))
        .font_weight(gpui::FontWeight::BOLD)
        .child(t::voice_live().to_uppercase())
}

/// `voiceKeyHint`: the app-wide shortcut for a voice control.
fn key_hint(key: char) -> String {
    // i18n-exempt: key names
    if cfg!(target_os = "macos") { format!("\u{2318}\u{21e7}{key}") } else { format!("Ctrl+Shift+{key}") }
}

/// `.call-btn`, or `.call-btn-locked` while a moderator holds it: ours can't lift it.
fn held_btn(id: &'static str, on_state: bool, held: bool) -> gpui::Stateful<gpui::Div> {
    if !held {
        return call_btn(id, on_state);
    }
    let mut wash = c(crate::call::DANGER);
    wash.a = 0.22;
    crate::call::round_btn(id, 34.).bg(wash).cursor_not_allowed()
}

fn mic_btn(s: &Session, on: &OnAct) -> impl IntoElement {
    let off = s.mic_off();
    let tip = if s.server_muted {
        t::voice_muted_by_mod()
    } else if off {
        t::voice_unmute_key(key_hint('M'))
    } else {
        t::voice_mute_key(key_hint('M'))
    };
    let held = s.server_muted;
    let tint = if held { c(crate::call::DANGER) } else if off { c(ACCENT) } else { white_a(0.88) };
    held_btn("voice-mic", off, held)
        .tip(tip)
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(click(on, Act::Mic))
        .child(icon(if off { "icons/mic-off.svg" } else { "icons/mic-on.svg" }, 18.7, tint))
}

fn route_btn(id: &'static str, on: &OnAct) -> Option<impl IntoElement> {
    let on = on.clone();
    crate::route::button(id, move |r, w, cx| on(Act::Route(r), w, cx))
}

fn deafen_btn(s: &Session, on: &OnAct) -> impl IntoElement {
    let off = s.deafened || s.server_deafened;
    let tip = if s.server_deafened {
        t::voice_deafened_by_mod()
    } else if s.deafened {
        t::voice_undeafen_key(key_hint('D'))
    } else {
        t::voice_deafen_key(key_hint('D'))
    };
    let held = s.server_deafened;
    let tint = if held { c(crate::call::DANGER) } else if off { c(ACCENT) } else { white_a(0.88) };
    held_btn("voice-deafen", off, held)
        .tip(tip)
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(click(on, Act::Deafen))
        .child(icon(if off { "icons/volume-mute.svg" } else { "icons/volume-max.svg" }, 18.7, tint))
}

fn video_btn(
    id: &'static str,
    path: &'static str,
    size: f32,
    state: bool,
    act: Act,
    on: &OnAct,
) -> impl IntoElement {
    let tip = match (id, state) {
        ("voice-camera", true) => t::voice_camera_turn_off(),
        ("voice-camera", false) => t::voice_camera_turn_on(),
        (_, true) => t::voice_screen_stop(),
        (_, false) => t::voice_screen_share(),
    };
    call_btn(id, state)
        .tip(tip)
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(click(on, act))
        .child(icon(
            path,
            size,
            if state { c(ACCENT) } else { white_a(0.88) },
        ))
}

/// `.voice-screen`: the shared screen on the stage at its own aspect, its bar on hover.
fn screen(
    voice: &Voice,
    store: &Store,
    seen: &Seen,
    viewing: &str,
    channel: &str,
    me: &str,
    max_w: f32,
    win_h: f32,
    on: &OnAct,
) -> AnyElement {
    let image = if viewing == me {
        seen.mine[1].clone()
    } else {
        voice
            .key_of(viewing)
            .and_then(|k| seen.members.get(&(k.to_string(), 1)).cloned())
    };
    let max_h = win_h * if voice.expanded { 0.74 } else { 0.42 };
    let (w, h) = match image.as_ref().map(|i| i.size(0)) {
        Some(sz) if sz.width.0 > 0 && sz.height.0 > 0 => {
            let aspect = sz.width.0 as f32 / sz.height.0 as f32;
            let w = max_w.min(max_h * aspect);
            (w, w / aspect)
        }
        // A video element with nothing to show yet is 150 px tall.
        _ => (max_w, 150f32.min(max_h)),
    };
    let label = if viewing == me {
        t::voice_your_screen()
    } else {
        t::voice_their_screen(store.chat_name(viewing, channel))
    };
    let btn = |id: &'static str, tip: SharedString, act: Act, child: AnyElement| {
        div()
            .id(id)
            .tip(tip)
            .size(px(28.))
            .flex_shrink_0()
            .rounded(px(8.))
            .flex()
            .items_center()
            .justify_center()
            .bg(white_a(0.12))
            .hover(|d| d.bg(white_a(0.22)))
            .cursor_pointer()
            .on_click(click(on, act))
            .child(child)
    };
    div()
        .id("voice-screen")
        .group("voice-screen")
        .relative()
        .mt(px(12.))
        .flex()
        .justify_center()
        .rounded(px(10.))
        .overflow_hidden()
        .bg(c(0x000000))
        .child(div().relative().w(px(w)).max_w_full().h(px(h)).children(image.map(|i| {
            img(i)
                .absolute()
                .inset_0()
                .size_full()
                .object_fit(ObjectFit::Contain)
        })))
        .child(
            div()
                .absolute()
                .left_0()
                .right_0()
                .bottom_0()
                .flex()
                .items_center()
                .gap(px(6.))
                .px(px(10.))
                .py(px(8.))
                .bg(gpui::linear_gradient(
                    180.,
                    gpui::linear_color_stop(gpui::transparent_black(), 0.),
                    gpui::linear_color_stop(ca(0, 0, 0, 0.7), 1.),
                ))
                .opacity(0.)
                .group_hover("voice-screen", |d| d.opacity(1.))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(px(13.))
                        .text_color(c(0xffffff))
                        .child(label),
                )
                .child(btn(
                    "voice-screen-size",
                    if voice.expanded { t::voice_smaller() } else { t::voice_bigger() },
                    Act::Expand,
                    icon(
                        if voice.expanded {
                            "ui/voice-smaller.svg"
                        } else {
                            "ui/voice-bigger.svg"
                        },
                        16.,
                        c(0xffffff),
                    )
                    .into_any_element(),
                ))
                .child(btn(
                    "voice-screen-hide",
                    t::voice_hide(),
                    Act::HideScreen,
                    icon("icons/x.svg", 12., c(0xffffff)).into_any_element(),
                )),
        )
        .into_any_element()
}

fn leave_btn(on: &OnAct) -> impl IntoElement {
    round_btn("voice-leave", 34.)
        .tip(t::voice_leave())
        .bg(c(crate::call::DANGER))
        .hover(|d| d.bg(c(0xff4db6)))
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(click(on, Act::Leave))
        .child(phone(20., c(0xffffff), true))
}

/// How much of the pane the stage takes: every member's tile, their faces, or one row.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StageMode {
    #[default]
    Expanded,
    Compact,
    /// One row, offered only when the faces need more than one.
    Minimised,
}

impl StageMode {
    pub fn key(self) -> &'static str {
        match self {
            StageMode::Expanded => "expanded",
            StageMode::Compact => "compact",
            StageMode::Minimised => "minimised",
        }
    }

    pub fn from_key(key: &str) -> Self {
        match key {
            "compact" | "bar" => StageMode::Compact,
            "minimised" => StageMode::Minimised,
            _ => StageMode::Expanded,
        }
    }

    /// The state shown: one row of faces already is the minimised row.
    pub fn shown(self, fits: bool) -> Self {
        if self == StageMode::Minimised && fits { StageMode::Compact } else { self }
    }

    /// The chevron, through the states the call offers: `fits` when its faces take one row.
    pub fn next(self, fits: bool) -> Self {
        match self.shown(fits) {
            StageMode::Expanded => StageMode::Compact,
            StageMode::Compact if !fits => StageMode::Minimised,
            _ => StageMode::Expanded,
        }
    }
}

/// A member's tile at a call's size: a few get large, personal faces, a crowd a tight grid.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tier {
    pub avatar: f32,
    pub w: f32,
    pub h: f32,
    pub name: f32,
}

pub fn tier(members: usize) -> Tier {
    match members {
        0..=4 => Tier { avatar: 96., w: 168., h: 156., name: 14. },
        5..=12 => Tier { avatar: 64., w: 116., h: 112., name: 13. },
        _ => Tier { avatar: 40., w: 72., h: 76., name: 11. },
    }
}

/// The controls' pill: the composer's dark.
const PILL_BG: u32 = 0x1b1c1d;
/// What a badge's ring cuts out of: the chat's own black.
const BADGE_RING: u32 = 0x030303;
/// A face in the compact and minimised rows, and the gap after it.
const ROW_FACE: f32 = 32.;
const ROW_GAP: f32 = 8.;
/// What the row's controls pill and chevron take beside the faces.
const ROW_CONTROLS: f32 = 128. + 28. + 24.;
/// The compact faces' height before they scroll: three rows.
const COMPACT_MAX_H: f32 = 3. * (ROW_FACE + ROW_GAP) + 8.;

/// The speaking ring and its soft light, at `glow` (0 silent, 1 loud).
fn glow_ring(glow: f32, width: f32, inset: bool) -> Vec<BoxShadow> {
    if glow <= 0.01 {
        return Vec::new();
    }
    let mut out = vec![BoxShadow {
        color: c(ACCENT).opacity(0.35 + 0.6 * glow),
        offset: point(px(0.), px(0.)),
        blur_radius: px(0.),
        spread_radius: px(width),
        inset,
    }];
    if !inset {
        out.push(BoxShadow {
            color: c(ACCENT).opacity(0.45 * glow),
            offset: point(px(0.), px(0.)),
            blur_radius: px(4. + 12. * glow),
            spread_radius: px(1. * glow),
            inset: false,
        });
    }
    out
}

/// How far a glow's light reaches past its face.
const GLOW_REACH: f32 = 20.;

/// The speaking glow behind a face, painted past the clips around it (a scroll area, the
/// stage) by its own reach, and not at all for a face scrolled out of view.
pub(crate) fn glow_layer(level: Rc<Cell<f32>>, width: f32, radius: Option<f32>, key: &str) -> impl IntoElement {
    let key = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        key.hash(&mut h);
        h.finish()
    };
    canvas(
        |_, _, _| {},
        move |b, _, window, _| {
            if !window.content_mask().bounds.intersects(&b) {
                return;
            }
            let r = radius.map_or(b.size.width / 2., px);
            let make: Rc<dyn Fn(f32) -> [BoxShadow; 2]> = Rc::new(move |g| glow_pair(g, width));
            window.with_content_mask_grown(px(GLOW_REACH), |w| {
                w.paint_live_shadows(b, gpui::Corners::all(r), key, gpui::LiveShadow { level, make })
            });
        },
    )
    .absolute()
    .inset_0()
}

/// [`glow_ring`]'s ring and light, unlit at silence rather than gone, so a live glow keeps
/// its two shadows to restyle.
fn glow_pair(glow: f32, width: f32) -> [BoxShadow; 2] {
    let g = if glow <= 0.01 { 0. } else { glow };
    [
        BoxShadow {
            color: c(ACCENT).opacity(if g > 0. { 0.35 + 0.6 * g } else { 0. }),
            offset: point(px(0.), px(0.)),
            blur_radius: px(0.),
            spread_radius: px(width),
            inset: false,
        },
        BoxShadow {
            color: c(ACCENT).opacity(0.45 * g),
            offset: point(px(0.), px(0.)),
            blur_radius: px(4. + 12. * g),
            spread_radius: px(g),
            inset: false,
        },
    ]
}

/// A muted or deafened mark, `size` across.
fn state_badge(path: &'static str, server: bool, tip: SharedString, size: f32) -> gpui::Stateful<gpui::Div> {
    div()
        .id(path)
        .tip(tip)
        .size(px(size))
        .flex_shrink_0()
        .rounded_full()
        .border_2()
        .border_color(c(BADGE_RING))
        .bg(if server { c(crate::call::DANGER) } else { c(0x262626) })
        .flex()
        .items_center()
        .justify_center()
        .child(icon(path, size * 0.46, if server { c(0xffffff) } else { c(crate::call::DANGER) }))
}

fn badges(p: &Member, size: f32) -> Option<gpui::Div> {
    (p.muted || p.deafened).then(|| {
        div()
            .flex()
            .gap(px(2.))
            .when(p.muted, |d| {
                d.child(state_badge("icons/mic-off.svg", p.server_muted, if p.server_muted { t::voice_muted_by_mod() } else { t::voice_muted() }, size))
            })
            .when(p.deafened, |d| {
                d.child(state_badge("icons/volume-mute.svg", p.server_deafened, if p.server_deafened { t::voice_deafened_by_mod() } else { t::voice_deafened() }, size))
            })
    })
}

fn chevron(mode: StageMode, fits: bool, on: &OnAct) -> impl IntoElement {
    let (tip, turn) = match mode.next(fits) {
        StageMode::Compact => (t::voice_stage_compact(), std::f32::consts::PI),
        StageMode::Minimised => (t::voice_stage_minimise(), std::f32::consts::PI),
        StageMode::Expanded => (t::voice_stage_expand(), 0.),
    };
    div()
        .id("voice-stage-fold")
        .tip(tip)
        .size(px(28.))
        .flex_shrink_0()
        .rounded(px(8.))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .hover(|d| d.bg(white_a(0.08)))
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(click(on, Act::Fold))
        .child(
            svg()
                .path("icons/chevron-down.svg")
                .size(px(16.))
                .text_color(white_a(0.6))
                .with_transformation(gpui::Transformation::rotate(gpui::radians(turn))),
        )
}

/// A member's camera picture, when they show one and it has arrived.
fn camera_of(voice: &Voice, seen: &Seen, p: &Member, me: &str) -> Option<Arc<RenderImage>> {
    if !voice.cam(p, me) {
        return None;
    }
    if p.npub == me {
        seen.mine[0].clone()
    } else {
        voice.key_of(&p.npub).and_then(|k| seen.members.get(&(k.to_string(), 0)).cloned())
    }
}

/// A member who left, shrinking and fading out over [`LEAVE_MS`].
fn ghost(store: &Store, m: &Member, gone: f32, avatar: f32, w: f32, h: f32) -> AnyElement {
    gpui::zoom(
        div()
            .w(px(w))
            .h(px(h))
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_center()
            .child(crate::ui::avatar(store, &m.npub, avatar)),
    )
    .scale(1. - 0.25 * gone)
    .opacity(1. - gone)
    .into_any_element()
}

/// A member's face that opens their mini profile beside it, and their menu on a right-click.
#[allow(clippy::too_many_arguments)]
fn face(
    store: &Store,
    voice: &Voice,
    seen: &Seen,
    p: &Member,
    me: &str,
    size: f32,
    camera: Option<(f32, f32)>,
    community: &str,
    channel: &str,
    on: &OnAct,
    anchors: &crate::kit::Anchors,
) -> gpui::Stateful<gpui::Div> {
    let key = SharedString::from(format!("voice-face-{channel}-{}", p.npub));
    let picture = camera.and_then(|wh| camera_of(voice, seen, p, me).map(|image| (image, wh)));
    let badge = (size * 0.3).clamp(16., 26.);
    div()
        .id(key.clone())
        .relative()
        .flex_shrink_0()
        .cursor_pointer()
        .on_click(click(on, Act::Profile(p.npub.clone(), community.to_string(), key.clone())))
        .on_mouse_down(gpui::MouseButton::Right, menu(on, community, channel, &p.npub))
        .map(|d| match picture {
            Some((image, (w, h))) => d
                .rounded(px(8.))
                .child(glow_layer(voice.glow.borrow_mut().cell(&p.npub), 2., Some(8.), &format!("face-{size}-{}", p.npub)))
                .child(img(image).w(px(w)).h(px(h)).rounded(px(8.)).object_fit(ObjectFit::Cover)),
            None => d.rounded_full().child(glow_layer(voice.glow.borrow_mut().cell(&p.npub), if size >= 64. { 3. } else { 2. }, None, &format!("face-{size}-{}", p.npub))).child(crate::ui::avatar(store, &p.npub, size)),
        })
        .children(badges(p, badge).map(|b| b.absolute().right(px(-4.)).bottom(px(-4.))))
        .child(crate::kit::anchor(anchors, key))
}

/// `.voice-stage`: who is in the call, and joining it or the call's own controls once in.
#[allow(clippy::too_many_arguments)]
pub fn stage(
    voice: &Voice,
    store: &Store,
    seen: &Seen,
    win_h: f32,
    community: &str,
    channel: &str,
    me: &str,
    slim: bool,
    on: &OnAct,
    anchors: &crate::kit::Anchors,
) -> AnyElement {
    let session = voice.in_call(channel);
    let people_n = voice.present(channel).len();
    // Room for faces in one row beside the controls, from the stage's last width.
    let row_room = (voice.stage_w.get() - 32. - ROW_CONTROLS).max(0.);
    let fits = people_n as f32 * (ROW_FACE + ROW_GAP) <= row_room + ROW_GAP;
    voice.compact_fits.set(fits);
    let mode = voice.mode.shown(fits);
    let viewing = voice.viewing(channel, me).filter(|_| mode == StageMode::Expanded);
    let people = voice.present(channel);
    let leaving = voice.leaving(channel);
    let joining = voice.joining.as_deref() == Some(channel);
    let connected = |npub: &str| npub == me || session.is_some_and(|s| s.peers.iter().any(|p| p.npub == npub && p.connected));
    // Our own trouble says so once, here, rather than flagging everyone we can't reach well.
    let own = session.and_then(Session::own_trouble);
    let glow = |npub: &str| voice.glow.borrow().get(npub);
    let name_of = |npub: &str| if npub == me { t::voice_you().to_string() } else { store.chat_name(npub, channel) };
    let status = div()
        .flex()
        .items_center()
        .gap(px(8.))
        .min_w_0()
        .text_size(px(12.))
        .text_color(white_a(0.55))
        .child(div().flex_shrink_0().child(if people.is_empty() { t::voice_nobody() } else { t::voice_in_call(people.len()) }))
        .when_some(session.filter(|s| s.sharing), |d, s| {
            d.child(div().child("·")).child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_color(c(ACCENT))
                    .child(if s.viewers > 0 { t::voice_sharing_watched(s.viewers) } else { t::voice_sharing() }),
            )
        })
        .when(session.is_some_and(|s| !s.mic), |d| {
            d.child(div().child("·")).child(
                div()
                    .id("voice-mic-warn")
                    .min_w_0()
                    .truncate()
                    .cursor_pointer()
                    .text_color(Quality::Fair.color())
                    .on_click(click(on, Act::MicAccess))
                    .child(t::voice_no_mic()),
            )
        })
        .when_some(own, |d, q| {
            d.child(
                div()
                    .id("voice-stage-ping")
                    .flex()
                    .items_center()
                    .gap(px(5.))
                    .tip(t::voice_unstable_tip())
                    .child(dot(q))
                    .child(t::voice_unstable()),
            )
        });
    let controls = |bar: bool| {
        let row = div().flex().items_center().gap(px(6.));
        match session {
            Some(s) => row
                .when(s.can_video && !bar, |d| {
                    d.child(video_btn("voice-camera", "icons/video.svg", 18.7, s.camera, Act::Camera(!s.camera), on))
                        .child(video_btn("voice-share", "ui/call-screen.svg", 20., s.sharing, Act::Share(!s.sharing), on))
                })
                .when(!bar, |d| d.children(route_btn("voice-stage-route", on)))
                .child(mic_btn(s, on))
                .child(deafen_btn(s, on))
                .child(leave_btn(on))
                .into_any_element(),
            None => row
                .child(
                    div()
                        .id("voice-join")
                        .h(px(34.))
                        .px(px(18.))
                        .rounded(px(17.))
                        .bg(c(ACCENT))
                        .flex()
                        .items_center()
                        .text_size(px(15.))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(c(0x000000))
                        .when(joining, |d| d.opacity(0.6))
                        .when(!joining, |d| {
                            d.cursor_pointer()
                                .hover(|d| d.opacity(0.9))
                                .on_click(click(on, Act::Join(community.to_string(), channel.to_string())))
                        })
                        .child(if joining { t::voice_joining() } else { t::voice_join() }),
                )
                .into_any_element(),
        }
    };
    let body = match mode {
        StageMode::Compact | StageMode::Minimised => {
            let row = mode == StageMode::Minimised;
            // Minimised: as many faces as fit beside the count, then how many more.
            let room = if row { row_room - 16. - 12. - 120. - 36. } else { row_room };
            let take = if row { ((room + ROW_GAP) / (ROW_FACE + ROW_GAP)).floor().max(1.) as usize } else { people.len() };
            let shown = people.len().min(take);
            let more = people.len() - shown;
            let faces = div()
                .flex()
                .flex_wrap()
                .items_center()
                // Room on every side for the rings and their light.
                .p(px(6.))
                .gap(px(ROW_GAP))
                .children(people.iter().take(shown).map(|p| {
                    let pending = session.is_some() && !connected(&p.npub);
                    crate::kit::pop(
                        SharedString::from(format!("voice-row-in-{}", p.npub)),
                        face(store, voice, seen, p, me, ROW_FACE, Some((52., ROW_FACE)), community, channel, on, anchors)
                            .tip(name_of(&p.npub))
                            .when(pending, |d| d.opacity(0.5)),
                        180,
                    )
                }))
                .children(leaving.iter().map(|(m, gone)| ghost(store, m, *gone, ROW_FACE, ROW_FACE, ROW_FACE)))
                .when(more > 0, |d| {
                    d.child(div().text_size(px(12.)).font_weight(gpui::FontWeight::SEMIBOLD).text_color(white_a(0.6)).child(format!("+{more}")))
                });
            div()
                .min_h(px(52.))
                .flex()
                .items_center()
                .gap(px(12.))
                .when(row, |d| d.child(icon("icons/volume-max.svg", 16., c(ACCENT))).child(status))
                .child(
                    div()
                        .id("voice-faces")
                        .flex_1()
                        .min_w_0()
                        .when(!row, |d| d.max_h(px(COMPACT_MAX_H)).overflow_y_scroll())
                        .child(faces),
                )
                .child(
                    div()
                        .p(px(4.))
                        .rounded_full()
                        .bg(c(PILL_BG))
                        .border_1()
                        .border_color(white_a(0.08))
                        .child(controls(true)),
                )
                .child(chevron(mode, fits, on))
                .into_any_element()
        }
        StageMode::Expanded => {
            let tr = {
                // A size tier change eases the tiles from one size to the next.
                let to = tier(people.len());
                let ease = |part: &str, v: f32| crate::kit::ease(SharedString::from(format!("voice-tier-{part}-{channel}")), v, 0.22, crate::kit::bezier(0.2, 0.8, 0.2, 1.));
                Tier { avatar: ease("a", to.avatar), w: ease("w", to.w), h: ease("h", to.h), ..to }
            };
            let tiles = people.iter().map(|p| {
                let g = glow(&p.npub);
                let pending = session.is_some() && !connected(&p.npub);
                let sharing = voice.sharing(p, me);
                let picture = camera_of(voice, seen, p, me);
                let has_cam = voice.cam(p, me) && (p.npub == me || voice.key_of(&p.npub).is_some());
                let is_viewing = viewing.as_deref() == Some(p.npub.as_str());
                let note = if p.elsewhere {
                    Some(t::voice_note_elsewhere())
                } else if pending {
                    Some(t::voice_note_connecting())
                } else {
                    None
                };
                let pick = (session.is_some() && sharing).then(|| on.clone());
                let npub = p.npub.clone();
                let link = session.filter(|_| p.npub != me).and_then(|s| s.peers.iter().find(|x| x.npub == p.npub && x.connected));
                let path = if p.elsewhere { Some(t::voice_elsewhere_tip()) } else { link.and_then(link_title) };
                let flag = link.filter(|_| own.is_none()).and_then(|l| voice.flag(l));
                let live_tip = match session {
                    Some(_) if is_viewing => t::voice_hide_their_screen(),
                    Some(_) => t::voice_show_their_screen(),
                    None => t::voice_sharing_their_screen(),
                };
                let name = div()
                    .max_w_full()
                    .px(px(6.))
                    .truncate()
                    .text_size(px(tr.name))
                    .line_height(px(tr.name + 4.))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(if p.npub == me || p.deafened { white_a(0.55) } else { white_a(0.95) })
                    .child(name_of(&p.npub));
                let (cam_w, cam_h) = (tr.w.max(184.), tr.h.max(120.));
                let tile = div()
                    .id(SharedString::from(format!("voice-tile-{}", p.npub)))
                    .when_some(path, |d, tip| d.tip(tip))
                    .relative()
                    .flex_shrink_0()
                    .w(px(if has_cam { cam_w } else { tr.w }))
                    .h(px(if has_cam { cam_h } else { tr.h }))
                    .rounded(px(16.))
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(if tr.avatar < 64. { 4. } else { 10. }))
                    .hover(|d| d.bg(white_a(0.04)))
                    .when(pending, |d| d.opacity(0.5))
                    .when_some(pick, |d, o| d.cursor_pointer().on_click(move |_, w, cx| o(Act::Pick(npub.clone()), w, cx)))
                    .on_mouse_down(gpui::MouseButton::Right, menu(on, community, channel, &p.npub))
                    .map(|d| match picture.filter(|_| has_cam) {
                        Some(image) => d
                            .child(
                                div()
                                    .absolute()
                                    .inset_0()
                                    .rounded(px(14.))
                                    .bg(c(0x000000))
                                    .when(is_viewing, |d| d.border_1().border_color(c(ACCENT)))
                                    .child(img(image).absolute().inset_0().size_full().rounded(px(14.)).object_fit(ObjectFit::Cover))
                                    .child(div().absolute().inset_0().rounded(px(14.)).shadow(glow_ring(g, 2., true))),
                            )
                            .children(badges(p, 22.).map(|b| b.absolute().right(px(6.)).bottom(px(6.))))
                            .child(
                                div()
                                    .absolute()
                                    .left(px(8.))
                                    .bottom(px(8.))
                                    .max_w(px(cam_w - 48.))
                                    .px(px(8.))
                                    .py(px(2.))
                                    .rounded(px(8.))
                                    .bg(ca(0, 0, 0, 0.6))
                                    .truncate()
                                    .text_size(px(13.))
                                    .line_height(px(20.))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(c(TEXT))
                                    .child(name_of(&p.npub)),
                            ),
                        None => d
                            .child(face(store, voice, seen, p, me, tr.avatar, None, community, channel, on, anchors))
                            .child(name)
                            .children(note.map(|n| div().mt(px(-6.)).text_size(px(10.)).text_color(white_a(0.4)).child(n))),
                    })
                    // A weak link to us, then LIVE.
                    .when(sharing || flag.is_some(), |d| {
                        d.child(
                            div()
                                .absolute()
                                .top(px(6.))
                                .right(px(6.))
                                .flex()
                                .items_center()
                                .gap(px(4.))
                                .children(flag.map(|f| {
                                    div()
                                        .px(px(6.))
                                        .py(px(1.))
                                        .rounded(px(6.))
                                        .bg(ca(0, 0, 0, 0.6))
                                        .text_color(f.grade.color())
                                        .text_size(px(10.))
                                        .line_height(px(16.))
                                        .font_weight(gpui::FontWeight::BOLD)
                                        .whitespace_nowrap()
                                        .child(f.label)
                                }))
                                .when(sharing, |d| {
                                    d.child(
                                        div()
                                            .id("voice-tile-live")
                                            .tip(live_tip)
                                            .px(px(6.))
                                            .rounded(px(6.))
                                            .bg(c(crate::call::DANGER))
                                            .text_color(c(0xffffff))
                                            .text_size(px(10.))
                                            .line_height(px(16.))
                                            .font_weight(gpui::FontWeight::BOLD)
                                            .child(t::voice_live().to_uppercase()),
                                    )
                                }),
                        )
                    });
                crate::kit::pop(SharedString::from(format!("voice-tile-in-{}", p.npub)), tile, 220)
            });
            let w = match voice.grid_w.get() {
                0. => 558.,
                w => w,
            };
            let record = voice.grid_w.clone();
            div()
                .flex()
                .flex_col()
                .gap(px(10.))
                .child(div().h(px(28.)).flex().items_center().gap(px(8.)).child(status).child(div().flex_1()).child(chevron(mode, fits, on)))
                .when_some(viewing.as_deref(), |d, v| d.child(screen(voice, store, seen, v, channel, me, w, win_h, on)))
                .when(!people.is_empty(), |d| {
                    d.child(
                        div()
                            .id("voice-grid")
                            .relative()
                            .max_h(px((win_h * if slim { 0.4 } else { 0.45 }).max(tr.h + 16.)))
                            .overflow_y_scroll()
                            .child(
                                // The grid's width, for the screen above it.
                                gpui::canvas(
                                    move |b, window, _| {
                                        let w = f32::from(b.size.width);
                                        if (record.replace(w) - w).abs() > 0.5 {
                                            window.refresh();
                                        }
                                    },
                                    |_, _, _, _| {},
                                )
                                .absolute()
                                .size_full(),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_wrap()
                                    .justify_center()
                                    .gap(px(4.))
                                    .children(tiles)
                                    .children(leaving.iter().map(|(m, gone)| ghost(store, m, *gone, tr.avatar, tr.w, tr.h))),
                            ),
                    )
                })
                .child(
                    div().flex().justify_center().child(
                        div()
                            .p(px(6.))
                            .rounded_full()
                            .bg(c(PILL_BG))
                            .border_1()
                            .border_color(white_a(0.08))
                            .child(controls(false)),
                    ),
                )
                .into_any_element()
        }
    };
    let record = voice.stage_w.clone();
    div()
        .relative()
        .px(px(16.))
        .py(px(if mode == StageMode::Expanded { 12. } else { 4. }))
        .child(
            // The stage's width, for how many faces a row holds.
            gpui::canvas(
                move |b, window, _| {
                    let w = f32::from(b.size.width);
                    if (record.replace(w) - w).abs() > 0.5 {
                        window.refresh();
                    }
                },
                |_, _, _, _| {},
            )
            .absolute()
            .size_full(),
        )
        .child(crate::kit::rise(
            SharedString::from(format!("voice-stage-{}", mode.key())),
            body,
            200,
            (0., -6., 0.98),
            crate::kit::bezier(0.2, 0.8, 0.2, 1.),
        ))
        .into_any_element()
}

fn clock(secs: u64) -> String {
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    crate::lang::digits(if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m:02}:{s:02}") })
}

/// `.voice-bar`: the call we are in, as the 1:1 call's pill, off its own channel's screen.
pub fn bar(voice: &Voice, store: &Store, pos: Option<Point<Pixels>>, top: f32, on: &OnAct) -> Option<AnyElement> {
    let s = voice.session.as_ref()?;
    let linked: Vec<&Peer> = s.linked().collect();
    let status = if !s.audio {
        t::voice_no_audio()
    } else {
        let time = clock(voice.since.map_or(0, |t| t.elapsed().as_secs()));
        if linked.is_empty() {
            t::voice_waiting(time)
        } else {
            t::voice_in_call_time(linked.len() + 1, time)
        }
    };
    // Only our own connection: a member's weak link shows on their tile.
    let own = s.own_trouble();
    let talk = linked.iter().map(|p| voice.glow.borrow().get(&p.npub)).fold(0f32, f32::max);
    let name = store
        .community(&s.community_id)
        .and_then(|c| c.channel(&s.channel_id))
        .map_or_else(|| t::voice_fallback_name().to_string(), |ch| ch.name.clone());
    let open = Act::Open(s.community_id.clone(), s.channel_id.clone());
    let down = {
        let (drag, bounds) = (voice.drag.clone(), voice.pill.clone());
        move |e: &gpui::MouseDownEvent, _: &mut Window, _: &mut App| {
            if let Some(b) = bounds.get() {
                drag.set(Some(crate::call::Drag { start: e.position, origin: b.origin, moved: false }));
            }
        }
    };
    let watch = {
        let (drag, record, on) = (voice.drag.clone(), voice.pill.clone(), on.clone());
        canvas(
            move |b, _, _| record.set(Some(b)),
            move |_, _, window, _| {
                if drag.get().is_none() {
                    return;
                }
                let (d, o) = (drag.clone(), on.clone());
                window.on_mouse_event(move |e: &gpui::MouseMoveEvent, phase, w, cx| {
                    let Some(mut g) = d.get() else { return };
                    if phase != gpui::DispatchPhase::Bubble {
                        return;
                    }
                    let delta = e.position - g.start;
                    if !g.moved && f32::from(delta.x).hypot(f32::from(delta.y)) < 4. {
                        return;
                    }
                    g.moved = true;
                    d.set(Some(g));
                    o(Act::Move(g.origin + delta), w, cx);
                });
                let (d, o) = (drag.clone(), on.clone());
                window.on_mouse_event(move |_: &gpui::MouseUpEvent, phase, w, cx| {
                    if phase != gpui::DispatchPhase::Bubble {
                        return;
                    }
                    if let Some(g) = d.take() {
                        o(if g.moved { Act::Moved } else { open.clone() }, w, cx);
                    }
                });
            },
        )
        .absolute()
        .inset_0()
    };
    let row = div()
        .id("voice-bar-row")
        .flex()
        .items_center()
        .gap(px(8.))
        .pt(px(6.))
        .pb(px(6.))
        .pl(px(6.))
        .pr(px(8.))
        .bg(c(0x262626))
        .rounded(px(15.))
        .cursor_grab()
        .on_mouse_down(gpui::MouseButton::Left, down)
        .child(
            div()
                .flex()
                .flex_shrink_0()
                .rounded_full()
                .shadow(vec![
                    BoxShadow {
                        color: accent_mix(0.12 + talk * 0.88),
                        offset: point(px(0.), px(0.)),
                        blur_radius: px(0.),
                        spread_radius: px(2.),
                        inset: false,
                    },
                    BoxShadow {
                        color: accent_mix(talk * 0.7),
                        offset: point(px(0.), px(0.)),
                        blur_radius: px(talk * 12.),
                        spread_radius: px(0.),
                        inset: false,
                    },
                ])
                .child(crate::community::logo(store, &s.community_id, 30., 15.)),
        )
        .child(
            div()
                .flex()
                .flex_col()
                // A fixed column: the clock's and the count's changes never move the buttons.
                .w(px(150.))
                .flex_shrink_0()
                .min_w_0()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .text_size(px(13.))
                        .line_height(px(15.))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(c(0xf6f6f6))
                        .child(icon("icons/volume-max.svg", 12., c(0xf6f6f6).opacity(0.7)).mr(px(4.)))
                        .child(div().truncate().child(name)),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(5.))
                        .h(px(13.2))
                        .text_size(px(11.5))
                        .line_height(px(13.2))
                        .text_color(c(0xb2b2b2))
                        .font_features(crate::kit::tabular())
                        .when_some(own, |d, q| d.child(div().id("voice-bar-ping").tip(t::voice_unstable()).child(dot(q))))
                        .child(div().truncate().child(status)),
                ),
        )
        .when(s.can_video, |d| {
            d.child(video_btn("voice-camera", "icons/video.svg", 18.7, s.camera, Act::Camera(!s.camera), on))
                .child(video_btn("voice-share", "ui/call-screen.svg", 20., s.sharing, Act::Share(!s.sharing), on))
        })
        .children(route_btn("voice-bar-route", on))
        .child(mic_btn(s, on))
        .child(deafen_btn(s, on))
        .child(leave_btn(on));
    let body = div()
        .id("voice-bar")
        .relative()
        .max_w(px(420.))
        .rounded(px(18.))
        .bg(c(0x101010))
        .border_1()
        .border_color(c(0x2c2e2f))
        .shadow(vec![BoxShadow {
            color: ca(0, 0, 0, 0.4),
            offset: point(px(0.), px(8.)),
            blur_radius: px(32.),
            spread_radius: px(0.),
            inset: false,
        }])
        .overflow_hidden()
        .occlude()
        .child(watch)
        .child(row);
    Some(match pos {
        Some(p) => div().absolute().left(p.x).top(p.y).child(body).into_any_element(),
        None => div()
            .absolute()
            .top(px(top))
            .left_0()
            .right_0()
            .flex()
            .justify_center()
            .child(body)
            .into_any_element(),
    })
}

impl Voice {
    pub fn bar_size(&self) -> gpui::Size<Pixels> {
        self.pill.get().map_or(gpui::size(px(420.), px(48.)), |b| b.size)
    }
}

#[cfg(test)]
mod stage_tests {
    use super::*;

    #[test]
    fn a_crowd_gets_smaller_tiles() {
        assert_eq!(tier(2).avatar, 96.);
        assert_eq!(tier(4).avatar, 96.);
        assert_eq!(tier(5).avatar, 64.);
        assert_eq!(tier(12).avatar, 64.);
        assert_eq!(tier(13).avatar, 40.);
    }

    #[test]
    fn a_glow_lights_fast_and_fades_slower() {
        let lit = ease_glow(0., 1., 0.03);
        let faded = ease_glow(1., 0., 0.03);
        assert!(lit > 0.6, "{lit}");
        assert!(faded > 0.8, "{faded}");
        // Settled after a moment either way, whatever the frame rate.
        let mut v = 0.;
        for _ in 0..120 {
            v = ease_glow(v, 1., 1. / 120.);
        }
        assert!(v > 0.99);
        let (mut a, mut b) = (1., 1.);
        a = ease_glow(a, 0., 0.1);
        for _ in 0..6 {
            b = ease_glow(b, 0., 0.1 / 6.);
        }
        assert!((a - b).abs() < 1e-4, "one long step equals six short ones");
    }

    #[test]
    fn silence_lets_the_glows_go() {
        let mut g = Glow::default();
        let t0 = Instant::now();
        assert!(g.step(&[("a".into(), 1.)], t0));
        assert!(g.get("a") > 0.);
        let mut t = t0;
        for _ in 0..200 {
            t += std::time::Duration::from_millis(16);
            g.step(&[], t);
        }
        assert!(!g.step(&[], t), "nothing left to draw");
        assert_eq!(g.get("a"), 0.);
    }

    #[test]
    fn the_chevron_offers_the_one_row_only_when_the_faces_need_more() {
        // Everyone fits one row: compact is that row.
        assert_eq!(StageMode::Expanded.next(true), StageMode::Compact);
        assert_eq!(StageMode::Compact.next(true), StageMode::Expanded);
        assert_eq!(StageMode::Minimised.shown(true), StageMode::Compact);
        // A crowd: compact wraps, and the single row follows it.
        assert_eq!(StageMode::Compact.next(false), StageMode::Minimised);
        assert_eq!(StageMode::Minimised.next(false), StageMode::Expanded);
        for m in [StageMode::Expanded, StageMode::Compact, StageMode::Minimised] {
            assert_eq!(StageMode::from_key(m.key()), m);
        }
        assert_eq!(StageMode::from_key("bar"), StageMode::Compact);
        assert_eq!(StageMode::from_key("nonsense"), StageMode::Expanded);
    }
}
