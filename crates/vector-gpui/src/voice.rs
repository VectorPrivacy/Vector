//! Voice Channels: the stage above a voice channel's chat, who is in each call under its
//! channel row, and the bar that follows the call around the app (`VoiceStage.svelte`,
//! `VoiceBar.svelte`, `ChannelList.svelte`).

use std::collections::HashMap;
use std::cell::Cell;
use std::rc::Rc;
use std::time::Instant;

use gpui::prelude::*;
use gpui::{AnyElement, App, BoxShadow, Bounds, Pixels, Point, canvas, SharedString, Window, div, point, px, svg};
use serde::Deserialize;

use crate::call::{Quality, accent_mix, call_btn, dot, grade, phone, round_btn};
use crate::data::Store;
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
}

impl Session {
    /// Off for any reason: our mute, a moderator's, or a deafen.
    pub fn mic_off(&self) -> bool {
        self.muted || self.server_muted || self.deafened || self.server_deafened
    }

    fn linked(&self) -> impl Iterator<Item = &Peer> {
        self.peers.iter().filter(|p| p.connected)
    }

    /// The call is as good as its slowest link.
    fn ping(&self) -> u32 {
        self.linked().map(|p| p.rtt_ms).max().unwrap_or(0)
    }

    fn level(&self, npub: &str, me: &str) -> f32 {
        if npub == me {
            return if self.mic_off() || !self.mic { 0. } else { self.mic_level };
        }
        self.peers.iter().find(|p| p.npub == npub).map_or(0., |p| p.level)
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
    let me = store.my_npub.as_str();
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
                .child("Connecting…"),
        );
    }
    for p in voice.present(channel) {
        let speaking = live.is_some_and(|s| !p.muted && s.level(&p.npub, me) > SPEAKING);
        let flag = |path: &'static str, server: bool| {
            icon(path, 12., if server { c(crate::call::DANGER) } else { white_a(0.45) })
        };
        let key = SharedString::from(format!("voice-m-{channel}-{}", p.npub));
        list = list.child(
            div()
                .id(key.clone())
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
                        .flex_shrink_0()
                        .rounded_full()
                        .when(speaking, |d| d.shadow(ring(2., c(ACCENT))))
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
                            .when(p.screen, |d| d.child(live_tag(9., 14., 5.)))
                            .when(p.camera, |d| d.child(flag("icons/video.svg", false)))
                            .when(p.muted, |d| d.child(flag("icons/mic-off.svg", p.server_muted)))
                            .when(p.deafened, |d| d.child(flag("icons/volume-mute.svg", p.server_deafened))),
                    )
                }),
        );
    }
    list.into_any_element()
}

fn ring(width: f32, color: gpui::Hsla) -> Vec<BoxShadow> {
    vec![BoxShadow {
        color,
        offset: point(px(0.), px(0.)),
        blur_radius: px(0.),
        spread_radius: px(width),
        inset: false,
    }]
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
        .child("LIVE")
}

fn rtt_quality(ms: u32) -> Option<Quality> {
    (ms > 0).then(|| grade(0., ms as f32))
}

fn mic_btn(s: &Session, on: &OnAct) -> impl IntoElement {
    let off = s.mic_off();
    call_btn("voice-mic", off)
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(click(on, Act::Mic))
        .child(icon(if off { "icons/mic-off.svg" } else { "icons/mic-on.svg" }, 18.7, if off { c(ACCENT) } else { white_a(0.88) }))
}

fn deafen_btn(s: &Session, on: &OnAct) -> impl IntoElement {
    let off = s.deafened || s.server_deafened;
    call_btn("voice-deafen", off)
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(click(on, Act::Deafen))
        .child(icon(
            if off { "icons/volume-mute.svg" } else { "icons/volume-max.svg" },
            18.7,
            if off { c(ACCENT) } else { white_a(0.88) },
        ))
}

fn leave_btn(on: &OnAct) -> impl IntoElement {
    round_btn("voice-leave", 34.)
        .bg(c(crate::call::DANGER))
        .hover(|d| d.bg(c(0xff4db6)))
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(click(on, Act::Leave))
        .child(phone(20., c(0xffffff), true))
}

/// `.voice-stage`: who is in the call, and joining it or the call's own controls once in.
pub fn stage(voice: &Voice, store: &Store, community: &str, channel: &str, me: &str, on: &OnAct) -> AnyElement {
    let session = voice.session.as_ref().filter(|s| s.channel_id == channel);
    let people = voice.present(channel);
    let joining = voice.joining.as_deref() == Some(channel);
    let connected = |npub: &str| npub == me || session.is_some_and(|s| s.peers.iter().any(|p| p.npub == npub && p.connected));
    let ping = session.map_or(0, Session::ping);
    let head = div()
        .flex()
        .items_center()
        .gap(px(8.))
        .h(px(24.))
        .text_size(px(12.))
        .text_color(c(0xb2b2b2))
        .child(if people.is_empty() {
            "Nobody is here yet".to_string()
        } else {
            format!("{} in the call", people.len())
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
                    .child("No microphone access: others can't hear you"),
            )
        })
        .when_some(rtt_quality(ping), |d, q| {
            d.child(div().ml_auto().flex().items_center().gap(px(5.)).child(dot(q)).child(format!("{ping} ms")))
        });
    let tiles = people.iter().map(|p| {
        let level = session.map_or(0., |s| s.level(&p.npub, me));
        let talk = (level * 2.).min(1.);
        let pending = session.is_some() && !connected(&p.npub);
        let badge = |path: &'static str, server: bool| {
            div()
                .size(px(26.))
                .rounded_full()
                .border_2()
                .border_color(c(0x0b0b0b))
                .bg(if server { c(crate::call::DANGER) } else { c(0x1a1a1a) })
                .flex()
                .items_center()
                .justify_center()
                .child(icon(path, 12., if server { c(0xffffff) } else { c(crate::call::DANGER) }))
        };
        let note = if p.elsewhere {
            Some("another app")
        } else if pending {
            Some("connecting")
        } else {
            None
        };
        div()
            .id(SharedString::from(format!("voice-tile-{}", p.npub)))
            .flex_shrink_0()
            .h(px(150.))
            .rounded(px(12.))
            .bg(white_a(0.04))
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(8.))
            .px(px(8.))
            .when(pending, |d| d.opacity(0.55))
            .on_mouse_down(gpui::MouseButton::Right, menu(on, community, channel, &p.npub))
            .child(
                div()
                    .relative()
                    .rounded_full()
                    .shadow(vec![
                        BoxShadow {
                            color: c(ACCENT).opacity(talk * 0.95),
                            offset: point(px(0.), px(0.)),
                            blur_radius: px(0.),
                            spread_radius: px(2.),
                            inset: false,
                        },
                        BoxShadow {
                            color: c(ACCENT).opacity(talk * 0.6),
                            offset: point(px(0.), px(0.)),
                            blur_radius: px(talk * 14.),
                            spread_radius: px(0.),
                            inset: false,
                        },
                    ])
                    .child(crate::ui::avatar(store, &p.npub, 64.))
                    .when(p.muted || p.deafened, |d| {
                        d.child(
                            div()
                                .absolute()
                                .right(px(-4.))
                                .bottom(px(-4.))
                                .flex()
                                .gap(px(2.))
                                .when(p.muted, |d| d.child(badge("icons/mic-off.svg", p.server_muted)))
                                .when(p.deafened, |d| d.child(badge("icons/volume-mute.svg", p.server_deafened))),
                        )
                    }),
            )
            .child(
                div()
                    .max_w_full()
                    .truncate()
                    .text_size(px(13.))
                    .line_height(px(24.))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(c(TEXT))
                    .child(if p.npub == me { "You".to_string() } else { store.chat_name(&p.npub, channel) }),
            )
            .children(note.map(|n| div().mt(px(-8.)).text_size(px(10.)).text_color(white_a(0.4)).child(n)))
    });
    let actions = div()
        .mt(px(12.))
        .flex()
        .items_center()
        .justify_center()
        .gap(px(8.));
    let actions = match session {
        Some(s) => actions.child(mic_btn(s, on)).child(deafen_btn(s, on)).child(leave_btn(on)),
        None => actions.child(
            div()
                .id("voice-join")
                .h(px(34.))
                .px(px(18.))
                .rounded(px(17.))
                .bg(c(ACCENT))
                .flex()
                .items_center()
                .text_size(px(16.))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(c(0x000000))
                .when(joining, |d| d.opacity(0.6))
                .when(!joining, |d| {
                    d.cursor_pointer()
                        .on_click(click(on, Act::Join(community.to_string(), channel.to_string())))
                })
                .child(if joining { "Joining…" } else { "Join Voice" }),
        ),
    };
    div()
        .bg(c(0x171717))
        .px(px(16.))
        .py(px(8.))
        .child(
            div()
                .rounded(px(16.))
                .bg(ca(20, 20, 20, 0.95))
                .border_1()
                .border_color(c(0x262626))
                .pt(px(12.))
                .px(px(14.))
                .pb(px(14.))
                .child(head)
                .when(!people.is_empty(), |d| {
                    // `repeat(auto-fill, minmax(180px, 240px))`, the tracks centred.
                    let w = match voice.grid_w.get() {
                        0. => 558.,
                        w => w,
                    };
                    let cols = (((w + 10.) / 190.).floor() as usize).max(1);
                    let col = ((w - 10. * (cols - 1) as f32) / cols as f32).min(240.);
                    let record = voice.grid_w.clone();
                    let mut grid = div().mt(px(10.)).relative().flex().flex_col().items_center().gap(px(10.)).child(
                        gpui::canvas(move |b, _, _| record.set(f32::from(b.size.width)), |_, _, _, _| {})
                            .absolute()
                            .size_full(),
                    );
                    let mut tiles = tiles.peekable();
                    while tiles.peek().is_some() {
                        let row: Vec<_> = tiles.by_ref().take(cols).map(|t| t.w(px(col))).collect();
                        grid = grid.child(
                            div()
                                .w(px(col * cols as f32 + 10. * (cols - 1) as f32))
                                .flex()
                                .gap(px(10.))
                                .children(row),
                        );
                    }
                    d.child(grid)
                })
                .child(actions),
        )
        .into_any_element()
}

fn clock(secs: u64) -> String {
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m:02}:{s:02}") }
}

/// `.voice-bar`: the call we are in, as the 1:1 call's pill, off its own channel's screen.
pub fn bar(voice: &Voice, store: &Store, pos: Option<Point<Pixels>>, top: f32, on: &OnAct) -> Option<AnyElement> {
    let s = voice.session.as_ref()?;
    let linked: Vec<&Peer> = s.linked().collect();
    let status = if !s.audio {
        "Without audio".to_string()
    } else {
        let time = clock(voice.since.map_or(0, |t| t.elapsed().as_secs()));
        if linked.is_empty() {
            format!("Waiting for others · {time}")
        } else {
            format!("{} in the call · {time}", linked.len() + 1)
        }
    };
    let ping = s.ping();
    let talk = ((linked.iter().map(|p| p.level).fold(0f32, f32::max) - 0.3) / 0.55).clamp(0., 1.);
    let name = store
        .community(&s.community_id)
        .and_then(|c| c.channel(&s.channel_id))
        .map_or_else(|| "Voice".to_string(), |ch| ch.name.clone());
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
                .flex_1()
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
                        .when_some(rtt_quality(ping), |d, q| d.child(dot(q)))
                        .child(div().truncate().child(status)),
                ),
        )
        .child(mic_btn(s, on))
        .child(deafen_btn(s, on))
        .child(leave_btn(on));
    let body = div()
        .id("voice-bar")
        .relative()
        .max_w(px(420.))
        .rounded(px(16.))
        .bg(ca(20, 20, 20, 0.95))
        .border_1()
        .border_color(c(0x333333))
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
